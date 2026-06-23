//! Wayland frontend watchdog — port of `src/wayland/watchdog.c`.
//!
//! A background thread samples loop-stage progress at ~1 Hz while armed.
//! If the heartbeat, stage, and stage timestamp stay unchanged for a
//! non-restful stage for longer than the stuck threshold, the daemon is
//! considered hung and is killed with `SIGKILL`.
//!
//! The watchdog is disarmed when there is no focused input context, so an
//! idle daemon causes zero wakeups.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Coarse sample interval (ms). Mirrors `TYPIO_WL_WATCHDOG_SAMPLE_MS`.
const SAMPLE_MS: u64 = 1000;
/// Default stuck threshold (ms). Mirrors `TYPIO_WL_WATCHDOG_STUCK_MS`.
const STUCK_MS: u64 = 3000;
/// Stuck threshold for `LoopStage::Present` (ms). `vkQueuePresentKHR` on
/// Wayland can transiently block well past the default 3 s threshold when
/// the compositor falls behind releasing swapchain images, even under
/// MAILBOX. A single blocking FFI call cannot heartbeat, so the default
/// threshold kills a recovering panel rather than a deadlocked one. The
/// panel's `wl_surface.frame` throttle (see `panel_present_blocked`)
/// keeps the present rate at the compositor refresh rate so this block
/// should not occur in steady state; 15 s tolerates a residual transient
/// stall while still catching a genuine present deadlock eventually.
const PRESENT_STUCK_MS: u64 = 15_000;

/// Loop stage identifiers. The discriminants mirror `TypioWlLoopStage` in
/// `src/wayland/internal.h` so trace output lines up with the C version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum LoopStage {
    #[default]
    Idle = 0,
    PanelUpdate = 1,
    PrepareRead = 2,
    Flush = 3,
    Poll = 4,
    ReadEvents = 5,
    DispatchPending = 6,
    AuxIo = 7,
    Repeat = 8,
    ConfigReload = 9,
    Present = 10,
}

impl LoopStage {
    /// Stages where blocking indefinitely is legitimate. flux presents
    /// synchronously on the main thread (`flux_frame_present` →
    /// `vkQueuePresentKHR`); the panel's `wl_surface.frame` throttle
    /// keeps that call non-blocking by never out-pacing the compositor,
    /// but a stall in `Present` is still a genuine bug — the main loop
    /// cannot heartbeat through a single blocking FFI call — so
    /// `Present` is not restful and is killed once the per-stage
    /// threshold elapses.
    fn is_restful(&self) -> bool {
        matches!(self, LoopStage::Poll | LoopStage::Idle)
    }

    fn from_i32(value: i32) -> Self {
        match value {
            0 => LoopStage::Idle,
            1 => LoopStage::PanelUpdate,
            2 => LoopStage::PrepareRead,
            3 => LoopStage::Flush,
            4 => LoopStage::Poll,
            5 => LoopStage::ReadEvents,
            6 => LoopStage::DispatchPending,
            7 => LoopStage::AuxIo,
            8 => LoopStage::Repeat,
            9 => LoopStage::ConfigReload,
            10 => LoopStage::Present,
            _ => LoopStage::Idle,
        }
    }

    /// Per-stage stuck threshold. `Present` gets a longer window because
    /// `vkQueuePresentKHR` can block inside the WSI/driver for several
    /// seconds during compositor back-pressure and a single FFI call
    /// cannot heartbeat; other non-restful stages use the default.
    fn stuck_threshold_ms(&self) -> u64 {
        match self {
            LoopStage::Present => PRESENT_STUCK_MS,
            _ => STUCK_MS,
        }
    }
}

/// Watchdog handle. Dropping it stops the background thread.
pub struct Watchdog {
    inner: Arc<WatchdogInner>,
    handle: Option<JoinHandle<()>>,
}

struct WatchdogInner {
    stop: AtomicBool,
    armed: AtomicBool,
    heartbeat_ms: AtomicU64,
    stage_since_ms: AtomicU64,
    loop_stage: AtomicI32,
    lethal: AtomicBool,
    stall_detected: AtomicBool,
    epoch: Instant,
    cond: Condvar,
    mutex: Mutex<()>,
}

impl WatchdogInner {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn set_stage(&self, stage: LoopStage) {
        self.loop_stage.store(stage as i32, Ordering::SeqCst);
        self.stage_since_ms.store(self.now_ms(), Ordering::SeqCst);
    }

    fn heartbeat(&self) {
        let now = self.now_ms();
        self.heartbeat_ms.store(now, Ordering::SeqCst);
        self.stage_since_ms.store(now, Ordering::SeqCst);
    }
}

impl Watchdog {
    /// Start the watchdog thread. It blocks until armed.
    pub fn start() -> Self {
        let inner = Arc::new(WatchdogInner {
            stop: AtomicBool::new(false),
            armed: AtomicBool::new(false),
            heartbeat_ms: AtomicU64::new(0),
            stage_since_ms: AtomicU64::new(0),
            loop_stage: AtomicI32::new(LoopStage::Idle as i32),
            lethal: AtomicBool::new(true),
            stall_detected: AtomicBool::new(false),
            epoch: Instant::now(),
            cond: Condvar::new(),
            mutex: Mutex::new(()),
        });

        let thread_inner = Arc::clone(&inner);
        let handle = thread::spawn(move || watchdog_thread(thread_inner));

        Self {
            inner,
            handle: Some(handle),
        }
    }

