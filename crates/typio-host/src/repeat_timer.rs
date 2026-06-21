//! Keyboard auto-repeat timer — pure mechanism.
//!
//! Phase 3c port of the *pure* parts of `src/wayland/keyboard/repeat.c`
//! (251 lines of C). The C file mixes pure timer arming with deep
//! coupling to the keyboard routing pipeline (TypioWlKeyboard struct,
//! xkb_state, TypioKeyTrackState enum, focus observations, virtual
//! keyboard forwarding). Only the pure mechanism is portable without
//! porting the entire keyboard subsystem; the dispatch logic waits for
//! the keyboard/router port.
//!
//! ## What this module ports
//!
//! - [`RepeatTimer`] — owns a Linux timerfd configured for an initial
//!   delay followed by a recurring interval. Exposes the raw fd for
//!   integration with any event loop (mirrors the C version's
//!   `repeat_timer_fd` field + `timerfd_settime` calls).
//! - [`should_repeat_for_modifiers`] — pure decision: don't auto-repeat
//!   when Ctrl/Alt/Super is held. Matches the C `keyboard_repeat_should_run`.
//!
//! ## What is NOT ported
//!
//! The C version's `typio_wl_keyboard_dispatch_repeat` is 130 lines of
//! deeply-coupled dispatch: it samples xkb_state, observes focus, checks
//! candidate guards, calls `typio_input_context_process_key`, and either
//! forwards via virtual keyboard or routes to the engine. All of that
//! needs the keyboard router state machine which hasn't been ported yet;
//! attempting to extract it standalone would produce an awkward stub.
//! Deferred to the keyboard/router port (Phase 4).

use std::io;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::time::Duration;

use nix::sys::time::TimeSpec;
use nix::sys::timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};

/// Bit flags for keyboard modifiers, mirroring the C `TYPIO_MOD_*`
/// constants used by the repeat gate. Defined here so the gate logic is
/// testable without the full xkbcommon integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const CAPSLOCK: Self = Self(1 << 1);
    pub const CTRL: Self = Self(1 << 2);
    pub const ALT: Self = Self(1 << 3);
    pub const SUPER: Self = Self(1 << 4);
    pub const NUMLOCK: Self = Self(1 << 5);

    /// True iff any of the given modifier bits is set.
    pub fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

/// Bit set of modifiers that suppress auto-repeat when held.
/// Matches the C macro `(TYPIO_MOD_CTRL | TYPIO_MOD_ALT | TYPIO_MOD_SUPER)`.
const REPEAT_SUPPRESSING_MODIFIERS: Modifiers =
    Modifiers(Modifiers::CTRL.0 | Modifiers::ALT.0 | Modifiers::SUPER.0);

/// Default initial delay before auto-repeat begins (matches X server
/// default; used until the compositor sends a `repeat_info` event).
pub const DEFAULT_DELAY: Duration = Duration::from_millis(600);

/// Default repeat rate in keys/sec, matching the X server default.
pub const DEFAULT_RATE: u32 = 30;

/// Resolve the auto-repeat delay and interval from the compositor's
/// `repeat_info` event (if any).
///
/// The input-method-v2 `repeat_info` event carries `(rate, delay)` where
/// `rate` is keys/sec and `delay` is milliseconds. A `rate` of `0` is the
/// protocol signal for "do not repeat"; this function returns `None` in
/// that case so the caller can skip arming the timer entirely. When the
/// compositor has not sent any `repeat_info` (`info == None`) the X-server
/// defaults ([`DEFAULT_DELAY`] + [`DEFAULT_RATE`]) are used.
pub fn resolve_repeat_params(compositor_info: Option<(i32, i32)>) -> Option<(Duration, Duration)> {
    match compositor_info {
        Some((rate, delay)) if rate > 0 => {
            let d = Duration::from_millis(delay.max(0) as u64);
            let i = RepeatTimer::interval_from_rate(rate as u32);
            Some((d, i))
        }
        Some(_) => None,
        None => Some((DEFAULT_DELAY, RepeatTimer::interval_from_rate(DEFAULT_RATE))),
    }
}

