/**
 * @file frontend_bind.c
 * @brief Wayland protocol binding, unbinding, and registry listeners.
 *
 * Everything in this file is a side-effecting function of the compositor
 * state: connect the display, bind globals from the registry, create the
 * input method + virtual keyboard, and build the text UI panel. The
 * registry/seat/output listeners registered here are the only way the rest
 * of the daemon learns about the compositor's global objects.
 *
 * Bind/unbind are the path used at startup AND on reconnect. They touch
 * nothing that must survive a compositor restart (engine/session state,
 * aux handlers, config watch, resume signal). On a hard failure bind
 * writes frontend->error_msg and returns false WITHOUT tearing down — the
 * caller decides whether to abort (startup) or retry (reconnect).
 * Unbind tolerates any subset of pointers being NULL.
 */

#include "frontend.h"
#include "internal.h"
#include "monotonic.h"
#include "panel.h"
#include "foreign/identity.h"

#include "typio/abi/log.h"
#include "typio/typio.h"

#include <stdlib.h>
#include <string.h>
#include <wayland-client.h>

/* Forward decls for the static listeners (definitions below). */
static void registry_handle_global(void *data, struct wl_registry *registry,
                                   uint32_t name, const char *interface,
                                   uint32_t version);
static void registry_handle_global_remove(void *data, struct wl_registry *registry,
                                          uint32_t name);
static void output_handle_geometry(void *data, struct wl_output *wl_output,
                                   int32_t x, int32_t y,
                                   int32_t physical_width,
                                   int32_t physical_height,
                                   int32_t subpixel, const char *make,
                                   const char *model, int32_t transform);
static void output_handle_mode(void *data, struct wl_output *wl_output,
                               uint32_t flags, int32_t width,
                               int32_t height, int32_t refresh);
static void output_handle_done(void *data, struct wl_output *wl_output);
static void output_handle_scale(void *data, struct wl_output *wl_output,
                                int32_t factor);
static void seat_handle_capabilities(void *data, struct wl_seat *seat,
                                     uint32_t capabilities);
static void seat_handle_name(void *data, struct wl_seat *seat, const char *name);

static const struct wl_registry_listener registry_listener = {
    .global = registry_handle_global,
    .global_remove = registry_handle_global_remove,
};

static const struct wl_seat_listener seat_listener = {
    .capabilities = seat_handle_capabilities,
    .name = seat_handle_name,
};

static const struct wl_output_listener output_listener = {
    .geometry = output_handle_geometry,
    .mode = output_handle_mode,
    .done = output_handle_done,
    .scale = output_handle_scale,
};

/* Bind (or rebind) every Wayland-derived object. See file header. */
bool typio_wl_frontend_wayland_bind(TypioWlFrontend *frontend) {
    frontend->display = wl_display_connect(frontend->display_name);
    if (!frontend->display) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "Failed to connect to Wayland display: %s",
                 frontend->display_name ? frontend->display_name : "(default)");
        typio_log_error("%s", frontend->error_msg);
        return false;
    }
    typio_log_info("Connected to Wayland display");

    frontend->registry = wl_display_get_registry(frontend->display);
    if (!frontend->registry) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "Failed to get Wayland registry");
        typio_log_error("%s", frontend->error_msg);
        return false;
    }
    wl_registry_add_listener(frontend->registry, &registry_listener, frontend);

    if (wl_display_roundtrip(frontend->display) < 0) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "Wayland roundtrip failed");
        typio_log_error("%s", frontend->error_msg);
        return false;
    }

    if (!frontend->im_manager) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "Session does not provide the required Wayland input-method/text-input protocol stack");
        typio_log_error("Compositor does not support zwp_input_method_manager_v2");
        return false;
    }
    if (!frontend->seat) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "No seat available");
        typio_log_error("No seat available");
        return false;
    }
    if (!frontend->compositor || !frontend->shm) {
        typio_log_warning("Compositor missing wl_compositor or wl_shm; panel candidates disabled");
    }

    frontend->input_method = zwp_input_method_manager_v2_get_input_method(
        frontend->im_manager, frontend->seat);
    if (!frontend->input_method) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg),
                 "Failed to create input method");
        typio_log_error("Failed to create input method");
        return false;
    }
    typio_wl_input_method_setup(frontend);

    if (frontend->vk && frontend->vk->manager && frontend->seat) {
        frontend->vk->vk =
            zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(
                frontend->vk->manager, frontend->seat);
        if (frontend->vk->vk) {
            typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_NEEDS_KEYMAP,
                                  "virtual keyboard object created");
            typio_log_info("Virtual keyboard created for key forwarding");
        } else {
            typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_BROKEN,
                                  "create_virtual_keyboard returned null");
            typio_log_warning("Failed to create virtual keyboard; unhandled keys will be lost");
        }
    } else {
        typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_ABSENT,
                              "virtual keyboard manager unavailable");
        typio_log_warning("No virtual keyboard manager; unhandled keys will be lost");
    }

    frontend->panel = typio_panel_create(frontend);
    if (frontend->panel) {
        if (typio_panel_is_available(frontend->panel)) {
            typio_log_info("Candidate panel surface ready");
        } else if (!frontend->compositor || !frontend->shm) {
            typio_log_warning("Panel disabled: compositor=%p, shm=%p",
                              (void *)frontend->compositor, (void *)frontend->shm);
        } else {
            typio_log_warning("Failed to initialize candidate panel surface; keeping candidate state inline");
        }
    } else {
        typio_log_warning("Failed to initialize text UI backend");
    }

    /* The identity provider tracks the focused toplevel's app_id via the
     * ext_foreign_toplevel_list_v1 staging protocol. Bind it once the
     * display is fully connected and the registry has been enumerated. */
    if (frontend->identity_provider) {
        if (typio_wl_identity_provider_bind(frontend->identity_provider,
                                            frontend->display) != 0) {
            typio_log_warning("Failed to bind focused-app identity provider");
        }
    }

    return true;
}

