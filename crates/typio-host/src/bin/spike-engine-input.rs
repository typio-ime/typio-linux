//! Phase 8 spike: full IME pipeline — compositor → engine → compositor.
//!
//! Each key press from the keyboard grab is routed through libtypio's
//! engine via `typio_input_context_process_key`. If the engine consumes
//! the key, the commit callback fires and the committed text goes to
//! the compositor. If the engine doesn't handle it, the key falls
//! through to direct passthrough.
//!
//! ```sh
//! cargo run --bin spike-engine-input -- --engine-dir ../typio-engine-rime/build
//! ```

use std::ffi::{c_char, c_void, CStr, CString};
use std::process::ExitCode;
use std::sync::Mutex;

use typio_abi::{TypioEngineInfo, TypioEngineType, TypioEventType, TypioKeyEvent, TypioResult};

use typio_host::input_method::{InputMethodFrontend, LifecycleEvent};
use typio_host::engine_loader::manifest::EngineManifest;

static COMMITTED_TEXT: Mutex<Option<String>> = Mutex::new(None);

extern "C" fn on_commit(
    _ctx: *mut typio_abi::TypioInputContext,
    text: *const c_char,
    _user_data: *mut c_void,
) {
    if text.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(text) };
    let s = text.to_string_lossy().into_owned();
    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
        *slot = Some(s);
    }
}

