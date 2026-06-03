/**
 * @file controller.c
 * @brief StateController implementation
 */

#include "state/controller.h"
#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"
#include "typio/abi/engine.h"
#include "typio/abi/string.h"
#include "typio/typio.h"
#include "typio/abi/log.h"

#include <stdlib.h>
#include <string.h>

struct TypioStateController {
    TypioInstance *instance;

    /* -- cached state snapshots ------------------------------------------- */
    char *active_engine_name;
    char *active_engine_display_name;
    char *active_voice_engine_name;
    char *active_voice_engine_display_name;
    char *status_icon;

    bool engine_active;

    bool has_status;
    TypioKeyboardEngineMode status;
    char *status_id;
    char *status_label;
    char *status_display_label;
    char *status_icon_name;

    /* -- listeners -------------------------------------------------------- */
    TypioStateListener *listeners;
    size_t listener_count;
    size_t listener_capacity;
};

/* -------------------------------------------------------------------------- */
/* Helpers                                                                    */
/* -------------------------------------------------------------------------- */

static char *typio_state_strdup(const char *src) {
    if (!src || !*src) {
        return nullptr;
    }
    return strdup(src);
}

static void typio_state_controller_broadcast(TypioStateController *ctrl,
                                             TypioStateChangeType change_type) {
    if (!ctrl) {
        return;
    }
    for (size_t i = 0; i < ctrl->listener_count; i++) {
        TypioStateListener *l = &ctrl->listeners[i];
        if (l->callback) {
            l->callback(l->user_data, change_type);
        }
    }
}

static void typio_state_controller_update_engine_active(
    TypioStateController *ctrl) {
    if (!ctrl || !ctrl->instance) {
        return;
    }
    TypioRegistry *registry = typio_instance_get_registry(ctrl->instance);
    char *active_name =
        registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    ctrl->engine_active = active_name != nullptr;
    typio_free_string(active_name);
}

static void typio_state_controller_clear_mode(TypioStateController *ctrl) {
    free(ctrl->status_id);
    free(ctrl->status_label);
    free(ctrl->status_display_label);
    free(ctrl->status_icon_name);
    ctrl->status_id = nullptr;
    ctrl->status_label = nullptr;
    ctrl->status_display_label = nullptr;
    ctrl->status_icon_name = nullptr;
    ctrl->has_status = false;
    memset(&ctrl->status, 0, sizeof(ctrl->status));
}

static void typio_state_controller_set_mode(TypioStateController *ctrl,
                                            const TypioKeyboardEngineMode *mode) {
    typio_state_controller_clear_mode(ctrl);
    if (!mode) {
        return;
    }
    ctrl->has_status = true;
    ctrl->status.id = ctrl->status_id = typio_state_strdup(mode->id);
    ctrl->status.label =
        ctrl->status_label = typio_state_strdup(mode->label);
    ctrl->status.display_label =
        ctrl->status_display_label = typio_state_strdup(mode->display_label);
    ctrl->status.icon_name = ctrl->status_icon_name = typio_state_strdup(mode->icon_name);
}

/* -------------------------------------------------------------------------- */
/* Lifecycle                                                                  */
/* -------------------------------------------------------------------------- */

TypioStateController *typio_state_controller_new(TypioInstance *instance) {
    if (!instance) {
        return nullptr;
    }
    TypioStateController *ctrl = calloc(1, sizeof(TypioStateController));
    if (!ctrl) {
        return nullptr;
    }
    ctrl->instance = instance;
    ctrl->listener_capacity = 4;
    ctrl->listeners = calloc(ctrl->listener_capacity, sizeof(TypioStateListener));
    if (!ctrl->listeners) {
        free(ctrl);
        return nullptr;
    }
    return ctrl;
}

void typio_state_controller_free(TypioStateController *ctrl) {
    if (!ctrl) {
        return;
    }
    free(ctrl->active_engine_name);
    free(ctrl->active_engine_display_name);
    free(ctrl->active_voice_engine_name);
    free(ctrl->active_voice_engine_display_name);
    free(ctrl->status_icon);
    typio_state_controller_clear_mode(ctrl);
    free(ctrl->listeners);
    free(ctrl);
}

/* -------------------------------------------------------------------------- */
/* Listeners                                                                  */
/* -------------------------------------------------------------------------- */

