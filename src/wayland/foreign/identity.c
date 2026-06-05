/**
 * @file identity.c
 * @brief ext_foreign_toplevel_list_v1-backed focused-application identity
 *
 * Replaces the previous niri-IPC adapter. The provider binds the staging
 * ext_foreign_toplevel_list_v1 protocol and tracks every toplevel the
 * compositor advertises. The "current" identity is taken from the
 * most-recently-activated toplevel; the per-app preference store only
 * needs the app_id, so even when the focus proxy is slightly stale the
 * per-app engine and mode restoration still hits the right entry.
 *
 * Mode/engine restore/remember still live here because they need direct
 * access to the active keyboard engine in the registry and to the input
 * context to push the remembered mode back in.
 */

#include "identity.h"

#include "internal.h"
#include "typio/abi/engine.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#include "ext-foreign-toplevel-list-v1-client-protocol.h"

#include <errno.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

#define TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME "ext_foreign_toplevel_list_v1"

typedef struct TypioWlForeignToplevel {
    struct ext_foreign_toplevel_handle_v1 *handle;
    struct wl_list link;

    char *app_id;
    char *title;
    char *identifier;

    /* Monotonically increasing per-provider counter; the toplevel with
     * the highest serial is the most recent one we saw. */
    uint32_t serial;
} TypioWlForeignToplevel;

struct TypioWlIdentityProvider {
    TypioInstance *instance;

    /* Wayland binding state. NULL between bind and unbind. */
    struct wl_display *display;
    struct wl_registry *registry;
    struct ext_foreign_toplevel_list_v1 *list;
    struct wl_list toplevels;  /* TypioWlForeignToplevel.link */

    /* The toplevel we currently treat as "focused" — i.e. the one with
     * the highest serial. NULL when the compositor has not yet advertised
     * any toplevel, or after the binding has been torn down. */
    TypioWlForeignToplevel *current;

    uint32_t next_serial;
};

/* -------------------------------------------------------------------------- */
/* Toplevel lifecycle                                                          */
/* -------------------------------------------------------------------------- */

static void toplevel_free(TypioWlForeignToplevel *toplevel) {
    if (!toplevel) {
        return;
    }
    if (toplevel->handle) {
        ext_foreign_toplevel_handle_v1_destroy(toplevel->handle);
    }
    wl_list_remove(&toplevel->link);
    free(toplevel->app_id);
    free(toplevel->title);
    free(toplevel->identifier);
    free(toplevel);
}

static void toplevel_update_current(TypioWlIdentityProvider *provider) {
    TypioWlForeignToplevel *best = NULL;
    TypioWlForeignToplevel *iter;

    wl_list_for_each(iter, &provider->toplevels, link) {
        if (!best || iter->serial > best->serial) {
            best = iter;
        }
    }
    provider->current = best;
}

/* -------------------------------------------------------------------------- */
/* ext_foreign_toplevel_handle_v1 listeners                                    */
/* -------------------------------------------------------------------------- */

static void handle_handle_title(void *data,
                                struct ext_foreign_toplevel_handle_v1 *handle,
                                const char *title) {
    TypioWlForeignToplevel *toplevel = data;
    char *copy = title ? typio_strdup(title) : NULL;
    if (!copy && title) {
        return;
    }
    free(toplevel->title);
    toplevel->title = copy;
    (void)handle;
}

static void handle_handle_app_id(void *data,
                                 struct ext_foreign_toplevel_handle_v1 *handle,
                                 const char *app_id) {
    TypioWlForeignToplevel *toplevel = data;
    char *copy = app_id ? typio_strdup(app_id) : NULL;
    if (!copy && app_id) {
        return;
    }
    free(toplevel->app_id);
    toplevel->app_id = copy;
    (void)handle;
}

static void handle_handle_identifier(void *data,
                                     struct ext_foreign_toplevel_handle_v1 *handle,
                                     const char *identifier) {
    TypioWlForeignToplevel *toplevel = data;
    char *copy = identifier ? typio_strdup(identifier) : NULL;
    if (!copy && identifier) {
        return;
    }
    free(toplevel->identifier);
    toplevel->identifier = copy;
    (void)handle;
}

static void handle_handle_done(void *data,
                               struct ext_foreign_toplevel_handle_v1 *handle) {
    TypioWlIdentityProvider *provider = data;
    TypioWlForeignToplevel *toplevel;

    wl_list_for_each(toplevel, &provider->toplevels, link) {
        if (toplevel->handle == handle) {
            /* Bump the serial so a freshly-completed toplevel is treated
             * as the most recent one. */
            toplevel->serial = provider->next_serial++;
            break;
        }
    }
    toplevel_update_current(provider);
    (void)handle;
}

