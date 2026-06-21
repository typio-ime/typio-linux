//! Centralized state controller — single source of truth for runtime surfaces.
//!
//! Port of the stateful half of `src/state/controller.c` (the pure
//! language-presentation slice lives in [`crate::language_display`]). The
//! controller sits between the Core layer and external surfaces (tray, D-Bus
//! status bus): it maintains a snapshot of user-visible state, answers queries
//! from ONE place instead of every surface reaching into `TypioInstance`, and
//! broadcasts change notifications so surfaces update uniformly.
//!
//! The libtypio registry/config reads are abstracted behind [`RegistryView`]
//! so the notify/broadcast logic is unit-testable without a live instance; the
//! production adapter wraps `TypioInstance` via the `typio` crate.

use crate::language_display::resolve_language_icon;

/// The kinds of state change a listener can observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateChange {
    Engine,
    VoiceEngine,
    Language,
    Status,
    StatusIcon,
    /// ADR-0034: the set of registered languages changed.
    Languages,
}

/// A keyboard-engine mode (status), used for the tray tooltip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineMode {
    pub id: Option<String>,
    pub label: Option<String>,
    pub display_label: Option<String>,
    pub icon_name: Option<String>,
}

/// The libtypio registry/config reads the controller depends on. The live
/// implementation wraps `TypioInstance`; tests provide a fake.
pub trait RegistryView {
    fn active_keyboard(&self) -> Option<String>;
    fn active_language(&self) -> Option<String>;
    fn active_voice(&self) -> Option<String>;
    fn engine_display_name(&self, _name: &str) -> Option<String> {
        None
    }
    /// `[languages.<tag>].icon` override; key is the full config path.
    fn config_icon(&self, _key: &str) -> Option<String> {
        None
    }
}

type Listener = Box<dyn FnMut(StateChange)>;

/// Centralized, observable snapshot of user-visible runtime state.
pub struct StateController<R: RegistryView> {
    registry: R,

    // ── cached state snapshots ──
    active_engine_name: Option<String>,
    active_engine_display_name: Option<String>,
    active_voice_engine_name: Option<String>,
    active_voice_engine_display_name: Option<String>,
    active_language: Option<String>,
    status_icon: String,
    status_icon_is_badge: bool,
    status_badge_text: Option<String>,
    engine_active: bool,
    status: Option<EngineMode>,

    // ── listeners ──
    listeners: Vec<(u64, Listener)>,
    next_listener_id: u64,
}

impl<R: RegistryView> StateController<R> {
    pub fn new(registry: R) -> Self {
        StateController {
            registry,
            active_engine_name: None,
            active_engine_display_name: None,
            active_voice_engine_name: None,
            active_voice_engine_display_name: None,
            active_language: None,
            status_icon: "typio-keyboard-off-symbolic".to_string(),
            status_icon_is_badge: false,
            status_badge_text: None,
            engine_active: false,
            status: None,
            listeners: Vec::new(),
            next_listener_id: 1,
        }
    }

    // ── Listener registration ────────────────────────────────────────────

    /// Register a change listener; returns an id used to remove it.
    pub fn add_listener(&mut self, callback: Listener) -> u64 {
        let id = self.next_listener_id;
        self.next_listener_id += 1;
        self.listeners.push((id, callback));
        id
    }

    /// Remove a previously-registered listener by id.
    pub fn remove_listener(&mut self, id: u64) {
        if let Some(pos) = self.listeners.iter().position(|(lid, _)| *lid == id) {
            drop(self.listeners.remove(pos));
        }
    }

    fn broadcast(&mut self, change: StateChange) {
        for (_, cb) in self.listeners.iter_mut() {
            cb(change);
        }
    }

    // ── State queries ────────────────────────────────────────────────────

    pub fn active_engine_name(&self) -> Option<&str> {
        self.active_engine_name.as_deref()
    }
    pub fn active_engine_display_name(&self) -> Option<&str> {
        self.active_engine_display_name.as_deref()
    }
    pub fn active_voice_engine_name(&self) -> Option<&str> {
        self.active_voice_engine_name.as_deref()
    }
    pub fn active_voice_engine_display_name(&self) -> Option<&str> {
        self.active_voice_engine_display_name.as_deref()
    }
    pub fn active_language(&self) -> Option<&str> {
        self.active_language.as_deref()
    }
    pub fn status_icon(&self) -> &str {
        &self.status_icon
    }
    pub fn status_icon_is_badge(&self) -> bool {
        self.status_icon_is_badge
    }
    pub fn status_badge_text(&self) -> Option<&str> {
        self.status_badge_text.as_deref()
    }
    pub fn engine_active(&self) -> bool {
        self.engine_active
    }
    pub fn current_status(&self) -> Option<&EngineMode> {
        self.status.as_ref()
    }

    // ── Internal refresh helpers ─────────────────────────────────────────

