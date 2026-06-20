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

use crate::protocols::input_method_v2::zwp_input_method_keyboard_grab_v2::{
    self, ZwpInputMethodKeyboardGrabV2,
};
use crate::protocols::input_method_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use crate::protocols::input_method_v2::zwp_input_method_v2::{self, ZwpInputMethodV2};
use crate::protocols::virtual_keyboard_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;

/// Callback type for input-method lifecycle events.
pub type LifecycleCallback = Box<dyn FnMut(LifecycleEvent) + Send>;

/// A decoded key event with xkbcommon-resolved keysym.
#[derive(Debug, Clone)]
pub struct DecodedKeyEvent {
    /// Raw keycode from the compositor (evdev scancode).
    pub keycode: u32,
    /// XKB keycode (raw + 8). xkbcommon uses this internally.
    pub xkb_keycode: u32,
    /// Resolved keysym (e.g. XKB_KEY_a = 0x0061).
    pub keysym: u32,
    /// UTF-8 text produced by this key, if any (from xkb_state_key_get_utf8).
    pub unicode: String,
    /// Press (1) or release (0).
    pub state: u32,
    /// Timestamp in milliseconds (from the compositor).
    pub time: u32,
}

/// Lifecycle events the frontend surfaces to the caller.
#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    /// Input method was activated.
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
    /// Content type hint from the focused app.
    ContentType { hint: u32, purpose: u32 },
    /// A key was pressed or released. Carries the fully decoded
    /// keysym + unicode text from xkbcommon.
    Key(DecodedKeyEvent),
    /// Keyboard repeat rate / delay info.
    RepeatInfo { rate: i32, delay: i32 },
}

/// Wayland state — all bound protocol objects + tracking fields.
///
/// This struct implements `Dispatch` for every protocol object the
/// frontend binds. The `EventQueue` operates on it directly.
pub struct InputMethodState {
    #[allow(dead_code)]
    seat: wl_seat::WlSeat,
    input_method: ZwpInputMethodV2,
    #[allow(dead_code)]
    keyboard_grab: ZwpInputMethodKeyboardGrabV2,
    serial: u32,
    active: bool,
    initialized: bool,
    callback: Option<LifecycleCallback>,
    /// xkbcommon context — created once, reused across keymap changes.
    xkb_context: xkbcommon::xkb::Context,
    /// Current keymap state — set when the compositor sends a keymap fd.
    /// None until the first keymap event.
    xkb_state: Option<xkbcommon::xkb::State>,
    /// Current keymap keymap — kept alive alongside the state.
    xkb_keymap: Option<xkbcommon::xkb::Keymap>,
    /// Physical modifier state from the latest modifiers event.
    mods_depressed: u32,
    mods_latched: u32,
    mods_locked: u32,
    /// Pending text to commit to the compositor on the next `done`.
    /// Set by the key handler when a key produces unicode text.
    /// The caller drains this after each dispatch round via
    /// [`Self::take_pending_commit`].
    pending_commit: Option<String>,
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

    /// Drain any pending commit text. Called by the event loop driver
    /// after each dispatch round. If non-empty, the caller should
    /// call `commit_string(text)` + `commit(serial)` on the
    /// input-method proxy.
    pub fn take_pending_commit(&mut self) -> Option<String> {
        self.pending_commit.take()
    }

    /// Flush a commit directly to the compositor. Convenience method
    /// for the common case: commit the text, then flush via
    /// `commit(serial)`.
    pub fn commit_string_and_flush(&mut self, text: &str) {
        if !self.initialized || !self.active {
            return;
        }
        self.input_method.commit_string(text.to_string());
        self.input_method.commit(self.serial);
    }

