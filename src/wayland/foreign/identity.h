/**
 * @file identity.h
 * @brief Focused-application identity provider backed by ext_foreign_toplevel_list_v1
 *
 * The previous implementation queried the niri compositor over its private
 * IPC socket. This module is the generic replacement: it binds the
 * ext_foreign_toplevel_list_v1 staging protocol, tracks every mapped
 * toplevel's app_id / title / identifier, and exposes the most-recently
 * active toplevel as the "current" identity for per-app engine and mode
 * restoration.
 *
 * The protocol does not carry an explicit focus event, so the most
 * recently created toplevel is used as a best-effort focus proxy. The
 * same app_id is what we need to look up per-app preferences, so the
 * imprecision only matters when two distinct apps both have a toplevel
 * in the list at the moment of a state-change event.
 */

#ifndef TYPIO_WL_FOREIGN_IDENTITY_H
#define TYPIO_WL_FOREIGN_IDENTITY_H

#include "frontend.h"

#ifdef __cplusplus
extern "C" {
#endif

struct wl_display;

typedef struct TypioWlIdentityProvider TypioWlIdentityProvider;

typedef struct TypioWlIdentity {
    char *provider_name;
    char *app_id;
    char *stable_key;
} TypioWlIdentity;

TypioWlIdentityProvider *typio_wl_identity_provider_new(TypioInstance *instance);
void typio_wl_identity_provider_free(TypioWlIdentityProvider *provider);

/* Bind the provider to the Wayland display. The provider must already have
 * been created with typio_wl_identity_provider_new(). Idempotent; calling
 * twice replaces the previous binding. Returns 0 on success or -1 if the
 * ext_foreign_toplevel_list_v1 global was not advertised by the compositor. */
int typio_wl_identity_provider_bind(struct TypioWlIdentityProvider *provider,
                                    struct wl_display *display);
void typio_wl_identity_provider_unbind(struct TypioWlIdentityProvider *provider);

const char *typio_wl_identity_provider_name(TypioWlIdentityProvider *provider);
bool typio_wl_identity_provider_query_current(TypioWlIdentityProvider *provider,
                                              TypioWlIdentity *identity);

void typio_wl_identity_clear(TypioWlIdentity *identity);

void typio_wl_frontend_refresh_identity(TypioWlFrontend *frontend);
void typio_wl_frontend_clear_identity(TypioWlFrontend *frontend);
void typio_wl_frontend_restore_identity_engine(TypioWlFrontend *frontend);
void typio_wl_frontend_remember_active_engine(TypioWlFrontend *frontend,
                                              const char *engine_name);
void typio_wl_frontend_remember_active_mode(TypioWlFrontend *frontend,
                                            const char *engine_name,
                                            const char *mode_id);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FOREIGN_IDENTITY_H */
