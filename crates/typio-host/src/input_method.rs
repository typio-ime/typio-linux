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
use std::time::{Duration, Instant};

use wayland_backend::client::ReadEventsGuard;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::{wl_callback, wl_keyboard, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::focus_controller::InputFacts;
use crate::panel::FluxPanel;
use crate::panel_coordinator::PanelCoordinator;
use crate::panel_scheduler::{self, PanelScheduleState};
use crate::protocols::input_method_v2::zwp_input_method_keyboard_grab_v2::{
    self, ZwpInputMethodKeyboardGrabV2,
};
use crate::protocols::input_method_v2::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use crate::protocols::input_method_v2::zwp_input_method_v2::{self, ZwpInputMethodV2};
use crate::protocols::input_method_v2::zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2};
use crate::protocols::viewporter::wp_viewport::WpViewport;
use crate::protocols::viewporter::wp_viewporter::WpViewporter;
use crate::protocols::virtual_keyboard_v1::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use crate::protocols::virtual_keyboard_v1::zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1};

/// Callback type for input-method lifecycle events.
pub type LifecycleCallback = Box<dyn FnMut(LifecycleEvent) + Send>;

/// A decoded key event with xkbcommon-resolved keysym.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Pending/current text-state carried across the input-method `done` boundary.
#[derive(Debug, Clone, Default)]
pub struct SessionState {
    /// The compositor says this text field is active.
    pub active: bool,
    /// Surrounding text from the focused application.
    pub surrounding_text: Option<String>,
    /// Cursor position in characters.
    pub cursor: u32,
    /// Anchor position in characters.
    pub anchor: u32,
    /// Content hint from the focused application.
    pub content_hint: u32,
    /// Content purpose from the focused application.
    pub content_purpose: u32,
    /// `text_change_cause` from the focused application.
    pub text_change_cause: u32,
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

/// Engine composition projection: what the panel should render and
/// what commit text (if any) is pending. Updated atomically by
/// `KeyboardRouter::drain_composition` (engine → host) and by
/// `KeyboardRouter::try_host_selection` (host-local highlight moves
/// under ADR-0012). The host's preedit text is *not* part of this
/// struct: it is forwarded to the compositor immediately via
/// `set_preedit_and_flush` and tracked by `KeyboardRouter::preedit_tracking`.
#[derive(Debug, Default)]
pub struct CompositionState {
    /// Current candidate list for the panel to render.
    pub candidates: Vec<String>,
    /// Index of the highlighted candidate.
    pub selected_candidate: usize,
    /// Engine-declared host-managed-selection flags (ADR-0012). When
    /// non-empty, the host intercepts the corresponding
    /// navigation/selection keys via [`crate::candidate_guard`] instead
    /// of forwarding them to `process_key`. Empty (opt-out) by default.
    pub host_managed_selection: crate::candidate_guard::HostSelectionFlags,
    /// Monotonic sequence bumped on every composition change — including
    /// host-local highlight moves — so observers can dedupe.
    pub composition_seq: u64,
    /// Pending commit text for the next flush. Set by the engine commit
    /// callback; taken by the event loop after each dispatch round.
    pub pending_commit: Option<String>,
}

impl CompositionState {
    /// Set the current candidate list + selected index. Bumps the
    /// composition sequence and returns the new value for logging.
    pub fn set_candidates(&mut self, candidates: Vec<String>, selected: usize) -> u64 {
        self.composition_seq = self.composition_seq.wrapping_add(1);
        self.candidates = candidates;
        self.selected_candidate = selected;
        self.composition_seq
    }

    /// Reset all composition state (focus lost / composition discarded).
    pub fn clear(&mut self) {
        self.candidates.clear();
        self.selected_candidate = 0;
        self.host_managed_selection = crate::candidate_guard::HostSelectionFlags::empty();
        self.pending_commit = None;
        // Note: composition_seq is monotonic across resets so observers
        // don't see a seq regression; do not bump or zero it here.
    }

    /// Take the pending commit text, if any.
    pub fn take_pending_commit(&mut self) -> Option<String> {
        self.pending_commit.take()
    }

