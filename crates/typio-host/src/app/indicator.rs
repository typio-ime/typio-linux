//! On-screen status indicator driving.
//!
//! All `App` methods that compose, schedule, render, or hide the
//! transient `<badge> · <engine> · <mode>` banner live here. The loop
//! driver in [`super::mod`] calls into [`App::trigger_indicator_focus`]
//! / [`App::trigger_indicator_reactivate`] / [`App::trigger_indicator_state_change`]
//! / [`App::request_indicator_show`] / [`App::render_indicator_banner`] /
//! [`App::hide_indicator`] / [`App::indicator_hide_remaining_ms`].

use std::ffi::CStr;
use std::time::Instant;

use nix::sys::time::TimeSpec;
use nix::sys::timerfd::{Expiration, TimerSetTimeFlags};
use typio_abi::TypioStatusSalience;

use crate::indicator::{EngineModeSnapshot, IndicatorConfig, LabelSources, Salience};
use crate::panel_coordinator::{FlushDecision, UiOwner};
use crate::watchdog::LoopStage;

use typio::TypioInstance;

use super::App;

/// Which indicator show-path to take. Mirror of
/// [`Indicator`](crate::indicator::Indicator)'s three public methods,
/// lifted into a tag so [`App::trigger_indicator_show`] can dispatch on
/// a single borrow scope without re-borrowing `self` for each arm.
#[cfg(feature = "wayland")]
#[derive(Debug, Clone, Copy)]
pub(super) enum IndicatorPath {
    Focus,
    Reactivate,
    StateChange,
}

/// [`LabelSources`] backed by the live `EngineRegistry`. Borrows its
/// strings so the indicator label composition is zero-allocation on the
/// hot path.
pub(super) struct RegistryLabelSources<'a> {
    pub(super) registry: &'a typio::core::registry::EngineRegistry,
}

impl<'a> LabelSources for RegistryLabelSources<'a> {
    fn active_language_tag(&self) -> Option<&str> {
        self.registry.active_language()
    }
    fn active_engine_name(&self) -> Option<&str> {
        self.registry.active_keyboard_name()
    }
    fn active_engine_display_name(&self) -> Option<&str> {
        self.registry
            .active_keyboard_name()
            .and_then(|name| self.registry.engine_info(name))
            .map(|info| info.display_name.as_str())
    }
}

impl App {
    /// Read `display.indicator_*` from libtypio's config cache. Returns
    /// the default-enabled, default-1500ms config when the instance or
    /// config pointer is unavailable.
    pub(super) fn load_indicator_config(&self) -> IndicatorConfig {
        let raw = match self.instance.as_ref() {
            Some(i) => i.as_ref() as *const TypioInstance as *mut TypioInstance,
            None => return IndicatorConfig::default(),
        };
        let cfg = typio::instance::typio_instance_get_config(raw);
        if cfg.is_null() {
            return IndicatorConfig::default();
        }
        let enabled =
            typio::config::typio_config_get_bool(cfg, c"display.indicator_enabled".as_ptr(), true);
        let duration_ms = typio::config::typio_config_get_int(
            cfg,
            c"display.indicator_duration_ms".as_ptr(),
            1500,
        );
        IndicatorConfig::from_values(enabled, duration_ms.into())
    }

    /// Trigger the indicator's focus-path show (FirstActivate). Reads the
    /// live registry and cached mode from libtypio; applies the salience
    /// gate + acknowledged-recency gate.
    #[cfg(feature = "wayland")]
    pub(super) fn trigger_indicator_focus(&mut self) {
        self.trigger_indicator_show(IndicatorPath::Focus);
    }

    /// Trigger the indicator's reactivate-path show (Reactivate). Gates:
    /// salience only — recency is skipped per ADR-0018.
    #[cfg(feature = "wayland")]
    pub(super) fn trigger_indicator_reactivate(&mut self) {
        self.trigger_indicator_show(IndicatorPath::Reactivate);
    }

    /// Trigger the indicator's deliberate-change show (no gates beyond
    /// `enabled`). Called from the `StateRefresh` drain — covers Ctrl+Shift
    /// chord, tray-driven engine/language switch, and IPC-driven mutations.
    #[cfg(feature = "wayland")]
    pub(super) fn trigger_indicator_state_change(&mut self) {
        self.trigger_indicator_show(IndicatorPath::StateChange);
    }

