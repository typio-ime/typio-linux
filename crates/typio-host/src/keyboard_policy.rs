//! Keyboard policy: pure decision rules for modifier handling, shortcut
//! chords, repeat gating, and per-key tracking state.
//!
//! Phase 5 port of the four `src/wayland/keyboard/policy/*.c` files
//! (modifiers.c, chords.c, repeat_guard.c, tracker.c — 227 lines of C
//! total). All pure functions; no I/O, no state, no frontend coupling.
//! These are the decision predicates the keyboard router (not yet
//! ported) consults at every key event.

use crate::repeat_timer::Modifiers;

// ── Keysym constants ─────────────────────────────────────────────────────
//
// Subset of `libtypio/include/typio/abi/event.h`. Only the keysyms the
// policy predicates consult are listed here; a full keysym table is not
// needed because the rest of the keyboard logic operates on raw u32
// keysyms from xkbcommon.

/// Numeric keysym, matching `xkb_keysym_t` / `TYPIO_KEY_*` constants.
pub type Keysym = u32;

pub const KEY_SPACE: Keysym = 0x0020;

pub const KEY_SHIFT_L: Keysym = 0xffe1;
pub const KEY_SHIFT_R: Keysym = 0xffe2;
pub const KEY_CONTROL_L: Keysym = 0xffe3;
pub const KEY_CONTROL_R: Keysym = 0xffe4;
pub const KEY_ALT_L: Keysym = 0xffe9;
pub const KEY_ALT_R: Keysym = 0xffea;
pub const KEY_SUPER_L: Keysym = 0xffeb;
pub const KEY_SUPER_R: Keysym = 0xffec;

pub const KEY_F1: Keysym = 0xffbe;
pub const KEY_F2: Keysym = 0xffbf;
pub const KEY_F3: Keysym = 0xffc0;
pub const KEY_F4: Keysym = 0xffc1;
pub const KEY_F5: Keysym = 0xffc2;
pub const KEY_F6: Keysym = 0xffc3;
pub const KEY_F7: Keysym = 0xffc4;
pub const KEY_F8: Keysym = 0xffc5;
pub const KEY_F9: Keysym = 0xffc6;
pub const KEY_F10: Keysym = 0xffc7;
pub const KEY_F11: Keysym = 0xffc8;
pub const KEY_F12: Keysym = 0xffc9;

pub const KEY_ESCAPE: Keysym = 0xff1b;
pub const KEY_BACKSPACE: Keysym = 0xff08;
pub const KEY_DELETE: Keysym = 0xffff;
pub const KEY_HOME: Keysym = 0xff50;
pub const KEY_END: Keysym = 0xff57;
pub const KEY_PAGE_UP: Keysym = 0xff55;
pub const KEY_PAGE_DOWN: Keysym = 0xff56;

/// Wayland `wl_keyboard.key_state.pressed` value (matches the wire
/// constant from `wayland-client-protocol.h`).
pub const WL_KEYBOARD_KEY_STATE_RELEASED: u32 = 0;
/// Wayland `wl_keyboard.key_state.pressed` value.
pub const WL_KEYBOARD_KEY_STATE_PRESSED: u32 = 1;

/// Bitmask of the three "blocking" modifiers: Ctrl, Alt, Super. A chord
/// of any of these suppresses plain-key routing and triggers engine
/// switch / shortcut logic.
const BLOCKING_MODIFIERS: Modifiers =
    Modifiers(Modifiers::CTRL.0 | Modifiers::ALT.0 | Modifiers::SUPER.0);

// ── modifiers ────────────────────────────────────────────────────────────

/// Map a modifier keysym to its [`Modifiers`] bit. Returns
/// [`Modifiers::NONE`] for non-modifier keysyms.
pub fn modifier_bit_for_keysym(keysym: Keysym) -> Modifiers {
    match keysym {
        KEY_SHIFT_L | KEY_SHIFT_R => Modifiers::SHIFT,
        KEY_CONTROL_L | KEY_CONTROL_R => Modifiers::CTRL,
        KEY_ALT_L | KEY_ALT_R => Modifiers::ALT,
        KEY_SUPER_L | KEY_SUPER_R => Modifiers::SUPER,
        _ => Modifiers::NONE,
    }
}

