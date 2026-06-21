//! On-screen status indicator: the transient `<badge> · <engine> · <mode>`
//! popup anchored near the caret.
//!
//! Port of `src/wayland/indicator.c` (deleted in `ae43b7f` when the C host
//! was retired). The indicator shares the candidate panel's
//! `zwp_input_popup_surface_v2` and is arbitrated by `PanelCoordinator`
//! (ADR-0017); this module owns only the gate state, label composition, and
//! the mode cache.
//!
//! ## Three show paths
//!
//! Per ADR-0018, the indicator has three trigger paths with different gate
//! semantics:
//!
//! | Path | Trigger | Gates |
//! |---|---|---|
//! | First-focus ([`Indicator::show_on_focus`]) | `FirstActivate` | enabled + salience `Notable` + acknowledged-recency |
//! | Reactivate ([`Indicator::show_on_reactivate`]) | `Reactivate` | enabled + salience `Notable` |
//! | Deliberate change ([`Indicator::show_for_state_change`]) | engine switch, mode change, profile toggle, `summon_indicator` | enabled only |
//!
//! The recency gate (suppress if user typed or saw the indicator within the
//! last 3 s) applies only to the first-focus path: a user mid-session who
//! moves to a new field via `REACTIVATE` may have just typed, but the new
//! caret's context can still differ enough to warrant a reminder. See the
//! "Re-activate while focused" section of `docs/explanation/wayland-input-method.md`.
//!
//! ## What this module does NOT own
//!
//! - The popup surface itself — `PanelCoordinator` plus `FluxPanel` own it.
//! - The anchor probe and timeout — `PanelCoordinator`.
//! - The auto-hide timerfd — the event loop in `app.rs`.
//!
//! The driver ([`Indicator::show_*`] caller) interprets each `Option<String>`
//! return value: `Some(label)` means "ask the coordinator to render this";
//! `None` means "gates suppressed, do nothing". When the coordinator accepts
//! and the popup actually maps, the driver calls [`Indicator::note_shown`] so
//! the recency gate's "last saw indicator" edge updates. This fixes a latent
//! bug in the C original, where a queued show that later flushed never
//! updated `last_indicator_ms` and never armed the auto-hide timer.

use std::time::{Duration, Instant};

use crate::language_display::language_badge;

/// Default auto-hide delay. Mirrors `TYPIO_INDICATOR_DEFAULT_DURATION_MS`.
const DEFAULT_DURATION_MS: u32 = 1500;
/// Min clamp for the configured duration. Below this the configured value is
/// replaced by the default (matches the C clamp behaviour: too-small →
/// default, not silently clamped to the floor).
const MIN_DURATION_MS: u32 = 100;
/// Max clamp for the configured duration.
const MAX_DURATION_MS: u32 = 10000;
/// Recency cooldown: if the user typed or saw the indicator within this
/// window, the first-focus show is suppressed. Mirrors
/// `TYPIO_INDICATOR_RECENT_INPUT_COOLDOWN_MS`.
const RECENT_INPUT_COOLDOWN: Duration = Duration::from_millis(3000);
/// Mode cache capacity. Engines remembered for fallback label composition
/// when a show fires without a fresh mode snapshot (e.g. the engine-switch
/// path, which has no live mode until the engine reports one).
const MODE_CACHE_CAPACITY: usize = 8;

/// Announcement salience. Mirrors libtypio's `TypioStatusSalience`. Governs
/// only the unprompted reveal; deliberate user actions always announce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Salience {
    /// Home-keyboard-like; never announce on the focus or reactivate paths.
    /// The default zero value of the libtypio enum.
    #[default]
    Quiet,
    /// Could surprise if typed into blind; eligible for unprompted reveal.
    Notable,
}

/// A read-only snapshot of the keyboard engine mode at a particular moment.
/// Mirrors the fields of libtypio's `TypioKeyboardEngineMode` that the
/// indicator consumes. Borrows its strings for zero-copy label building.
#[derive(Debug, Clone, Copy)]
pub struct EngineModeSnapshot<'a> {
    pub display_label: Option<&'a str>,
    pub salience: Salience,
}

