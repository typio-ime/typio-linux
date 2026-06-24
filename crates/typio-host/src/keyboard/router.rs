//! Keyboard event router.
//!
//! Bridges the Wayland input-method keyboard grab to libtypio's input
//! context. Decides whether a key is consumed by the engine or forwarded
//! to the focused application via the virtual keyboard.

use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

use typio_abi::{TypioComposition, TypioEventType, TypioKeyEvent};

use crate::candidate_guard::{classify_host_selection, HostSelectionAction, HostSelectionFlags};
use crate::input_method::{DecodedKeyEvent, InputMethodState};
use crate::keyboard_policy::{
    modifier_bit_for_keysym, sync_physical_modifiers, tracking_mark_released_pending,
    tracking_reset, tracking_reset_generations, KeyTrackState, WL_KEYBOARD_KEY_STATE_PRESSED,
    WL_KEYBOARD_KEY_STATE_RELEASED,
};
use crate::repeat_timer::Modifiers;
use crate::text_ui_state::{text_ui_plan_update, PreeditTracking, TextUiPlan};

/// Maximum number of keys tracked for symmetric press/release. Mirrors
/// `TYPIO_WL_MAX_TRACKED_KEYS` in the C host.
pub const MAX_TRACKED_KEYS: usize = 256;

/// Pending text committed by the engine since the last key dispatch.
static PENDING_COMMIT: Mutex<Option<String>> = Mutex::new(None);

/// Pending composition state since the last key dispatch. Staged by the
/// engine's composition callback on the same thread that called
/// `typio_input_context_process_key`; drained by `drain_composition`.
///
/// `cursor_pos` is the engine's requested byte offset into `preedit_text`
/// (negative means "place at the end"); see [`crate::preedit::resolve_cursor`].
/// `host_managed_selection` carries the engine's declared
/// selection-intercept flags (ADR-0012) so the host can apply
/// [`crate::candidate_guard`] without a separate engine→host round-trip.
#[derive(Default)]
struct PendingComposition {
    preedit_text: String,
    cursor_pos: i32,
    candidates: Vec<String>,
    selected: usize,
    host_managed_selection: HostSelectionFlags,
}

static PENDING_COMPOSITION: Mutex<Option<PendingComposition>> = Mutex::new(None);

extern "C" fn on_commit_abi(
    ctx: *mut typio_abi::TypioInputContext,
    text: *const c_char,
    user_data: *mut c_void,
) {
    on_commit(ctx as *mut typio::TypioInputContext, text, user_data)
}

extern "C" fn on_commit(
    _ctx: *mut typio::TypioInputContext,
    text: *const c_char,
    _user_data: *mut c_void,
) {
    if text.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    if let Ok(mut slot) = PENDING_COMMIT.lock() {
        *slot = Some(s);
    }
}

extern "C" fn on_composition_abi(
    ctx: *mut typio_abi::TypioInputContext,
    comp: *const TypioComposition,
    user_data: *mut c_void,
) {
    on_composition(ctx as *mut typio::TypioInputContext, comp, user_data)
}

extern "C" fn on_composition(
    _ctx: *mut typio::TypioInputContext,
    comp: *const TypioComposition,
    _user_data: *mut c_void,
) {
    if comp.is_null() {
        return;
    }
    let comp = unsafe { &*comp };

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
    let cursor_pos = comp.cursor_pos;
    let host_managed_selection = HostSelectionFlags::from_bits_truncate(comp.host_managed_selection);

    if let Ok(mut slot) = PENDING_COMPOSITION.lock() {
        *slot = Some(PendingComposition {
            preedit_text: preedit,
            cursor_pos,
            candidates,
            selected,
            host_managed_selection,
        });
    }
}

/// What the repeat timer should do when it fires for a held key.
///
/// The initial key press is either consumed by the engine (in which case
/// repeats must re-enter the engine with `is_repeat: true`) or forwarded
/// to the focused application via the virtual keyboard (in which case
/// repeats are synthetic key presses sent the same way). The router
/// remembers which path is active so the main loop can drive both kinds
/// of repeat through a single timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepeatMode {
    /// Re-dispatch to the engine with `is_repeat: true`.
    Engine,
    /// Forward a synthetic press to the virtual keyboard.
    Forward,
}

/// Result of [`KeyboardRouter::dispatch_repeat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatOutcome {
    /// The repeat key was forwarded to the virtual keyboard.
    Forwarded,
    /// The engine consumed the repeat event. The caller should drain
    /// any resulting commit/composition output.
    Consumed,
    /// The repeat chain has ended — either no key is pending or the
    /// engine declined the repeat. The caller should stop the timer.
    Stopped,
}

