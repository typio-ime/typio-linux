/**
 * @file lifecycle_state.h
 * @brief Orthogonal lifecycle state axes for the Wayland frontend
 *
 * The declared @c TypioWlLifecyclePhase (lifecycle.h) is a single enum that
 * names the frontend's intended lifecycle boundary. It does not directly
 * encode the live resource axes: whether we are connected to the compositor,
 * whether the input method is focused, whether a keyboard grab is established,
 * and whether a composition is in flight.
 *
 * This module observes those four orthogonal axes and provides a pure
 * projection back to a lifecycle phase. The orthogonal state is
 * never stored as a second source of truth; it is *observed* from the
 * live frontend fields (see @c typio_wl_lifecycle_observe in lifecycle.c)
 * and compared, via the projection, against the phase the frontend
 * believes it is in. The reconciler uses that comparison to detect and
 * repair divergence.
 */

#ifndef TYPIO_WL_LIFECYCLE_STATE_H
#define TYPIO_WL_LIFECYCLE_STATE_H

#include "lifecycle.h"

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    TYPIO_WL_CONN_DISCONNECTED = 0,
    TYPIO_WL_CONN_CONNECTED,
} TypioWlConnState;

typedef enum {
    TYPIO_WL_FOCUS_UNFOCUSED = 0,
    TYPIO_WL_FOCUS_FOCUSED,
} TypioWlFocusState;

typedef enum {
    TYPIO_WL_GRAB_NONE = 0,
    TYPIO_WL_GRAB_PENDING_KEYMAP,
    TYPIO_WL_GRAB_READY,
} TypioWlGrabState;

typedef enum {
    TYPIO_WL_COMP_IDLE = 0,
    TYPIO_WL_COMP_COMPOSING,
} TypioWlCompState;

typedef struct TypioWlLifecycleState {
    TypioWlConnState  conn;
    TypioWlFocusState focus;
    TypioWlGrabState  grab;
    TypioWlCompState  comp;
} TypioWlLifecycleState;

const char *typio_wl_conn_state_name(TypioWlConnState state);
const char *typio_wl_focus_state_name(TypioWlFocusState state);
const char *typio_wl_grab_state_name(TypioWlGrabState state);
const char *typio_wl_comp_state_name(TypioWlCompState state);

TypioWlLifecyclePhase
typio_wl_lifecycle_project_phase(const TypioWlLifecycleState *state);
bool typio_wl_lifecycle_state_agrees(const TypioWlLifecycleState *state,
                                     TypioWlLifecyclePhase declared);

struct TypioWlFrontend;

/**
 * Observe the orthogonal lifecycle axes from the live frontend fields.
 * A read-only snapshot of reality (connection, focus, grab, composition),
 * not a stored second source of truth. Implemented in lifecycle.c because
 * it must read the frontend struct; declared here alongside the type it
 * returns. The reconciler compares its projection against the frontend's
 * declared @c lifecycle_phase.
 */
TypioWlLifecycleState
typio_wl_lifecycle_observe(const struct TypioWlFrontend *frontend);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_LIFECYCLE_STATE_H */