impl EngineModeSnapshot<'_> {
    /// Empty snapshot — no display label, QUIET salience. Used by callers
    /// that have no live mode info (e.g. the engine-switch path that fires
    /// before the new engine has reported its mode).
    pub fn empty() -> Self {
        Self {
            display_label: None,
            salience: Salience::Quiet,
        }
    }
}

/// Indicator configuration. Production values are read from libtypio config
/// once at startup and on reload; tests use defaults.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorConfig {
    pub enabled: bool,
    pub duration: Duration,
}

impl Default for IndicatorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration: Duration::from_millis(DEFAULT_DURATION_MS as u64),
        }
    }
}

impl IndicatorConfig {
    /// Build from raw libtypio config values. `duration_ms` is clamped to
    /// the same range as the C original: too-small values fall back to the
    /// default (not silently clamped to the floor), too-large values clamp
    /// to the ceiling.
    pub fn from_values(enabled: bool, duration_ms: i64) -> Self {
        let mut ms = duration_ms.clamp(0, i64::MAX) as u64;
        if ms < MIN_DURATION_MS as u64 {
            ms = DEFAULT_DURATION_MS as u64;
        }
        if ms > MAX_DURATION_MS as u64 {
            ms = MAX_DURATION_MS as u64;
        }
        Self {
            enabled,
            duration: Duration::from_millis(ms),
        }
    }
}

/// Read-only view over the registry state needed to compose an indicator
/// label. The production implementation reads from the live `EngineRegistry`;
/// tests provide a fixture.
pub trait LabelSources {
    /// Active BCP-47 language tag (e.g. `"zh-Hans"`), if any.
    fn active_language_tag(&self) -> Option<&str>;
    /// Active keyboard engine identifier (e.g. `"rime"`), if any.
    fn active_engine_name(&self) -> Option<&str>;
    /// Localized engine display name (e.g. `"Rime"`), if any. Falls back to
    /// the raw engine name when absent.
    fn active_engine_display_name(&self) -> Option<&str>;
}

/// Compose the indicator label `<badge> · <engine> · <mode>` — joined by
/// literal `" · "` (space-middot-space) — dropping any empty segment.
/// Returns `None` when all segments are empty, so the caller treats that as
/// a no-op.
///
/// `mode_label` is the live mode display label (preferred); `cached_mode_label`
/// is the fallback when the live snapshot has no label — typically the
/// engine-switch path, which fires before the new engine reports its mode.
pub fn compose_label(
    language_tag: Option<&str>,
    engine_display_name: Option<&str>,
    engine_name: Option<&str>,
    mode_label: Option<&str>,
    cached_mode_label: Option<&str>,
) -> Option<String> {
    // Segment 1: language badge (compact glyph or uppercased primary subtag).
    let badge = language_tag.map(language_badge).unwrap_or_default();
    let lang_segment: Option<&str> = (!badge.is_empty()).then_some(badge.as_str());

    // Segment 2: engine display name, falling back to the raw engine name.
    let engine_segment = engine_display_name
        .filter(|s| !s.is_empty())
        .or_else(|| engine_name.filter(|s| !s.is_empty()));

    // Segment 3: mode label, preferring the live snapshot over the cache.
    let mode_segment = mode_label
        .filter(|s| !s.is_empty())
        .or_else(|| cached_mode_label.filter(|s| !s.is_empty()));

    let mut segs: Vec<&str> = Vec::with_capacity(3);
    if let Some(s) = lang_segment {
        segs.push(s);
    }
    if let Some(s) = engine_segment {
        segs.push(s);
    }
    if let Some(s) = mode_segment {
        segs.push(s);
    }

    if segs.is_empty() {
        None
    } else {
        Some(segs.join(" · "))
    }
}

/// Per-engine mode label cache. Remembers the most recent non-empty
/// `display_label` for each engine seen, so the engine-switch path (which
/// fires with `mode=None`) can still show the engine's last-known mode.
///
/// Move-to-front LRU with a hard capacity cap; lookups don't reorder (the
/// caller is responsible for `update` on cache misses).
#[derive(Debug, Default)]
pub struct ModeCache {
    entries: Vec<(String, String)>,
}

