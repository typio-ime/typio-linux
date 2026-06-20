//! Phase 8 spike: full IME pipeline — compositor → engine → compositor.
//!
//! Combines the input-method frontend (keyboard grab + xkbcommon) with
//! libtypio's engine registry. Key presses are routed through the loaded
//! engine; if the engine produces a commit, it goes to the compositor.
//!
//! ## Try it
//!
//! ```sh
//! cargo run --bin spike-engine-input -- --engine-dir ../typio-engine-rime/build
//! # focus a text field in another app and type
//! ```

use std::ffi::{c_char, c_void, CStr};
use std::process::ExitCode;
use std::sync::Mutex;

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
    let text = text.to_string_lossy().into_owned();
    if let Ok(mut slot) = COMMITTED_TEXT.lock() {
        *slot = Some(text);
    }
}

fn main() -> ExitCode {
    eprintln!("typio-host Phase 8 spike: full IME pipeline");

    let engine_dir = parse_engine_dir().unwrap_or_else(|| {
        eprintln!("No --engine-dir given; running in passthrough mode.");
        String::new()
    });

    // 1. Create libtypio instance.
    let temp = std::env::temp_dir().join(format!("typio-engine-spike-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&temp);

    let mut instance = typio::instance::TypioInstance::new_rust(
        temp.to_str(),
        temp.to_str(),
        temp.to_str(),
        Vec::new(),
    );

    if let Err(e) = instance.init_rust() {
        eprintln!("FAIL: TypioInstance::init_rust: {e:?}");
        return ExitCode::from(1);
    }
    eprintln!("OK: TypioInstance initialized");

    // 2. Load engines (scan the dir via our Rust engine_loader to
    //    see what's available, then report).
    if !engine_dir.is_empty() {
        let mut loader = EngineLoader::with_voice();
        let mut temp_registry = typio::core::registry::EngineRegistry::new();
        let report = loader.load_dir(&mut temp_registry, std::path::Path::new(&engine_dir));
        eprintln!(
            "Engine dir {}: {} registered, {} skipped, {} failed",
            engine_dir,
            report.registered,
            report.skipped.len(),
            report.failed.len()
        );
    }

    // 3. Create input context.
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
    eprintln!("OK: input context ready, commit callback installed");

    // 4. Connect to Wayland.
    let callback = Box::new(|_event: LifecycleEvent| {
        // Logging happens in the event loop driver below.
    });

    let mut frontend = match InputMethodFrontend::connect(Some(callback)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: Wayland connect: {e}");
            return ExitCode::from(1);
        }
    };

    eprintln!("OK: Wayland connected, keyboard grabbed");
    eprintln!("Focus a text field and type. Ctrl+C to exit.");

    // 5. Event loop: dispatch + drain engine commits.
    loop {
        if let Err(e) = frontend.dispatch() {
            eprintln!("dispatch error: {e}");
            break;
        }

        // Drain any text committed by the engine.
        if let Ok(mut slot) = COMMITTED_TEXT.lock() {
            if let Some(text) = slot.take() {
                eprintln!("ENGINE COMMIT: {text:?}");
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
