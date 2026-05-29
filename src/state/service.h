/**
 * @file service.h
 * @brief Transport-agnostic IPC dispatch (ADR-0008).
 *
 * Implements TIP v1 method handlers (config.*, engine.*, daemon.*, events.*).
 * Knows nothing about UDS framing — that lives in `ipc/uds_server.c`.
 */

#ifndef TYPIO_STATUS_SERVICE_H
#define TYPIO_STATUS_SERVICE_H

#include "state/controller.h"
#include "typio/abi/types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioStatusService TypioStatusService;

typedef void (*TypioStatusServiceStopCallback)(void *user_data);
typedef struct TypioStatusRuntimeState {
    const char *frontend_backend;
    const char *lifecycle_phase;
    const char *virtual_keyboard_state;
    bool keyboard_grab_active;
    bool virtual_keyboard_has_keymap;
    bool watchdog_armed;
    uint32_t active_key_generation;
    uint32_t virtual_keyboard_keymap_generation;
    uint32_t virtual_keyboard_drop_count;
    uint32_t virtual_keyboard_state_age_ms;
    uint32_t virtual_keyboard_keymap_age_ms;
    uint32_t virtual_keyboard_forward_age_ms;
    int32_t virtual_keyboard_keymap_deadline_remaining_ms;
} TypioStatusRuntimeState;

typedef void (*TypioStatusServiceRuntimeStateCallback)(void *user_data,
                                                        TypioStatusRuntimeState *state);

/**
 * @brief Called when a client invokes `events.subscribe`.
 *
 * @param client_ctx   Opaque token identifying the client (from the
 *                     transport — e.g. the UDS client pointer).
 * @param topics       Topic names the client subscribed to, or NULL/0 for
 *                     "all topics".
 */
typedef void (*TypioStatusServiceSubscribeCallback)(
    void *user_data, void *client_ctx,
    const char *const *topics, size_t topic_count);

TypioStatusService *typio_status_service_new(TypioInstance *instance);
void typio_status_service_destroy(TypioStatusService *svc);

/**
 * @brief Dispatch a JSON-RPC method.
 *
 * @param method     Method name (e.g. `"engine.list"`).
 * @param params     Raw JSON object for params, or NULL.
 * @param id         JSON-RPC request id.
 * @param client_ctx Opaque transport-side identifier for the calling client.
 *                   May be NULL for transports that do not support
 *                   `events.subscribe`. Forwarded to the subscribe callback.
 * @return malloc'd JSON-RPC response (success or error envelope).
 */
char *typio_status_service_handle(TypioStatusService *svc,
                                   const char *method,
                                   const char *params,
                                   int id,
                                   void *client_ctx);

void typio_status_service_set_stop_callback(TypioStatusService *svc,
                                             TypioStatusServiceStopCallback cb,
                                             void *user_data);
void typio_status_service_set_runtime_state_callback(
    TypioStatusService *svc,
    TypioStatusServiceRuntimeStateCallback cb,
    void *user_data);
void typio_status_service_set_subscribe_callback(
    TypioStatusService *svc,
    TypioStatusServiceSubscribeCallback cb,
    void *user_data);
void typio_status_service_bind_state_controller(TypioStatusService *svc,
                                                 TypioStateController *ctrl);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_STATUS_SERVICE_H */
