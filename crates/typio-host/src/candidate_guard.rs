//! Host-managed candidate selection: pure decision rules.
//!
//! Phase 7 port of the pure parts of `src/wayland/candidate_guard.c`
//! (170 lines of C). When an engine publishes a composition with
//! host-managed-selection flags set, the host intercepts the
//! corresponding navigation/selection keys before they reach the
//! engine's `process_key`. The host manages the selected index locally
//! and commits via `commit_candidate`.
//!
//! ## What this module ports
//!
//! All the pure decision logic:
//! - [`HostSelKey`] enum (the 16 host-selection key codes)
//! - [`HostSelCategory`] enum (4 functional groups + NONE)
//! - [`HostSelectionFlags`] bitset (matches the engine contract)
//! - [`host_selection_keysym`] — keysym → SelKey
//! - [`host_selection_category`] — SelKey → Category
//! - [`host_selection_resolve`] — SelKey + current index + count → target index
//! - [`host_selection_is_commit`] — is this SelKey a commit-style key?
//! - [`should_consume_key`] — given (candidate_count, flags, keysym),
//!   should the host swallow this keysym?
//!
//! ## What is NOT ported
//!
//! `typio_wl_host_selection_try_commit` in C — it needed the input
//! context (`session->ctx`) to actually call `commit_candidate`. The
//! pure resolution is in [`host_selection_resolve`]; the commit call
//! lives in [`KeyboardRouter::try_host_selection`] in `router.rs`,
//! which threads the libtypio input-context pointer through and falls
//! back to `process_key_engine` when the engine declines (returns
//! `TypioErrorNotFound`).
//!
//! ## Opt-in semantics (deviation from the C ancestor)
//!
//! The C host intercepted arrow keys by default whenever candidates
//! existed. The Rust port requires explicit opt-in via a non-empty
//! `TypioHostManagedSelection` flag set, so engines that have been
//! running un-intercepted on this daemon continue to receive every
//! key. See [`should_consume_key`].

// Constants below mirror the upstream XCB/X11 `XKB_KEY_*` names which
// use mixed case; we keep the same names so grep'ers can cross-reference
// against the C version and the xkbcommon-keysyms.h header. We also keep
// the complete digit set even if unused outside this module — silencing
// both lints at the module boundary rather than per-constant.
#![allow(non_snake_case)]

use crate::keyboard_policy::Keysym;
use bitflags::bitflags;

// XKB keysyms we need that aren't in keyboard_policy's subset.
// All kept here even if unused outside the module — they are the
// complete digit/space/enter/arrow set the candidate guard consults.
#[allow(dead_code)]
const XKB_KEY_UP: Keysym = 0xff52;
#[allow(dead_code)]
const XKB_KEY_DOWN: Keysym = 0xff54;
#[allow(dead_code)]
const XKB_KEY_LEFT: Keysym = 0xff51;
#[allow(dead_code)]
const XKB_KEY_RIGHT: Keysym = 0xff53;
#[allow(dead_code)]
const XKB_KEY_RETURN: Keysym = 0xff0d;
#[allow(dead_code)]
const XKB_KEY_KP_ENTER: Keysym = 0xff8d;
const XKB_KEY_SPACE: Keysym = 0x0020;
const XKB_KEY_0: Keysym = 0x0030;
const XKB_KEY_1: Keysym = 0x0031;
#[allow(dead_code)]
const XKB_KEY_2: Keysym = 0x0032;
#[allow(dead_code)]
const XKB_KEY_3: Keysym = 0x0033;
#[allow(dead_code)]
const XKB_KEY_4: Keysym = 0x0034;
#[allow(dead_code)]
const XKB_KEY_5: Keysym = 0x0035;
#[allow(dead_code)]
const XKB_KEY_6: Keysym = 0x0036;
#[allow(dead_code)]
const XKB_KEY_7: Keysym = 0x0037;
#[allow(dead_code)]
const XKB_KEY_8: Keysym = 0x0038;
const XKB_KEY_9: Keysym = 0x0039;

