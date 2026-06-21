//! typio — unified Wayland input-method daemon (Rust).
//!
//! Combines everything into a single process:
//!   - Wayland input-method-v2 + keyboard grab + xkbcommon
//!   - libtypio TypioInstance + engine registration
//!   - flux panel (Vulkan candidate rendering)
//!   - UDS control server (TIP v3 JSON-RPC)
//!
//! ## Usage
//!
//! ```sh
//! # Build an engine first (e.g. rime):
//! cd ../typio-engine-rime && cargo build --release
//!
//! # Run the daemon:
//! cargo run --bin typio -- --engine-dir ../typio-engine-rime/build
//!
//! # Focus a text field and type.
//! ```

use std::ffi::{c_char, c_void, CStr};
use std::process::ExitCode;
use std::sync::Mutex;

use typio_abi::{
    TypioComposition, TypioCompositionCallback, TypioEventType,
    TypioKeyEvent, TypioResult,
};

use typio_host::engine_loader::manifest::EngineManifest;
use typio_host::input_method::{InputMethodFrontend, LifecycleEvent};
use typio_host::ipc::framing::{Request, Response, StandardError};
use typio_host::ipc::protocol;
use typio_host::ipc::protocol::methods;
use typio_host::panel::FluxPanel;
use typio_host::uds_server::{RequestOutcome, UdsServer};

// ── Engine commit callback ────────────────────────────────────────────────

static COMMITTED_TEXT: Mutex<Option<String>> = Mutex::new(None);
static COMPOSITION_CANDIDATES: Mutex<Option<(Vec<String>, usize)>> = Mutex::new(None);
static COMPOSITION_PREEDIT: Mutex<Option<String>> = Mutex::new(None);

extern "C" fn on_commit(
    _ctx: *mut typio_abi::TypioInputContext,
    text: *const c_char,
    _user_data: *mut c_void,
) {
    if text.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
        *slot = Some(s);
    }
}

