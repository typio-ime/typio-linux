/**
 * @file bus.c
 * @brief D-Bus connection and watcher lifecycle for the system tray
 */

#include "typio_build_config.h"
#include "tray_internal.h"
#include "state/controller.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#define TYPIO_TRAY_BUS_MAX_DISPATCH_PER_TICK 16

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

#ifdef HAVE_LIBDBUS
#include <systemd/sd-bus.h>
#endif

static const char *typio_tray_default_icon_theme_path(void) {
    static char install_theme_path[512];
    static char source_theme_path[512];

    if (snprintf(install_theme_path, sizeof(install_theme_path),
                 "%s/hicolor", TYPIO_INSTALL_ICON_DIR) > 0 &&
        access(install_theme_path, R_OK) == 0) {
        return install_theme_path;
    }

    if (access(TYPIO_INSTALL_ICON_DIR, R_OK) == 0) {
        return TYPIO_INSTALL_ICON_DIR;
    }

    if (snprintf(source_theme_path, sizeof(source_theme_path),
                 "%s/hicolor", TYPIO_SOURCE_ICON_DIR) > 0 &&
        access(source_theme_path, R_OK) == 0) {
        return source_theme_path;
    }

    if (access(TYPIO_SOURCE_ICON_DIR, R_OK) == 0) {
        return TYPIO_SOURCE_ICON_DIR;
    }

    return "";
}

#ifdef HAVE_LIBDBUS

/* Forward declarations of the per-(path, interface) sd-bus method
 * handlers. Each returns 0 on success (sending a reply on the message),
 * negative sd-bus error on failure. The first void* argument is the
 * TypioTray* userdata registered with sd_bus_add_object_vtable().
 * Definitions live in sni.c; declared in tray_internal.h. */

static int watcher_owner_changed(sd_bus_message *m, void *userdata,
                                 sd_bus_error *ret_error);

/* Vtable for org.freedesktop.DBus.Properties on /StatusNotifierItem.
 * Note: GetAll needs a defined output signature 'a{sv}', so the
 * 'Get' and 'GetAll' methods get separate vtable rows. */
static const sd_bus_vtable sni_properties_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("Get", "ss", "v", typio_tray_sni_properties_get, 0),
    SD_BUS_METHOD("GetAll", "s", "a{sv}", typio_tray_sni_properties_getall, 0),
    SD_BUS_VTABLE_END,
};