/// Host-managed-selection key code. Port of `TypioWlHostSelKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum HostSelKey {
    /// No host-managed interpretation for this key.
    #[default]
    None = 0,
    /// Up arrow or Left arrow.
    NavUp = 1,
    /// Down arrow or Right arrow.
    NavDown = 2,
    /// Space — commit the currently-selected candidate.
    CommitSelected = 3,
    /// Enter / KP_Enter — commit preedit as-is (no candidate selection).
    CommitRaw = 4,
    CommitIndex1 = 5,
    CommitIndex2 = 6,
    CommitIndex3 = 7,
    CommitIndex4 = 8,
    CommitIndex5 = 9,
    CommitIndex6 = 10,
    CommitIndex7 = 11,
    CommitIndex8 = 12,
    CommitIndex9 = 13,
    CommitIndex0 = 14,
}

/// Functional category of a [`HostSelKey`]. Used to gate by flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum HostSelCategory {
    #[default]
    None = 0,
    /// Up/Down/Left/Right navigation.
    Navigate = 1,
    /// Space — commit current selection.
    Commit = 2,
    /// Enter — commit preedit raw.
    CommitRaw = 3,
    /// Number keys 0–9 — pick by index.
    IndexPick = 4,
}

bitflags! {
    /// Engine-declared host-managed-selection capability flags. Matches
    /// the C constants in `typio/abi/input_context.h`:
    /// `TYPIO_HOST_SEL_NAVIGATE / _COMMIT / _INDEX_PICK / _COMMIT_RAW`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct HostSelectionFlags: u32 {
        /// Up/Down/Left/Right.
        const NAVIGATE   = 1 << 0;
        /// Space.
        const COMMIT     = 1 << 1;
        /// 0–9.
        const INDEX_PICK = 1 << 2;
        /// Enter / KP_Enter — commit preedit as-is.
        const COMMIT_RAW = 1 << 3;
    }
}

/// True iff `keysym` is Up/Down/Left/Right.
pub fn is_navigation_keysym(keysym: Keysym) -> bool {
    matches!(
        keysym,
        XKB_KEY_UP | XKB_KEY_DOWN | XKB_KEY_LEFT | XKB_KEY_RIGHT
    )
}

/// Map a keysym to a [`HostSelKey`]. Returns [`HostSelKey::None`] for
/// keysyms the host doesn't manage.
pub fn host_selection_keysym(keysym: Keysym) -> HostSelKey {
    match keysym {
        XKB_KEY_UP | XKB_KEY_LEFT => HostSelKey::NavUp,
        XKB_KEY_DOWN | XKB_KEY_RIGHT => HostSelKey::NavDown,
        XKB_KEY_SPACE => HostSelKey::CommitSelected,
        XKB_KEY_RETURN | XKB_KEY_KP_ENTER => HostSelKey::CommitRaw,
        _ => {
            if (XKB_KEY_1..=XKB_KEY_9).contains(&keysym) {
                // INDEX_1 + (keysym - '1') — the enum discriminants are
                // contiguous starting at HostSelKey::CommitIndex1 = 5.
                let offset = (keysym - XKB_KEY_1) as u8;
                index_offset_to_sel(offset)
            } else if keysym == XKB_KEY_0 {
                HostSelKey::CommitIndex0
            } else {
                HostSelKey::None
            }
        }
    }
}

#[inline]
fn index_offset_to_sel(offset: u8) -> HostSelKey {
    match offset {
        0 => HostSelKey::CommitIndex1,
        1 => HostSelKey::CommitIndex2,
        2 => HostSelKey::CommitIndex3,
        3 => HostSelKey::CommitIndex4,
        4 => HostSelKey::CommitIndex5,
        5 => HostSelKey::CommitIndex6,
        6 => HostSelKey::CommitIndex7,
        7 => HostSelKey::CommitIndex8,
        8 => HostSelKey::CommitIndex9,
        _ => HostSelKey::None,
    }
}

/// Classify a [`HostSelKey`] into its functional category.
pub fn host_selection_category(sel: HostSelKey) -> HostSelCategory {
    use HostSelKey::*;
    match sel {
        NavUp | NavDown => HostSelCategory::Navigate,
        CommitSelected => HostSelCategory::Commit,
        CommitRaw => HostSelCategory::CommitRaw,
        CommitIndex1 | CommitIndex2 | CommitIndex3 | CommitIndex4 | CommitIndex5 | CommitIndex6
        | CommitIndex7 | CommitIndex8 | CommitIndex9 | CommitIndex0 => HostSelCategory::IndexPick,
        None => HostSelCategory::None,
    }
}