    /// Resolve the status icon via the language-only chain and store the
    /// badge state. Mirrors `resolve_status_icon`.
    fn resolve_status_icon(&mut self) {
        let tag = self.registry.active_language();
        let engine_active = self.engine_active;
        let cfg = |key: &str| self.registry.config_icon(key);
        let resolved = resolve_language_icon(tag.as_deref(), engine_active, Some(&cfg));
        self.status_icon = resolved.icon;
        self.status_icon_is_badge = resolved.badge_text.is_some();
        self.status_badge_text = resolved.badge_text;
    }

    /// Refresh the active-language snapshot from the registry. Returns true
    /// when the language changed.
    fn refresh_language(&mut self) -> bool {
        let lang = self.registry.active_language();
        if lang != self.active_language {
            self.active_language = lang;
            true
        } else {
            false
        }
    }

    fn update_engine_active(&mut self) {
        self.engine_active = self.registry.active_keyboard().is_some();
    }

    // ── Notifications from Core ───────────────────────────────────────────

    /// The active keyboard engine changed. `info` is `(name, display_name)`.
    pub fn notify_engine_changed(&mut self, info: Option<(&str, Option<&str>)>) {
        self.active_engine_name = info.map(|(n, _)| n.to_string());
        self.active_engine_display_name = info.and_then(|(_, d)| d.map(str::to_string));

        self.update_engine_active();
        self.resolve_status_icon();
        self.broadcast(StateChange::Engine);

        if self.refresh_language() {
            self.resolve_status_icon();
            self.broadcast(StateChange::Language);
            self.broadcast(StateChange::StatusIcon);
        }
    }

    /// The active voice engine changed.
    pub fn notify_voice_engine_changed(&mut self, info: Option<(&str, Option<&str>)>) {
        self.active_voice_engine_name = info.map(|(n, _)| n.to_string());
        self.active_voice_engine_display_name = info.and_then(|(_, d)| d.map(str::to_string));
        self.broadcast(StateChange::VoiceEngine);

        if self.refresh_language() {
            self.resolve_status_icon();
            self.broadcast(StateChange::Language);
            self.broadcast(StateChange::StatusIcon);
        }
    }

    /// An engine pushed a new mode (status). Stored for the tooltip; the
    /// mode's icon is intentionally not consumed for the tray (ADR-0033).
    pub fn notify_status_changed(&mut self, mode: Option<EngineMode>) {
        self.status = mode;
        self.broadcast(StateChange::Status);
    }

    /// Engine-pushed status icon. The tray icon is language-only (ADR-0033),
    /// so the icon name is not consumed here; we still broadcast.
    pub fn notify_status_icon_changed(&mut self, _icon_name: Option<&str>) {
        self.broadcast(StateChange::StatusIcon);
    }

    /// ADR-0034: an engine updated its declared languages at runtime.
    pub fn notify_languages_changed(&mut self) {
        let lang_changed = self.refresh_language();
        self.resolve_status_icon();
        self.broadcast(StateChange::Languages);
        if lang_changed {
            self.broadcast(StateChange::Language);
        }
        self.broadcast(StateChange::StatusIcon);
    }

    /// Re-read all state from the registry and broadcast an initial sync.
    /// Call once after all listeners have registered.
    pub fn sync(&mut self) {
        let kb = self.registry.active_keyboard();
        self.active_engine_display_name = kb
            .as_deref()
            .and_then(|n| self.registry.engine_display_name(n))
            .filter(|s| !s.is_empty());
        self.engine_active = kb.is_some();
        self.active_engine_name = kb;

        let voice = self.registry.active_voice();
        self.active_voice_engine_display_name = voice
            .as_deref()
            .and_then(|n| self.registry.engine_display_name(n))
            .filter(|s| !s.is_empty());
        self.active_voice_engine_name = voice;

        self.active_language = self.registry.active_language();
        self.resolve_status_icon();

        self.broadcast(StateChange::Engine);
        self.broadcast(StateChange::VoiceEngine);
        self.broadcast(StateChange::Language);
        self.broadcast(StateChange::StatusIcon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default, Clone)]
    struct FakeRegistry {
        active_keyboard: Option<String>,
        active_language: Option<String>,
        active_voice: Option<String>,
        display_names: std::collections::HashMap<String, String>,
        config_icons: std::collections::HashMap<String, String>,
    }

    impl RegistryView for FakeRegistry {
        fn active_keyboard(&self) -> Option<String> {
            self.active_keyboard.clone()
        }
        fn active_language(&self) -> Option<String> {
            self.active_language.clone()
        }
        fn active_voice(&self) -> Option<String> {
            self.active_voice.clone()
        }
        fn engine_display_name(&self, name: &str) -> Option<String> {
            self.display_names.get(name).cloned()
        }
        fn config_icon(&self, key: &str) -> Option<String> {
            self.config_icons.get(key).cloned()
        }
    }

    /// Controller wired to a recording listener; returns (controller, log).
    fn with_recorder(
        reg: FakeRegistry,
    ) -> (StateController<FakeRegistry>, Rc<RefCell<Vec<StateChange>>>) {
        let mut ctrl = StateController::new(reg);
        let log = Rc::new(RefCell::new(Vec::new()));
        let l2 = log.clone();
        ctrl.add_listener(Box::new(move |c| l2.borrow_mut().push(c)));
        (ctrl, log)
    }

