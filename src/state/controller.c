/**
 * @file controller.c
 * @brief Centralised state snapshot + change broadcast for runtime surfaces.
 *
 * ─── Push vs pull: a known asymmetry ─────────────────────────────────────
 *
 * The controller holds TWO kinds of state, and the difference matters for
 * anyone touching the icon resolver or the language-change broadcasts:
 *
 *   • Pushed (cached here):  keyboard/voice engine identity, engine mode.
 *     libtypio fires `engine_changed` / `voice_engine_changed` / `status_*`
 *     callbacks; the controller's `notify_*` handlers cache the value and
 *     broadcast. The cache IS the source of truth for these — surfaces read
 *     `typio_state_controller_get_active_engine_name` etc. without round-
 *     tripping to the registry.
 *
 *   • Pulled (queried live):  the ACTIVE LANGUAGE. libtypio has no dedicated
 *     language-changed callback (see ADR-0031 "Negative (accepted)"); the
 *     registry resolves the language as a side effect of engine activation,
 *     so the host detects language transitions by diffing
 *     `typio_registry_get_active_language` against the cached snapshot in
 *     `typio_state_controller_refresh_language` after every engine callback.
 *
 * Consequence for icon resolution: `typio_resolve_language_icon` is pure over
 * `(tag, engine_active, cfg)`, but the WRAPPER
 * `typio_state_controller_resolve_status_icon` queries the registry for the
 * live tag rather than reading `ctrl->active_language`. This is deliberate —
 * it ensures the icon tracks the actual registry state on the diff path,
 * not a possibly-stale snapshot. If libtypio ever adds a language-changed
 * callback, the diff collapses into a push handler and the wrapper can read
 * the snapshot instead.
 *
 * Do not introduce a second icon-resolution path that bypasses the wrapper
 * (the original `typio_state_controller_sync` had one; it produced an
 * icon/engine mismatch at startup until ADR-0033 unified both paths).
 */

#include "state/controller.h"
#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"
#include "typio/abi/config.h"
#include "typio/abi/engine.h"
#include "typio/abi/string.h"
#include "typio/typio.h"
#include "typio/abi/log.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct TypioStateController {
    TypioInstance *instance;

    /* -- cached state snapshots ------------------------------------------- */
    char *active_engine_name;
    char *active_engine_display_name;
    char *active_voice_engine_name;
    char *active_voice_engine_display_name;
    char *active_language;
    char *status_icon;
    /* When the icon resolves to the language floor (ADR-0032), the tray renders
     * a text badge instead of looking up status_icon as a freedesktop name.
     * status_icon then holds the generic name as a render-failure fallback. */
    bool  status_icon_is_badge;
    char *status_badge_text;

    bool engine_active;

    bool has_status;
    TypioKeyboardEngineMode status;
    char *status_id;
    char *status_label;
    char *status_display_label;
    char *status_icon_name;

    /* -- listeners -------------------------------------------------------- */
    TypioStateListener *listeners;
    size_t listener_count;
    size_t listener_capacity;
};

/* -------------------------------------------------------------------------- */
/* Helpers                                                                    */
/* -------------------------------------------------------------------------- */

static char *typio_state_strdup(const char *src) {
    if (!src || !*src) {
        return nullptr;
    }
    return strdup(src);
}

/* Resolve the tray/indicator status icon by a language-only precedence chain.
 * The tray icon encodes the active language and nothing else; engine identity
 * rides the tooltip and menu. Engine manifest icons and engine-pushed mode
 * icons remain defined in the manifest and `typio_instance_get_last_status_icon`
 * for other consumers (panel, settings), but are deliberately not consumed
 * here.
 *
 *   1. [languages.<tag>].icon config                — explicit per-language icon
 *   2. language badge                               — rendered text, the floor
 *   3. generic typio-keyboard-symbolic              — active, no icon anywhere
 *   4. typio-keyboard-off-symbolic                  — nothing active
 *
 * Pure over inputs; see `typio_resolve_language_icon` for the public entry.
 * Returns a freshly allocated string (never NULL); caller owns it. */
