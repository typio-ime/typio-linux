/**
 * @file menu_model.c
 * @brief Tray dbusmenu model construction, decoupled from sd_bus.
 *
 * The tree-building logic that used to live inline in `handle_menu_getlayout`
 * is extracted here so it can be unit-tested without an sd_bus fixture.
 * `sni.c` now calls `typio_tray_menu_build` and walks the result with a
 * small recursive serialiser.
 *
 * Ownership: each node owns its label/type/accessible_desc strings (strdup'd)
 * and its children array. `typio_tray_menu_item_free` recurses.
 */

#include "menu_model.h"

#include "state/controller.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/abi/engine.h"
#include "typio/abi/string.h"

#include <stdlib.h>
#include <string.h>

/* ─── ID layout (kept in sync with sni.c — see ADR-0033 follow-ups) ─────── */

#define TYPIO_TRAY_SECTION_MISC    1000
#define TYPIO_TRAY_SECTION_LANG    2000
#define TYPIO_TRAY_SECTION_ENGINE  3000
#define TYPIO_TRAY_SECTION_VOICE   4000

#define TYPIO_TRAY_LANG_BASE       TYPIO_TRAY_SECTION_LANG
#define TYPIO_TRAY_LANG_MAX        16
#define TYPIO_TRAY_ENGINE_BASE     TYPIO_TRAY_SECTION_ENGINE
#define TYPIO_TRAY_ENGINE_MAX      10
#define TYPIO_TRAY_VOICE_BASE      TYPIO_TRAY_SECTION_VOICE
#define TYPIO_TRAY_VOICE_MAX       16

#define TYPIO_TRAY_ITEM_RESTART    (TYPIO_TRAY_SECTION_MISC + 1)
#define TYPIO_TRAY_ITEM_QUIT       (TYPIO_TRAY_SECTION_MISC + 2)
#define TYPIO_TRAY_ITEM_SEP_BEGIN  (TYPIO_TRAY_SECTION_MISC + 100)

/* ─── Node struct ──────────────────────────────────────────────────────── */

struct TypioTrayMenuItem {
    int32_t id;
    char *label;
    char *type;               /* NULL = standard, "separator" = separator. */
    char *accessible_desc;
    bool enabled;
    int toggle_state;         /* -1 = none, 0 = off, 1 = on */
    bool is_submenu_parent;

    TypioTrayMenuItem **children;
    size_t child_count;
    size_t child_capacity;
};

static char *dup_or_null(const char *s) {
    if (!s || !s[0]) {
        return NULL;
    }
    return strdup(s);
}

static TypioTrayMenuItem *item_alloc(int32_t id, const char *label,
                                     const char *type, bool enabled,
                                     int toggle_state, bool is_submenu,
                                     const char *accessible_desc) {
    TypioTrayMenuItem *item = calloc(1, sizeof(*item));
    if (!item) {
        return NULL;
    }
    item->id = id;
    item->label = dup_or_null(label);
    item->type = dup_or_null(type);
    item->accessible_desc = dup_or_null(accessible_desc);
    item->enabled = enabled;
    item->toggle_state = toggle_state;
    item->is_submenu_parent = is_submenu;
    return item;
}

TypioTrayMenuItem *typio_tray_menu_item_new_standard(
    int32_t id, const char *label, bool enabled, const char *accessible_desc) {
    return item_alloc(id, label, NULL, enabled, -1, false, accessible_desc);
}

TypioTrayMenuItem *typio_tray_menu_item_new_separator(int32_t id) {
    return item_alloc(id, NULL, "separator", true, -1, false, NULL);
}

TypioTrayMenuItem *typio_tray_menu_item_new_radio(
    int32_t id, const char *label, bool enabled, bool selected,
    const char *accessible_desc) {
    return item_alloc(id, label, NULL, enabled, selected ? 1 : 0, false,
                      accessible_desc);
}

TypioTrayMenuItem *typio_tray_menu_item_new_submenu(
    int32_t id, const char *label, bool enabled, bool selected,
    const char *accessible_desc) {
    return item_alloc(id, label, NULL, enabled, selected ? 1 : 0, true,
                      accessible_desc);
}

