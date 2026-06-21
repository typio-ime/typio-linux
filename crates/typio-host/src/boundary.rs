//! Pure boundary-bridge policy for activation/deactivation handoff.
//!
//! Port of `src/engine/boundary.c`. These predicates decide, at the engine
//! boundary, whether stale key/modifier state must be flushed or carried
//! across an activation handoff. All pure — no I/O, no state.

use crate::focus_controller::GrabWant;

// ── Keysym / modifier constants ──────────────────────────────────────────
//
// Subset of `libtypio/include/typio/abi/event.h` and `.../types.h`. The
// boundary bridge reasons in the libtypio *ABI* modifier space (the values
// the engine sees), which is distinct from the host's internal xkb modifier
// bit layout used by `keyboard_policy`.

/// Numeric keysym, matching `xkb_keysym_t` / `TYPIO_KEY_*` constants.
pub type Keysym = u32;

/// `TYPIO_KEY_Return`.
pub const KEY_RETURN: Keysym = 0xff0d;
/// `TYPIO_KEY_KP_Enter`.
pub const KEY_KP_ENTER: Keysym = 0xff8d;

/// `TYPIO_MOD_CTRL` (matches `libtypio/include/typio/abi/types.h`).
pub const MOD_CTRL: u32 = 1 << 1;
/// `TYPIO_MOD_ALT`.
pub const MOD_ALT: u32 = 1 << 2;
/// `TYPIO_MOD_SUPER`.
pub const MOD_SUPER: u32 = 1 << 3;

/// Modifiers that mark a key event as part of a shortcut/chord — a release
/// of one of these (or Enter) must forward an orphan-release cleanup so the
/// engine does not strand a half-applied chord. Matches the C macro
/// `(TYPIO_MOD_CTRL | TYPIO_MOD_ALT | TYPIO_MOD_SUPER)`.
pub const SHORTCUT_MODIFIER_MASK: u32 = MOD_CTRL | MOD_ALT | MOD_SUPER;

/// Should an orphan key release be forwarded as a cleanup event?
///
/// True for Enter / KP_Enter unconditionally, or when a blocking modifier was
/// seen, or when the event carries any shortcut modifier (Ctrl/Alt/Super).
pub fn should_forward_orphan_release_cleanup(
    keysym: Keysym,
    modifiers: u32,
    saw_blocking_modifier: bool,
) -> bool {
    if keysym == KEY_RETURN || keysym == KEY_KP_ENTER {
        return true;
    }
    saw_blocking_modifier || (modifiers & SHORTCUT_MODIFIER_MASK) != 0
}

/// Should carried virtual-keyboard modifiers be reset? Carried modifiers are
/// only preserved across a soft pause; any other handoff resets them.
pub fn should_reset_carried_modifiers(want: GrabWant, carried_vk_modifiers: bool) -> bool {
    carried_vk_modifiers && want != GrabWant::SoftPause
}

/// Should the current modifier state be carried across a soft pause? Only when
/// we own the current grab generation, the want is a soft pause, and there is
/// actually some modifier state (depressed/latched/locked) to carry.
pub fn should_carry_modifiers(
    want: GrabWant,
    own_current_generation: bool,
    mods_depressed: u32,
    mods_latched: u32,
    mods_locked: u32,
) -> bool {
    if !own_current_generation || want != GrabWant::SoftPause {
        return false;
    }
    mods_depressed != 0 || mods_latched != 0 || mods_locked != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOD_SHIFT: u32 = 1 << 0;
    const MOD_NONE: u32 = 0;

    #[test]
    fn cleans_up_orphan_release_for_shortcut_modifiers() {
        assert!(should_forward_orphan_release_cleanup(
            0x0020, MOD_CTRL, false
        ));
        assert!(should_forward_orphan_release_cleanup(
            0x0020, MOD_ALT, false
        ));
        assert!(should_forward_orphan_release_cleanup(
            0x0020, MOD_SUPER, false
        ));
        assert!(!should_forward_orphan_release_cleanup(
            0x0020, MOD_SHIFT, false
        ));
        assert!(!should_forward_orphan_release_cleanup(
            0x0020, MOD_NONE, false
        ));
    }

    #[test]
    fn cleans_up_orphan_release_after_modifier_was_seen() {
        assert!(should_forward_orphan_release_cleanup(
            0x0020, MOD_NONE, true
        ));
    }

    #[test]
    fn cleans_up_orphan_release_for_enter_without_modifiers() {
        assert!(should_forward_orphan_release_cleanup(
            KEY_RETURN, MOD_NONE, false
        ));
        assert!(should_forward_orphan_release_cleanup(
            KEY_KP_ENTER,
            MOD_NONE,
            false
        ));
    }

    #[test]
    fn resets_carried_modifiers_outside_soft_pause() {
        assert!(should_reset_carried_modifiers(GrabWant::Yes, true));
        assert!(should_reset_carried_modifiers(GrabWant::None, true));
        assert!(!should_reset_carried_modifiers(GrabWant::SoftPause, true));
        assert!(!should_reset_carried_modifiers(GrabWant::None, false));
        assert!(!should_reset_carried_modifiers(GrabWant::Yes, false));
    }

    #[test]
    fn carries_modifiers_only_for_owned_soft_pause_with_mask() {
        assert!(should_carry_modifiers(GrabWant::SoftPause, true, 1, 0, 0));
        assert!(should_carry_modifiers(GrabWant::SoftPause, true, 0, 1, 0));
        assert!(should_carry_modifiers(GrabWant::SoftPause, true, 0, 0, 1));
        assert!(!should_carry_modifiers(GrabWant::Yes, true, 1, 0, 0));
        assert!(!should_carry_modifiers(GrabWant::SoftPause, false, 1, 0, 0));
        assert!(!should_carry_modifiers(GrabWant::SoftPause, true, 0, 0, 0));
    }
}
