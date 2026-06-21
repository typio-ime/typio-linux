//! Startup health checks for desktop notifications.
//!
//! Port of `src/notify/health.c`. Two concerns:
//! - **Config-gated predicates** — whether each notification category is
//!   enabled, read from the config namespace `notifications.*`. These are pure
//!   over the config reads, which are abstracted behind [`HealthView`] so the
//!   logic is unit-testable without a live `TypioInstance`.
//! - **Startup health collector** — the registry/engine availability checks
//!   emitted at startup.
//!
//! Mirrors the C `startup_setting`/`startup_int_setting` helpers exactly,
//! including the "negative int falls back to default" rule.

/// Severity for a [`StartupIssue`]. Mirrors `TypioStartupIssueSeverity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupIssueSeverity {
    Warning = 0,
    Error = 1,
}

/// A single user-facing startup issue. Mirrors `TypioStartupIssue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupIssue {
    pub severity: StartupIssueSeverity,
    pub code: String,
    pub title: String,
    pub body: String,
}

impl StartupIssue {
    fn new(severity: StartupIssueSeverity, code: &str, title: &str, body: &str) -> Self {
        StartupIssue {
            severity,
            code: code.to_string(),
            title: title.to_string(),
            body: body.to_string(),
        }
    }
}

/// Read surface the health checks need. Implementations may back this with a
/// live `TypioInstance` (production) or a fake (tests).
pub trait HealthView {
    /// `typio_config_get_bool(key, default)`.
    fn config_bool(&self, key: &str, default: bool) -> bool;
    /// `typio_config_get_int(key, default)`. Should already clamp negatives to
    /// the default (mirrors `startup_int_setting`).
    fn config_int(&self, key: &str, default: i64) -> i64;
    /// Whether the engine registry is attached.
    fn registry_present(&self) -> bool;
    /// Active keyboard engine name, if any.
    fn active_keyboard(&self) -> Option<String>;
}

/// `typio_startup_notifications_enabled` / `typio_notifications_enabled`.
pub fn startup_notifications_enabled<V: HealthView + ?Sized>(view: &V) -> bool {
    view.config_bool("notifications.enable", true)
}

/// `typio_startup_checks_enabled`.
pub fn startup_checks_enabled<V: HealthView + ?Sized>(view: &V) -> bool {
    startup_notifications_enabled(view) && view.config_bool("notifications.startup_checks", true)
}

/// `typio_runtime_notifications_enabled`.
pub fn runtime_notifications_enabled<V: HealthView + ?Sized>(view: &V) -> bool {
    startup_notifications_enabled(view) && view.config_bool("notifications.runtime", true)
}

/// `typio_voice_notifications_enabled`.
pub fn voice_notifications_enabled<V: HealthView + ?Sized>(view: &V) -> bool {
    runtime_notifications_enabled(view) && view.config_bool("notifications.voice", true)
}

/// `typio_notification_cooldown_ms`.
pub fn notification_cooldown_ms<V: HealthView + ?Sized>(view: &V, default: u64) -> u64 {
    let v = view.config_int("notifications.cooldown_ms", default as i64);
    if v < 0 {
        default
    } else {
        v as u64
    }
}