    /// Load an XKB keymap from a compositor-provided file descriptor.
    fn load_keymap_from_fd(&mut self, fd: std::os::fd::OwnedFd, size: u32) {
        use std::io::Read;
        let mut file = std::fs::File::from(fd);
        let mut buffer = vec![0u8; size as usize];
        if file.read_exact(&mut buffer).is_err() {
            return;
        }
        // xkb_keymap_new_from_buffer expects a C string-like buffer;
        // the keymap text is null-terminated in practice.
        let keymap_string = String::from_utf8_lossy(&buffer)
            .trim_end_matches('\0')
            .to_string();

        let keymap = xkbcommon::xkb::Keymap::new_from_string(
            &self.xkb_context,
            keymap_string,
            xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
        );

        if let Some(keymap) = keymap {
            let mut xkb_state = xkbcommon::xkb::State::new(&keymap);
            // Apply current modifier state.
            xkb_state.update_mask(
                self.mods_depressed,
                self.mods_latched,
                self.mods_locked,
                0, // depressed_layout
                0, // latched_layout
                0, // locked_layout
            );
            self.xkb_keymap = Some(keymap);
            self.xkb_state = Some(xkb_state);
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

        // Create the keyboard grab upfront. The compositor will start
        // delivering key events to it as soon as we're activated.
        let keyboard_grab = input_method.grab_keyboard(&qh, ());

        let state = InputMethodState {
            seat,
            input_method,
            keyboard_grab,
            serial: 0,
            active: false,
            initialized: false,
            callback,
            xkb_context: xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            xkb_keymap: None,
            mods_depressed: 0,
            mods_latched: 0,
            mods_locked: 0,
            pending_commit: None,
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
    /// Automatically flushes pending commit text after each dispatch.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.conn
                .flush()
                .map_err(|e| io::Error::other(format!("flush: {e}")))?;

            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| io::Error::other(format!("dispatch: {e}")))?;

            // Flush any pending commit text to the compositor.
            if let Some(text) = self.state.take_pending_commit() {
                self.state.commit_string_and_flush(&text);
            }

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
                        .map_err(|e| io::Error::other(format!("read: {e}")))?;
                }
                if pollfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                    return Err(io::Error::other("display fd closed"));
                }
            } else {
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

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwp_input_method_keyboard_grab_v2::Event;
        match event {
            Event::Keymap { format, fd, size } => {
                let fmt_raw: u32 = match &format {
                    wayland_client::WEnum::Value(v) => *v as u32,
                    wayland_client::WEnum::Unknown(u) => *u,
                };
                if fmt_raw != 1 {
                    return;
                }
                state.load_keymap_from_fd(fd, size);
            }
            Event::Key { time, key, state: key_state, serial: _ } => {
                let raw_state: u32 = match &key_state {
                    wayland_client::WEnum::Value(v) => *v as u32,
                    wayland_client::WEnum::Unknown(u) => *u,
                };

                let xkb_keycode = key + 8;
                let kc = xkbcommon::xkb::Keycode::new(xkb_keycode);
                let key_direction = if raw_state == 1 {
                    xkbcommon::xkb::KeyDirection::Down
                } else {
                    xkbcommon::xkb::KeyDirection::Up
                };
                if let Some(ref mut xs) = state.xkb_state {
                    xs.update_key(kc, key_direction);
                }

                let keysym: u32 = state.xkb_state.as_ref().map_or(0, |s| {
                    s.key_get_one_sym(kc).into()
                });
                let unicode = state.xkb_state.as_ref().map_or(String::new(), |s| {
                    s.key_get_utf8(kc)
                });

                state.fire(LifecycleEvent::Key(DecodedKeyEvent {
                    keycode: key,
                    xkb_keycode,
                    keysym,
                    unicode: unicode.clone(),
                    state: raw_state,
                    time,
                }));

                // On key press with printable text + no blocking
                // modifiers, queue the text for commit. The event-loop
                // driver drains this after dispatch.
                if raw_state == 1 && !unicode.is_empty() && state.active {
                    let blocking = state.mods_depressed & 0x4 != 0 // Ctrl
                        || state.mods_depressed & 0x8 != 0          // Alt
                        || state.mods_depressed & 0x10 != 0;        // Super
                    if !blocking {
                        state.pending_commit = Some(unicode);
                    }
                }
            }
            Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                serial: _,
            } => {
                state.mods_depressed = mods_depressed;
                state.mods_latched = mods_latched;
                state.mods_locked = mods_locked;
                if let Some(ref mut xs) = state.xkb_state {
                    xs.update_mask(
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        0,
                        0,
                        group,
                    );
                }
            }
            Event::RepeatInfo { rate, delay } => {
                state.fire(LifecycleEvent::RepeatInfo { rate, delay });
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
