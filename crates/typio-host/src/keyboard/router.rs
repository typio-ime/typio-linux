//! Keyboard event router.
//!
//! Bridges the Wayland input-method keyboard grab to libtypio's input
//! context. Decides whether a key is consumed by the engine or forwarded
//! to the focused application via the virtual keyboard.

use std::ffi::{c_char, c_void, CStr};
use std::sync::Mutex;

use typio_abi::{TypioComposition, TypioEventType, TypioKeyEvent};

use crate::input_method::{DecodedKeyEvent, InputMethodState};
use crate::keyboard_policy::{
    modifier_bit_for_keysym, sync_physical_modifiers, tracking_mark_released_pending,
    tracking_reset, tracking_reset_generations, KeyTrackState, WL_KEYBOARD_KEY_STATE_PRESSED,
    WL_KEYBOARD_KEY_STATE_RELEASED,
};
use crate::repeat_timer::Modifiers;

/// Maximum number of keys tracked for symmetric press/release. Mirrors
/// `TYPIO_WL_MAX_TRACKED_KEYS` in the C host.
pub const MAX_TRACKED_KEYS: usize = 256;

/// Pending text committed by the engine since the last key dispatch.
static PENDING_COMMIT: Mutex<Option<String>> = Mutex::new(None);

/// Pending composition state since the last key dispatch.
///
/// `(preedit_text, candidates, selected_index)`. The preedit is tracked
/// separately from candidates because an engine often emits a preedit
/// before it has any candidates (e.g. pinyin after the first keystroke);
/// treating candidates as the source of preedit would hide that
/// intermediate state from the user.
static PENDING_COMPOSITION: Mutex<Option<(String, Vec<String>, usize)>> = Mutex::new(None);

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

    if let Ok(mut slot) = PENDING_COMPOSITION.lock() {
        *slot = Some((preedit, candidates, selected));
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
    /// True if any non-modifier key has been pressed since the last
    /// shortcut chord fired or was reset. Drives `chord_should_switch_engine`.
    shortcut_saw_non_modifier: bool,
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
            shortcut_already_triggered: false,
            shortcut_fired: false,
            engine_tracked_mods: Modifiers::NONE,
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
    }

    /// Drain any pending composition update and update preedit/candidates.
    pub fn drain_composition(&self, frontend: &mut InputMethodState) {
        if let Ok(mut slot) = PENDING_COMPOSITION.lock() {
            if let Some((preedit, candidates, selected)) = slot.take() {
                let preedit_len = preedit.len();
                let candidate_count = candidates.len();
                // Preedit is the source of truth for what shows inline in
                // the focused text field. Candidates drive the popup. Either
                // can change independently of the other: an empty preedit
                // with non-empty candidates means the engine is offering
                // completions; a non-empty preedit with no candidates means
                // the engine is mid-composition (e.g. pinyin after one
                // keystroke) and will show candidates later.
                if preedit.is_empty() && candidates.is_empty() {
                    frontend.clear_preedit_and_flush();
                } else {
                    let cursor = preedit.len() as u32;
                    frontend.set_preedit_and_flush(&preedit, cursor);
                }
                let composition_seq = frontend.set_candidates(candidates, selected);
                frontend.mark_panel_dirty();
                tracing::debug!(
                    target: "typio.engine.composition",
                    composition_seq,
                    preedit_len,
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

        if state == WL_KEYBOARD_KEY_STATE_PRESSED && !is_modifier_key {
            self.shortcut_saw_non_modifier = true;
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
                shortcut_already_triggered: false,
                shortcut_fired: false,
                engine_tracked_mods: Modifiers::NONE,
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
}