/// True iff `sel` is any commit-style key (selected, raw, or indexed).
pub fn host_selection_is_commit(sel: HostSelKey) -> bool {
    use HostSelKey::*;
    matches!(
        sel,
        CommitSelected
            | CommitRaw
            | CommitIndex1
            | CommitIndex2
            | CommitIndex3
            | CommitIndex4
            | CommitIndex5
            | CommitIndex6
            | CommitIndex7
            | CommitIndex8
            | CommitIndex9
            | CommitIndex0
    )
}

/// Resolve a [`HostSelKey`] to a concrete candidate index, given the
/// currently-selected index and the total candidate count.
///
/// Returns `Some(index)` if the key applies, `None` if it doesn't (e.g.
/// index pick beyond the candidate count, or `None` key, or zero
/// candidates).
///
/// Mirrors `typio_wl_host_selection_resolve` in C, except the C version
/// returns `-1` for "no resolution" — we use `Option<usize>` for
/// idiomatic Rust. The conversion is `idx as i32` at the C boundary if
/// needed.
pub fn host_selection_resolve(
    sel: HostSelKey,
    current_selected: usize,
    candidate_count: usize,
) -> Option<usize> {
    if candidate_count == 0 {
        return Option::None;
    }
    let max = candidate_count - 1;
    let target: Option<usize> = match sel {
        HostSelKey::NavUp => Some(current_selected.saturating_sub(1)),
        HostSelKey::NavDown => {
            if current_selected < max {
                Some(current_selected + 1)
            } else {
                Some(max)
            }
        }
        HostSelKey::CommitSelected => Some(current_selected),
        HostSelKey::CommitRaw => Option::None, // raw commit doesn't pick an index
        HostSelKey::CommitIndex0 => (9 < candidate_count).then_some(9),
        HostSelKey::CommitIndex1 => Some(0),
        HostSelKey::CommitIndex2 => Some(1),
        HostSelKey::CommitIndex3 => Some(2),
        HostSelKey::CommitIndex4 => Some(3),
        HostSelKey::CommitIndex5 => Some(4),
        HostSelKey::CommitIndex6 => Some(5),
        HostSelKey::CommitIndex7 => Some(6),
        HostSelKey::CommitIndex8 => Some(7),
        HostSelKey::CommitIndex9 => Some(8),
        HostSelKey::None => Option::None,
    };
    // Filter out-of-range index picks.
    target.filter(|&i| i < candidate_count)
}

/// Concrete action the host should take for a key under host-managed
/// selection. Returned by [`classify_host_selection`]; applied by
/// `KeyboardRouter::try_host_selection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSelectionAction {
    /// Move the highlight to this candidate index locally. The engine
    /// does not see the key.
    Navigate(usize),
    /// Call `typio_input_context_commit_candidate(ctx, index)` so the
    /// engine can dispatch its `commit_candidate` vtable entry.
    Commit(usize),
    /// Swallow the key without further action. Used for releases under
    /// host-managed selection so the engine doesn't observe an unpaired
    /// release for a press it didn't see.
    Swallow,
}

/// Pure decision: given the key state, keysym, candidate count,
/// currently-selected index, and the engine's declared flags, decide
/// what the host should do. Returns `None` when the host should fall
/// back to `process_key` (engine retains control).
///
/// Split out from `try_host_selection` so it is unit-testable without
/// an `InputMethodState` (which requires live Wayland globals). The
/// router's `try_host_selection` is the effectful layer that reads
/// state, calls this, and applies the result.
pub fn classify_host_selection(
    key_state_pressed: bool,
    keysym: Keysym,
    candidate_count: usize,
    selected: usize,
    flags: HostSelectionFlags,
) -> Option<HostSelectionAction> {
    if !should_consume_key(candidate_count, flags, keysym) {
        return None;
    }
    // After this point the host owns the key.
    if !key_state_pressed {
        return Some(HostSelectionAction::Swallow);
    }
    let sel_key = host_selection_keysym(keysym);
    if host_selection_is_commit(sel_key) {
        // CommitRaw (Enter) does not resolve to an index — let the
        // engine handle the raw preedit commit.
        let idx = host_selection_resolve(sel_key, selected, candidate_count)?;
        return Some(HostSelectionAction::Commit(idx));
    }
    let new_idx = host_selection_resolve(sel_key, selected, candidate_count)?;
    Some(HostSelectionAction::Navigate(new_idx))
}

