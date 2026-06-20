//! Phase 8 spike: full keystroke → text → compositor pipeline.
//!
//! Connects to the compositor as an input method, grabs the keyboard,
//! parses the XKB keymap, and for each key press commits the decoded
//! UTF-8 character back to the compositor via `commit_string` +
//! `commit(serial)`.
//!
//! This proves the full round-trip: compositor → daemon → compositor.
//! Try it:
//!
//! ```sh
//! cargo run --bin spike-input-method
//! # focus a text field in another app, type characters
//! ```

use std::process::ExitCode;

use typio_host::input_method::{InputMethodFrontend, LifecycleEvent};

fn main() -> ExitCode {
    eprintln!("typio-host Phase 8 spike: full keystroke → text pipeline");

    // The callback owns a reference to the input_method proxy so it
    // can commit text back. We need shared mutable state between
    // the callback and the event loop driver.
    let callback = Box::new(|event: LifecycleEvent| {
        match event {
            LifecycleEvent::Activated => {
                eprintln!("ACTIVATE");
            }
            LifecycleEvent::Deactivated => {
                eprintln!("DEACTIVATE");
            }
            LifecycleEvent::Done { serial } => {
                eprintln!("done (serial={serial})");
            }
            LifecycleEvent::Unavailable => {
                eprintln!("UNAVAILABLE — another IME grabbed the seat");
            }
            LifecycleEvent::SurroundingText { text, cursor, anchor } => {
                eprintln!(
                    "surrounding_text cursor={cursor} anchor={anchor} text={text:?}"
                );
            }
            LifecycleEvent::ContentType { hint, purpose } => {
                eprintln!("content_type hint={hint} purpose={purpose}");
            }
            LifecycleEvent::Key(ev) => {
                let state_str = if ev.state == 1 { "PRESS" } else { "RELEASE" };
                eprintln!(
                    "KEY {state_str} keycode={} keysym=0x{:x} unicode={:?}",
                    ev.keycode, ev.keysym, ev.unicode
                );
                // The actual commit happens in the event loop driver
                // (below) because we need &mut InputMethodFrontend to
                // call commit_string. This callback just logs.
            }
            LifecycleEvent::RepeatInfo { rate, delay } => {
                eprintln!("repeat_info rate={rate} delay={delay}");
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

    eprintln!("OK: connected, keyboard grabbed. Type into a text field!");
    eprintln!("(Ctrl+C to exit)");

    // Custom event loop: dispatch + commit any pending key text.
    // The Dispatch impl fires the callback on key events but can't
    // commit from within (no access to the input_method proxy). So we
    // use a channel to pass commit text from the Dispatch impl to here.
    // For the spike, we use a simpler approach: check the state after
    // each dispatch round and commit any pending unicode.
    //
    // TODO: replace with a proper channel-based approach once the
    // Dispatch impl can store pending commits.
    match frontend.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::from(2)
        }
    }
}