/// A keyboard router tied to a libtypio input context.
pub struct KeyboardRouter {
    ctx: *mut typio::TypioInputContext,
    /// Key currently held down and subject to auto-repeat, if any.
    /// Set on the initial press (whether the key was consumed by the
    /// engine or forwarded to the application) and cleared on release.
    repeat_key: Option<DecodedKeyEvent>,
    /// What the repeat timer should do for [`Self::repeat_key`].
    repeat_mode: RepeatMode,
    /// Physical modifier state tracked from key events.
    pub(crate) physical_modifiers: Modifiers,
    /// Current grab epoch. Keys from an older epoch are ignored.
    active_generation: u32,
    /// Per-key tracking state for symmetric press/release.
    key_tracking_states: Vec<KeyTrackState>,
    /// Per-key epoch stamp.
    key_tracking_generations: Vec<u32>,
    /// True if any non-modifier key is currently held down. Drives
    /// `chord_should_switch_engine`: the switch chord is suppressed only
    /// while a non-modifier key is actually held, so typing a key and
    /// releasing it no longer blocks the next Ctrl+Shift.
    shortcut_saw_non_modifier: bool,
    /// Per-keycode "currently held" flag for non-modifier keys (indexed
    /// by evdev scancode). Backs [`Self::held_non_modifier_count`] so
    /// compositor-driven auto-repeats of an already-held key don't
    /// double-count.
    non_modifier_held: [bool; 256],
    /// Number of entries in [`Self::non_modifier_held`] currently true.
    /// When this drops to zero, [`Self::shortcut_saw_non_modifier`] is
    /// cleared.
    held_non_modifier_count: u32,
    /// True if the switch chord already fired in this gesture; prevents
    /// repeat triggers while the modifiers stay held.
    shortcut_already_triggered: bool,
    /// Set when a chord completes during `dispatch_key`. The main loop
    /// drains this and cycles the active keyboard engine.
    shortcut_fired: bool,
    /// Modifiers whose most recent press was forwarded to the engine
    /// rather than swallowed by chord suppression. Used to forward
    /// releases symmetrically so the engine never observes an unpaired
    /// modifier release (which could spuriously toggle state, e.g. a
    /// Rime schema switch on a Shift release whose press was consumed
    /// by the Ctrl+Shift switch chord).
    engine_tracked_mods: Modifiers,
    /// Last preedit the host actually sent to the compositor. Used by
    /// [`Self::drain_composition`] to suppress redundant
    /// `set_preedit_string` + `commit` Wayland round-trips when the
    /// engine reports the same preedit text and cursor as the previous
    /// composition (the canonical case: Up/Down arrow navigation moves
    /// only the candidate highlight, leaving the inline preedit text
    /// untouched). See [`crate::text_ui_state::text_ui_plan_update`].
    preedit_tracking: PreeditTracking,
}

/// Default engine-switch chord: Ctrl+Shift, the standard Linux IME
/// switch. Both modifiers must be pressed (any side, any order); the
/// chord fires when the second one goes down.
pub fn default_switch_binding() -> crate::keyboard_policy::ShortcutBinding {
    use crate::keyboard_policy::ShortcutBinding;
    ShortcutBinding {
        modifiers: Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
        keysym: 0, // unused — chord_is_switch_modifier covers both sides
    }
}

impl KeyboardRouter {
    /// Create a new router for the given TypioInstance.
    ///
    /// # Safety
    /// `instance` must be a valid, initialized `TypioInstance` pointer.
    pub unsafe fn new(instance: *mut typio::TypioInstance) -> Option<Self> {
        let ctx = typio::input_context::typio_input_context_new(instance);
        if ctx.is_null() {
            return None;
        }
        typio::input_context::typio_input_context_set_commit_callback(
            ctx,
            Some(on_commit_abi),
            std::ptr::null_mut(),
        );
        typio::input_context::typio_input_context_set_composition_callback(
            ctx,
            Some(on_composition_abi),
            std::ptr::null_mut(),
        );
        Some(Self {
            ctx,
            repeat_key: None,
            repeat_mode: RepeatMode::Forward,
            physical_modifiers: Modifiers::NONE,
            active_generation: 1,
            key_tracking_states: vec![KeyTrackState::default(); MAX_TRACKED_KEYS],
            key_tracking_generations: vec![0u32; MAX_TRACKED_KEYS],
            shortcut_saw_non_modifier: false,
            non_modifier_held: [false; 256],
            held_non_modifier_count: 0,
            shortcut_already_triggered: false,
            shortcut_fired: false,
            engine_tracked_mods: Modifiers::NONE,
            preedit_tracking: PreeditTracking::new(),
        })
    }