bool typio_tray_menu_item_add_child(TypioTrayMenuItem *parent,
                                    TypioTrayMenuItem *child) {
    if (!parent || !child) {
        return false;
    }
    if (parent->child_count == parent->child_capacity) {
        size_t new_cap = parent->child_capacity ? parent->child_capacity * 2 : 4;
        TypioTrayMenuItem **grown = realloc(parent->children,
                                            new_cap * sizeof(*grown));
        if (!grown) {
            return false;
        }
        parent->children = grown;
        parent->child_capacity = new_cap;
    }
    parent->children[parent->child_count++] = child;
    return true;
}

void typio_tray_menu_item_free(TypioTrayMenuItem *item) {
    if (!item) {
        return;
    }
    for (size_t i = 0; i < item->child_count; i++) {
        typio_tray_menu_item_free(item->children[i]);
    }
    free(item->children);
    free(item->label);
    free(item->type);
    free(item->accessible_desc);
    free(item);
}

/* ─── Accessors ────────────────────────────────────────────────────────── */

int32_t typio_tray_menu_item_get_id(const TypioTrayMenuItem *item) {
    return item ? item->id : 0;
}

const char *typio_tray_menu_item_get_label(const TypioTrayMenuItem *item) {
    return item ? item->label : NULL;
}

const char *typio_tray_menu_item_get_type(const TypioTrayMenuItem *item) {
    /* Radio items have no explicit type string stored (NULL) but the
     * serialiser emits type="radio" based on toggle_state. Expose that
     * consistently to readers. */
    if (!item) {
        return NULL;
    }
    if (item->type) {
        return item->type;
    }
    if (item->toggle_state >= 0) {
        return "radio";
    }
    return NULL;
}

const char *typio_tray_menu_item_get_accessible_desc(
    const TypioTrayMenuItem *item) {
    if (!item) {
        return NULL;
    }
    return item->accessible_desc ? item->accessible_desc : item->label;
}

bool typio_tray_menu_item_get_enabled(const TypioTrayMenuItem *item) {
    return item ? item->enabled : false;
}

int typio_tray_menu_item_get_toggle_state(const TypioTrayMenuItem *item) {
    return item ? item->toggle_state : -1;
}

bool typio_tray_menu_item_is_submenu_parent(const TypioTrayMenuItem *item) {
    return item ? item->is_submenu_parent : false;
}

size_t typio_tray_menu_item_get_child_count(const TypioTrayMenuItem *item) {
    return item ? item->child_count : 0;
}

const TypioTrayMenuItem *typio_tray_menu_item_get_child(
    const TypioTrayMenuItem *item, size_t index) {
    if (!item || index >= item->child_count) {
        return NULL;
    }
    return item->children[index];
}

/* ─── Builder ─────────────────────────────────────────────────────────── */

/* True when the named keyboard engine's declared languages contain @p tag.
 * Mirrors sni.c's helper; kept local so the model has no dependency on
 * tray_internal.h. */
static bool engine_declares_language(TypioRegistry *registry,
                                     const char *engine_name,
                                     const char *lang_tag) {
    if (!registry || !engine_name || !lang_tag) {
        return false;
    }
    size_t count = 0;
    char **langs = typio_registry_get_engine_languages(registry, engine_name,
                                                       &count);
    bool found = false;
    for (size_t i = 0; i < count; i++) {
        if (langs[i] && strcmp(langs[i], lang_tag) == 0) {
            found = true;
            break;
        }
    }
    typio_free_string_array(langs, count);
    return found;
}

static TypioTrayMenuItem *build_root(void) {
    /* Root is always id=0 per the dbusmenu convention. */
    return typio_tray_menu_item_new_submenu(0, NULL, true, false, NULL);
}