    /// Stop the watchdog thread and wait for it to exit.
    pub fn stop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.cond.notify_all();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Arm or disarm the watchdog. When disarmed the thread sleeps with
    /// zero wakeups.
    pub fn set_armed(&self, armed: bool) {
        self.inner.armed.store(armed, Ordering::SeqCst);
        self.inner.cond.notify_all();
    }

    /// Record that the main loop is making progress.
    pub fn heartbeat(&self) {
        self.inner.heartbeat();
    }

    /// Set the current loop stage and refresh the stage timestamp.
    pub fn set_stage(&self, stage: LoopStage) {
        self.inner.set_stage(stage);
    }

    /// Convenience: heartbeat and return to `Idle`.
    pub fn stage_done(&self) {
        self.inner.heartbeat();
        self.inner.set_stage(LoopStage::Idle);
    }

    /// In unit tests, set `lethal = false` so a simulated stall is recorded
    /// instead of killing the process.
    #[cfg(test)]
    pub fn set_lethal(&self, lethal: bool) {
        self.inner.lethal.store(lethal, Ordering::SeqCst);
    }

    /// Test hook: true if a stall was detected while non-lethal.
    #[cfg(test)]
    pub fn stall_detected(&self) -> bool {
        self.inner.stall_detected.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watchdog_thread(inner: Arc<WatchdogInner>) {
    let mut last_heartbeat_ms: u64 = 0;
    let mut last_stage: i32 = LoopStage::Idle as i32;
    let mut last_stage_since_ms: u64 = 0;

    let mut guard = inner.mutex.lock().unwrap();

    while !inner.stop.load(Ordering::SeqCst) {
        // Disarmed: block until armed or stopped.
        while !inner.armed.load(Ordering::SeqCst) && !inner.stop.load(Ordering::SeqCst) {
            guard = inner.cond.wait(guard).unwrap();
        }
        if inner.stop.load(Ordering::SeqCst) {
            break;
        }

        // Armed: sample at SAMPLE_MS, but wake immediately if disarmed/stopped.
        let wait_result = inner
            .cond
            .wait_timeout(guard, Duration::from_millis(SAMPLE_MS))
            .unwrap();
        guard = wait_result.0;

        if inner.stop.load(Ordering::SeqCst) {
            break;
        }
        if !inner.armed.load(Ordering::SeqCst) {
            continue;
        }

        let heartbeat_ms = inner.heartbeat_ms.load(Ordering::SeqCst);
        let stage = inner.loop_stage.load(Ordering::SeqCst);
        let stage_since_ms = inner.stage_since_ms.load(Ordering::SeqCst);

        let unchanged = heartbeat_ms == last_heartbeat_ms
            && stage == last_stage
            && stage_since_ms == last_stage_since_ms;

        let stage_enum = LoopStage::from_i32(stage);
        if unchanged && !stage_enum.is_restful() {
            let now = inner.now_ms();
            let stuck_ms = now.saturating_sub(heartbeat_ms);
            if stuck_ms >= stage_enum.stuck_threshold_ms() {
                tracing::warn!(
                    target: "typio.watchdog",
                    stuck_ms,
                    stage = ?stage_enum,
                    "loop stuck"
                );
                if inner.lethal.load(Ordering::SeqCst) {
                    unsafe {
                        libc::kill(libc::getpid(), libc::SIGKILL);
                    }
                    break;
                } else {
                    inner.stall_detected.store(true, Ordering::SeqCst);
                    // Keep running so tests can observe the flag.
                }
            }
        }

        last_heartbeat_ms = heartbeat_ms;
        last_stage = stage;
        last_stage_since_ms = stage_since_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disarmed_and_stops_cleanly() {
        let mut wd = Watchdog::start();
        assert!(!wd.inner.armed.load(Ordering::SeqCst));
        wd.stop();
    }

    #[test]
    fn arming_and_disarming_wakes_thread() {
        let wd = Watchdog::start();
        wd.set_armed(true);
        std::thread::sleep(Duration::from_millis(50));
        assert!(wd.inner.armed.load(Ordering::SeqCst));
        wd.set_armed(false);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!wd.inner.armed.load(Ordering::SeqCst));
    }

    #[test]
    fn stall_is_detected_when_non_lethal() {
        let wd = Watchdog::start();
        wd.set_lethal(false);
        wd.set_armed(true);
        wd.set_stage(LoopStage::PanelUpdate);
        // Keep heartbeat older than STUCK_MS by not calling heartbeat().
        std::thread::sleep(Duration::from_millis(STUCK_MS + SAMPLE_MS + 100));
        assert!(wd.stall_detected(), "expected stall to be detected");
    }

    #[test]
    fn restful_stage_does_not_trigger_stall() {
        let wd = Watchdog::start();
        wd.set_lethal(false);
        wd.set_armed(true);
        wd.set_stage(LoopStage::Poll);
        std::thread::sleep(Duration::from_millis(STUCK_MS + SAMPLE_MS + 100));
        assert!(!wd.stall_detected());
    }

    #[test]
    fn heartbeat_prevents_stall() {
        let wd = Watchdog::start();
        wd.set_lethal(false);
        wd.set_armed(true);
        wd.set_stage(LoopStage::PanelUpdate);
        for _ in 0..((STUCK_MS + SAMPLE_MS) / 100 + 2) {
            wd.heartbeat();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!wd.stall_detected());
    }

    #[test]
    fn present_stage_is_not_restful() {
        // flux_frame_present runs synchronously on the main loop and calls
        // vkQueuePresentKHR directly. A stall here is a genuine hang (the
        // panel's wl_surface.frame throttle should prevent it in steady
        // state), so Present must stay non-restful.
        assert!(!LoopStage::Present.is_restful());
        assert!(LoopStage::Poll.is_restful());
    }
}
