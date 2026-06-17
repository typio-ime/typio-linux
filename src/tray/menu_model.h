/**
 * @file menu_model.h
 * @brief Pure in-memory model of the tray dbusmenu, decoupled from sd_bus.
 *
 * The model captures WHAT the tray menu contains at the current instant
 * (items, labels, radio state, submenu nesting). `handle_menu_getlayout`
 * builds a model from the live registry state and then serialises it to a
 * dbusmenu `GetLayout` reply. Splitting the two concerns makes the menu
 * structure unit-testable without an sd_bus fixture.
 *
 * Tree nodes own their strings and their children. Free the root with
 * `typio_tray_menu_item_free` and the entire tree is reclaimed recursively.
 */

#ifndef TYPIO_TRAY_MENU_MODEL_H
#define TYPIO_TRAY_MENU_MODEL_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioTrayMenuItem TypioTrayMenuItem;
typedef struct TypioInstance TypioInstance;

/* ─── Construction ──────────────────────────────────────────────────────── */

/* Each constructor returns NULL on allocation failure. The returned node
 * owns its strings; `typio_tray_menu_item_free` releases them.
 *
 * `accessible_desc` is optional. NULL means "inherit the label" at
 * serialise time, which is the right default for almost every item. */

TypioTrayMenuItem *typio_tray_menu_item_new_standard(
    int32_t id, const char *label, bool enabled, const char *accessible_desc);

TypioTrayMenuItem *typio_tray_menu_item_new_separator(int32_t id);

/* Radio leaf item: `selected` controls toggle-state (0/1). The serialiser
 * emits type="radio" + toggle-state so the host panel draws a native radio
 * indicator. */
TypioTrayMenuItem *typio_tray_menu_item_new_radio(
    int32_t id, const char *label, bool enabled, bool selected,
    const char *accessible_desc);

/* Submenu parent: carries children-display=submenu. When `selected` is true
 * the parent also advertises type="radio" + toggle-state=1 so the active
 * language is marked at the top level even when its engines live in a
 * submenu. Pass `selected=false` for a submenu parent that does not
 * participate in radio grouping. */
TypioTrayMenuItem *typio_tray_menu_item_new_submenu(
    int32_t id, const char *label, bool enabled, bool selected,
    const char *accessible_desc);

/* Append @p child to @p parent's children. Takes ownership of @p child.
 * Returns false on allocation failure (the child is NOT freed; caller
 * must handle). */
bool typio_tray_menu_item_add_child(TypioTrayMenuItem *parent,
                                    TypioTrayMenuItem *child);

/* Recursive free. NULL is a no-op. */
void typio_tray_menu_item_free(TypioTrayMenuItem *item);

/* ─── Accessors (read-only; for tests and the sd_bus serialiser) ────────── */

int32_t typio_tray_menu_item_get_id(const TypioTrayMenuItem *item);
const char *typio_tray_menu_item_get_label(const TypioTrayMenuItem *item);
const char *typio_tray_menu_item_get_type(const TypioTrayMenuItem *item);
const char *typio_tray_menu_item_get_accessible_desc(const TypioTrayMenuItem *item);
bool typio_tray_menu_item_get_enabled(const TypioTrayMenuItem *item);
/* -1 = not a toggle, 0 = radio off, 1 = radio on. */
int typio_tray_menu_item_get_toggle_state(const TypioTrayMenuItem *item);
bool typio_tray_menu_item_is_submenu_parent(const TypioTrayMenuItem *item);
size_t typio_tray_menu_item_get_child_count(const TypioTrayMenuItem *item);
const TypioTrayMenuItem *typio_tray_menu_item_get_child(
    const TypioTrayMenuItem *item, size_t index);

/* ─── Builder ──────────────────────────────────────────────────────────── */

/* Build the full tray menu tree from the current registry state.
 *
 * Layout (ADR-0033):
 *   - Each registered language becomes a top-level entry. A language with
 *     at least one declared engine becomes a submenu parent; a layout-only
 *     language is a flat radio leaf.
 *   - Engines that declare none of the registered languages appear in a
 *     trailing flat "Engines" section so they remain reachable.
 *   - Voice engines form their own radio group below the keyboard section.
 *   - Restart and Quit are always present.
 *
 * @param instance  Live TypioInstance; the registry is read from here.
 * @param engine_name  Currently active keyboard engine name (may be NULL).
 *                     Used to mark the matching engine item as selected.
 * @return Root submenu item (id=0); caller frees with
 *         `typio_tray_menu_item_free`. NULL on allocation failure.
 */
TypioTrayMenuItem *typio_tray_menu_build(TypioInstance *instance,
                                         const char *engine_name);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_TRAY_MENU_MODEL_H */