static int append_language_section(TypioTrayMenuItem *root,
                                   TypioRegistry *registry,
                                   const char *engine_name,
                                   int *next_sep_id) {
    size_t lang_count = 0;
    char **langs = typio_registry_list_languages(registry, &lang_count);
    char *active_lang = typio_registry_get_active_language(registry);
    size_t engine_count = 0;
    char **engines = typio_registry_list_ordered_keyboards(registry,
                                                           &engine_count);
    size_t const engine_cap = engine_count < TYPIO_TRAY_ENGINE_MAX
                              ? engine_count : TYPIO_TRAY_ENGINE_MAX;
    bool *engine_placed = engine_cap > 0
        ? calloc(engine_cap, sizeof(bool)) : NULL;
    int rc = 0;
    size_t lang_shown = 0;

    for (size_t i = 0; i < lang_count && i < TYPIO_TRAY_LANG_MAX; i++) {
        char lang_label[96];
        typio_language_menu_label(langs[i], lang_label, sizeof(lang_label));
        bool is_current = active_lang && strcmp(langs[i], active_lang) == 0;

        size_t child_match = 0;
        for (size_t j = 0; j < engine_cap; j++) {
            if (!engine_placed[j] &&
                engine_declares_language(registry, engines[j], langs[i])) {
                child_match++;
            }
        }

        TypioTrayMenuItem *lang_item;
        if (child_match == 0) {
            lang_item = typio_tray_menu_item_new_radio(
                TYPIO_TRAY_LANG_BASE + (int32_t)i, lang_label, true,
                is_current, NULL);
        } else {
            lang_item = typio_tray_menu_item_new_submenu(
                TYPIO_TRAY_LANG_BASE + (int32_t)i, lang_label, true,
                is_current, NULL);
            for (size_t j = 0; j < engine_cap; j++) {
                if (engine_placed[j] ||
                    !engine_declares_language(registry, engines[j], langs[i])) {
                    continue;
                }
                engine_placed[j] = true;
                const TypioEngineInfo *info =
                    typio_registry_get_engine_info(registry, engines[j]);
                const char *display =
                    (info && info->display_name && info->display_name[0])
                        ? info->display_name : engines[j];
                bool eng_current = engine_name &&
                                   strcmp(engines[j], engine_name) == 0;
                TypioTrayMenuItem *eng_item = typio_tray_menu_item_new_radio(
                    TYPIO_TRAY_ENGINE_BASE + (int32_t)j, display, true,
                    eng_current, NULL);
                typio_engine_info_free((TypioEngineInfo *)info);
                if (!eng_item || !typio_tray_menu_item_add_child(lang_item,
                                                                 eng_item)) {
                    if (eng_item) {
                        typio_tray_menu_item_free(eng_item);
                    }
                    rc = -1;
                    goto out;
                }
            }
        }
        if (!lang_item || !typio_tray_menu_item_add_child(root, lang_item)) {
            if (lang_item) {
                typio_tray_menu_item_free(lang_item);
            }
            rc = -1;
            goto out;
        }
        lang_shown++;
    }

    /* Orphan engines (no matching language). */
    bool orphan_shown = false;
    for (size_t j = 0; j < engine_cap; j++) {
        if (engine_placed[j]) {
            continue;
        }
        if (!orphan_shown) {
            if (lang_shown > 0) {
                TypioTrayMenuItem *sep = typio_tray_menu_item_new_separator(
                    (*next_sep_id)++);
                if (!sep || !typio_tray_menu_item_add_child(root, sep)) {
                    if (sep) typio_tray_menu_item_free(sep);
                    rc = -1;
                    goto out;
                }
            }
            TypioTrayMenuItem *header = typio_tray_menu_item_new_standard(
                (*next_sep_id)++, "Engines", false, NULL);
            if (!header || !typio_tray_menu_item_add_child(root, header)) {
                if (header) typio_tray_menu_item_free(header);
                rc = -1;
                goto out;
            }
            orphan_shown = true;
        }
        const TypioEngineInfo *info =
            typio_registry_get_engine_info(registry, engines[j]);
        const char *display =
            (info && info->display_name && info->display_name[0])
                ? info->display_name : engines[j];
        bool is_current = engine_name &&
                          strcmp(engines[j], engine_name) == 0;
        TypioTrayMenuItem *eng_item = typio_tray_menu_item_new_radio(
            TYPIO_TRAY_ENGINE_BASE + (int32_t)j, display, true, is_current,
            NULL);
        typio_engine_info_free((TypioEngineInfo *)info);
        if (!eng_item || !typio_tray_menu_item_add_child(root, eng_item)) {
            if (eng_item) typio_tray_menu_item_free(eng_item);
            rc = -1;
            goto out;
        }
    }

    if (lang_shown > 0 || orphan_shown) {
        TypioTrayMenuItem *sep = typio_tray_menu_item_new_separator(
            (*next_sep_id)++);
        if (!sep || !typio_tray_menu_item_add_child(root, sep)) {
            if (sep) typio_tray_menu_item_free(sep);
            rc = -1;
            goto out;
        }
    }

out:
    free(engine_placed);
    typio_free_string(active_lang);
    typio_free_string_array(langs, lang_count);
    typio_free_string_array(engines, engine_count);
    return rc;
}