    /// Notify the engine that the input context has gained focus.
    pub fn focus_in(&mut self) {
        typio::input_context::typio_input_context_focus_in(self.ctx);
    }

    /// Notify the engine that the input context has lost focus.
    pub fn focus_out(&mut self) {
        typio::input_context::typio_input_context_focus_out(self.ctx);
    }

    /// Forget the last preedit we claimed to have sent to the compositor.
    /// Called by the focus controller whenever it clears the visible
    /// preedit through a path other than `drain_composition`, so the
    /// next composition is not suppressed against stale tracking.
    pub fn preedit_tracking_reset(&mut self) {
        self.preedit_tracking.reset();
    }

    /// True iff the libtypio input context currently reports itself focused.
    pub fn is_focused(&self) -> bool {
        typio::input_context::typio_input_context_is_focused(self.ctx)
    }

    /// Reset the engine's in-flight composition and candidate state.
    pub fn reset(&mut self) {
        typio::input_context::typio_input_context_reset(self.ctx);
        if let Ok(mut slot) = PENDING_COMMIT.lock() {
            slot.take();
        }
        if let Ok(mut slot) = PENDING_COMPOSITION.lock() {
            slot.take();
        }
        // Forget any preedit we claimed to have sent — the engine reset
        // may be followed by the compositor clearing the field on its
        // own, and the next composition must not be suppressed as a
        // "no-op" against stale tracking.
        self.preedit_tracking.reset();
    }

    /// Drain any pending composition update and update preedit/candidates.
    pub fn drain_composition(&mut self, frontend: &mut InputMethodState) {
        if let Ok(mut slot) = PENDING_COMPOSITION.lock() {
            if let Some(pending) = slot.take() {
                let PendingComposition {
                    preedit_text: preedit,
                    cursor_pos,
                    candidates,
                    selected,
                    host_managed_selection,
                } = pending;
                let preedit_len = preedit.len();
                let candidate_count = candidates.len();
                // Mirror the engine's declared selection-intercept flags
                // into the state so the next `dispatch_key` can apply
                // candidate_guard without consulting the engine again.
                frontend.composition.host_managed_selection = host_managed_selection;
                // Preedit is the source of truth for what shows inline in
                // the focused text field. Candidates drive the popup. Either
                // can change independently of the other: an empty preedit
                // with non-empty candidates means the engine is offering
                // completions; a non-empty preedit with no candidates means
                // the engine is mid-composition (e.g. pinyin after one
                // keystroke) and will show candidates later.
                if preedit.is_empty() && candidates.is_empty() {
                    // Both cleared. Only re-clear if we actually had a
                    // preedit outstanding; otherwise this would emit a
                    // `set_preedit_string("") + commit` Wayland round-trip
                    // on every composition tick where the engine reports
                    // "nothing to show" (e.g. after every commit).
                    if self.preedit_tracking.last_text.is_some()
                        || self.preedit_tracking.last_cursor != -1
                    {
                        frontend.clear_preedit_and_flush();
                        self.preedit_tracking.reset();
                    }
                } else {
                    // Resolve the engine's cursor_pos (non-negative wins,
                    // negative falls back to end) so left/right navigation
                    // inside the preedit actually moves the visible caret
                    // instead of always parking at the right edge.
                    let cursor =
                        crate::preedit::resolve_cursor(cursor_pos, preedit_len) as u32;
                    // Compare against what we last actually sent to the
                    // compositor. Up/Down candidate navigation is the
                    // canonical case where the engine emits a composition
                    // with identical preedit text + cursor and a different
                    // `selected` — re-sending the preedit there is pure
                    // waste (a `set_preedit_string` + `commit` Wayland
                    // round-trip per arrow press).
                    let plan = text_ui_plan_update(
                        self.preedit_tracking.last_text.as_deref(),
                        self.preedit_tracking.last_cursor,
                        Some(preedit.as_str()),
                        cursor_pos,
                    );
                    if plan == TextUiPlan::SyncPreeditAndPanel {
                        frontend.set_preedit_and_flush(&preedit, cursor);
                        self.preedit_tracking.last_text = Some(preedit.clone());
                        self.preedit_tracking.last_cursor = cursor_pos;
                    }
                    // SyncPanelOnly: skip the Wayland round-trip; the
                    // candidate-panel repaint below is driven independently
                    // by `mark_panel_dirty`.
                }
                let composition_seq = frontend.set_candidates(candidates, selected);
                frontend.mark_panel_dirty();
                tracing::debug!(
                    target: "typio.engine.composition",
                    composition_seq,
                    preedit_len,
                    cursor_pos,
                    candidate_count,
                    selected,
                    "composition update"
                );
            }
        }
    }

