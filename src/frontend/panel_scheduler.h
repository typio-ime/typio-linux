#ifndef TYPIO_WL_PANEL_SCHEDULER_H
#define TYPIO_WL_PANEL_SCHEDULER_H

#include "panel.h"

#include <stdbool.h>

typedef enum TypioWlPanelScheduleState {
    TYPIO_WL_PANEL_SCHEDULE_IDLE = 0,
    TYPIO_WL_PANEL_SCHEDULE_DIRTY = 1,
    TYPIO_WL_PANEL_SCHEDULE_RETRY = 2,
} TypioWlPanelScheduleState;

TypioWlPanelScheduleState
typio_wl_panel_scheduler_mark_dirty(TypioWlPanelScheduleState current_state);

TypioWlPanelScheduleState
typio_wl_panel_scheduler_complete(TypioPanelUpdateResult result);

TypioWlPanelScheduleState typio_wl_panel_scheduler_cancel(void);

bool typio_wl_panel_scheduler_should_flush(TypioWlPanelScheduleState state,
                                           bool has_session,
                                           bool has_context,
                                           bool context_focused);

int typio_wl_panel_scheduler_poll_timeout_ms(TypioWlPanelScheduleState state,
                                             bool flushable,
                                             int current_timeout_ms);

#endif /* TYPIO_WL_PANEL_SCHEDULER_H */