/* Destroy every Wayland-derived object and disconnect the display, nulling
 * each pointer so a subsequent bind starts clean. Leaves engine/session,
 * aux handlers, config watch, and the resume signal untouched. Safe to call
 * with any subset already NULL. */
void typio_wl_frontend_wayland_unbind(TypioWlFrontend *frontend) {
    if (frontend->identity_provider) {
        typio_wl_identity_provider_unbind(frontend->identity_provider);
    }
    if (frontend->keyboard) {
        typio_wl_keyboard_destroy(frontend->keyboard);
        frontend->keyboard = nullptr;
    }
    if (frontend->panel) {
        typio_panel_destroy(frontend->panel);
        frontend->panel = nullptr;
    }
    if (frontend->vk && frontend->vk->vk) {
        typio_wl_vk_set_state(frontend, TYPIO_WL_VK_STATE_ABSENT,
                              "wayland unbind");
        zwp_virtual_keyboard_v1_destroy(frontend->vk->vk);
        frontend->vk->vk = nullptr;
    }
    if (frontend->vk && frontend->vk->manager) {
        zwp_virtual_keyboard_manager_v1_destroy(frontend->vk->manager);
        frontend->vk->manager = nullptr;
    }
    if (frontend->input_method) {
        zwp_input_method_v2_destroy(frontend->input_method);
        frontend->input_method = nullptr;
    }
    if (frontend->im_manager) {
        zwp_input_method_manager_v2_destroy(frontend->im_manager);
        frontend->im_manager = nullptr;
    }
    if (frontend->seat) {
        wl_seat_destroy(frontend->seat);
        frontend->seat = nullptr;
    }
    if (frontend->shm) {
        wl_shm_destroy(frontend->shm);
        frontend->shm = nullptr;
    }
    if (frontend->viewporter) {
        wp_viewporter_destroy(frontend->viewporter);
        frontend->viewporter = nullptr;
    }
    if (frontend->fractional_scale_manager) {
        wp_fractional_scale_manager_v1_destroy(frontend->fractional_scale_manager);
        frontend->fractional_scale_manager = nullptr;
    }
    if (frontend->compositor) {
        wl_compositor_destroy(frontend->compositor);
        frontend->compositor = nullptr;
    }
    if (frontend->registry) {
        wl_registry_destroy(frontend->registry);
        frontend->registry = nullptr;
    }
    while (frontend->outputs) {
        TypioWlOutput *output = frontend->outputs;
        frontend->outputs = output->next;
        if (output->output) {
            wl_output_destroy(output->output);
        }
        free(output);
    }
    if (frontend->display) {
        wl_display_disconnect(frontend->display);
        frontend->display = nullptr;
    }
}

/* ── Registry listener implementations ─────────────────────────────────── */

