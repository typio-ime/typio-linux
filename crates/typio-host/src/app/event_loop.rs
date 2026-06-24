//! The Wayland event-loop driver.
//!
//! `App::run_with_wayland` is the main per-tick pipeline: flush,
//! prepare-read, dispatch, poll, read, focus-controller, key drain,
//! panel flush, repeat, config-reload. Each stage is annotated with
//! `LoopStage` so the watchdog can attribute stalls.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::fd::{AsFd, AsRawFd};
use std::rc::Rc;
use std::time::Instant;

use typio::instance::TypioInstance;

use crate::ipc_bus::IpcBus;
use crate::keyboard::router::RepeatOutcome;
use crate::panel_coordinator::UiOwner;
use crate::panel_scheduler::{self, PanelUpdateResult};
use crate::session_glue::FocusTransition;
use crate::watchdog::LoopStage;

use super::{arm_repeat, tray::cycle_active_language, App, DaemonEvent};

impl App {
    /// The Wayland main loop. Returns the daemon exit code.
    ///
    /// Stages per tick (annotated with `LoopStage` for the watchdog):
    ///   Idle → AuxIo (focus controller) → Flush → PrepareRead →
    ///   DispatchPending → Poll → ReadEvents → Repeat → PanelUpdate →
    ///   ConfigReload.
    pub(super) fn run_with_wayland(&mut self, ipc_bus: &Rc<RefCell<IpcBus>>) -> i32 {
        let wl_fd = self.frontend.as_mut().unwrap().fd();
        let uds_fd = ipc_bus.borrow().epoll_fd();
        let repeat_fd = self.repeat_timer.as_mut().unwrap().fd();
        // Re-borrow the watchdog field on every use so a long-lived immutable
        // borrow does not block the mutable borrow needed by reload_config().
        macro_rules! wd {
            () => {
                self.watchdog.as_ref().unwrap()
            };
        }

        let (inotify_fd, cfg_timer_fd) = self
            .config_watcher
            .as_ref()
            .map(|w| (w.inotify_fd(), w.timer_fd()))
            .unwrap_or((-1, -1));

        // Indicator auto-hide timer. We pull the raw fd up-front (stable
        // for the timerfd's lifetime) so we can add it to the static poll
        // set; the timer is armed/disarmed via `TimerFd::set` elsewhere.
        let indicator_fd = self
            .indicator_timer
            .as_ref()
            .map(|t| t.as_fd().as_raw_fd())
            .unwrap_or(-1);

        let mut fds = [
            libc::pollfd { fd: wl_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: uds_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: repeat_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: inotify_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: cfg_timer_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: indicator_fd, events: libc::POLLIN, revents: 0 },
        ];

        while !self.drain_events() {
            wd!().set_stage(LoopStage::Idle);
            wd!().heartbeat();

            // 1. Start-of-tick fact bookkeeping.
            {
                let frontend = self.frontend.as_mut().unwrap();
                frontend.state_mut().facts_mut().connection_alive = true;
                if let Some(ref mut rs) = self.resume_signal {
                    if !rs.tick().is_empty() {
                        frontend.state_mut().facts_mut().suspend_gap_detected = true;
                    }
                }
                if frontend.stopped() {
                    eprintln!("typio: input method unavailable");
                    return 1;
                }
            }

            // 2. Flush outgoing Wayland requests, then prepare a read and
            //    dispatch any already-queued events before polling.
            wd!().set_stage(LoopStage::Flush);
            {
                let frontend = self.frontend.as_ref().unwrap();
                if let Err(e) = frontend.flush() {
                    eprintln!("Wayland flush error: {e}");
                    return 1;
                }
            }
            wd!().set_stage(LoopStage::PrepareRead);
            let read_guard = {
                let frontend = self.frontend.as_mut().unwrap();
                match frontend.prepare_read_loop() {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        eprintln!("Wayland prepare_read error: {e}");
                        return 1;
                    }
                }
            };
            wd!().set_stage(LoopStage::DispatchPending);
            ipc_bus.borrow_mut().dispatch();

            for slot in fds.iter_mut() {
                slot.revents = 0;
            }

            // 3. Poll. Let the panel scheduler and the panel anchor deadline
            //    shorten the timeout.
            wd!().set_stage(LoopStage::Poll);
            wd!().heartbeat();
            let timeout_ms = {
                let frontend = self.frontend.as_ref().unwrap();
                let state = frontend.state();
                let router = self.router.as_ref().unwrap();
                let flushable = panel_scheduler::should_flush(
                    state.panel_schedule_state,
                    router.is_focused(),
                    !router.ctx().is_null(),
                    router.is_focused(),
                );
                let mut timeout_ms =
                    panel_scheduler::poll_timeout_ms(state.panel_schedule_state, flushable, -1);
                let now = Instant::now();
                let mut reduce_timeout = |remaining: i32| {
                    if timeout_ms < 0 || remaining < timeout_ms {
                        timeout_ms = remaining;
                    }
                };
                if let Some(remaining) = state.panel_coord.anchor_deadline_remaining_ms(now) {
                    reduce_timeout(remaining as i32);
                }
                if let Some(indicator_remaining) = self.indicator_hide_remaining_ms(now) {
                    reduce_timeout(indicator_remaining);
                }
                timeout_ms
            };
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout_ms) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                eprintln!("poll error: {e}");
                return 1;
            }

            // 4. Read and dispatch new Wayland events, or cancel the prepared read.
            wd!().set_stage(LoopStage::ReadEvents);
            if fds[0].revents & libc::POLLIN != 0 {
                let frontend = self.frontend.as_mut().unwrap();
                if let Some(guard) = read_guard {
                    let timing_enabled =
                        tracing::enabled!(target: "typio.wayland.io", tracing::Level::DEBUG);
                    let started = timing_enabled.then(Instant::now);
                    if let Err(e) = frontend.read_and_dispatch(guard) {
                        eprintln!("Wayland read error: {e}");
                        return 1;
                    }
                    if let Some(started) = started {
                        tracing::debug!(
                            target: "typio.wayland.io",
                            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                            "read_and_dispatch"
                        );
                    }
                }
            } else if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                eprintln!("Wayland display disconnected");
                return 1;
            }
            // If POLLIN was not set, `read_guard` is dropped here and cancels the read.

            // 5. Run the focus-controller pipeline.
            wd!().set_stage(LoopStage::AuxIo);
            let focus_transition = {
                let engine_present = self
                    .instance
                    .as_ref()
                    .and_then(|i| i.registry_rust())
                    .map(|r| r.active_keyboard_name().is_some())
                    .unwrap_or(false);
                let frontend = self.frontend.as_mut().unwrap();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();
                let mut transition = None;
                if let Some(ref mut driver) = self.focus_driver {
                    transition = driver.tick(frontend, router, timer, engine_present);
                }
                transition
            };

            // 5b. Translate the focus transition into an indicator trigger.
            //    The focus driver has already applied its effects (grab
            //    build/teardown, anchor reset, panel hide on deactivate);
            //    this only layers the indicator on top.
            if let Some(t) = focus_transition {
                match t {
                    FocusTransition::FirstActivate => self.trigger_indicator_focus(),
                    FocusTransition::Reactivate => self.trigger_indicator_reactivate(),
                    FocusTransition::Deactivate => self.hide_indicator(),
                }
            }

            // 6. Drain engine output and process all pending key events.
            //    Draining the whole queue (not just one event) is what
            //    prevents the "stuck backspace" symptom: if a release and
            //    a subsequent press arrive in the same Wayland dispatch
            //    batch, both must reach the router in order — losing the
            //    release leaves the repeat timer armed forever.
            {
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();

                router.drain_commit(state);
                router.drain_composition(state);

                let pending_keys = state.take_pending_keys();
                if !pending_keys.is_empty() {
                    tracing::debug!(
                        target: "typio.input.queue",
                        pending_key_count = pending_keys.len(),
                        "drain pending keys"
                    );
                }
                for key in pending_keys {
                    // Snapshot the values we need before any mutable borrows
                    // below — both are cheap `Copy` reads.
                    let mods = state.mods_depressed;
                    let compositor_info = state.compositor_repeat_info;
                    if key.state == 1 {
                        // Try host-managed selection (ADR-0012) first.
                        // Engines that opt in get their navigation/commit
                        // keys handled locally without a synchronous FFI
                        // round-trip; engines that don't opt in see no
                        // behaviour change.
                        let consumed = match router.try_host_selection(&key, state) {
                            Some(handled) => handled,
                            None => router.dispatch_key(&key, mods),
                        };
                        // Any key that reached the engine (consumed or
                        // forwarded) counts as "user activity" for the
                        // indicator's acknowledged-recency gate. Releases,
                        // modifier-only events, and filtered-out keys do
                        // not (mirrors the C `record_key_activity` caller
                        // in keyboard.c).
                        if let Some(indicator) = self.indicator.as_mut() {
                            indicator.record_key_activity(Instant::now());
                        }
                        if router.take_switch_chord_fired() {
                            // Ctrl+Shift (default) just completed. Cycle
                            // to the next language (reusing the engine last
                            // used for it); with <2 languages the cycle
                            // falls back to engine cycling. Suppresses
                            // forwarding of the modifier press itself.
                            eprintln!("indicator: Ctrl+Shift chord fired");
                            let instance_ptr = self
                                .instance
                                .as_mut()
                                .map(|i| i.as_mut() as *mut TypioInstance)
                                .unwrap_or(std::ptr::null_mut());
                            cycle_active_language(instance_ptr);
                            let _ = self.event_tx.send(DaemonEvent::StateRefresh);
                        } else if consumed {
                            // Engine consumed the key. Drain any output it
                            // produced, then arm the repeat timer in engine
                            // mode so the held key re-dispatches with
                            // `is_repeat: true` (e.g. backspace deleting a
                            // long preedit one char per tick).
                            router.drain_commit(state);
                            router.drain_composition(state);
                            router.on_consumed(key.clone());
                            arm_repeat(timer, compositor_info, mods);
                        } else {
                            // Engine declined the key; forward it to the
                            // focused app and arm the timer in forward mode
                            // so the main loop synthesises repeats.
                            state.forward_key(key.time, key.keycode, key.state);
                            router.on_forward(key.clone());
                            arm_repeat(timer, compositor_info, mods);
                        }
                    } else {
                        // Forward release events to the engine so
                        // engines that need them (e.g. Rime schema
                        // switching on a lone Shift release) can
                        // complete gesture detection. Modifier state
                        // is mirrored separately via the Modifiers
                        // grab event, so not forwarding a consumed
                        // release here does not leave a stuck modifier
                        // in the focused app. Host-managed selection
                        // releases are swallowed by `try_host_selection`
                        // so the engine never sees an unpaired release
                        // for a press the host intercepted.
                        let consumed = match router.try_host_selection(&key, state) {
                            Some(handled) => handled,
                            None => router.dispatch_key(&key, mods),
                        };
                        if consumed {
                            router.drain_commit(state);
                            router.drain_composition(state);
                        } else {
                            state.forward_key(key.time, key.keycode, key.state);
                        }
                        router.on_release(&key);
                        let _ = timer.stop();
                    }
                }
            }

            // 7. Flush the candidate panel if the scheduler says so.
            wd!().set_stage(LoopStage::PanelUpdate);
            {
                // Pull the watchdog handle up-front, mirroring
                // `render_indicator_banner`. The heartbeat closure
                // captures only this reference (not all of `self`),
                // which lets it coexist with the mutable `frontend`
                // borrow used to obtain `panel` — a split-borrow
                // requirement that the inline `wd!()` form would
                // trip when nested inside a closure passed to
                // `draw_candidates`.
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
                heartbeat();

                let frontend = self.frontend.as_mut().unwrap();
                let router = self.router.as_mut().unwrap();
                // Cheap scalars only — avoid cloning the candidates Vec on
                // every tick. The clone is deferred to the rare flush path
                // below; the common Idle or non-flushing tick now pays only
                // three field reads.
                let (schedule_state, selected, composition_seq, candidate_count) = {
                    let state = frontend.state();
                    (
                        state.panel_schedule_state,
                        state.composition.selected_candidate,
                        state.composition.composition_seq,
                        state.composition.candidates.len(),
                    )
                };
                let has_session = router.is_focused();
                let has_context = !router.ctx().is_null();
                // `has_session` and `context_focused` are both
                // `router.is_focused()`; collapse them into one source.
                let context_focused = has_session;
                let should_flush = panel_scheduler::should_flush(
                    schedule_state,
                    has_session,
                    has_context,
                    context_focused,
                );
                if schedule_state != panel_scheduler::PanelScheduleState::Idle {
                    tracing::debug!(
                        target: "typio.panel.scheduler",
                        schedule_state = ?schedule_state,
                        should_flush,
                        composition_seq,
                        candidate_count,
                        selected,
                        has_session,
                        has_context,
                        context_focused,
                        "panel scheduler tick"
                    );
                }
                if should_flush {
                    // Now that we know we'll actually render, take the
                    // expensive snapshot — the candidate strings, needed
                    // for measurement and `flux_text_draw`.
                    let candidates = frontend.state().composition.candidates.clone();
                    let scale = frontend.state().buffer_scale;
                    let owner = frontend.state().panel_coord().visible_owner();

                    // Frame throttle. flux presents synchronously on this
                    // thread (the async present thread was dropped from flux
                    // because Mesa's Wayland WSI dispatches wl_display
                    // events inside vkQueuePresentKHR and races the loop).
                    // Under compositor back-pressure — the norm during rapid
                    // candidate paging — a synchronous present blocks until
                    // the compositor releases a swapchain image, which can
                    // exceed the watchdog's Present threshold and SIGKILL
                    // the daemon. Arming wl_surface.frame after each present
                    // and skipping the next present until the compositor
                    // acks keeps the present rate at the compositor refresh
                    // rate, so the swapchain never exhausts free images and
                    // present never blocks. Hides are never throttled — they
                    // detach the buffer and cannot block.
                    let throttled = !candidates.is_empty()
                        && frontend.state_mut().panel_present_blocked();

                    // Manage surface ownership before the mutable panel
                    // borrow. Candidates claim the surface when shown
                    // (superseding a visible indicator, so its auto-hide
                    // cannot later kill the candidate list); they release
                    // it when empty unless an overlay is borrowing it.
                    {
                        let coord = frontend.state_mut().panel_coord_mut();
                        if candidates.is_empty() {
                            if owner != UiOwner::Indicator && owner != UiOwner::Voice {
                                coord.hide(UiOwner::Candidate);
                            }
                        } else {
                            coord.claim(UiOwner::Candidate);
                        }
                    }
                    let (result, presented, hid) = if throttled {
                        tracing::trace!(
                            target: "typio.panel.host",
                            "panel: skip present reason=frame_throttled"
                        );
                        (PanelUpdateResult::Done, false, false)
                    } else if let Some(panel) = frontend.panel_mut() {
                        panel.set_scale(scale);
                        heartbeat();
                        let mut hid = false;
                        let presented = if candidates.is_empty() {
                            if owner == UiOwner::Indicator || owner == UiOwner::Voice {
                                // Surface is loaned to the indicator/voice
                                // overlay; the candidate path does not own it
                                // right now and must not hide it (would kill
                                // the overlay mid-display).
                                tracing::trace!(
                                    target: "typio.panel.host",
                                    owner = ?owner,
                                    "panel: skip-hide reason=empty_on_loan"
                                );
                            } else {
                                tracing::debug!(
                                    target: "typio.panel.host",
                                    owner = ?owner,
                                    "panel: hide reason=candidates_empty"
                                );
                                panel.hide();
                                hid = true;
                            }
                            false
                        } else {
                            panel.ensure_candidate_size(&candidates);
                            heartbeat();
                            panel.draw_candidates(
                                &candidates,
                                selected,
                                composition_seq,
                                &heartbeat,
                                &enter_present,
                            );
                            true
                        };
                        (PanelUpdateResult::Done, presented, hid)
                    } else {
                        (PanelUpdateResult::Done, false, false)
                    };
                    if throttled {
                        // Leave the schedule dirty so the tick that wakes
                        // on the frame callback (or the fallback timeout)
                        // re-attempts the present with the latest coalesced
                        // candidates. Do not call complete(): that would
                        // move to Idle and drop the pending frame.
                    } else {
                        if hid {
                            frontend.state_mut().clear_panel_frame_callback();
                        }
                        frontend.state_mut().panel_schedule_state =
                            panel_scheduler::complete(result);
                        if presented {
                            frontend.arm_panel_frame_callback();
                        }
                    }
                }
            }

            // 7b. Flush any pending positioned status UI (indicator / voice)
            //     when the anchor becomes ready or the caret fallback fires.
            //     Drives the deferred-show path: a `show_on_focus` or
            //     `show_for_state_change` call returned a label, the
            //     coordinator queued it because the anchor wasn't ready,
            //     and now the anchor resolved (or the caret fallback fired).
            {
                let now = Instant::now();
                let flushed = {
                    let frontend = self.frontend.as_mut().unwrap();
                    let state = frontend.state_mut();
                    state.panel_coord_mut().flush_pending_with_timeout(now)
                };
                if let Some((owner, label)) = flushed {
                    eprintln!(
                        "indicator: deferred flush owner={:?} label='{}'",
                        owner, label
                    );
                    if owner == UiOwner::Indicator {
                        self.render_indicator_banner(&label, now);
                    }
                }
                // UiOwner::Voice is reserved for a future chunk; the flush
                // path is in place and tested by panel_coordinator's queue
                // tests, but no producer feeds it yet.
            }

            // 8. Repeat timer expiration.
            if fds[2].revents & libc::POLLIN != 0 {
                wd!().set_stage(LoopStage::Repeat);
                let mut buf = [0u8; 8];
                unsafe {
                    libc::read(repeat_fd, buf.as_mut_ptr() as *mut c_void, buf.len());
                }
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();
                let mods = state.mods_depressed;
                match router.dispatch_repeat(state, mods) {
                    RepeatOutcome::Forwarded => {}
                    RepeatOutcome::Consumed => {
                        router.drain_commit(state);
                        router.drain_composition(state);
                    }
                    RepeatOutcome::Stopped => {
                        let _ = timer.stop();
                    }
                }
            }

            // 8b. Indicator auto-hide timer expiration. The timerfd fires
            //     once after `display.indicator_duration_ms`; we hide the
            //     popup and disarm. The indicator's recency tracking is
            //     left intact so a recent indicator still suppresses the
            //     next focus-path reveal.
            if fds[5].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 8];
                if let Some(tf) = self.indicator_timer.as_ref() {
                    unsafe {
                        libc::read(
                            tf.as_fd().as_raw_fd(),
                            buf.as_mut_ptr() as *mut c_void,
                            buf.len(),
                        );
                    }
                }
                self.hide_indicator();
            }

            // End-of-tick heartbeat.
            wd!().stage_done();

            // 9. Config watcher events. These are handled after the main
            //    pipeline so a temporary field borrow can be used for the
            //    config reload without colliding with the watchdog macro.
            if fds[3].revents & libc::POLLIN != 0 {
                if let Some(ref mut watcher) = self.config_watcher {
                    match watcher.drain_inotify() {
                        Ok(outcome) => {
                            if outcome.should_rearm_watches {
                                let _ = watcher.rearm_watches();
                            }
                            if outcome.should_schedule_reload {
                                let _ = watcher.schedule_reload();
                            }
                        }
                        Err(e) => eprintln!("config watcher inotify error: {e}"),
                    }
                }
            }
            if fds[4].revents & libc::POLLIN != 0 {
                let should_reload = if let Some(ref mut watcher) = self.config_watcher {
                    match watcher.drain_timer() {
                        Ok(true) => true,
                        Ok(false) => false,
                        Err(e) => {
                            eprintln!("config watcher timer error: {e}");
                            false
                        }
                    }
                } else {
                    false
                };
                if should_reload {
                    wd!().set_stage(LoopStage::ConfigReload);
                    self.reload_config();
                }
            }
        }

        eprintln!("typio: shutting down...");
        0
    }
}
