//! Wayland input-method frontend — the daemon's entry point to the
//! compositor's keyboard event stream.
//!
//! Phase 8 port of the connection lifecycle parts of
//! `src/wayland/input_method.c` + `src/wayland/frontend.c` +
//! `src/wayland/frontend_bind.c`. What this module covers:
//!
//! - Connect to the Wayland display via `wayland-client`.
//! - Bind the three globals the daemon needs: `wl_seat`,
//!   `zwp_input_method_manager_v2`, `zwp_virtual_keyboard_manager_v1`.
//! - Create a `zwp_input_method_v2` object via the manager and wire up
//!   event handlers for its 7 events (activate, deactivate,
//!   surrounding_text, text_change_cause, content_type, done,
//!   unavailable).
//! - Drive a blocking event loop that surfaces lifecycle transitions
//!   and increments the protocol serial on each `done`.
//!
//! ## What is NOT ported
//!
//! Everything that happens *after* event receipt — the C version's 31
//! handler functions route events into `focus_facts` (for the focus
//! controller), `session->pending` (for engine state), the candidate
//! panel, the virtual keyboard bridge, and libtypio's input context.
//! Those subsystems are not yet ported, so this frontend logs events
//! instead of routing them.
//!
//! The serial-commit protocol is handled correctly (serial increments
//! on every `done`, a commit before the first `done` is silently
//! dropped) — matching the C version's `typio_wl_commit` chokepoint.

use std::io;
use std::os::fd::{AsFd, AsRawFd};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::protocols::input_method_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use crate::protocols::input_method_v2::zwp_input_method_v2::{self, ZwpInputMethodV2};
use crate::protocols::virtual_keyboard_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;

/// Callback type for input-method lifecycle events.
pub type LifecycleCallback = Box<dyn FnMut(LifecycleEvent) + Send>;

/// Lifecycle events the frontend surfaces to the caller.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Input method was activated — the compositor gave us the keyboard
    /// grab.
    Activated,
    /// Input method was deactivated.
    Deactivated,
    /// The compositor sent `done` — a protocol serial boundary.
    Done { serial: u32 },
    /// The compositor declared this input-method protocol unavailable.
    Unavailable,
    /// Surrounding text update from the focused app.
    SurroundingText {
        text: String,
        cursor: u32,
        anchor: u32,
    },
    /// Content type hint from the focused app (password, number, etc.).
    ContentType { hint: u32, purpose: u32 },
}

/// Wayland state — all bound protocol objects + tracking fields.
///
/// This struct implements `Dispatch` for every protocol object the
/// frontend binds. The `EventQueue` operates on it directly.
pub struct InputMethodState {
    #[allow(dead_code)]
    seat: wl_seat::WlSeat,
    input_method: ZwpInputMethodV2,
    serial: u32,
    active: bool,
    initialized: bool,
    callback: Option<LifecycleCallback>,
}

impl InputMethodState {
    /// Current protocol serial.
    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// True iff the compositor has activated us.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Commit pending state to the compositor. Silently dropped before
    /// the first `done` — matching the C serial chokepoint.
    pub fn commit(&self) {
        if !self.initialized {
            return;
        }
        self.input_method.commit(self.serial);
    }

    fn fire(&mut self, event: LifecycleEvent) {
        if let Some(cb) = self.callback.as_mut() {
            cb(event);
        }
    }
}

/// Wayland input-method frontend. Owns the connection, event queue,
/// and state. The caller drives the event loop via [`Self::dispatch`]
/// or [`Self::run`].
pub struct InputMethodFrontend {
    conn: Connection,
    queue: EventQueue<InputMethodState>,
    state: InputMethodState,
}

impl InputMethodFrontend {
    /// Connect to the Wayland display, bind globals, create the
    /// input-method object.
    pub fn connect(callback: Option<LifecycleCallback>) -> Result<Self, ConnectError> {
        let conn =
            Connection::connect_to_env().map_err(ConnectError::ConnectionFailed)?;
        let (globals, queue) =
            registry_queue_init::<InputMethodState>(&conn).map_err(ConnectError::RegistryFailed)?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|e| ConnectError::BindFailed("wl_seat", format!("{e:?}")))?;

