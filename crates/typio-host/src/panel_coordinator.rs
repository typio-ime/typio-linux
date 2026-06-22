//! Frontend-side panel ownership, anchor probing, and positioned-popup readiness.
//!
//! Rust port of `src/wayland/panel_coordinator.c`. This module holds the
//! decision state for the single popup surface: which UI owner (candidate
//! panel, indicator, voice status) owns it, whether the compositor has
//! provided a usable cursor anchor, and when to give up waiting for one.
//!
//! Rendering itself stays in [`crate::panel::FluxPanel`]; this coordinator
//! only decides *when* rendering should happen and how to handle the anchor
//! probe / caret fallback.

use std::time::Instant;

/// Default anchor-probe enable flag. Mirrors `TYPIO_ANCHOR_PROBE_DEFAULT_ENABLED`.
const DEFAULT_ANCHOR_PROBE_ENABLED: bool = true;
/// Default anchor-probe timeout. Mirrors `TYPIO_ANCHOR_PROBE_DEFAULT_TIMEOUT_MS`.
const DEFAULT_ANCHOR_TIMEOUT_MS: u64 = 150;
/// Minimum clamp for the configured timeout.
const MIN_ANCHOR_TIMEOUT_MS: u64 = 50;
/// Maximum clamp for the configured timeout.
const MAX_ANCHOR_TIMEOUT_MS: u64 = 1000;

/// Which subsystem owns the positioned popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiOwner {
    /// No owner.
    #[default]
    None = 0,
    /// Candidate panel.
    Candidate = 1,
    /// Status indicator (e.g. language switch feedback).
    Indicator = 2,
    /// Voice-input status overlay.
    Voice = 3,
}

/// Panel-coordinator configuration. Production values are read from the
/// libtypio config; tests can use defaults.
#[derive(Debug, Clone, Copy)]
pub struct PanelCoordinatorConfig {
    pub anchor_probe_enabled: bool,
    pub anchor_timeout_ms: u64,
}

impl Default for PanelCoordinatorConfig {
    fn default() -> Self {
        Self {
            anchor_probe_enabled: DEFAULT_ANCHOR_PROBE_ENABLED,
            anchor_timeout_ms: DEFAULT_ANCHOR_TIMEOUT_MS,
        }
    }
}

impl PanelCoordinatorConfig {
    /// Build from raw libtypio config values.
    pub fn from_values(anchor_probe_enabled: bool, anchor_timeout_ms: i64) -> Self {
        let mut timeout = anchor_timeout_ms.clamp(0, i64::MAX) as u64;
        if timeout < MIN_ANCHOR_TIMEOUT_MS {
            timeout = DEFAULT_ANCHOR_TIMEOUT_MS;
        }
        if timeout > MAX_ANCHOR_TIMEOUT_MS {
            timeout = MAX_ANCHOR_TIMEOUT_MS;
        }
        Self {
            anchor_probe_enabled,
            anchor_timeout_ms: timeout,
        }
    }
}

/// Outcome of asking the coordinator whether to draw a positioned popup now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushDecision {
    /// The popup is not ready yet; keep it pending.
    Pending,
    /// The popup should be shown (anchor ready or caret fallback).
    Show,
    /// The anchor timed out and there is no usable fallback; discard the popup.
    Cancel,
}

/// Decision state for the single positioned popup surface.
#[derive(Debug, Clone)]
pub struct PanelCoordinator {
    config: PanelCoordinatorConfig,
    position_anchor_generation: u64,
    position_anchor_ready_generation: u64,
    position_anchor_probe_generation: u64,
    position_anchor_has_caret: bool,
    positioned_ui_pending: bool,
    positioned_ui_pending_owner: UiOwner,
    positioned_ui_pending_since: Instant,
    positioned_ui_pending_label: String,
    ui_owner: UiOwner,
}

impl Default for PanelCoordinator {
    fn default() -> Self {
        Self {
            config: PanelCoordinatorConfig::default(),
            position_anchor_generation: 0,
            position_anchor_ready_generation: 0,
            position_anchor_probe_generation: 0,
            position_anchor_has_caret: false,
            positioned_ui_pending: false,
            positioned_ui_pending_owner: UiOwner::None,
            positioned_ui_pending_since: Instant::now(),
            positioned_ui_pending_label: String::new(),
            ui_owner: UiOwner::None,
        }
    }
}

