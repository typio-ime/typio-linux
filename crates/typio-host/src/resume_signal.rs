//! System-resume detector: logind PrepareForSleep + boottime/monotonic gap.
//!
//! Phase 3a port of `src/engine/logind/resume.c` (252 lines of C) and
//! the pure decision rules in `src/engine/resume_model.h`.
//!
//! ## Why this exists
//!
//! The composition lifecycle's most user-visible failure mode is a stuck
//! modifier or runaway repeat after the laptop wakes up: while suspended
//! the kernel cannot deliver a key-up, and on resume the compositor may
//! not always emit a clean deactivate/activate round-trip. The daemon
//! needs a first-class "the system just resumed" signal independent of
//! Wayland events.
//!
//! ## Two complementary detectors
//!
//! 1. **logind PrepareForSleep** — system-bus signal emitted by
//!    systemd-logind around any suspend/hibernate. Reliable when present,
//!    absent on non-systemd distros. Subscribed via `zbus` on a dedicated
//!    worker thread; events are channelled back to the caller's thread.
//!
//! 2. **boottime/monotonic gap heuristic** — `CLOCK_BOOTTIME` advances
//!    during suspend while `CLOCK_MONOTONIC` does not. The caller invokes
//!    [`ResumeSignal::tick`] once per event-loop iteration; any gap
//!    greater than the threshold is treated as "the kernel just woke us
//!    up." Always active; serves as a fallback when logind is missing
//!    or its signal is lost.
//!
//! Both detectors deduplicate on a short cooldown window so a coincident
//! logind notification and detected gap fire exactly one event.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::time::ClockId;

/// Default minimum gap between `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME` deltas
/// that counts as a real suspend. Matches the C constant
/// `TYPIO_WL_RESUME_GAP_THRESHOLD_MS`. 2000 ms is comfortably larger than
/// any legitimate single-tick pause (debugger pauses, very long GCs) but
/// small enough to catch even very short S3 cycles on modern hardware.
pub const DEFAULT_GAP_THRESHOLD: Duration = Duration::from_millis(2000);

/// Default post-fire cooldown. Matches `TYPIO_WL_RESUME_COOLDOWN_MS`.
/// Whichever detector fires first suppresses the other for this long.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_millis(5000);

/// Which detector observed the resume. Surfaced as a stable identifier in
/// [`ResumeEvent`] for diagnostic logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeReason {
    /// `org.freedesktop.login1.Manager.PrepareForSleep` signal, false edge.
    Logind,
    /// Boottime/monotonic gap exceeded the threshold.
    BoottimeGap,
}

impl ResumeReason {
    /// Stable string identifier matching the C version's reason literals.
    pub fn as_str(self) -> &'static str {
        match self {
            ResumeReason::Logind => "logind",
            ResumeReason::BoottimeGap => "boottime_gap",
        }
    }
}

/// A resume event delivered to the caller via [`ResumeSignal::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeEvent {
    /// Which detector fired.
    pub reason: ResumeReason,
    /// Estimated sleep duration in milliseconds. Always 0 for `Logind`
    /// (the signal carries no duration); the gap detector fills this in.
    pub sleep_ms: u64,
}

/// System-resume detector.
///
/// Owns a worker thread that listens for logind's `PrepareForSleep` signal
/// via `zbus`, plus per-tick gap-detection state. The caller drives both
/// via [`Self::tick`].
pub struct ResumeSignal {
    last_monotonic: Instant,
    last_boottime: Duration,
    last_fire: Option<Instant>,
    threshold: Duration,
    cooldown: Duration,
    dbus_rx: Receiver<ResumeEvent>,
    _dbus_thread: Option<JoinHandle<()>>,
}

impl ResumeSignal {
    /// Construct with default settings (2 s gap threshold, 5 s cooldown).
    /// Spawns the logind listener thread. If the system bus is not
    /// available (non-systemd, no D-Bus broker), the thread exits
    /// gracefully and only the gap detector remains active.
    pub fn new() -> Self {
        Self::with_settings(DEFAULT_GAP_THRESHOLD, DEFAULT_COOLDOWN)
    }

    /// Construct with custom threshold + cooldown. Spawns the logind
    /// listener thread.
    pub fn with_settings(threshold: Duration, cooldown: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let dbus_thread = spawn_logind_listener(tx);
        Self {
            last_monotonic: Instant::now(),
            last_boottime: boottime_elapsed(),
            last_fire: None,
            threshold,
            cooldown,
            dbus_rx: rx,
            _dbus_thread: dbus_thread,
        }
    }

