/**
 * @file startup_guard.h
 * @brief Epoch-based startup filtering for freshly activated keyboard grabs
 *
 * When a new keyboard grab is established, the compositor may re-send a
 * *release* for a key that was physically held before the grab existed and
 * lifted just after.  Such an orphan release has no matching press in the
 * current grab generation; the release path forwards it to the virtual
 * keyboard so the focused client's key state does not get stuck down.
 *
 * The guard uses the Wayland dispatch epoch (a counter incremented after each
 * wl_display_dispatch) to bound that orphan-release window deterministically:
 * a release arriving within the first TYPIO_WL_STARTUP_GUARD_EPOCHS dispatch
 * cycles after the grab was created may be a pre-grab orphan.
 *
 * Genuine new *presses* are never filtered here — a key whose generation does
 * not match the active grab is dropped by the grab-generation fence instead.
 */

#ifndef TYPIO_WL_STARTUP_GUARD_H
#define TYPIO_WL_STARTUP_GUARD_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Number of Wayland dispatch epochs after grab creation during which key
 * presses are treated as potential compositor re-sends of held keys.
 *
 * The grab request is a Wayland client→server message.  The compositor
 * responds with keymap, modifiers, and any held-key press events.  These
 * responses arrive in the NEXT dispatch cycle (epoch + 1).  We guard for
 * two cycles to handle compositors that batch the response across cycles.
 */
#define TYPIO_WL_STARTUP_GUARD_EPOCHS 2ULL

bool typio_wl_startup_guard_is_in_guard_window(uint64_t created_at_epoch,
                                                uint64_t current_epoch);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_STARTUP_GUARD_H */
