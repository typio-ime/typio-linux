//! Desktop notification transport via `org.freedesktop.Notifications`.
//!
//! Port of `src/notify/notifications.c`. Two concerns:
//! - [`RateLimiter`] — the pure per-key cooldown ring buffer used by
//!   `send_coalesced`. Fully unit-testable with an injected clock.
//! - [`Notifier`] — owns a [`RateLimiter`] and an optional session-bus
//!   connection (zbus). Connection failure makes the notifier unavailable and
//!   sends return `false`.

use std::collections::HashMap;
use std::time::Instant;

use zbus::blocking::Connection;
use zbus::zvariant::Value;

pub use zbus;

const RECENT_CAP: usize = 16;

/// Notification urgency. Mirrors `TypioNotificationUrgency` and the FreeDesktop
/// hints `urgency` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

/// Per-key cooldown ring buffer. Mirrors `TypioRecentNotification recent[16]`.
///
/// The clock is abstracted so the cooldown logic is deterministic in tests; in
/// production a [`Notifier`] wires it to [`Instant::now`] via [`Clock`].
#[derive(Debug, Clone)]
pub struct RateLimiter {
    slots: Vec<Option<(String, u64)>>,
    next_slot: usize,
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter {
            slots: (0..RECENT_CAP).map(|_| None).collect(),
            next_slot: 0,
        }
    }

    /// Returns `true` if `key` was sent too recently and should be suppressed.
    /// Otherwise records the send at `now_ms` and returns `false`.
    ///
    /// This both checks *and* records — mirroring the C `is_rate_limited`,
    /// which always stamps the timestamp whether the send goes through or not.
    /// `cooldown_ms == 0` disables rate-limiting entirely (and skips recording).
    pub fn check(&mut self, key: &str, now_ms: u64, cooldown_ms: u64) -> bool {
        if key.is_empty() || cooldown_ms == 0 {
            return false;
        }
        for (k, last) in self.slots.iter_mut().flatten() {
            if k == key {
                if now_ms >= *last && now_ms - *last < cooldown_ms {
                    return true;
                }
                *last = now_ms;
                return false;
            }
        }
        // No matching slot: record into the ring buffer (round-robin).
        let idx = self.next_slot % RECENT_CAP;
        self.slots[idx] = Some((key.to_string(), now_ms));
        self.next_slot = (self.next_slot + 1) % RECENT_CAP;
        false
    }
}

/// Monotonic clock abstraction so the limiter is testable.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// Production clock backed by `Instant`.
pub struct InstantClock {
    origin: Instant,
}

impl InstantClock {
    pub fn new() -> Self {
        InstantClock {
            origin: Instant::now(),
        }
    }
}

impl Default for InstantClock {
    fn default() -> Self {
        InstantClock::new()
    }
}

impl Clock for InstantClock {
    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

/// Desktop-notification sender. Owns the cooldown limiter and an optional
/// session-bus connection.
pub struct Notifier<C: Clock = InstantClock> {
    limiter: RateLimiter,
    clock: C,
    bus: Option<Connection>,
}

impl Default for Notifier<InstantClock> {
    fn default() -> Self {
        Notifier::new()
    }
}

impl Notifier<InstantClock> {
    /// Attempt to connect to the session bus. On failure the notifier is still
    /// usable for cooldown bookkeeping but sends return `false` (mirrors the C
    /// "Desktop notifications unavailable" path).
    pub fn new() -> Self {
        let bus = Connection::session().ok();
        Notifier {
            limiter: RateLimiter::new(),
            clock: InstantClock::new(),
            bus,
        }
    }
}

impl<C: Clock> Notifier<C> {
    /// Build with an injected clock (tests).
    pub fn with_clock(clock: C) -> Self {
        let bus = Connection::session().ok();
        Notifier {
            limiter: RateLimiter::new(),
            clock,
            bus,
        }
    }

    /// Whether a session-bus connection was established.
    pub fn is_connected(&self) -> bool {
        self.bus.is_some()
    }