char *typio_resolve_language_icon(const char *active_language_tag,
                                 bool engine_active,
                                 TypioConfig *cfg,
                                 bool *out_is_badge,
                                 char **out_badge_text) {
    if (!out_is_badge || !out_badge_text) {
        /* Defensive: still return a non-NULL icon so callers can free
         * unconditionally. */
        return strdup(engine_active
                          ? "typio-keyboard-symbolic"
                          : "typio-keyboard-off-symbolic");
    }
    *out_is_badge = false;
    *out_badge_text = nullptr;

    /* 1/2. Language layers: a configured per-language icon wins, else the
     *      rendered badge. Layout-only languages (empty keyboard slot) and
     *      engine-backed languages alike resolve to a meaningful "on" glyph. */
    if (active_language_tag && active_language_tag[0]) {
        if (cfg) {
            char key[160];
            snprintf(key, sizeof(key), "languages.%s.icon", active_language_tag);
            const char *cfg_icon = typio_config_get_string(cfg, key, nullptr);
            if (cfg_icon && cfg_icon[0]) {
                return strdup(cfg_icon); /* 1. explicit per-language icon */
            }
        }
        char badge[32];
        typio_language_badge(active_language_tag, badge, sizeof(badge));
        if (badge[0]) {
            *out_is_badge = true;
            *out_badge_text = strdup(badge);
            return strdup("typio-keyboard-symbolic"); /* 2. badge; name as fallback */
        }
        /* No badge glyph for this tag: fall through to the generic "on". */
    }
    /* 3. Engine or language active but iconless. 4. Nothing active. */
    return strdup(engine_active
                      ? "typio-keyboard-symbolic"
                      : "typio-keyboard-off-symbolic");
}

/* Thin wrapper that pulls inputs from the controller's snapshot + the live
 * registry/config, then writes the badge state back into the controller. */
static char *typio_state_controller_resolve_status_icon(TypioStateController *ctrl) {
    TypioRegistry *registry =
        ctrl->instance ? typio_instance_get_registry(ctrl->instance) : nullptr;
    char *tag = registry ? typio_registry_get_active_language(registry) : nullptr;
    TypioConfig *cfg =
        ctrl->instance ? typio_instance_get_config(ctrl->instance) : nullptr;

    /* Reset badge state — the resolver reassigns when it picks layer 2. */
    ctrl->status_icon_is_badge = false;
    free(ctrl->status_badge_text);
    ctrl->status_badge_text = nullptr;

    char *icon = typio_resolve_language_icon(tag, ctrl->engine_active, cfg,
                                             &ctrl->status_icon_is_badge,
                                             &ctrl->status_badge_text);
    free(tag);
    return icon;
}

/* Single source of truth for language display strings (ADR-0033). Both the
 * endonym (short display name) and the badge (icon glyph) come from one
 * table so adding a language means adding one row, not two. `prefix`
 * matches the BCP 47 primary subtag; the lookup accepts any tag whose first
 * separator ('\0' / '-' / '_') lands exactly after the prefix, so script
 * and region suffixes are ignored. Order matters: longer prefixes first so
 * "ary" wins over "ar", "nb"/"nn" win over "no".
 *
 * The set is curated, not exhaustive: it covers the languages most likely
 * to appear in input-method engines. Long-tail tags fall back to the raw
 * tag for endonym and the uppercased primary subtag for badge — visible,
 * just not localised. ICU/cldr integration is the scalable answer and is
 * tracked as future work; until then, prefer adding a row here over
 * special-casing callers. */