impl ModeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record or refresh the cached mode label for `engine_name`. No-op if
    /// either argument is empty (an empty label is not a useful fallback).
    pub fn update(&mut self, engine_name: &str, display_label: &str) {
        if engine_name.is_empty() || display_label.is_empty() {
            return;
        }
        self.entries.retain(|(k, _)| k != engine_name);
        self.entries
            .insert(0, (engine_name.to_string(), display_label.to_string()));
        if self.entries.len() > MODE_CACHE_CAPACITY {
            self.entries.truncate(MODE_CACHE_CAPACITY);
        }
    }

    /// Look up the cached mode label for `engine_name`, if any. Does not
    /// reorder — refresh with [`Self::update`].
    pub fn lookup(&self, engine_name: &str) -> Option<&str> {
        if engine_name.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|(k, _)| k == engine_name)
            .map(|(_, v)| v.as_str())
    }

    /// Number of cached engines.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True iff no engines are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Indicator state machine. Owns gate state (recency tracking, active flag,
/// mode cache) but NOT the popup surface, anchor probe, or hide timerfd.
#[derive(Debug, Default)]
pub struct Indicator {
    /// True iff a show has been accepted by the coordinator and not yet hidden.
    active: bool,
    /// Last time a key was dispatched to the engine (resets the "user just
    /// typed" edge of the recency gate).
    last_key_activity: Option<Instant>,
    /// Last time a show actually became visible (resets the "user just saw
    /// the indicator" edge of the recency gate).
    last_indicator_shown: Option<Instant>,
    /// Per-engine mode label cache for fallback label composition.
    mode_cache: ModeCache,
}

impl Indicator {
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff the indicator is currently visible.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Snapshot of the mode cache (for inspection / tracing).
    pub fn mode_cache(&self) -> &ModeCache {
        &self.mode_cache
    }

    /// Record that a key was dispatched to the engine. Updates the recency
    /// gate's "user just typed" edge. Called by the keyboard router for
    /// every PRESSED event that passes the guard, mirroring
    /// `typio_wl_frontend_record_key_activity`.
    pub fn record_key_activity(&mut self, now: Instant) {
        self.last_key_activity = Some(now);
    }

    /// First-focus path (`FirstActivate`). Gates: enabled + salience
    /// `Notable` + acknowledged-recency. Returns the label to render, if
    /// any.
    pub fn show_on_focus(
        &mut self,
        now: Instant,
        mode: Option<&EngineModeSnapshot<'_>>,
        config: &IndicatorConfig,
        sources: &dyn LabelSources,
    ) -> Option<String> {
        if !config.enabled {
            return None;
        }
        if let Some(m) = mode {
            if m.salience == Salience::Quiet {
                return None;
            }
        }
        if self.is_within_recency_cooldown(now) {
            return None;
        }
        self.build_and_commit(mode, sources)
    }

    /// Reactivate path (`Reactivate`). Gates: enabled + salience `Notable`.
    /// The recency gate is intentionally NOT applied: the user moved to a
    /// new caret in the same session, which can change context even when
    /// they were just typing. See ADR-0018.
    pub fn show_on_reactivate(
        &mut self,
        _now: Instant,
        mode: Option<&EngineModeSnapshot<'_>>,
        config: &IndicatorConfig,
        sources: &dyn LabelSources,
    ) -> Option<String> {
        if !config.enabled {
            return None;
        }
        if let Some(m) = mode {
            if m.salience == Salience::Quiet {
                return None;
            }
        }
        self.build_and_commit(mode, sources)
    }

    /// Deliberate-change path (engine switch, mode change, profile toggle).
    /// Gates: enabled only. The user just acted, always announce.
    pub fn show_for_state_change(
        &mut self,
        _now: Instant,
        mode: Option<&EngineModeSnapshot<'_>>,
        config: &IndicatorConfig,
        sources: &dyn LabelSources,
    ) -> Option<String> {
        if !config.enabled {
            return None;
        }
        self.build_and_commit(mode, sources)
    }

    /// Summon shortcut path. Behaves like [`Self::show_for_state_change`] —
    /// always announces, bypasses both gates. The user explicitly asked.
    pub fn show_on_summon(
        &mut self,
        now: Instant,
        config: &IndicatorConfig,
        sources: &dyn LabelSources,
    ) -> Option<String> {
        self.show_for_state_change(now, None, config, sources)
    }

