/**
 * @file controller.h
 * @brief Centralized state controller — single source of truth for runtime surfaces
 *
 * The StateController sits between the Core layer and external runtime surfaces
 * (system tray, D-Bus status bus, etc.). It:
 *
 *   1. Maintains a snapshot of user-visible state (active engine, mode, icon).
 *   2. Provides query APIs so surfaces read state from ONE place instead of
 *      reaching directly into TypioInstance.
 *   3. Broadcasts change notifications to registered listeners so every surface
 *      updates uniformly.
 *
 * All external surfaces (tray, status bus, and any future UI) should base their
 * behaviour on this layer.
 */

#ifndef TYPIO_STATE_CONTROLLER_H
#define TYPIO_STATE_CONTROLLER_H

#include "typio/abi/types.h"
#include "typio/abi/config.h"

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioStateController TypioStateController;

typedef enum {
    TYPIO_STATE_CHANGE_ENGINE,
    TYPIO_STATE_CHANGE_VOICE_ENGINE,
    TYPIO_STATE_CHANGE_LANGUAGE,
    TYPIO_STATE_CHANGE_STATUS,
    TYPIO_STATE_CHANGE_STATUS_ICON,
} TypioStateChangeType;

typedef void (*TypioStateChangeCallback)(void *user_data,
                                         TypioStateChangeType change_type);

typedef struct TypioStateListener {
    void *user_data;
    TypioStateChangeCallback callback;
} TypioStateListener;

/* -------------------------------------------------------------------------- */
/* Lifecycle                                                                  */
/* -------------------------------------------------------------------------- */

TypioStateController *typio_state_controller_new(TypioInstance *instance);
void typio_state_controller_free(TypioStateController *ctrl);

/* -------------------------------------------------------------------------- */
/* Listener registration                                                      */
/* -------------------------------------------------------------------------- */

void typio_state_controller_add_listener(TypioStateController *ctrl,
                                         TypioStateListener listener);
void typio_state_controller_remove_listener(TypioStateController *ctrl,
                                            void *user_data);

/* -------------------------------------------------------------------------- */
/* State queries — single source of truth for external surfaces               */
/* -------------------------------------------------------------------------- */

const char *typio_state_controller_get_active_engine_name(
    TypioStateController *ctrl);
const char *typio_state_controller_get_active_engine_display_name(
    TypioStateController *ctrl);
const char *typio_state_controller_get_active_voice_engine_name(
    TypioStateController *ctrl);
const char *typio_state_controller_get_active_voice_engine_display_name(
    TypioStateController *ctrl);
const char *typio_state_controller_get_active_language(
    TypioStateController *ctrl);
const char *typio_state_controller_get_status_icon(
    TypioStateController *ctrl);
/* True when the status icon resolved to the language floor and should be drawn
 * as a text badge (ADR-0032) rather than looked up as a freedesktop name.
 * get_status_icon() then holds a generic name for render-failure fallback. */
bool typio_state_controller_get_status_icon_is_badge(
    TypioStateController *ctrl);
/* The badge text (language script glyphs) when is_badge is true, else NULL. */
const char *typio_state_controller_get_status_badge_text(
    TypioStateController *ctrl);
bool typio_state_controller_get_engine_active(
    TypioStateController *ctrl);
const TypioKeyboardEngineMode *typio_state_controller_get_current_status(
    TypioStateController *ctrl);

/**
 * @brief Map a BCP-47 language tag to its endonym for display.
 *
 * The registry exposes only the raw tag (ADR-0031); libtypio has no
 * language-display API, so the host owns this presentation table. Returns the
 * tag itself for anything unlisted (so new languages still render), or NULL for
 * a NULL/empty tag. The returned string is static — do not free.
 */
const char *typio_language_endonym(const char *tag);

/**
 * @brief Compact one-to-three glyph badge for a BCP-47 tag (e.g. 中 / あ / الد
 *        / EN), written into @p out.
 *
 * The language is the reliable visual identity (ADR-0031): it is always present
 * — even for layout-only languages with no engine — and stable across engine /
 * mode churn. This badge is the icon-sized form used by the on-screen indicator
 * (and, in future, the tray pixmap). Unlisted tags fall back to the uppercased
 * primary subtag (e.g. `ary-x` → `ARY`). @p out is set to an empty string for a
 * NULL/empty tag.
 */
void typio_language_badge(const char *tag, char *out, size_t out_size);

/**
 * @brief Disambiguated language label for list/menu surfaces.
 *
 * Returns the endonym with a script qualifier appended when the tag carries
 * an ISO 15924 script subtag: @c zh-Hans → "中文 (简)", @c zh-Hant →
 * "中文 (繁)", @c sr-Latn → "Srpski (Latin)". Tags with only a primary
 * subtag or a region subtag collapse to the bare endonym. Used by the tray
 * menu where multiple script variants of one primary language would
 * otherwise render indistinguishably. @p out is always NUL-terminated.
 */
void typio_language_menu_label(const char *tag, char *out, size_t out_size);

/**
 * @brief Resolve the tray/indicator status icon by the language-only chain
 *        (ADR-0033).
 *
 * Pure over its inputs — does not query @c TypioInstance or @c TypioRegistry.
 * This makes it unit-testable without fixtures. The chain is, most-specific
 * first:
 *
 *   1. @c [languages.<tag>].icon config override
 *   2. language badge (rendered text)
 *   3. generic @c typio-keyboard-symbolic (anything active, no icon found)
 *   4. @c typio-keyboard-off-symbolic (nothing active)
 *
 * @param active_language_tag Active BCP-47 tag, or NULL when no language is
 *        active. May be a script/region-qualified tag (@c zh-Hans,
 *        @c pt-BR); the badge lookup matches on the primary subtag.
 * @param engine_active True when a keyboard engine is active even if no
 *        language is set (legacy/engine-cycling installs).
 * @param cfg Optional config, used to look up per-language icon overrides.
 *        NULL is accepted (layer 1 is skipped).
 * @param out_is_badge Set to true when the returned icon name is a fallback
 *        and the caller should instead render @p out_badge_text as a pixmap.
 *        Set to false otherwise. Must not be NULL.
 * @param out_badge_text When @p out_is_badge is set to true, set to a freshly
 *        allocated string the caller frees with @c free(). Set to NULL
 *        otherwise. Must not be NULL.
 * @return Freshly allocated icon name (never NULL); caller frees with
 *         @c free().
 */
char *typio_resolve_language_icon(const char *active_language_tag,
                                 bool engine_active,
                                 TypioConfig *cfg,
                                 bool *out_is_badge,
                                 char **out_badge_text);

/* -------------------------------------------------------------------------- */
/* Notifications from Core — called by the daemon's Rust→C callbacks          */
/* -------------------------------------------------------------------------- */

void typio_state_controller_notify_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info);
void typio_state_controller_notify_voice_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info);
void typio_state_controller_notify_status_changed(
    TypioStateController *ctrl,
    const TypioKeyboardEngineMode *mode);
void typio_state_controller_notify_status_icon_changed(
    TypioStateController *ctrl,
    const char *icon_name);

/**
 * @brief Re-read all state from Core and broadcast changes.
 *
 * Call once after all listeners have registered (e.g. at startup) so every
 * surface receives an initial sync.
 */
void typio_state_controller_sync(TypioStateController *ctrl);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_STATE_CONTROLLER_H */
