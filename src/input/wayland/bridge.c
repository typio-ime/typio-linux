/**
 * @file bridge.c
 * @brief Virtual keyboard forwarding helpers
 */


#include "bridge.h"
#include "debug.h"
#include "clock.h"
#include "recent_log.h"
#include "internal.h"
#include "trace.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <string.h>
#include <unistd.h>
#include <xkbcommon/xkbcommon.h>

#define TYPIO_WL_VK_KEYMAP_TIMEOUT_MS 1500
#define TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_WINDOW_MS 30000
#define TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_THRESHOLD 3
#define TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_INTERVAL 5

static uint64_t typio_wl_vk_age_ms(uint64_t now_ms, uint64_t then_ms) {
    if (then_ms == 0 || now_ms < then_ms) {
        return 0;
    }

    return now_ms - then_ms;
}

static bool typio_wl_vk_has_current_generation_keymap(TypioWlFrontend *frontend) {
    return frontend && frontend->tracker->active_generation != 0 &&
           frontend->vk->keymap_generation ==
               frontend->tracker->active_generation;
}

static bool typio_wl_vk_reason_is_keymap_cancel(const char *reason) {
    return reason &&
           (strcmp(reason, "keyboard grab cleared before keymap") == 0 ||
            strcmp(reason, "keymap wait cancelled") == 0);
}

static TypioLogLevel typio_wl_vk_state_log_level(TypioWlVirtualKeyboardState state,
                                                 const char *reason) {
    if (state == TYPIO_WL_VK_STATE_BROKEN) {
        return TYPIO_LOG_ERROR;
    }
    if (state == TYPIO_WL_VK_STATE_ABSENT) {
        return TYPIO_LOG_WARNING;
    }
    if (state == TYPIO_WL_VK_STATE_READY &&
        typio_wl_vk_reason_is_keymap_cancel(reason)) {
        return TYPIO_LOG_DEBUG;
    }
    return TYPIO_LOG_INFO;
}

static void typio_wl_vk_record_keymap_cancel(TypioWlFrontend *frontend,
                                             const char *reason) {
    uint64_t now_ms;
    uint64_t count;
    uint64_t window_ms;
    uint64_t last_keymap_age_ms;
    uint64_t last_forward_age_ms;
    bool has_current_keymap;
    bool keyboard_grab_active;

    if (!frontend || !typio_wl_vk_reason_is_keymap_cancel(reason)) {
        return;
    }

    now_ms = typio_wl_monotonic_ms();
    if (frontend->vk->keymap_cancel_window_start_ms == 0 ||
        now_ms - frontend->vk->keymap_cancel_window_start_ms >
            TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_WINDOW_MS) {
        frontend->vk->keymap_cancel_window_start_ms = now_ms;
        frontend->vk->keymap_cancel_count = 0;
    }

    frontend->vk->keymap_cancel_count++;
    count = frontend->vk->keymap_cancel_count;
    window_ms = now_ms - frontend->vk->keymap_cancel_window_start_ms;
    last_keymap_age_ms = typio_wl_vk_age_ms(now_ms,
                                            frontend->vk->last_keymap_ms);
    last_forward_age_ms = typio_wl_vk_age_ms(now_ms,
                                             frontend->vk->last_forward_ms);
    has_current_keymap = typio_wl_vk_has_current_generation_keymap(frontend);
    keyboard_grab_active = frontend->keyboard && frontend->keyboard->grab;

    typio_log_debug("Virtual keyboard keymap wait cancelled: reason=%s count=%" PRIu64
              " window_ms=%" PRIu64 " active_generation=%u keymap_generation=%u"
              " current_generation_keymap=%s keyboard_grab=%s"
              " last_keymap_age_ms=%" PRIu64 " last_forward_age_ms=%" PRIu64,
              reason, count, window_ms,
              frontend->tracker->active_generation,
              frontend->vk->keymap_generation,
              has_current_keymap ? "yes" : "no",
              keyboard_grab_active ? "yes" : "no",
              last_keymap_age_ms, last_forward_age_ms);

    if (count < TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_THRESHOLD) {
        return;
    }
    if (count != TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_THRESHOLD &&
        ((count - TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_THRESHOLD) %
         TYPIO_WL_VK_KEYMAP_CANCEL_WARNING_INTERVAL) != 0) {
        return;
    }

    typio_log_warning("Repeated virtual keyboard keymap cancellations before readiness: "
              "count=%" PRIu64 " window_ms=%" PRIu64 " state=%s"
              " active_generation=%u keymap_generation=%u"
              " current_generation_keymap=%s keyboard_grab=%s"
              " last_keymap_age_ms=%" PRIu64 " last_forward_age_ms=%" PRIu64,
              count, window_ms,
              typio_wl_vk_state_name(frontend->vk->state),
              frontend->tracker->active_generation,
              frontend->vk->keymap_generation,
              has_current_keymap ? "yes" : "no",
              keyboard_grab_active ? "yes" : "no",
              last_keymap_age_ms, last_forward_age_ms);
}

