/**
 * @file uds_server.h
 * @brief Unix Domain Socket server for the Typio IPC protocol (ADR-0008).
 *
 * Multiplexes multiple long-lived client connections via epoll(7).
 * Each connection carries request/response method calls and may opt into
 * a server→client event stream via `events.subscribe`.
 *
 * The epoll fd is exposed so the host's main loop can `poll()` it.
 */

#ifndef TYPIO_UDS_SERVER_H
#define TYPIO_UDS_SERVER_H

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioUdsServer TypioUdsServer;

/** Opaque per-client handle passed to the request handler. */
typedef struct TypioUdsClient TypioUdsClient;

/**
 * @brief Handler called for each complete JSON-RPC request frame.
 *
 * @param json_request  NUL-terminated JSON payload (no length prefix).
 * @param client        Opaque token identifying the connection. The
 *                      handler may pass this to
 *                      `typio_uds_server_subscribe` to mark the client as
 *                      a subscriber to specific event topics.
 * @param user_data     The pointer passed to `set_handler`.
 * @return malloc'd JSON response string, or NULL for no reply.
 */
typedef char *(*TypioUdsRequestHandler)(const char *json_request,
                                         TypioUdsClient *client,
                                         void *user_data);

TypioUdsServer *typio_uds_server_new(const char *socket_path);
void typio_uds_server_destroy(TypioUdsServer *srv);
int typio_uds_server_get_fd(TypioUdsServer *srv);
void typio_uds_server_dispatch(TypioUdsServer *srv);

void typio_uds_server_set_handler(TypioUdsServer *srv,
                                   TypioUdsRequestHandler handler,
                                   void *user_data);

/**
 * @brief Mark @p client as a subscriber.
 *
 * @param topics      Topic names to subscribe to. NULL/0 = wildcard.
 * @param topic_count Number of strings in @p topics.
 *
 * Replaces any prior subscription on this client. Pass an empty list to
 * unsubscribe.
 */
void typio_uds_server_subscribe(TypioUdsServer *srv,
                                 TypioUdsClient *client,
                                 const char *const *topics,
                                 size_t topic_count);

/**
 * @brief Send a JSON notification to every subscribed client matching @p topic.
 *
 * The frame is the JSON-RPC notification envelope. Clients that subscribed
 * with a wildcard (no topics) receive every event; clients that subscribed
 * with an explicit list receive only the matching ones. Clients that have
 * not subscribed receive nothing.
 */
void typio_uds_server_emit(TypioUdsServer *srv,
                            const char *topic,
                            const char *payload_json);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_UDS_SERVER_H */