    /// Shared body of the three trigger paths. Resolves label sources from
    /// the live registry, asks the [`Indicator`](crate::indicator::Indicator)
    /// state machine for a label, and feeds any returned label to
    /// [`Self::request_indicator_show`].
    #[cfg(feature = "wayland")]
    fn trigger_indicator_show(&mut self, path: IndicatorPath) {
        let now = Instant::now();

        // Read the current mode from libtypio's cache. The mode-changed
        // callback stores the fresh mode in `last_mode` before firing, so
        // by the time StateRefresh delivers us here, the data is current.
        // This covers rime's schema/mode switches: the engine reports its
        // new active mode (e.g. display_label="中", salience=Notable), the
        // callback fires, we read it here, and the indicator shows the mode
        // suffix instead of just the bare engine name.
        let mode_display: Option<String> = self.read_mode_display_label();
        let mode_salience = self.read_mode_salience();

        let label = {
            let Some(instance) = self.instance.as_ref() else {
                eprintln!("indicator: no instance, skipping");
                return;
            };
            let Some(registry) = instance.registry_rust() else {
                eprintln!("indicator: no registry, skipping");
                return;
            };
            let sources = RegistryLabelSources { registry };
            let Some(indicator) = self.indicator.as_mut() else {
                eprintln!("indicator: no indicator state machine, skipping");
                return;
            };
            let cfg = self.indicator_config;

            // Build the mode snapshot from the libtypio-cached mode. The
            // snapshot always exists when we have a valid mode pointer,
            // even if `display_label` is None — the salience gate still
            // needs to see it.
            let mode_snapshot = EngineModeSnapshot {
                display_label: mode_display.as_deref(),
                salience: mode_salience,
            };
            let mode_ref = Some(&mode_snapshot);

            let label = match path {
                IndicatorPath::Focus => indicator.show_on_focus(now, mode_ref, &cfg, &sources),
                IndicatorPath::Reactivate => {
                    indicator.show_on_reactivate(now, mode_ref, &cfg, &sources)
                }
                IndicatorPath::StateChange => {
                    indicator.show_for_state_change(now, mode_ref, &cfg, &sources)
                }
            };
            eprintln!(
                "indicator: path={:?} mode_display={:?} salience={:?} lang={:?} engine={:?} → label={:?}",
                path,
                mode_display,
                mode_salience,
                sources.active_language_tag(),
                sources.active_engine_name(),
                label
            );
            label
        };
        if let Some(label) = label {
            self.request_indicator_show(label, now);
        }
    }

