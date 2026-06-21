//! Pure in-memory model of the tray dbusmenu, decoupled from sd_bus.
//!
//! Port of `src/tray/menu_model.c`. The model captures WHAT the tray menu
//! contains at the current instant (items, labels, radio state, submenu
//! nesting). [`build`] turns a registry snapshot into the tree; serialising
//! the tree to a dbusmenu `GetLayout` reply is a separate concern (the
//! not-yet-ported sd_bus layer). Splitting the two makes the menu structure
//! unit-testable without a D-Bus fixture.
//!
//! The C builder reads a live `TypioInstance`/`TypioRegistry`; here that input
//! is a plain-data [`RegistrySnapshot`], so the ADR-0033 layout logic is pure
//! and testable. A thin adapter populating the snapshot from the live registry
//! is wired when the tray D-Bus surface is ported.

use crate::language_display::language_menu_label;

// ─── ID layout (kept in sync with sni.c — see ADR-0033) ──────────────────

const SECTION_MISC: i32 = 1000; // Restart, Quit, separators
const SECTION_LANG: i32 = 2000; // per-language entries (submenus or flat)
const SECTION_ENGINE: i32 = 3000; // per-engine entries inside a language submenu
const SECTION_ORPHAN: i32 = 4000; // engines that declare no registered language
const SECTION_VOICE: i32 = 5000; // voice engine entries

const LANG_BASE: i32 = SECTION_LANG;
const LANG_MAX: usize = 16;
const ENGINE_MAX: usize = 16;
const ORPHAN_BASE: i32 = SECTION_ORPHAN;
const VOICE_BASE: i32 = SECTION_VOICE;
const VOICE_MAX: usize = 16;

const ITEM_RESTART: i32 = SECTION_MISC + 1;
const ITEM_QUIT: i32 = SECTION_MISC + 2;
const ITEM_SEP_BEGIN: i32 = SECTION_MISC + 100;

/// Composite ID for an engine appearing under a language submenu. A single
/// engine may legitimately appear under several language submenus (e.g. rime
/// under both zh and yue), so each (language, engine) pair needs a distinct
/// dbusmenu ID: `ENGINE_BASE + lang_idx * ENGINE_MAX + engine_idx`.
fn engine_in_lang(lang_idx: usize, engine_idx: usize) -> i32 {
    SECTION_ENGINE + (lang_idx as i32) * (ENGINE_MAX as i32) + (engine_idx as i32)
}

// ─── Tree node ───────────────────────────────────────────────────────────

/// A tray dbusmenu node. Owns its label/strings and its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuItem {
    pub id: i32,
    pub label: Option<String>,
    /// Explicit type: `None` (standard/radio) or `Some("separator")`.
    type_: Option<String>,
    accessible_desc: Option<String>,
    pub enabled: bool,
    /// `-1` = not a toggle, `0` = radio off, `1` = radio on.
    pub toggle_state: i32,
    pub is_submenu_parent: bool,
    pub children: Vec<TrayMenuItem>,
}

/// Empty strings collapse to `None` (mirrors the C `dup_or_null`).
fn opt(s: Option<&str>) -> Option<String> {
    s.filter(|v| !v.is_empty()).map(str::to_string)
}

impl TrayMenuItem {
    fn alloc(
        id: i32,
        label: Option<&str>,
        type_: Option<&str>,
        enabled: bool,
        toggle_state: i32,
        is_submenu_parent: bool,
        accessible_desc: Option<&str>,
    ) -> Self {
        TrayMenuItem {
            id,
            label: opt(label),
            type_: opt(type_),
            accessible_desc: opt(accessible_desc),
            enabled,
            toggle_state,
            is_submenu_parent,
            children: Vec::new(),
        }
    }

    /// Standard clickable item.
    pub fn new_standard(
        id: i32,
        label: Option<&str>,
        enabled: bool,
        accessible_desc: Option<&str>,
    ) -> Self {
        Self::alloc(id, label, None, enabled, -1, false, accessible_desc)
    }

    /// Separator line.
    pub fn new_separator(id: i32) -> Self {
        Self::alloc(id, None, Some("separator"), true, -1, false, None)
    }

    /// Radio leaf (`selected` drives toggle-state).
    pub fn new_radio(
        id: i32,
        label: Option<&str>,
        enabled: bool,
        selected: bool,
        accessible_desc: Option<&str>,
    ) -> Self {
        Self::alloc(
            id,
            label,
            None,
            enabled,
            i32::from(selected),
            false,
            accessible_desc,
        )
    }

