//! Capped-exponential backoff schedule for Wayland reconnect.
//!
//! Phase 3c port of `src/engine/backoff.{h,c}` (43 + 25 lines of C).
//! Pure functions — no clocks, no state — so the schedule can be
//! unit-tested without sleeping or a live display. The reconnect loop
//! (once it is ported) consults this for each attempt's delay and the
//! give-up cutoff.

/// Default base delay (first retry waits this long). Matches the C
/// constant `TYPIO_WL_RECONNECT_BASE_DELAY_MS`.
pub const DEFAULT_BASE_DELAY_MS: u32 = 250;

/// Default maximum single-attempt delay. Matches
/// `TYPIO_WL_RECONNECT_MAX_DELAY_MS`.
pub const DEFAULT_MAX_DELAY_MS: u32 = 8000;

/// Default maximum number of attempts before giving up. Matches
/// `TYPIO_WL_RECONNECT_MAX_ATTEMPTS`. A compositor that never returns
/// lets the daemon exit and hand off to the service manager instead of
/// spinning forever.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 12;

/// Delay before reconnect attempt `attempt` (0-based):
/// `base_delay * 2^attempt`, clamped to `max_delay_ms`.
///
/// The doubling is computed shift-safe so a large `attempt` cannot
/// overflow; once `base << shift` would exceed the max we just clamp.
pub fn reconnect_delay_ms(attempt: u32) -> u32 {
    reconnect_delay_ms_with(attempt, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_DELAY_MS)
}

/// Whether attempt `attempt` (0-based) should still be tried.
pub fn should_retry(attempt: u32) -> bool {
    attempt < DEFAULT_MAX_ATTEMPTS
}

/// Configurable variant of [`reconnect_delay_ms`]. Exposed for tests and
/// for callers that want non-default schedule parameters.
pub fn reconnect_delay_ms_with(attempt: u32, base_ms: u32, max_ms: u32) -> u32 {
    // Cap the shift before computing 2^attempt so the multiply cannot
    // overflow; once base<<shift would exceed the max we just clamp.
    if attempt >= 16 {
        return max_ms;
    }
    let delay = base_ms.checked_shl(attempt);
    match delay {
        Some(d) if d <= max_ms && d >= base_ms => d,
        // Either the shift overflowed, the delay exceeded the cap, or
        // (theoretically) wrapped below the base. All three cases clamp.
        _ => max_ms,
    }
}

/// Configurable variant of [`should_retry`].
pub fn should_retry_with(attempt: u32, max_attempts: u32) -> bool {
    attempt < max_attempts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_starts_at_base_and_doubles() {
        assert_eq!(reconnect_delay_ms(0), 250);
        assert_eq!(reconnect_delay_ms(1), 500);
        assert_eq!(reconnect_delay_ms(2), 1000);
        assert_eq!(reconnect_delay_ms(3), 2000);
        assert_eq!(reconnect_delay_ms(4), 4000);
        assert_eq!(reconnect_delay_ms(5), 8000);
    }

    #[test]
    fn schedule_clamps_at_max_delay() {
        // 2^5 * 250 = 8000 = max; 2^6 * 250 = 16000, clamps to 8000.
        assert_eq!(reconnect_delay_ms(5), 8000);
        assert_eq!(reconnect_delay_ms(6), 8000);
        assert_eq!(reconnect_delay_ms(7), 8000);
        assert_eq!(reconnect_delay_ms(100), 8000);
    }

    #[test]
    fn shift_overflow_does_not_panic_for_huge_attempt() {
        // attempt=32 would shift u32 out of range. The C version has the
        // same guard (attempt >= 16 → clamp) which is the safer cap.
        assert_eq!(reconnect_delay_ms(u32::MAX), DEFAULT_MAX_DELAY_MS);
    }

    #[test]
    fn should_retry_returns_true_within_max_attempts() {
        for a in 0..DEFAULT_MAX_ATTEMPTS {
            assert!(should_retry(a), "attempt {a} should be retryable");
        }
    }

    #[test]
    fn should_retry_returns_false_at_and_above_max_attempts() {
        assert!(!should_retry(DEFAULT_MAX_ATTEMPTS));
        assert!(!should_retry(DEFAULT_MAX_ATTEMPTS + 1));
        assert!(!should_retry(u32::MAX));
    }

    #[test]
    fn configurable_schedule_uses_custom_params() {
        // 100ms base, 4 attempts, 1s cap
        assert_eq!(reconnect_delay_ms_with(0, 100, 1000), 100);
        assert_eq!(reconnect_delay_ms_with(1, 100, 1000), 200);
        assert_eq!(reconnect_delay_ms_with(2, 100, 1000), 400);
        assert_eq!(reconnect_delay_ms_with(3, 100, 1000), 800);
        assert_eq!(reconnect_delay_ms_with(4, 100, 1000), 1000); // would be 1600, clamped
        assert!(should_retry_with(3, 4));
        assert!(!should_retry_with(4, 4));
    }
}