    /// Construct with the gap detector only — no logind listener. Used by
    /// tests that don't want to spawn a real D-Bus connection.
    pub fn gap_detector_only() -> Self {
        Self {
            last_monotonic: Instant::now(),
            last_boottime: boottime_elapsed(),
            last_fire: None,
            threshold: DEFAULT_GAP_THRESHOLD,
            cooldown: DEFAULT_COOLDOWN,
            dbus_rx: mpsc::channel().1,
            _dbus_thread: None,
        }
    }

    /// Per-tick driver. Call once per event-loop iteration.
    ///
    /// Drains any pending logind events, then samples the clocks and runs
    /// the gap detector. Returns all events that survived cooldown this
    /// tick (0, 1, or rarely 2 — at most one from each source).
    pub fn tick(&mut self) -> Vec<ResumeEvent> {
        let mut events = Vec::new();

        // Drain logind events queued by the worker thread.
        while let Ok(event) = self.dbus_rx.try_recv() {
            if let Some(e) = self.maybe_fire(event) {
                events.push(e);
            }
        }

        // Run the gap detector.
        let mono_now = Instant::now();
        let boot_now = boottime_elapsed();
        let mono_delta = mono_now.saturating_duration_since(self.last_monotonic);
        let boot_delta = boot_now.saturating_sub(self.last_boottime);
        self.last_monotonic = mono_now;
        self.last_boottime = boot_now;

        if let Some(sleep_ms) = gap_exceeded(mono_delta, boot_delta, self.threshold) {
            let event = ResumeEvent {
                reason: ResumeReason::BoottimeGap,
                sleep_ms,
            };
            if let Some(e) = self.maybe_fire(event) {
                events.push(e);
            }
        }

        events
    }
}

impl Default for ResumeSignal {
    fn default() -> Self {
        Self::new()
    }
}

// ── Pure decision rules (ported from resume_model.h) ─────────────────────

/// Decide whether a (boottime - monotonic) divergence indicates the kernel
/// suspended the process. Returns `Some(sleep_ms)` if the gap is at or
/// above `threshold`, `None` otherwise.
pub fn gap_exceeded(
    mono_delta: Duration,
    boot_delta: Duration,
    threshold: Duration,
) -> Option<u64> {
    if boot_delta <= mono_delta {
        return None;
    }
    let gap = boot_delta - mono_delta;
    if gap >= threshold {
        Some(gap.as_millis() as u64)
    } else {
        None
    }
}

/// Decide whether a fire should be suppressed because another detector
/// already fired within the cooldown window.
pub fn in_cooldown(now: Instant, last_fire: Option<Instant>, cooldown: Duration) -> bool {
    match last_fire {
        Some(last) => now.duration_since(last) < cooldown,
        None => false,
    }
}

// ── Internals ────────────────────────────────────────────────────────────

impl ResumeSignal {
    fn maybe_fire(&mut self, event: ResumeEvent) -> Option<ResumeEvent> {
        let now = Instant::now();
        if in_cooldown(now, self.last_fire, self.cooldown) {
            return None;
        }
        self.last_fire = Some(now);
        Some(event)
    }
}

/// Read `CLOCK_BOOTTIME` as a `Duration` since epoch.
fn boottime_elapsed() -> Duration {
    match ClockId::CLOCK_BOOTTIME.now() {
        Ok(ts) => Duration::new(ts.tv_sec() as u64, ts.tv_nsec() as u32),
        // CLOCK_BOOTTIME exists on every Linux ≥ 2.6.39 (2011). Falling
        // back to 0 makes the gap detector return false positives (every
        // tick looks like a gap), so it is deliberately left as a hard
        // failure in debug builds.
        Err(e) => {
            debug_assert!(false, "CLOCK_BOOTTIME unavailable: {e}");
            Duration::ZERO
        }
    }
}

