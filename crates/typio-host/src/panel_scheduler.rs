//! Pure scheduling policy for the candidate panel's dirty/retry tick.
//!
//! Port of `src/wayland/panel_scheduler.c`. This is the pure decision core of
//! the panel redraw scheduler — given the current schedule state and a few
//! live flags, it decides whether the panel should flush this tick and what
//! poll timeout the event loop should use. No I/O, no Wayland handles.

/// Per-tick schedule state of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelScheduleState {
    /// Nothing pending.
    #[default]
    Idle = 0,
    /// A redraw is queued for the next flush opportunity.
    Dirty = 1,
    /// The previous flush asked for a retry; keep polling at the retry cadence.
    Retry = 2,
}

/// Outcome of a panel update attempt. Mirrors the relevant slice of the C
/// `TypioPanelUpdateResult` — the scheduler only distinguishes "retry" from
/// everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelUpdateResult {
    /// The update completed (or failed permanently); either way, no retry.
    Done,
    /// The update needs to be attempted again next tick.
    Retry,
}

/// Retry poll cadence (ms). Mirrors `TYPIO_WL_PANEL_RETRY_POLL_MS`.
pub const RETRY_POLL_MS: i32 = 16;

/// `typio_wl_panel_scheduler_mark_dirty`.
pub fn mark_dirty(_current: PanelScheduleState) -> PanelScheduleState {
    PanelScheduleState::Dirty
}

/// `typio_wl_panel_scheduler_complete`.
pub fn complete(result: PanelUpdateResult) -> PanelScheduleState {
    match result {
        PanelUpdateResult::Retry => PanelScheduleState::Retry,
        PanelUpdateResult::Done => PanelScheduleState::Idle,
    }
}

/// `typio_wl_panel_scheduler_cancel`.
pub fn cancel() -> PanelScheduleState {
    PanelScheduleState::Idle
}

/// `typio_wl_panel_scheduler_should_flush`.
pub fn should_flush(
    state: PanelScheduleState,
    has_session: bool,
    has_context: bool,
    context_focused: bool,
) -> bool {
    state != PanelScheduleState::Idle && has_session && has_context && context_focused
}

/// `typio_wl_panel_scheduler_poll_timeout_ms`. A negative `current_timeout_ms`
/// means "block indefinitely"; retry still shortens it to the retry cadence.
pub fn poll_timeout_ms(state: PanelScheduleState, flushable: bool, current_timeout_ms: i32) -> i32 {
    if state != PanelScheduleState::Retry
        || !flushable
        || (0..=RETRY_POLL_MS).contains(&current_timeout_ms)
    {
        return current_timeout_ms;
    }
    RETRY_POLL_MS
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_dirty_overrides_retry_and_idle() {
        assert_eq!(
            mark_dirty(PanelScheduleState::Idle),
            PanelScheduleState::Dirty
        );
        assert_eq!(
            mark_dirty(PanelScheduleState::Retry),
            PanelScheduleState::Dirty
        );
        assert_eq!(
            mark_dirty(PanelScheduleState::Dirty),
            PanelScheduleState::Dirty
        );
    }

    #[test]
    fn complete_retry_yields_retry_state() {
        assert_eq!(
            complete(PanelUpdateResult::Retry),
            PanelScheduleState::Retry
        );
        assert_eq!(complete(PanelUpdateResult::Done), PanelScheduleState::Idle);
    }

    #[test]
    fn cancel_returns_idle() {
        assert_eq!(cancel(), PanelScheduleState::Idle);
    }

    #[test]
    fn should_flush_requires_all_conditions() {
        // Idle state never flushes even with everything present.
        assert!(!should_flush(PanelScheduleState::Idle, true, true, true));
        // Dirty + everything present → flush.
        assert!(should_flush(PanelScheduleState::Dirty, true, true, true));
        // Missing any flag suppresses the flush.
        assert!(!should_flush(PanelScheduleState::Dirty, false, true, true));
        assert!(!should_flush(PanelScheduleState::Dirty, true, false, true));
        assert!(!should_flush(PanelScheduleState::Dirty, true, true, false));
        // Retry also flushes when all flags are present.
        assert!(should_flush(PanelScheduleState::Retry, true, true, true));
    }

    #[test]
    fn poll_timeout_clamps_to_retry_cadence_only_when_retrying() {
        // Non-retry state: timeout untouched, including infinite.
        assert_eq!(poll_timeout_ms(PanelScheduleState::Idle, true, -1), -1);
        assert_eq!(poll_timeout_ms(PanelScheduleState::Dirty, true, 100), 100);
        // Retry + flushable + infinite → retry cadence.
        assert_eq!(
            poll_timeout_ms(PanelScheduleState::Retry, true, -1),
            RETRY_POLL_MS
        );
        // Retry + flushable + long finite → clamped to retry cadence.
        assert_eq!(
            poll_timeout_ms(PanelScheduleState::Retry, true, 200),
            RETRY_POLL_MS
        );
        // Retry + flushable + already-short finite → untouched.
        assert_eq!(poll_timeout_ms(PanelScheduleState::Retry, true, 8), 8);
        assert_eq!(
            poll_timeout_ms(PanelScheduleState::Retry, true, RETRY_POLL_MS),
            RETRY_POLL_MS
        );
        // Retry but not flushable → untouched (don't wake just to skip).
        assert_eq!(poll_timeout_ms(PanelScheduleState::Retry, false, 100), 100);
    }
}