    /// Submenu parent (children-display=submenu). When `selected`, also
    /// advertises radio + toggle-state=1 so the active language is marked at
    /// the top level even when its engines live in a submenu.
    pub fn new_submenu(
        id: i32,
        label: Option<&str>,
        enabled: bool,
        selected: bool,
        accessible_desc: Option<&str>,
    ) -> Self {
        Self::alloc(
            id,
            label,
            None,
            enabled,
            i32::from(selected),
            true,
            accessible_desc,
        )
    }

    /// Append a child (takes ownership).
    pub fn add_child(&mut self, child: TrayMenuItem) {
        self.children.push(child);
    }

    /// Serialised type: an explicit type, else `"radio"` when toggle-able,
    /// else `None`.
    pub fn type_str(&self) -> Option<&str> {
        if let Some(t) = &self.type_ {
            return Some(t);
        }
        if self.toggle_state >= 0 {
            return Some("radio");
        }
        None
    }

    /// Accessible description, defaulting to the label when unset.
    pub fn accessible_desc(&self) -> Option<&str> {
        self.accessible_desc.as_deref().or(self.label.as_deref())
    }

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn child(&self, index: usize) -> Option<&TrayMenuItem> {
        self.children.get(index)
    }
}

// ─── Registry snapshot (plain-data builder input) ────────────────────────

/// One keyboard or voice engine as seen by the builder.
#[derive(Debug, Clone, Default)]
pub struct EngineDesc {
    pub name: String,
    /// Display name; empty falls back to `name`.
    pub display_name: Option<String>,
    /// Declared language tags (keyboard engines only).
    pub languages: Vec<String>,
}

impl EngineDesc {
    fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.name)
    }

    fn declares(&self, tag: &str) -> bool {
        self.languages.iter().any(|l| l == tag)
    }
}

/// Plain-data view of the registry state the tray menu is built from.
#[derive(Debug, Clone, Default)]
pub struct RegistrySnapshot {
    /// Registered language tags, in display order.
    pub languages: Vec<String>,
    pub active_language: Option<String>,
    /// Keyboard engines, in `list_ordered_keyboards` order.
    pub keyboards: Vec<EngineDesc>,
    /// Voice engines, in `list_voices` order.
    pub voices: Vec<EngineDesc>,
    pub active_voice: Option<String>,
}

// ─── Builder (ADR-0033) ──────────────────────────────────────────────────

/// Build the full tray menu tree from a registry snapshot.
///
/// Layout: each registered language is a top-level entry — a submenu parent
/// when at least one engine declares it, else a flat radio leaf. Engines that
/// declare none of the registered languages appear in a trailing flat
/// "Engines" section. Voice engines form their own radio group. Restart and
/// Quit are always present.
pub fn build(snapshot: &RegistrySnapshot, engine_name: Option<&str>) -> TrayMenuItem {
    // Root is always id=0 per the dbusmenu convention.
    let mut root = TrayMenuItem::new_submenu(0, None, true, false, None);
    let mut next_sep_id = ITEM_SEP_BEGIN;

    append_language_section(&mut root, snapshot, engine_name, &mut next_sep_id);
    append_voice_section(&mut root, snapshot, &mut next_sep_id);
    append_misc_section(&mut root);

    root
}

fn append_language_section(
    root: &mut TrayMenuItem,
    snapshot: &RegistrySnapshot,
    engine_name: Option<&str>,
    next_sep_id: &mut i32,
) {
    let engine_cap = snapshot.keyboards.len().min(ENGINE_MAX);
    let keyboards = &snapshot.keyboards[..engine_cap];
    let mut engine_placed = vec![false; engine_cap];
    let mut lang_shown = 0usize;

    for (i, lang) in snapshot.languages.iter().take(LANG_MAX).enumerate() {
        let lang_label = language_menu_label(lang);
        let is_current = snapshot.active_language.as_deref() == Some(lang.as_str());

        let child_match = keyboards.iter().filter(|e| e.declares(lang)).count();

        let lang_item = if child_match == 0 {
            // Layout-only language: clicking switches the language slot.
            TrayMenuItem::new_radio(
                LANG_BASE + i as i32,
                Some(&lang_label),
                true,
                is_current,
                None,
            )
        } else {
            let mut item = TrayMenuItem::new_submenu(
                LANG_BASE + i as i32,
                Some(&lang_label),
                true,
                is_current,
                None,
            );
            for (j, engine) in keyboards.iter().enumerate() {
                if !engine.declares(lang) {
                    continue;
                }
                engine_placed[j] = true;
                let eng_current = engine_name == Some(engine.name.as_str());
                item.add_child(TrayMenuItem::new_radio(
                    engine_in_lang(i, j),
                    Some(engine.display()),
                    true,
                    eng_current,
                    None,
                ));
            }
            item
        };
        root.add_child(lang_item);
        lang_shown += 1;
    }

    // Orphan engines (declare no registered language).
    let mut orphan_shown = false;
    for (j, engine) in keyboards.iter().enumerate() {
        if engine_placed[j] {
            continue;
        }
        if !orphan_shown {
            if lang_shown > 0 {
                root.add_child(TrayMenuItem::new_separator(*next_sep_id));
                *next_sep_id += 1;
            }
            root.add_child(TrayMenuItem::new_standard(
                *next_sep_id,
                Some("Engines"),
                false,
                None,
            ));
            *next_sep_id += 1;
            orphan_shown = true;
        }
        let is_current = engine_name == Some(engine.name.as_str());
        root.add_child(TrayMenuItem::new_radio(
            ORPHAN_BASE + j as i32,
            Some(engine.display()),
            true,
            is_current,
            None,
        ));
    }

    if lang_shown > 0 || orphan_shown {
        root.add_child(TrayMenuItem::new_separator(*next_sep_id));
        *next_sep_id += 1;
    }
}