void typio_state_controller_add_listener(TypioStateController *ctrl,
                                         TypioStateListener listener) {
    if (!ctrl) {
        return;
    }
    if (ctrl->listener_count >= ctrl->listener_capacity) {
        size_t new_cap = ctrl->listener_capacity * 2;
        TypioStateListener *new_list =
            realloc(ctrl->listeners, new_cap * sizeof(TypioStateListener));
        if (!new_list) {
            typio_log_error("Failed to grow state-controller listener list");
            return;
        }
        ctrl->listeners = new_list;
        ctrl->listener_capacity = new_cap;
    }
    ctrl->listeners[ctrl->listener_count++] = listener;
}

void typio_state_controller_remove_listener(TypioStateController *ctrl,
                                            void *user_data) {
    if (!ctrl) {
        return;
    }
    for (size_t i = 0; i < ctrl->listener_count; i++) {
        if (ctrl->listeners[i].user_data == user_data) {
            /* shift remaining entries down */
            size_t rest = ctrl->listener_count - i - 1;
            if (rest > 0) {
                memmove(&ctrl->listeners[i],
                        &ctrl->listeners[i + 1],
                        rest * sizeof(TypioStateListener));
            }
            ctrl->listener_count--;
            return;
        }
    }
}

/* -------------------------------------------------------------------------- */
/* Queries                                                                    */
/* -------------------------------------------------------------------------- */

const char *typio_state_controller_get_active_engine_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_engine_name : nullptr;
}

const char *typio_state_controller_get_active_engine_display_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_engine_display_name : nullptr;
}

const char *typio_state_controller_get_active_voice_engine_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_voice_engine_name : nullptr;
}

const char *typio_state_controller_get_active_voice_engine_display_name(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->active_voice_engine_display_name : nullptr;
}

const char *typio_state_controller_get_status_icon(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->status_icon : nullptr;
}

bool typio_state_controller_get_engine_active(
    TypioStateController *ctrl) {
    return ctrl ? ctrl->engine_active : false;
}

const TypioKeyboardEngineMode *typio_state_controller_get_current_status(
    TypioStateController *ctrl) {
    if (!ctrl || !ctrl->has_status) {
        return nullptr;
    }
    return &ctrl->status;
}

/* -------------------------------------------------------------------------- */
/* Core notifications                                                         */
/* -------------------------------------------------------------------------- */

void typio_state_controller_notify_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info) {
    if (!ctrl) {
        return;
    }

    /* Determine if the engine actually changed before freeing old name. */
    bool engine_changed = true;
    if (ctrl->active_engine_name && info && info->name) {
        engine_changed = strcmp(ctrl->active_engine_name, info->name) != 0;
    }

    free(ctrl->active_engine_name);
    free(ctrl->active_engine_display_name);
    ctrl->active_engine_name =
        (info && info->name) ? strdup(info->name) : nullptr;
    ctrl->active_engine_display_name =
        (info && info->display_name) ? strdup(info->display_name) : nullptr;

    /* Re-evaluate status icon.
     *
     * For the SAME engine the dynamic icon (last_status_icon) takes
     * precedence so that schema/mode switches inside one engine keep the
     * correct tray icon.
     *
     * When the engine CHANGES we must NOT reuse the previous engine's
     * dynamic icon because typio_instance_get_last_status_icon is global
     * and would be stale.  Fall back to the new engine's static icon. */
    {
        free(ctrl->status_icon);
        if (!engine_changed) {
            const char *icon = typio_instance_get_last_status_icon(ctrl->instance);
            if (icon && *icon) {
                ctrl->status_icon = strdup(icon);
            } else if (info && info->icon && info->icon[0]) {
                ctrl->status_icon = strdup(info->icon);
            } else if (info) {
                ctrl->status_icon = strdup("typio-keyboard-symbolic");
            } else {
                ctrl->status_icon = strdup("typio-keyboard-off-symbolic");
            }
        } else {
            if (info && info->icon && info->icon[0]) {
                ctrl->status_icon = strdup(info->icon);
            } else if (info) {
                ctrl->status_icon = strdup("typio-keyboard-symbolic");
            } else {
                ctrl->status_icon = strdup("typio-keyboard-off-symbolic");
            }
        }
    }

    typio_state_controller_update_engine_active(ctrl);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_ENGINE);
}