static int append_voice_section(TypioTrayMenuItem *root,
                                TypioRegistry *registry,
                                int *next_sep_id) {
    size_t voice_count = 0;
    char **voices = typio_registry_list_voices(registry, &voice_count);
    char *active_voice = typio_registry_get_active_voice(registry);
    int rc = 0;
    size_t voice_shown = 0;

    for (size_t i = 0; i < voice_count && i < TYPIO_TRAY_VOICE_MAX; i++) {
        const TypioEngineInfo *info =
            typio_registry_get_engine_info(registry, voices[i]);
        const char *display =
            (info && info->display_name && info->display_name[0])
                ? info->display_name : voices[i];
        bool is_current = active_voice && strcmp(voices[i], active_voice) == 0;
        TypioTrayMenuItem *v = typio_tray_menu_item_new_radio(
            TYPIO_TRAY_VOICE_BASE + (int32_t)i, display, true, is_current,
            NULL);
        typio_engine_info_free((TypioEngineInfo *)info);
        if (!v || !typio_tray_menu_item_add_child(root, v)) {
            if (v) typio_tray_menu_item_free(v);
            rc = -1;
            goto out;
        }
        voice_shown++;
    }

    if (voice_shown > 0) {
        TypioTrayMenuItem *sep = typio_tray_menu_item_new_separator(
            (*next_sep_id)++);
        if (!sep || !typio_tray_menu_item_add_child(root, sep)) {
            if (sep) typio_tray_menu_item_free(sep);
            rc = -1;
            goto out;
        }
    }

out:
    typio_free_string(active_voice);
    typio_free_string_array(voices, voice_count);
    return rc;
}

static int append_misc_section(TypioTrayMenuItem *root) {
    TypioTrayMenuItem *restart = typio_tray_menu_item_new_standard(
        TYPIO_TRAY_ITEM_RESTART, "Restart", true, NULL);
    if (!restart || !typio_tray_menu_item_add_child(root, restart)) {
        if (restart) typio_tray_menu_item_free(restart);
        return -1;
    }
    TypioTrayMenuItem *quit = typio_tray_menu_item_new_standard(
        TYPIO_TRAY_ITEM_QUIT, "Quit", true, NULL);
    if (!quit || !typio_tray_menu_item_add_child(root, quit)) {
        if (quit) typio_tray_menu_item_free(quit);
        return -1;
    }
    return 0;
}

TypioTrayMenuItem *typio_tray_menu_build(TypioInstance *instance,
                                         const char *engine_name) {
    if (!instance) {
        return NULL;
    }
    TypioTrayMenuItem *root = build_root();
    if (!root) {
        return NULL;
    }

    TypioRegistry *registry = typio_instance_get_registry(instance);
    int next_sep_id = TYPIO_TRAY_ITEM_SEP_BEGIN;

    if (registry) {
        if (append_language_section(root, registry, engine_name,
                                     &next_sep_id) != 0) {
            typio_tray_menu_item_free(root);
            return NULL;
        }
        if (append_voice_section(root, registry, &next_sep_id) != 0) {
            typio_tray_menu_item_free(root);
            return NULL;
        }
    }

    if (append_misc_section(root) != 0) {
        typio_tray_menu_item_free(root);
        return NULL;
    }
    return root;
}
