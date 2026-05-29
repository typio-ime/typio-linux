/**
 * @file identity.c
 * @brief Focused-application identity providers and per-identity engine restore
 *
 * The TOML persistence layer (per-app engine/mode memory) lives in
 * core/src/instance/identity.rs.  This file retains only the
 * compositor-specific query logic and the mode-restore glue that
 * needs direct engine keyboard-op access.
 */

#include "identity.h"

#include "internal.h"
#include "typio/abi/engine.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

typedef struct TypioWlIdentityProviderOps {
    const char *name;
    bool (*query_current)(struct TypioWlIdentityProvider *provider,
                          TypioWlIdentity *identity);
} TypioWlIdentityProviderOps;

struct TypioWlIdentityProvider {
    TypioInstance *instance;
    const TypioWlIdentityProviderOps *ops;
    char *niri_socket_path;
};

/* -------------------------------------------------------------------------- */
/* Minimal JSON parser (no external dependency)                               */
/* -------------------------------------------------------------------------- */

static char *json_find_string_value(const char *json, const char *field_name) {
    char pattern[128];
    const char *cursor;
    const char *start;
    char *result;
    size_t out = 0;
    bool escaped = false;

    if (!json || !field_name || !*field_name)
        return nullptr;

    snprintf(pattern, sizeof(pattern), "\"%s\":\"", field_name);
    cursor = strstr(json, pattern);
    if (!cursor)
        return nullptr;

    start = cursor + strlen(pattern);
    result = calloc(strlen(start) + 1, sizeof(char));
    if (!result)
        return nullptr;

    for (const char *p = start; *p; ++p) {
        char ch = *p;

        if (escaped) {
            switch (ch) {
            case 'n':
                result[out++] = '\n';
                break;
            case 'r':
                result[out++] = '\r';
                break;
            case 't':
                result[out++] = '\t';
                break;
            case '\\':
            case '"':
            case '/':
                result[out++] = ch;
                break;
            default:
                result[out++] = ch;
                break;
            }
            escaped = false;
            continue;
        }

        if (ch == '\\') {
            escaped = true;
            continue;
        }

        if (ch == '"') {
            result[out] = '\0';
            return result;
        }

        result[out++] = ch;
    }

    free(result);
    return nullptr;
}

/* -------------------------------------------------------------------------- */
/* Niri compositor IPC                                                        */
/* -------------------------------------------------------------------------- */

static bool niri_socket_request(const char *socket_path,
                                const char *request,
                                char *response,
                                size_t response_size) {
    int fd;
    struct sockaddr_un addr = {};
    size_t request_len;
    size_t written = 0;
    size_t total = 0;

    if (!socket_path || !*socket_path || !request || !response || response_size == 0)
        return false;

    fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        typio_log_warning("Failed to create niri IPC socket: errno=%d",
                  errno);
        return false;
    }

    addr.sun_family = AF_UNIX;
    if (strlen(socket_path) >= sizeof(addr.sun_path)) {
        typio_log_warning("Niri socket path too long: %s",
                  socket_path);
        close(fd);
        return false;
    }
    snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", socket_path);

    if (connect(fd, (const struct sockaddr *)&addr, sizeof(addr)) != 0) {
        typio_log_warning("Failed to connect to niri IPC socket %s: errno=%d",
                  socket_path, errno);
        close(fd);
        return false;
    }

    request_len = strlen(request);
    while (written < request_len) {
        ssize_t rv = write(fd, request + written, request_len - written);
        if (rv <= 0) {
            typio_log_warning("Failed to write niri IPC request: errno=%d",
                      errno);
            close(fd);
            return false;
        }
        written += (size_t)rv;
    }

    shutdown(fd, SHUT_WR);

    while (total + 1 < response_size) {
        ssize_t rv = read(fd, response + total, response_size - total - 1);
        if (rv < 0) {
            if (errno == EINTR)
                continue;
            typio_log_warning("Failed to read niri IPC response: errno=%d",
                      errno);
            close(fd);
            return false;
        }
        if (rv == 0)
            break;
        total += (size_t)rv;
        if (memchr(response, '\n', total) != NULL)
            break;
    }

    response[total] = '\0';
    close(fd);
    return total > 0;
}

