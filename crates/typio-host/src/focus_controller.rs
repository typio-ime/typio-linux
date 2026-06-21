//! Desired-vs-actual focus lifecycle controller.
//!
//! Port of `src/engine/focus_controller.c` — the pure half of the session
//! controller. There is no stored lifecycle phase. The only persisted things
//! are raw input facts and live resource handles. Every event-loop tick runs
//! one step:
//!
//! ```text
//! facts   = record(inputs)
//! desired = reduce(facts, prev)   pure
//! actual  = observe(resources)    live snapshot
//! effects = diff(desired, actual) pure, minimal, idempotent
//! apply(effects)                  effectful
//! ```
//!
//! This module owns the pure half: facts, desired, actual, effects, `reduce`,
//! and `diff`. The effectful half (observe + apply) reads and mutates the
//! Wayland frontend and lives elsewhere.
//!
//! See `docs/explanation/focus-controller.md` and ADR-0003.

// ── Input facts ──────────────────────────────────────────────────────────

/// Raw input facts recorded from Wayland events and environment detectors.
/// Each fact has exactly one source. Facts are recorded, never interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputFacts {
    /// An activate event arrived in the current dispatch batch.
    pub im_activate_seen: bool,
    /// A deactivate event arrived in the current dispatch batch.
    pub im_deactivate_seen: bool,
    /// The current `done()` batch included an activate (distinguishes
    /// reactivation from a plain text-state update).
    pub im_done_had_activate: bool,
    /// The current `done()` batch included a deactivate. Mirrors
    /// `im_done_had_activate`: a deactivate is committed by the same `done()`
    /// that clears the per-event `im_deactivate_seen`, so without this
    /// batch-surviving flag a plain focus-out (click away to a non-editable)
    /// is lost before `reduce` runs and the grab never soft-pauses.
    pub im_done_had_deactivate: bool,
    /// Serial from the most recent `done()` event.
    pub im_done_serial: u32,
    /// Wayland connection is alive (no POLLHUP observed).
    pub connection_alive: bool,
    /// The system-resume detector fired (logind PrepareForSleep or
    /// boottime-gap heuristic).
    pub suspend_gap_detected: bool,
    /// A keyboard engine is registered and ready to process input. When
    /// false, the controller still focuses the input context but skips the
    /// keyboard grab (no consumer for the key stream).
    pub engine_present: bool,
}

// ── Desired state ────────────────────────────────────────────────────────

/// Whether the session wants a keyboard grab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrabWant {
    /// No grab wanted.
    #[default]
    None,
    /// Normal deactivate: release keys, reset tracking, but retain the grab
    /// object so the next activation can reuse it (soft pause).
    SoftPause,
    /// Focus is established: grab must exist and be ready for key routing.
    Yes,
}

impl GrabWant {
    /// Pure name helper, for tracing.
    pub fn name(self) -> &'static str {
        match self {
            GrabWant::None => "NONE",
            GrabWant::SoftPause => "SOFT_PAUSE",
            GrabWant::Yes => "YES",
        }
    }
}

/// Desired resource configuration derived from facts.
///
/// `focus_in` / `focus_out` / `reactivate` are edge-triggered: they are true
/// only on the tick when the relevant transition crosses a boundary. This
/// prevents repeated calls while the state is stable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DesiredState {
    pub grab: GrabWant,
    /// YES edge: was not YES before, is YES now. Triggers engine `focus_in`.
    pub focus_in: bool,
    /// YES→non-YES edge: was YES, is not YES now. Triggers engine `focus_out`.
    pub focus_out: bool,
    /// YES→YES with an `activate_seen` in the same done batch: re-anchor the
    /// panel to the new caret. The grab, engine state, and in-flight
    /// composition are preserved.
    pub reactivate: bool,
}

// ── Actual state ─────────────────────────────────────────────────────────

/// Unified readiness of the grab + virtual-keyboard-keymap resource. This is
/// a single resource with one state, not a phase plus a separate vk state
/// machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrabResourceState {
    /// No grab.
    #[default]
    Absent,
    /// Grab exists but the current epoch has not completed the keymap handoff
    /// to the virtual keyboard. Modifier updates may proceed; key presses may
    /// not.
    NeedsKeymap,
    /// Grab exists and the compositor keymap has been forwarded to vk in the
    /// current epoch. Keys may be routed to the engine.
    Ready,
    /// The keymap path is unhealthy (timeout, repeated cancellation, fd dup
    /// failure). The grab must be torn down and rebuilt.
    Broken,
}