/// A keyboard-repeat timer.
///
/// Owns a Linux timerfd configured with an initial delay followed by a
/// recurring interval (the rate derived from `repeat_rate` in keys/sec).
/// Exposes the timer fd for integration with any external event loop.
pub struct RepeatTimer {
    timer: TimerFd,
    /// True iff the timer is currently armed. Tracked separately from
    /// the timerfd's kernel state so we can short-circuit
    /// [`Self::dispatch`] without a syscall.
    armed: bool,
}

impl RepeatTimer {
    /// Construct a disarmed timer.
    pub fn new() -> io::Result<Self> {
        let timer =
            TimerFd::new(ClockId::CLOCK_MONOTONIC, TimerFlags::TFD_NONBLOCK).map_err(nix_to_io)?;
        Ok(Self {
            timer,
            armed: false,
        })
    }

    /// The timer file descriptor. Add to your event loop with read interest.
    pub fn fd(&self) -> RawFd {
        self.timer.as_fd().as_raw_fd()
    }

    /// True iff the timer was last armed via [`Self::start`] and not
    /// subsequently stopped.
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Arm the timer with the given initial delay followed by a recurring
    /// `interval`. Subsequent dispatches fire once per `interval` until
    /// [`Self::stop`] is called.
    pub fn start(&mut self, delay: Duration, interval: Duration) -> io::Result<()> {
        let expiration = Expiration::IntervalDelayed(
            TimeSpec::from_duration(delay),
            TimeSpec::from_duration(interval),
        );
        self.timer
            .set(expiration, TimerSetTimeFlags::empty())
            .map_err(nix_to_io)?;
        self.armed = true;
        Ok(())
    }

    /// Disarm the timer. Safe to call on an already-disarmed timer;
    /// arming a timer with a zero `it_value` is the kernel-defined
    /// disarm semantic.
    pub fn stop(&mut self) -> io::Result<()> {
        // OneShot with zero duration disarms the timer (timerfd_settime(2)):
        // "Setting both fields of it_value to zero disarms the timer."
        let expiration = Expiration::OneShot(TimeSpec::from_duration(Duration::ZERO));
        self.timer
            .set(expiration, TimerSetTimeFlags::empty())
            .map_err(nix_to_io)?;
        self.armed = false;
        Ok(())
    }

    /// Compute the interval from a Wayland keyboard `repeat_rate`
    /// expressed in keys per second. Returns 1 ms minimum so a
    /// pathological high rate (e.g. 10000) does not produce a zero
    /// interval.
    pub fn interval_from_rate(repeat_rate: u32) -> Duration {
        if repeat_rate == 0 {
            // Caller should have checked; fall back to something sane
            // rather than dividing by zero.
            return Duration::from_millis(1000 / 30);
        }
        let ms = 1000 / repeat_rate;
        Duration::from_millis(ms.max(1) as u64)
    }
}

impl Default for RepeatTimer {
    fn default() -> Self {
        Self::new().expect("RepeatTimer::new should not fail under normal conditions")
    }
}

/// Pure decision: should auto-repeat fire for a keypress with these
/// modifiers held?
///
/// Returns false when any of Ctrl, Alt, or Super is held. Matches the C
/// `keyboard_repeat_should_run` predicate.
pub fn should_repeat_for_modifiers(modifiers: Modifiers) -> bool {
    !modifiers.intersects(REPEAT_SUPPRESSING_MODIFIERS)
}