/// Compute the effective modifier mask the engine should see, given
/// physical state, xkb-derived state, and the current key event.
///
/// Port of `typio_wl_modifier_policy_effective_modifiers` in C.
///
/// - `physical_modifiers` — what the Wayland compositor reports
/// - `xkb_modifiers` — what xkbcommon derives from the keymap
/// - `active_generation_owned_keys` — true iff the host "owns" the
///   current generation of key presses (engine is in a clean state);
///   when false, blocking modifiers from xkb are folded in as a
///   safety net
/// - `keysym` — the current key's keysym
/// - `state` — `WL_KEYBOARD_KEY_STATE_PRESSED` or `_RELEASED`
pub fn effective_modifiers(
    physical_modifiers: Modifiers,
    xkb_modifiers: Modifiers,
    active_generation_owned_keys: bool,
    keysym: Keysym,
    state: u32,
) -> Modifiers {
    let locks = Modifiers(xkb_modifiers.0 & (Modifiers::CAPSLOCK.0 | Modifiers::NUMLOCK.0));
    let mut modifiers = Modifiers(physical_modifiers.0 | locks.0);
    if !active_generation_owned_keys {
        // OR in the xkb-derived blocking modifiers as a fallback.
        modifiers = Modifiers(modifiers.0 | (xkb_modifiers.0 & BLOCKING_MODIFIERS.0));
    }
    modifiers_for_current_key(modifiers, keysym, state)
}

/// Overlay xkb-derived Shift/Ctrl/Alt/Super onto physical (which may be
/// stale for those keys). Port of `sync_physical_modifiers` in C.
pub fn sync_physical_modifiers(
    physical_modifiers: Modifiers,
    xkb_modifiers: Modifiers,
) -> Modifiers {
    let blocking =
        Modifiers(Modifiers::SHIFT.0 | Modifiers::CTRL.0 | Modifiers::ALT.0 | Modifiers::SUPER.0);
    Modifiers((physical_modifiers.0 & !blocking.0) | (xkb_modifiers.0 & blocking.0))
}

fn modifiers_for_current_key(modifiers: Modifiers, keysym: Keysym, state: u32) -> Modifiers {
    let bit = modifier_bit_for_keysym(keysym);
    if bit == Modifiers::NONE {
        return modifiers;
    }
    if state == WL_KEYBOARD_KEY_STATE_PRESSED {
        Modifiers(modifiers.0 | bit.0)
    } else {
        Modifiers(modifiers.0 & !bit.0)
    }
}

// ── chords ───────────────────────────────────────────────────────────────

/// A keyboard shortcut binding: a modifier set + a trigger keysym.
/// Port of `TypioShortcutBinding` (subset — only the fields the policy
/// predicates need).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutBinding {
    /// Required modifier mask (any of Shift/Ctrl/Alt/Super).
    pub modifiers: Modifiers,
    /// Trigger keysym (the non-modifier key that completes the chord).
    pub keysym: Keysym,
}

/// True iff `keysym` is one of the physical keys that produces a
/// modifier in `binding.modifiers`.
pub fn chord_is_switch_modifier(binding: &ShortcutBinding, keysym: Keysym) -> bool {
    keysym_is_modifier_for(binding.modifiers, keysym)
}

/// True iff the chord should fire an engine switch now, given current
/// state.
///
/// Rules (mirrors `typio_wl_shortcut_chord_should_switch_engine`):
/// - already triggered in this gesture → no
/// - saw a non-modifier key in between → no
/// - current keysym isn't a chord modifier → no
/// - required modifier set isn't fully held → no
/// - otherwise yes
pub fn chord_should_switch_engine(
    binding: &ShortcutBinding,
    keysym: Keysym,
    modifiers: Modifiers,
    saw_non_modifier: bool,
    already_triggered: bool,
) -> bool {
    if already_triggered || saw_non_modifier {
        return false;
    }
    if !keysym_is_modifier_for(binding.modifiers, keysym) {
        return false;
    }
    // All required modifier bits must be set.
    (modifiers.0 & binding.modifiers.0) == binding.modifiers.0
}

fn keysym_is_modifier_for(required_mods: Modifiers, keysym: Keysym) -> bool {
    if required_mods.intersects(Modifiers::CTRL)
        && (keysym == KEY_CONTROL_L || keysym == KEY_CONTROL_R)
    {
        return true;
    }
    if required_mods.intersects(Modifiers::SHIFT)
        && (keysym == KEY_SHIFT_L || keysym == KEY_SHIFT_R)
    {
        return true;
    }
    if required_mods.intersects(Modifiers::ALT) && (keysym == KEY_ALT_L || keysym == KEY_ALT_R) {
        return true;
    }
    if required_mods.intersects(Modifiers::SUPER)
        && (keysym == KEY_SUPER_L || keysym == KEY_SUPER_R)
    {
        return true;
    }
    false
}