bool typio_wl_identity_parse_niri_focused_window(const char *response,
                                                 TypioWlIdentity *identity) {
    char *app_id;

    if (!response || !identity)
        return false;

    memset(identity, 0, sizeof(*identity));
    app_id = json_find_string_value(response, "app_id");
    if (!app_id || !*app_id) {
        typio_log_warning("Niri identity response did not contain app_id: %s",
                  response);
        free(app_id);
        return false;
    }

    identity->provider_name = typio_strdup("niri");
    identity->app_id = app_id;
    identity->stable_key = typio_strjoin3("niri:", app_id, "");
    if (!identity->provider_name || !identity->stable_key) {
        typio_wl_identity_clear(identity);
        return false;
    }

    return true;
}

static bool niri_identity_query(struct TypioWlIdentityProvider *provider,
                                TypioWlIdentity *identity) {
    char response[4096];

    if (!provider || !provider->niri_socket_path || !identity)
        return false;

    if (!niri_socket_request(provider->niri_socket_path,
                             "\"FocusedWindow\"\n",
                             response, sizeof(response))) {
        typio_log_warning("Niri identity query failed for socket %s",
                  provider->niri_socket_path);
        return false;
    }

    return typio_wl_identity_parse_niri_focused_window(response, identity);
}

static const TypioWlIdentityProviderOps niri_identity_provider_ops = {
    .name = "niri",
    .query_current = niri_identity_query,
};

/* -------------------------------------------------------------------------- */
/* Provider lifecycle                                                         */
/* -------------------------------------------------------------------------- */

TypioWlIdentityProvider *typio_wl_identity_provider_new(TypioInstance *instance) {
    TypioWlIdentityProvider *provider;
    const char *niri_socket;

    provider = calloc(1, sizeof(*provider));
    if (!provider)
        return nullptr;

    provider->instance = instance;

    niri_socket = getenv("NIRI_SOCKET");
    if (niri_socket && *niri_socket) {
        provider->ops = &niri_identity_provider_ops;
        provider->niri_socket_path = typio_strdup(niri_socket);
        if (!provider->niri_socket_path) {
            free(provider);
            return nullptr;
        }
        typio_log_info("Focused-app identity provider enabled: niri");
    }

    return provider;
}

void typio_wl_identity_provider_free(TypioWlIdentityProvider *provider) {
    if (!provider)
        return;

    free(provider->niri_socket_path);
    free(provider);
}

const char *typio_wl_identity_provider_name(TypioWlIdentityProvider *provider) {
    return provider && provider->ops ? provider->ops->name : "none";
}

bool typio_wl_identity_provider_query_current(TypioWlIdentityProvider *provider,
                                              TypioWlIdentity *identity) {
    if (!provider || !provider->ops || !provider->ops->query_current || !identity)
        return false;

    memset(identity, 0, sizeof(*identity));
    return provider->ops->query_current(provider, identity);
}

/* -------------------------------------------------------------------------- */
/* Identity refresh / clear                                                   */
/* -------------------------------------------------------------------------- */

void typio_wl_identity_clear(TypioWlIdentity *identity) {
    if (!identity)
        return;

    free(identity->provider_name);
    free(identity->app_id);
    free(identity->stable_key);
    memset(identity, 0, sizeof(*identity));
}

void typio_wl_frontend_clear_identity(TypioWlFrontend *frontend) {
    if (!frontend)
        return;

    typio_wl_identity_clear(&frontend->current_identity);
}

void typio_wl_frontend_refresh_identity(TypioWlFrontend *frontend) {
    TypioWlIdentity identity = {};

    if (!frontend)
        return;

    typio_wl_frontend_clear_identity(frontend);
    if (!frontend->identity_provider)
        return;

    if (!typio_wl_identity_provider_query_current(frontend->identity_provider,
                                                  &identity)) {
        typio_log_debug("No focused-app identity available from provider %s",
                  typio_wl_identity_provider_name(frontend->identity_provider));
        return;
    }

    frontend->current_identity = identity;
    typio_log_debug("Focused app identity: provider=%s app_id=%s",
              frontend->current_identity.provider_name,
              frontend->current_identity.app_id);
}

/* -------------------------------------------------------------------------- */
/* Mode restore (retained in daemon — needs engine keyboard ops)              */
/* -------------------------------------------------------------------------- */

