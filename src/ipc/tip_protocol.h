/**
 * @file tip_protocol.h
 * @brief Typio IPC Protocol (TIP) v1 method/topic constants
 *
 * Wire vocabulary for the daemon UDS control surface (ADR-0008).
 * Resource-oriented dotted camelCase methods. The first IPC version with
 * an explicit `protocolVersion` field reported by `hello`; the older
 * unversioned vocabulary is not interoperable.
 */

#ifndef TYPIO_TIP_PROTOCOL_H
#define TYPIO_TIP_PROTOCOL_H

#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---------- Protocol version reported by hello ---------- */
#define TYPIO_IPC_PROTOCOL_VERSION 1

/**
 * @brief Return the canonical UDS socket path.
 *
 * Prefers $XDG_RUNTIME_DIR/typio/daemon.sock.
 * Falls back to ~/.local/share/typio/daemon.sock.
 * Caller must free() the returned string.
 */
char *typio_ipc_socket_path(void);

/* ---------- JSON-RPC 2.0 methods (TIP v1) ---------- */
#define TYPIO_IPC_METHOD_HELLO            "hello"

#define TYPIO_IPC_METHOD_CONFIG_GET       "config.get"
#define TYPIO_IPC_METHOD_CONFIG_SET       "config.set"
#define TYPIO_IPC_METHOD_CONFIG_UNSET     "config.unset"
#define TYPIO_IPC_METHOD_CONFIG_LIST      "config.list"
#define TYPIO_IPC_METHOD_CONFIG_SHOW      "config.show"
#define TYPIO_IPC_METHOD_CONFIG_RELOAD    "config.reload"

#define TYPIO_IPC_METHOD_ENGINE_LIST      "engine.list"
#define TYPIO_IPC_METHOD_ENGINE_DESCRIBE  "engine.describe"
#define TYPIO_IPC_METHOD_ENGINE_USE       "engine.use"
#define TYPIO_IPC_METHOD_ENGINE_NEXT      "engine.next"
#define TYPIO_IPC_METHOD_ENGINE_INVOKE    "engine.invoke"

#define TYPIO_IPC_METHOD_DAEMON_STATUS    "daemon.status"
#define TYPIO_IPC_METHOD_DAEMON_STOP      "daemon.stop"
#define TYPIO_IPC_METHOD_DAEMON_VERSION   "daemon.version"

#define TYPIO_IPC_METHOD_EVENTS_SUBSCRIBE "events.subscribe"

/* ---------- Event topics (server -> client notification.method) ---------- */
#define TYPIO_IPC_TOPIC_ENGINE_CHANGED      "engine.changed"
#define TYPIO_IPC_TOPIC_ENGINE_MODE_CHANGED "engine.modeChanged"
#define TYPIO_IPC_TOPIC_CONFIG_CHANGED      "config.changed"
#define TYPIO_IPC_TOPIC_RUNTIME_CHANGED     "runtime.changed"
#define TYPIO_IPC_TOPIC_DAEMON_SHUTDOWN     "daemon.shuttingDown"

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_TIP_PROTOCOL_H */