static void handle_handle_closed(void *data,
                                 struct ext_foreign_toplevel_handle_v1 *handle) {
    TypioWlIdentityProvider *provider = data;
    TypioWlForeignToplevel *toplevel, *tmp;

    wl_list_for_each_safe(toplevel, tmp, &provider->toplevels, link) {
        if (toplevel->handle == handle) {
            toplevel_free(toplevel);
            break;
        }
    }
    toplevel_update_current(provider);
}

static const struct ext_foreign_toplevel_handle_v1_listener toplevel_listener = {
    .title = handle_handle_title,
    .app_id = handle_handle_app_id,
    .identifier = handle_handle_identifier,
    .done = handle_handle_done,
    .closed = handle_handle_closed,
};

/* -------------------------------------------------------------------------- */
/* ext_foreign_toplevel_list_v1 listeners                                      */
/* -------------------------------------------------------------------------- */

static void handle_list_toplevel(void *data,
                                 struct ext_foreign_toplevel_list_v1 *list,
                                 struct ext_foreign_toplevel_handle_v1 *handle) {
    TypioWlIdentityProvider *provider = data;
    TypioWlForeignToplevel *toplevel = calloc(1, sizeof(*toplevel));
    if (!toplevel) {
        ext_foreign_toplevel_handle_v1_destroy(handle);
        return;
    }
    toplevel->handle = handle;
    toplevel->serial = provider->next_serial++;
    wl_list_insert(&provider->toplevels, &toplevel->link);
    ext_foreign_toplevel_handle_v1_add_listener(handle,
                                                 &toplevel_listener,
                                                 provider);
    toplevel_update_current(provider);
    (void)list;
}

static void handle_list_finished(void *data,
                                 struct ext_foreign_toplevel_list_v1 *list) {
    TypioWlIdentityProvider *provider = data;
    TypioWlForeignToplevel *toplevel, *tmp;

    /* The compositor is done sending us toplevel events. We can't do much
     * with the list itself, but the existing handles stay valid until we
     * destroy them. Clear the list pointer so future unbind is a no-op
     * for it. */
    wl_list_for_each_safe(toplevel, tmp, &provider->toplevels, link) {
        toplevel_free(toplevel);
    }
    provider->current = NULL;
    provider->list = NULL;
    (void)list;
}

static const struct ext_foreign_toplevel_list_v1_listener list_listener = {
    .toplevel = handle_list_toplevel,
    .finished = handle_list_finished,
};

/* -------------------------------------------------------------------------- */
/* wl_registry listener                                                        */
/* -------------------------------------------------------------------------- */

static void registry_handle_global(void *data,
                                   struct wl_registry *registry,
                                   uint32_t name,
                                   const char *interface,
                                   uint32_t version) {
    TypioWlIdentityProvider *provider = data;

    if (strcmp(interface, ext_foreign_toplevel_list_v1_interface.name) == 0) {
        provider->list = wl_registry_bind(registry,
                                          name,
                                          &ext_foreign_toplevel_list_v1_interface,
                                          1);
        if (provider->list) {
            ext_foreign_toplevel_list_v1_add_listener(provider->list,
                                                     &list_listener,
                                                     provider);
        }
    }
}

