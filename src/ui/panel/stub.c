/**
 * @file stub.c
 * @brief No-op Panel implementation when flux (the GPU canvas) is unavailable.
 */

#include "panel.h"

#include <stddef.h>

TypioPanel *typio_panel_create(TypioWlFrontend *frontend) {
    (void)frontend;
    return NULL;
}

void typio_panel_destroy(TypioPanel *panel) {
    (void)panel;
}

bool typio_panel_is_available(TypioPanel *panel) {
    (void)panel;
    return false;
}

bool typio_panel_present_retry_pending(TypioPanel *panel) {
    (void)panel;
    return false;
}

bool typio_panel_update_content(TypioPanel *panel, const TypioPanelContent *content) {
    (void)panel;
    (void)content;
    return false;
}

bool typio_panel_update(TypioPanel *panel, TypioInputContext *ctx) {
    (void)panel;
    (void)ctx;
    return false;
}

void typio_panel_hide(TypioPanel *panel) {
    (void)panel;
}

void typio_panel_invalidate_config(TypioPanel *panel) {
    (void)panel;
}

void typio_panel_handle_output_change(TypioPanel *panel, struct wl_output *output) {
    (void)panel;
    (void)output;
}

bool typio_panel_show_status(TypioPanel *panel, const char *text) {
    (void)panel;
    (void)text;
    return false;
}

void typio_panel_hide_status(TypioPanel *panel) {
    (void)panel;
}