    /// Note that the coordinator accepted and rendered the indicator.
    /// Updates the recency-tracking timestamp and the active flag. Call this
    /// both on immediate shows and when a queued show later flushes through
    /// `flush_pending_with_timeout`.
    pub fn note_shown(&mut self, now: Instant) {
        self.active = true;
        self.last_indicator_shown = Some(now);
    }

    /// Hide the indicator (timer expired, deactivate, or coordinator cancel).
    /// Clears the active flag but does NOT clear the recency edges — a
    /// recent indicator display still suppresses the next focus-path reveal.
    pub fn hide(&mut self) {
        self.active = false;
    }

    // ── Internals ────────────────────────────────────────────────────────

    /// Build the label from `sources`, refreshing the mode cache if the
    /// caller provided a fresh display_label.
    fn build_and_commit(
        &mut self,
        mode: Option<&EngineModeSnapshot<'_>>,
        sources: &dyn LabelSources,
    ) -> Option<String> {
        let lang = sources.active_language_tag();
        let engine_name = sources.active_engine_name();
        let engine_display = sources.active_engine_display_name();
        let mode_label = mode.and_then(|m| m.display_label);

        // Refresh the cache first so a fresh label is available for the
        // fallback lookup. Clone the cached value out of the borrow before
        // the mutable `update` to keep the borrow checker happy.
        if let Some(name) = engine_name {
            if let Some(label) = mode_label {
                self.mode_cache.update(name, label);
            }
        }
        let cached: Option<String> = engine_name
            .and_then(|n| self.mode_cache.lookup(n))
            .map(str::to_string);

        compose_label(
            lang,
            engine_display,
            engine_name,
            mode_label,
            cached.as_deref(),
        )
    }

    /// True iff "now" is within the recency cooldown of either edge.
    fn is_within_recency_cooldown(&self, now: Instant) -> bool {
        let last = match (self.last_key_activity, self.last_indicator_shown) {
            (None, None) => return false,
            (Some(t), None) => t,
            (None, Some(t)) => t,
            (Some(a), Some(b)) => a.max(b),
        };
        now.duration_since(last) < RECENT_INPUT_COOLDOWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test fixtures ────────────────────────────────────────────────────

    /// In-memory `LabelSources` for tests. Owns its strings so the borrowed
    /// trait methods have something to point at.
    struct Sources {
        lang: String,
        engine_name: String,
        engine_display: String,
    }

    impl Sources {
        fn new() -> Self {
            Self {
                lang: String::new(),
                engine_name: String::new(),
                engine_display: String::new(),
            }
        }

        fn lang(mut self, s: &str) -> Self {
            self.lang = s.to_string();
            self
        }

        fn engine(mut self, name: &str, display: &str) -> Self {
            self.engine_name = name.to_string();
            self.engine_display = display.to_string();
            self
        }
    }

    impl LabelSources for Sources {
        fn active_language_tag(&self) -> Option<&str> {
            (!self.lang.is_empty()).then_some(self.lang.as_str())
        }
        fn active_engine_name(&self) -> Option<&str> {
            (!self.engine_name.is_empty()).then_some(self.engine_name.as_str())
        }
        fn active_engine_display_name(&self) -> Option<&str> {
            (!self.engine_display.is_empty()).then_some(self.engine_display.as_str())
        }
    }

    fn mode(label: &str, salience: Salience) -> EngineModeSnapshot<'_> {
        EngineModeSnapshot {
            display_label: (!label.is_empty()).then_some(label),
            salience,
        }
    }