    /// Read the cached mode's `display_label` from libtypio (e.g. "中",
    /// "A", "Latin"). Returns `None` when no engine has reported a mode
    /// yet, or when the mode has no display label.
    fn read_mode_display_label(&self) -> Option<String> {
        let raw = self.instance.as_ref()?;
        let raw = raw.as_ref() as *const TypioInstance as *mut TypioInstance;
        let mode_ptr = typio::instance::typio_instance_get_last_keyboard_mode(raw);
        if mode_ptr.is_null() {
            return None;
        }
        let mode = unsafe { &*mode_ptr };
        if mode.display_label.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(mode.display_label) }.to_string_lossy().into_owned())
        }
    }

    /// Read the cached mode's salience. Returns `Quiet` when no mode is set.
    fn read_mode_salience(&self) -> Salience {
        let raw = match self.instance.as_ref() {
            Some(i) => i.as_ref() as *const TypioInstance as *mut TypioInstance,
            None => return Salience::Quiet,
        };
        let mode_ptr = typio::instance::typio_instance_get_last_keyboard_mode(raw);
        if mode_ptr.is_null() {
            return Salience::Quiet;
        }
        let mode = unsafe { &*mode_ptr };
        match mode.salience {
            TypioStatusSalience::TypioStatusSalienceNotable => Salience::Notable,
            _ => Salience::Quiet,
        }
    }

    /// Feed an indicator show request through the `PanelCoordinator`.
    /// If the anchor is ready the banner renders immediately and the
    /// auto-hide timer is armed; otherwise the coordinator queues the
    /// request and it flushes on a later tick through
    /// `flush_pending_with_timeout`.
    #[cfg(feature = "wayland")]
    pub(super) fn request_indicator_show(&mut self, label: String, now: Instant) {
        let decision = {
            let Some(frontend) = self.frontend.as_mut() else {
                eprintln!("indicator: no frontend, skipping");
                return;
            };
            let coord = frontend.state_mut().panel_coord_mut();
            let anchor_ready = coord.anchor_ready();
            let decision = coord.decide_positioned_flush(UiOwner::Indicator, &label);
            eprintln!(
                "indicator: coordinator anchor_ready={} → {:?}",
                anchor_ready, decision
            );
            decision
        };
        match decision {
            FlushDecision::Show => self.render_indicator_banner(&label, now),
            FlushDecision::Pending => {
                eprintln!("indicator: queued, will flush when anchor resolves");
            }
            FlushDecision::Cancel => {
                eprintln!("indicator: coordinator cancelled the request");
                if let Some(indicator) = self.indicator.as_mut() {
                    indicator.hide();
                }
            }
        }
    }

    /// Render the indicator banner onto the candidate Panel's surface,
    /// mark the indicator as shown (updates recency tracking), and arm
    /// the auto-hide timer. Called either from
    /// [`Self::request_indicator_show`] when the coordinator accepts
    /// immediately, or from the loop's `flush_pending_with_timeout` path
    /// when a queued show later becomes renderable.
    #[cfg(feature = "wayland")]
    pub(super) fn render_indicator_banner(&mut self, label: &str, now: Instant) {
        let scale = self
            .frontend
            .as_ref()
            .map(|f| f.state().buffer_scale)
            .unwrap_or(1.0);
        let wd_ref = self.watchdog.as_ref();
        let heartbeat = move || {
            if let Some(wd) = wd_ref {
                wd.heartbeat();
            }
        };
        let enter_present = move || {
            if let Some(wd) = wd_ref {
                wd.set_stage(LoopStage::Present);
            }
        };
        if let Some(panel) = self.frontend.as_mut().and_then(|f| f.panel_mut()) {
            panel.set_scale(scale);
            heartbeat();
            panel.ensure_banner_size(label);
            heartbeat();
            panel.draw_status_banner(label, &heartbeat, &enter_present);
        }
        if let Some(indicator) = self.indicator.as_mut() {
            indicator.note_shown(now);
        }
        self.arm_indicator_timer(now);
    }

    /// Hide the indicator (timer expiry, deactivate, or coordinator
    /// cancel). Clears the indicator's active flag, dismisses any queued
    /// request for the Indicator owner, detaches the popup surface if the
    /// Indicator was the visible owner, and disarms the auto-hide timer.
    /// Leaves the recency edges intact: a recently-shown indicator still
    /// suppresses the next focus-path reveal.
    #[cfg(feature = "wayland")]
    pub(super) fn hide_indicator(&mut self) {
        if let Some(indicator) = self.indicator.as_mut() {
            indicator.hide();
        }
        let indicator_owned = {
            let Some(frontend) = self.frontend.as_mut() else {
                return;
            };
            let coord = frontend.state_mut().panel_coord_mut();
            let was_visible = coord.visible_owner() == UiOwner::Indicator;
            coord.hide(UiOwner::Indicator);
            was_visible
        };
        if indicator_owned {
            eprintln!("panel: hide reason=indicator_autohide");
            if let Some(panel) = self.frontend.as_mut().and_then(|f| f.panel_mut()) {
                panel.hide();
            }
        }
        self.disarm_indicator_timer();
    }

    /// Arm the auto-hide timer for the indicator's configured duration
    /// (clamped to 100–10000 ms in [`IndicatorConfig`]). Idempotent —
    /// re-arming replaces any prior deadline.
    #[cfg(feature = "wayland")]
    pub(super) fn arm_indicator_timer(&mut self, now: Instant) {
        let duration = self.indicator_config.duration;
        if let Some(tf) = self.indicator_timer.as_ref() {
            let expiration = Expiration::OneShot(TimeSpec::from_duration(duration));
            let _ = tf.set(expiration, TimerSetTimeFlags::empty());
        }
        self.indicator_hide_deadline = Some(now + duration);
    }

    /// Disarm the auto-hide timer. Safe to call on an already-disarmed
    /// timer; arming with a zero `it_value` is the kernel-defined disarm.
    #[cfg(feature = "wayland")]
    pub(super) fn disarm_indicator_timer(&mut self) {
        if let Some(tf) = self.indicator_timer.as_ref() {
            let expiration =
                Expiration::OneShot(TimeSpec::from_duration(std::time::Duration::ZERO));
            let _ = tf.set(expiration, TimerSetTimeFlags::empty());
        }
        self.indicator_hide_deadline = None;
    }

    /// Remaining milliseconds until the indicator auto-hide deadline, or
    /// `None` when the timer is not armed.
    #[cfg(feature = "wayland")]
    pub(super) fn indicator_hide_remaining_ms(&self, now: Instant) -> Option<i32> {
        self.indicator_hide_deadline
            .and_then(|d| d.checked_duration_since(now))
            .map(|rem| rem.as_millis() as i32)
            .map(|ms| ms.max(0))
    }
}
