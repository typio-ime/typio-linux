//! Epoch-based startup filtering for freshly activated keyboard grabs.
//!
//! Port of `src/engine/startup.c`. When a new keyboard grab is established the
//! compositor may re-send a *release* for a key that was physically held
//! before the grab existed and lifted just after. Such an orphan release has
//! no matching press in the current grab generation.
//!
//! The guard uses the Wayland dispatch epoch (a counter incremented after each
//! `wl_display_dispatch`) to bound that orphan-release window deterministically:
//! a release arriving within the first [`STARTUP_GUARD_EPOCHS`] dispatch cycles
//! after the grab was created may be a pre-grab orphan. Genuine new *presses*
//! are never filtered here — they are dropped by the grab-generation fence.

/// Number of Wayland dispatch epochs after grab creation during which key
/// presses are treated as potential compositor re-sends of held keys.
///
/// The grab request is a client→server message; the compositor's response
/// (keymap, modifiers, held-key presses) arrives in the NEXT dispatch cycle
/// (epoch + 1). We guard for two cycles to handle compositors that batch the
/// response across cycles.
pub const STARTUP_GUARD_EPOCHS: u64 = 2;

/// Is `current_epoch` within the startup guard window opened at
/// `created_at_epoch`? A current epoch before the creation epoch (clock/epoch
/// regression) is never in the window.
pub fn is_in_guard_window(created_at_epoch: u64, current_epoch: u64) -> bool {
    if current_epoch < created_at_epoch {
        return false;
    }
    (current_epoch - created_at_epoch) <= STARTUP_GUARD_EPOCHS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_window_same_epoch() {
        assert!(is_in_guard_window(10, 10));
    }

    #[test]
    fn guard_window_epoch_plus_one() {
        assert!(is_in_guard_window(10, 11));
    }

    #[test]
    fn guard_window_epoch_plus_two() {
        assert!(is_in_guard_window(10, 12));
    }

    #[test]
    fn guard_window_epoch_plus_three_outside() {
        assert!(!is_in_guard_window(10, 13));
    }

    #[test]
    fn guard_window_current_before_created() {
        assert!(!is_in_guard_window(10, 5));
    }
}