    /// Enter a soft pause: retain the grab object, but release tracked keys
    /// and stop repeat state.
    pub fn soft_pause(&mut self) {
        tracking_mark_released_pending(&mut self.key_tracking_states);
        self.repeat_key = None;
        self.repeat_mode = RepeatMode::Forward;
        self.physical_modifiers = Modifiers::NONE;
        self.engine_tracked_mods = Modifiers::NONE;
        self.non_modifier_held = [false; 256];
        self.held_non_modifier_count = 0;
        self.shortcut_saw_non_modifier = false;
    }

    /// Scrub the current key generation and reset all per-key tracking.
    pub fn scrub_generation(&mut self) {
        self.active_generation = self.active_generation.wrapping_add(1);
        if self.active_generation == 0 {
            self.active_generation = 1;
        }
        tracking_reset(&mut self.key_tracking_states);
        tracking_reset_generations(&mut self.key_tracking_generations);
        self.repeat_key = None;
        self.repeat_mode = RepeatMode::Forward;
        // Reset the shortcut chord so a grab handoff doesn't carry
        // stale gesture state into the next focus.
        self.non_modifier_held = [false; 256];
        self.held_non_modifier_count = 0;
        self.shortcut_saw_non_modifier = false;
        self.shortcut_already_triggered = false;
        self.shortcut_fired = false;
        self.engine_tracked_mods = Modifiers::NONE;
    }

    /// True iff the configured engine-switch chord (Ctrl+Shift by
    /// default) completed during the most recent `dispatch_key`. The
    /// main loop drains this once per tick and cycles the active
    /// keyboard. Reading clears the flag.
    pub fn take_switch_chord_fired(&mut self) -> bool {
        std::mem::take(&mut self.shortcut_fired)
    }

    /// Current grab epoch.
    pub fn active_generation(&self) -> u32 {
        self.active_generation
    }

    /// Raw input context pointer. The caller must not free it.
    pub fn ctx(&self) -> *mut typio::TypioInputContext {
        self.ctx
    }

    /// Try to handle `key` through host-managed candidate selection
    /// (ADR-0012), bypassing the engine's `process_key`.
    ///
    /// Returns `Some(handled)` when the host took the key (either by
    /// moving the highlight locally — navigation — or by dispatching
    /// `typio_input_context_commit_candidate` — commit). Returns
    /// `None` when the host did not take the key and the caller should
    /// fall back to the engine via [`Self::dispatch_key`].
    ///
    /// **Opt-in.** The engine must have declared a non-empty
    /// `host_managed_selection` flag set in its last composition; see
    /// [`crate::candidate_guard::should_consume_key`]. Engines that
    /// have not opted in are not affected.
    ///
    /// For **navigation** keys (Up/Down/Left/Right) the highlight is
    /// moved locally and the panel is marked dirty. The engine never
    /// sees the key — this is the canonical perf win of host-managed
    /// selection: no synchronous FFI round-trip per arrow press.
    ///
    /// For **commit** keys (Space / digits / Enter-raw) the host calls
    /// `typio_input_context_commit_candidate` so the engine can
    /// dispatch its own `commit_candidate` vtable entry. If the engine
    /// declines (returns `TypioErrorNotFound` — typically because it
    /// doesn't implement the vtable entry), the host returns `None` so
    /// the caller can fall back to `process_key`, preserving the
    /// user's intent instead of dropping the key.
    ///
    /// Release events under host-managed selection are swallowed
    /// (`Some(true)`) so the engine never observes an unpaired release
    /// for a press it didn't see.
    pub fn try_host_selection(
        &mut self,
        key: &DecodedKeyEvent,
        frontend: &mut InputMethodState,
    ) -> Option<bool> {
        let action = classify_host_selection(
            key.state == WL_KEYBOARD_KEY_STATE_PRESSED,
            key.keysym,
            frontend.composition.candidates.len(),
            frontend.composition.selected_candidate,
            frontend.composition.host_managed_selection,
        )?;
        match action {
            HostSelectionAction::Swallow => Some(true),
            HostSelectionAction::Navigate(new_idx) => {
                if new_idx != frontend.composition.selected_candidate {
                    frontend.composition.selected_candidate = new_idx;
                    frontend.composition.composition_seq =
                        frontend.composition.composition_seq.wrapping_add(1);
                    frontend.mark_panel_dirty();
                    tracing::trace!(
                        target: "typio.engine.host_sel",
                        selected = new_idx,
                        "host-managed navigation"
                    );
                }
                Some(true)
            }
            HostSelectionAction::Commit(idx) => {
                let r = typio::input_context::typio_input_context_commit_candidate(
                    self.ctx,
                    idx as i32,
                );
                if r == typio_abi::TypioResult::TypioOk {
                    return Some(true);
                }
                tracing::debug!(
                    target: "typio.engine.host_sel",
                    idx,
                    result = ?r,
                    "commit_candidate declined; falling back to process_key"
                );
                // Engine declined (TypioErrorNotFound). Fall back to
                // process_key so the user's intent isn't lost.
                None
            }
        }
    }

