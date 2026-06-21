//! Pure decision helpers for text-UI (inline preedit + candidate panel)
//! synchronization and positioned-popup readiness.
//!
//! Port of `src/ui/state.c`. No I/O; the effectful layer consults these to
//! decide whether to re-send the compositor preedit and when to show or give
//! up on a cursor-anchored popup.

/// What the text-UI sync step must do this tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextUiPlan {
    /// Preedit text or cursor changed: re-send the compositor preedit *and*
    /// repaint the candidate panel.
    #[default]
    SyncPreeditAndPanel,
    /// Preedit unchanged: only the panel needs a repaint.
    SyncPanelOnly,
}

/// Decide whether the inline preedit needs re-sending, or only the panel
/// needs repainting. `None` text is treated as the empty string.
pub fn text_ui_plan_update(
    last_text: Option<&str>,
    last_cursor: i32,
    next_text: Option<&str>,
    next_cursor: i32,
) -> TextUiPlan {
    let last = last_text.unwrap_or("");
    let next = next_text.unwrap_or("");

    if last_cursor != next_cursor || last != next {
        TextUiPlan::SyncPreeditAndPanel
    } else {
        TextUiPlan::SyncPanelOnly
    }
}

/// Tracking state for the last preedit the host sent to the compositor.
/// Resetting clears the text and parks the cursor at the sentinel `-1`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreeditTracking {
    pub last_text: Option<String>,
    pub last_cursor: i32,
}

impl PreeditTracking {
    /// Fresh tracking with the cursor at the `-1` sentinel.
    pub fn new() -> Self {
        Self {
            last_text: None,
            last_cursor: -1,
        }
    }

    /// Clear tracked preedit state (text → `None`, cursor → `-1`).
    pub fn reset(&mut self) {
        self.last_text = None;
        self.last_cursor = -1;
    }
}

/// What to do with a cursor-anchored (positioned) popup this tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PositionedUiPlan {
    /// Anchor not ready yet and not timed out: keep waiting.
    #[default]
    Wait,
    /// Anchor is ready: show the popup.
    Show,
    /// Anchor never arrived within the timeout: give up.
    Cancel,
}

/// Decide whether to show, cancel, or keep waiting for a positioned popup.
///
/// A clock regression (`now_ms < since_ms`) is treated as no elapsed time, so
/// the popup keeps waiting rather than cancelling spuriously.
pub fn positioned_ui_plan(
    pending: bool,
    anchor_ready: bool,
    since_ms: u64,
    now_ms: u64,
    timeout_ms: u64,
) -> PositionedUiPlan {
    if !pending {
        return PositionedUiPlan::Wait;
    }
    if anchor_ready {
        return PositionedUiPlan::Show;
    }

    let elapsed_ms = if since_ms > 0 && now_ms >= since_ms {
        now_ms - since_ms
    } else {
        0
    };

    if since_ms > 0 && elapsed_ms >= timeout_ms {
        PositionedUiPlan::Cancel
    } else {
        PositionedUiPlan::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncs_panel_only_when_preedit_and_cursor_match() {
        assert_eq!(
            text_ui_plan_update(Some("ni"), 2, Some("ni"), 2),
            TextUiPlan::SyncPanelOnly
        );
    }

    #[test]
    fn syncs_when_preedit_text_changes() {
        assert_eq!(
            text_ui_plan_update(Some("ni"), 2, Some("nih"), 3),
            TextUiPlan::SyncPreeditAndPanel
        );
    }

    #[test]
    fn syncs_when_cursor_changes() {
        assert_eq!(
            text_ui_plan_update(Some("ni"), 1, Some("ni"), 2),
            TextUiPlan::SyncPreeditAndPanel
        );
    }

    #[test]
    fn treats_none_preedit_as_empty_string() {
        assert_eq!(
            text_ui_plan_update(None, -1, None, -1),
            TextUiPlan::SyncPanelOnly
        );
        assert_eq!(
            text_ui_plan_update(None, -1, Some(""), -1),
            TextUiPlan::SyncPanelOnly
        );
        assert_eq!(
            text_ui_plan_update(Some(""), -1, Some("ni"), 2),
            TextUiPlan::SyncPreeditAndPanel
        );
    }

    #[test]
    fn reset_tracking_clears_preedit_state() {
        let mut tracking = PreeditTracking {
            last_text: Some("ni".to_string()),
            last_cursor: 2,
        };
        tracking.reset();
        assert_eq!(tracking.last_text, None);
        assert_eq!(tracking.last_cursor, -1);
    }

    #[test]
    fn new_tracking_parks_cursor_at_sentinel() {
        let tracking = PreeditTracking::new();
        assert_eq!(tracking.last_text, None);
        assert_eq!(tracking.last_cursor, -1);
    }

    #[test]
    fn positioned_ui_waits_for_ready_anchor() {
        assert_eq!(
            positioned_ui_plan(false, false, 1000, 1200, 100),
            PositionedUiPlan::Wait
        );
        assert_eq!(
            positioned_ui_plan(true, false, 1000, 1050, 100),
            PositionedUiPlan::Wait
        );
        assert_eq!(
            positioned_ui_plan(true, true, 1000, 1050, 100),
            PositionedUiPlan::Show
        );
    }

    #[test]
    fn positioned_ui_cancels_when_anchor_times_out() {
        assert_eq!(
            positioned_ui_plan(true, false, 1000, 1100, 100),
            PositionedUiPlan::Cancel
        );
        assert_eq!(
            positioned_ui_plan(true, false, 1000, 1200, 100),
            PositionedUiPlan::Cancel
        );
    }

    #[test]
    fn positioned_ui_handles_clock_regression_as_wait() {
        assert_eq!(
            positioned_ui_plan(true, false, 1000, 900, 100),
            PositionedUiPlan::Wait
        );
    }
}