    /// Stage a commit text from the engine.
    pub fn set_pending_commit(&mut self, text: String) {
        self.pending_commit = Some(text);
    }
}

/// Maximum wait for a `wl_surface.frame` callback before the panel
/// stops throttling and presents anyway. Real compositors ack a visible
/// surface every refresh (~16 ms); the fallback only kicks in when the
/// compositor drops the callback (off-screen popup, broken IME surface
/// handling). Sized well above the poll cadence so a healthy compositor
/// always wins, short enough that a dropped callback never stalls the
/// panel long enough to approach the watchdog.
const FRAME_CALLBACK_FALLBACK: Duration = Duration::from_millis(120);

/// Wayland state — all bound protocol objects + tracking fields.
///
/// This struct implements `Dispatch` for every protocol object the
/// frontend binds. The `EventQueue` operates on it directly.
pub struct InputMethodState {
    #[allow(dead_code)]
    seat: wl_seat::WlSeat,
    input_method: ZwpInputMethodV2,
    /// Keyboard grab object; recreated by the focus controller on hard
    /// boundaries and retained during a soft pause.
    keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    /// Virtual keyboard for forwarding unhandled keys to the focused app.
    virtual_keyboard: ZwpVirtualKeyboardV1,
    /// Compositor proxy (for creating panel surfaces later).
    #[allow(dead_code)]
    compositor: WlCompositor,
    /// The wl_surface backing the candidate panel popup.
    popup_surface_obj: wl_surface::WlSurface,
    /// `wl_callback` armed via `wl_surface.frame` after each present.
    /// The compositor fires it once it has consumed the presented
    /// buffer. Held alive until the `done` event clears the pending
    /// flag below.
    panel_frame_callback: Option<wl_callback::WlCallback>,
    /// True between a present and the matching `wl_surface.frame`
    /// `done` callback. While set, the panel flush path skips
    /// presenting (coalescing candidate updates into the next frame)
    /// so the daemon never submits faster than the compositor can
    /// release swapchain images — the back-pressure that makes the
    /// synchronous `vkQueuePresentKHR` in flux block the main loop
    /// for >15 s during rapid candidate paging, tripping the
    /// watchdog. See [`Self::panel_present_blocked`].
    pub panel_frame_pending: bool,
    /// When the current outstanding frame callback was armed. Bounds
    /// the wait so a compositor that drops the callback (off-screen
    /// popup, broken IME surface handling) cannot stall the panel
    /// forever; see [`FRAME_CALLBACK_FALLBACK`].
    panel_frame_requested_at: Option<Instant>,
    /// The input-method popup surface (positioning protocol).
    #[allow(dead_code)]
    popup_surface: ZwpInputPopupSurfaceV2,
    /// `wp_viewporter` global. Bound when the compositor advertises it;
    /// `None` falls back to exact-size resize (the legacy path described
    /// by ADR-0013).
    #[allow(dead_code)]
    viewporter: Option<WpViewporter>,
    /// `wp_viewport` attached to `popup_surface_obj`. Used by the panel
    /// to crop an oversized, grow-only swapchain to the exact content
    /// rect (ADR-0013).
    #[allow(dead_code)]
    panel_viewport: Option<WpViewport>,
    /// Text input rectangle from the compositor (cursor position).
    pub text_input_rect: Option<(i32, i32, i32, i32)>,
    /// Engine composition projection (candidates, selection, commit).
    pub composition: CompositionState,
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
    pub mods_depressed: u32,
    pub mods_latched: u32,
    pub mods_locked: u32,
    /// Pending key events to be processed by the engine after dispatch.
    /// Set by the Dispatch impl when a key press or release arrives;
    /// the event-loop driver drains all of them after dispatch_pending
    /// returns.
    ///
    /// This is a queue, not a single slot, so that two key events
    /// delivered in the same Wayland dispatch batch — most commonly
    /// `release(BS)` followed immediately by `press(other)` or
    /// vice-versa — are both preserved. With a single `Option` slot,
    /// the second event overwrote the first and the lost event was
    /// usually the release of a key whose repeat timer was armed;
    /// the daemon then repeated that key forever (the "stuck
    /// backspace" symptom). The queue guarantees release events
    /// always reach the loop, so `router.on_release` +
    /// `timer.stop()` fire on every release.
    pub pending_keys: Vec<DecodedKeyEvent>,
    /// Raw input facts recorded this tick for the focus controller.
    pub facts: InputFacts,
    /// Set when the compositor declares the input method unavailable.
    stopped: bool,
    /// Pending text state accumulated since the previous `done`.
    pending: SessionState,
    /// Text state committed by the latest `done`.
    current: SessionState,
    /// Optional libtypio input context used to apply surrounding text.
    input_context: Option<*mut typio::TypioInputContext>,
    /// True once a keymap event has been received for the current grab epoch.
    pub keymap_received_this_epoch: bool,
    /// Panel redraw scheduling state.
    pub panel_schedule_state: PanelScheduleState,
    /// Panel coordinator: anchor probing, caret fallback, and popup ownership.
    pub panel_coord: PanelCoordinator,
    pub buffer_scale: f32,
    /// Latest compositor-reported repeat info `(rate, delay_ms)` from the
    /// grab's `repeat_info` event. `None` until the compositor sends it;
    /// the host falls back to X-server defaults ([`crate::repeat_timer`]
    /// constants) until then. A rate of `0` is the protocol signal for
    /// "do not repeat".
    pub compositor_repeat_info: Option<(i32, i32)>,
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

    /// Mutable access to the raw input facts for this tick.
    pub fn facts_mut(&mut self) -> &mut InputFacts {
        &mut self.facts
    }

    /// Take the recorded facts so the focus controller can consume them.
    pub fn take_facts(&mut self) -> InputFacts {
        std::mem::take(&mut self.facts)
    }

    /// True if the compositor declared the input method unavailable.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Provide the libtypio input context used to apply surrounding text.
    pub fn set_input_context(&mut self, ctx: *mut typio::TypioInputContext) {
        self.input_context = if ctx.is_null() { None } else { Some(ctx) };
    }

    /// Current text-state snapshot committed by the latest `done`.
    pub fn current_session(&self) -> &SessionState {
        &self.current
    }

