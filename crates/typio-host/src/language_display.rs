//! Language presentation: endonyms, icon-sized badges, disambiguated menu
//! labels, and the tray/indicator status-icon resolution chain (ADR-0031/33).
//!
//! Port of the *pure* slice of `src/state/controller.c`. The registry exposes
//! only the raw BCP-47 tag; libtypio has no language-display API, so the host
//! owns this presentation table. Everything here is pure over its inputs —
//! no `TypioInstance`, no registry, no config object — so it is unit-testable
//! without fixtures. The stateful `TypioStateController` (listeners, snapshot,
//! instance wiring) is a separate, not-yet-ported concern.

/// Single source of truth for language display strings (ADR-0033). Both the
/// endonym and the badge come from one row so adding a language is one edit.
/// `prefix` matches the BCP-47 primary subtag; any tag whose first separator
/// (`'\0'` / `'-'` / `'_'`) lands exactly after the prefix matches, so script
/// and region suffixes are ignored. Order matters: longer prefixes first so
/// "ary" wins over "ar" and "nb"/"nn" win over "no".
const LANGUAGE_DISPLAY: &[(&str, &str, &str)] = &[
    ("ary", "الدارجة", "الد"), // Moroccan Darija (layout-only)
    ("ar", "العربية", "ع"),
    ("bn", "বাংলা", "বা"),
    ("ca", "Català", "CA"),
    ("cs", "Čeština", "ČE"),
    ("da", "Dansk", "DA"),
    ("de", "Deutsch", "DE"),
    ("el", "Ελληνικά", "Ελ"),
    ("en", "English", "EN"),
    ("es", "Español", "ES"),
    ("fa", "فارسی", "ف"),
    ("fi", "Suomi", "FI"),
    ("fr", "Français", "FR"),
    ("he", "עברית", "א"),
    ("hi", "हिन्दी", "हि"),
    ("hu", "Magyar", "MA"),
    ("id", "Indonesia", "ID"),
    ("it", "Italiano", "IT"),
    ("ja", "日本語", "あ"),
    ("ko", "한국어", "한"),
    ("nb", "Bokmål", "BO"), // Norwegian Bokmål — before "no"
    ("nl", "Nederlands", "NE"),
    ("nn", "Nynorsk", "NY"), // Norwegian Nynorsk — before "no"
    ("no", "Norsk", "NO"),
    ("pl", "Polski", "PL"),
    ("pt", "Português", "PT"),
    ("ro", "Română", "RO"),
    ("ru", "Русский", "Рус"),
    ("sk", "Slovenčina", "SK"),
    ("sv", "Svenska", "SV"),
    ("th", "ไทย", "ไ"),
    ("tr", "Türkçe", "TÜ"),
    ("uk", "Українська", "УК"),
    ("vi", "Tiếng Việt", "VI"),
    ("zh", "中文", "中"),
    ("yue", "粵語", "粵"),   // Cantonese — rime-supported
    ("wuu", "吳語", "吳"),   // Wu — rime-supported
    ("nan", "閩南語", "閩"), // Min Nan — rime-supported
    ("hak", "客家話", "客"), // Hakka — rime-supported
];

/// Human-readable qualifiers for the ISO-15924 script subtags most likely to
/// appear in a tag with multiple script variants. Curated, not exhaustive:
/// 4-letter scripts not listed here pass through verbatim.
const SCRIPT_DISPLAY: &[(&str, &str)] = &[
    ("Hans", "简"), // Simplified Han
    ("Hant", "繁"), // Traditional Han
    ("Latn", "Latin"),
    ("Cyrl", "Cyrillic"),
    ("Arab", "Arabic"),
    ("Hebr", "Hebrew"),
    ("Deva", "Devanagari"),
    ("Beng", "Bengali"),
    ("Grek", "Greek"),
    ("Hang", "Hangul"),
    ("Hira", "Hiragana"),
    ("Kana", "Katakana"),
    ("Thai", "Thai"),
    ("Tibt", "Tibetan"),
];