static const struct TypioLanguageDisplay {
    const char *prefix;
    const char *endonym;
    const char *badge;
} g_language_display[] = {
    { "ary", "الدارجة",   "الد" },   /* Moroccan Darija (layout-only) */
    { "ar",  "العربية",   "ع" },
    { "bn",  "বাংলা",     "বা" },
    { "ca",  "Català",    "CA" },
    { "cs",  "Čeština",   "ČE" },
    { "da",  "Dansk",     "DA" },
    { "de",  "Deutsch",   "DE" },
    { "el",  "Ελληνικά",  "Ελ" },
    { "en",  "English",   "EN" },
    { "es",  "Español",   "ES" },
    { "fa",  "فارسی",     "ف" },
    { "fi",  "Suomi",     "FI" },
    { "fr",  "Français",  "FR" },
    { "he",  "עברית",     "א" },
    { "hi",  "हिन्दी",     "हि" },
    { "hu",  "Magyar",    "MA" },
    { "id",  "Indonesia", "ID" },
    { "it",  "Italiano",  "IT" },
    { "ja",  "日本語",     "あ" },
    { "ko",  "한국어",     "한" },
    { "nb",  "Bokmål",    "BO" },   /* Norwegian Bokmål — before "no" */
    { "nl",  "Nederlands","NE" },
    { "nn",  "Nynorsk",   "NY" },   /* Norwegian Nynorsk — before "no" */
    { "no",  "Norsk",     "NO" },
    { "pl",  "Polski",    "PL" },
    { "pt",  "Português", "PT" },
    { "ro",  "Română",    "RO" },
    { "ru",  "Русский",   "Рус" },
    { "sk",  "Slovenčina","SK" },
    { "sv",  "Svenska",   "SV" },
    { "th",  "ไทย",       "ไ" },
    { "tr",  "Türkçe",    "TÜ" },
    { "uk",  "Українська","УК" },
    { "vi",  "Tiếng Việt","VI" },
    { "zh",  "中文",       "中" },
};

/* Return the table row whose prefix matches the primary subtag of @p tag, or
 * NULL when no row matches / @p tag is empty. */
static const struct TypioLanguageDisplay *language_display_lookup(
    const char *tag) {
    if (!tag || !tag[0]) {
        return nullptr;
    }
    for (size_t i = 0; i < sizeof(g_language_display) / sizeof(g_language_display[0]); i++) {
        size_t n = strlen(g_language_display[i].prefix);
        if (strncmp(tag, g_language_display[i].prefix, n) == 0 &&
            (tag[n] == '\0' || tag[n] == '-' || tag[n] == '_')) {
            return &g_language_display[i];
        }
    }
    return nullptr;
}

const char *typio_language_endonym(const char *tag) {
    /* Preserve the original contract: NULL or empty tag returns nullptr so
     * callers can treat the result as a "language is set" flag. */
    if (!tag || !tag[0]) {
        return nullptr;
    }
    const struct TypioLanguageDisplay *row = language_display_lookup(tag);
    return row ? row->endonym : tag;
}

void typio_language_badge(const char *tag, char *out, size_t out_size) {
    if (!out || out_size == 0) {
        return;
    }
    out[0] = '\0';
    if (!tag || !tag[0]) {
        return;
    }
    const struct TypioLanguageDisplay *row = language_display_lookup(tag);
    if (row) {
        snprintf(out, out_size, "%s", row->badge);
        return;
    }
    /* Fallback: the uppercased primary subtag (e.g. "ary-x" -> "ARY"). */
    size_t i = 0;
    for (; tag[i] && tag[i] != '-' && tag[i] != '_' && i + 1 < out_size; i++) {
        char c = tag[i];
        out[i] = (c >= 'a' && c <= 'z') ? (char)(c - 'a' + 'A') : c;
    }
    out[i] = '\0';
}

/* Human-readable qualifiers for the ISO 15924 script subtags most likely to
 * appear in a BCP 47 tag with multiple script variants (zh-Hans / zh-Hant,
 * sr-Latn / sr-Cyrl, uz-Latn / uz-Cyrl, …). Entries with a translated short
 * name render as e.g. "中文 (简)"; unlisted codes fall through to the raw
 * 4-letter subtag so the entries remain distinguishable.
 *
 * Curated, not exhaustive: 4-letter scripts not in this table pass through
 * verbatim. Add a row when a script's English name is shorter or clearer
 * than the code (Hans vs. 简 is a judgement call — both work, 简 reads
 * better in a CJK menu). */
