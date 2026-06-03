#include "state.h"

#include <stdlib.h>
#include <string.h>

TypioWlTextUiPlan typio_wl_text_ui_plan_update(const char *last_preedit_text,
                                               int last_preedit_cursor,
                                               const char *next_preedit_text,
                                               int next_preedit_cursor) {
    const char *last_text = last_preedit_text ? last_preedit_text : "";
    const char *next_text = next_preedit_text ? next_preedit_text : "";

    if (last_preedit_cursor != next_preedit_cursor ||
        strcmp(last_text, next_text) != 0) {
        return TYPIO_WL_TEXT_UI_SYNC_PREEDIT_AND_PANEL;
    }

    return TYPIO_WL_TEXT_UI_SYNC_PANEL_ONLY;
}

void typio_wl_text_ui_reset_tracking(char **last_preedit_text,
                                     int *last_preedit_cursor) {
    if (last_preedit_text) {
        free(*last_preedit_text);
        *last_preedit_text = NULL;
    }

    if (last_preedit_cursor) {
        *last_preedit_cursor = -1;
    }
}

TypioWlPositionedUiPlan typio_wl_positioned_ui_plan(bool pending,
                                                    bool anchor_ready,
                                                    uint64_t since_ms,
                                                    uint64_t now_ms,
                                                    uint64_t timeout_ms) {
    if (!pending) {
        return TYPIO_WL_POSITIONED_UI_WAIT;
    }

    if (anchor_ready) {
        return TYPIO_WL_POSITIONED_UI_SHOW;
    }

    uint64_t elapsed_ms = 0;
    if (since_ms > 0 && now_ms >= since_ms) {
        elapsed_ms = now_ms - since_ms;
    }

    if (since_ms > 0 && elapsed_ms >= timeout_ms) {
        return TYPIO_WL_POSITIONED_UI_CANCEL;
    }

    return TYPIO_WL_POSITIONED_UI_WAIT;
}