    /// Dispatch one decoded key event to the engine.
    ///
    /// Returns `true` if the engine consumed the key. The event reaches
    /// the engine with `is_repeat: false`; repeats are driven by the
    /// main loop's repeat timer via [`Self::dispatch_repeat`].
    pub fn dispatch_key(&mut self, key: &DecodedKeyEvent, xkb_mods_depressed: u32) -> bool {
        let state = if key.state == 1 {
            WL_KEYBOARD_KEY_STATE_PRESSED
        } else {
            WL_KEYBOARD_KEY_STATE_RELEASED
        };

        // Update physical modifier tracking.
        let bit = modifier_bit_for_keysym(key.keysym);
        let is_modifier_key = bit != Modifiers::NONE;
        // Whether to suppress this modifier's release from the engine.
        // A release is only forwarded when the matching press was also
        // forwarded; chord-suppressed presses get chord-suppressed
        // releases so the engine never sees an unpaired event that
        // could spuriously toggle state (e.g. Rime schema switch).
        let mut suppress_engine_release = false;
        if is_modifier_key {
            if state == WL_KEYBOARD_KEY_STATE_PRESSED {
                self.physical_modifiers = Modifiers(self.physical_modifiers.0 | bit.0);
            } else {
                if (self.engine_tracked_mods.0 & bit.0) == 0 {
                    suppress_engine_release = true;
                }
                self.physical_modifiers = Modifiers(self.physical_modifiers.0 & !bit.0);
                self.engine_tracked_mods = Modifiers(self.engine_tracked_mods.0 & !bit.0);
                // Releasing any chord modifier ends the current gesture.
                self.shortcut_saw_non_modifier = false;
                self.shortcut_already_triggered = false;
            }
        }

        // Detect the Ctrl+Shift engine-switch chord. The chord fires
        // exactly once per gesture on the keypress that completes the
        // binding's modifier set, as long as no non-modifier key was
        // pressed in between (matches `chord_should_switch_engine`).
        if state == WL_KEYBOARD_KEY_STATE_PRESSED
            && is_modifier_key
            && !self.shortcut_already_triggered
            && crate::keyboard_policy::chord_should_switch_engine(
                &default_switch_binding(),
                key.keysym,
                self.physical_modifiers,
                self.shortcut_saw_non_modifier,
                self.shortcut_already_triggered,
            )
        {
            self.shortcut_already_triggered = true;
            self.shortcut_fired = true;
            // The chord supersedes the engine: don't forward the
            // modifier presses to it either, or rime/compose may react
            // (e.g. compose-key chords). This modifier's press never
            // reached the engine, so drop it from the tracked set;
            // its release will be suppressed to match.
            self.engine_tracked_mods = Modifiers(self.engine_tracked_mods.0 & !bit.0);
            return true;
        }

        if !is_modifier_key {
            self.track_non_modifier_held(key.keycode, state == WL_KEYBOARD_KEY_STATE_PRESSED);
        }

        if key.state != 1 {
            // Release: forward to the engine so engines that need
            // release events (e.g. Rime schema switching on a lone
            // Shift release) can complete gesture detection — unless
            // the matching press was chord-suppressed.
            if suppress_engine_release {
                return false;
            }
            let consumed = self.process_key_engine(key, xkb_mods_depressed, false);
            return consumed;
        }

        // Press: record the modifier as engine-tracked before
        // forwarding so its later release can be paired.
        if is_modifier_key {
            self.engine_tracked_mods = Modifiers(self.engine_tracked_mods.0 | bit.0);
        }
        let consumed = self.process_key_engine(key, xkb_mods_depressed, false);
        consumed
    }

