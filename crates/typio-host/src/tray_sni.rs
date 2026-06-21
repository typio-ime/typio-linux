//! StatusNotifierItem tray — D-Bus transport over zbus.
//!
//! Two interfaces are served on the session bus:
//!
//! - `org.kde.StatusNotifierItem` at `/StatusNotifierItem` — the icon surface
//!   (Category/Id/Title/Status/IconName/IconPixmap/ToolTip/Menu + Activate/
//!   ContextMenu/SecondaryActivate/Scroll + NewIcon/NewStatus/... signals).
//! - `com.canonical.dbusmenu` at `/MenuBar` — the right-click menu
//!   (GetLayout/Event/GetProperty/GetGroupProperties/AboutToWorld +
//!   Version/TextDirection/Status properties + LayoutUpdated signal).
//!
//! Registration with `org.kde.StatusNotifierWatcher` is explicit via
//! [`Tray::register`]. The pure click-ID decode is extracted into
//! [`decode_menu_click`] and unit-tested; the rest is compile-verified
//! scaffolding whose runtime behaviour needs a live StatusNotifierWatcher
//! (waybar / KDE / GNOME shell).

use std::sync::{Arc, Mutex};

use zbus::{
    blocking::Connection,
    interface,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::icon_badge;
use crate::tray_menu;

pub use zbus;

// ── Menu item ID scheme (must match tray_menu) ───────────────────────────

pub const SECTION_MISC: i32 = 1000;
pub const SECTION_LANG: i32 = 2000;
pub const SECTION_ENGINE: i32 = 3000;
pub const SECTION_ORPHAN: i32 = 4000;
pub const SECTION_VOICE: i32 = 5000;
pub const SECTION_PROP: i32 = 6000;
pub const SECTION_CMD: i32 = 7000;

pub const LANG_MAX: i32 = 16;
pub const ENGINE_MAX: i32 = 16;
pub const ENGINE_IN_LANG_MAX: i32 = LANG_MAX * ENGINE_MAX; // 256
pub const ORPHAN_MAX: i32 = 16;
pub const VOICE_MAX: i32 = 16;

pub const ITEM_RESTART: i32 = SECTION_MISC + 1;
pub const ITEM_QUIT: i32 = SECTION_MISC + 2;

/// A decoded menu click — the pure slice of `handle_menu_event`. Indices are
/// resolved to engine/language names by the controller (which has the registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Restart,
    Quit,
    Language(i32),
    EngineInLanguage { lang_idx: i32, engine_idx: i32 },
    OrphanEngine(i32),
    Voice(i32),
    Unknown,
}

/// Actions that can originate from the tray surface (menu clicks or SNI
/// activation gestures). The controller wires the callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Menu(MenuAction),
    Activate,
    SecondaryActivate,
    ScrollUp,
    ScrollDown,
    ContextMenu,
}

pub type ActionHandler = Box<dyn FnMut(TrayAction) + Send>;

/// Decode a dbusmenu item ID into the action it represents. Pure; tested.
pub fn decode_menu_click(id: i32) -> MenuAction {
    if id == ITEM_RESTART {
        return MenuAction::Restart;
    }
    if id == ITEM_QUIT {
        return MenuAction::Quit;
    }
    if (SECTION_ENGINE..SECTION_ENGINE + ENGINE_IN_LANG_MAX).contains(&id) {
        let offset = id - SECTION_ENGINE;
        return MenuAction::EngineInLanguage {
            lang_idx: offset / ENGINE_MAX,
            engine_idx: offset % ENGINE_MAX,
        };
    }
    if (SECTION_ORPHAN..SECTION_ORPHAN + ORPHAN_MAX).contains(&id) {
        return MenuAction::OrphanEngine(id - SECTION_ORPHAN);
    }
    if (SECTION_LANG..SECTION_LANG + LANG_MAX).contains(&id) {
        return MenuAction::Language(id - SECTION_LANG);
    }
    if (SECTION_VOICE..SECTION_VOICE + VOICE_MAX).contains(&id) {
        return MenuAction::Voice(id - SECTION_VOICE);
    }
    MenuAction::Unknown
}

// ── Tray status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayStatus {
    #[default]
    Passive,
    Active,
    NeedsAttention,
}