impl PanelCoordinator {
    /// Create a coordinator with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a coordinator with explicit configuration.
    pub fn with_config(config: PanelCoordinatorConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Current configuration.
    pub fn config(&self) -> &PanelCoordinatorConfig {
        &self.config
    }

    /// True iff the compositor has provided a usable anchor for the current
    /// focus generation.
    pub fn anchor_ready(&self) -> bool {
        self.position_anchor_generation > 0
            && self.position_anchor_ready_generation == self.position_anchor_generation
    }

    /// Start a new anchor generation. Called on focus_in / hard boundaries.
    pub fn reset_anchor(&mut self) {
        self.position_anchor_generation = self.position_anchor_generation.wrapping_add(1);
        if self.position_anchor_generation == 0 {
            self.position_anchor_generation = 1;
        }
        self.position_anchor_ready_generation = 0;
        self.position_anchor_probe_generation = 0;
        self.position_anchor_has_caret = false;
    }

    /// Record that the compositor sent a text-input rectangle for this popup.
    pub fn note_caret_rect(&mut self) {
        self.position_anchor_has_caret = true;
    }

    /// Clear the caret-rect flag (e.g. on focus_out).
    pub fn clear_caret_rect(&mut self) {
        self.position_anchor_has_caret = false;
    }

    /// Mark the current anchor generation as ready. Idempotent.
    pub fn mark_anchor_ready(&mut self) {
        if self.position_anchor_generation == 0 {
            return;
        }
        if self.anchor_ready() {
            return;
        }
        self.position_anchor_ready_generation = self.position_anchor_generation;
    }

    /// True iff the caller should send an anchor probe for the current
    /// generation.
    pub fn should_probe_anchor(&self) -> bool {
        self.config.anchor_probe_enabled
            && self.position_anchor_generation > 0
            && self.position_anchor_probe_generation != self.position_anchor_generation
    }

    /// Record that an anchor probe has been sent for the current generation.
    pub fn record_probe_sent(&mut self) {
        self.position_anchor_probe_generation = self.position_anchor_generation;
    }

    /// Queue a positioned UI update for later, once the anchor is ready.
    /// Returns `true` if it was queued.
    pub fn queue_positioned_ui(&mut self, owner: UiOwner, label: &str) -> bool {
        if label.is_empty() || owner == UiOwner::None {
            return false;
        }
        if !self.positioned_ui_pending || self.positioned_ui_pending_owner != owner {
            self.positioned_ui_pending_since = Instant::now();
        }
        self.positioned_ui_pending_owner = owner;
        self.positioned_ui_pending_label = label.to_string();
        self.positioned_ui_pending = true;
        true
    }

    /// Cancel any pending positioned UI.
    pub fn cancel_pending(&mut self) {
        self.positioned_ui_pending = false;
        self.positioned_ui_pending_owner = UiOwner::None;
        self.positioned_ui_pending_label.clear();
    }

    /// True iff there is a positioned UI waiting for an anchor.
    pub fn has_pending(&self) -> bool {
        self.positioned_ui_pending
    }

    /// Owner of the pending positioned UI, if any.
    pub fn pending_owner(&self) -> UiOwner {
        if self.positioned_ui_pending {
            self.positioned_ui_pending_owner
        } else {
            UiOwner::None
        }
    }

    /// Label of the pending positioned UI, if any.
    pub fn pending_label(&self) -> &str {
        if self.positioned_ui_pending {
            &self.positioned_ui_pending_label
        } else {
            ""
        }
    }

    /// Current visible popup owner.
    pub fn visible_owner(&self) -> UiOwner {
        self.ui_owner
    }

    /// Decide whether a positioned popup for `owner` may be shown now.
    ///
    /// If the anchor is not ready yet, the request is queued and
    /// [`FlushDecision::Pending`] is returned. If the anchor is ready, any
    /// pending state is cleared and [`FlushDecision::Show`] is returned.
    pub fn decide_positioned_flush(&mut self, owner: UiOwner, label: &str) -> FlushDecision {
        if self.anchor_ready() {
            self.cancel_pending();
            self.ui_owner = owner;
            return FlushDecision::Show;
        }

        // Already pending for someone else? The new request wins; reset the timer.
        if self.positioned_ui_pending && self.positioned_ui_pending_owner != owner {
            self.positioned_ui_pending_since = Instant::now();
        }
        self.queue_positioned_ui(owner, label);
        FlushDecision::Pending
    }

    /// Hide the popup owned by `owner`.
    pub fn hide(&mut self, owner: UiOwner) {
        if self.positioned_ui_pending && self.positioned_ui_pending_owner == owner {
            self.cancel_pending();
        }
        if self.ui_owner == owner {
            self.ui_owner = UiOwner::None;
        }
    }

    /// Claim the visible surface for `owner`, superseding the current owner.
    ///
    /// The candidate-panel path manages the surface directly — it does not
    /// go through the anchor-probe logic of `decide_*_flush` — so it needs
    /// a way to declare ownership. Claiming `Candidate` while the indicator
    /// is visible makes the indicator's auto-hide path see
    /// `visible_owner() != Indicator` and skip `panel.hide()`, so a late
    /// indicator timer can no longer kill an in-flight candidate list.
    pub fn claim(&mut self, owner: UiOwner) {
        if owner == UiOwner::None {
            return;
        }
        if self.positioned_ui_pending_owner != owner {
            self.cancel_pending();
        }
        self.ui_owner = owner;
    }

    /// Hide all popups and cancel any pending positioning.
    pub fn hide_all(&mut self) {
        self.cancel_pending();
        self.ui_owner = UiOwner::None;
    }

    /// Flush a pending positioned UI if the anchor is now ready. Returns the
    /// owner and label to show, or `None` if there is nothing to flush.
    pub fn flush_pending(&mut self) -> Option<(UiOwner, String)> {
        if !self.positioned_ui_pending || !self.anchor_ready() {
            return None;
        }
        let owner = self.positioned_ui_pending_owner;
        let label = self.positioned_ui_pending_label.clone();
        self.cancel_pending();
        self.ui_owner = owner;
        Some((owner, label))
    }

    /// Flush a pending positioned UI, applying the anchor timeout and caret
    /// fallback. Returns the owner and label to show, or `None` if the popup
    /// is still waiting or was cancelled.
    pub fn flush_pending_with_timeout(&mut self, now: Instant) -> Option<(UiOwner, String)> {
        if !self.positioned_ui_pending {
            return None;
        }
        if self.anchor_ready() {
            return self.flush_pending();
        }
        let elapsed_ms = now
            .saturating_duration_since(self.positioned_ui_pending_since)
            .as_millis() as u64;
        if elapsed_ms < self.config.anchor_timeout_ms {
            return None;
        }
        let caret_fallback = self.position_anchor_has_caret
            || self.positioned_ui_pending_owner == UiOwner::Indicator;
        if caret_fallback {
            self.mark_anchor_ready();
            return self.flush_pending();
        }
        self.cancel_pending();
        None
    }

    /// Remaining milliseconds until the pending anchor probe deadline, or
    /// `None` if there is no pending popup or the anchor is already ready.
    pub fn anchor_deadline_remaining_ms(&self, now: Instant) -> Option<u64> {
        if !self.positioned_ui_pending || self.anchor_ready() {
            return None;
        }
        let elapsed = now
            .saturating_duration_since(self.positioned_ui_pending_since)
            .as_millis() as u64;
        let timeout = self.config.anchor_timeout_ms;
        if elapsed >= timeout {
            Some(0)
        } else {
            Some(timeout - elapsed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_is_not_ready_initially() {
        let coord = PanelCoordinator::new();
        assert!(!coord.anchor_ready());
    }

    #[test]
    fn reset_anchor_creates_new_generation() {
        let mut coord = PanelCoordinator::new();
        coord.reset_anchor();
        assert!(!coord.anchor_ready());
        coord.mark_anchor_ready();
        assert!(coord.anchor_ready());
        coord.reset_anchor();
        assert!(!coord.anchor_ready());
    }

    #[test]
    fn note_caret_rect_enables_caret_fallback() {
        let mut coord = PanelCoordinator::new();
        coord.reset_anchor();
        coord.note_caret_rect();
        // Not ready until marked.
        assert!(!coord.anchor_ready());
        // Caret rect noted → caret fallback path is armed.
        assert!(coord.position_anchor_has_caret);
    }

    #[test]
    fn queue_and_flush_pending() {
        let mut coord = PanelCoordinator::new();
        coord.reset_anchor();
        assert!(coord.queue_positioned_ui(UiOwner::Indicator, "EN"));
        assert!(coord.has_pending());
        assert_eq!(coord.pending_owner(), UiOwner::Indicator);
        assert_eq!(coord.pending_label(), "EN");
        // Not ready yet → flush_pending returns None.
        assert!(coord.flush_pending().is_none());
        coord.mark_anchor_ready();
        assert_eq!(
            coord.flush_pending(),
            Some((UiOwner::Indicator, "EN".to_string()))
        );
        assert!(!coord.has_pending());
    }

    #[test]
    fn hide_clears_pending_for_owner() {
        let mut coord = PanelCoordinator::new();
        coord.reset_anchor();
        coord.queue_positioned_ui(UiOwner::Candidate, "candidate");
        coord.hide(UiOwner::Candidate);
        assert!(!coord.has_pending());
    }

    #[test]
    fn config_clamps_timeout() {
        let cfg = PanelCoordinatorConfig::from_values(true, 10);
        assert_eq!(cfg.anchor_timeout_ms, DEFAULT_ANCHOR_TIMEOUT_MS);
        let cfg = PanelCoordinatorConfig::from_values(true, 5000);
        assert_eq!(cfg.anchor_timeout_ms, MAX_ANCHOR_TIMEOUT_MS);
        let cfg = PanelCoordinatorConfig::from_values(true, 200);
        assert_eq!(cfg.anchor_timeout_ms, 200);
    }
}
