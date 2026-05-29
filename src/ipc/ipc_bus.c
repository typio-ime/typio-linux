/**
 * @file ipc_bus.c
 * @brief Glue: TypioStatusService ↔ UDS transport ↔ state controller.
 *
 * Per ADR-0008 the IPC bus is the only control transport. It:
 *
 *   1. Routes incoming JSON-RPC frames into `typio_status_service_handle`.
 *   2. Owns subscription state by passing through to `uds_server_subscribe`.
 *   3. Listens on the state controller and pushes events to subscribed
 *      clients as JSON-RPC notifications on dotted topics (engine.changed,
 *      engine.modeChanged, config.changed, runtime.changed,
 *      daemon.shuttingDown).
 */

#include "ipc_bus.h"
#include "state/controller.h"
#include "state/service.h"
#include "tip_json.h"
#include "tip_protocol.h"
#include "uds_server.h"

#include "typio/runtime/registry.h"
#include "typio/runtime/instance.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#include <stdlib.h>
#include <string.h>

struct TypioIpcBus {
    TypioStatusService *service;
    TypioUdsServer *uds;
    TypioInstance *instance;
    TypioStateController *state_controller;
};

/* ------------------------------------------------------------------ */
/*  Request dispatcher                                                */
/* ------------------------------------------------------------------ */

static char *ipc_bus_handle_request(const char *json_request,
                                     TypioUdsClient *client,
                                     void *user_data)
{
    TypioIpcBus *bus = user_data;
    if (!bus || !bus->service)
        return tip_json_build_error(0, -32603, "Internal error");

    int id = 0;
    tip_json_extract_id(json_request, &id);
    char *method = tip_json_extract_string(json_request, "method");
    if (!method)
        return tip_json_build_error(id, -32600,
                                    "Invalid Request: missing method");

    const char *params_start = NULL;
    size_t params_len = 0;
    char *params = NULL;
    if (tip_json_extract_params(json_request, &params_start, &params_len)
        && params_start && params_len > 0) {
        params = malloc(params_len + 1);
        if (params) {
            memcpy(params, params_start, params_len);
            params[params_len] = '\0';
        }
    }

    char *response = typio_status_service_handle(bus->service, method, params,
                                                 id, client);
    free(method);
    free(params);

    if (!response)
        return tip_json_build_error(id, -32603, "Internal error");
    return response;
}

/* ------------------------------------------------------------------ */
/*  Subscribe callback                                                */
/* ------------------------------------------------------------------ */

static void ipc_bus_on_subscribe(void *user_data, void *client_ctx,
                                  const char *const *topics, size_t n)
{
    TypioIpcBus *bus = user_data;
    if (!bus || !bus->uds) return;
    typio_uds_server_subscribe(bus->uds, (TypioUdsClient *)client_ctx, topics, n);
}

/* ------------------------------------------------------------------ */
/*  State change → event emission                                     */
/* ------------------------------------------------------------------ */

static char *build_engine_changed_payload(TypioInstance *inst)
{
    TypioRegistry *reg = inst ? typio_instance_get_registry(inst) : NULL;
    char *active_kb = reg ? typio_registry_get_active_keyboard(reg) : NULL;
    char *active_voice = reg ? typio_registry_get_active_voice(reg) : NULL;
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    TIP_JSON_KEY(b, "activeKeyboardEngine");
    tip_json_builder_append_string(b, active_kb ? active_kb : "");
    TIP_JSON_COMMA(b);
    TIP_JSON_KEY(b, "activeVoiceEngine");
    tip_json_builder_append_string(b, active_voice ? active_voice : "");
    TIP_JSON_OBJ_END(b);
    typio_free_string(active_kb);
    typio_free_string(active_voice);
    return tip_json_builder_steal(b);
}

static char *build_mode_changed_payload(TypioStateController *ctrl)
{
    const TypioEngineMode *mode = ctrl
        ? typio_state_controller_get_current_mode(ctrl) : NULL;
    TipJsonBuilder *b = tip_json_builder_new();
    TIP_JSON_OBJ_START(b);
    if (mode) {
        TIP_JSON_KEY(b, "modeClass");
        tip_json_builder_append_string(b,
            mode->mode_class == TYPIO_MODE_CLASS_LATIN ? "latin" : "native");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "modeId");
        tip_json_builder_append_string(b, mode->mode_id ? mode->mode_id : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "displayLabel");
        tip_json_builder_append_string(b,
            mode->display_label ? mode->display_label : "");
        TIP_JSON_COMMA(b);
        TIP_JSON_KEY(b, "iconName");
        tip_json_builder_append_string(b,
            mode->icon_name ? mode->icon_name : "");
    }
    TIP_JSON_OBJ_END(b);
    return tip_json_builder_steal(b);
}