static void registry_handle_global(void *data, struct wl_registry *registry,
                                   uint32_t name, const char *interface,
                                   [[maybe_unused]] uint32_t version) {
    TypioWlFrontend *frontend = data;

    if (strcmp(interface, zwp_input_method_manager_v2_interface.name) == 0) {
        frontend->im_manager = wl_registry_bind(registry, name,
                                                &zwp_input_method_manager_v2_interface, 1);
        typio_log_info("Bound zwp_input_method_manager_v2");
    } else if (strcmp(interface, wl_compositor_interface.name) == 0) {
        /* v6 enables wl_surface.preferred_buffer_scale (used as an
         * integer fallback when wp_fractional_scale_v1 is absent). */
        uint32_t want = version >= 6 ? 6u : version;
        frontend->compositor = wl_registry_bind(registry, name,
                                                &wl_compositor_interface, want);
        typio_log_info("Bound wl_compositor v%u", want);
    } else if (strcmp(interface, wl_shm_interface.name) == 0) {
        frontend->shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
        typio_log_info("Bound wl_shm");
    } else if (strcmp(interface, wp_fractional_scale_manager_v1_interface.name) == 0) {
        frontend->fractional_scale_manager = wl_registry_bind(
            registry, name, &wp_fractional_scale_manager_v1_interface, 1);
        typio_log_info("Bound wp_fractional_scale_manager_v1");
    } else if (strcmp(interface, wp_viewporter_interface.name) == 0) {
        frontend->viewporter = wl_registry_bind(
            registry, name, &wp_viewporter_interface, 1);
        typio_log_info("Bound wp_viewporter");
    } else if (strcmp(interface, zwp_virtual_keyboard_manager_v1_interface.name) == 0) {
        if (frontend->vk) {
            frontend->vk->manager = wl_registry_bind(registry, name,
                                                     &zwp_virtual_keyboard_manager_v1_interface, 1);
        }
        typio_log_info("Bound zwp_virtual_keyboard_manager_v1");
    } else if (strcmp(interface, wl_seat_interface.name) == 0) {
        /* Only bind first seat */
        if (!frontend->seat) {
            frontend->seat = wl_registry_bind(registry, name,
                                              &wl_seat_interface, 1);
            wl_seat_add_listener(frontend->seat, &seat_listener, frontend);
            typio_log_info("Bound wl_seat");
        }
    } else if (strcmp(interface, wl_output_interface.name) == 0) {
        TypioWlOutput *output = calloc(1, sizeof(*output));
        if (!output) {
            typio_log_warning("Failed to allocate wl_output tracking");
            return;
        }

        output->name = name;
        output->frontend = frontend;
        output->scale = 1;
        output->output = wl_registry_bind(registry, name, &wl_output_interface,
                                          version >= 2 ? 2u : version);
        if (!output->output) {
            free(output);
            typio_log_warning("Failed to bind wl_output");
            return;
        }

        wl_output_add_listener(output->output, &output_listener, output);
        output->next = frontend->outputs;
        frontend->outputs = output;
        typio_log_info("Bound wl_output %u", name);
    }
}

static void registry_handle_global_remove(void *data,
                                          [[maybe_unused]] struct wl_registry *registry,
                                          uint32_t name) {
    TypioWlFrontend *frontend = data;
    TypioWlOutput **link;

    if (!frontend) {
        return;
    }

    link = &frontend->outputs;
    while (*link) {
        TypioWlOutput *output = *link;
        if (output->name == name) {
            *link = output->next;
            if (output->output) {
                typio_panel_handle_output_change(frontend->panel, output->output);
                wl_output_destroy(output->output);
            }
            free(output);
            return;
        }
        link = &output->next;
    }
}

/* ── Seat listener implementations ─────────────────────────────────────── */

static void seat_handle_capabilities([[maybe_unused]] void *data,
                                     [[maybe_unused]] struct wl_seat *seat,
                                     uint32_t capabilities) {
    typio_log_debug("Seat capabilities: 0x%x", capabilities);
}

static void seat_handle_name([[maybe_unused]] void *data,
                             [[maybe_unused]] struct wl_seat *seat,
                             const char *name) {
    typio_log_debug("Seat name: %s", name);
}

/* ── Output listener implementations ───────────────────────────────────── */

static void output_handle_geometry([[maybe_unused]] void *data,
                                   [[maybe_unused]] struct wl_output *wl_output,
                                   [[maybe_unused]] int32_t x,
                                   [[maybe_unused]] int32_t y,
                                   [[maybe_unused]] int32_t physical_width,
                                   [[maybe_unused]] int32_t physical_height,
                                   [[maybe_unused]] int32_t subpixel,
                                   [[maybe_unused]] const char *make,
                                   [[maybe_unused]] const char *model,
                                   [[maybe_unused]] int32_t transform) {
}

static void output_handle_mode([[maybe_unused]] void *data,
                               [[maybe_unused]] struct wl_output *wl_output,
                               [[maybe_unused]] uint32_t flags,
                               [[maybe_unused]] int32_t width,
                               [[maybe_unused]] int32_t height,
                               [[maybe_unused]] int32_t refresh) {
}

static void output_handle_done(void *data, [[maybe_unused]] struct wl_output *wl_output) {
    TypioWlOutput *output = data;

    if (!output) {
        return;
    }

    typio_log_debug("wl_output %u scale=%d", output->name, output->scale);
}

static void output_handle_scale(void *data, [[maybe_unused]] struct wl_output *wl_output,
                                int32_t factor) {
    TypioWlOutput *output = data;

    if (!output) {
        return;
    }

    output->scale = factor > 0 ? factor : 1;
    typio_panel_handle_output_change(output->frontend->panel, output->output);
}