fn nix_to_io(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_intersect_works() {
        assert!(Modifiers::CTRL.intersects(Modifiers(Modifiers::CTRL.0 | Modifiers::ALT.0)));
        assert!(!Modifiers::SHIFT.intersects(Modifiers::CTRL));
        assert!(!Modifiers::NONE.intersects(Modifiers::NONE));
    }

    #[test]
    fn should_repeat_returns_true_for_plain_keys() {
        assert!(should_repeat_for_modifiers(Modifiers::NONE));
        assert!(should_repeat_for_modifiers(Modifiers::SHIFT));
        assert!(should_repeat_for_modifiers(Modifiers::CAPSLOCK));
        assert!(should_repeat_for_modifiers(Modifiers::NUMLOCK));
        assert!(should_repeat_for_modifiers(Modifiers(
            Modifiers::SHIFT.0 | Modifiers::CAPSLOCK.0
        )));
    }

    #[test]
    fn should_repeat_returns_false_when_ctrl_alt_or_super_held() {
        assert!(!should_repeat_for_modifiers(Modifiers::CTRL));
        assert!(!should_repeat_for_modifiers(Modifiers::ALT));
        assert!(!should_repeat_for_modifiers(Modifiers::SUPER));
        // Any combination that includes a suppressor still suppresses.
        assert!(!should_repeat_for_modifiers(Modifiers(
            Modifiers::SHIFT.0 | Modifiers::CTRL.0
        )));
        assert!(!should_repeat_for_modifiers(Modifiers(
            Modifiers::CTRL.0 | Modifiers::ALT.0 | Modifiers::SUPER.0
        )));
    }

    #[test]
    fn timer_starts_disarmed() {
        let t = RepeatTimer::new().unwrap();
        assert!(!t.is_armed());
        assert!(t.fd() >= 0);
    }

    #[test]
    fn timer_arm_and_disarm_toggles_flag() {
        let mut t = RepeatTimer::new().unwrap();
        t.start(Duration::from_millis(50), Duration::from_millis(20))
            .unwrap();
        assert!(t.is_armed());
        t.stop().unwrap();
        assert!(!t.is_armed());
        // Stop on an already-disarmed timer is a no-op.
        t.stop().unwrap();
        assert!(!t.is_armed());
    }

    #[test]
    fn interval_from_rate_clamps_below_one_ms() {
        // 30 Hz → ~33ms
        assert_eq!(
            RepeatTimer::interval_from_rate(30),
            Duration::from_millis(33)
        );
        // 1 Hz → 1000ms
        assert_eq!(
            RepeatTimer::interval_from_rate(1),
            Duration::from_millis(1000)
        );
        // 10000 Hz → clamps to 1ms minimum
        assert_eq!(
            RepeatTimer::interval_from_rate(10000),
            Duration::from_millis(1)
        );
        // 0 (caller bug) → falls back to a sane default rather than panicking
        assert_eq!(
            RepeatTimer::interval_from_rate(0),
            Duration::from_millis(1000 / 30)
        );
    }

    #[test]
    fn resolve_repeat_params_defaults_when_no_compositor_info() {
        let (delay, interval) = resolve_repeat_params(None).expect("default params");
        assert_eq!(delay, DEFAULT_DELAY);
        assert_eq!(interval, RepeatTimer::interval_from_rate(DEFAULT_RATE));
    }

    #[test]
    fn resolve_repeat_params_uses_compositor_info_when_present() {
        let (delay, interval) =
            resolve_repeat_params(Some((25, 500))).expect("compositor-provided params");
        assert_eq!(delay, Duration::from_millis(500));
        assert_eq!(interval, RepeatTimer::interval_from_rate(25));
    }

    #[test]
    fn resolve_repeat_params_returns_none_when_rate_is_zero() {
        // rate == 0 is the protocol signal for "do not repeat".
        assert!(resolve_repeat_params(Some((0, 500))).is_none());
    }

    #[test]
    fn resolve_repeat_params_clamps_negative_delay() {
        let (delay, _) =
            resolve_repeat_params(Some((30, -10))).expect("clamped-negative-delay params");
        assert_eq!(delay, Duration::ZERO);
    }
}