void typio_state_controller_notify_voice_engine_changed(
    TypioStateController *ctrl,
    const TypioEngineInfo *info) {
    if (!ctrl) {
        return;
    }
    free(ctrl->active_voice_engine_name);
    free(ctrl->active_voice_engine_display_name);
    ctrl->active_voice_engine_name =
        (info && info->name) ? strdup(info->name) : nullptr;
    ctrl->active_voice_engine_display_name =
        (info && info->display_name) ? strdup(info->display_name) : nullptr;
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_VOICE_ENGINE);
}

void typio_state_controller_notify_status_changed(
    TypioStateController *ctrl,
    const TypioKeyboardEngineMode *mode) {
    if (!ctrl) {
        return;
    }
    typio_state_controller_set_mode(ctrl, mode);
    if (mode && mode->icon_name && mode->icon_name[0]) {
        free(ctrl->status_icon);
        ctrl->status_icon = strdup(mode->icon_name);
    }
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS);
}

void typio_state_controller_notify_status_icon_changed(
    TypioStateController *ctrl,
    const char *icon_name) {
    if (!ctrl) {
        return;
    }
    free(ctrl->status_icon);
    ctrl->status_icon = typio_state_strdup(icon_name);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
}

/* -------------------------------------------------------------------------- */
/* Sync                                                                       */
/* -------------------------------------------------------------------------- */

void typio_state_controller_sync(TypioStateController *ctrl) {
    if (!ctrl || !ctrl->instance) {
        return;
    }

    TypioRegistry *registry = typio_instance_get_registry(ctrl->instance);

    /* Engine */
    char *active_kb_name = registry
        ? typio_registry_get_active_keyboard(registry) : nullptr;
    char *active_kb_display = (active_kb_name && registry)
        ? typio_registry_get_engine_display_name(registry, active_kb_name)
        : nullptr;
    char *active_kb_icon = (active_kb_name && registry)
        ? typio_registry_get_engine_icon(registry, active_kb_name)
        : nullptr;
    {
        free(ctrl->active_engine_name);
        free(ctrl->active_engine_display_name);
        ctrl->active_engine_name = active_kb_name
            ? typio_state_strdup(active_kb_name) : nullptr;
        ctrl->active_engine_display_name = (active_kb_display && *active_kb_display)
            ? typio_state_strdup(active_kb_display) : nullptr;
        ctrl->engine_active = active_kb_name != nullptr;
    }

    /* Voice engine */
    {
        char *voice_name = registry
            ? typio_registry_get_active_voice(registry) : nullptr;
        char *voice_display = (voice_name && registry)
            ? typio_registry_get_engine_display_name(registry, voice_name)
            : nullptr;
        free(ctrl->active_voice_engine_name);
        free(ctrl->active_voice_engine_display_name);
        ctrl->active_voice_engine_name = voice_name
            ? typio_state_strdup(voice_name) : nullptr;
        ctrl->active_voice_engine_display_name = (voice_display && *voice_display)
            ? typio_state_strdup(voice_display) : nullptr;
        typio_free_string(voice_name);
        typio_free_string(voice_display);
    }

    /* Status icon */
    {
        free(ctrl->status_icon);
        const char *icon = typio_instance_get_last_status_icon(ctrl->instance);
        if (icon && *icon) {
            ctrl->status_icon = strdup(icon);
        } else if (active_kb_icon && *active_kb_icon) {
            ctrl->status_icon = strdup(active_kb_icon);
        } else if (active_kb_name) {
            ctrl->status_icon = strdup("typio-keyboard-symbolic");
        } else {
            ctrl->status_icon = strdup("typio-keyboard-off-symbolic");
        }
    }

    typio_free_string(active_kb_icon);
    typio_free_string(active_kb_display);
    typio_free_string(active_kb_name);

    /* Mode — we cannot query current mode directly from instance, so we
     * clear it and wait for the next mode notification from Core. */
    typio_state_controller_clear_mode(ctrl);

    /* Broadcast every change type so listeners perform a full refresh. */
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_ENGINE);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_VOICE_ENGINE);
    typio_state_controller_broadcast(ctrl, TYPIO_STATE_CHANGE_STATUS_ICON);
}