fn main() -> ExitCode {
    eprintln!("typio-host: full IME pipeline spike");

    let engine_dir = parse_engine_dir();

    // 1. Create libtypio instance + init.
    let temp = std::env::temp_dir().join(format!("typio-engine-spike-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);

    let mut instance = typio::instance::TypioInstance::new_rust(
        temp.to_str(),
        temp.to_str(),
        temp.to_str(),
        Vec::new(),
    );

    if let Err(e) = instance.init_rust() {
        eprintln!("FAIL: init: {e:?}");
        return ExitCode::from(1);
    }
    eprintln!("OK: TypioInstance initialized");

    // Get the instance raw pointer for C ABI calls.
    let instance_ptr = instance.as_mut() as *mut _;

    // 2. Load engines into the instance's registry via C ABI.
    if let Some(ref dir) = engine_dir {
        let reg_ptr = typio::instance::typio_instance_get_registry(instance_ptr);

        // Scan the directory using our Rust engine_loader to discover
        // manifests, then register each via the C ABI because the
        // instance's registry is a TypioRegistry (C-side).
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("WARN: cannot read engine dir {dir}: {e}");
                return ExitCode::from(1);
            }
        };

        let mut registered = 0u32;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !name.starts_with("typio-engine-") || !name.ends_with(".toml") {
                continue;
            }

            // Parse the manifest.
            let manifest = match EngineManifest::read_from(&path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("  SKIP {}: {e}", path.display());
                    continue;
                }
            };

            // Build C strings for TypioEngineInfo fields.
            let c_name = CString::new(manifest.name.as_str()).unwrap();
            let c_display = CString::new(
                manifest.display_name.as_deref().unwrap_or(&manifest.name),
            ).unwrap();
            let c_desc = CString::new(manifest.description.as_deref().unwrap_or("")).unwrap();
            let c_author = CString::new(manifest.author.as_deref().unwrap_or("")).unwrap();
            let c_icon = manifest.icon.as_ref().map(|s| CString::new(s.as_str()).unwrap());
            let c_lang = CString::new(manifest.primary_language()).unwrap();

            // Build argv.
            let argv_strings: Vec<CString> = match manifest.argv(&path) {
                Ok(v) => v.into_iter().filter_map(|s| CString::new(s).ok()).collect(),
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

            let info = TypioEngineInfo {
                name: c_name.as_ptr(),
                display_name: c_display.as_ptr(),
                description: c_desc.as_ptr(),
                author: c_author.as_ptr(),
                icon: c_icon.as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null()),
                language: c_lang.as_ptr(),
                type_: if manifest.engine_type == "voice" {
                    TypioEngineType::TypioEngineTypeVoice
                } else {
                    TypioEngineType::TypioEngineTypeKeyboard
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
                registered += 1;
                // Leaks the CStrings — intentional; the engine process
                // backend holds these pointers for the daemon lifetime.
                std::mem::forget(c_name);
                std::mem::forget(c_display);
                std::mem::forget(c_desc);
                std::mem::forget(c_author);
                std::mem::forget(c_icon);
                std::mem::forget(c_lang);
                std::mem::forget(argv_strings);
            } else {
                eprintln!("  SKIP {}: register returned {result:?}", manifest.name);
            }
        }

        eprintln!("OK: {registered} engine(s) registered");

        // Activate the first keyboard engine.
        let mut kb_count: usize = 0;
        let kb_list = typio::c_api::registry::typio_registry_list_keyboards(
            reg_ptr,
            &mut kb_count,
        );
        if !kb_list.is_null() && kb_count > 0 {
            let first = unsafe { *kb_list };
            if !first.is_null() {
                let name = unsafe { CStr::from_ptr(first) };
                eprintln!("OK: activating first keyboard engine: {}", name.to_string_lossy());
                typio::c_api::registry::typio_registry_set_active_keyboard(reg_ptr, first);
            }
        }
    }

    // 3. Create input context + wire callbacks.
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
    typio::input_context::typio_input_context_focus_in(ctx);
    eprintln!("OK: input context ready");

    // 4. Connect to Wayland.
    let callback = Box::new(|event: LifecycleEvent| {
        if let LifecycleEvent::Key(ref ev) = event {
            eprintln!(
                "KEY {} sym=0x{:x} char={:?}",
                if ev.state == 1 { "↓" } else { "↑" },
                ev.keysym,
                ev.unicode
            );
        }
    });

    let mut frontend = match InputMethodFrontend::connect(Some(callback)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: Wayland: {e}");
            return ExitCode::from(1);
        }
    };

    eprintln!("OK: Wayland connected. Type into a text field. Ctrl+C to exit.");

    // 5. Event loop: dispatch → engine → commit.
    loop {
        if let Err(e) = frontend.dispatch() {
            eprintln!("dispatch error: {e}");
            break;
        }

        // Drain pending key: route through engine or direct passthrough.
        if let Some(key) = frontend.state_mut().pending_key.take() {
            // Clear any stale commit from a previous key.
            if let Ok(mut slot) = COMMITTED_TEXT.lock() {
                *slot = None;
            }

            // Construct TypioKeyEvent for the C ABI.
            let mods = frontend.state().mods_depressed;
            let event = TypioKeyEvent {
                struct_size: std::mem::size_of::<TypioKeyEvent>(),
                type_: if key.state == 1 {
                    TypioEventType::TypioEventKeyPress
                } else {
                    TypioEventType::TypioEventKeyRelease
                },
                keycode: key.keycode,
                keysym: key.keysym,
                modifiers: mods,
                unicode: key.unicode.chars().next().unwrap_or('\0') as u32,
                time: key.time as u64,
                is_repeat: false,
                base_keysym: key.keysym,
            };

            let consumed = typio::input_context::typio_input_context_process_key(
                ctx,
                &event,
            );

            if consumed {
                // Engine handled it — check for committed text.
                if let Ok(mut slot) = COMMITTED_TEXT.lock() {
                    if let Some(text) = slot.take() {
                        eprintln!("  ENGINE → commit: {text:?}");
                        frontend.state_mut().commit_string_and_flush(&text);
                    }
                }
            } else if key.state == 1 {
                // Engine didn't handle it — forward to the focused app
                // via the virtual keyboard so the keystroke doesn't
                // disappear.
                eprintln!("  FORWARD keycode={}", key.keycode);
                frontend.state().forward_key(key.time, key.keycode, 1);
            }
        }

        // Also check for commit text that arrived asynchronously.
        if let Ok(mut slot) = COMMITTED_TEXT.lock() {
            if let Some(text) = slot.take() {
                frontend.state_mut().commit_string_and_flush(&text);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    let _ = std::fs::remove_dir_all(&temp);
    ExitCode::SUCCESS
}

fn parse_engine_dir() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--engine-dir" {
            return args.next();
        }
        if let Some(rest) = a.strip_prefix("--engine-dir=") {
            return Some(rest.to_string());
        }
    }
    None
}
