//! Signal handlers and process-global callback trampolines.
//!
//! Split out of `mod.rs` so the daemon lifecycle file owns *what happens
//! on a signal* (drain, refresh, exit) rather than *how the kernel
//! delivers it*. The trampolines here are intentionally tiny: they do
//! the minimum async-signal-safe work (set a flag, send an mpsc message)
//! and let the main loop react on its own thread.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::DaemonEvent;

/// Async-signal-safe shutdown flag.
///
/// Only the SIGINT/SIGTERM handler writes this. The main loop translates
/// it into a daemon exit on the next tick. Non-signal paths
/// (`DaemonEvent::Shutdown` via the event channel) must NOT touch this
/// flag — keeping it signal-only preserves async-signal-safety.
pub(super) static SHUTDOWN_FROM_SIGNAL: AtomicBool = AtomicBool::new(false);

/// Process-global sender for the mode-changed callback. Stored in a
/// `OnceLock` because the C ABI callback holds a raw `user_data` pointer
/// that must be valid for the instance's lifetime, and there is only one
/// daemon per process. The `Mutex` makes `&Sender` safely shareable
/// across the engine communication thread (where out-of-process engine
/// responses fire the callback) and the main loop thread.
static MODE_CALLBACK_TX: OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<DaemonEvent>>> =
    OnceLock::new();

/// Install the sender used by [`mode_changed_trampoline`]. Called once
/// from [`crate::app::App::init`] after the daemon event channel is
/// wired. Subsequent calls are no-ops (the first sender wins), matching
/// the singleton nature of the trampoline.
pub(super) fn set_mode_callback_tx(tx: std::sync::mpsc::Sender<DaemonEvent>) {
    let _ = MODE_CALLBACK_TX.set(std::sync::Mutex::new(tx));
}

/// Swap the shutdown flag and return the previous value. Used by the
/// main loop's per-tick drain to translate a signal into the same exit
/// path as `DaemonEvent::Shutdown`.
pub(super) fn take_shutdown_requested() -> bool {
    SHUTDOWN_FROM_SIGNAL.swap(false, Ordering::Relaxed)
}

/// Reset the shutdown flag. Used by tests that touch the signal path.
#[cfg(test)]
pub(super) fn reset_shutdown_flag() {
    SHUTDOWN_FROM_SIGNAL.store(false, Ordering::SeqCst);
}

extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_FROM_SIGNAL.store(true, Ordering::SeqCst);
}

/// C trampoline for `TypioKeyboardModeChangedCallback`. Fires when an
/// engine reports a **deliberate** mode change (e.g. rime switching schema
/// or toggling 中/A). Marshals to the main loop via `DaemonEvent::StateRefresh`;
/// the main loop then reads the fresh mode from
/// `typio_instance_get_last_keyboard_mode` and triggers the indicator's
/// no-gate deliberate-change path.
///
/// The first parameter uses the **opaque** `typio_abi::TypioInstance`
/// (not `typio::instance::TypioInstance`) to match the callback typedef
/// exactly. The actual pointer is to the real struct; we never dereference
/// it here, so the opacity is harmless.
pub(super) extern "C" fn mode_changed_trampoline(
    _instance: *mut typio_abi::TypioInstance,
    _mode: *const typio_abi::TypioKeyboardEngineMode,
    _user_data: *mut c_void,
) {
    if let Some(mutex) = MODE_CALLBACK_TX.get() {
        if let Ok(tx) = mutex.lock() {
            let _ = tx.send(DaemonEvent::StateRefresh);
        }
    }
}

pub(super) fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
    }
}