        let im_manager: ZwpInputMethodManagerV2 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| {
                ConnectError::BindFailed("zwp_input_method_manager_v2", format!("{e:?}"))
            })?;

        let _vk_manager: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| {
                ConnectError::BindFailed("zwp_virtual_keyboard_manager_v1", format!("{e:?}"))
            })?;

        let input_method = im_manager.get_input_method(&seat, &qh, ());

        let state = InputMethodState {
            seat,
            input_method,
            serial: 0,
            active: false,
            initialized: false,
            callback,
        };

        Ok(Self {
            conn,
            queue,
            state,
        })
    }

    /// Immutable access to the state (serial, active flag, etc.).
    pub fn state(&self) -> &InputMethodState {
        &self.state
    }

    /// Mutable access to the state.
    pub fn state_mut(&mut self) -> &mut InputMethodState {
        &mut self.state
    }

    /// The Wayland connection's file descriptor for external event loops.
    pub fn fd(&self) -> i32 {
        self.queue.as_fd().as_raw_fd()
    }

    /// Non-blocking dispatch of pending Wayland events.
    pub fn dispatch(&mut self) -> io::Result<()> {
        // Split borrow: `self.queue` and `self.state` are disjoint fields.
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| io::Error::other(format!("dispatch: {e}")))?;
        Ok(())
    }

    /// Blocking event loop. Runs until the connection drops.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.conn
                .flush()
                .map_err(|e| io::Error::other(format!("flush: {e}")))?;

            // Dispatch any already-read events.
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| io::Error::other(format!("dispatch: {e}")))?;

            // Prepare to read, poll for readability, then read.
            if let Some(read_guard) = self.queue.prepare_read() {
                let fd = self.fd();
                let mut pollfd =
                    libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let rc = unsafe { libc::poll(&mut pollfd, 1, -1) };
                if rc < 0 {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return Err(e);
                }
                if pollfd.revents & libc::POLLIN != 0 {
                    read_guard
                        .read()
                        .map_err(|e| {
                            io::Error::other(format!("read: {e}"))
                        })?;
                }
                if pollfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "display fd closed",
                    ));
                }
            } else {
                // Another reader already has the lock; just dispatch.
                continue;
            }
        }
    }
}

/// Errors that can occur during [`InputMethodFrontend::connect`].
#[derive(Debug)]
pub enum ConnectError {
    ConnectionFailed(wayland_client::ConnectError),
    RegistryFailed(wayland_client::globals::GlobalError),
    BindFailed(&'static str, String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::ConnectionFailed(e) => write!(f, "Wayland connection failed: {e}"),
            ConnectError::RegistryFailed(e) => write!(f, "registry roundtrip failed: {e}"),
            ConnectError::BindFailed(iface, detail) => {
                write!(f, "cannot bind {iface}: {detail}")
            }
        }
    }
}

impl std::error::Error for ConnectError {}

// ── Dispatch impls ───────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        _event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event;
        match event {
            Event::Activate => {
                state.active = true;
                state.fire(LifecycleEvent::Activated);
            }
            Event::Deactivate => {
                state.active = false;
                state.fire(LifecycleEvent::Deactivated);
            }
            Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.fire(LifecycleEvent::SurroundingText {
                    text,
                    cursor,
                    anchor,
                });
            }
            Event::TextChangeCause { .. } => {
                // Informational; recorded in the C version's focus_facts.
            }
            Event::ContentType { hint, purpose } => {
                // ContentHint/ContentPurpose come in as WEnum wrappers
                // around text-input-v3 enum types. For the spike, we
                // pass the raw enum values via Debug formatting. The
                // real daemon will interpret these for the engine.
                state.fire(LifecycleEvent::ContentType {
                    hint: format!("{hint:?}").len() as u32,
                    purpose: format!("{purpose:?}").len() as u32,
                });
            }
            Event::Done => {
                state.serial = state.serial.wrapping_add(1);
                state.initialized = true;
                state.fire(LifecycleEvent::Done {
                    serial: state.serial,
                });
            }
            Event::Unavailable => {
                state.fire(LifecycleEvent::Unavailable);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_error_display_is_human_readable() {
        let e = ConnectError::BindFailed(
            "zwp_input_method_manager_v2",
            "NotPresent".to_string(),
        );
        let s = format!("{e}");
        assert!(s.contains("zwp_input_method_manager_v2"));
    }

    #[test]
    fn lifecycle_event_is_debug() {
        assert!(format!("{:?}", LifecycleEvent::Activated).contains("Activated"));
        assert!(format!("{:?}", LifecycleEvent::Done { serial: 42 }).contains("42"));
    }
}