static void ipc_bus_state_listener(void *user_data,
                                    TypioStateChangeType change_type)
{
    TypioIpcBus *bus = user_data;
    if (!bus || !bus->uds) return;

    switch (change_type) {
    case TYPIO_STATE_CHANGE_ENGINE:
    case TYPIO_STATE_CHANGE_VOICE_ENGINE: {
        char *payload = build_engine_changed_payload(bus->instance);
        typio_uds_server_emit(bus->uds, TYPIO_IPC_TOPIC_ENGINE_CHANGED, payload);
        free(payload);
        break;
    }
    case TYPIO_STATE_CHANGE_MODE: {
        char *payload = build_mode_changed_payload(bus->state_controller);
        typio_uds_server_emit(bus->uds, TYPIO_IPC_TOPIC_ENGINE_MODE_CHANGED, payload);
        free(payload);
        break;
    }
    case TYPIO_STATE_CHANGE_STATUS_ICON:
        /* Status icon is consumed by the systray adapter, not pushed over UDS. */
        break;
    }
}

/* ------------------------------------------------------------------ */
/*  Public API                                                        */
/* ------------------------------------------------------------------ */

TypioIpcBus *typio_ipc_bus_new(TypioInstance *instance)
{
    if (!instance) return NULL;

    TypioIpcBus *bus = calloc(1, sizeof(*bus));
    if (!bus) return NULL;
    bus->instance = instance;

    bus->service = typio_status_service_new(instance);
    if (!bus->service) goto fail;

    char *socket_path = typio_ipc_socket_path();
    if (!socket_path) goto fail;

    bus->uds = typio_uds_server_new(socket_path);
    free(socket_path);
    if (!bus->uds) goto fail;

    typio_uds_server_set_handler(bus->uds, ipc_bus_handle_request, bus);
    typio_status_service_set_subscribe_callback(bus->service,
                                                 ipc_bus_on_subscribe, bus);
    typio_log_info("Typio IPC bus initialized (TIP v%d)",
                   TYPIO_IPC_PROTOCOL_VERSION);
    return bus;

fail:
    if (bus->uds) typio_uds_server_destroy(bus->uds);
    if (bus->service) typio_status_service_destroy(bus->service);
    free(bus);
    return NULL;
}

void typio_ipc_bus_destroy(TypioIpcBus *bus)
{
    if (!bus) return;
    if (bus->uds)
        typio_uds_server_emit(bus->uds, TYPIO_IPC_TOPIC_DAEMON_SHUTDOWN, "{}");
    if (bus->state_controller)
        typio_state_controller_remove_listener(bus->state_controller, bus);
    if (bus->uds) typio_uds_server_destroy(bus->uds);
    if (bus->service) typio_status_service_destroy(bus->service);
    free(bus);
}

int typio_ipc_bus_get_fd(TypioIpcBus *bus)
{
    return bus ? typio_uds_server_get_fd(bus->uds) : -1;
}

void typio_ipc_bus_dispatch(TypioIpcBus *bus)
{
    if (!bus || !bus->uds) return;
    typio_uds_server_dispatch(bus->uds);
}

void typio_ipc_bus_set_runtime_state_callback(TypioIpcBus *bus,
                                               TypioIpcBusRuntimeStateCallback callback,
                                               void *user_data)
{
    if (!bus) return;
    typio_status_service_set_runtime_state_callback(
        bus->service,
        (TypioStatusServiceRuntimeStateCallback)callback, user_data);
}

void typio_ipc_bus_set_stop_callback(TypioIpcBus *bus,
                                      TypioIpcBusStopCallback callback,
                                      void *user_data)
{
    if (!bus) return;
    typio_status_service_set_stop_callback(
        bus->service,
        (TypioStatusServiceStopCallback)callback, user_data);
}

void typio_ipc_bus_bind_state_controller(TypioIpcBus *bus,
                                          struct TypioStateController *ctrl)
{
    if (!bus) return;
    if (bus->state_controller && bus->state_controller != ctrl)
        typio_state_controller_remove_listener(bus->state_controller, bus);
    bus->state_controller = ctrl;
    typio_status_service_bind_state_controller(bus->service, ctrl);
    if (ctrl)
        typio_state_controller_add_listener(
            ctrl,
            (TypioStateListener){ .user_data = bus,
                                  .callback = ipc_bus_state_listener });
}