static void typio_wl_vk_mark_forward_progress(TypioWlFrontend *frontend) {
    if (!frontend) {
        return;
    }

    frontend->vk->last_forward_ms = typio_wl_monotonic_ms();
    frontend->vk->active_generation_dirty = true;
}


const char *typio_wl_vk_state_name(TypioWlVirtualKeyboardState state) {
    switch (state) {
    case TYPIO_WL_VK_STATE_ABSENT:
        return "absent";
    case TYPIO_WL_VK_STATE_NEEDS_KEYMAP:
        return "needs_keymap";
    case TYPIO_WL_VK_STATE_READY:
        return "ready";
    case TYPIO_WL_VK_STATE_BROKEN:
        return "broken";
    default:
        return "unknown";
    }
}

void typio_wl_vk_set_state(TypioWlFrontend *frontend,
                           TypioWlVirtualKeyboardState state,
                           const char *reason) {
    TypioWlVirtualKeyboardState previous;
    TypioLogLevel level;
    uint64_t now_ms;

    if (!frontend)
        return;

    now_ms = typio_wl_monotonic_ms();
    previous = frontend->vk->state;
    frontend->vk->state = state;
    frontend->vk->state_since_ms = now_ms;
    frontend->vk->has_keymap =
        state == TYPIO_WL_VK_STATE_READY &&
        typio_wl_vk_has_current_generation_keymap(frontend);
    if (state == TYPIO_WL_VK_STATE_READY || state == TYPIO_WL_VK_STATE_BROKEN ||
        state == TYPIO_WL_VK_STATE_ABSENT) {
        frontend->vk->keymap_deadline_ms = 0;
    }

    if (previous == state)
        return;

    if (state == TYPIO_WL_VK_STATE_READY) {
        frontend->vk->drop_count = 0;
    }

    typio_wl_trace(frontend,
                   "vk_state",
                   "from=%s to=%s reason=%s dropped=%" PRIu64,
                   typio_wl_vk_state_name(previous),
                   typio_wl_vk_state_name(state),
                   reason ? reason : "no reason",
                   frontend->vk->drop_count);

    level = typio_wl_vk_state_log_level(state, reason);

    typio_logf(level,
              "Virtual keyboard state changed: %s -> %s (%s, dropped=%" PRIu64 ")",
              typio_wl_vk_state_name(previous),
              typio_wl_vk_state_name(state),
              reason ? reason : "no reason",
              frontend->vk->drop_count);
    typio_wl_frontend_emit_runtime_state_changed(frontend);
}

void typio_wl_vk_expect_keymap(TypioWlFrontend *frontend,
                               const char *reason) {
    uint64_t now_ms;

    if (!frontend || !frontend->vk->vk) {
        return;
    }

    now_ms = typio_wl_monotonic_ms();
    frontend->vk->keymap_deadline_ms = now_ms + TYPIO_WL_VK_KEYMAP_TIMEOUT_MS;
    typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_NEEDS_KEYMAP,
                          reason ? reason : "awaiting keymap");
    typio_wl_trace(frontend,
                   "vk_state",
                   "awaiting=keymap timeout_ms=%u reason=%s",
                   TYPIO_WL_VK_KEYMAP_TIMEOUT_MS,
                   reason ? reason : "awaiting keymap");
    typio_wl_frontend_emit_runtime_state_changed(frontend);
}