// ── repeat_guard ─────────────────────────────────────────────────────────

/// True iff a modifier transition between two samples should cancel any
/// in-flight key repeat. A transition in the *blocking* bits (Ctrl/Alt/
/// Super) is the only thing that matters — Shift alone doesn't cancel.
///
/// Port of `typio_wl_repeat_should_cancel_on_modifier_transition`.
pub fn repeat_should_cancel_on_modifier_transition(
    previous_modifiers: Modifiers,
    current_modifiers: Modifiers,
) -> bool {
    let prev_blocking = Modifiers(previous_modifiers.0 & BLOCKING_MODIFIERS.0);
    let curr_blocking = Modifiers(current_modifiers.0 & BLOCKING_MODIFIERS.0);
    prev_blocking != curr_blocking
}

/// True iff keyboard repeat should fire for a key in the given tracking
/// state. States like `SuppressedStartup`/`ReleasedPending`/
/// `EngineNotReady` cancel repeat.
///
/// Port of `typio_wl_repeat_should_run_for_state`.
pub fn repeat_should_run_for_state(state: KeyTrackState) -> bool {
    !matches!(
        state,
        KeyTrackState::SuppressedStartup
            | KeyTrackState::ReleasedPending
            | KeyTrackState::EngineNotReady
    )
}

// ── tracker ──────────────────────────────────────────────────────────────

/// Per-key tracking state. Port of `TypioKeyTrackState` enum.
///
/// Each key the host routes can be in exactly one of these states. The
/// state machine transitions are documented in
/// `src/wayland/internal.h` near the `TypioKeyTrackState` definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum KeyTrackState {
    /// Default: key is not currently being routed by the host.
    #[default]
    Idle = 0,
    /// Key was forwarded to the focused app (engine did not consume).
    Forwarded = 1,
    /// Key is a basic passthrough (no engine active).
    BasicPassthrough = 2,
    /// Key is an application shortcut bypassing the engine.
    AppShortcut = 3,
    /// Key was forwarded but its physical release is still pending (used
    /// to swallow the release after a focus transition).
    ReleasedPending = 4,
    /// Key is being suppressed because the engine is still warming up
    /// (engine-not-ready startup guard).
    SuppressedStartup = 5,
    /// Key is being suppressed because the active engine reports
    /// `EngineAvailability::Preparing` or `::Failed`.
    EngineNotReady = 6,
    /// Key is the push-to-talk hotkey for voice input.
    VoicePtt = 7,
    /// Key would be push-to-talk but no voice engine is available.
    VoicePttUnavail = 8,
}

impl KeyTrackState {
    /// Stable string name for trace output. Matches the C
    /// `typio_wl_key_tracking_state_name`.
    pub fn name(self) -> &'static str {
        match self {
            KeyTrackState::Idle => "idle",
            KeyTrackState::Forwarded => "forwarded",
            KeyTrackState::BasicPassthrough => "basic_passthrough",
            KeyTrackState::AppShortcut => "app_shortcut",
            KeyTrackState::ReleasedPending => "released_pending",
            KeyTrackState::SuppressedStartup => "suppressed_startup",
            KeyTrackState::EngineNotReady => "engine_not_ready",
            KeyTrackState::VoicePtt => "voice_ptt",
            KeyTrackState::VoicePttUnavail => "voice_ptt_unavail",
        }
    }
}

/// Free function form of [`KeyTrackState::name`] — matches the C API
/// surface (`typio_wl_key_tracking_state_name`).
pub fn state_name(state: KeyTrackState) -> &'static str {
    state.name()
}

/// Reset a slice of tracking states to [`KeyTrackState::Idle`]. Port of
/// `typio_wl_key_tracking_reset`.
pub fn tracking_reset(states: &mut [KeyTrackState]) {
    for s in states.iter_mut() {
        *s = KeyTrackState::Idle;
    }
}

/// Reset a slice of generation counters to 0. Port of
/// `typio_wl_key_tracking_reset_generations`.
pub fn tracking_reset_generations(generations: &mut [u32]) {
    for g in generations.iter_mut() {
        *g = 0;
    }
}