    /// Update the "non-modifier key held" tracker that backs
    /// [`Self::shortcut_saw_non_modifier`]. Called from
    /// [`Self::dispatch_key`] for every non-modifier event.
    ///
    /// The per-keycode dedup means compositor-driven auto-repeats of an
    /// already-held key don't inflate the count. When the count drops to
    /// zero the chord gate clears, so a typed-and-released key no longer
    /// suppresses the next Ctrl+Shift (the old sticky-bool behaviour
    /// swallowed the first chord after any regular typing).
    fn track_non_modifier_held(&mut self, keycode: u32, pressed: bool) {
        let idx = keycode as usize;
        if idx < self.non_modifier_held.len() {
            let slot = &mut self.non_modifier_held[idx];
            if pressed {
                if !*slot {
                    *slot = true;
                    self.held_non_modifier_count += 1;
                }
            } else if *slot {
                *slot = false;
                self.held_non_modifier_count = self.held_non_modifier_count.saturating_sub(1);
            }
        } else {
            // Rare high keycode beyond the table; fall back to a
            // conservative counter-only path so the chord gate still
            // tracks roughly correctly.
            if pressed {
                self.held_non_modifier_count += 1;
            } else {
                self.held_non_modifier_count = self.held_non_modifier_count.saturating_sub(1);
            }
        }
        self.shortcut_saw_non_modifier = self.held_non_modifier_count > 0;
    }

    /// Dispatch a key-repeat event to the engine with `is_repeat: true`.
    ///
    /// This skips the modifier tracking, chord detection, and release
    /// early-return that [`Self::dispatch_key`] performs — none of those
    /// apply to a synthetic repeat (the physical state hasn't changed
    /// since the initial press). Returns `true` if the engine consumed
    /// the repeat.
    fn dispatch_key_repeat(&mut self, key: &DecodedKeyEvent, xkb_mods_depressed: u32) -> bool {
        self.process_key_engine(key, xkb_mods_depressed, true)
    }

    /// Shared engine-dispatch core used by both the initial-press and
    /// repeat paths. Builds the `TypioKeyEvent` with the supplied
    /// `is_repeat` flag and forwards it to libtypio.
    fn process_key_engine(
        &self,
        key: &DecodedKeyEvent,
        xkb_mods_depressed: u32,
        is_repeat: bool,
    ) -> bool {
        // Synchronize physical modifiers with xkb-derived state.
        let effective =
            sync_physical_modifiers(self.physical_modifiers, Modifiers(xkb_mods_depressed));

        let event = TypioKeyEvent {
            struct_size: std::mem::size_of::<TypioKeyEvent>(),
            type_: if key.state == 1 {
                TypioEventType::TypioEventKeyPress
            } else {
                TypioEventType::TypioEventKeyRelease
            },
            keycode: key.keycode,
            keysym: key.keysym,
            modifiers: effective.0,
            unicode: key.unicode.chars().next().unwrap_or('\0') as u32,
            time: key.time as u64,
            is_repeat,
            base_keysym: key.keysym,
        };

        let timing_enabled = tracing::enabled!(target: "typio.engine.key", tracing::Level::TRACE)
            || tracing::enabled!(target: "typio.engine.key", tracing::Level::INFO);
        let started = timing_enabled.then(std::time::Instant::now);
        let consumed = typio::input_context::typio_input_context_process_key(self.ctx, &event);
        if let Some(started) = started {
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            if elapsed_ms > 5.0 {
                tracing::info!(
                    target: "typio.engine.key",
                    elapsed_ms,
                    keysym = key.keysym,
                    keycode = key.keycode,
                    pressed = key.state == 1,
                    is_repeat,
                    consumed,
                    "slow process_key"
                );
            } else {
                tracing::trace!(
                    target: "typio.engine.key",
                    elapsed_ms,
                    keysym = key.keysym,
                    keycode = key.keycode,
                    pressed = key.state == 1,
                    is_repeat,
                    consumed,
                    "process_key"
                );
            }
        }
        consumed
    }

    /// Drain any pending commit text and forward it to the compositor.
    pub fn drain_commit(&self, frontend: &mut InputMethodState) {
        if let Ok(mut slot) = PENDING_COMMIT.lock() {
            if let Some(text) = slot.take() {
                frontend.commit_string_and_flush(&text);
            }
        }
    }

    /// Record that a key was forwarded to the application via the
    /// virtual keyboard. Arms the repeat timer in `Forward` mode so the
    /// main loop will keep sending synthetic presses until release.
    pub fn on_forward(&mut self, key: DecodedKeyEvent) {
        self.repeat_key = Some(key);
        self.repeat_mode = RepeatMode::Forward;
    }