extern "C" fn on_composition(
    _ctx: *mut typio_abi::TypioInputContext,
    comp: *const TypioComposition,
    _user_data: *mut c_void,
) {
    if comp.is_null() {
        return;
    }
    let comp = unsafe { &*comp };

    // Extract candidates.
    let mut candidates = Vec::new();
    if !comp.candidates.is_null() && comp.candidate_count > 0 {
        for i in 0..comp.candidate_count {
            let c = unsafe { &*comp.candidates.add(i) };
            if !c.text.is_null() {
                let text = unsafe { CStr::from_ptr(c.text) }
                    .to_string_lossy()
                    .into_owned();
                candidates.push(text);
            }
        }
    }

    // Extract preedit (first segment for simplicity).
    let mut preedit = String::new();
    if !comp.segments.is_null() && comp.segment_count > 0 {
        for i in 0..comp.segment_count {
            let seg = unsafe { &*comp.segments.add(i) };
            if !seg.text.is_null() {
                preedit.push_str(&unsafe { CStr::from_ptr(seg.text) }.to_string_lossy());
            }
        }
    }

    let selected = comp.selected.max(0) as usize;

    if let Ok(mut slot) = COMPOSITION_CANDIDATES.lock() {
        *slot = Some((candidates, selected));
    }
    if let Ok(mut slot) = COMPOSITION_PREEDIT.lock() {
        if preedit.is_empty() {
            *slot = None;
        } else {
            *slot = Some(preedit);
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────────────

fn main() -> ExitCode {
    eprintln!("typio: starting (v{})", env!("CARGO_PKG_VERSION"));

    let engine_dir = parse_arg("--engine-dir");
    let socket_override = parse_arg("--socket");
    let panel_width: u32 = 300;
    let panel_height: u32 = 200;

    // ── 1. TypioInstance + engine registration ────────────────────────────
    let temp = std::env::temp_dir().join(format!("typio-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);

    let mut instance = typio::instance::TypioInstance::new_rust(
        temp.to_str(),
        temp.to_str(),
        temp.to_str(),
        Vec::new(),
    );
    if let Err(e) = instance.init_rust() {
        eprintln!("FAIL: TypioInstance init: {e:?}");
        return ExitCode::from(1);
    }
    eprintln!("OK: TypioInstance initialized");

    let instance_ptr = instance.as_mut() as *mut _;
    let reg_ptr = typio::instance::typio_instance_get_registry(instance_ptr);

    // Register engines from the manifest directory.
    let mut keyboards: Vec<String> = Vec::new();
    let mut voices: Vec<String> = Vec::new();
    if let Some(ref dir) = engine_dir {
        if !reg_ptr.is_null() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = match path.file_name().and_then(|n| n.to_str()) {
                        Some(n) => n.to_string(),
                        None => continue,
                    };
                    if !name.starts_with("typio-engine-") || !name.ends_with(".toml") {
                        continue;
                    }
                    let manifest = match EngineManifest::read_from(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let c_name = std::ffi::CString::new(manifest.name.as_str()).unwrap();
                    let c_display = std::ffi::CString::new(
                        manifest.display_name.as_deref().unwrap_or(&manifest.name),
                    ).unwrap();
                    let c_desc = std::ffi::CString::new(
                        manifest.description.as_deref().unwrap_or(""),
                    ).unwrap();
                    let c_author = std::ffi::CString::new(
                        manifest.author.as_deref().unwrap_or(""),
                    ).unwrap();
                    let c_icon = manifest.icon.as_ref().map(|s| std::ffi::CString::new(s.as_str()).unwrap());
                    let c_lang = std::ffi::CString::new(manifest.primary_language()).unwrap();

                    let argv_strings: Vec<std::ffi::CString> = match manifest.argv(&path) {
                        Ok(v) => v.into_iter().filter_map(|s| std::ffi::CString::new(s).ok()).collect(),
                        Err(_) => continue,
                    };
                    if argv_strings.is_empty() {
                        continue;
                    }
                    let argv_ptrs: Vec<*const c_char> = argv_strings
                        .iter()
                        .map(|s| s.as_ptr())
                        .chain(std::iter::once(std::ptr::null()))
                        .collect();

                    let info = typio_abi::TypioEngineInfo {
                        name: c_name.as_ptr(),
                        display_name: c_display.as_ptr(),
                        description: c_desc.as_ptr(),
                        author: c_author.as_ptr(),
                        icon: c_icon.as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null()),
                        language: c_lang.as_ptr(),
                        type_: if manifest.engine_type == "voice" {
                            typio_abi::TypioEngineType::TypioEngineTypeVoice
                        } else {
                            typio_abi::TypioEngineType::TypioEngineTypeKeyboard
                        },
                        required_capabilities: std::ptr::null(),
                        optional_capabilities: std::ptr::null(),
                    };

                    let result = typio::c_api::registry::typio_registry_register_engine_process(
                        reg_ptr,
                        &info,
                        argv_ptrs.as_ptr(),
                    );

                    if result == TypioResult::TypioOk {
                        eprintln!("  REGISTERED: {} ({})", manifest.name, manifest.engine_type);
                        if manifest.engine_type == "voice" {
                            voices.push(manifest.name.clone());
                        } else {
                            keyboards.push(manifest.name.clone());
                        }
                        // Leak CStrings — engine backend holds them.
                        std::mem::forget(c_name);
                        std::mem::forget(c_display);
                        std::mem::forget(c_desc);
                        std::mem::forget(c_author);
                        std::mem::forget(c_icon);
                        std::mem::forget(c_lang);
                        std::mem::forget(argv_strings);
                    }
                }
            }
        }

        // Activate first keyboard engine.
        let mut kb_count: usize = 0;
        let kb_list = typio::c_api::registry::typio_registry_list_keyboards(reg_ptr, &mut kb_count);
        if !kb_list.is_null() && kb_count > 0 {
            let first = unsafe { *kb_list };
            if !first.is_null() {
                let name = unsafe { CStr::from_ptr(first) };
                eprintln!("OK: activating keyboard engine: {}", name.to_string_lossy());
                typio::c_api::registry::typio_registry_set_active_keyboard(reg_ptr, first);
            }
        }
    }

    // ── 2. Input context ──────────────────────────────────────────────────
    let ctx = typio::input_context::typio_input_context_new(instance_ptr);
    if ctx.is_null() {
        eprintln!("FAIL: cannot create input context");
        return ExitCode::from(1);
    }
    typio::input_context::typio_input_context_set_commit_callback(
        ctx,
        Some(on_commit),
        std::ptr::null_mut(),
    );
    typio::input_context::typio_input_context_set_composition_callback(
        ctx,
        Some(on_composition as TypioCompositionCallback),
        std::ptr::null_mut(),
    );
    typio::input_context::typio_input_context_focus_in(ctx);
    eprintln!("OK: input context ready (commit + composition callbacks)");

    // ── 3. Wayland connection ─────────────────────────────────────────────
    let callback = Box::new(|_event: LifecycleEvent| {
        // Lifecycle events handled in the event loop below.
    });

    let mut frontend = match InputMethodFrontend::connect(Some(callback)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: Wayland connect: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!("OK: Wayland connected, keyboard grabbed");

    // ── 4. Flux panel ─────────────────────────────────────────────────────
    // Use the SAME wl_display as the input-method frontend — extract
    // the raw pointer from the wayland-client Connection.
    let panel_display = frontend.raw_display_ptr();
    let mut panel = if !panel_display.is_null() {
        unsafe {
            match FluxPanel::new(panel_display, panel_width, panel_height) {
                Ok(p) => {
                    eprintln!("OK: flux panel created");
                    p
            }
                Err(e) => {
                    eprintln!("WARN: panel creation failed: {e} — running without panel");
                    return run_without_panel(frontend, ctx);
                }
            }
        }
    } else {
        eprintln!("WARN: cannot connect panel display — running without panel");
        return run_without_panel(frontend, ctx);
    };

    // ── 5. UDS server ─────────────────────────────────────────────────────
    let socket_path = match socket_override {
        Some(s) => std::path::PathBuf::from(s),
        None => protocol::socket_path(),
    };
    let mut server = match UdsServer::bind(&socket_path) {
        Ok(s) => {
            eprintln!("OK: UDS listening on {}", socket_path.display());
            s
        }
        Err(e) => {
            eprintln!("WARN: UDS bind failed: {e} — running without IPC");
            return run_without_uds(frontend, ctx, panel);
        }
    };

    let kb_clone = keyboards.clone();
    let vc_clone = voices.clone();
    server.set_handler(move |json: &str, _client| {
        let req = match Request::parse(json) {
            Ok(r) => r,
            Err(_) => {
                return RequestOutcome::respond(
                    Response::error(0, -32600, "Invalid Request")
                        .to_json()
                        .unwrap(),
                );
            }
        };
        let id = req.id.clone();
        let resp = match req.method.as_str() {
            methods::HELLO => Response::success(
                id,
                serde_json::json!({
                    "protocolVersion": protocol::PROTOCOL_VERSION,
                    "daemonVersion": env!("CARGO_PKG_VERSION"),
                    "daemon": "typio",
                    "loadedEngines": {
                        "keyboard": kb_clone,
                        "voice": vc_clone,
                    }
                }),
            ),
            methods::DAEMON_VERSION => {
                Response::success(id, serde_json::json!(env!("CARGO_PKG_VERSION")))
            }
            _ => Response::error(
                id,
                StandardError::MethodNotFound.code(),
                StandardError::MethodNotFound.message(),
            ),
        };
        RequestOutcome::respond(resp.to_json().unwrap())
    });

    // ── 6. Combined event loop ────────────────────────────────────────────
    eprintln!("typio: running. Focus a text field and type. Ctrl+C to exit.");

    let wl_fd = frontend.fd();
    let uds_fd = server.epoll_fd();
    let mut fds = [
        libc::pollfd { fd: wl_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: uds_fd, events: libc::POLLIN, revents: 0 },
    ];

    loop {
        // Flush Wayland + dispatch pending.
        if let Err(e) = frontend.dispatch() {
            eprintln!("dispatch error: {e}");
            break;
        }
        server.dispatch();

        // Process pending key from keyboard grab.
        if let Some(key) = frontend.state_mut().take_pending_key() {
            if key.state == 1 {
                let mods = frontend.state().mods_depressed;
                let event = TypioKeyEvent {
                    struct_size: std::mem::size_of::<TypioKeyEvent>(),
                    type_: TypioEventType::TypioEventKeyPress,
                    keycode: key.keycode,
                    keysym: key.keysym,
                    modifiers: mods,
                    unicode: key.unicode.chars().next().unwrap_or('\0') as u32,
                    time: key.time as u64,
                    is_repeat: false,
                    base_keysym: key.keysym,
                };

                let consumed = typio::input_context::typio_input_context_process_key(ctx, &event);

                if consumed {
                    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
                        if let Some(text) = slot.take() {
                            frontend.state_mut().commit_string_and_flush(&text);
                        }
                    }
                } else {
                    // Forward unhandled key to the focused app.
                    frontend.state().forward_key(key.time, key.keycode, 1);
                }
            } else {
                // Key release — forward to app.
                frontend.state().forward_key(key.time, key.keycode, 0);
            }
        }

        // Drain composition updates → panel + preedit.
        if let Ok(mut slot) = COMPOSITION_CANDIDATES.lock() {
            if let Some((candidates, selected)) = slot.take() {
                if candidates.is_empty() {
                    // Clear preedit when composition ends.
                    frontend.state_mut().clear_preedit_and_flush();
                } else {
                    // Update preedit text.
                    let preedit_text = COMPOSITION_PREEDIT.lock()
                        .ok()
                        .and_then(|s| s.clone());
                    if let Some(ref pt) = preedit_text {
                        let cursor = pt.len() as u32;
                        frontend.state_mut().set_preedit_and_flush(pt, cursor);
                    }
                }
                // Draw candidates on the panel.
                panel.draw_candidates(&candidates, selected);
            }
        }

        // Render panel if we have candidates.
        let candidates = &frontend.state().candidates;
        if !candidates.is_empty() {
            panel.draw_candidates(candidates, frontend.state().selected_candidate);
        }

        // Poll both fds.
        fds[0].revents = 0;
        fds[1].revents = 0;
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 100) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!("poll error: {e}");
            break;
        }
    }

    let _ = std::fs::remove_dir_all(&temp);
    ExitCode::SUCCESS
}

