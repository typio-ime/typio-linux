/**
 * @file lifecycle_policy.c
 * @brief Pure, dependency-free lifecycle decision functions.
 *
 * These functions reason only about the lifecycle enums — no frontend, no
 * Wayland, no I/O. They are split out of lifecycle.c (which carries the heavy
 * imperative helpers that touch the grab, virtual keyboard, and engine) so the
 * decision logic can be unit-tested standalone, in the same spirit as
 * reconcile.c / resume_model.h. See tests/test_lifecycle.c.
 */

#include "lifecycle.h"

const char *typio_wl_lifecycle_phase_name(TypioWlLifecyclePhase phase) {
    switch (phase) {
    case TYPIO_WL_PHASE_INACTIVE:     return "INACTIVE";
    case TYPIO_WL_PHASE_ACTIVATING:   return "ACTIVATING";
    case TYPIO_WL_PHASE_ACTIVE:       return "ACTIVE";
    case TYPIO_WL_PHASE_DEACTIVATING: return "DEACTIVATING";
    }
    return "UNKNOWN";
}

bool typio_wl_lifecycle_transition_is_valid(TypioWlLifecyclePhase from,
                                            TypioWlLifecyclePhase to) {
    if (from == to)
        return true;
    switch (from) {
    case TYPIO_WL_PHASE_INACTIVE:
        return to == TYPIO_WL_PHASE_ACTIVATING;
    case TYPIO_WL_PHASE_ACTIVATING:
        return to == TYPIO_WL_PHASE_ACTIVE ||
               to == TYPIO_WL_PHASE_DEACTIVATING;
    case TYPIO_WL_PHASE_ACTIVE:
        /* ACTIVE -> ACTIVATING is a re-activation (the compositor re-asserts
         * focus on a new field without an intervening deactivate). */
        return to == TYPIO_WL_PHASE_DEACTIVATING ||
               to == TYPIO_WL_PHASE_ACTIVATING;
    case TYPIO_WL_PHASE_DEACTIVATING:
        return to == TYPIO_WL_PHASE_INACTIVE ||
               to == TYPIO_WL_PHASE_ACTIVATING;
    }
    return false;
}

bool typio_wl_lifecycle_phase_allows_key_events(TypioWlLifecyclePhase phase) {
    return phase == TYPIO_WL_PHASE_ACTIVE;
}

bool typio_wl_lifecycle_phase_allows_modifier_events(TypioWlLifecyclePhase phase) {
    return phase == TYPIO_WL_PHASE_ACTIVE ||
           phase == TYPIO_WL_PHASE_ACTIVATING;
}

TypioWlDoneAction typio_wl_lifecycle_classify_done(bool was_active,
                                                   bool now_active,
                                                   bool activate_seen) {
    if (now_active && !was_active)
        return TYPIO_WL_DONE_ACTIVATE;
    if (was_active && !now_active)
        return TYPIO_WL_DONE_DEACTIVATE;
    /* Still active. Only a fresh `activate` in this batch means a genuine
     * re-activation (a move to a new field); otherwise this `done` is just a
     * text-state update and must leave focus state untouched. */
    if (was_active && now_active && activate_seen)
        return TYPIO_WL_DONE_REACTIVATE;
    return TYPIO_WL_DONE_NONE;
}