    /// Mark the candidate panel dirty so the event loop flushes it.
    pub fn mark_panel_dirty(&mut self) {
        self.panel_schedule_state = panel_scheduler::mark_dirty(self.panel_schedule_state);
    }

    /// Whether the panel must wait before presenting again.
    ///
    /// Returns `true` while a `wl_surface.frame` callback armed after
    /// the previous present is still outstanding and the fallback
    /// window has not elapsed. Callers skip the present and leave the
    /// schedule dirty so the next tick (after the callback fires, or
    /// after the fallback) re-flushes with the latest coalesced
    /// candidate state. This caps the present rate at the compositor's
    /// refresh rate, so the swapchain never exhausts free images and
    /// the synchronous `vkQueuePresentKHR` never blocks the main loop.
    ///
    /// Returns `false` (clearing any stale pending state) when no
    /// callback is outstanding or the fallback elapsed.
    pub fn panel_present_blocked(&mut self) -> bool {
        if !self.panel_frame_pending {
            return false;
        }
        let elapsed = self
            .panel_frame_requested_at
            .map(|t| t.elapsed())
            .unwrap_or(Duration::ZERO);
        if elapsed >= FRAME_CALLBACK_FALLBACK {
            self.clear_panel_frame_callback();
            return false;
        }
        true
    }

    /// Remaining milliseconds until the panel present fallback expires.
    pub fn panel_present_fallback_remaining_ms(&self, now: Instant) -> Option<i32> {
        if !self.panel_frame_pending {
            return None;
        }
        let requested_at = self.panel_frame_requested_at?;
        let deadline = requested_at + FRAME_CALLBACK_FALLBACK;
        if now >= deadline {
            Some(0)
        } else {
            Some(deadline.saturating_duration_since(now).as_millis() as i32)
        }
    }

    /// Drop the outstanding frame callback and pending flag.
    fn clear_panel_frame_callback(&mut self) {
        self.panel_frame_pending = false;
        self.panel_frame_requested_at = None;
        // `wl_callback` has no destroy request; dropping the proxy is
        // correct disposal (the server frees it after `done`).
        self.panel_frame_callback = None;
    }

    /// Current panel schedule state.
    pub fn panel_schedule_state(&self) -> PanelScheduleState {
        self.panel_schedule_state
    }

    /// Reset the positioned-popup anchor generation. Call on focus_in and
    /// hard boundaries so the next caret rect belongs to a new generation.
    pub fn reset_panel_anchor(&mut self) {
        self.panel_coord.reset_anchor();
    }

    /// Clear the cached caret-rect flag.
    pub fn clear_caret_rect(&mut self) {
        self.panel_coord.clear_caret_rect();
    }

    /// Send an anchor probe (empty preedit + commit) to force the compositor
    /// to emit a fresh `text_input_rectangle` for this popup.
    pub fn probe_anchor(&mut self) {
        if self.panel_coord.should_probe_anchor() {
            self.set_preedit_and_flush("", 0);
            self.panel_coord.record_probe_sent();
        }
    }

    /// Mutable access to the panel coordinator.
    pub fn panel_coord_mut(&mut self) -> &mut PanelCoordinator {
        &mut self.panel_coord
    }

    /// Immutable access to the panel coordinator.
    pub fn panel_coord(&self) -> &PanelCoordinator {
        &self.panel_coord
    }

    /// Set the panel schedule state.
    pub fn set_panel_schedule_state(&mut self, state: PanelScheduleState) {
        self.panel_schedule_state = state;
    }

    /// Clear candidate panel state (focus lost / composition discarded).
    pub fn clear_panel_state(&mut self) {
        self.composition.clear();
        self.panel_schedule_state = panel_scheduler::cancel();
    }

    /// Whether a keyboard grab object currently exists.
    pub fn keyboard_grab_present(&self) -> bool {
        self.keyboard_grab.is_some()
    }

    /// Create a new keyboard grab from the input-method object.
    pub fn create_keyboard_grab(&mut self, qh: &QueueHandle<Self>) {
        if self.keyboard_grab.is_none() {
            tracing::debug!(target: "typio.wayland.grab", "create");
            self.keyboard_grab = Some(self.input_method.grab_keyboard(qh, ()));
            self.keymap_received_this_epoch = false;
        }
    }

    /// Destroy the current keyboard grab object.
    pub fn destroy_keyboard_grab(&mut self) {
        if let Some(grab) = self.keyboard_grab.take() {
            tracing::debug!(target: "typio.wayland.grab", "destroy");
            drop(grab);
            self.keymap_received_this_epoch = false;
        }
    }

    /// Commit pending state to the compositor. Silently dropped before
    /// the first `done` — matching the C serial chokepoint.
    pub fn commit(&self) {
        if !self.initialized {
            return;
        }
        self.input_method.commit(self.serial);
    }

    /// Forward a key to the focused app via the virtual keyboard.
    /// Used when the engine doesn't consume the key.
    pub fn forward_key(&self, time: u32, key: u32, state: u32) {
        self.virtual_keyboard.key(time, key, state);
    }

    /// Forward modifier state to the focused app.
    pub fn forward_modifiers(&self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.virtual_keyboard
            .modifiers(depressed, latched, locked, group);
    }

