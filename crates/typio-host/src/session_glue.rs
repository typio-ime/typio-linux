//! Input-method session lifecycle driver.
//!
//! Effectful half of the focus controller: observes live Wayland/libtypio
//! resources each tick and applies the minimal effect set produced by the
//! pure `focus_controller` module. This is the Rust analog of the C host's
//! `src/wayland/focus_effects.c` + the per-tick pipeline driver in
//! `src/wayland/event_loop.c`.

use crate::focus_controller::{self, ActualState, DesiredState, EffectSet, GrabResourceState};
use crate::input_method::InputMethodFrontend;
use crate::keyboard::router::KeyboardRouter;
use crate::repeat_timer::RepeatTimer;

/// Effectful surface the focus controller drives. Split out so the apply
/// order can be unit-tested with recording mocks.
pub(crate) trait ApplyTarget {
    /// 1. Discard composition before focus leaves.
    fn discard_composition(&mut self);
    /// 2. Focus out.
    fn focus_out(&mut self);
    /// 3. Destroy grab (hard teardown).
    fn destroy_grab(&mut self);
    /// 4. Clear compositor-visible preedit.
    fn clear_preedit(&mut self);
    /// 5. Commit any staged state.
    fn commit(&mut self);
    /// 6. Scrub key generation.
    fn scrub_generation(&mut self);
    /// 7. Create grab.
    fn create_grab(&mut self);
    /// 8. Focus in (after grab is ready/created).
    fn focus_in(&mut self);
    /// 9. Reactivate — re-anchor panel without changing focus state.
    fn reactivate(&mut self);
}

impl ApplyTarget
    for (
        &mut InputMethodFrontend,
        &mut KeyboardRouter,
        &mut RepeatTimer,
    )
{
    fn discard_composition(&mut self) {
        self.1.reset();
    }

    fn focus_out(&mut self) {
        self.1.focus_out();
        self.1.soft_pause();
        let _ = self.2.stop();
        tracing::debug!(
            target: "typio.panel.host",
            owner = ?self.0.state().panel_coord().visible_owner(),
            "panel: hide reason=focus_out"
        );
        if let Some(panel) = self.0.panel_mut() {
            panel.hide();
        }
        self.0.state_mut().clear_panel_state();
        self.0.state_mut().panel_coord_mut().hide_all();
        self.0.state_mut().clear_caret_rect();
    }

    fn destroy_grab(&mut self) {
        self.0.destroy_keyboard_grab();
        let _ = self.2.stop();
        self.1.physical_modifiers = crate::repeat_timer::Modifiers::NONE;
    }

    fn clear_preedit(&mut self) {
        self.0.state_mut().clear_preedit_and_flush();
        // The compositor has been told the preedit is gone; mirror that
        // into the router's tracking so the next engine composition is
        // not suppressed as a "no-op" against a preedit the user can no
        // longer see.
        self.1.preedit_tracking_reset();
    }

    fn commit(&mut self) {
        self.0.state_mut().commit();
    }

    fn scrub_generation(&mut self) {
        self.1.scrub_generation();
    }

    fn create_grab(&mut self) {
        self.0.create_keyboard_grab();
    }

    fn focus_in(&mut self) {
        let state = self.0.state_mut();
        state.text_input_rect = None;
        state.clear_caret_rect();
        state.reset_panel_anchor();
        state.probe_anchor();
        self.1.focus_in();
    }

    fn reactivate(&mut self) {
        let state = self.0.state_mut();
        state.text_input_rect = None;
        state.clear_caret_rect();
        state.reset_panel_anchor();
        state.probe_anchor();
    }
}

/// Per-tick driver that runs the focus-controller pipeline.
pub struct FocusDriver {
    prev: DesiredState,
}

impl Default for FocusDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// The focus-transition outcome of a single tick, if any. The driver's
/// `apply` step has already executed the side effects (grab build/teardown,
/// anchor reset, etc.); this value lets the caller layer additional
/// follow-on work — notably, the indicator's focus-path trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTransition {
    /// First activation of a field (was inactive, now active). Drives the
    /// indicator's `show_on_focus` path (salience + recency gates).
    FirstActivate,
    /// Re-focus inside the same session (active, fresh `activate`). Drives
    /// the indicator's `show_on_reactivate` path (salience gate only).
    Reactivate,
    /// Focus left the field. Hides the indicator along with all other
    /// Panel UI.
    Deactivate,
}

impl FocusDriver {
    pub fn new() -> Self {
        Self {
            prev: DesiredState::default(),
        }
    }

