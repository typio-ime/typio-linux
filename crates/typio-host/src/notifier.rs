//! Desktop notification transport via the FreeDesktop Notifications D-Bus API.
//!
//! Phase 5 port of `src/notify/notifications.{h,c}` (251 lines of C).
//! Calls `org.freedesktop.Notifications.Notify` on the user session bus.
//! zbus replaces the C version's `sd-bus`/`libsystemd` dependency for
//! this codepath.
//!
//! ## Two-layer API
//!
//! - [`Notifier::send`] — fire-and-forget a single notification.
//! - [`Notifier::send_coalesced`] — rate-limit by `key`: if the same
//!   key was used successfully within the cooldown window, the call is
//!   silently dropped (returns `Ok(())` so the caller can't tell
//!   whether the notification was sent or suppressed).
//!
//! The rate limiter is a 16-entry ring buffer, matching the C
//! `TYPIO_NOTIFY_RECENT_CAP`. Keys longer than 96 bytes are truncated.

// The zbus #[proxy] macro generates a Notify trait method with 9 args
// (matches the FreeDesktop spec). Clippy's default threshold is 7; we
// can't reduce the spec'd arg count.
#![allow(clippy::too_many_arguments)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::proxy;

/// FreeDesktop notification urgency level. Matches the byte stored in
/// the `urgency` hint of the `Notify` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Urgency {
    /// Low — informational.
    Low = 0,
    /// Normal — default.
    #[default]
    Normal = 1,
    /// Critical — does not expire by default.
    Critical = 2,
}

/// Connection to the session-bus notification service.
///
/// Cheap to share via `Arc<Notifier>`; the underlying zbus connection
/// is itself shareable but the `recent` ring buffer needs a `Mutex`.
/// Deliberately NOT `Clone` (the C version uses a single instance too).
#[derive(Debug)]
pub struct Notifier {
    conn: Connection,
    recent: Mutex<RecentKeys>,
}

/// Per-key send-history ring buffer (port of `TypioRecentNotification[16]`
/// in the C version).
#[derive(Debug, Default)]
struct RecentKeys {
    /// `(key, last_sent_at)` pairs. Capped at 16; oldest evicted.
    entries: VecDeque<(String, Instant)>,
}

impl RecentKeys {
    const CAP: usize = 16;
    const KEY_TRUNC: usize = 96;

    fn normalize(key: &str) -> String {
        if key.len() <= Self::KEY_TRUNC {
            key.to_string()
        } else {
            // Match the C `snprintf(buf, 96, "%s", key)` truncation.
            key.chars().take(Self::KEY_TRUNC).collect()
        }
    }

    /// Returns `true` if `key` was sent within `cooldown`, and updates
    /// the timestamp if not.
    fn check_and_record(&mut self, key: &str, cooldown: Duration) -> bool {
        if key.is_empty() || cooldown.is_zero() {
            return false;
        }
        let normalized = Self::normalize(key);
        let now = Instant::now();
        if let Some((_, ts)) = self.entries.iter_mut().find(|(k, _)| *k == normalized) {
            if now.saturating_duration_since(*ts) < cooldown {
                return true;
            }
            *ts = now;
            return false;
        }
        // New key — record.
        if self.entries.len() >= Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back((normalized, now));
        false
    }
}

/// A notification request ready to send. Builder pattern; use
/// [`Notification::new`] for defaults.
#[derive(Debug, Clone)]
pub struct Notification {
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    /// Application icon name (freedesktop icon theme). Defaults to
    /// `"typio-keyboard-symbolic"` like the C version.
    pub app_icon: String,
    /// Notification expire timeout. `0` means "never expires" (matches
    /// the C convention for critical urgency).
    pub expire_timeout: Duration,
}

impl Notification {
    /// Construct a notification with the given summary and default
    /// values for everything else.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            body: String::new(),
            urgency: Urgency::Normal,
            app_icon: "typio-keyboard-symbolic".to_string(),
            expire_timeout: Duration::from_secs(12),
        }
    }

    /// Set the body text.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Set the urgency level.
    pub fn urgency(mut self, urgency: Urgency) -> Self {
        self.urgency = urgency;
        self
    }

    /// Override the application icon name.
    pub fn app_icon(mut self, icon: impl Into<String>) -> Self {
        self.app_icon = icon.into();
        self
    }

    /// Override the expire timeout.
    pub fn expire_timeout(mut self, timeout: Duration) -> Self {
        self.expire_timeout = timeout;
        self
    }
}

/// zbus proxy for `org.freedesktop.Notifications.Notify`. The signature
/// is fixed by the FreeDesktop spec; we don't bother modelling actions
/// or hint types beyond `urgency` because typio doesn't use them.
#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait FreedesktopNotifications {
    /// Invoke the Notify method. Returns the allocated notification id
    /// (we discard it because typio never replaces an existing one).
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

impl Notifier {
    /// Connect to the session bus. Fails if no D-Bus broker is available
    /// (rare on a desktop session).
    pub fn connect() -> zbus::Result<Self> {
        let conn = Connection::session()?;
        Ok(Self {
            conn,
            recent: Mutex::new(RecentKeys::default()),
        })
    }