    /// Record that a key was consumed by the engine. Arms the repeat
    /// timer in `Engine` mode so the main loop will keep re-dispatching
    /// the key with `is_repeat: true` until release.
    pub fn on_consumed(&mut self, key: DecodedKeyEvent) {
        self.repeat_key = Some(key);
        self.repeat_mode = RepeatMode::Engine;
    }

    /// Record a key release. Clears repeat state if it matches the
    /// currently held key, stopping further repeats.
    pub fn on_release(&mut self, key: &DecodedKeyEvent) {
        if let Some(ref last) = self.repeat_key {
            if last.keycode == key.keycode {
                self.repeat_key = None;
                self.repeat_mode = RepeatMode::Forward;
            }
        }
    }

    /// Drive one repeat tick. Called by the main loop when the repeat
    /// timer fires.
    ///
    /// For [`RepeatMode::Forward`] the held key is re-sent to the virtual
    /// keyboard. For [`RepeatMode::Engine`] the key is re-dispatched to
    /// the engine with `is_repeat: true`; if the engine declines the
    /// repeat, the repeat chain is stopped ([`RepeatOutcome::Stopped`])
    /// and the caller should disarm the timer.
    pub fn dispatch_repeat(
        &mut self,
        frontend: &mut InputMethodState,
        xkb_mods_depressed: u32,
    ) -> RepeatOutcome {
        let Some(key) = self.repeat_key.clone() else {
            return RepeatOutcome::Stopped;
        };
        match self.repeat_mode {
            RepeatMode::Forward => {
                frontend.forward_key(key.time, key.keycode, key.state);
                RepeatOutcome::Forwarded
            }
            RepeatMode::Engine => {
                // Host-managed selection repeats (e.g. held Down arrow
                // cycling candidates) take the host path before
                // re-entering the engine. Falls through when the host
                // declines — same opt-in gate as the initial press.
                if let Some(handled) = self.try_host_selection(&key, frontend) {
                    return if handled {
                        RepeatOutcome::Consumed
                    } else {
                        RepeatOutcome::Stopped
                    };
                }
                let consumed = self.dispatch_key_repeat(&key, xkb_mods_depressed);
                if consumed {
                    RepeatOutcome::Consumed
                } else {
                    // Engine declined the repeat — stop further repeats.
                    self.repeat_key = None;
                    self.repeat_mode = RepeatMode::Forward;
                    RepeatOutcome::Stopped
                }
            }
        }
    }
}

impl Drop for KeyboardRouter {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            typio::input_context::typio_input_context_focus_out(self.ctx);
            typio::input_context::typio_input_context_free(self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl KeyboardRouter {
        /// Test-only constructor that bypasses the libtypio setup. The
        /// provided context pointer is stored but not dereferenced by the
        /// lifecycle helpers under test.
        pub(crate) fn new_for_test(ctx: *mut typio::TypioInputContext) -> Self {
            Self {
                ctx,
                repeat_key: None,
                repeat_mode: RepeatMode::Forward,
                physical_modifiers: Modifiers::NONE,
                active_generation: 1,
                key_tracking_states: vec![KeyTrackState::default(); MAX_TRACKED_KEYS],
                key_tracking_generations: vec![0u32; MAX_TRACKED_KEYS],
                shortcut_saw_non_modifier: false,
                non_modifier_held: [false; 256],
                held_non_modifier_count: 0,
                shortcut_already_triggered: false,
                shortcut_fired: false,
                engine_tracked_mods: Modifiers::NONE,
                preedit_tracking: PreeditTracking::new(),
            }
        }
    }

    #[test]
    fn scrub_generation_increments_epoch_and_clears_tracking() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        router.key_tracking_states[0] = KeyTrackState::Forwarded;
        router.key_tracking_generations[0] = 7;
        router.repeat_key = Some(DecodedKeyEvent {
            keycode: 1,
            xkb_keycode: 9,
            keysym: 0x0061,
            unicode: "a".to_string(),
            state: 1,
            time: 0,
        });

        let before = router.active_generation();
        router.scrub_generation();

        assert_eq!(router.active_generation(), before + 1);
        assert_eq!(router.key_tracking_states[0], KeyTrackState::Idle);
        assert_eq!(router.key_tracking_generations[0], 0);
        assert!(router.repeat_key.is_none());
    }

