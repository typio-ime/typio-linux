#ifndef TYPIO_WL_TEXT_UI_STATE_H
#define TYPIO_WL_TEXT_UI_STATE_H

typedef enum TypioWlTextUiPlan {
    TYPIO_WL_TEXT_UI_SYNC_PREEDIT_AND_PANEL = 0,
    TYPIO_WL_TEXT_UI_SYNC_PANEL_ONLY = 1,
} TypioWlTextUiPlan;

TypioWlTextUiPlan typio_wl_text_ui_plan_update(const char *last_preedit_text,
                                               int last_preedit_cursor,
                                               const char *next_preedit_text,
                                               int next_preedit_cursor);

void typio_wl_text_ui_reset_tracking(bool *panel_update_pending,
                                     char **last_preedit_text,
                                     int *last_preedit_cursor);

bool typio_wl_text_ui_should_flush_panel_update(bool panel_update_pending,
                                                bool has_session,
                                                bool has_context,
                                                bool context_focused);

#endif /* TYPIO_WL_TEXT_UI_STATE_H */