void typio_wl_vk_cancel_keymap_wait(TypioWlFrontend *frontend,
                                    const char *reason) {
    if (!frontend ||
        frontend->vk->state != TYPIO_WL_VK_STATE_NEEDS_KEYMAP) {
        return;
    }

    typio_wl_vk_record_keymap_cancel(frontend, reason);

    if (typio_wl_vk_has_current_generation_keymap(frontend)) {
        typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_READY,
                              reason ? reason : "keymap wait cancelled");
        return;
    }

    frontend->vk->keymap_deadline_ms = 0;
    typio_wl_trace(frontend,
                   "vk_state",
                   "awaiting=keymap timeout_ms=cancelled reason=%s",
                   reason ? reason : "keymap wait cancelled");
    typio_wl_frontend_emit_runtime_state_changed(frontend);
}

static void typio_wl_vk_trigger_fail_safe(TypioWlFrontend *frontend,
                                          const char *operation,
                                          uint64_t drops) {
    uint64_t now_ms;
    uint64_t last_keymap_age_ms;
    uint64_t last_forward_age_ms;
    uint64_t cancel_window_ms;
    bool has_current_keymap;
    bool keyboard_grab_active;

    if (!frontend || !frontend->running) {
        return;
    }

    now_ms = typio_wl_monotonic_ms();
    last_keymap_age_ms = typio_wl_vk_age_ms(now_ms,
                                            frontend->vk->last_keymap_ms);
    last_forward_age_ms = typio_wl_vk_age_ms(now_ms,
                                             frontend->vk->last_forward_ms);
    cancel_window_ms = typio_wl_vk_age_ms(
        now_ms, frontend->vk->keymap_cancel_window_start_ms);
    has_current_keymap = typio_wl_vk_has_current_generation_keymap(frontend);
    keyboard_grab_active = frontend->keyboard && frontend->keyboard->grab;

    typio_log_error("Virtual keyboard fail-safe stop: operation=%s state=%s drops=%" PRIu64
              " cancel_count=%" PRIu64 " cancel_window_ms=%" PRIu64
              " active_generation=%u keymap_generation=%u"
              " current_generation_keymap=%s keyboard_grab=%s"
              " last_keymap_age_ms=%" PRIu64 " last_forward_age_ms=%" PRIu64
              " phase=%s",
              operation ? operation : "event",
              typio_wl_vk_state_name(frontend->vk->state),
              drops,
              frontend->vk->keymap_cancel_count,
              cancel_window_ms,
              frontend->tracker->active_generation,
              frontend->vk->keymap_generation,
              has_current_keymap ? "yes" : "no",
              keyboard_grab_active ? "yes" : "no",
              last_keymap_age_ms,
              last_forward_age_ms,
              typio_wl_grab_resource_state_name(
                  typio_wl_session_observe(frontend).grab));
    typio_dump_recent_log();
    if (frontend->keyboard) {
        typio_wl_keyboard_release_grab(frontend->keyboard);
    }
    typio_wl_frontend_stop(frontend);
}

bool typio_wl_vk_is_ready(TypioWlFrontend *frontend,
                          const char *operation) {
    uint64_t drops;

    if (!frontend)
        return false;

    if (frontend->vk->vk &&
        frontend->vk->state == TYPIO_WL_VK_STATE_NEEDS_KEYMAP &&
        typio_wl_vk_has_current_generation_keymap(frontend)) {
        typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_READY,
                              "keymap available");
    }

    if (frontend->vk->state == TYPIO_WL_VK_STATE_READY)
        return true;

    frontend->vk->drop_count++;
    drops = frontend->vk->drop_count;
    if (drops == 1 || drops % 50 == 0) {
        typio_log_warning("Dropped virtual keyboard %s: state=%s drops=%" PRIu64,
                  operation ? operation : "event",
                  typio_wl_vk_state_name(frontend->vk->state),
                  drops);
    }

    if (frontend->vk->state == TYPIO_WL_VK_STATE_BROKEN) {
        typio_wl_vk_trigger_fail_safe(frontend, operation, drops);
    }

    return false;
}