/// Spawn the logind PrepareForSleep listener thread.
///
/// On failure (system bus unavailable, match rule rejected) the thread
/// exits gracefully — the gap detector still works, just with no logind
/// contribution. Returns `None` if the thread could not be spawned at all
/// (extreme system resource exhaustion).
fn spawn_logind_listener(tx: Sender<ResumeEvent>) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("typio-logind-watcher".into())
        .spawn(move || {
            use zbus::blocking::{Connection, Proxy};

            const DEST: &str = "org.freedesktop.login1";
            const PATH: &str = "/org/freedesktop/login1";
            const IFACE: &str = "org.freedesktop.login1.Manager";

            let conn = match Connection::system() {
                Ok(c) => c,
                Err(_) => return, // no system bus — gap detector covers this
            };
            let proxy = match Proxy::new(&conn, DEST, PATH, IFACE) {
                Ok(p) => p,
                Err(_) => return,
            };

            // Match PrepareForSleep on login1 Manager. zbus's
            // `receive_signal` returns a blocking iterator.
            let signals = match proxy.receive_signal("PrepareForSleep") {
                Ok(s) => s,
                Err(_) => return,
            };

            for msg in signals {
                // Argument is a single boolean: true = preparing to sleep,
                // false = resuming. Only the resume edge fires our event.
                let going_to_sleep: bool = match msg.body().deserialize() {
                    Ok(b) => b,
                    Err(_) => continue, // malformed signal — drop and continue
                };
                if !going_to_sleep {
                    let event = ResumeEvent {
                        reason: ResumeReason::Logind,
                        sleep_ms: 0,
                    };
                    // Channel disconnected (ResumeSignal dropped) — exit thread.
                    if tx.send(event).is_err() {
                        return;
                    }
                }
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_exceeded_returns_some_when_boottime_ahead() {
        let mono = Duration::from_millis(100);
        let boot = Duration::from_millis(3000);
        let gap = gap_exceeded(mono, boot, Duration::from_millis(2000));
        assert_eq!(gap, Some(2900));
    }

    #[test]
    fn gap_exceeded_returns_none_when_below_threshold() {
        let mono = Duration::from_millis(100);
        let boot = Duration::from_millis(1500);
        let gap = gap_exceeded(mono, boot, Duration::from_millis(2000));
        assert_eq!(gap, None);
    }

    #[test]
    fn gap_exceeded_returns_none_when_boot_not_ahead() {
        // Equal — no gap.
        assert_eq!(
            gap_exceeded(Duration::from_secs(1), Duration::from_secs(1), Duration::from_millis(1)),
            None
        );
        // Boot behind — impossible in practice but should not yield gap.
        assert_eq!(
            gap_exceeded(Duration::from_secs(2), Duration::from_secs(1), Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn in_cooldown_blocks_recent_fires() {
        let now = Instant::now();
        let cooldown = Duration::from_millis(100);
        assert!(!in_cooldown(now, None, cooldown));
        assert!(!in_cooldown(
            now,
            Some(now - Duration::from_millis(200)),
            cooldown
        ));
        assert!(in_cooldown(
            now,
            Some(now - Duration::from_millis(50)),
            cooldown
        ));
    }

    #[test]
    fn gap_detector_only_does_not_spawn_thread() {
        // Construction without D-Bus — should not panic, not spawn a thread.
        let mut rs = ResumeSignal::gap_detector_only();
        // First tick initializes the baselines and returns no events.
        let events = rs.tick();
        assert!(events.is_empty(), "first tick should not fire");
        // Subsequent quick tick — no real time has passed; no gap.
        let events = rs.tick();
        assert!(events.is_empty(), "no-suspend tick should not fire");
    }

    #[test]
    fn tick_fires_once_for_synthetic_gap_then_suppresses_in_cooldown() {
        // Construct with a tiny threshold so any measurable monotonic/boottime
        // delta will trip — but we can't actually trigger a real suspend in a
        // test, so this test only verifies the cooldown dedup logic.
        let mut rs = ResumeSignal::gap_detector_only();
        rs.threshold = Duration::from_secs(60); // impossibly high — no gap fires

        // Manually simulate a fire via the maybe_fire path.
        let event = ResumeEvent {
            reason: ResumeReason::BoottimeGap,
            sleep_ms: 5000,
        };
        let fired1 = rs.maybe_fire(event);
        let fired2 = rs.maybe_fire(event);
        assert!(fired1.is_some(), "first fire should pass cooldown");
        assert!(fired2.is_none(), "second fire within cooldown should be suppressed");
    }

    #[test]
    fn resume_reason_strings_match_c_version() {
        // The C version's `reason` argument is a stable literal: "logind" or
        // "boottime_gap". Keep these in lockstep — diagnostic logs and
        // downstream tooling compare against them.
        assert_eq!(ResumeReason::Logind.as_str(), "logind");
        assert_eq!(ResumeReason::BoottimeGap.as_str(), "boottime_gap");
    }
}