    /// Raw pointer to the popup wl_surface. Use this to create a
    /// FluxPanel on the SAME surface so Vulkan rendering and popup
    /// positioning share one wl_surface.
    pub fn popup_surface_raw_ptr(&self) -> *mut std::ffi::c_void {
        self.popup_surface_obj.id().as_ptr() as *mut std::ffi::c_void
    }

    /// Set the current candidate list + selected index for the panel.
    /// Forwards to [`CompositionState::set_candidates`].
    pub fn set_candidates(&mut self, candidates: Vec<String>, selected: usize) -> u64 {
        self.composition.set_candidates(candidates, selected)
    }

    /// Take all pending key events for processing by the event loop.
    /// Returns the events in arrival order. The Vec is empty when no
    /// key events are pending.
    pub fn take_pending_keys(&mut self) -> Vec<DecodedKeyEvent> {
        std::mem::take(&mut self.pending_keys)
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
        self.composition.take_pending_commit()
    }

    /// Flush a commit directly to the compositor.
    pub fn commit_string_and_flush(&mut self, text: &str) {
        if !self.initialized || !self.active {
            return;
        }
        self.input_method.commit_string(text.to_string());
        self.input_method.commit(self.serial);
    }

    /// Send preedit text to the compositor (shows inline composition
    /// in the focused text field). Followed by commit(serial) to flush.
    pub fn set_preedit_and_flush(&mut self, text: &str, cursor: u32) {
        if !self.initialized || !self.active {
            return;
        }
        self.input_method
            .set_preedit_string(text.to_string(), cursor as i32, cursor as i32);
        self.input_method.commit(self.serial);
    }

    /// Clear any preedit and commit nothing (used on key release or
    /// engine reset).
    pub fn clear_preedit_and_flush(&mut self) {
        if !self.initialized || !self.active {
            return;
        }
        self.input_method.set_preedit_string(String::new(), 0, 0);
        self.input_method.commit(self.serial);
    }

    /// Load an XKB keymap from a compositor-provided file descriptor.
    fn load_keymap_from_fd(&mut self, fd: std::os::fd::OwnedFd, _size: u32) {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::from(fd);
        if let Err(e) = file.seek(SeekFrom::Start(0)) {
            tracing::warn!(target: "typio.wayland.keymap", "seek to start failed: {e}");
            return;
        }

        let mut buffer = Vec::new();
        if let Err(e) = file.read_to_end(&mut buffer) {
            tracing::warn!(target: "typio.wayland.keymap", "read keymap fd failed: {e}");
            return;
        }

        let mut keymap_string = String::from_utf8_lossy(&buffer).into_owned();
        keymap_string = keymap_string.trim_matches('\0').to_string();

        let keymap = xkbcommon::xkb::Keymap::new_from_string(
            &self.xkb_context,
            keymap_string,
            xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
        );

        match keymap {
            Some(km) => {
                let mut xkb_state = xkbcommon::xkb::State::new(&km);
                xkb_state.update_mask(
                    self.mods_depressed,
                    self.mods_latched,
                    self.mods_locked,
                    0,
                    0,
                    0,
                );
                self.xkb_keymap = Some(km);
                self.xkb_state = Some(xkb_state);
                tracing::debug!(target: "typio.wayland.keymap", "XKB state ready");
            }
            None => tracing::warn!(target: "typio.wayland.keymap", "xkb_keymap_new_from_string failed"),
        }
    }
}

/// Wayland input-method frontend. Owns the connection, event queue,
/// and state. The caller drives the event loop via [`Self::dispatch`]
/// or [`Self::run`].
pub struct InputMethodFrontend {
    // Drop order matters here. Rust drops fields in declaration order, so
    // the panel MUST be declared before the Wayland connection: the panel
    // owns Vulkan resources that reference the wl_surface, and libflux's
    // teardown still makes Wayland protocol calls. If `conn` or `state`
    // is dropped first, libflux hits a closed connection or freed proxy
    // and segfaults. Order:
    //   1. panel     — releases Vulkan surface/canvas/text/arena
    //   2. state     — frees wl_surface / vk / grab proxies
    //   3. queue     — releases event queue
    //   4. conn      — closes the display socket last
    panel: Option<FluxPanel>,
    state: InputMethodState,
    queue: EventQueue<InputMethodState>,
    conn: Connection,
}

impl InputMethodFrontend {
    /// Connect to the Wayland display, bind globals, create the
    /// input-method object, and attempt to create the GPU panel.
    pub fn connect(callback: Option<LifecycleCallback>) -> Result<Self, ConnectError> {
        let mut frontend = Self::connect_internal(callback, true)?;

        let display_ptr = frontend.raw_display_ptr();
        let surface_ptr = frontend.state.popup_surface_raw_ptr();
        let viewport = frontend.state.panel_viewport.clone();
        // Allocate the initial swapchain at `PANEL_PREALLOC_WIDTH ×
        // PANEL_PREALLOC_HEIGHT` (512×128). This covers the first
        // automatic indicator banner at scales 1, 1.5, 2 and 3 without
        // invoking `flux_surface_resize`, which blocks on
        // `vkDeviceWaitIdle` + compositor swapchain release and trips
        // the 3 s watchdog on a fresh daemon (the watchdog is armed
        // before the first PanelUpdate but the resize runs inside that
        // stage). See the audit table on `PANEL_PREALLOC_WIDTH` in
        // panel.rs: at scale 3 the longest observed default label
        // ("中 · Rime · 懿拼音") quantises to 448×128, fitting inside
        // 512×128 with one width-quantum of headroom.
        //
        // Larger labels or scales ≥ 4 still fall through to the
        // grow-only path in `FluxPanel::apply_grow_only_size`; that
        // resize then happens during real user interaction, where the
        // watchdog tolerance has been replaced by genuine cadence.
        // The previous 256×128 pre-allocation only verified the
        // height axis and tripped the watchdog at scale 2 on the
        // width axis (banner needs 320 px quantised vs 256 px
        // allocated).
        match unsafe {
            FluxPanel::new_from_surface(
                display_ptr,
                surface_ptr,
                viewport,
                crate::panel::PANEL_PREALLOC_WIDTH,
                crate::panel::PANEL_PREALLOC_HEIGHT,
            )
        } {
            Ok(panel) => frontend.panel = Some(panel),
            Err(e) => tracing::warn!(target: "typio.panel.host", "FluxPanel creation failed: {e}"),
        }

        Ok(frontend)
    }