impl GrabResourceState {
    /// Pure name helper, for tracing.
    pub fn name(self) -> &'static str {
        match self {
            GrabResourceState::Absent => "ABSENT",
            GrabResourceState::NeedsKeymap => "NEEDS_KEYMAP",
            GrabResourceState::Ready => "READY",
            GrabResourceState::Broken => "BROKEN",
        }
    }
}

/// Read-only snapshot of live resources. Not a second source of truth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActualState {
    pub connection_alive: bool,
    pub ic_focused: bool,
    pub grab: GrabResourceState,
}

// ── Effects ──────────────────────────────────────────────────────────────

/// Minimal, idempotent effect set produced by [`diff`].
///
/// Applying the same effect set twice is a no-op (or harmless). This is what
/// makes recovery free: suspend, reconnect, and reconcile-repair all funnel
/// into diff → apply rather than bespoke scrub paths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EffectSet {
    pub destroy_grab: bool,
    pub create_grab: bool,
    pub scrub_generation: bool,
    pub send_focus_in: bool,
    pub send_focus_out: bool,
    pub discard_composition: bool,
    pub clear_preedit: bool,
    pub commit: bool,
    pub reactivate: bool,
}

// ── Pure functions ───────────────────────────────────────────────────────

/// Derive desired state from input facts.
///
/// Rules (first match wins):
///   - `!connection_alive || suspend_gap_detected` → NONE
///   - `im_activate_seen || im_done_had_activate`   → YES (if engine present, else NONE)
///   - `im_deactivate_seen || im_done_had_deactivate` → SOFT_PAUSE
///   - otherwise → preserve `prev.grab`
///
/// Activate is checked before deactivate so a focus move that batches both
/// (deactivate-old + activate-new + done) stays YES / reactivates rather than
/// soft-pausing.
pub fn reduce(facts: &InputFacts, prev: &DesiredState) -> DesiredState {
    let mut d = DesiredState::default();

    // Grab want: hard boundary first, then activation, then deactivation.
    if !facts.connection_alive || facts.suspend_gap_detected {
        d.grab = GrabWant::None;
    } else if facts.im_activate_seen || facts.im_done_had_activate {
        // Skip the grab entirely if no engine is registered: focusing the
        // input context is still meaningful (state in sync), but a grab with
        // no consumer would be wasted work.
        d.grab = if facts.engine_present {
            GrabWant::Yes
        } else {
            GrabWant::None
        };
    } else if facts.im_deactivate_seen || facts.im_done_had_deactivate {
        d.grab = GrabWant::SoftPause;
    } else {
        d.grab = prev.grab;
    }

    // Edge-triggered focus events.
    d.focus_in = d.grab == GrabWant::Yes && prev.grab != GrabWant::Yes;
    d.focus_out = d.grab != GrabWant::Yes && prev.grab == GrabWant::Yes;

    // Reactivate: a fresh activate inside a done batch while we are stably YES
    // means the compositor moved us to a new caret without an intervening
    // deactivate. Preserve the grab (and the in-flight composition);
    // re-anchor the panel to the new caret.
    d.reactivate =
        d.grab == GrabWant::Yes && prev.grab == GrabWant::Yes && facts.im_done_had_activate;

    d
}

/// Compute the minimal effect set needed to converge actual onto desired.
///
/// Rules are evaluated independently; multiple can fire on one tick.
pub fn diff(desired: &DesiredState, actual: &ActualState) -> EffectSet {
    let mut e = EffectSet::default();

    // Hard teardown: we do not want the grab at all. Scrub the key generation
    // as well: any transition to NONE is a hard boundary that must fence
    // stale in-flight key state.
    if desired.grab == GrabWant::None && actual.grab != GrabResourceState::Absent {
        e.destroy_grab = true;
        e.scrub_generation = true;
        e.discard_composition = true;
        e.clear_preedit = true;
        e.commit = true;
    }

    // Creation: we need a grab but it is absent. Covers normal activation,
    // the soft-pause recovery case where the grab was silently dropped while
    // paused, and the "no engine, no grab" degenerate path.
    if (desired.grab == GrabWant::Yes || desired.grab == GrabWant::SoftPause)
        && actual.grab == GrabResourceState::Absent
    {
        e.create_grab = true;
        e.scrub_generation = true;
    }

    // Broken recovery: tear down and rebuild in the same tick.
    if desired.grab == GrabWant::Yes && actual.grab == GrabResourceState::Broken {
        e.destroy_grab = true;
        e.create_grab = true;
        e.scrub_generation = true;
    }

    // Focus edges.
    if desired.focus_in {
        e.send_focus_in = true;
    }
    if desired.focus_out {
        e.send_focus_out = true;
        // Leaving an active field abandons any in-flight composition. Discard
        // it engine-side and blank the compositor preedit so a half-typed
        // attempt cannot leak into the next field on its focus_in.
        e.discard_composition = true;
        e.clear_preedit = true;
        e.commit = true;
    }

    // Reactivate: re-anchor the panel to the new caret. The grab and the
    // engine state are preserved.
    if desired.reactivate {
        e.reactivate = true;
    }

    e
}