static void identity_restore_mode(TypioWlFrontend *frontend) {
    TypioRegistry *registry;
    char *active_name = nullptr;
    char *engine_name = nullptr;
    char *mode_id = nullptr;
    const TypioEngineMode *current_mode;

    if (!frontend || !frontend->instance || !frontend->session ||
        !frontend->session->ctx || !frontend->current_identity.provider_name ||
        !frontend->current_identity.app_id) {
        return;
    }

    if (!typio_instance_identity_load_mode(frontend->instance,
                                           frontend->current_identity.provider_name,
                                           frontend->current_identity.app_id,
                                           &engine_name,
                                           &mode_id)) {
        typio_free_string(engine_name);
        typio_free_string(mode_id);
        return;
    }

    registry = typio_instance_get_registry(frontend->instance);
    active_name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    if (!active_name || !typio_str_equals(active_name, engine_name)) {
        goto cleanup;
    }

    current_mode = typio_instance_get_last_mode(frontend->instance);
    if (current_mode && current_mode->mode_id &&
        typio_str_equals(current_mode->mode_id, mode_id)) {
        goto cleanup;
    }

    /* The current TypioRegistry C surface does not expose a host-callable
     * `set_mode` on the active engine — engines manage modes internally and
     * report them via `typio_instance_notify_mode`. We can no longer push a
     * stored mode_id back into the engine from here. Persisted mode is still
     * remembered; restoring it will require a libtypio API addition. */
    typio_log_debug("Mode-restore requested (%s) for %s, but registry API "
                    "lacks a set_mode helper — skipping",
                    mode_id, frontend->current_identity.stable_key);

cleanup:
    typio_free_string(active_name);
    typio_free_string(engine_name);
    typio_free_string(mode_id);
}

/* -------------------------------------------------------------------------- */
/* Engine / mode restore & remember — thin wrappers around core API           */
/* -------------------------------------------------------------------------- */

void typio_wl_frontend_restore_identity_engine(TypioWlFrontend *frontend) {
    TypioRegistry *registry;
    char *active_name;
    char *engine_name;

    if (!frontend || !frontend->instance ||
        !frontend->current_identity.provider_name ||
        !frontend->current_identity.app_id ||
        !typio_instance_identity_preferences_enabled(frontend->instance))
        return;

    engine_name = typio_instance_identity_load_engine(
        frontend->instance,
        frontend->current_identity.provider_name,
        frontend->current_identity.app_id);
    if (!engine_name || !*engine_name) {
        typio_free_string(engine_name);
        identity_restore_mode(frontend);
        return;
    }

    registry = typio_instance_get_registry(frontend->instance);
    active_name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    if (active_name && typio_str_equals(active_name, engine_name)) {
        typio_free_string(active_name);
        typio_free_string(engine_name);
        identity_restore_mode(frontend);
        return;
    }

    if (registry && typio_registry_set_active_keyboard(registry, engine_name) == TYPIO_OK) {
        typio_log_info("Restored keyboard engine %s for %s",
                       engine_name,
                       frontend->current_identity.stable_key);
    }

    typio_free_string(active_name);
    typio_free_string(engine_name);
    identity_restore_mode(frontend);
}

void typio_wl_frontend_remember_active_engine(TypioWlFrontend *frontend,
                                              const char *engine_name) {
    if (!frontend || !frontend->instance || !engine_name || !*engine_name ||
        !frontend->current_identity.provider_name ||
        !frontend->current_identity.app_id ||
        !typio_instance_identity_preferences_enabled(frontend->instance)) {
        return;
    }

    typio_instance_identity_store_engine(frontend->instance,
                                         frontend->current_identity.provider_name,
                                         frontend->current_identity.app_id,
                                         engine_name);
    typio_log_info("Remembered keyboard engine %s for %s",
              engine_name,
              frontend->current_identity.stable_key);
}

void typio_wl_frontend_remember_active_mode(TypioWlFrontend *frontend,
                                            const char *engine_name,
                                            const char *mode_id) {
    if (!frontend || !frontend->instance || !engine_name || !*engine_name ||
        !mode_id || !*mode_id ||
        !frontend->current_identity.provider_name ||
        !frontend->current_identity.app_id ||
        !typio_instance_identity_preferences_enabled(frontend->instance)) {
        return;
    }

    typio_instance_identity_store_mode(frontend->instance,
                                       frontend->current_identity.provider_name,
                                       frontend->current_identity.app_id,
                                       engine_name,
                                       mode_id);
    typio_log_info("Remembered keyboard mode %s (%s) for %s",
              mode_id,
              engine_name,
              frontend->current_identity.stable_key);
}