    /// Shared connection setup. When `create_panel` is false the GPU panel is
    /// not created; this keeps unit tests that only exercise the protocol
    /// state machine from crashing in environments without a Vulkan surface.
    fn connect_internal(
        callback: Option<LifecycleCallback>,
        _create_panel: bool,
    ) -> Result<Self, ConnectError> {
        let conn = Connection::connect_to_env().map_err(ConnectError::ConnectionFailed)?;
        let (globals, queue) =
            registry_queue_init::<InputMethodState>(&conn).map_err(ConnectError::RegistryFailed)?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=9, ())
            .map_err(|e| ConnectError::BindFailed("wl_seat", format!("{e:?}")))?;

        let im_manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ()).map_err(|e| {
            ConnectError::BindFailed("zwp_input_method_manager_v2", format!("{e:?}"))
        })?;

        let _vk_manager: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&qh, 1..=1, ()).map_err(|e| {
                ConnectError::BindFailed("zwp_virtual_keyboard_manager_v1", format!("{e:?}"))
            })?;

        // Bind wl_compositor (for creating panel surfaces).
        let compositor: WlCompositor = globals
            .bind(&qh, 1..=6, ())
            .map_err(|e| ConnectError::BindFailed("wl_compositor", format!("{e:?}")))?;

        // Bind wp_viewporter if the compositor advertises it. ADR-0013
        // requires this for the grow-only swapchain: without a viewport
        // the buffer must equal the content exactly, which forces a
        // swapchain rebuild (vkDeviceWaitIdle + WSI roundtrips) on every
        // candidate-page width change — the watchdog-killing stall.
        // Compositors without viewporter fall back to exact-size resize.
        let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
        match &viewporter {
            Some(_) => tracing::info!(
                target: "typio.wayland.viewporter",
                "compositor advertises wp_viewporter (grow-only swapchain active)"
            ),
            None => tracing::warn!(
                target: "typio.wayland.viewporter",
                "compositor lacks wp_viewporter — candidate-page width changes rebuild the swapchain (watchdog-killing stall); see ADR-0013"
            ),
        }

        // Create a wl_surface for the panel popup.
        let popup_surface_obj = compositor.create_surface(&qh, ());

        // Attach a wp_viewport to the popup surface (if we have a
        // viewporter). Cloned into FluxPanel later so it owns its own
        // reference; the original stays here for lifetime.
        let panel_viewport: Option<WpViewport> = viewporter
            .as_ref()
            .map(|vp| vp.get_viewport(&popup_surface_obj, &qh, ()));

        let input_method = im_manager.get_input_method(&seat, &qh, ());

        // Keyboard grab is created lazily by the focus controller when an
        // input context is focused, not eagerly here.  An eager grab would
        // capture the keypress used to launch the daemon (e.g. Enter in a
        // terminal) and forward it back to the terminal via the virtual
        // keyboard.
        let keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2> = None;

        // Create the virtual keyboard for forwarding unhandled keys.
        let virtual_keyboard = _vk_manager.create_virtual_keyboard(&seat, &qh, ());

        // Create the popup surface (for the candidate panel).
        let popup_surface = input_method.get_input_popup_surface(&popup_surface_obj, &qh, ());

        let state = InputMethodState {
            seat,
            input_method,
            keyboard_grab,
            virtual_keyboard,
            compositor,
            popup_surface_obj,
            panel_frame_callback: None,
            panel_frame_pending: false,
            panel_frame_requested_at: None,
            popup_surface,
            viewporter,
            panel_viewport,
            text_input_rect: None,
            composition: CompositionState::default(),
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
            pending_keys: Vec::new(),
            facts: InputFacts::default(),
            stopped: false,
            pending: SessionState::default(),
            current: SessionState::default(),
            input_context: None,
            keymap_received_this_epoch: false,
            panel_schedule_state: PanelScheduleState::default(),
            panel_coord: PanelCoordinator::new(),
            buffer_scale: 1.0,
            compositor_repeat_info: None,
        };

        Ok(Self {
            conn,
            queue,
            state,
            panel: None,
        })
    }

    #[cfg(test)]
    fn connect_test() -> Result<Self, ConnectError> {
        Self::connect_internal(None, false)
    }

    /// Immutable access to the state (serial, active flag, etc.).
    pub fn state(&self) -> &InputMethodState {
        &self.state
    }

    /// Mutable access to the state.
    pub fn state_mut(&mut self) -> &mut InputMethodState {
        &mut self.state
    }

    /// Mutable access to the candidate panel, if one was created.
    pub fn panel_mut(&mut self) -> Option<&mut FluxPanel> {
        self.panel.as_mut()
    }

    /// Provide the libtypio input context used to apply surrounding text.
    pub fn set_input_context(&mut self, ctx: *mut typio::TypioInputContext) {
        self.state.set_input_context(ctx);
    }

    /// True if the compositor declared the input method unavailable.
    pub fn stopped(&self) -> bool {
        self.state.stopped()
    }

    /// Whether a keyboard grab object currently exists.
    pub fn keyboard_grab_present(&self) -> bool {
        self.state.keyboard_grab_present()
    }

    /// Create a new keyboard grab object.
    pub fn create_keyboard_grab(&mut self) {
        let qh = self.queue.handle();
        self.state.create_keyboard_grab(&qh);
    }

    /// Destroy the current keyboard grab object.
    pub fn destroy_keyboard_grab(&mut self) {
        self.state.destroy_keyboard_grab();
    }

    /// True if the first keymap for the current grab epoch has arrived.
    pub fn keymap_received_this_epoch(&self) -> bool {
        self.state.keymap_received_this_epoch
    }

    /// The Wayland connection's file descriptor for external event loops.
    pub fn fd(&self) -> i32 {
        self.queue.as_fd().as_raw_fd()
    }

    /// Raw `wl_display*` pointer. Needed for creating raw Wayland
    /// objects (like the panel's wl_surface) that share this
    /// connection.
    ///
    /// # Safety
    /// The caller must not close the display or use it after the
    /// frontend is dropped.
    /// Get the raw wl_display* from this connection. Needed for flux
    /// panel surface creation on the SAME Wayland connection.
    pub fn raw_display_ptr(&self) -> *mut std::ffi::c_void {
        let display_id = self.conn.backend().display_id();
        let proxy_ptr = display_id.as_ptr();

        #[link(name = "wayland-client")]
        extern "C" {
            fn wl_proxy_get_display(proxy: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }

        unsafe { wl_proxy_get_display(proxy_ptr as *mut std::ffi::c_void) }
    }

    /// Non-blocking dispatch of pending Wayland events.
    pub fn dispatch(&mut self) -> io::Result<()> {
        // Split borrow: `self.queue` and `self.state` are disjoint fields.
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| io::Error::other(format!("dispatch: {e}")))?;
        Ok(())
    }

    /// Flush pending Wayland requests to the compositor.
    pub fn flush(&self) -> io::Result<()> {
        self.conn
            .flush()
            .map_err(|e| io::Error::other(format!("flush: {e}")))
    }

    /// Arm a `wl_surface.frame` callback on the popup surface after a
    /// successful present. The panel flush path consults
    /// [`InputMethodState::panel_present_blocked`] before the next
    /// present and skips while the callback is outstanding, so the
    /// daemon never submits frames faster than the compositor releases
    /// swapchain images. Idempotent: re-arming replaces any prior
    /// outstanding callback.
    pub fn arm_panel_frame_callback(&mut self) {
        let qh = self.queue.handle();
        let cb = self.state.popup_surface_obj.frame(&qh, ());
        // The throttle prevents re-arming while a callback is outstanding,
        // so the slot is normally empty; overwriting drops any stragger
        // (wl_callback has no destroy request, so dropping the proxy is
        // the correct disposal — the server frees it after `done`).
        self.state.panel_frame_callback = Some(cb);
        self.state.panel_frame_pending = true;
        self.state.panel_frame_requested_at = Some(Instant::now());
    }

    /// Prepare a read from the Wayland socket, dispatching any already-queued
    /// events first. Mirrors `wl_display_prepare_read` + `dispatch_pending` in
    /// the C event loop.
    pub fn prepare_read_loop(&mut self) -> io::Result<ReadEventsGuard> {
        loop {
            match self.queue.prepare_read() {
                Some(guard) => return Ok(guard),
                None => {
                    self.queue
                        .dispatch_pending(&mut self.state)
                        .map_err(|e| io::Error::other(format!("dispatch: {e}")))?;
                }
            }
        }
    }

    /// Read events from the Wayland socket and dispatch any pending events
    /// that arrive. Consumes the read guard returned by `prepare_read_loop`.
    pub fn read_and_dispatch(&mut self, guard: ReadEventsGuard) -> io::Result<()> {
        guard
            .read()
            .map_err(|e| io::Error::other(format!("read: {e}")))?;
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
                let mut pollfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
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

impl InputMethodState {
    /// Apply the pending `done` batch to the current session state and, if a
    /// libtypio input context is wired, forward surrounding text to it.
    fn apply_pending_to_current(&mut self) {
        self.current = self.pending.clone();
        if let Some(ctx) = self.input_context {
            if let Some(ref text) = self.current.surrounding_text {
                if let Ok(c_text) = std::ffi::CString::new(text.as_str()) {
                    typio::input_context::typio_input_context_set_surrounding(
                        ctx,
                        c_text.as_ptr(),
                        self.current.cursor as i32,
                        self.current.anchor as i32,
                    );
                }
            }
        }
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
                tracing::debug!(target: "typio.wayland.frontend", "Activate");
                state.active = true;
                state.facts.im_activate_seen = true;
                state.pending = SessionState {
                    active: true,
                    ..SessionState::default()
                };
                state.fire(LifecycleEvent::Activated);
            }
            Event::Deactivate => {
                tracing::debug!(target: "typio.wayland.frontend", "Deactivate");
                state.active = false;
                state.facts.im_deactivate_seen = true;
                state.pending.active = false;
                state.fire(LifecycleEvent::Deactivated);
            }
            Event::SurroundingText {
                text,
                cursor,
                anchor,
            } => {
                state.pending.surrounding_text = Some(text);
                state.pending.cursor = cursor;
                state.pending.anchor = anchor;
                state.fire(LifecycleEvent::SurroundingText {
                    text: state.pending.surrounding_text.clone().unwrap_or_default(),
                    cursor,
                    anchor,
                });
            }
            Event::TextChangeCause { cause } => {
                state.pending.text_change_cause = u32::from(cause);
            }
            Event::ContentType { hint, purpose } => {
                let hint_raw: u32 = match &hint {
                    wayland_client::WEnum::Value(v) => (*v).into(),
                    wayland_client::WEnum::Unknown(u) => *u,
                };
                let purpose_raw: u32 = match &purpose {
                    wayland_client::WEnum::Value(v) => (*v).into(),
                    wayland_client::WEnum::Unknown(u) => *u,
                };
                state.pending.content_hint = hint_raw;
                state.pending.content_purpose = purpose_raw;
                state.fire(LifecycleEvent::ContentType {
                    hint: hint_raw,
                    purpose: purpose_raw,
                });
            }
            Event::Done => {
                state.serial = state.serial.wrapping_add(1);
                state.initialized = true;
                state.facts.im_done_had_activate = state.facts.im_activate_seen;
                state.facts.im_done_had_deactivate = state.facts.im_deactivate_seen;
                state.facts.im_done_serial = state.serial;
                state.apply_pending_to_current();
                // Clear per-event facts; the batch facts survive until the
                // focus controller consumes them at the end of the tick.
                state.facts.im_activate_seen = false;
                state.facts.im_deactivate_seen = false;
                state.fire(LifecycleEvent::Done {
                    serial: state.serial,
                });
            }
            Event::Unavailable => {
                state.stopped = true;
                state.fire(LifecycleEvent::Unavailable);
            }
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlCompositor, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &WlCompositor,
        _event: <WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewporter,
        _event: <WpViewporter as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for InputMethodState {
    fn event(
        _state: &mut Self,
        _proxy: &WpViewport,
        _event: <WpViewport as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        proxy: &wl_surface::WlSurface,
        event: <wl_surface::WlSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_surface::Event;
        if let Event::PreferredBufferScale { factor } = event {
            state.buffer_scale = factor as f32;
            proxy.set_buffer_scale(factor);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _proxy: &wl_callback::WlCallback,
        _event: <wl_callback::WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // The compositor consumed the previous frame: it is safe to
        // present again. The panel stays in whatever schedule state the
        // engine updates left it (Dirty updates during the wait are
        // coalesced into the next flush); we only clear the throttle.
        state.clear_panel_frame_callback();
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event;
        state.text_input_rect = Some((x, y, width, height));
        state.panel_coord.note_caret_rect();
        state.panel_coord.mark_anchor_ready();
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
                tracing::debug!(
                    target: "typio.wayland.keymap",
                    "Keymap event received, format={format:?} size={size}"
                );
                state.keymap_received_this_epoch = true;
                let fmt_raw: u32 = match &format {
                    wayland_client::WEnum::Value(v) => *v as u32,
                    wayland_client::WEnum::Unknown(u) => *u,
                };
                if fmt_raw != 1 {
                    return;
                }
                // Forward the keymap to the virtual keyboard before any
                // `key`/`modifiers` requests. The compositor rejects those
                // with protocol error 0 (no_keymap) if the vk has no keymap.
                // `load_keymap_from_fd` consumes the fd, so dup it first.
                // The wayland backend dups the fd again when serializing the
                // request, so it is safe to drop `vk_fd` right after the call.
                match fd.try_clone() {
                    Ok(vk_fd) => state.virtual_keyboard.keymap(fmt_raw, vk_fd.as_fd(), size),
                    Err(e) => tracing::warn!(
                        target: "typio.wayland.keymap",
                        "dup keymap fd for vk failed: {e}"
                    ),
                }
                state.load_keymap_from_fd(fd, size);
            }
            Event::Key {
                time,
                key,
                state: key_state,
                serial: _,
            } => {
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

                let keysym: u32 = state
                    .xkb_state
                    .as_ref()
                    .map_or(0, |s| s.key_get_one_sym(kc).into());
                let unicode = state
                    .xkb_state
                    .as_ref()
                    .map_or(String::new(), |s| s.key_get_utf8(kc));

                state.fire(LifecycleEvent::Key(DecodedKeyEvent {
                    keycode: key,
                    xkb_keycode,
                    keysym,
                    unicode: unicode.clone(),
                    state: raw_state,
                    time,
                }));

                // Queue for the event-loop driver. Press events go to the
                // engine; release events are forwarded to the focused app
                // and stop the repeat timer. Without queueing releases,
                // the timer fires forever after a single press.
                //
                // Multiple events may arrive in the same Wayland dispatch
                // batch (e.g. `release(BS)` immediately followed by
                // `press(other)`); all are queued in arrival order so the
                // loop sees every release and can disarm the repeat timer.
                // A single-slot overwrite here was the root cause of the
                // occasional "stuck backspace" — the release was lost and
                // the timer kept firing the consumed press forever.
                //
                // When the text field is deactivated (state.active == false)
                // the grab may still be retained as a soft pause. Forward
                // the key directly to the virtual keyboard so shortcuts and
                // regular keys still reach the focused application instead
                // of being silently swallowed by the retained grab.
                if state.active {
                    state.pending_keys.push(DecodedKeyEvent {
                        keycode: key,
                        xkb_keycode,
                        keysym,
                        unicode,
                        state: raw_state,
                        time,
                    });
                } else {
                    state.forward_key(time, key, raw_state);
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
                    xs.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                // Mirror the grab's modifier state to the virtual keyboard
                // so the focused app sees Ctrl/Alt/Shift held when a
                // forwarded key arrives. Without this, Ctrl-C arrives as a
                // bare 'c'. The grab always delivers Keymap before the
                // first Modifiers, so vk keymap is already set by this
                // point (vk.modifiers requires a keymap or the compositor
                // rejects it with protocol error 0).
                state
                    .virtual_keyboard
                    .modifiers(mods_depressed, mods_latched, mods_locked, group);
            }
            Event::RepeatInfo { rate, delay } => {
                state.compositor_repeat_info = Some((rate, delay));
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
        let e = ConnectError::BindFailed("zwp_input_method_manager_v2", "NotPresent".to_string());
        let s = format!("{e}");
        assert!(s.contains("zwp_input_method_manager_v2"));
    }

    #[test]
    fn lifecycle_event_is_debug() {
        assert!(format!("{:?}", LifecycleEvent::Activated).contains("Activated"));
        assert!(format!("{:?}", LifecycleEvent::Done { serial: 42 }).contains("42"));
    }

    #[test]
    fn state_helpers_round_trip() {
        let Ok(mut frontend) = InputMethodFrontend::connect_test() else {
            eprintln!("skipping input_method state-helper test: no Wayland display");
            return;
        };

        let state = frontend.state_mut();
        assert_eq!(state.serial(), 0);
        assert!(!state.is_active());
        assert!(!state.stopped());

        state.set_candidates(vec!["alpha".to_string(), "beta".to_string()], 1);
        assert_eq!(state.composition.candidates, vec!["alpha", "beta"]);
        assert_eq!(state.composition.selected_candidate, 1);

        state.mark_panel_dirty();
        assert_eq!(state.panel_schedule_state, PanelScheduleState::Dirty);

        state.clear_panel_state();
        assert!(state.composition.candidates.is_empty());
        assert_eq!(state.composition.selected_candidate, 0);
        assert_eq!(state.panel_schedule_state, PanelScheduleState::Idle);

        state.facts.im_done_serial = 7;
        let facts = state.take_facts();
        assert_eq!(facts.im_done_serial, 7);
        assert_eq!(state.facts.im_done_serial, 0);

        let press = DecodedKeyEvent {
            keycode: 30,
            xkb_keycode: 38,
            keysym: 0x0061,
            unicode: "a".to_string(),
            state: 1,
            time: 123,
        };
        let release = DecodedKeyEvent {
            keycode: 30,
            xkb_keycode: 38,
            keysym: 0x0061,
            unicode: String::new(),
            state: 0,
            time: 130,
        };

        // Single-event drain.
        state.pending_keys.push(press.clone());
        assert_eq!(state.take_pending_keys(), vec![press.clone()]);
        assert!(state.take_pending_keys().is_empty());

        // Multi-event drain preserves arrival order. Regression test for
        // the "stuck backspace" bug: a single-slot Option overwrote the
        // release when a second event arrived in the same Wayland
        // dispatch batch, leaving the repeat timer armed forever.
        state.pending_keys.push(press.clone());
        state.pending_keys.push(release.clone());
        assert_eq!(
            state.take_pending_keys(),
            vec![press.clone(), release.clone()]
        );
        assert!(state.take_pending_keys().is_empty());

        state.composition.set_pending_commit("hello".to_string());
        assert_eq!(state.take_pending_commit(), Some("hello".to_string()));
        assert!(state.take_pending_commit().is_none());
    }
}