/// Return the table row whose prefix matches the primary subtag of `tag`.
fn display_lookup(tag: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    if tag.is_empty() {
        return None;
    }
    let bytes = tag.as_bytes();
    LANGUAGE_DISPLAY.iter().find(|(prefix, _, _)| {
        let n = prefix.len();
        bytes.len() >= n
            && &tag[..n] == *prefix
            && (bytes.len() == n || bytes[n] == b'-' || bytes[n] == b'_')
    })
}

/// The endonym (short native display name) for a tag. Known tags return their
/// table endonym; unlisted-but-nonempty tags return the tag itself (so new
/// languages still render); empty tags return `None` (callers treat `None` as
/// "no language set"). Script/region suffixes don't change the endonym.
pub fn language_endonym(tag: &str) -> Option<&str> {
    if tag.is_empty() {
        return None;
    }
    Some(display_lookup(tag).map(|row| row.1).unwrap_or(tag))
}

/// Compact one-to-three glyph badge for a tag (e.g. `中` / `あ` / `EN`). Known
/// tags use their table badge; unlisted tags fall back to the uppercased
/// primary subtag (e.g. `ary-x` → `ARY`). Empty tags yield an empty string.
pub fn language_badge(tag: &str) -> String {
    if tag.is_empty() {
        return String::new();
    }
    if let Some(row) = display_lookup(tag) {
        return row.2.to_string();
    }
    // Fallback: the uppercased primary subtag.
    tag.chars()
        .take_while(|&c| c != '-' && c != '_')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Disambiguated label for list/menu surfaces: the endonym plus a script
/// qualifier when the tag carries an ISO-15924 script subtag
/// (`zh-Hans` → `中文 (简)`, `sr-Latn` → `sr-Latn (Latin)`). Tags with only a
/// primary or region subtag collapse to the bare endonym.
pub fn language_menu_label(tag: &str) -> String {
    let endonym = language_endonym(tag).unwrap_or(tag);

    // BCP-47 separator: prefer '-', fall back to '_'.
    let sep = tag.find('-').or_else(|| tag.find('_'));
    let mut script_qual: Option<&str> = None;
    if let Some(sep) = sep {
        let rest = &tag[sep + 1..];
        // Scripts are exactly 4 letters, title-cased (Hans, Latn, Cyrl…).
        if rest.len() == 4 {
            let b = rest.as_bytes();
            let is_title_alpha = b[0].is_ascii_uppercase()
                && b[1].is_ascii_lowercase()
                && b[2].is_ascii_lowercase()
                && b[3].is_ascii_lowercase();
            if is_title_alpha {
                // Unknown script: pass the raw subtag through so entries stay
                // distinguishable instead of collapsing.
                script_qual = Some(script_qualifier_lookup(rest).unwrap_or(rest));
            }
        }
    }

    match script_qual {
        Some(qual) => format!("{endonym} ({qual})"),
        None => endonym.to_string(),
    }
}

fn script_qualifier_lookup(code: &str) -> Option<&'static str> {
    SCRIPT_DISPLAY
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, q)| *q)
}

/// The resolved tray/indicator status icon plus optional badge text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIcon {
    /// Freedesktop icon name. When `badge_text` is `Some`, this is the
    /// render-failure fallback name; the caller should draw the badge text.
    pub icon: String,
    /// Set when the icon resolved to the language badge (layer 2) and should
    /// be drawn as a text pixmap rather than looked up by name (ADR-0032).
    pub badge_text: Option<String>,
}

impl ResolvedIcon {
    /// True when the icon resolved to a rendered badge (ADR-0032).
    pub fn is_badge(&self) -> bool {
        self.badge_text.is_some()
    }
}