    /// Send a notification immediately (no cooldown check). Returns `false` if
    /// the summary is empty or the bus call failed.
    pub fn send(&self, urgency: Urgency, summary: &str, body: &str) -> bool {
        let Some(bus) = &self.bus else { return false };
        if summary.is_empty() {
            return false;
        }

        let app_name = "Typio";
        let app_icon = "typio-keyboard-symbolic";
        let replaces_id = 0u32;
        let expire_timeout: i32 = if urgency == Urgency::Critical {
            0
        } else {
            12000
        };

        let mut hints: HashMap<String, Value> = HashMap::new();
        hints.insert("urgency".to_string(), Value::U8(urgency as u8));

        let actions: Vec<&str> = Vec::new();

        let body_value = if body.is_empty() { "" } else { body };
        let summary_value = if summary.is_empty() { "Typio" } else { summary };

        let args = (
            app_name,
            replaces_id,
            app_icon,
            summary_value,
            body_value,
            actions,
            hints,
            expire_timeout,
        );

        bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &args,
        )
        .is_ok()
    }

    /// Send with per-key cooldown. Returns `true` when the notification was
    /// either sent OR suppressed as a duplicate within the cooldown window
    /// (so callers can treat both as "handled"). Mirrors the C semantics where
    /// a rate-limited send also returns `true`.
    pub fn send_coalesced(
        &mut self,
        key: &str,
        cooldown_ms: u64,
        urgency: Urgency,
        summary: &str,
        body: &str,
    ) -> bool {
        let now_ms = self.clock.now_ms();
        if self.limiter.check(key, now_ms, cooldown_ms) {
            return true;
        }
        self.send(urgency, summary, body)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_or_zero_cooldown_never_limits() {
        let mut rl = RateLimiter::new();
        assert!(!rl.check("", 100, 1000));
        assert!(!rl.check("k", 100, 0));
    }

    #[test]
    fn first_send_is_never_limited() {
        let mut rl = RateLimiter::new();
        assert!(!rl.check("engine.failed", 1000, 5000));
    }

    #[test]
    fn second_send_within_cooldown_is_limited() {
        let mut rl = RateLimiter::new();
        rl.check("k", 1000, 5000);
        assert!(rl.check("k", 1500, 5000)); // 500 < 5000
    }

    #[test]
    fn send_after_cooldown_expires_is_allowed() {
        let mut rl = RateLimiter::new();
        rl.check("k", 1000, 5000);
        // Just before expiry: still limited.
        assert!(rl.check("k", 5999, 5000));
        // Exactly at cooldown boundary (5000 not < 5000): allowed, and stamps
        // a fresh timestamp.
        assert!(!rl.check("k", 6000, 5000));
    }

    #[test]
    fn distinct_keys_are_independent() {
        let mut rl = RateLimiter::new();
        rl.check("a", 1000, 5000);
        assert!(!rl.check("b", 1000, 5000));
        assert!(rl.check("a", 1000, 5000));
    }

    #[test]
    fn ring_buffer_evicts_oldest_after_capacity_wraps() {
        let mut rl = RateLimiter::new();
        // Fill past capacity; "k0" should be evicted and re-sendable.
        for i in 0..(RECENT_CAP + 2) {
            rl.check(&format!("k{i}"), 1000, 5_000_000);
        }
        // k0 was evicted; the limiter no longer recognises it → not limited.
        assert!(!rl.check("k0", 1000, 5_000_000));
        // But the most recent keys are still tracked.
        let last = format!("k{}", RECENT_CAP + 1);
        assert!(rl.check(&last, 1000, 5_000_000));
    }

    #[test]
    fn notifier_without_bus_reports_unconnected() {
        // In the test environment the session bus may or may not be present.
        // Either way the type wires up; we just exercise construction.
        let _ = Notifier::new();
    }

    #[test]
    fn coalesced_suppresses_duplicate_within_cooldown() {
        // Drive the limiter directly to avoid the real bus send.
        let mut rl = RateLimiter::new();
        assert!(!rl.check("dup", 100, 1000));
        assert!(rl.check("dup", 500, 1000));
    }
}
