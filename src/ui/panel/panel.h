/**
 * @file panel.h
 * @brief Public API for the candidate Panel — the floating IME UI.
 *
 * One TypioPanel owns the input-popup wl_surface, the GPU present pipeline,
 * the layout/shaper caches, and the theme. The frontend builds a
 * TypioPanelContent (candidates, preedit, status) and pushes it here.
 */

#ifndef TYPIO_WL_PANEL_H
#define TYPIO_WL_PANEL_H

#include "content.h"

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

struct wl_output;

typedef struct TypioInputContext TypioInputContext;
typedef struct TypioWlFrontend TypioWlFrontend;
typedef struct TypioPanel TypioPanel;

/* Create the panel: its wl_surface, input-popup surface, HiDPI helpers, and
 * shaper/layout caches. Returns NULL if the compositor or input-method
 * globals are unavailable (popup disabled) or on allocation failure. */
TypioPanel *typio_panel_create(TypioWlFrontend *frontend);
void typio_panel_destroy(TypioPanel *panel);

/* Whether the panel has a usable input-popup surface. */
bool typio_panel_is_available(TypioPanel *panel);

/* True when a present stalled and the loop should re-present (lock/suspend). */
bool typio_panel_present_retry_pending(TypioPanel *panel);

/* Primary update path: composite the given content and present one frame. */
bool typio_panel_update_content(TypioPanel *panel,
                                const TypioPanelContent *content);

/* Convenience wrapper for the InputContext-only case (candidates + preedit). */
bool typio_panel_update(TypioPanel *panel, TypioInputContext *ctx);

void typio_panel_hide(TypioPanel *panel);
void typio_panel_invalidate_config(TypioPanel *panel);
void typio_panel_handle_output_change(TypioPanel *panel, struct wl_output *output);

/* Transient status banner (e.g. voice "[Recording...]") shown in the panel
 * when there are no candidates. Internally builds a TypioPanelContent. */
bool typio_panel_show_status(TypioPanel *panel, const char *text);
void typio_panel_hide_status(TypioPanel *panel);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_PANEL_H */
