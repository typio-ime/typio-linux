//! Phase 8 spike: bind zwp_input_method_v2 on a live compositor.
//!
//! Run with:
//! ```sh
//! cargo run --bin spike-input-method
//! ```
//!
//! Then focus a text field in any app. You should see ACTIVATE / DEACTIVATE
//! transitions, SURROUNDING TEXT updates, and DONE serials being printed.

use std::process::ExitCode;

use typio_host::input_method::{InputMethodFrontend, LifecycleEvent};

fn main() -> ExitCode {
    eprintln!("typio-host Phase 8 spike: binding zwp_input_method_v2");

    let callback = Box::new(|event: LifecycleEvent| {
        match event {
            LifecycleEvent::Activated => {
                eprintln!("EVENT: ACTIVATE — compositor gave us the keyboard grab");
            }
            LifecycleEvent::Deactivated => {
                eprintln!("EVENT: DEACTIVATE — grab released");
            }
            LifecycleEvent::Done { serial } => {
                eprintln!("EVENT: done (serial={serial})");
            }
            LifecycleEvent::Unavailable => {
                eprintln!("EVENT: UNAVAILABLE — another IME grabbed the seat");
            }
            LifecycleEvent::SurroundingText { text, cursor, anchor } => {
                eprintln!(
                    "EVENT: surrounding_text cursor={cursor} anchor={anchor} text={text:?}"
                );
            }
            LifecycleEvent::ContentType { hint, purpose } => {
                eprintln!("EVENT: content_type hint={hint} purpose={purpose}");
            }
            LifecycleEvent::Key(ev) => {
                let state_str = if ev.state == 1 { "PRESS" } else { "RELEASE" };
                eprintln!(
                    "KEY {state_str} keycode={} keysym=0x{:x} unicode={:?} time={}",
                    ev.keycode, ev.keysym, ev.unicode, ev.time
                );
            }
            LifecycleEvent::RepeatInfo { rate, delay } => {
                eprintln!("EVENT: repeat_info rate={rate} delay={delay}");
            }
        }
    });

    let mut frontend = match InputMethodFrontend::connect(Some(callback)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("FAIL: cannot connect: {e}");
            return ExitCode::from(1);
        }
    };

    eprintln!("OK: connected, input_method_v2 bound. Waiting for events...");
    eprintln!("(focus a text field to see ACTIVATE; Ctrl+C to exit)");

    match frontend.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::from(2)
        }
    }
}