    #[test]
    fn scrub_generation_never_produces_zero_generation() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        router.active_generation = u32::MAX;
        router.scrub_generation();
        assert_eq!(router.active_generation(), 1);
    }

    #[test]
    fn soft_pause_marks_forwarded_keys_released_pending() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        router.key_tracking_states[0] = KeyTrackState::Forwarded;
        router.key_tracking_states[1] = KeyTrackState::Idle;
        router.key_tracking_states[2] = KeyTrackState::AppShortcut;
        router.key_tracking_states[3] = KeyTrackState::SuppressedStartup;
        router.repeat_key = Some(DecodedKeyEvent {
            keycode: 1,
            xkb_keycode: 9,
            keysym: 0x0061,
            unicode: "a".to_string(),
            state: 1,
            time: 0,
        });

        router.soft_pause();

        assert_eq!(
            router.key_tracking_states[0],
            KeyTrackState::ReleasedPending
        );
        assert_eq!(router.key_tracking_states[1], KeyTrackState::Idle);
        assert_eq!(
            router.key_tracking_states[2],
            KeyTrackState::ReleasedPending
        );
        assert_eq!(
            router.key_tracking_states[3],
            KeyTrackState::SuppressedStartup
        );
        assert!(router.repeat_key.is_none());
        assert_eq!(router.physical_modifiers, Modifiers::NONE);
    }

    #[test]
    fn is_focused_handles_null_ctx_gracefully() {
        let router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        assert!(!router.is_focused());
    }

    #[test]
    fn preedit_tracking_starts_empty_and_resets() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        router.preedit_tracking.last_text = Some("ni".to_string());
        router.preedit_tracking.last_cursor = 2;
        router.preedit_tracking_reset();
        assert_eq!(router.preedit_tracking.last_text, None);
        assert_eq!(router.preedit_tracking.last_cursor, -1);
    }

    #[test]
    fn preedit_dedup_classifies_arrow_vs_text_change() {
        // Simulate the canonical arrow-navigation case: same preedit
        // text and cursor, only the selected candidate index differs.
        // The plan must be SyncPanelOnly so `drain_composition` skips
        // the `set_preedit_string + commit` Wayland round-trip.
        let plan = text_ui_plan_update(Some("nihao"), 5, Some("nihao"), 5);
        assert_eq!(plan, TextUiPlan::SyncPanelOnly);

        // A real preedit edit transitions to SyncPreeditAndPanel.
        let plan = text_ui_plan_update(Some("ni"), 2, Some("nih"), 3);
        assert_eq!(plan, TextUiPlan::SyncPreeditAndPanel);

        // Cursor-only move inside the same preedit also resends.
        let plan = text_ui_plan_update(Some("ni"), 1, Some("ni"), 2);
        assert_eq!(plan, TextUiPlan::SyncPreeditAndPanel);
    }

    #[test]
    fn non_modifier_release_clears_chord_gate() {
        // Regression: after typing and releasing a regular key, the
        // Ctrl+Shift chord must be reachable on the first try. The old
        // sticky `shortcut_saw_non_modifier` flag stayed true until a
        // modifier was released, swallowing the first chord.
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());

        // Pressing a non-modifier key (evdev scancode for 'a' is 30)
        // taints the gesture.
        router.track_non_modifier_held(30, true);
        assert!(router.shortcut_saw_non_modifier);

        // Releasing it must clear the gate immediately.
        router.track_non_modifier_held(30, false);
        assert!(!router.shortcut_saw_non_modifier);
    }

    #[test]
    fn non_modifier_chord_gate_handles_repeat_and_overlap() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());

        // Compositor-driven auto-repeat of an already-held key must not
        // inflate the count (otherwise a release wouldn't zero it).
        router.track_non_modifier_held(30, true);
        router.track_non_modifier_held(30, true);
        router.track_non_modifier_held(30, true);
        assert!(router.shortcut_saw_non_modifier);
        router.track_non_modifier_held(30, false);
        assert!(!router.shortcut_saw_non_modifier);

        // Two distinct keys held: gate stays true until both release.
        router.track_non_modifier_held(30, true);
        router.track_non_modifier_held(48, true); // 'b'
        assert!(router.shortcut_saw_non_modifier);
        router.track_non_modifier_held(30, false);
        assert!(router.shortcut_saw_non_modifier);
        router.track_non_modifier_held(48, false);
        assert!(!router.shortcut_saw_non_modifier);
    }

    #[test]
    fn scrub_generation_clears_non_modifier_tracker() {
        let mut router = KeyboardRouter::new_for_test(std::ptr::null_mut());
        router.track_non_modifier_held(30, true);
        assert!(router.shortcut_saw_non_modifier);

        router.scrub_generation();

        assert!(!router.shortcut_saw_non_modifier);
        assert_eq!(router.held_non_modifier_count, 0);
    }
}