impl TrayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TrayStatus::Passive => "Passive",
            TrayStatus::Active => "Active",
            TrayStatus::NeedsAttention => "NeedsAttention",
        }
    }
}

/// A rendered badge pixmap: `(width, height, big-endian ARGB32 bytes)`.
/// Matches the SNI `a(iiay)` element signature.
pub type Pixmap = (i32, i32, Vec<u8>);

/// Mutable tray state, shared between the interface structs and the controller.
#[derive(Default)]
pub struct TrayState {
    pub title: Option<String>,
    pub status: TrayStatus,
    pub icon_name: Option<String>,
    pub overlay_icon_name: Option<String>,
    pub icon_theme_path: Option<String>,
    pub tooltip_title: Option<String>,
    pub tooltip_description: Option<String>,
    pub badge_text: Option<String>,
    pub badge_pixmaps: Vec<Pixmap>,
    pub menu_revision: u32,
    pub engine_name: Option<String>,
    pub engine_active: bool,
    pub action_handler: Option<ActionHandler>,
    pub menu_snapshot: Option<crate::tray_menu::RegistrySnapshot>,
}

impl TrayState {
    fn badge_active(&self) -> bool {
        !self.badge_pixmaps.is_empty()
    }

    fn effective_icon_name(&self) -> String {
        if self.badge_active() {
            return String::new();
        }
        self.icon_name
            .clone()
            .unwrap_or_else(|| "typio-keyboard-symbolic".to_string())
    }

    fn pixmap_array(&self) -> Vec<Pixmap> {
        self.badge_pixmaps
            .iter()
            .filter(|(w, h, argb)| !argb.is_empty() && *w > 0 && *h > 0)
            .cloned()
            .collect()
    }
}

// ── org.kde.StatusNotifierItem ───────────────────────────────────────────

/// The SNI interface. Served on `/StatusNotifierItem`. Icon-related properties
/// use `emits_changed = "false"` because SNI hosts read changes via the
/// NewIcon / NewOverlayIcon / NewAttentionIcon signals.
pub struct StatusNotifierItem {
    state: Arc<Mutex<TrayState>>,
}