/// Run without a panel (text still works, no candidate display).
fn run_without_panel(
    mut frontend: InputMethodFrontend,
    ctx: *mut typio::TypioInputContext,
) -> ExitCode {
    eprintln!("typio: running without panel");
    loop {
        if let Err(e) = frontend.dispatch() {
            eprintln!("dispatch error: {e}");
            break;
        }
        if let Some(key) = frontend.state_mut().take_pending_key() {
            if key.state == 1 {
                let mods = frontend.state().mods_depressed;
                let event = TypioKeyEvent {
                    struct_size: std::mem::size_of::<TypioKeyEvent>(),
                    type_: TypioEventType::TypioEventKeyPress,
                    keycode: key.keycode,
                    keysym: key.keysym,
                    modifiers: mods,
                    unicode: key.unicode.chars().next().unwrap_or('\0') as u32,
                    time: key.time as u64,
                    is_repeat: false,
                    base_keysym: key.keysym,
                };
                let consumed = typio::input_context::typio_input_context_process_key(ctx, &event);
                if consumed {
                    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
                        if let Some(text) = slot.take() {
                            frontend.state_mut().commit_string_and_flush(&text);
                        }
                    }
                } else {
                    frontend.state().forward_key(key.time, key.keycode, 1);
                }
            } else {
                frontend.state().forward_key(key.time, key.keycode, 0);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    ExitCode::SUCCESS
}

/// Run without UDS (panel + Wayland still work).
fn run_without_uds(
    mut frontend: InputMethodFrontend,
    ctx: *mut typio::TypioInputContext,
    mut panel: FluxPanel,
) -> ExitCode {
    eprintln!("typio: running without UDS");
    loop {
        if let Err(e) = frontend.dispatch() {
            eprintln!("dispatch error: {e}");
            break;
        }
        if let Some(key) = frontend.state_mut().take_pending_key() {
            if key.state == 1 {
                let mods = frontend.state().mods_depressed;
                let event = TypioKeyEvent {
                    struct_size: std::mem::size_of::<TypioKeyEvent>(),
                    type_: TypioEventType::TypioEventKeyPress,
                    keycode: key.keycode,
                    keysym: key.keysym,
                    modifiers: mods,
                    unicode: key.unicode.chars().next().unwrap_or('\0') as u32,
                    time: key.time as u64,
                    is_repeat: false,
                    base_keysym: key.keysym,
                };
                let consumed = typio::input_context::typio_input_context_process_key(ctx, &event);
                if consumed {
                    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
                        if let Some(text) = slot.take() {
                            frontend.state_mut().commit_string_and_flush(&text);
                        }
                    }
                } else {
                    frontend.state().forward_key(key.time, key.keycode, 1);
                }
            } else {
                frontend.state().forward_key(key.time, key.keycode, 0);
            }
        }
        let candidates = &frontend.state().candidates;
        if !candidates.is_empty() {
            panel.draw_candidates(candidates, frontend.state().selected_candidate);
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    ExitCode::SUCCESS
}

fn parse_arg(flag: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == flag {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}