static const struct TypioScriptDisplay {
    const char *code;        /* ISO 15924, title-cased as in BCP 47 */
    const char *qualifier;
} g_script_display[] = {
    { "Hans", "简" },        /* Simplified Han */
    { "Hant", "繁" },        /* Traditional Han */
    { "Latn", "Latin" },
    { "Cyrl", "Cyrillic" },
    { "Arab", "Arabic" },
    { "Hebr", "Hebrew" },
    { "Deva", "Devanagari" },
    { "Beng", "Bengali" },
    { "Grek", "Greek" },
    { "Hang", "Hangul" },
    { "Hira", "Hiragana" },
    { "Kana", "Katakana" },
    { "Thai", "Thai" },
    { "Tibt", "Tibetan" },
};

static const char *script_qualifier_lookup(const char *s) {
    if (!s) {
        return nullptr;
    }
    for (size_t i = 0; i < sizeof(g_script_display) / sizeof(g_script_display[0]); i++) {
        if (strcmp(s, g_script_display[i].code) == 0) {
            return g_script_display[i].qualifier;
        }
    }
    return nullptr;
}

/* Build a disambiguated language label for surfaces that list multiple
 * languages side-by-side (notably the tray menu): endonym plus a script
 * qualifier when the tag carries an ISO 15924 script subtag (e.g.
 * "zh-Hans" -> "中文 (简)", "zh-Hant" -> "中文 (繁)"). Tags with only a
 * primary subtag or a region subtag collapse to the bare endonym, so the
 * common case is unchanged. `out` always receives a NUL-terminated string. */
void typio_language_menu_label(const char *tag, char *out, size_t out_size) {
    if (!out || out_size == 0) {
        return;
    }
    const char *endonym = typio_language_endonym(tag);
    if (!endonym) {
        endonym = tag ? tag : "";
    }

    const char *script_qual = nullptr;
    const char *dash = tag ? strchr(tag, '-') : nullptr;
    const char *underscore = tag ? strchr(tag, '_') : nullptr;
    const char *sep = dash ? dash : underscore;
    if (sep && strlen(sep + 1) == 4) {
        const char *s = sep + 1;
        /* BCP 47 scripts are 4 letters, title-cased (Hans, Hant, Cyrl, Latn). */
        bool is_alpha = (s[0] >= 'A' && s[0] <= 'Z') &&
                        (s[1] >= 'a' && s[1] <= 'z') &&
                        (s[2] >= 'a' && s[2] <= 'z') &&
                        (s[3] >= 'a' && s[3] <= 'z');
        if (is_alpha) {
            script_qual = script_qualifier_lookup(s);
            if (!script_qual) {
                /* Unknown script: fall back to the raw 4-letter subtag so the
                 * entries remain distinguishable instead of collapsing. */
                script_qual = s;
            }
        }
    }

    if (script_qual) {
        snprintf(out, out_size, "%s (%s)", endonym, script_qual);
    } else {
        snprintf(out, out_size, "%s", endonym);
    }
}

static void typio_state_controller_broadcast(TypioStateController *ctrl,
                                             TypioStateChangeType change_type) {
    if (!ctrl) {
        return;
    }
    for (size_t i = 0; i < ctrl->listener_count; i++) {
        TypioStateListener *l = &ctrl->listeners[i];
        if (l->callback) {
            l->callback(l->user_data, change_type);
        }
    }
}

/* Refresh the active-language snapshot from the registry. Returns true when
 * the language changed. The registry has no dedicated language callback;
 * every language activation fires the keyboard/voice engine callbacks, so
 * diffing here catches all transitions — including layout-only languages
 * where the new slot state is "no engine" (ADR-0031). */