/// Resolve the tray/indicator status icon by the language-only chain
/// (ADR-0033), most-specific first:
///
///   1. `[languages.<tag>].icon` config override (via `cfg_icon`)
///   2. language badge (rendered text)
///   3. generic `typio-keyboard-symbolic` (something active, no icon found)
///   4. `typio-keyboard-off-symbolic` (nothing active)
///
/// `cfg_icon` is a caller-provided lookup mapping a full config key (e.g.
/// `"languages.zh.icon"`) to its value — keeping this function pure and
/// independent of the libtypio config object.
pub type ConfigIconProvider<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn resolve_language_icon(
    active_language_tag: Option<&str>,
    engine_active: bool,
    cfg_icon: Option<ConfigIconProvider>,
) -> ResolvedIcon {
    if let Some(tag) = active_language_tag.filter(|t| !t.is_empty()) {
        // 1. Explicit per-language icon override (exact-tag key).
        if let Some(lookup) = cfg_icon {
            let key = format!("languages.{tag}.icon");
            if let Some(icon) = lookup(&key).filter(|s| !s.is_empty()) {
                return ResolvedIcon {
                    icon,
                    badge_text: None,
                };
            }
        }
        // 2. Rendered badge; generic name kept as the render-failure fallback.
        let badge = language_badge(tag);
        if !badge.is_empty() {
            return ResolvedIcon {
                icon: "typio-keyboard-symbolic".to_string(),
                badge_text: Some(badge),
            };
        }
    }

    // 3. Active but iconless, or 4. nothing active.
    ResolvedIcon {
        icon: if engine_active {
            "typio-keyboard-symbolic"
        } else {
            "typio-keyboard-off-symbolic"
        }
        .to_string(),
        badge_text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── endonym ──────────────────────────────────────────────────────────

    #[test]
    fn endonym_known() {
        assert_eq!(language_endonym("en"), Some("English"));
        assert_eq!(language_endonym("ja"), Some("日本語"));
        assert_eq!(language_endonym("ar"), Some("العربية"));
        assert_eq!(language_endonym("ary"), Some("الدارجة"));
        assert_eq!(language_endonym("uk"), Some("Українська"));
        assert_eq!(language_endonym("vi"), Some("Tiếng Việt"));
        assert_eq!(language_endonym("pt"), Some("Português"));
        assert_eq!(language_endonym("hi"), Some("हिन्दी"));
    }

    #[test]
    fn endonym_matches_primary_subtag() {
        assert_eq!(language_endonym("zh-Hans"), Some("中文"));
        assert_eq!(language_endonym("zh-Hant"), Some("中文"));
        assert_eq!(language_endonym("en-US"), Some("English"));
        assert_eq!(language_endonym("pt-BR"), Some("Português"));
    }

    #[test]
    fn endonym_unknown_falls_back_to_tag() {
        assert_eq!(language_endonym("xx-Region"), Some("xx-Region"));
    }

    #[test]
    fn endonym_empty_is_none() {
        assert_eq!(language_endonym(""), None);
    }

    // ── badge ────────────────────────────────────────────────────────────

    #[test]
    fn badge_known() {
        assert_eq!(language_badge("zh"), "中");
        assert_eq!(language_badge("en"), "EN");
        assert_eq!(language_badge("ary"), "الد");
        assert_eq!(language_badge("ru"), "Рус");
        assert_eq!(language_badge("el"), "Ελ");
        assert_eq!(language_badge("bn"), "বা");
        assert_eq!(language_badge("th"), "ไ");
    }

    #[test]
    fn badge_uppercase_fallback() {
        assert_eq!(language_badge("xx"), "XX");
        assert_eq!(language_badge("xx-Latn"), "XX");
        assert_eq!(language_badge("xyz-foo"), "XYZ");
    }

    #[test]
    fn badge_empty() {
        assert_eq!(language_badge(""), "");
    }

    // ── menu label ───────────────────────────────────────────────────────

    #[test]
    fn menu_label_no_script() {
        assert_eq!(language_menu_label("zh"), "中文");
        assert_eq!(language_menu_label("en"), "English");
        assert_eq!(language_menu_label("en-US"), "English");
    }

    #[test]
    fn menu_label_script_disambiguation() {
        assert_eq!(language_menu_label("zh-Hans"), "中文 (简)");
        assert_eq!(language_menu_label("zh-Hant"), "中文 (繁)");
        assert_eq!(language_menu_label("sr-Latn"), "sr-Latn (Latin)");
        assert_eq!(language_menu_label("sr-Cyrl"), "sr-Cyrl (Cyrillic)");
        assert_eq!(language_menu_label("uz-Arab"), "uz-Arab (Arabic)");
        assert_eq!(language_menu_label("bn-Deva"), "বাংলা (Devanagari)");
        assert_eq!(language_menu_label("ja-Hira"), "日本語 (Hiragana)");
    }

    #[test]
    fn menu_label_unknown_script_passthrough() {
        assert_eq!(language_menu_label("en-Abcd"), "English (Abcd)");
        assert_eq!(language_menu_label("en-Foo"), "English");
    }

    #[test]
    fn menu_label_underscore_separator() {
        assert_eq!(language_menu_label("zh_Hans"), "中文 (简)");
    }

    #[test]
    fn menu_label_empty() {
        assert_eq!(language_menu_label(""), "");
    }

    // ── resolve_language_icon ────────────────────────────────────────────

    #[test]
    fn icon_layer4_nothing_active() {
        let r = resolve_language_icon(None, false, None);
        assert_eq!(r.icon, "typio-keyboard-off-symbolic");
        assert!(!r.is_badge());
    }

    #[test]
    fn icon_layer3_engine_only_no_language() {
        let r = resolve_language_icon(None, true, None);
        assert_eq!(r.icon, "typio-keyboard-symbolic");
        assert!(!r.is_badge());
    }

    #[test]
    fn icon_layer2_language_badge() {
        let r = resolve_language_icon(Some("zh"), false, None);
        assert_eq!(r.icon, "typio-keyboard-symbolic");
        assert_eq!(r.badge_text.as_deref(), Some("中"));

        let r = resolve_language_icon(Some("zh-Hans"), false, None);
        assert_eq!(r.badge_text.as_deref(), Some("中"));

        let r = resolve_language_icon(Some("en"), false, None);
        assert_eq!(r.badge_text.as_deref(), Some("EN"));
    }

    #[test]
    fn icon_layer2_unknown_tag_uppercase_fallback() {
        let r = resolve_language_icon(Some("xx"), false, None);
        assert_eq!(r.icon, "typio-keyboard-symbolic");
        assert_eq!(r.badge_text.as_deref(), Some("XX"));

        let r = resolve_language_icon(Some("xx-Latn"), false, None);
        assert_eq!(r.badge_text.as_deref(), Some("XX"));
    }

    #[test]
    fn icon_layer1_config_override() {
        let cfg = |key: &str| {
            if key == "languages.zh.icon" {
                Some("my-zh-icon-symbolic".to_string())
            } else {
                None
            }
        };
        let r = resolve_language_icon(Some("zh"), false, Some(&cfg));
        assert_eq!(r.icon, "my-zh-icon-symbolic");
        assert!(!r.is_badge());

        // Override for one tag does not leak into another.
        let r = resolve_language_icon(Some("en"), false, Some(&cfg));
        assert_eq!(r.badge_text.as_deref(), Some("EN"));
    }

    #[test]
    fn icon_layer1_config_override_is_exact_tag() {
        let cfg = |key: &str| {
            if key == "languages.zh-Hans.icon" {
                Some("hans-only-icon".to_string())
            } else {
                None
            }
        };
        let r = resolve_language_icon(Some("zh-Hans"), false, Some(&cfg));
        assert_eq!(r.icon, "hans-only-icon");
        assert!(!r.is_badge());

        // zh (primary) does NOT pick up the zh-Hans override.
        let r = resolve_language_icon(Some("zh"), false, Some(&cfg));
        assert_eq!(r.badge_text.as_deref(), Some("中"));
    }

    #[test]
    fn icon_null_config_is_safe() {
        let r = resolve_language_icon(Some("ja"), false, None);
        assert_eq!(r.badge_text.as_deref(), Some("あ"));
    }
}