void typio_wl_vk_health_check(TypioWlFrontend *frontend) {
    uint64_t now_ms;

    if (!frontend || !frontend->running ||
        !frontend->keyboard || !frontend->keyboard->grab ||
        frontend->vk->state != TYPIO_WL_VK_STATE_NEEDS_KEYMAP ||
        frontend->vk->keymap_deadline_ms == 0) {
        return;
    }

    now_ms = typio_wl_monotonic_ms();
    if (now_ms < frontend->vk->keymap_deadline_ms) {
        return;
    }

    typio_log_error("Virtual keyboard keymap timeout: waited=%" PRIu64 "ms since_state=%" PRIu64 "ms last_keymap=%" PRIu64 " last_forward=%" PRIu64,
              now_ms - frontend->vk->state_since_ms,
              frontend->vk->state_since_ms,
              frontend->vk->last_keymap_ms,
              frontend->vk->last_forward_ms);
    typio_wl_vk_trigger_fail_safe(frontend, "keymap timeout",
                                  frontend->vk->drop_count);
}

void typio_wl_vk_forward_key(struct TypioWlKeyboard *keyboard,
                             uint32_t time,
                             uint32_t key,
                             uint32_t state,
                             uint32_t unicode) {
    const char *reason = "forward";
    TypioWlFrontend *frontend;
    char unicode_desc[64];

    if (!keyboard)
        return;

    frontend = keyboard->frontend;
    if (!frontend || !typio_wl_vk_is_ready(frontend, "key"))
        return;

    typio_wl_vk_mark_forward_progress(frontend);
    if (key < TYPIO_WL_MAX_TRACKED_KEYS &&
        frontend->tracker->states[key] == TYPIO_KEY_TRACK_RELEASED_PENDING) {
        reason = "synthetic_release";
    }
    typio_wl_key_debug_format(unicode, unicode_desc, sizeof(unicode_desc));

    typio_wl_trace(frontend,
                   "vk_key",
                   "state=%s keycode=%u time=%u %s reason=%s",
                   state == WL_KEYBOARD_KEY_STATE_PRESSED ? "press" : "release",
                   key, time, unicode_desc, reason);
    zwp_virtual_keyboard_v1_key(frontend->vk->vk, time, key, state);
}

void typio_wl_vk_forward_modifiers(struct TypioWlKeyboard *keyboard,
                                   uint32_t mods_depressed,
                                   uint32_t mods_latched,
                                   uint32_t mods_locked,
                                   uint32_t group) {
    TypioWlFrontend *frontend;

    if (!keyboard)
        return;

    frontend = keyboard->frontend;
    if (!frontend || !typio_wl_vk_is_ready(frontend, "modifier update"))
        return;

    typio_wl_vk_mark_forward_progress(frontend);
    typio_wl_trace(frontend,
                   "vk_modifiers",
                   "depressed=0x%x latched=0x%x locked=0x%x group=%u",
                   mods_depressed, mods_latched, mods_locked, group);
    zwp_virtual_keyboard_v1_modifiers(frontend->vk->vk,
                                      mods_depressed, mods_latched,
                                      mods_locked, group);
}

void typio_wl_vk_forward_modifier_state(TypioWlFrontend *frontend,
                                        uint32_t mods_depressed,
                                        uint32_t mods_latched,
                                        uint32_t mods_locked,
                                        uint32_t group) {
    if (!frontend || !typio_wl_vk_is_ready(frontend, "modifier carry"))
        return;

    typio_wl_vk_mark_forward_progress(frontend);
    typio_wl_trace(frontend,
                   "vk_modifiers",
                   "depressed=0x%x latched=0x%x locked=0x%x group=%u",
                   mods_depressed, mods_latched, mods_locked, group);
    zwp_virtual_keyboard_v1_modifiers(frontend->vk->vk,
                                      mods_depressed, mods_latched,
                                      mods_locked, group);
}