fn append_voice_section(
    root: &mut TrayMenuItem,
    snapshot: &RegistrySnapshot,
    next_sep_id: &mut i32,
) {
    let mut voice_shown = 0usize;
    for (i, voice) in snapshot.voices.iter().take(VOICE_MAX).enumerate() {
        let is_current = snapshot.active_voice.as_deref() == Some(voice.name.as_str());
        root.add_child(TrayMenuItem::new_radio(
            VOICE_BASE + i as i32,
            Some(voice.display()),
            true,
            is_current,
            None,
        ));
        voice_shown += 1;
    }
    if voice_shown > 0 {
        root.add_child(TrayMenuItem::new_separator(*next_sep_id));
        *next_sep_id += 1;
    }
}

fn append_misc_section(root: &mut TrayMenuItem) {
    root.add_child(TrayMenuItem::new_standard(
        ITEM_RESTART,
        Some("Restart"),
        true,
        None,
    ));
    root.add_child(TrayMenuItem::new_standard(
        ITEM_QUIT,
        Some("Quit"),
        true,
        None,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kbd(name: &str, display: &str, langs: &[&str]) -> EngineDesc {
        EngineDesc {
            name: name.to_string(),
            display_name: Some(display.to_string()),
            languages: langs.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── Tree model ───────────────────────────────────────────────────────

    #[test]
    fn node_constructors_and_tree_ops() {
        let mut root = TrayMenuItem::new_submenu(0, Some("root"), true, false, None);
        let a = TrayMenuItem::new_radio(100, Some("A"), true, true, None);
        let b = TrayMenuItem::new_radio(101, Some("B"), true, false, None);
        let sep = TrayMenuItem::new_separator(102);
        let act = TrayMenuItem::new_standard(103, Some("Action"), false, None);

        root.add_child(a);
        root.add_child(b);
        root.add_child(sep);
        root.add_child(act);

        assert_eq!(root.child_count(), 4);
        assert!(root.is_submenu_parent);

        let a = root.child(0).unwrap();
        assert_eq!(a.id, 100);
        assert_eq!(a.toggle_state, 1);
        assert_eq!(a.type_str(), Some("radio"));
        // accessible_desc defaults to the label when unset.
        assert_eq!(a.accessible_desc(), Some("A"));

        let b = root.child(1).unwrap();
        assert_eq!(b.toggle_state, 0);
        assert_eq!(b.type_str(), Some("radio"));

        let sep = root.child(2).unwrap();
        assert_eq!(sep.type_str(), Some("separator"));
        assert_eq!(sep.toggle_state, -1);

        let act = root.child(3).unwrap();
        assert!(!act.enabled);
        assert_eq!(act.type_str(), None);
    }

    #[test]
    fn out_of_range_child_is_none() {
        let root = TrayMenuItem::new_submenu(0, None, true, false, None);
        assert!(root.child(0).is_none());
    }

    // ── Builder ──────────────────────────────────────────────────────────

    /// Last two children are always Restart(1001) / Quit(1002).
    fn assert_misc_tail(root: &TrayMenuItem) {
        let n = root.child_count();
        let restart = root.child(n - 2).unwrap();
        let quit = root.child(n - 1).unwrap();
        assert_eq!(restart.id, ITEM_RESTART);
        assert_eq!(restart.label.as_deref(), Some("Restart"));
        assert_eq!(restart.toggle_state, -1);
        assert_eq!(quit.id, ITEM_QUIT);
        assert_eq!(quit.label.as_deref(), Some("Quit"));
    }

    #[test]
    fn root_is_submenu_with_restart_quit() {
        let root = build(&RegistrySnapshot::default(), None);
        assert_eq!(root.id, 0);
        assert!(root.is_submenu_parent);
        assert_eq!(root.child_count(), 2);
        assert_misc_tail(&root);
    }

    #[test]
    fn two_languages_with_engines() {
        let snap = RegistrySnapshot {
            languages: vec!["en".into(), "zh".into()],
            active_language: Some("zh".into()),
            keyboards: vec![kbd("basic", "Basic", &["en"]), kbd("rime", "Rime", &["zh"])],
            ..Default::default()
        };
        let root = build(&snap, Some("rime"));

        let en = root.child(0).unwrap();
        let zh = root.child(1).unwrap();
        assert_eq!(en.id, 2000);
        assert_eq!(zh.id, 2001);
        assert!(en.is_submenu_parent);
        assert!(zh.is_submenu_parent);
        // zh is the active language → toggle on; en off.
        assert_eq!(zh.toggle_state, 1);
        assert_eq!(en.toggle_state, 0);

        // English submenu holds the basic engine; 中文 holds rime (active).
        let basic = en.child(0).unwrap();
        assert_eq!(basic.id, engine_in_lang(0, 0));
        assert_eq!(basic.toggle_state, 0);
        let rime = zh.child(0).unwrap();
        assert_eq!(rime.id, engine_in_lang(1, 1));
        assert_eq!(rime.toggle_state, 1);

        assert_misc_tail(&root);
    }

    #[test]
    fn layout_only_language_is_flat_radio() {
        let snap = RegistrySnapshot {
            languages: vec!["en".into()],
            keyboards: vec![kbd("basic", "Basic", &[])], // declares no language
            ..Default::default()
        };
        let root = build(&snap, Some("basic"));
        let en = root.child(0).unwrap();
        assert!(!en.is_submenu_parent);
        assert_eq!(en.type_str(), Some("radio"));
    }

    #[test]
    fn orphan_engine_section() {
        let snap = RegistrySnapshot {
            languages: vec![], // no languages → engine is orphaned
            keyboards: vec![kbd("mystery", "Mystery", &["xx"])],
            ..Default::default()
        };
        let root = build(&snap, None);
        // First child is the disabled "Engines" header, then the radio.
        let header = root.child(0).unwrap();
        assert_eq!(header.label.as_deref(), Some("Engines"));
        assert!(!header.enabled);
        assert_eq!(header.toggle_state, -1);
        let mystery = root.child(1).unwrap();
        assert_eq!(mystery.id, ORPHAN_BASE);
        assert_eq!(mystery.type_str(), Some("radio"));
        assert_misc_tail(&root);
    }

    #[test]
    fn script_disambiguation_in_label() {
        let snap = RegistrySnapshot {
            languages: vec!["zh-Hans".into()],
            keyboards: vec![],
            ..Default::default()
        };
        let root = build(&snap, None);
        let zh = root.child(0).unwrap();
        assert_eq!(zh.label.as_deref(), Some("中文 (简)"));
    }

    #[test]
    fn multi_language_engine_appears_under_each() {
        let snap = RegistrySnapshot {
            languages: vec!["zh".into(), "yue".into()],
            keyboards: vec![kbd("rime", "Rime", &["zh", "yue"])],
            ..Default::default()
        };
        let root = build(&snap, None);
        let zh = root.child(0).unwrap();
        let yue = root.child(1).unwrap();
        // rime appears under both, with distinct composite IDs.
        let zh_rime = zh.child(0).unwrap();
        let yue_rime = yue.child(0).unwrap();
        assert_eq!(zh_rime.id, engine_in_lang(0, 0));
        assert_eq!(yue_rime.id, engine_in_lang(1, 0));
        assert_ne!(zh_rime.id, yue_rime.id);
    }

    #[test]
    fn voice_section_radio_group() {
        let snap = RegistrySnapshot {
            voices: vec![EngineDesc {
                name: "whisper".into(),
                display_name: Some("Whisper".into()),
                languages: vec![],
            }],
            active_voice: Some("whisper".into()),
            ..Default::default()
        };
        let root = build(&snap, None);
        let voice = root.child(0).unwrap();
        assert_eq!(voice.id, VOICE_BASE);
        assert_eq!(voice.label.as_deref(), Some("Whisper"));
        assert_eq!(voice.toggle_state, 1);
        assert_misc_tail(&root);
    }
}
