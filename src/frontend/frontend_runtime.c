/**
 * @file frontend_runtime.c
 * @brief IPC runtime-state serialization for the Wayland frontend.
 *
 * The IPC bus invokes a runtime-state callback periodically so that external
 * clients (typioctl) can introspect the daemon's current health. This file
 * owns the projection from the live TypioWlFrontend struct into the
 * dependency-free TypioIpcBusRuntimeState struct, and the small time-math
 * helpers that convert monotonic timestamps into IPC-friendly deltas.
 *
 * The IPC `lifecycle_phase` string is derived from the actual grab resource
 * state (via typio_wl_session_observe), not from a stored phase. This is
 * the only place the legacy `INACTIVE / ACTIVATING / ACTIVE / DEACTIVATING`
 * vocabulary leaks to the outside world, and it is computed fresh on every
 * read.
 */

#include "frontend.h"
#include "internal.h"
#include "clock.h"
#include "session_controller.h"
#include "ipc/ipc_bus.h"

#include "typio/abi/log.h"
#include "typio/abi/types.h"
#include "typio/typio.h"

#include <stdint.h>

static uint32_t frontend_runtime_age_ms(uint64_t now_ms, uint64_t since_ms) {
    if (since_ms == 0 || now_ms <= since_ms) return 0;
    uint64_t delta = now_ms - since_ms;
    return (delta > UINT32_MAX) ? UINT32_MAX : (uint32_t)delta;
}

static int32_t frontend_runtime_deadline_remaining_ms(uint64_t now_ms,
                                                      uint64_t deadline_ms) {
    if (deadline_ms == 0) return 0;
    int64_t delta = (int64_t)deadline_ms - (int64_t)now_ms;
    if (delta > INT32_MAX) return INT32_MAX;
    if (delta < INT32_MIN) return INT32_MIN;
    return (int32_t)delta;
}

/* Non-static: frontend.c takes the address of this function as the IPC
 * runtime-state callback. Both files see the same prototype via the
 * extern declaration in frontend.c. */
void frontend_fill_runtime_state(void *user_data,
                                 TypioIpcBusRuntimeState *state) {
    TypioWlFrontend *frontend = user_data;
    if (!frontend || !state) return;

    uint64_t now_ms = typio_wl_monotonic_ms();
    state->frontend_backend = "wayland";
    state->lifecycle_phase =
        typio_wl_grab_resource_state_name(
            typio_wl_session_observe(frontend).grab);
    state->virtual_keyboard_state =
        typio_wl_vk_state_name(frontend->vk ? frontend->vk->state
                                            : TYPIO_WL_VK_STATE_ABSENT);
    state->keyboard_grab_active = frontend->keyboard && frontend->keyboard->grab;
    state->virtual_keyboard_has_keymap = frontend->vk && frontend->vk->has_keymap;
    state->watchdog_armed = atomic_load(&frontend->watchdog->armed);
    state->active_key_generation = frontend->tracker->active_generation;
    state->virtual_keyboard_keymap_generation =
        frontend->vk ? frontend->vk->keymap_generation : 0;
    state->virtual_keyboard_drop_count =
        frontend->vk && frontend->vk->drop_count > UINT32_MAX
            ? UINT32_MAX
            : (uint32_t)(frontend->vk ? frontend->vk->drop_count : 0);
    state->virtual_keyboard_state_age_ms =
        frontend_runtime_age_ms(now_ms,
                                frontend->vk ? frontend->vk->state_since_ms : 0);
    state->virtual_keyboard_keymap_age_ms =
        frontend_runtime_age_ms(now_ms,
                                frontend->vk ? frontend->vk->last_keymap_ms : 0);
    state->virtual_keyboard_forward_age_ms =
        frontend_runtime_age_ms(now_ms,
                                frontend->vk ? frontend->vk->last_forward_ms : 0);
    state->virtual_keyboard_keymap_deadline_remaining_ms =
        frontend_runtime_deadline_remaining_ms(
            now_ms,
            frontend->vk ? frontend->vk->keymap_deadline_ms : 0);
}

void typio_wl_frontend_emit_runtime_state_changed(TypioWlFrontend *frontend) {
    /* Runtime state changes are pushed via the state controller listener on
     * the IPC bus (events.subscribe; topic "runtime.changed"); no explicit
     * fan-out is needed here. */
    (void)frontend;
}