void typio_wl_vk_release_forwarded_keys(TypioWlFrontend *frontend,
                                        const char *(*key_state_name)(TypioKeyTrackState state)) {
    size_t released;
    uint32_t time;
    bool use_generic_name;

    if (!frontend || !frontend->vk->vk ||
        frontend->vk->state == TYPIO_WL_VK_STATE_ABSENT ||
        frontend->vk->state == TYPIO_WL_VK_STATE_BROKEN)
        return;

    time = (uint32_t)typio_wl_monotonic_ms();
    use_generic_name = key_state_name == nullptr;

    for (size_t key = 0; key < TYPIO_WL_MAX_TRACKED_KEYS; key++) {
        if (frontend->tracker->states[key] != TYPIO_KEY_TRACK_FORWARDED &&
            frontend->tracker->states[key] != TYPIO_KEY_TRACK_BASIC_PASSTHROUGH &&
            frontend->tracker->states[key] != TYPIO_KEY_TRACK_APP_SHORTCUT)
            continue;

        typio_wl_trace(frontend,
                       "vk_key",
                       "state=release keycode=%zu time=%u unicode=none char=- reason=hard_reset route=%s",
                       key, time,
                       use_generic_name ? "tracked"
                                        : key_state_name(frontend->tracker->states[key]));
        zwp_virtual_keyboard_v1_key(frontend->vk->vk,
                                    time, (uint32_t)key,
                                    WL_KEYBOARD_KEY_STATE_RELEASED);
    }

    released = typio_wl_key_tracking_mark_released_pending(frontend->tracker->states,
                                                           TYPIO_WL_MAX_TRACKED_KEYS);
    if (released > 0) {
        typio_log_debug("Force-released %zu forwarded keys at lifecycle boundary",
                  released);
    }
}

void typio_wl_vk_reset_modifiers(TypioWlFrontend *frontend) {
    if (!frontend || !typio_wl_vk_is_ready(frontend, "modifier reset"))
        return;

    typio_wl_vk_mark_forward_progress(frontend);
    typio_wl_trace(frontend,
                   "vk_reset_modifiers",
                   "depressed=0x0 latched=0x0 locked=0x0 group=0");
    zwp_virtual_keyboard_v1_modifiers(frontend->vk->vk, 0, 0, 0, 0);
    frontend->vk->carried_modifiers = false;

    if (frontend->keyboard) {
        frontend->keyboard->physical_modifiers = 0;
        frontend->keyboard->mods_depressed = 0;
        frontend->keyboard->mods_latched = 0;
        frontend->keyboard->mods_locked = 0;
        frontend->keyboard->mods_group = 0;
        if (frontend->keyboard->xkb_state) {
            xkb_state_update_mask(frontend->keyboard->xkb_state,
                                  0, 0, 0, 0, 0, 0);
        }
    }
}

void typio_wl_vk_forward_keymap(TypioWlFrontend *frontend,
                                uint32_t format,
                                int32_t fd,
                                uint32_t size) {
    int vk_fd;

    if (!frontend || !frontend->vk->vk)
        return;

    vk_fd = dup(fd);
    if (vk_fd < 0) {
        typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_BROKEN,
                              "failed to duplicate keymap fd");
        return;
    }

    zwp_virtual_keyboard_v1_keymap(frontend->vk->vk, format, vk_fd, size);
    close(vk_fd);
    frontend->vk->last_keymap_ms = typio_wl_monotonic_ms();
    frontend->vk->keymap_generation = frontend->tracker->active_generation;
    typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_READY,
                          "received compositor keymap");
    typio_wl_trace(frontend,
                   "keymap",
                   "stage=forwarded_to_vk format=%u size=%u",
                   format, size);
    typio_wl_frontend_emit_runtime_state_changed(frontend);
}