static bool typio_state_controller_refresh_language(TypioStateController *ctrl) {
    if (!ctrl || !ctrl->instance) {
        return false;
    }
    TypioRegistry *registry = typio_instance_get_registry(ctrl->instance);
    char *lang = registry ? typio_registry_get_active_language(registry) : nullptr;
    bool changed;
    if (!lang || !ctrl->active_language) {
        changed = (lang != nullptr) != (ctrl->active_language != nullptr);
    } else {
        changed = strcmp(lang, ctrl->active_language) != 0;
    }
    if (changed) {
        free(ctrl->active_language);
        ctrl->active_language = typio_state_strdup(lang);
    }
    typio_free_string(lang);
    return changed;
}

static void typio_state_controller_update_engine_active(
    TypioStateController *ctrl) {
    if (!ctrl || !ctrl->instance) {
        return;
    }
    TypioRegistry *registry = typio_instance_get_registry(ctrl->instance);
    char *active_name =
        registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    ctrl->engine_active = active_name != nullptr;
    typio_free_string(active_name);
}

static void typio_state_controller_clear_mode(TypioStateController *ctrl) {
    free(ctrl->status_id);
    free(ctrl->status_label);
    free(ctrl->status_display_label);
    free(ctrl->status_icon_name);
    ctrl->status_id = nullptr;
    ctrl->status_label = nullptr;
    ctrl->status_display_label = nullptr;
    ctrl->status_icon_name = nullptr;
    ctrl->has_status = false;
    memset(&ctrl->status, 0, sizeof(ctrl->status));
}

static void typio_state_controller_set_mode(TypioStateController *ctrl,
                                            const TypioKeyboardEngineMode *mode) {
    typio_state_controller_clear_mode(ctrl);
    if (!mode) {
        return;
    }
    ctrl->has_status = true;
    ctrl->status.id = ctrl->status_id = typio_state_strdup(mode->id);
    ctrl->status.label =
        ctrl->status_label = typio_state_strdup(mode->label);
    ctrl->status.display_label =
        ctrl->status_display_label = typio_state_strdup(mode->display_label);
    ctrl->status.icon_name = ctrl->status_icon_name = typio_state_strdup(mode->icon_name);
}

/* -------------------------------------------------------------------------- */
/* Lifecycle                                                                  */
/* -------------------------------------------------------------------------- */

TypioStateController *typio_state_controller_new(TypioInstance *instance) {
    if (!instance) {
        return nullptr;
    }
    TypioStateController *ctrl = calloc(1, sizeof(TypioStateController));
    if (!ctrl) {
        return nullptr;
    }
    ctrl->instance = instance;
    ctrl->listener_capacity = 4;
    ctrl->listeners = calloc(ctrl->listener_capacity, sizeof(TypioStateListener));
    if (!ctrl->listeners) {
        free(ctrl);
        return nullptr;
    }
    return ctrl;
}

void typio_state_controller_free(TypioStateController *ctrl) {
    if (!ctrl) {
        return;
    }
    free(ctrl->active_engine_name);
    free(ctrl->active_engine_display_name);
    free(ctrl->active_voice_engine_name);
    free(ctrl->active_voice_engine_display_name);
    free(ctrl->active_language);
    free(ctrl->status_icon);
    free(ctrl->status_badge_text);
    typio_state_controller_clear_mode(ctrl);
    free(ctrl->listeners);
    free(ctrl);
}

/* -------------------------------------------------------------------------- */
/* Listeners                                                                  */
/* -------------------------------------------------------------------------- */

void typio_state_controller_add_listener(TypioStateController *ctrl,
                                         TypioStateListener listener) {
    if (!ctrl) {
        return;
    }
    if (ctrl->listener_count >= ctrl->listener_capacity) {
        size_t new_cap = ctrl->listener_capacity * 2;
        TypioStateListener *new_list =
            realloc(ctrl->listeners, new_cap * sizeof(TypioStateListener));
        if (!new_list) {
            typio_log_error("Failed to grow state-controller listener list");
            return;
        }
        ctrl->listeners = new_list;
        ctrl->listener_capacity = new_cap;
    }
    ctrl->listeners[ctrl->listener_count++] = listener;
}

