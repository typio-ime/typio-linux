/**
 * @file tray_internal.h
 * @brief Internal structures for system tray
 */

#ifndef TYPIO_TRAY_INTERNAL_H
#define TYPIO_TRAY_INTERNAL_H

#include "tray.h"

#ifdef HAVE_LIBDBUS
#  include <systemd/sd-bus.h>
#endif

typedef struct TypioStateController TypioStateController;

#ifdef __cplusplus
extern "C" {
#endif

/* D-Bus service names and paths */
#define SNI_WATCHER_SERVICE "org.kde.StatusNotifierWatcher"
#define SNI_WATCHER_PATH "/StatusNotifierWatcher"
#define SNI_WATCHER_INTERFACE "org.kde.StatusNotifierWatcher"

#define SNI_ITEM_INTERFACE "org.kde.StatusNotifierItem"
#define SNI_ITEM_PATH "/StatusNotifierItem"

#define DBUS_SERVICE "org.freedesktop.DBus"
#define DBUS_PATH "/org/freedesktop/DBus"
#define DBUS_INTERFACE "org.freedesktop.DBus"
#define DBUS_PROPERTIES_INTERFACE "org.freedesktop.DBus.Properties"
#define DBUS_INTROSPECTABLE_INTERFACE "org.freedesktop.DBus.Introspectable"

/* Menu interface */
#define DBUSMENU_INTERFACE "com.canonical.dbusmenu"
#define DBUSMENU_PATH "/MenuBar"

/**
 * @brief Main tray structure
 */
struct TypioTray {
    /* Typio instance */
    TypioInstance *instance;

    /* D-Bus connection */
#ifdef HAVE_LIBDBUS
    sd_bus *bus;
    /* sd_bus_slot returned by sd_bus_add_object_vtable calls; nulled on
     * teardown. The slot is unref'd explicitly before sd_bus_unref to
     * avoid a use-after-unref on in-flight messages. */
    sd_bus_slot *vtable_slot;
    /* Match slot for org.freedesktop.DBus.NameOwnerChanged
     * (StatusNotifierWatcher presence). */
    sd_bus_slot *watcher_match_slot;
#endif

    /* Service name */
    char *service_name;             /* e.g., org.kde.StatusNotifierItem-PID-N */

    /* State */
    bool registered;
    TypioTrayStatus status;

    /* Properties */
    char *icon_name;
    char *icon_theme_path;
    char *attention_icon_name;
    char *tooltip_title;
    char *tooltip_description;
    char *title;

    /* Current engine info */
    char *engine_name;
    bool engine_active;

    /* Menu revision */
    uint32_t menu_revision;

    /* Callbacks */
    TypioTrayMenuCallback menu_callback;
    void *user_data;

    /* State controller binding */
    struct TypioStateController *state_controller;
};

/* SNI implementation functions */
int typio_tray_sni_register(TypioTray *tray);
void typio_tray_sni_emit_signal(TypioTray *tray, const char *signal_name);

#ifdef HAVE_LIBDBUS
/* sd-bus per-(path, interface) method handlers. Defined in sni.c;
 * the vtables that reference them live in bus.c so registration
 * happens after the bus is opened. */
int typio_tray_sni_method_call(sd_bus_message *m, void *userdata,
                               sd_bus_error *ret_error);
int typio_tray_sni_properties_get(sd_bus_message *m, void *userdata,
                                  sd_bus_error *ret_error);
int typio_tray_sni_properties_getall(sd_bus_message *m, void *userdata,
                                     sd_bus_error *ret_error);
int typio_tray_menu_method_call(sd_bus_message *m, void *userdata,
                                sd_bus_error *ret_error);
int typio_tray_menu_properties_get(sd_bus_message *m, void *userdata,
                                   sd_bus_error *ret_error);
int typio_tray_menu_properties_getall(sd_bus_message *m, void *userdata,
                                      sd_bus_error *ret_error);
int typio_tray_introspect(sd_bus_message *m, void *userdata,
                          sd_bus_error *ret_error);
#endif

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_TRAY_INTERNAL_H */
