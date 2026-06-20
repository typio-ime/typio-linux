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

use std::ffi::{c_char, c_void, CStr};
use std::process::ExitCode;
use std::sync::Mutex;

use typio_abi::{TypioEventType, TypioKeyEvent};

use typio_host::engine_loader::EngineLoader;
use typio_host::input_method::{InputMethodFrontend, LifecycleEvent};

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

    // 2. Load engines into the instance's registry.
    if let Some(ref dir) = engine_dir {
        let instance_ptr = instance.as_mut() as *mut _;
        let reg_ptr = typio::instance::typio_instance_get_registry(instance_ptr);
        if !reg_ptr.is_null() {
            let mut loader = EngineLoader::with_voice();
            let report = loader.load_dir(
                &mut typio::core::registry::EngineRegistry::new(),
                std::path::Path::new(dir),
            );
            eprintln!(
                "Engine scan {}: {} ok, {} skip, {} fail",
                dir,
                report.registered,
                report.skipped.len(),
                report.failed.len()
            );
        }
    }

    // 3. Create input context + wire callbacks.
    let instance_ptr = instance.as_mut() as *mut _;
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
            } else if !key.unicode.is_empty() && key.state == 1 {
                // Engine didn't handle it — direct passthrough.
                let blocking = mods & 0x4 != 0 || mods & 0x8 != 0 || mods & 0x10 != 0;
                if !blocking {
                    eprintln!("  PASS → {key:?}");
                    frontend.state_mut().commit_string_and_flush(&key.unicode);
                }
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