/// Mark every key in `states` that is currently in a "forwarded-like"
/// state (Forwarded, BasicPassthrough, AppShortcut) as
/// [`KeyTrackState::ReleasedPending`]. Returns the number of keys
/// transitioned. Port of `typio_wl_key_tracking_mark_released_pending`.
pub fn tracking_mark_released_pending(states: &mut [KeyTrackState]) -> usize {
    let mut changed = 0;
    for s in states.iter_mut() {
        if matches!(
            *s,
            KeyTrackState::Forwarded | KeyTrackState::BasicPassthrough | KeyTrackState::AppShortcut
        ) {
            *s = KeyTrackState::ReleasedPending;
            changed += 1;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_bit_for_keysym_maps_known_modifiers() {
        assert_eq!(modifier_bit_for_keysym(KEY_SHIFT_L), Modifiers::SHIFT);
        assert_eq!(modifier_bit_for_keysym(KEY_SHIFT_R), Modifiers::SHIFT);
        assert_eq!(modifier_bit_for_keysym(KEY_CONTROL_L), Modifiers::CTRL);
        assert_eq!(modifier_bit_for_keysym(KEY_CONTROL_R), Modifiers::CTRL);
        assert_eq!(modifier_bit_for_keysym(KEY_ALT_L), Modifiers::ALT);
        assert_eq!(modifier_bit_for_keysym(KEY_ALT_R), Modifiers::ALT);
        assert_eq!(modifier_bit_for_keysym(KEY_SUPER_L), Modifiers::SUPER);
        assert_eq!(modifier_bit_for_keysym(KEY_SUPER_R), Modifiers::SUPER);
        assert_eq!(modifier_bit_for_keysym(KEY_SPACE), Modifiers::NONE);
        assert_eq!(modifier_bit_for_keysym(KEY_F1), Modifiers::NONE);
    }

    #[test]
    fn effective_modifiers_or_blocks_when_generation_not_owned() {
        let phys = Modifiers::SHIFT;
        let xkb = Modifiers(Modifiers::CTRL.0 | Modifiers::ALT.0);
        let m = effective_modifiers(phys, xkb, false, KEY_SPACE, WL_KEYBOARD_KEY_STATE_PRESSED);
        assert!(m.intersects(Modifiers::SHIFT));
        assert!(m.intersects(Modifiers::CTRL));
        assert!(m.intersects(Modifiers::ALT));
    }

    #[test]
    fn effective_modifiers_drops_blocking_modifiers_when_owned() {
        let phys = Modifiers::SHIFT;
        let xkb = Modifiers(Modifiers::CTRL.0 | Modifiers::ALT.0);
        let m = effective_modifiers(phys, xkb, true, KEY_SPACE, WL_KEYBOARD_KEY_STATE_PRESSED);
        assert!(m.intersects(Modifiers::SHIFT));
        assert!(!m.intersects(Modifiers::CTRL));
        assert!(!m.intersects(Modifiers::ALT));
    }

    #[test]
    fn effective_modifiers_toggles_bit_for_current_key_press() {
        let m = effective_modifiers(
            Modifiers::NONE,
            Modifiers::NONE,
            true,
            KEY_SHIFT_L,
            WL_KEYBOARD_KEY_STATE_PRESSED,
        );
        assert!(m.intersects(Modifiers::SHIFT));
    }

    #[test]
    fn effective_modifiers_toggles_bit_for_current_key_release() {
        let m = effective_modifiers(
            Modifiers::SHIFT,
            Modifiers::NONE,
            true,
            KEY_SHIFT_L,
            WL_KEYBOARD_KEY_STATE_RELEASED,
        );
        assert!(!m.intersects(Modifiers::SHIFT));
    }

    #[test]
    fn sync_physical_modifiers_overlays_xkb_blocking_bits() {
        let phys = Modifiers::NONE;
        let xkb = Modifiers(Modifiers::SHIFT.0 | Modifiers::CTRL.0);
        let m = sync_physical_modifiers(phys, xkb);
        assert!(m.intersects(Modifiers::SHIFT));
        assert!(m.intersects(Modifiers::CTRL));
    }

    #[test]
    fn repeat_should_cancel_on_modifier_transition_logic() {
        // No transition in blocking mods → don't cancel.
        assert!(!repeat_should_cancel_on_modifier_transition(
            Modifiers::NONE,
            Modifiers::SHIFT,
        ));
        // Ctrl added → cancel.
        assert!(repeat_should_cancel_on_modifier_transition(
            Modifiers::NONE,
            Modifiers::CTRL,
        ));
        // Ctrl removed → cancel.
        assert!(repeat_should_cancel_on_modifier_transition(
            Modifiers::CTRL,
            Modifiers::NONE,
        ));
        // Ctrl→Alt is still a transition (different bits) → cancel.
        assert!(repeat_should_cancel_on_modifier_transition(
            Modifiers::CTRL,
            Modifiers::ALT,
        ));
    }

    #[test]
    fn repeat_should_run_for_state_respects_lifecycle_states() {
        use KeyTrackState::*;
        assert!(!repeat_should_run_for_state(SuppressedStartup));
        assert!(!repeat_should_run_for_state(ReleasedPending));
        assert!(!repeat_should_run_for_state(EngineNotReady));
        assert!(repeat_should_run_for_state(Idle));
        assert!(repeat_should_run_for_state(Forwarded));
        assert!(repeat_should_run_for_state(AppShortcut));
    }

    #[test]
    fn chord_is_switch_modifier_recognises_chord_keys() {
        let binding = ShortcutBinding {
            modifiers: Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            keysym: KEY_SPACE,
        };
        assert!(chord_is_switch_modifier(&binding, KEY_CONTROL_L));
        assert!(chord_is_switch_modifier(&binding, KEY_SHIFT_R));
        assert!(!chord_is_switch_modifier(&binding, KEY_ALT_L));
        assert!(!chord_is_switch_modifier(&binding, KEY_SPACE));
    }

    #[test]
    fn chord_should_switch_engine_requires_all_chord_mods_held() {
        let binding = ShortcutBinding {
            modifiers: Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            keysym: KEY_SPACE,
        };
        // Only Ctrl held → not all of (Ctrl+Shift).
        assert!(!chord_should_switch_engine(
            &binding,
            KEY_CONTROL_L,
            Modifiers::CTRL,
            false,
            false,
        ));
        // Both held + keying Ctrl_L → switch.
        assert!(chord_should_switch_engine(
            &binding,
            KEY_CONTROL_L,
            Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            false,
            false,
        ));
        // Already triggered → don't re-trigger.
        assert!(!chord_should_switch_engine(
            &binding,
            KEY_CONTROL_L,
            Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            false,
            true,
        ));
        // Saw a non-modifier key in between → cancel.
        assert!(!chord_should_switch_engine(
            &binding,
            KEY_CONTROL_L,
            Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
            true,
            false,
        ));
    }

    #[test]
    fn state_name_covers_all_variants() {
        use KeyTrackState::*;
        for s in [
            Idle,
            Forwarded,
            BasicPassthrough,
            AppShortcut,
            ReleasedPending,
            SuppressedStartup,
            EngineNotReady,
            VoicePtt,
            VoicePttUnavail,
        ] {
            let name = state_name(s);
            assert!(!name.is_empty());
            assert_ne!(name, "unknown");
        }
    }

    #[test]
    fn tracking_reset_zeroes_all_states() {
        let mut states = [
            KeyTrackState::Forwarded,
            KeyTrackState::AppShortcut,
            KeyTrackState::Idle,
        ];
        tracking_reset(&mut states);
        assert!(states.iter().all(|s| *s == KeyTrackState::Idle));
    }

    #[test]
    fn tracking_reset_generations_zeroes_all() {
        let mut gens = [1u32, 2, 3, 4];
        tracking_reset_generations(&mut gens);
        assert!(gens.iter().all(|g| *g == 0));
    }

    #[test]
    fn tracking_mark_released_pending_only_transitions_forwarded_like() {
        let mut states = [
            KeyTrackState::Forwarded,
            KeyTrackState::Idle,
            KeyTrackState::BasicPassthrough,
            KeyTrackState::AppShortcut,
            KeyTrackState::SuppressedStartup,
            KeyTrackState::VoicePtt,
        ];
        let changed = tracking_mark_released_pending(&mut states);
        assert_eq!(changed, 3);
        assert_eq!(states[0], KeyTrackState::ReleasedPending);
        assert_eq!(states[1], KeyTrackState::Idle);
        assert_eq!(states[2], KeyTrackState::ReleasedPending);
        assert_eq!(states[3], KeyTrackState::ReleasedPending);
        assert_eq!(states[4], KeyTrackState::SuppressedStartup);
        assert_eq!(states[5], KeyTrackState::VoicePtt);
    }
}
