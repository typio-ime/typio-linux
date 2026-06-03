#ifndef TYPIO_WL_TEXT_UI_STATE_H
#define TYPIO_WL_TEXT_UI_STATE_H

#include <stdbool.h>
#include <stdint.h>

typedef enum TypioWlTextUiPlan {
    TYPIO_WL_TEXT_UI_SYNC_PREEDIT_AND_PANEL = 0,
    TYPIO_WL_TEXT_UI_SYNC_PANEL_ONLY = 1,
} TypioWlTextUiPlan;

typedef enum TypioWlPositionedUiPlan {
    TYPIO_WL_POSITIONED_UI_WAIT = 0,
    TYPIO_WL_POSITIONED_UI_SHOW = 1,
    TYPIO_WL_POSITIONED_UI_CANCEL = 2,
} TypioWlPositionedUiPlan;

TypioWlTextUiPlan typio_wl_text_ui_plan_update(const char *last_preedit_text,
                                               int last_preedit_cursor,
                                               const char *next_preedit_text,
                                               int next_preedit_cursor);

void typio_wl_text_ui_reset_tracking(char **last_preedit_text,
                                     int *last_preedit_cursor);

TypioWlPositionedUiPlan typio_wl_positioned_ui_plan(bool pending,
                                                    bool anchor_ready,
                                                    uint64_t since_ms,
                                                    uint64_t now_ms,
                                                    uint64_t timeout_ms);

#endif /* TYPIO_WL_TEXT_UI_STATE_H */