void typio_state_controller_remove_listener(TypioStateController *ctrl,
                                            void *user_data) {
    if (!ctrl) {
        return;
    }
    for (size_t i = 0; i < ctrl->listener_count; i++) {
        if (ctrl->listeners[i].user_data == user_data) {
            /* shift remaining entries down */
            size_t rest = ctrl->listener_count - i - 1;
            if (rest > 0) {
                memmove(&ctrl->listeners[i],
                        &ctrl->listeners[i + 1],
                        rest * sizeof(TypioStateListener));
            }
            ctrl->listener_count--;
            return;
        }
    }
}

/* -------------------------------------------------------------------------- */
/* Queries                                                                    */
/* -------------------------------------------------------------------------- */

const char *typio_state_controller_get_active_engine_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_engine_name : nullptr;
}

const char *typio_state_controller_get_active_engine_display_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_engine_display_name : nullptr;
}

const char *typio_state_controller_get_active_voice_engine_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_voice_engine_name : nullptr;
}

const char *typio_state_controller_get_active_voice_engine_display_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_voice_engine_display_name : nullptr;
}

const char *typio_state_controller_get_active_language(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_language : nullptr;
}

const char *typio_state_controller_get_status_icon(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->status_icon : nullptr;
}

bool typio_state_controller_get_status_icon_is_badge(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->status_icon_is_badge : false;
}

const char *typio_state_controller_get_status_badge_text(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->status_badge_text : nullptr;
}

bool typio_state_controller_get_engine_active(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->engine_active : false;
}

const TypioKeyboardEngineMode *typio_state_controller_get_current_status(
    TypioStateController *ctrl) {
    if (!ctrl || !ctrl->has_status) {
        return nullptr;
    }
    return &ctrl->status;
}

/* -------------------------------------------------------------------------- */
/* Core notifications                                                         */
/* -------------------------------------------------------------------------- */

void typio_state_controller_notify_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info) {
    if (!ctrl) {
        return;
    }

    /* Engine identity no longer drives the tray icon (ADR-0033), so we just
     * snapshot name/display_name for downstream consumers (tooltip, IPC). */
    free(ctrl->active_engine_name);
    free(ctrl->active_engine_display_name);
    ctrl->active_engine_name =
        (info && info->name) ? strdup(info->name) : nullptr;
    ctrl->active_engine_display_name =
        (info && info->display_name) ? strdup(info->display_name) : nullptr;

    /* Re-evaluate the status icon via the language-only precedence chain.
     * A layout-only language (info == NULL but a language is active) resolves
     * to an "on" icon rather than the off glyph. The icon no longer takes
     * engine identity into account; engine_pushed_icon and manifest icons
     * stay available for non-tray consumers. */
    free(ctrl->status_icon);
    ctrl->status_icon = typio_state_controller_resolve_status_icon(ctrl);

    typio_state_controller_update_engine_active(ctrl);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_ENGINE);
    if (typio_state_controller_refresh_language(ctrl)) {
        /* Language transitions arrive as a side effect of the keyboard-engine
         * callback (libtypio has no dedicated language callback). The badge
         * depends on the active language, so re-resolve the status icon here
         * too — otherwise switching languages without changing engines
         * leaves the icon stuck on the previous language. */
        free(ctrl->status_icon);
        ctrl->status_icon = typio_state_controller_resolve_status_icon(ctrl);
        typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_LANGUAGE);
        typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
    }
}

void typio_state_controller_notify_voice_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info) {
    if (!ctrl) {
        return;
    }
    free(ctrl->active_voice_engine_name);
    free(ctrl->active_voice_engine_display_name);
    ctrl->active_voice_engine_name =
        (info && info->name) ? strdup(info->name) : nullptr;
    ctrl->active_voice_engine_display_name =
        (info && info->display_name) ? strdup(info->display_name) : nullptr;
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_VOICE_ENGINE);
    if (typio_state_controller_refresh_language(ctrl)) {
        /* Mirror the keyboard path: the badge keys off the active language,
         * so re-resolve on language transitions even when the trigger was a
         * voice-engine change. */
        free(ctrl->status_icon);
        ctrl->status_icon = typio_state_controller_resolve_status_icon(ctrl);
        typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_LANGUAGE);
        typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
    }
}