    /// Run one tick of the pipeline:
    ///   take facts → reduce → observe → diff → apply.
    ///
    /// Returns the focus transition that fired this tick, if any. The
    /// `apply` step has already happened by the time this returns.
    pub fn tick(
        &mut self,
        frontend: &mut InputMethodFrontend,
        router: &mut KeyboardRouter,
        timer: &mut RepeatTimer,
        engine_present: bool,
    ) -> Option<FocusTransition> {
        let mut facts = frontend.state_mut().take_facts();
        facts.engine_present = engine_present;

        let desired = focus_controller::reduce(&facts, &self.prev);
        let actual = observe(frontend.state(), router);
        let effects = focus_controller::diff(&desired, &actual);

        // Capture the transition before `apply` consumes the effects.
        let transition = if effects.send_focus_in {
            Some(FocusTransition::FirstActivate)
        } else if effects.reactivate {
            Some(FocusTransition::Reactivate)
        } else if effects.send_focus_out {
            Some(FocusTransition::Deactivate)
        } else {
            None
        };

        apply(&effects, &mut (frontend, router, timer));

        self.prev = desired;
        transition
    }
}

fn observe(state: &crate::input_method::InputMethodState, router: &KeyboardRouter) -> ActualState {
    ActualState {
        connection_alive: true,
        ic_focused: router.is_focused(),
        grab: if !state.keyboard_grab_present() {
            GrabResourceState::Absent
        } else if !state.keymap_received_this_epoch {
            GrabResourceState::NeedsKeymap
        } else {
            GrabResourceState::Ready
        },
    }
}

fn apply(effects: &EffectSet, target: &mut dyn ApplyTarget) {
    if effects.discard_composition {
        target.discard_composition();
    }
    if effects.send_focus_out {
        target.focus_out();
    }
    if effects.destroy_grab {
        target.destroy_grab();
    }
    if effects.clear_preedit {
        target.clear_preedit();
    }
    if effects.commit {
        target.commit();
    }
    if effects.scrub_generation {
        target.scrub_generation();
    }
    if effects.create_grab {
        target.create_grab();
    }
    if effects.send_focus_in {
        target.focus_in();
    }
    if effects.reactivate {
        target.reactivate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Call {
        DiscardComposition,
        FocusOut,
        DestroyGrab,
        ClearPreedit,
        Commit,
        ScrubGeneration,
        CreateGrab,
        FocusIn,
        Reactivate,
    }

    struct Recorder {
        calls: Vec<Call>,
    }

    impl Recorder {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }
    }

    impl ApplyTarget for Recorder {
        fn discard_composition(&mut self) {
            self.calls.push(Call::DiscardComposition);
        }
        fn focus_out(&mut self) {
            self.calls.push(Call::FocusOut);
        }
        fn destroy_grab(&mut self) {
            self.calls.push(Call::DestroyGrab);
        }
        fn clear_preedit(&mut self) {
            self.calls.push(Call::ClearPreedit);
        }
        fn commit(&mut self) {
            self.calls.push(Call::Commit);
        }
        fn scrub_generation(&mut self) {
            self.calls.push(Call::ScrubGeneration);
        }
        fn create_grab(&mut self) {
            self.calls.push(Call::CreateGrab);
        }
        fn focus_in(&mut self) {
            self.calls.push(Call::FocusIn);
        }
        fn reactivate(&mut self) {
            self.calls.push(Call::Reactivate);
        }
    }

    #[test]
    fn no_op_effects_produce_no_calls() {
        let mut rec = Recorder::new();
        apply(&EffectSet::default(), &mut rec);
        assert!(rec.calls.is_empty());
    }

    #[test]
    fn apply_executes_effects_in_contract_order() {
        let mut rec = Recorder::new();
        let effects = EffectSet {
            discard_composition: true,
            send_focus_out: true,
            destroy_grab: true,
            clear_preedit: true,
            commit: true,
            scrub_generation: true,
            create_grab: true,
            send_focus_in: true,
            reactivate: true,
        };
        apply(&effects, &mut rec);
        assert_eq!(
            rec.calls,
            vec![
                Call::DiscardComposition,
                Call::FocusOut,
                Call::DestroyGrab,
                Call::ClearPreedit,
                Call::Commit,
                Call::ScrubGeneration,
                Call::CreateGrab,
                Call::FocusIn,
                Call::Reactivate,
            ]
        );
    }

    #[test]
    fn partial_effects_keep_order() {
        let mut rec = Recorder::new();
        let effects = EffectSet {
            commit: true,
            create_grab: true,
            send_focus_in: true,
            ..EffectSet::default()
        };
        apply(&effects, &mut rec);
        assert_eq!(
            rec.calls,
            vec![Call::Commit, Call::CreateGrab, Call::FocusIn]
        );
    }
}