static const sd_bus_vtable sni_item_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("ContextMenu", "ii", NULL, typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("Activate", "ii", NULL, typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("SecondaryActivate", "ii", NULL, typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("Scroll", "is", NULL, typio_tray_sni_method_call, 0),
    SD_BUS_VTABLE_END,
};

static const sd_bus_vtable menu_properties_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("Get", "ss", "v", typio_tray_menu_properties_get, 0),
    SD_BUS_METHOD("GetAll", "s", "a{sv}", typio_tray_menu_properties_getall, 0),
    SD_BUS_VTABLE_END,
};

static const sd_bus_vtable menu_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("GetLayout", "iias", "u(ia{sv}av)", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("Event", "isvu", NULL, typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("GetProperty", "is", "v", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("GetGroupProperties", "ias", "a(ia{sv})", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("AboutToShow", "i", "b", typio_tray_menu_method_call, 0),
    SD_BUS_VTABLE_END,
};

/* The introspect handler is interface-agnostic but the XML it emits
 * depends on the path. The userdata passed at registration time is
 * the TypioTray*; the handler reads sd_bus_message_get_path() to pick
 * the right XML. */
static const sd_bus_vtable introspect_vtable[] = {
    SD_BUS_VTABLE_START(SD_BUS_VTABLE_UNPRIVILEGED),
    SD_BUS_METHOD("Introspect", NULL, "s", typio_tray_introspect, 0),
    SD_BUS_VTABLE_END,
};

static int watcher_owner_changed(sd_bus_message *m, void *userdata,
                                 sd_bus_error *ret_error) {
    TypioTray *tray = userdata;
    const char *name = nullptr;
    const char *old_owner = nullptr;
    const char *new_owner = nullptr;
    int r;
    (void)ret_error;

    r = sd_bus_message_read(m, "sss", &name, &old_owner, &new_owner);
    if (r < 0 || !name || strcmp(name, SNI_WATCHER_SERVICE) != 0) {
        return 0;
    }

    if (new_owner && new_owner[0] != '\0') {
        typio_log_info("StatusNotifierWatcher appeared as %s", new_owner);
        if (!tray->registered) {
            typio_tray_sni_register(tray);
        }
    } else {
        typio_log_info("StatusNotifierWatcher disappeared");
        tray->registered = false;
    }
    return 0;
}

#endif /* HAVE_LIBDBUS */


TypioTray *typio_tray_new(TypioInstance *instance, const TypioTrayConfig *config) {
    pid_t pid;
    int ret;
    static int instance_counter = 0;
    TypioTray *tray;
    int service_name_len;
#ifdef HAVE_LIBDBUS
    int r;
#endif

    if (!instance) {
        return nullptr;
    }

    tray = calloc(1, sizeof(TypioTray));
    if (!tray) {
        return nullptr;
    }

    tray->instance = instance;

    if (config) {
        if (config->icon_name) {
            tray->icon_name = typio_strdup(config->icon_name);
        }
        if (config->tooltip) {
            tray->tooltip_title = typio_strdup(config->tooltip);
        }
        tray->menu_callback = config->menu_callback;
        tray->user_data = config->user_data;
    }

    if (!tray->icon_name) {
        tray->icon_name = typio_strdup("typio-keyboard-off-symbolic");
    }
    tray->icon_theme_path = typio_strdup(typio_tray_default_icon_theme_path());
    if (!tray->tooltip_title) {
        tray->tooltip_title = typio_strdup("Typio Input Method");
    }
    tray->title = typio_strdup("Typio");

#ifdef HAVE_LIBDBUS
    r = sd_bus_open_user(&tray->bus);
    if (r < 0) {
        typio_log_error("Failed to connect to session D-Bus: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }

    /* Watch the bus for org.freedesktop.DBus.NameOwnerChanged on
     * org.kde.StatusNotifierWatcher — when the watcher appears or
     * disappears, (re)register or unmark accordingly. */
    r = sd_bus_match_signal(tray->bus,
                            &tray->watcher_match_slot,
                            "org.freedesktop.DBus",
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "NameOwnerChanged",
                            watcher_owner_changed,
                            tray);
    if (r < 0) {
        typio_log_warning("Failed to watch StatusNotifierWatcher ownership: %s",
                          strerror(-r));
    }
    sd_bus_flush(tray->bus);

    /* Register the per-(path, interface) vtables. The slot returned
     * for the first registration is anchored on tray->vtable_slot; the
     * subsequent sd_bus_add_object_vtable calls return NULL slots
     * because the slot is owned by the bus. We unref vtable_slot on
     * teardown. */
    r = sd_bus_add_object_vtable(tray->bus,
                                 &tray->vtable_slot,
                                 SNI_ITEM_PATH,
                                 SNI_ITEM_INTERFACE,
                                 sni_item_vtable,
                                 tray);
    if (r < 0) {
        typio_log_error("Failed to register SNI object path: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
    r = sd_bus_add_object_vtable(tray->bus, nullptr, SNI_ITEM_PATH,
                                 DBUS_PROPERTIES_INTERFACE,
                                 sni_properties_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register SNI Properties: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
    r = sd_bus_add_object_vtable(tray->bus, nullptr, SNI_ITEM_PATH,
                                 DBUS_INTROSPECTABLE_INTERFACE,
                                 introspect_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register SNI Introspect: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
    r = sd_bus_add_object_vtable(tray->bus, nullptr, DBUSMENU_PATH,
                                 DBUSMENU_INTERFACE,
                                 menu_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register menu object path: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
    r = sd_bus_add_object_vtable(tray->bus, nullptr, DBUSMENU_PATH,
                                 DBUS_PROPERTIES_INTERFACE,
                                 menu_properties_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register menu Properties: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
    r = sd_bus_add_object_vtable(tray->bus, nullptr, DBUSMENU_PATH,
                                 DBUS_INTROSPECTABLE_INTERFACE,
                                 introspect_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register menu Introspect: %s", strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
#endif

    pid = getpid();
    service_name_len = snprintf(nullptr, 0, "org.kde.StatusNotifierItem-%d-%d",
                                (int)pid, instance_counter++);
    if (service_name_len < 0) {
        typio_tray_destroy(tray);
        return nullptr;
    }

    tray->service_name = malloc((size_t)service_name_len + 1);
    if (!tray->service_name) {
        typio_tray_destroy(tray);
        return nullptr;
    }

    if (snprintf(tray->service_name, (size_t)service_name_len + 1,
                 "org.kde.StatusNotifierItem-%d-%d", (int)pid,
                 instance_counter - 1) < 0) {
        typio_tray_destroy(tray);
        return nullptr;
    }

#ifdef HAVE_LIBDBUS
    r = sd_bus_request_name(tray->bus, tray->service_name, 0);
    if (r < 0 && r != -EALREADY) {
        typio_log_error("Failed to acquire D-Bus name %s: %s",
                        tray->service_name, strerror(-r));
        typio_tray_destroy(tray);
        return nullptr;
    }
#endif

    typio_tray_sni_register(tray);
    return tray;
}

void typio_tray_destroy(TypioTray *tray) {
    if (!tray) {
        return;
    }

#ifdef HAVE_LIBDBUS
    if (tray->bus) {
        /* Unref the vtable slot first; this removes all per-(path, interface)
         * vtables registered through it. The watcher match slot is
         * unref'd next. Finally close + unref the bus. */
        if (tray->vtable_slot) {
            sd_bus_slot_unref(tray->vtable_slot);
            tray->vtable_slot = nullptr;
        }
        if (tray->watcher_match_slot) {
            sd_bus_slot_unref(tray->watcher_match_slot);
            tray->watcher_match_slot = nullptr;
        }
        sd_bus_close(tray->bus);
        sd_bus_unref(tray->bus);
    }
#endif

    free(tray->service_name);
    free(tray->icon_name);
    free(tray->icon_theme_path);
    free(tray->attention_icon_name);
    free(tray->tooltip_title);
    free(tray->tooltip_description);
    free(tray->title);
    free(tray->engine_name);

    typio_log_info("System tray destroyed");
    free(tray);
}

int typio_tray_get_fd(TypioTray *tray) {
#ifdef HAVE_LIBDBUS
    if (!tray || !tray->bus) {
        return -1;
    }
    int fd = sd_bus_get_fd(tray->bus);
    return fd < 0 ? -1 : fd;
#else
    (void)tray;
    return -1;
#endif
}

int typio_tray_dispatch(TypioTray *tray) {
#ifdef HAVE_LIBDBUS
    int dispatched = 0;

    if (!tray || !tray->bus) {
        return -1;
    }

    while (dispatched < TYPIO_TRAY_BUS_MAX_DISPATCH_PER_TICK &&
           sd_bus_process(tray->bus, nullptr) > 0) {
        dispatched++;
    }
    return 0;
#else
    (void)tray;
    return -1;
#endif
}

/* Assemble the tray tooltip from controller state. The active profile (e.g.
 * Rime schema name) rides on the keyboard line. The tray bus is the single
 * owner of all state-driven tray mutations (icon, engine, tooltip). */
static void tray_refresh_tooltip(TypioTray *tray, TypioStateController *ctrl) {
    const char *kb =
        typio_state_controller_get_active_engine_display_name(ctrl);
    const char *voice =
        typio_state_controller_get_active_voice_engine_display_name(ctrl);
    const TypioKeyboardEngineMode *mode =
        typio_state_controller_get_current_status(ctrl);
    const char *profile = (mode && mode->label && mode->label[0])
                          ? mode->label : nullptr;
    char desc[256];

    if (!kb || !*kb) {
        kb = typio_state_controller_get_active_engine_name(ctrl);
    }
    if (!kb || !*kb) {
        kb = "Unavailable";
    }
    if (!voice || !*voice) {
        voice = typio_state_controller_get_active_voice_engine_name(ctrl);
    }
    if (!voice || !*voice) {
        voice = "Disabled";
    }

    if (profile) {
        snprintf(desc, sizeof(desc), "Keyboard: %s (%s)\nVoice: %s",
                 kb, profile, voice);
    } else {
        snprintf(desc, sizeof(desc), "Keyboard: %s\nVoice: %s", kb, voice);
    }
    typio_tray_set_tooltip(tray, "Typio", desc);
}

static void tray_state_change_callback(void *user_data,
                                       TypioStateChangeType change_type) {
    TypioTray *tray = user_data;
    TypioStateController *ctrl = tray ? tray->state_controller : nullptr;
    if (!ctrl) {
        return;
    }

    switch (change_type) {
        case TYPIO_STATE_CHANGE_ENGINE:
        case TYPIO_STATE_CHANGE_VOICE_ENGINE:
        case TYPIO_STATE_CHANGE_STATUS_ICON: {
            const char *engine_name =
                typio_state_controller_get_active_engine_name(ctrl);
            const char *icon_name =
                typio_state_controller_get_status_icon(ctrl);
            bool is_active =
                typio_state_controller_get_engine_active(ctrl);
            typio_tray_set_icon(tray, icon_name);
            typio_tray_update_engine(tray, engine_name, is_active);
            tray_refresh_tooltip(tray, ctrl);
            break;
        }
        case TYPIO_STATE_CHANGE_STATUS: {
            const TypioKeyboardEngineMode *mode =
                typio_state_controller_get_current_status(ctrl);
            if (mode && mode->icon_name) {
                typio_tray_set_icon(tray, mode->icon_name);
            }
            tray_refresh_tooltip(tray, ctrl);
            break;
        }
    }
}

void typio_tray_bind_state_controller(TypioTray *tray,
                                      TypioStateController *ctrl) {
    if (!tray) {
        return;
    }
    if (tray->state_controller && tray->state_controller != ctrl) {
        typio_state_controller_remove_listener(tray->state_controller, tray);
    }
    tray->state_controller = ctrl;
    if (ctrl) {
        typio_state_controller_add_listener(
            ctrl,
            (TypioStateListener){ .user_data = tray,
                                  .callback = tray_state_change_callback });
    }
}