void typio_state_controller_notify_status_changed(
    TypioStateController *ctrl,
    const TypioKeyboardEngineMode *mode) {
    if (!ctrl) {
        return;
    }
    /* Store the mode for the tooltip (label/display_label) and broadcast so
     * the systray adapter refreshes the tooltip. The mode's `icon_name` is
     * intentionally not consumed: the tray icon encodes the active language
     * only (ADR-0033). Engine-pushed icons remain available to other
     * surfaces via `typio_instance_get_last_status_icon`. */
    typio_state_controller_set_mode(ctrl, mode);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS);
}

void typio_state_controller_notify_status_icon_changed(
    TypioStateController *ctrl,
    const char *icon_name) {
    if (!ctrl) {
        return;
    }
    /* Engine-pushed status icons no longer reach the tray; the tray icon is
     * language-only (ADR-0033). libtypio still emits these events and the
     * latest value remains queryable via `typio_instance_get_last_status_icon`
     * for any non-tray consumer that wants it. Broadcast so future listeners
     * can react without the tray base icon shifting. */
    (void)icon_name;
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
}

/* -------------------------------------------------------------------------- */
/* Sync                                                                       */
/* -------------------------------------------------------------------------- */

void typio_state_controller_sync(TypioStateController *ctrl) {
    if (!ctrl || !ctrl->instance) {
        return;
    }

    TypioRegistry *registry = typio_instance_get_registry(ctrl->instance);

    /* Engine */
    char *active_kb_name = registry
        ? typio_registry_get_active_keyboard(registry) : nullptr;
    char *active_kb_display = (active_kb_name && registry)
        ? typio_registry_get_engine_display_name(registry, active_kb_name)
        : nullptr;
    char *active_kb_icon = (active_kb_name && registry)
        ? typio_registry_get_engine_icon(registry, active_kb_name)
        : nullptr;
    {
        free(ctrl->active_engine_name);
        free(ctrl->active_engine_display_name);
        ctrl->active_engine_name = active_kb_name
            ? typio_state_strdup(active_kb_name) : nullptr;
        ctrl->active_engine_display_name = (active_kb_display && *active_kb_display)
            ? typio_state_strdup(active_kb_display) : nullptr;
        ctrl->engine_active = active_kb_name != nullptr;
    }

    /* Voice engine */
    {
        char *voice_name = registry
            ? typio_registry_get_active_voice(registry) : nullptr;
        char *voice_display = (voice_name && registry)
            ? typio_registry_get_engine_display_name(registry, voice_name)
            : nullptr;
        free(ctrl->active_voice_engine_name);
        free(ctrl->active_voice_engine_display_name);
        ctrl->active_voice_engine_name = voice_name
            ? typio_state_strdup(voice_name) : nullptr;
        ctrl->active_voice_engine_display_name = (voice_display && *voice_display)
            ? typio_state_strdup(voice_display) : nullptr;
        typio_free_string(voice_name);
        typio_free_string(voice_display);
    }

    /* Status icon — route through the same language-only precedence chain
     * as `typio_state_controller_notify_engine_changed` so startup sync and
     * live updates agree. Engine identity is intentionally not consumed. */
    {
        free(ctrl->status_icon);
        ctrl->status_icon = typio_state_controller_resolve_status_icon(ctrl);
    }

    typio_free_string(active_kb_icon);
    typio_free_string(active_kb_display);
    typio_free_string(active_kb_name);

    /* Mode — we cannot query current mode directly from instance, so we
     * clear it and wait for the next mode notification from Core. */
    typio_state_controller_clear_mode(ctrl);

    typio_state_controller_refresh_language(ctrl);

    /* Broadcast every change type so listeners perform a full refresh. */
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_ENGINE);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_VOICE_ENGINE);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_LANGUAGE);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
}
