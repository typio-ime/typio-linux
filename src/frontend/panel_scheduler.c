#include "panel_scheduler.h"

#define TYPIO_WL_PANEL_RETRY_POLL_MS 16

TypioWlPanelScheduleState
typio_wl_panel_scheduler_mark_dirty(TypioWlPanelScheduleState current_state) {
    (void)current_state;
    return TYPIO_WL_PANEL_SCHEDULE_DIRTY;
}

TypioWlPanelScheduleState
typio_wl_panel_scheduler_complete(TypioPanelUpdateResult result) {
    return result == TYPIO_PANEL_UPDATE_RETRY ? TYPIO_WL_PANEL_SCHEDULE_RETRY
                                              : TYPIO_WL_PANEL_SCHEDULE_IDLE;
}

TypioWlPanelScheduleState typio_wl_panel_scheduler_cancel(void) {
    return TYPIO_WL_PANEL_SCHEDULE_IDLE;
}

bool typio_wl_panel_scheduler_should_flush(TypioWlPanelScheduleState state,
                                           bool has_session,
                                           bool has_context,
                                           bool context_focused) {
    return state != TYPIO_WL_PANEL_SCHEDULE_IDLE &&
           has_session &&
           has_context &&
           context_focused;
}

int typio_wl_panel_scheduler_poll_timeout_ms(TypioWlPanelScheduleState state,
                                             bool flushable,
                                             int current_timeout_ms) {
    if (state != TYPIO_WL_PANEL_SCHEDULE_RETRY ||
        !flushable ||
        current_timeout_ms <= TYPIO_WL_PANEL_RETRY_POLL_MS) {
        return current_timeout_ms;
    }

    return TYPIO_WL_PANEL_RETRY_POLL_MS;
}