    /// Send a notification immediately. No rate limiting.
    ///
    /// Errors are returned verbatim. Most desktops always accept the
    /// call; a returned `Err` usually means the user has no
    /// notification daemon running.
    pub fn send(&self, n: &Notification) -> zbus::Result<()> {
        let proxy = FreedesktopNotificationsProxyBlocking::new(&self.conn)?;
        // Critical urgency → expire_timeout = 0 (never expires).
        let expire_ms: i32 = if n.urgency == Urgency::Critical {
            0
        } else {
            n.expire_timeout.as_millis().min(i32::MAX as u128) as i32
        };
        let mut hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
            std::collections::HashMap::with_capacity(1);
        hints.insert("urgency", zbus::zvariant::Value::U8(n.urgency as u8));
        let _id = proxy.notify(
            "Typio",
            /* replaces_id */ 0,
            &n.app_icon,
            &n.summary,
            &n.body,
            /* actions */ &[],
            hints,
            expire_ms,
        )?;
        Ok(())
    }

    /// Send a notification, suppressing duplicates.
    ///
    /// If `key` was used successfully within `cooldown`, the call is a
    /// no-op and returns `Ok(())`. Otherwise the notification is sent
    /// and `key` is recorded with the current time.
    pub fn send_coalesced(
        &self,
        key: &str,
        cooldown: Duration,
        n: &Notification,
    ) -> zbus::Result<()> {
        {
            let mut recent = self.recent.lock().expect("recent keys mutex poisoned");
            if recent.check_and_record(key, cooldown) {
                return Ok(());
            }
        }
        self.send(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_default_is_normal() {
        assert_eq!(Urgency::default(), Urgency::Normal);
        assert_eq!(Urgency::Normal as u8, 1);
        assert_eq!(Urgency::Low as u8, 0);
        assert_eq!(Urgency::Critical as u8, 2);
    }

    #[test]
    fn recent_keys_check_and_record_dedupes_within_cooldown() {
        let mut r = RecentKeys::default();
        let cd = Duration::from_secs(60);
        // First send: not limited, recorded.
        assert!(!r.check_and_record("engine.failed", cd));
        // Immediate second send: limited.
        assert!(r.check_and_record("engine.failed", cd));
    }

    #[test]
    fn recent_keys_distinguish_keys_are_independent() {
        let mut r = RecentKeys::default();
        let cd = Duration::from_secs(60);
        assert!(!r.check_and_record("a", cd));
        // Different key → not limited.
        assert!(!r.check_and_record("b", cd));
        // First key still within cooldown.
        assert!(r.check_and_record("a", cd));
    }

    #[test]
    fn recent_keys_empty_key_is_never_limited() {
        let mut r = RecentKeys::default();
        // Empty key: rate-limit logic is bypassed.
        assert!(!r.check_and_record("", Duration::from_secs(60)));
        assert!(!r.check_and_record("", Duration::from_secs(60)));
    }

    #[test]
    fn recent_keys_zero_cooldown_is_never_limited() {
        let mut r = RecentKeys::default();
        assert!(!r.check_and_record("a", Duration::ZERO));
        assert!(!r.check_and_record("a", Duration::ZERO));
    }

    #[test]
    fn recent_keys_eviction_keeps_latest_16() {
        let mut r = RecentKeys::default();
        let cd = Duration::from_secs(60);
        // Push 18 distinct keys; the ring buffer should cap at 16.
        for i in 0..18 {
            let _ = r.check_and_record(&format!("k{i}"), cd);
        }
        assert_eq!(r.entries.len(), RecentKeys::CAP);
        // The oldest two ("k0", "k1") should have been evicted.
        assert!(r.entries.iter().all(|(k, _)| !k.starts_with("k0")));
        // "k17" should be the newest entry.
        assert_eq!(r.entries.back().unwrap().0, "k17");
    }

    #[test]
    fn recent_keys_truncates_long_keys() {
        let mut r = RecentKeys::default();
        let cd = Duration::from_secs(60);
        let long = "x".repeat(200);
        // Truncation to 96 bytes — the second call should be limited
        // because the truncated form matches.
        assert!(!r.check_and_record(&long, cd));
        assert!(r.check_and_record(&long, cd));
    }

    #[test]
    fn notification_builder_sets_defaults_and_overrides() {
        let n = Notification::new("hello")
            .body("world")
            .urgency(Urgency::Critical)
            .app_icon("custom")
            .expire_timeout(Duration::from_secs(5));
        assert_eq!(n.summary, "hello");
        assert_eq!(n.body, "world");
        assert_eq!(n.urgency, Urgency::Critical);
        assert_eq!(n.app_icon, "custom");
        assert_eq!(n.expire_timeout, Duration::from_secs(5));
    }

    #[test]
    fn notification_default_icon_matches_typio_keyboard_symbolic() {
        let n = Notification::new("test");
        assert_eq!(n.app_icon, "typio-keyboard-symbolic");
    }
}
