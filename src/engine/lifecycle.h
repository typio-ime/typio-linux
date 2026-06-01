/**
 * @file lifecycle.h
 * @brief Lifecycle and timing helpers for Wayland input-method sessions
 */

#ifndef TYPIO_WL_LIFECYCLE_H
#define TYPIO_WL_LIFECYCLE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * @brief Coarse lifecycle phase of the Wayland input-method session.
 *
 * Single enum that conflates connection, focus, grab, and composition
 * concerns for the happy path. Reality is observed via the orthogonal
 * axes in lifecycle_state.h; the reconciler compares the projection
 * against this declared phase.
 */
typedef enum {
    TYPIO_WL_PHASE_INACTIVE = 0,
    TYPIO_WL_PHASE_ACTIVATING,
    TYPIO_WL_PHASE_ACTIVE,
    TYPIO_WL_PHASE_DEACTIVATING,
} TypioWlLifecyclePhase;

const char *typio_wl_lifecycle_phase_name(TypioWlLifecyclePhase phase);
bool typio_wl_lifecycle_transition_is_valid(TypioWlLifecyclePhase from,
                                            TypioWlLifecyclePhase to);
bool typio_wl_lifecycle_phase_allows_key_events(TypioWlLifecyclePhase phase);
bool typio_wl_lifecycle_phase_allows_modifier_events(TypioWlLifecyclePhase phase);
/**
 * @brief How a compositor `done` should change IME state, derived purely from
 *        the activation edges plus whether an `activate` arrived in the same
 *        batch.
 *
 * @p activate_seen distinguishes a genuine (re)activation from a plain
 * text-state update `done` (surrounding text, content type), which the
 * compositor also delivers while state is unchanged — those must not rebuild
 * activation state mid-composition.
 */
typedef enum {
    TYPIO_WL_DONE_NONE = 0,       /* No state change (e.g. a text-state update). */
    TYPIO_WL_DONE_ACTIVATE,       /* IME activated: build grab, engine focus_in. */
    TYPIO_WL_DONE_DEACTIVATE,     /* IME deactivated: drop grab, clear preedit. */
    TYPIO_WL_DONE_REACTIVATE,     /* Re-activated while active (new field): re-anchor. */
} TypioWlDoneAction;

TypioWlDoneAction typio_wl_lifecycle_classify_done(bool was_active,
                                                   bool now_active,
                                                   bool activate_seen);

struct TypioWlFrontend;
struct TypioWlKeyboard;

void typio_wl_lifecycle_set_phase(struct TypioWlFrontend *frontend,
                                  TypioWlLifecyclePhase phase,
                                  const char *reason);
void typio_wl_lifecycle_hard_reset_keyboard(struct TypioWlFrontend *frontend,
                                            const char *reason);

/**
 * Drop every piece of in-flight input-method state that could plausibly
 * be stale across a system suspend: the keyboard grab, per-key tracking
 * and generations, any active repeat, the carried virtual-keyboard
 * modifiers, and the compositor-visible preedit. The lifecycle phase is
 * forced back to INACTIVE so the next @c activate from the compositor
 * goes through the full activation sequence and rebuilds a fresh grab.
 *
 * Called from @c resume_signal — both the logind @c PrepareForSleep
 * subscriber and the boottime-gap detector funnel here. The handler is
 * idempotent: firing twice for the same wake-up is safe.
 *
 * @param reason   stable string identifying the source ("logind",
 *                 "boottime_gap"); appears in trace output.
 * @param sleep_ms wall-clock ms the system slept for, or 0 if the
 *                 detector couldn't compute one (logind doesn't tell us).
 */
void typio_wl_lifecycle_on_resume(struct TypioWlFrontend *frontend,
                                  const char *reason,
                                  uint64_t sleep_ms);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_LIFECYCLE_H */