/// Decide whether the host should swallow `keysym` instead of forwarding
/// it to the engine's `process_key`. Returns `true` when:
///
/// 1. there are candidates to navigate (`candidate_count > 0`), AND
/// 2. `flags` is non-empty AND declares the category this keysym falls
///    into.
///
/// **Opt-in only.** The C ancestor (`typio_wl_candidate_guard_should_consume`)
/// fell back to "always intercept arrows when candidates exist" when
/// `flags` was empty. The Rust port ships opt-in to avoid taking over
/// candidate navigation from engines that have adapted to running
/// un-intercepted on this host: an engine that wants host-managed
/// selection must explicitly declare a non-zero `TypioHostManagedSelection`
/// flag (ADR-0012). Engines that don't are left fully in charge, exactly
/// as before.
pub fn should_consume_key(
    candidate_count: usize,
    flags: HostSelectionFlags,
    keysym: Keysym,
) -> bool {
    if candidate_count == 0 || flags.is_empty() {
        return false;
    }
    let sel = host_selection_keysym(keysym);
    if matches!(sel, HostSelKey::None) {
        return false;
    }
    let cat = host_selection_category(sel);
    match cat {
        HostSelCategory::Navigate => flags.contains(HostSelectionFlags::NAVIGATE),
        HostSelCategory::Commit => flags.contains(HostSelectionFlags::COMMIT),
        HostSelCategory::CommitRaw => flags.contains(HostSelectionFlags::COMMIT_RAW),
        HostSelCategory::IndexPick => flags.contains(HostSelectionFlags::INDEX_PICK),
        HostSelCategory::None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_classification_basic() {
        assert_eq!(host_selection_keysym(XKB_KEY_UP), HostSelKey::NavUp);
        assert_eq!(host_selection_keysym(XKB_KEY_LEFT), HostSelKey::NavUp);
        assert_eq!(host_selection_keysym(XKB_KEY_DOWN), HostSelKey::NavDown);
        assert_eq!(host_selection_keysym(XKB_KEY_RIGHT), HostSelKey::NavDown);
        assert_eq!(
            host_selection_keysym(XKB_KEY_SPACE),
            HostSelKey::CommitSelected
        );
        assert_eq!(host_selection_keysym(XKB_KEY_RETURN), HostSelKey::CommitRaw);
        assert_eq!(
            host_selection_keysym(XKB_KEY_KP_ENTER),
            HostSelKey::CommitRaw
        );
        // Number keys map to indexed picks.
        assert_eq!(host_selection_keysym(XKB_KEY_1), HostSelKey::CommitIndex1);
        assert_eq!(host_selection_keysym(XKB_KEY_5), HostSelKey::CommitIndex5);
        assert_eq!(host_selection_keysym(XKB_KEY_9), HostSelKey::CommitIndex9);
        assert_eq!(host_selection_keysym(XKB_KEY_0), HostSelKey::CommitIndex0);
        // Anything else is None.
        assert_eq!(host_selection_keysym(0xffbe), HostSelKey::None);
        assert_eq!(host_selection_keysym(0xffffff), HostSelKey::None);
    }

    #[test]
    fn category_grouping() {
        use HostSelCategory::*;
        assert_eq!(host_selection_category(HostSelKey::NavUp), Navigate);
        assert_eq!(host_selection_category(HostSelKey::NavDown), Navigate);
        assert_eq!(host_selection_category(HostSelKey::CommitSelected), Commit);
        assert_eq!(host_selection_category(HostSelKey::CommitRaw), CommitRaw);
        assert_eq!(host_selection_category(HostSelKey::CommitIndex3), IndexPick);
        assert_eq!(host_selection_category(HostSelKey::CommitIndex0), IndexPick);
        assert_eq!(host_selection_category(HostSelKey::None), None);
    }

    #[test]
    fn is_commit_covers_all_commit_variants() {
        for sel in [
            HostSelKey::CommitSelected,
            HostSelKey::CommitRaw,
            HostSelKey::CommitIndex0,
            HostSelKey::CommitIndex1,
            HostSelKey::CommitIndex9,
        ] {
            assert!(host_selection_is_commit(sel), "{sel:?} should be commit");
        }
        assert!(!host_selection_is_commit(HostSelKey::NavUp));
        assert!(!host_selection_is_commit(HostSelKey::NavDown));
        assert!(!host_selection_is_commit(HostSelKey::None));
    }

    #[test]
    fn resolve_navigation_clamps_at_edges() {
        // Up at index 0 stays at 0.
        assert_eq!(host_selection_resolve(HostSelKey::NavUp, 0, 5), Some(0));
        // Up in the middle decrements.
        assert_eq!(host_selection_resolve(HostSelKey::NavUp, 3, 5), Some(2));
        // Down at the last index stays at last.
        assert_eq!(host_selection_resolve(HostSelKey::NavDown, 4, 5), Some(4));
        // Down in the middle increments.
        assert_eq!(host_selection_resolve(HostSelKey::NavDown, 2, 5), Some(3));
    }

    #[test]
    fn resolve_commit_selected_returns_current() {
        assert_eq!(
            host_selection_resolve(HostSelKey::CommitSelected, 2, 5),
            Some(2)
        );
        assert_eq!(
            host_selection_resolve(HostSelKey::CommitSelected, 0, 5),
            Some(0)
        );
    }

    #[test]
    fn resolve_index_picks_filter_when_out_of_range() {
        // Candidate count = 3, picking index 5 → None.
        assert_eq!(host_selection_resolve(HostSelKey::CommitIndex5, 0, 3), None);
        // Picking index 2 within count=3 → Some(2).
        assert_eq!(
            host_selection_resolve(HostSelKey::CommitIndex3, 0, 3),
            Some(2)
        );
        // Picking index 0 (key '0', maps to position 9) → out of range.
        assert_eq!(host_selection_resolve(HostSelKey::CommitIndex0, 0, 5), None);
        // With 10+ candidates, index 0 key resolves to position 9.
        assert_eq!(
            host_selection_resolve(HostSelKey::CommitIndex0, 0, 10),
            Some(9)
        );
    }

    #[test]
    fn resolve_returns_none_for_zero_candidates_or_no_key() {
        assert_eq!(host_selection_resolve(HostSelKey::NavUp, 0, 0), None);
        assert_eq!(host_selection_resolve(HostSelKey::None, 0, 5), None);
        // CommitRaw is not an index pick.
        assert_eq!(host_selection_resolve(HostSelKey::CommitRaw, 2, 5), None);
    }

    #[test]
    fn should_consume_key_with_no_candidates_returns_false() {
        // No candidates → never consume.
        assert!(!should_consume_key(
            0,
            HostSelectionFlags::empty(),
            XKB_KEY_UP
        ));
        assert!(!should_consume_key(
            0,
            HostSelectionFlags::all(),
            XKB_KEY_UP
        ));
    }

    #[test]
    fn should_consume_key_requires_explicit_opt_in() {
        // With candidates but NO declared flags, the host stays out of
        // the way — engines that have not opted into host-managed
        // selection continue to receive every key (no behaviour change
        // from before this module was wired).
        assert!(!should_consume_key(
            3,
            HostSelectionFlags::empty(),
            XKB_KEY_UP
        ));
        assert!(!should_consume_key(
            3,
            HostSelectionFlags::empty(),
            XKB_KEY_DOWN
        ));
        assert!(!should_consume_key(
            3,
            HostSelectionFlags::empty(),
            XKB_KEY_SPACE
        ));
        assert!(!should_consume_key(
            3,
            HostSelectionFlags::empty(),
            XKB_KEY_RETURN
        ));
        assert!(!should_consume_key(
            3,
            HostSelectionFlags::empty(),
            XKB_KEY_1
        ));
    }

    #[test]
    fn should_consume_key_respects_declared_flags() {
        // Engine declares NAVIGATE + INDEX_PICK but not COMMIT.
        let flags = HostSelectionFlags::NAVIGATE | HostSelectionFlags::INDEX_PICK;
        assert!(should_consume_key(5, flags, XKB_KEY_UP));
        assert!(should_consume_key(5, flags, XKB_KEY_2));
        // Space is not in the declared set → not consumed.
        assert!(!should_consume_key(5, flags, XKB_KEY_SPACE));
        // Enter not declared either.
        assert!(!should_consume_key(5, flags, XKB_KEY_RETURN));
        // Arrows also fall through when NAVIGATE is not declared, even
        // with other flags set.
        let non_nav = HostSelectionFlags::COMMIT | HostSelectionFlags::INDEX_PICK;
        assert!(!should_consume_key(5, non_nav, XKB_KEY_UP));
        assert!(!should_consume_key(5, non_nav, XKB_KEY_DOWN));
    }

    #[test]
    fn should_consume_key_ignores_irrelevant_keysyms() {
        // Even with all flags set, a non-selection keysym is not consumed.
        assert!(!should_consume_key(5, HostSelectionFlags::all(), 0xffbe));
        // 'a' key — not a selection key.
        assert!(!should_consume_key(5, HostSelectionFlags::all(), 0x0061));
    }

    #[test]
    fn navigation_keysym_check() {
        assert!(is_navigation_keysym(XKB_KEY_UP));
        assert!(is_navigation_keysym(XKB_KEY_DOWN));
        assert!(is_navigation_keysym(XKB_KEY_LEFT));
        assert!(is_navigation_keysym(XKB_KEY_RIGHT));
        assert!(!is_navigation_keysym(XKB_KEY_SPACE));
        assert!(!is_navigation_keysym(XKB_KEY_RETURN));
    }

    #[test]
    fn classify_returns_none_when_engine_did_not_opt_in() {
        // Empty flags → host stays out of the way.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_UP, 5, 0, HostSelectionFlags::empty()),
            None
        );
        assert_eq!(
            classify_host_selection(true, XKB_KEY_DOWN, 5, 0, HostSelectionFlags::empty()),
            None
        );
    }

    #[test]
    fn classify_returns_none_when_no_candidates() {
        // No candidates to navigate → fall through to engine.
        assert_eq!(
            classify_host_selection(
                true,
                XKB_KEY_UP,
                0,
                0,
                HostSelectionFlags::NAVIGATE
            ),
            None
        );
    }

    #[test]
    fn classify_navigate_press_moves_highlight() {
        // Down at index 0 with NAVIGATE flag → Navigate(1).
        assert_eq!(
            classify_host_selection(
                true,
                XKB_KEY_DOWN,
                5,
                0,
                HostSelectionFlags::NAVIGATE
            ),
            Some(HostSelectionAction::Navigate(1))
        );
        // Up at index 3 → Navigate(2).
        assert_eq!(
            classify_host_selection(true, XKB_KEY_UP, 5, 3, HostSelectionFlags::NAVIGATE),
            Some(HostSelectionAction::Navigate(2))
        );
        // Up at index 0 clamps to 0.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_UP, 5, 0, HostSelectionFlags::NAVIGATE),
            Some(HostSelectionAction::Navigate(0))
        );
    }

    #[test]
    fn classify_release_under_host_managed_is_swallowed() {
        // Press Down → Navigate. Release Down (state=0) under the same
        // flag → Swallow, so the engine doesn't see an unpaired release.
        assert_eq!(
            classify_host_selection(false, XKB_KEY_DOWN, 5, 0, HostSelectionFlags::NAVIGATE),
            Some(HostSelectionAction::Swallow)
        );
    }

    #[test]
    fn classify_commit_index_press_resolves_target() {
        // '3' key with INDEX_PICK + 5 candidates → Commit(2).
        assert_eq!(
            classify_host_selection(true, XKB_KEY_3, 5, 0, HostSelectionFlags::INDEX_PICK),
            Some(HostSelectionAction::Commit(2))
        );
        // Space with COMMIT flag → Commit(currently selected).
        assert_eq!(
            classify_host_selection(true, XKB_KEY_SPACE, 5, 2, HostSelectionFlags::COMMIT),
            Some(HostSelectionAction::Commit(2))
        );
    }

    #[test]
    fn classify_commit_raw_falls_through_to_engine() {
        // Enter with COMMIT_RAW → engine must handle the raw preedit
        // commit (the host doesn't know how). classify returns None so
        // try_host_selection falls back to process_key.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_RETURN, 5, 0, HostSelectionFlags::COMMIT_RAW),
            None
        );
    }

    #[test]
    fn classify_index_pick_out_of_range_falls_through() {
        // '5' key with INDEX_PICK + only 3 candidates → no resolution,
        // fall through to engine.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_5, 3, 0, HostSelectionFlags::INDEX_PICK),
            None
        );
    }

    #[test]
    fn classify_flag_not_declined_for_category() {
        // Down arrow with COMMIT flag (not NAVIGATE) → host did not
        // declare navigation, fall through.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_DOWN, 5, 0, HostSelectionFlags::COMMIT),
            None
        );
        // Space with NAVIGATE flag (not COMMIT) → fall through.
        assert_eq!(
            classify_host_selection(true, XKB_KEY_SPACE, 5, 0, HostSelectionFlags::NAVIGATE),
            None
        );
    }
}