/// `typio_startup_health_collect`. Returns the issue list; the C version writes
/// into a fixed-capacity array and returns the count, but returning a `Vec` is
/// the idiomatic Rust equivalent (callers can `.truncate(cap)` if they need to
/// mirror the bounded buffer behaviour).
pub fn startup_health_collect<V: HealthView + ?Sized>(view: &V) -> Vec<StartupIssue> {
    let mut issues = Vec::new();
    if !view.registry_present() {
        issues.push(StartupIssue::new(
            StartupIssueSeverity::Error,
            "engine-registry-missing",
            "Typio startup incomplete",
            "Engine registry is unavailable, so no input engine can be activated.",
        ));
        return issues;
    }
    if view.active_keyboard().is_none() {
        issues.push(StartupIssue::new(
            StartupIssueSeverity::Error,
            "no-active-keyboard-engine",
            "No keyboard engine is active",
            "Typio started without an active keyboard engine. Check \
             your engine build/install state.",
        ));
    }
    issues
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeView {
        bools: HashMap<String, bool>,
        ints: HashMap<String, i64>,
        registry: bool,
        active_kb: Option<String>,
    }

    impl Default for FakeView {
        fn default() -> Self {
            // The C defaults: enable=true, startup_checks=true, runtime=true,
            // voice=true. Seed them so tests only override what they care about.
            let mut bools = HashMap::new();
            bools.insert("notifications.enable".into(), true);
            bools.insert("notifications.startup_checks".into(), true);
            bools.insert("notifications.runtime".into(), true);
            bools.insert("notifications.voice".into(), true);
            FakeView {
                bools,
                ints: HashMap::new(),
                registry: true,
                active_kb: Some("basic".into()),
            }
        }
    }

    impl HealthView for FakeView {
        fn config_bool(&self, key: &str, default: bool) -> bool {
            self.bools.get(key).copied().unwrap_or(default)
        }
        fn config_int(&self, key: &str, default: i64) -> i64 {
            self.ints.get(key).copied().unwrap_or(default)
        }
        fn registry_present(&self) -> bool {
            self.registry
        }
        fn active_keyboard(&self) -> Option<String> {
            self.active_kb.clone()
        }
    }

    #[test]
    fn defaults_enable_every_category() {
        let v = FakeView::default();
        assert!(startup_notifications_enabled(&v));
        assert!(startup_checks_enabled(&v));
        assert!(runtime_notifications_enabled(&v));
        assert!(voice_notifications_enabled(&v));
    }

    #[test]
    fn disabling_master_enable_disables_everything() {
        let mut v = FakeView::default();
        v.bools.insert("notifications.enable".into(), false);
        assert!(!startup_notifications_enabled(&v));
        assert!(!startup_checks_enabled(&v));
        assert!(!runtime_notifications_enabled(&v));
        assert!(!voice_notifications_enabled(&v));
    }

    #[test]
    fn voice_requires_runtime_which_requires_master() {
        let mut v = FakeView::default();
        v.bools.insert("notifications.runtime".into(), false);
        assert!(!runtime_notifications_enabled(&v));
        assert!(!voice_notifications_enabled(&v));
        // Startup checks are independent of runtime and remain enabled.
        assert!(startup_checks_enabled(&v));
    }

    #[test]
    fn voice_flag_is_independent_gate() {
        let mut v = FakeView::default();
        v.bools.insert("notifications.voice".into(), false);
        assert!(runtime_notifications_enabled(&v));
        assert!(!voice_notifications_enabled(&v));
    }

    #[test]
    fn cooldown_uses_default_when_unset() {
        let v = FakeView::default();
        assert_eq!(notification_cooldown_ms(&v, 5000), 5000);
    }

    #[test]
    fn cooldown_reads_configured_value() {
        let mut v = FakeView::default();
        v.ints.insert("notifications.cooldown_ms".into(), 1500);
        assert_eq!(notification_cooldown_ms(&v, 5000), 1500);
    }

    #[test]
    fn cooldown_negative_falls_back_to_default() {
        let mut v = FakeView::default();
        v.ints.insert("notifications.cooldown_ms".into(), -1);
        assert_eq!(notification_cooldown_ms(&v, 5000), 5000);
    }

    #[test]
    fn health_collect_empty_when_healthy() {
        let v = FakeView::default();
        assert!(startup_health_collect(&v).is_empty());
    }

    #[test]
    fn health_collect_reports_missing_registry() {
        let v = FakeView { registry: false, ..Default::default() };
        let issues = startup_health_collect(&v);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, StartupIssueSeverity::Error);
        assert_eq!(issues[0].code, "engine-registry-missing");
    }

    #[test]
    fn health_collect_reports_no_active_keyboard() {
        let v = FakeView { active_kb: None, ..Default::default() };
        let issues = startup_health_collect(&v);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "no-active-keyboard-engine");
    }

    #[test]
    fn missing_registry_short_circuits_keyboard_check() {
        // Mirrors the C: registry-missing returns before the keyboard check.
        let v = FakeView {
            registry: false,
            active_kb: None,
            ..Default::default()
        };
        let issues = startup_health_collect(&v);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "engine-registry-missing");
    }
}