static void registry_handle_global_remove(void *data,
                                          struct wl_registry *registry,
                                          uint32_t name) {
    (void)data;
    (void)registry;
    (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_handle_global,
    .global_remove = registry_handle_global_remove,
};

/* -------------------------------------------------------------------------- */
/* Provider lifecycle                                                          */
/* -------------------------------------------------------------------------- */

TypioWlIdentityProvider *typio_wl_identity_provider_new(TypioInstance *instance) {
    TypioWlIdentityProvider *provider = calloc(1, sizeof(*provider));
    if (!provider) {
        return NULL;
    }
    provider->instance = instance;
    wl_list_init(&provider->toplevels);
    provider->next_serial = 1;
    return provider;
}

void typio_wl_identity_provider_free(TypioWlIdentityProvider *provider) {
    if (!provider) {
        return;
    }
    typio_wl_identity_provider_unbind(provider);
    free(provider);
}

int typio_wl_identity_provider_bind(TypioWlIdentityProvider *provider,
                                    struct wl_display *display) {
    TypioWlForeignToplevel *toplevel, *tmp;

    if (!provider || !display) {
        return -1;
    }

    if (provider->display == display && provider->registry) {
        return 0;
    }

    /* Drop any previous binding state. */
    wl_list_for_each_safe(toplevel, tmp, &provider->toplevels, link) {
        toplevel_free(toplevel);
    }
    provider->current = NULL;
    provider->list = NULL;
    if (provider->registry) {
        wl_registry_destroy(provider->registry);
        provider->registry = NULL;
    }

    provider->display = display;
    provider->registry = wl_display_get_registry(display);
    if (!provider->registry) {
        provider->display = NULL;
        return -1;
    }
    wl_registry_add_listener(provider->registry, &registry_listener, provider);
    /* Trigger the initial global enumeration. The new toplevel_list
     * binding will come back asynchronously through the registry listener
     * and any pre-existing toplevels will be reported through the
     * list_listener's toplevel event after a wl_display_roundtrip. */
    wl_display_roundtrip(display);
    if (!provider->list) {
        /* The compositor doesn't advertise ext_foreign_toplevel_list_v1.
         * We still keep the registry listener around in case it adds the
         * global later (e.g. on hot-reload of the protocol), but for now
         * query_current will report no identity. */
        typio_log_info("Focused-app identity: ext_foreign_toplevel_list_v1 not "
                      "advertised by compositor");
    } else {
        typio_log_info("Focused-app identity provider enabled: %s",
                      TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME);
    }
    return 0;
}

void typio_wl_identity_provider_unbind(TypioWlIdentityProvider *provider) {
    TypioWlForeignToplevel *toplevel, *tmp;

    if (!provider) {
        return;
    }

    wl_list_for_each_safe(toplevel, tmp, &provider->toplevels, link) {
        toplevel_free(toplevel);
    }
    provider->current = NULL;

    if (provider->list) {
        ext_foreign_toplevel_list_v1_destroy(provider->list);
        provider->list = NULL;
    }
    if (provider->registry) {
        wl_registry_destroy(provider->registry);
        provider->registry = NULL;
    }
    provider->display = NULL;
}

const char *typio_wl_identity_provider_name(TypioWlIdentityProvider *provider) {
    (void)provider;
    return TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME;
}

bool typio_wl_identity_provider_query_current(TypioWlIdentityProvider *provider,
                                              TypioWlIdentity *identity) {
    if (!provider || !identity) {
        return false;
    }

    memset(identity, 0, sizeof(*identity));
    if (!provider->current || !provider->current->app_id ||
        !provider->current->app_id[0]) {
        return false;
    }

    identity->provider_name = typio_strdup(TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME);
    identity->app_id = typio_strdup(provider->current->app_id);
    /* Use the protocol-stable identifier as the persistence key. When the
     * compositor does not emit one (older wlroots), fall back to app_id so
     * the user still gets a stable bucket per app. */
    if (provider->current->identifier && provider->current->identifier[0]) {
        identity->stable_key = typio_strjoin3(
            TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME ":",
            provider->current->identifier, "");
    } else {
        identity->stable_key = typio_strjoin3(
            TYPIO_WL_FOREIGN_IDENTITY_PROVIDER_NAME ":",
            provider->current->app_id, "");
    }

    if (!identity->provider_name || !identity->app_id || !identity->stable_key) {
        typio_wl_identity_clear(identity);
        return false;
    }
    return true;
}

/* -------------------------------------------------------------------------- */
/* Identity clear                                                              */
/* -------------------------------------------------------------------------- */

void typio_wl_identity_clear(TypioWlIdentity *identity) {
    if (!identity) {
        return;
    }
    free(identity->provider_name);
    free(identity->app_id);
    free(identity->stable_key);
    memset(identity, 0, sizeof(*identity));
}

/* -------------------------------------------------------------------------- */
/* Frontend identity refresh / clear                                           */
/* -------------------------------------------------------------------------- */

void typio_wl_frontend_clear_identity(TypioWlFrontend *frontend) {
    if (!frontend) {
        return;
    }
    typio_wl_identity_clear(&frontend->current_identity);
}

void typio_wl_frontend_refresh_identity(TypioWlFrontend *frontend) {
    TypioWlIdentity identity = {};

    if (!frontend) {
        return;
    }

    typio_wl_frontend_clear_identity(frontend);
    if (!frontend->identity_provider) {
        return;
    }

    if (!typio_wl_identity_provider_query_current(frontend->identity_provider,
                                                  &identity)) {
        typio_log_debug("No focused-app identity available from provider %s",
                        typio_wl_identity_provider_name(
                            frontend->identity_provider));
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
    char *active_name = NULL;
    char *engine_name = NULL;
    char *mode_id = NULL;
    const TypioKeyboardEngineMode *current_mode;

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
    active_name = registry ? typio_registry_get_active_keyboard(registry) : NULL;
    if (!active_name || !typio_str_equals(active_name, engine_name)) {
        goto cleanup;
    }

    current_mode = typio_instance_get_last_keyboard_mode(frontend->instance);
    if (current_mode && current_mode->id &&
        typio_str_equals(current_mode->id, mode_id)) {
        goto cleanup;
    }

    /* Push the remembered profile back into the active engine. The engine's
     * own "schema"/option notification then drives the status reflection. */
    if (typio_input_context_set_active_mode(frontend->session->ctx, mode_id) == TYPIO_OK) {
        typio_log_debug("Restored mode '%s' for %s",
                        mode_id, frontend->current_identity.stable_key);
    } else {
        typio_log_debug("Mode-restore '%s' for %s not applied "
                        "(engine has no set_mode or rejected it)",
                        mode_id, frontend->current_identity.stable_key);
    }

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
    active_name = registry ? typio_registry_get_active_keyboard(registry) : NULL;
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