// ── Done-event classifier (pure helper, for tracing) ─────────────────────

/// Outcome of a `done` event, classified by the relevant focus axes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DoneAction {
    /// No focus change.
    #[default]
    Noop,
    /// Was inactive, now active.
    FirstActivate,
    /// Was active, now inactive.
    Deactivate,
    /// Active, then a real (re)activate.
    Reactivate,
}

impl DoneAction {
    /// Pure name helper, for tracing.
    pub fn name(self) -> &'static str {
        match self {
            DoneAction::Noop => "NOOP",
            DoneAction::FirstActivate => "FIRST_ACTIVATE",
            DoneAction::Deactivate => "DEACTIVATE",
            DoneAction::Reactivate => "REACTIVATE",
        }
    }
}

/// Pure classifier for a `done` event.
pub fn classify_done(was_active: bool, now_active: bool, activate_seen: bool) -> DoneAction {
    if now_active && !was_active {
        return DoneAction::FirstActivate;
    }
    if was_active && !now_active {
        return DoneAction::Deactivate;
    }
    // Still active. Only a fresh `activate` in this batch means a genuine
    // re-activation (a move to a new field); otherwise this `done` is just a
    // text-state update and must leave focus state untouched.
    if was_active && now_active && activate_seen {
        return DoneAction::Reactivate;
    }
    DoneAction::Noop
}

// ── Guard predicates (pure, on a snapshotted actual) ─────────────────────

/// Can the host safely route a key to the engine right now? True iff the
/// input context is focused AND the grab resource is READY.
pub fn can_route_keys(actual: &ActualState) -> bool {
    actual.ic_focused && actual.grab == GrabResourceState::Ready
}

/// Can the host safely process modifier updates right now? True iff the grab
/// resource is anything other than ABSENT or BROKEN. Modifiers (and their
/// keymap handoff) can flow during the NEEDS_KEYMAP window.
pub fn can_route_modifiers(actual: &ActualState) -> bool {
    matches!(
        actual.grab,
        GrabResourceState::NeedsKeymap | GrabResourceState::Ready
    )
}