impl StatusNotifierItem {
    fn new(state: Arc<Mutex<TrayState>>) -> Self {
        StatusNotifierItem { state }
    }
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItem {
    /// Category — always ApplicationStatus for an input method.
    #[zbus(property)]
    fn category(&self) -> &'static str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &'static str {
        "typio"
    }

    #[zbus(property)]
    fn title(&self) -> String {
        let s = self.state.lock().unwrap();
        s.title.clone().unwrap_or_else(|| "Typio".to_string())
    }

    /// Status — emits the NewStatus signal on change (not PropertiesChanged).
    #[zbus(property)]
    fn status(&self) -> String {
        self.state.lock().unwrap().status.as_str().to_string()
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.state.lock().unwrap().effective_icon_name()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .icon_theme_path
            .clone()
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.state.lock().unwrap().pixmap_array()
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> String {
        let s = self.state.lock().unwrap();
        if !s.badge_active() {
            s.overlay_icon_name.clone().unwrap_or_default()
        } else {
            String::new()
        }
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &'static str {
        ""
    }

    /// ToolTip — signature `(sa(iiay)ss)`.
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let s = self.state.lock().unwrap();
        let icon = if s.badge_active() {
            String::new()
        } else {
            s.icon_name
                .clone()
                .unwrap_or_else(|| "typio-keyboard-symbolic".to_string())
        };
        let title = s
            .tooltip_title
            .clone()
            .unwrap_or_else(|| "Typio".to_string());
        let desc = s.tooltip_description.clone().unwrap_or_default();
        (icon, Vec::new(), title, desc)
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/MenuBar").unwrap()
    }

    // Methods — all lenient on coordinates per the SNI spec.
    fn context_menu(&self, _x: i32, _y: i32) {
        self.fire_action("context_menu");
    }

    fn activate(&self, _x: i32, _y: i32) {
        self.fire_action("activate");
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.fire_action("secondary_activate");
    }

    fn scroll(&self, delta: i32, _orientation: &str) {
        self.fire_action(if delta > 0 {
            "scroll_up"
        } else {
            "scroll_down"
        });
    }

    // Signals. The macro generates the emitter taking a `&SignalEmitter`.
    #[zbus(signal)]
    async fn new_icon(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_attention_icon(
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_overlay_icon(emitter: &zbus::object_server::SignalEmitter<'_>)
        -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        status: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_title(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

impl StatusNotifierItem {
    fn fire_action(&self, name: &str) {
        let action = match name {
            "activate" => TrayAction::Activate,
            "secondary_activate" => TrayAction::SecondaryActivate,
            "scroll_up" => TrayAction::ScrollUp,
            "scroll_down" => TrayAction::ScrollDown,
            _ => TrayAction::ContextMenu,
        };
        if let Some(handler) = self.state.lock().unwrap().action_handler.as_mut() {
            handler(action);
        }
    }
}

// ── com.canonical.dbusmenu ───────────────────────────────────────────────

pub struct DbusMenu {
    state: Arc<Mutex<TrayState>>,
}

impl DbusMenu {
    fn new(state: Arc<Mutex<TrayState>>) -> Self {
        DbusMenu { state }
    }
}

/// A dbusmenu layout node: `(id, props, children)`. Serialised as
/// `(ia{sv}av)`; each child in `children` is a variant wrapping a node.
pub type LayoutNode = (
    i32,
    std::collections::HashMap<String, OwnedValue>,
    Vec<OwnedValue>,
);

/// Serialise a [`tray_menu::TrayMenuItem`] tree into a dbusmenu layout node.
fn item_to_layout(item: &tray_menu::TrayMenuItem) -> LayoutNode {
    let mut props: std::collections::HashMap<String, OwnedValue> = std::collections::HashMap::new();

    if let Some(label) = item.label.as_deref().filter(|s| !s.is_empty()) {
        props.insert(
            "label".into(),
            OwnedValue::try_from(Value::from(label.to_string())).unwrap(),
        );
    }
    if let Some(type_) = item.type_str() {
        props.insert(
            "type".into(),
            OwnedValue::try_from(Value::from(type_.to_string())).unwrap(),
        );
    }
    if item.is_submenu_parent {
        props.insert(
            "children-display".into(),
            OwnedValue::try_from(Value::from("submenu".to_string())).unwrap(),
        );
    }
    if item.toggle_state >= 0 {
        props.insert(
            "toggle-type".into(),
            OwnedValue::try_from(Value::from("radio".to_string())).unwrap(),
        );
        props.insert(
            "toggle-state".into(),
            OwnedValue::try_from(Value::from(item.toggle_state)).unwrap(),
        );
    }
    props.insert(
        "enabled".into(),
        OwnedValue::try_from(Value::from(item.enabled)).unwrap(),
    );

    let children: Vec<OwnedValue> = item
        .children
        .iter()
        .map(|c| OwnedValue::try_from(Value::from(item_to_layout(c))).unwrap())
        .collect();

    (item.id, props, children)
}

#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &'static str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &'static str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    /// GetLayout(parent_id, depth, properties) → (revision, layout).
    /// Builds the menu from the current registry snapshot via [`tray_menu::build`]
    /// and serialises it to the dbusmenu `(ia{sv}av)` form.
    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, LayoutNode)> {
        let state = self.state.lock().unwrap();
        let revision = state.menu_revision;
        let layout = if let Some(ref snapshot) = state.menu_snapshot {
            item_to_layout(&tray_menu::build(snapshot, state.engine_name.as_deref()))
        } else {
            item_to_layout(&tray_menu::TrayMenuItem::new_submenu(
                0, None, true, false, None,
            ))
        };
        Ok((revision, layout))
    }

    /// Event(id, type, data, timestamp). Routes "clicked" events through the
    /// ID decode; other events are acknowledged.
    fn event(&self, id: i32, event_type: String, _data: OwnedValue, _time: u32) {
        if event_type != "clicked" {
            return;
        }
        let action = decode_menu_click(id);
        if action == MenuAction::Unknown {
            return;
        }
        if let Some(handler) = self.state.lock().unwrap().action_handler.as_mut() {
            handler(TrayAction::Menu(action));
        }
    }

    fn get_property(&self, _id: i32, _name: String) -> OwnedValue {
        Value::from("")
            .try_into()
            .unwrap_or_else(|_| Value::from(String::new()).try_into().unwrap())
    }

    fn get_group_properties(
        &self,
        _ids: Vec<i32>,
        _names: Vec<String>,
    ) -> Vec<(i32, std::collections::HashMap<String, OwnedValue>)> {
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

// ── Tray controller ──────────────────────────────────────────────────────

/// Owns the session-bus connection + the tray state and exposes the public
/// lifecycle. Created via [`Tray::new`]; call [`Tray::register`] to publish
/// with the StatusNotifierWatcher.
pub struct Tray {
    state: Arc<Mutex<TrayState>>,
    connection: Option<Connection>,
    service_name: String,
    registered: bool,
}

impl Tray {
    /// `typio_tray_new`. Connects to the session bus and serves both
    /// interfaces. Registration is deferred to [`Tray::register`].
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(TrayState::default()));
        let connection = Connection::session().ok();
        if let Some(conn) = &connection {
            let _ = conn.object_server().at(
                "/StatusNotifierItem",
                StatusNotifierItem::new(state.clone()),
            );
            let _ = conn
                .object_server()
                .at("/MenuBar", DbusMenu::new(state.clone()));
        }
        let pid = std::process::id();
        let service_name = format!("org.kde.StatusNotifierItem-{pid}-1");
        Tray {
            state,
            connection,
            service_name,
            registered: false,
        }
    }

    pub fn is_registered(&self) -> bool {
        self.registered
    }

    /// D-Bus unique service name this tray advertises to the SNI watcher.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// `typio_tray_sni_register` — call RegisterStatusNotifierItem on the
    /// watcher. Best-effort; returns false if the watcher is absent.
    pub fn register(&mut self) -> bool {
        let Some(conn) = &self.connection else {
            return false;
        };
        // Claim the well-known name first. The SNI spec says the
        // `service` argument to RegisterStatusNotifierItem is the bus
        // name hosting the /StatusNotifierItem object — watchers use it
        // to look us up, so without requesting the name the watcher
        // can't resolve us and no slot appears in the panel.
        if conn.request_name(self.service_name.as_str()).is_err() {
            return false;
        }
        let res = conn.call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &(&self.service_name),
        );
        self.registered = res.is_ok();
        self.registered
    }

    /// `typio_tray_set_status`.
    pub fn set_status(&self, status: TrayStatus) {
        let mut s = self.state.lock().unwrap();
        if s.status == status {
            return;
        }
        s.status = status;
        drop(s);
        self.emit_new_status();
    }

    /// `typio_tray_set_icon`. A non-empty named icon supersedes any badge.
    pub fn set_icon(&self, icon_name: Option<&str>) {
        let proposed = icon_name
            .filter(|n| !n.is_empty())
            .unwrap_or("typio-keyboard-symbolic");
        let mut s = self.state.lock().unwrap();
        let had_badge = s.badge_active();
        let same = s.icon_name.as_deref() == Some(proposed);
        if same && !had_badge {
            return;
        }
        s.badge_pixmaps.clear();
        s.badge_text = None;
        s.icon_name = Some(proposed.to_string());
        drop(s);
        self.emit_new_icon();
        if had_badge {
            self.emit_new_overlay_icon();
        }
    }

    /// `typio_tray_set_badge` (ADR-0032). Renders the badge text at the SNI
    /// size ladder via [`icon_badge`] and drives IconPixmap.
    pub fn set_badge(&self, badge_text: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        let had_badge = s.badge_active();
        match badge_text {
            None | Some("") => {
                if had_badge {
                    s.badge_pixmaps.clear();
                    s.badge_text = None;
                    drop(s);
                    self.emit_new_icon();
                    self.emit_new_overlay_icon();
                }
            }
            Some(t) => {
                if had_badge && s.badge_text.as_deref() == Some(t) {
                    return;
                }
                drop(s);
                // Render the size ladder via the (already-ported) CPU badge
                // rasteriser. SNI carries raw ARGB32 bitmaps, not GPU textures.
                let sizes = [16u32, 22, 24, 32, 44, 48, 64, 96, 128];
                let pixmaps: Vec<Pixmap> = icon_badge::render(t, &sizes, 0x00FF_FFFFu32)
                    .into_iter()
                    .map(|p| (p.width, p.height, p.argb))
                    .collect();
                let mut s = self.state.lock().unwrap();
                s.badge_pixmaps = pixmaps;
                s.badge_text = Some(t.to_string());
                drop(s);
                self.emit_new_icon();
                if !had_badge {
                    self.emit_new_overlay_icon();
                }
            }
        }
    }

    /// `typio_tray_set_overlay_icon`.
    pub fn set_overlay_icon(&self, icon_name: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        let want = icon_name.filter(|n| !n.is_empty());
        let have = s.overlay_icon_name.as_deref();
        match (want, have) {
            (None, None) => return,
            (Some(a), Some(b)) if a == b => return,
            _ => {
                s.overlay_icon_name = want.map(str::to_string);
            }
        }
        drop(s);
        self.emit_new_overlay_icon();
    }

    /// `typio_tray_set_tooltip`.
    pub fn set_tooltip(&self, title: Option<&str>, description: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        s.tooltip_title = title.map(str::to_string);
        s.tooltip_description = description.map(str::to_string);
        drop(s);
        self.emit_new_tool_tip();
    }

    /// `typio_tray_update_engine`.
    pub fn update_engine(&self, engine_name: Option<&str>, is_active: bool) {
        let tooltip_title = match engine_name {
            Some(n) => format!("Typio - {n}{}", if is_active { " (active)" } else { "" }),
            None => "Typio - No engine".to_string(),
        };
        {
            let mut s = self.state.lock().unwrap();
            s.engine_name = engine_name.map(str::to_string);
            s.engine_active = is_active;
            s.menu_revision = s.menu_revision.wrapping_add(1);
            s.tooltip_title = Some(tooltip_title);
        }
        self.emit_layout_updated();
        self.emit_new_tool_tip();
        self.set_status(if is_active {
            TrayStatus::Active
        } else {
            TrayStatus::Passive
        });
    }

    /// `typio_tray_invalidate_menu`.
    pub fn invalidate_menu(&self) {
        let rev = {
            let mut s = self.state.lock().unwrap();
            s.menu_revision = s.menu_revision.wrapping_add(1);
            s.menu_revision
        };
        self.emit_layout_updated_at(rev);
    }

    /// Install the handler called for menu clicks and SNI activation gestures.
    pub fn set_action_handler<F: FnMut(TrayAction) + Send + 'static>(&self, handler: F) {
        self.state.lock().unwrap().action_handler = Some(Box::new(handler));
    }

    /// Replace the registry snapshot used to build the dbusmenu layout and emit
    /// a `LayoutUpdated` signal so hosts re-request the menu.
    pub fn set_menu_snapshot(&self, snapshot: crate::tray_menu::RegistrySnapshot) {
        let rev = {
            let mut s = self.state.lock().unwrap();
            s.menu_snapshot = Some(snapshot);
            s.menu_revision = s.menu_revision.wrapping_add(1);
            s.menu_revision
        };
        self.emit_layout_updated_at(rev);
    }

    fn emit_new_icon(&self) {
        self.with_emitter(|emitter| {
            let _ = zbus::block_on(StatusNotifierItem::new_icon(emitter));
        });
    }
    fn emit_new_overlay_icon(&self) {
        self.with_emitter(|emitter| {
            let _ = zbus::block_on(StatusNotifierItem::new_overlay_icon(emitter));
        });
    }
    fn emit_new_status(&self) {
        let status = self.state.lock().unwrap().status;
        self.with_emitter(|emitter| {
            let _ = zbus::block_on(StatusNotifierItem::new_status(emitter, status.as_str()));
        });
    }
    fn emit_new_tool_tip(&self) {
        self.with_emitter(|emitter| {
            let _ = zbus::block_on(StatusNotifierItem::new_tool_tip(emitter));
        });
    }
    fn emit_layout_updated(&self) {
        let rev = self.state.lock().unwrap().menu_revision;
        self.emit_layout_updated_at(rev);
    }
    fn emit_layout_updated_at(&self, rev: u32) {
        if let Some(conn) = &self.connection {
            if let Ok(iface) = conn.object_server().interface::<_, DbusMenu>("/MenuBar") {
                let _ = zbus::block_on(DbusMenu::layout_updated(iface.signal_emitter(), rev, 0));
            }
        }
    }
    /// Resolve the SNI signal emitter and hand it to `f`.
    fn with_emitter<F: FnOnce(&zbus::object_server::SignalEmitter)>(&self, f: F) {
        if let Some(conn) = &self.connection {
            if let Ok(iface) = conn
                .object_server()
                .interface::<_, StatusNotifierItem>("/StatusNotifierItem")
            {
                f(iface.signal_emitter());
            }
        }
    }
}

impl Default for Tray {
    fn default() -> Self {
        Tray::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_misc_actions() {
        assert_eq!(decode_menu_click(ITEM_RESTART), MenuAction::Restart);
        assert_eq!(decode_menu_click(ITEM_QUIT), MenuAction::Quit);
    }

    #[test]
    fn decode_language_section() {
        assert_eq!(decode_menu_click(SECTION_LANG), MenuAction::Language(0));
        assert_eq!(decode_menu_click(SECTION_LANG + 3), MenuAction::Language(3));
        assert_eq!(
            decode_menu_click(SECTION_LANG + LANG_MAX - 1),
            MenuAction::Language(LANG_MAX - 1)
        );
        // Just past the section boundary is unknown.
        assert_eq!(
            decode_menu_click(SECTION_LANG + LANG_MAX),
            MenuAction::Unknown
        );
    }

    #[test]
    fn decode_engine_in_language_uses_composite_formula() {
        // ADR-0034: id = SECTION_ENGINE + lang_idx * ENGINE_MAX + engine_idx.
        let id = SECTION_ENGINE + 2 * ENGINE_MAX + 5;
        assert_eq!(
            decode_menu_click(id),
            MenuAction::EngineInLanguage {
                lang_idx: 2,
                engine_idx: 5
            }
        );
        let id = SECTION_ENGINE;
        assert_eq!(
            decode_menu_click(id),
            MenuAction::EngineInLanguage {
                lang_idx: 0,
                engine_idx: 0
            }
        );
    }

    #[test]
    fn decode_orphan_and_voice_sections() {
        assert_eq!(
            decode_menu_click(SECTION_ORPHAN + 1),
            MenuAction::OrphanEngine(1)
        );
        assert_eq!(decode_menu_click(SECTION_VOICE + 4), MenuAction::Voice(4));
    }

    #[test]
    fn decode_out_of_range_returns_unknown() {
        assert_eq!(decode_menu_click(0), MenuAction::Unknown);
        assert_eq!(decode_menu_click(99999), MenuAction::Unknown);
        assert_eq!(decode_menu_click(SECTION_MISC + 50), MenuAction::Unknown);
    }

    #[test]
    fn status_string_matches_sni_spec() {
        assert_eq!(TrayStatus::Passive.as_str(), "Passive");
        assert_eq!(TrayStatus::Active.as_str(), "Active");
        assert_eq!(TrayStatus::NeedsAttention.as_str(), "NeedsAttention");
    }

    #[test]
    fn badge_active_suppresses_icon_name_and_overlay() {
        let mut s = TrayState {
            icon_name: Some("typio-keyboard-symbolic".into()),
            ..Default::default()
        };
        s.overlay_icon_name = Some("mic".into());
        assert!(!s.badge_active());
        assert_eq!(s.effective_icon_name(), "typio-keyboard-symbolic");

        s.badge_pixmaps.push((22, 22, vec![0u8; 22 * 22 * 4]));
        assert!(s.badge_active());
        assert_eq!(s.effective_icon_name(), ""); // suppressed
                                                 // Overlay reads as empty while badge drives IconPixmap.
        let _overlay = if !s.badge_active() {
            s.overlay_icon_name.clone().unwrap_or_default()
        } else {
            String::new()
        };
        assert!(_overlay.is_empty());
    }

    #[test]
    fn pixmap_array_filters_invalid_entries() {
        let mut s = TrayState::default();
        s.badge_pixmaps.push((0, 0, vec![])); // filtered
        s.badge_pixmaps.push((-1, 22, vec![1u8])); // filtered (w<=0)
        s.badge_pixmaps.push((22, 22, vec![0u8; 22 * 22 * 4])); // kept
        assert_eq!(s.pixmap_array().len(), 1);
    }
}