    fn notable(label: &str) -> EngineModeSnapshot<'_> {
        mode(label, Salience::Notable)
    }

    fn quiet(label: &str) -> EngineModeSnapshot<'_> {
        mode(label, Salience::Quiet)
    }

    /// A base time for tests. Tick forward by adding `Duration::from_millis`.
    fn t0() -> Instant {
        // Instant::now() is non-deterministic but we only ever compute
        // *differences* (via duration_since), so the absolute value is fine.
        Instant::now()
    }

    // ── IndicatorConfig ──────────────────────────────────────────────────

    #[test]
    fn config_default() {
        let c = IndicatorConfig::default();
        assert!(c.enabled);
        assert_eq!(c.duration, Duration::from_millis(1500));
    }

    #[test]
    fn config_clamps_below_floor_to_default() {
        // Matches the C clamp: too-small → default, not silently to floor.
        let c = IndicatorConfig::from_values(true, 50);
        assert_eq!(c.duration, Duration::from_millis(1500));
        let c = IndicatorConfig::from_values(true, 0);
        assert_eq!(c.duration, Duration::from_millis(1500));
        let c = IndicatorConfig::from_values(true, 99);
        assert_eq!(c.duration, Duration::from_millis(1500));
    }

    #[test]
    fn config_clamps_above_ceiling() {
        let c = IndicatorConfig::from_values(true, 10000);
        assert_eq!(c.duration, Duration::from_millis(10000));
        let c = IndicatorConfig::from_values(true, 999_999);
        assert_eq!(c.duration, Duration::from_millis(10000));
    }

    #[test]
    fn config_accepts_in_range() {
        let c = IndicatorConfig::from_values(true, 100);
        assert_eq!(c.duration, Duration::from_millis(100));
        let c = IndicatorConfig::from_values(true, 2200);
        assert_eq!(c.duration, Duration::from_millis(2200));
    }

    #[test]
    fn config_disabled_flag_propagates() {
        let c = IndicatorConfig::from_values(false, 1500);
        assert!(!c.enabled);
    }

    // ── compose_label ───────────────────────────────────────────────────

    #[test]
    fn label_all_empty_is_none() {
        assert_eq!(compose_label(None, None, None, None, None), None);
        assert_eq!(compose_label(Some(""), None, None, None, None), None);
    }

    #[test]
    fn label_language_only() {
        let l = compose_label(Some("zh"), None, None, None, None).unwrap();
        assert_eq!(l, "中");
    }

    #[test]
    fn label_engine_only_uses_display_name() {
        let l = compose_label(None, Some("Rime"), Some("rime"), None, None).unwrap();
        assert_eq!(l, "Rime");
    }

    #[test]
    fn label_engine_falls_back_to_raw_name() {
        // Display name empty → fall back to engine name.
        let l = compose_label(None, Some(""), Some("rime"), None, None).unwrap();
        assert_eq!(l, "rime");
    }

    #[test]
    fn label_mode_only() {
        let l = compose_label(None, None, None, Some("中/A"), None).unwrap();
        assert_eq!(l, "中/A");
    }

    #[test]
    fn label_all_three_segments() {
        let l = compose_label(Some("zh"), Some("Rime"), Some("rime"), Some("中/A"), None).unwrap();
        assert_eq!(l, "中 · Rime · 中/A");
    }

    #[test]
    fn label_uses_cached_mode_when_live_is_missing() {
        let l = compose_label(Some("en"), Some("Rime"), Some("rime"), None, Some("Latin")).unwrap();
        assert_eq!(l, "EN · Rime · Latin");
    }

    #[test]
    fn label_prefers_live_mode_over_cached() {
        let l = compose_label(Some("en"), Some("Rime"), Some("rime"), Some("Live"), Some("Cached")).unwrap();
        assert_eq!(l, "EN · Rime · Live");
    }

    #[test]
    fn label_empty_mode_falls_through_to_cached() {
        // Empty string is the same as None for the live mode.
        let l = compose_label(Some("en"), Some("Rime"), Some("rime"), Some(""), Some("Cached")).unwrap();
        assert_eq!(l, "EN · Rime · Cached");
    }

    #[test]
    fn label_language_and_mode_no_engine() {
        // Layout-only language (e.g. Darija): no engine segment at all.
        let l = compose_label(Some("ary"), None, None, None, None).unwrap();
        assert_eq!(l, "الد");
    }

    #[test]
    fn label_unknown_language_uses_uppercase_fallback_badge() {
        let l = compose_label(Some("xx"), None, None, None, None).unwrap();
        assert_eq!(l, "XX");
    }

    // ── ModeCache ───────────────────────────────────────────────────────

    #[test]
    fn cache_starts_empty() {
        let c = ModeCache::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert_eq!(c.lookup("rime"), None);
    }

    #[test]
    fn cache_update_and_lookup() {
        let mut c = ModeCache::new();
        c.update("rime", "中/A");
        assert_eq!(c.len(), 1);
        assert_eq!(c.lookup("rime"), Some("中/A"));
        assert_eq!(c.lookup("pinyin"), None);
    }

    #[test]
    fn cache_update_empty_args_is_noop() {
        let mut c = ModeCache::new();
        c.update("", "label");
        c.update("rime", "");
        assert!(c.is_empty());
    }

    #[test]
    fn cache_update_moves_existing_to_front() {
        let mut c = ModeCache::new();
        c.update("rime", "old");
        c.update("pinyin", "pn");
        c.update("rime", "new");
        assert_eq!(c.lookup("rime"), Some("new"));
        // Two entries total, not three.
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cache_capacity_caps_at_eight_dropping_oldest() {
        let mut c = ModeCache::new();
        for i in 0..MODE_CACHE_CAPACITY {
            c.update(&format!("e{i}"), &format!("m{i}"));
        }
        assert_eq!(c.len(), MODE_CACHE_CAPACITY);
        // Add one more — the tail (e0) should drop.
        c.update("e_new", "m_new");
        assert_eq!(c.len(), MODE_CACHE_CAPACITY);
        assert_eq!(c.lookup("e0"), None);
        assert_eq!(c.lookup("e_new"), Some("m_new"));
        // e1 should still be there (it was second-oldest).
        assert_eq!(c.lookup("e1"), Some("m1"));
    }

    // ── Indicator state machine ─────────────────────────────────────────

    #[test]
    fn indicator_starts_inactive() {
        let ind = Indicator::new();
        assert!(!ind.is_active());
    }

    // -- show_on_focus (FirstActivate) --

    #[test]
    fn focus_disabled_returns_none() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig {
            enabled: false,
            duration: Duration::from_millis(1500),
        };
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        assert!(ind.show_on_focus(t0(), Some(&m), &cfg, &src).is_none());
    }

    #[test]
    fn focus_quiet_salience_suppressed() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = quiet("中/A");
        assert!(ind.show_on_focus(t0(), Some(&m), &cfg, &src).is_none());
    }

    #[test]
    fn focus_notable_no_recency_shows() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let label = ind.show_on_focus(t0(), Some(&m), &cfg, &src).unwrap();
        assert_eq!(label, "中 · Rime · 中/A");
    }

    #[test]
    fn focus_after_recent_key_suppressed() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let base = t0();
        ind.record_key_activity(base);
        // Within the cooldown window.
        let now = base + Duration::from_millis(2999);
        assert!(ind.show_on_focus(now, Some(&m), &cfg, &src).is_none());
    }

    #[test]
    fn focus_after_recent_indicator_suppressed() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let base = t0();
        let label = ind.show_on_focus(base, Some(&m), &cfg, &src).unwrap();
        ind.note_shown(base);
        assert_eq!(label, "中 · Rime · 中/A");
        // Within the cooldown.
        let now = base + Duration::from_millis(2999);
        assert!(ind.show_on_focus(now, Some(&m), &cfg, &src).is_none());
    }

    #[test]
    fn focus_after_cooldown_boundary_shows() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let base = t0();
        ind.record_key_activity(base);
        // Exactly at the cooldown — boundary is exclusive on the low end.
        let now = base + Duration::from_millis(3000);
        assert!(ind.show_on_focus(now, Some(&m), &cfg, &src).is_some());
    }

    #[test]
    fn focus_with_no_mode_treats_salience_as_unknown_not_quiet() {
        // No mode snapshot → salience gate is skipped (None is treated as
        // "unknown, don't suppress"). This is the summon / engine-switch
        // path's default behaviour when the new engine hasn't reported yet.
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let label = ind.show_on_focus(t0(), None, &cfg, &src).unwrap();
        assert_eq!(label, "中 · Rime");
    }

    // -- show_on_reactivate --

    #[test]
    fn reactivate_quiet_suppressed() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = quiet("中/A");
        assert!(ind.show_on_reactivate(t0(), Some(&m), &cfg, &src).is_none());
    }

    #[test]
    fn reactivate_notable_shows_even_after_recent_key() {
        // REACTIVATE intentionally skips the recency gate: the user moved
        // to a new field, context may differ.
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let base = t0();
        ind.record_key_activity(base);
        let label = ind.show_on_reactivate(base, Some(&m), &cfg, &src).unwrap();
        assert_eq!(label, "中 · Rime · 中/A");
    }

    // -- show_for_state_change (deliberate) --

    #[test]
    fn state_change_always_shows_when_enabled() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");

        // Even with QUIET salience and recent activity.
        let m = quiet("中/A");
        let base = t0();
        ind.record_key_activity(base);
        let label = ind.show_for_state_change(base, Some(&m), &cfg, &src).unwrap();
        assert_eq!(label, "中 · Rime · 中/A");
    }

    #[test]
    fn state_change_with_no_mode_uses_cache() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");

        // First show with a live mode — populates the cache.
        let m = notable("中/A");
        let _ = ind.show_for_state_change(t0(), Some(&m), &cfg, &src).unwrap();

        // Second show with no live mode — falls back to the cached label.
        let label = ind.show_for_state_change(t0(), None, &cfg, &src).unwrap();
        assert_eq!(label, "中 · Rime · 中/A");
    }

    #[test]
    fn state_change_when_disabled_returns_none() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig {
            enabled: false,
            duration: Duration::from_millis(1500),
        };
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        assert!(ind.show_for_state_change(t0(), Some(&m), &cfg, &src).is_none());
    }

    // -- show_on_summon --

    #[test]
    fn summon_always_shows_when_enabled() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("en").engine("rime", "Rime");
        let label = ind.show_on_summon(t0(), &cfg, &src).unwrap();
        assert_eq!(label, "EN · Rime");
    }

    #[test]
    fn summon_after_recent_key_still_shows() {
        // Summon is user-initiated; bypasses recency.
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("en").engine("rime", "Rime");
        let base = t0();
        ind.record_key_activity(base);
        let label = ind.show_on_summon(base, &cfg, &src).unwrap();
        assert_eq!(label, "EN · Rime");
    }

    #[test]
    fn summon_when_no_engine_still_uses_language() {
        // Layout-only language: the language segment carries the banner.
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("ary");
        let label = ind.show_on_summon(t0(), &cfg, &src).unwrap();
        assert_eq!(label, "الد");
    }

    #[test]
    fn summon_with_nothing_active_returns_none() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new();
        assert!(ind.show_on_summon(t0(), &cfg, &src).is_none());
    }

    // -- note_shown / hide --

    #[test]
    fn note_shown_sets_active() {
        let mut ind = Indicator::new();
        assert!(!ind.is_active());
        ind.note_shown(t0());
        assert!(ind.is_active());
    }

    #[test]
    fn hide_clears_active() {
        let mut ind = Indicator::new();
        ind.note_shown(t0());
        assert!(ind.is_active());
        ind.hide();
        assert!(!ind.is_active());
    }

    #[test]
    fn hide_does_not_clear_recency() {
        // A recently-shown indicator should still suppress the next
        // focus-path reveal even after it's been hidden by the timer.
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let base = t0();
        ind.note_shown(base);
        ind.hide();
        // Within cooldown → suppressed.
        let now = base + Duration::from_millis(1000);
        assert!(ind.show_on_focus(now, Some(&m), &cfg, &src).is_none());
    }

    // -- record_key_activity --

    #[test]
    fn record_key_activity_updates_recency() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");

        // First show works (no prior activity).
        assert!(ind.show_on_focus(t0(), Some(&m), &cfg, &src).is_some());

        // After a key press, the next focus-path show is suppressed.
        let base = t0();
        ind.record_key_activity(base);
        assert!(ind.show_on_focus(base, Some(&m), &cfg, &src).is_none());
    }

    // -- mode cache integration --

    #[test]
    fn mode_cache_populated_on_successful_show() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh").engine("rime", "Rime");
        let m = notable("中/A");
        let _ = ind.show_on_focus(t0(), Some(&m), &cfg, &src).unwrap();
        assert_eq!(ind.mode_cache().lookup("rime"), Some("中/A"));
    }

    #[test]
    fn mode_cache_not_populated_when_no_engine() {
        let mut ind = Indicator::new();
        let cfg = IndicatorConfig::default();
        let src = Sources::new().lang("zh");
        let m = notable("中/A");
        let _ = ind.show_on_focus(t0(), Some(&m), &cfg, &src);
        assert!(ind.mode_cache().is_empty());
    }
}