/// Is the actual state mid-transition (between focus edges)? True iff the
/// engine is focused but the grab is not yet READY (or is BROKEN). The
/// keyboard guard's stuck-press failsafe uses this to avoid tearing down the
/// daemon while a normal activation handshake is still in flight.
pub fn is_transitioning(actual: &ActualState) -> bool {
    actual.ic_focused
        && matches!(
            actual.grab,
            GrabResourceState::Absent | GrabResourceState::NeedsKeymap | GrabResourceState::Broken
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive() -> InputFacts {
        InputFacts {
            connection_alive: true,
            ..Default::default()
        }
    }

    // ── Reduce: grab want ────────────────────────────────────────────────

    #[test]
    fn reduce_connection_dead_forces_none() {
        let facts = InputFacts {
            connection_alive: false,
            im_activate_seen: true,
            ..Default::default()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::None);
    }

    #[test]
    fn reduce_suspend_gap_forces_none() {
        let facts = InputFacts {
            suspend_gap_detected: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::None);
    }

    #[test]
    fn reduce_deactivate_to_soft_pause() {
        let facts = InputFacts {
            im_deactivate_seen: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::SoftPause);
    }

    #[test]
    fn reduce_done_committed_deactivate_to_soft_pause() {
        let facts = InputFacts {
            im_done_had_deactivate: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert_eq!(d.grab, GrabWant::SoftPause);
        assert!(d.focus_out);
    }

    #[test]
    fn reduce_batch_with_activate_and_deactivate_stays_yes() {
        let facts = InputFacts {
            engine_present: true,
            im_done_had_activate: true,
            im_done_had_deactivate: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert_eq!(d.grab, GrabWant::Yes);
        assert!(!d.focus_out);
        assert!(d.reactivate);
    }

    #[test]
    fn reduce_activate_to_yes() {
        let facts = InputFacts {
            im_activate_seen: true,
            engine_present: true,
            ..alive()
        };
        let prev = DesiredState::default();
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::Yes);
    }

    #[test]
    fn reduce_done_had_activate_to_yes() {
        let facts = InputFacts {
            im_done_had_activate: true,
            engine_present: true,
            ..alive()
        };
        let prev = DesiredState::default();
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::Yes);
    }

    #[test]
    fn reduce_activate_without_engine_stays_none() {
        let facts = InputFacts {
            im_activate_seen: true,
            engine_present: false,
            ..alive()
        };
        let prev = DesiredState::default();
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::None);
    }

    #[test]
    fn reduce_no_event_preserves_prev() {
        let facts = alive();
        let prev = DesiredState {
            grab: GrabWant::SoftPause,
            ..Default::default()
        };
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::SoftPause);
    }

    #[test]
    fn reduce_hard_boundary_overrides_deactivate() {
        let facts = InputFacts {
            suspend_gap_detected: true,
            im_deactivate_seen: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        assert_eq!(reduce(&facts, &prev).grab, GrabWant::None);
    }

    // ── Reduce: focus edge detection ─────────────────────────────────────

    #[test]
    fn reduce_none_to_yes_triggers_focus_in() {
        let facts = InputFacts {
            im_activate_seen: true,
            engine_present: true,
            ..alive()
        };
        let d = reduce(&facts, &DesiredState::default());
        assert!(d.focus_in);
        assert!(!d.focus_out);
    }

    #[test]
    fn reduce_yes_to_none_triggers_focus_out() {
        let facts = InputFacts {
            im_deactivate_seen: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert!(!d.focus_in);
        assert!(d.focus_out);
    }

    #[test]
    fn reduce_soft_pause_to_yes_triggers_focus_in() {
        let facts = InputFacts {
            im_activate_seen: true,
            engine_present: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::SoftPause,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert!(d.focus_in);
        assert!(!d.focus_out);
    }

    #[test]
    fn reduce_stable_yes_no_edge() {
        let facts = alive();
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert!(!d.focus_in);
        assert!(!d.focus_out);
    }

    #[test]
    fn reduce_reactivate_no_edge() {
        let facts = InputFacts {
            im_done_had_activate: true,
            engine_present: true,
            ..alive()
        };
        let prev = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let d = reduce(&facts, &prev);
        assert!(!d.focus_in);
        assert!(!d.focus_out);
    }

    // ── Diff: grab lifecycle ─────────────────────────────────────────────

    #[test]
    fn diff_none_with_absent_noop() {
        let d = DesiredState::default();
        let a = ActualState::default();
        let e = diff(&d, &a);
        assert!(!e.destroy_grab);
        assert!(!e.create_grab);
    }

    #[test]
    fn diff_none_with_ready_destroys() {
        let d = DesiredState::default();
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(e.destroy_grab);
        assert!(e.discard_composition);
        assert!(e.clear_preedit);
        assert!(e.commit);
    }

    #[test]
    fn diff_yes_with_absent_creates() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let a = ActualState::default();
        let e = diff(&d, &a);
        assert!(e.create_grab);
        assert!(e.scrub_generation);
    }

    #[test]
    fn diff_yes_with_broken_rebuilds() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Broken,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(e.destroy_grab);
        assert!(e.create_grab);
        assert!(e.scrub_generation);
    }

    #[test]
    fn diff_soft_pause_with_absent_creates() {
        let d = DesiredState {
            grab: GrabWant::SoftPause,
            ..Default::default()
        };
        let a = ActualState::default();
        let e = diff(&d, &a);
        assert!(e.create_grab);
        assert!(e.scrub_generation);
    }

    #[test]
    fn diff_soft_pause_with_ready_noop() {
        let d = DesiredState {
            grab: GrabWant::SoftPause,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(!e.destroy_grab);
        assert!(!e.create_grab);
    }

    #[test]
    fn diff_yes_with_needs_keymap_noop() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::NeedsKeymap,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(!e.destroy_grab);
        assert!(!e.create_grab);
    }

    // ── Diff: focus edges ────────────────────────────────────────────────

    #[test]
    fn diff_focus_in_effect() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            focus_in: true,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(e.send_focus_in);
        assert!(!e.send_focus_out);
    }

    #[test]
    fn diff_focus_in_with_retained_grab_does_not_create_grab() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            focus_in: true,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(e.send_focus_in);
        assert!(!e.create_grab);
    }

    #[test]
    fn diff_focus_out_effect() {
        let d = DesiredState {
            grab: GrabWant::SoftPause,
            focus_out: true,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(!e.send_focus_in);
        assert!(e.send_focus_out);
    }

    #[test]
    fn diff_focus_out_discards_composition() {
        let d = DesiredState {
            grab: GrabWant::SoftPause,
            focus_out: true,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(!e.destroy_grab);
        assert!(e.discard_composition);
        assert!(e.clear_preedit);
        assert!(e.commit);
    }

    #[test]
    fn diff_no_focus_change_keeps_composition() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        let e = diff(&d, &a);
        assert!(!e.discard_composition);
        assert!(!e.clear_preedit);
    }

    // ── Diff: idempotency ────────────────────────────────────────────────

    #[test]
    fn diff_idempotent_stable_none() {
        let d = DesiredState::default();
        let a = ActualState::default();
        assert_eq!(diff(&d, &a), diff(&d, &a));
        assert!(!diff(&d, &a).destroy_grab);
        assert!(!diff(&d, &a).create_grab);
    }

    #[test]
    fn diff_idempotent_stable_yes_ready() {
        let d = DesiredState {
            grab: GrabWant::Yes,
            ..Default::default()
        };
        let a = ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        };
        assert_eq!(diff(&d, &a), diff(&d, &a));
    }

    // ── Default safety (Rust analog of the C null-pointer tests) ─────────

    #[test]
    fn reduce_default_returns_safe_defaults() {
        // A dead connection (default `connection_alive == false`) forces NONE.
        let d = reduce(&InputFacts::default(), &DesiredState::default());
        assert_eq!(d.grab, GrabWant::None);
        assert!(!d.focus_in);
        assert!(!d.focus_out);
    }

    #[test]
    fn diff_default_returns_empty_effects() {
        let e = diff(&DesiredState::default(), &ActualState::default());
        assert!(!e.destroy_grab);
        assert!(!e.create_grab);
    }

    // ── Classifier ───────────────────────────────────────────────────────

    #[test]
    fn classify_done_cases() {
        assert_eq!(classify_done(false, true, false), DoneAction::FirstActivate);
        assert_eq!(classify_done(true, false, false), DoneAction::Deactivate);
        assert_eq!(classify_done(true, true, true), DoneAction::Reactivate);
        assert_eq!(classify_done(true, true, false), DoneAction::Noop);
        assert_eq!(classify_done(false, false, true), DoneAction::Noop);
    }

    // ── Guards ───────────────────────────────────────────────────────────

    #[test]
    fn can_route_keys_only_when_focused_and_ready() {
        assert!(can_route_keys(&ActualState {
            ic_focused: true,
            grab: GrabResourceState::Ready,
            ..Default::default()
        }));
        assert!(!can_route_keys(&ActualState {
            ic_focused: false,
            grab: GrabResourceState::Ready,
            ..Default::default()
        }));
        assert!(!can_route_keys(&ActualState {
            ic_focused: true,
            grab: GrabResourceState::NeedsKeymap,
            ..Default::default()
        }));
    }

    #[test]
    fn can_route_modifiers_during_keymap_window() {
        assert!(can_route_modifiers(&ActualState {
            grab: GrabResourceState::NeedsKeymap,
            ..Default::default()
        }));
        assert!(can_route_modifiers(&ActualState {
            grab: GrabResourceState::Ready,
            ..Default::default()
        }));
        assert!(!can_route_modifiers(&ActualState {
            grab: GrabResourceState::Absent,
            ..Default::default()
        }));
        assert!(!can_route_modifiers(&ActualState {
            grab: GrabResourceState::Broken,
            ..Default::default()
        }));
    }

    #[test]
    fn is_transitioning_when_focused_but_not_ready() {
        assert!(is_transitioning(&ActualState {
            ic_focused: true,
            grab: GrabResourceState::NeedsKeymap,
            ..Default::default()
        }));
        assert!(!is_transitioning(&ActualState {
            ic_focused: true,
            grab: GrabResourceState::Ready,
            ..Default::default()
        }));
        assert!(!is_transitioning(&ActualState {
            ic_focused: false,
            grab: GrabResourceState::Absent,
            ..Default::default()
        }));
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(GrabWant::SoftPause.name(), "SOFT_PAUSE");
        assert_eq!(GrabResourceState::NeedsKeymap.name(), "NEEDS_KEYMAP");
        assert_eq!(DoneAction::FirstActivate.name(), "FIRST_ACTIVATE");
    }
}