    #[test]
    fn listener_add_and_remove() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut ctrl = StateController::new(FakeRegistry::default());
        let l = log.clone();
        let id = ctrl.add_listener(Box::new(move |c| l.borrow_mut().push(c)));

        ctrl.notify_status_changed(None);
        assert_eq!(*log.borrow(), vec![StateChange::Status]);

        ctrl.remove_listener(id);
        ctrl.notify_status_changed(None);
        assert_eq!(log.borrow().len(), 1, "removed listener still fired");
    }

    #[test]
    fn engine_changed_without_language_change_broadcasts_engine_only() {
        let reg = FakeRegistry {
            active_keyboard: Some("rime".into()),
            active_language: None,
            ..Default::default()
        };
        let (mut ctrl, log) = with_recorder(reg);
        ctrl.notify_engine_changed(Some(("rime", Some("Rime"))));
        assert_eq!(*log.borrow(), vec![StateChange::Engine]);
        assert_eq!(ctrl.active_engine_name(), Some("rime"));
        assert_eq!(ctrl.active_engine_display_name(), Some("Rime"));
        assert!(ctrl.engine_active());
    }

    #[test]
    fn engine_changed_with_language_change_broadcasts_full_sequence() {
        let reg = FakeRegistry {
            active_keyboard: Some("rime".into()),
            active_language: Some("zh".into()),
            ..Default::default()
        };
        let (mut ctrl, log) = with_recorder(reg);
        ctrl.notify_engine_changed(Some(("rime", Some("Rime"))));
        assert_eq!(
            *log.borrow(),
            vec![
                StateChange::Engine,
                StateChange::Language,
                StateChange::StatusIcon
            ]
        );
        // zh resolves to the 中 badge.
        assert!(ctrl.status_icon_is_badge());
        assert_eq!(ctrl.status_badge_text(), Some("中"));
        assert_eq!(ctrl.active_language(), Some("zh"));
    }

    #[test]
    fn config_icon_override_suppresses_badge() {
        let mut reg = FakeRegistry {
            active_keyboard: Some("rime".into()),
            active_language: Some("zh".into()),
            ..Default::default()
        };
        reg.config_icons
            .insert("languages.zh.icon".into(), "my-zh-icon".into());
        let (mut ctrl, _log) = with_recorder(reg);
        ctrl.notify_engine_changed(Some(("rime", None)));
        assert_eq!(ctrl.status_icon(), "my-zh-icon");
        assert!(!ctrl.status_icon_is_badge());
    }

    #[test]
    fn voice_engine_changed_broadcasts_voice() {
        let (mut ctrl, log) = with_recorder(FakeRegistry::default());
        ctrl.notify_voice_engine_changed(Some(("whisper", Some("Whisper"))));
        assert_eq!(*log.borrow(), vec![StateChange::VoiceEngine]);
        assert_eq!(ctrl.active_voice_engine_name(), Some("whisper"));
    }

    #[test]
    fn status_changed_stores_mode() {
        let (mut ctrl, log) = with_recorder(FakeRegistry::default());
        let mode = EngineMode {
            label: Some("拼音".into()),
            ..Default::default()
        };
        ctrl.notify_status_changed(Some(mode.clone()));
        assert_eq!(*log.borrow(), vec![StateChange::Status]);
        assert_eq!(ctrl.current_status(), Some(&mode));
    }

    #[test]
    fn languages_changed_sequence() {
        // active language present → lang_changed true on first refresh.
        let reg = FakeRegistry {
            active_language: Some("ja".into()),
            ..Default::default()
        };
        let (mut ctrl, log) = with_recorder(reg);
        ctrl.notify_languages_changed();
        assert_eq!(
            *log.borrow(),
            vec![
                StateChange::Languages,
                StateChange::Language,
                StateChange::StatusIcon
            ]
        );

        // Second call: language unchanged → no Language broadcast.
        log.borrow_mut().clear();
        ctrl.notify_languages_changed();
        assert_eq!(
            *log.borrow(),
            vec![StateChange::Languages, StateChange::StatusIcon]
        );
    }

    #[test]
    fn sync_snapshots_and_broadcasts_initial_state() {
        let mut reg = FakeRegistry {
            active_keyboard: Some("rime".into()),
            active_language: Some("zh".into()),
            active_voice: Some("whisper".into()),
            ..Default::default()
        };
        reg.display_names.insert("rime".into(), "Rime".into());
        reg.display_names.insert("whisper".into(), "Whisper".into());
        let (mut ctrl, log) = with_recorder(reg);
        ctrl.sync();
        assert_eq!(ctrl.active_engine_name(), Some("rime"));
        assert_eq!(ctrl.active_engine_display_name(), Some("Rime"));
        assert_eq!(ctrl.active_voice_engine_display_name(), Some("Whisper"));
        assert_eq!(ctrl.active_language(), Some("zh"));
        assert!(ctrl.engine_active());
        assert_eq!(
            *log.borrow(),
            vec![
                StateChange::Engine,
                StateChange::VoiceEngine,
                StateChange::Language,
                StateChange::StatusIcon
            ]
        );
    }
}
