/**
 * @file tray_internal.h
 * @brief Internal structures for system tray
 */

#ifndef TYPIO_TRAY_INTERNAL_H
#define TYPIO_TRAY_INTERNAL_H

#include "tray.h"
#include "icon_badge.h"

#ifdef HAVE_LIBSYSTEMD
#  include <systemd/sd-bus.h>
#endif

/* Pixmap sizes rendered for a language badge. The ladder covers the common
 * SNI host target sizes (16/22/24/32 for Waybar/Swaybar/Plasma/GNOME shell)
 * plus 44/48/64/96/128 for HiDPI and fractional-scaled outputs. SNI has no
 * scale channel, so we ship one pixmap per size and let the host pick the
 * closest fit rather than upscaling a tiny raster (ADR-0032). */
#define TYPIO_TRAY_BADGE_SIZES 9

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
#ifdef HAVE_LIBSYSTEMD
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
    char *overlay_icon_name;        /* corner overlay: voice presence (ADR-0032) */
    char *tooltip_title;
    char *tooltip_description;
    char *title;

    /* Language text badge (ADR-0032). When badge_pixmap_count > 0 the IconPixmap
     * channel carries these rendered bitmaps and IconName is suppressed. */
    char            *badge_text;
    TypioBadgePixmap badge_pixmaps[TYPIO_TRAY_BADGE_SIZES];
    size_t           badge_pixmap_count;

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

#ifdef HAVE_LIBSYSTEMD
/* sd-bus per-(path, interface) method handlers. Defined in sni.c;
 * the vtables that reference them live in bus.c so registration
 * happens after the bus is opened. */
int typio_tray_sni_method_call(sd_bus_message *m, void *userdata,
                               sd_bus_error *ret_error);
int typio_tray_menu_method_call(sd_bus_message *m, void *userdata,
                                sd_bus_error *ret_error);
/* sd_bus_property_get_t getters. Properties.Get / GetAll and the
 * Introspectable interface are synthesised by sd-bus from the
 * SD_BUS_PROPERTY / SD_BUS_SIGNAL rows in the per-path vtables. */
int typio_tray_sni_get_property(sd_bus *bus, const char *path,
                                const char *interface, const char *property,
                                sd_bus_message *reply, void *userdata,
                                sd_bus_error *ret_error);
int typio_tray_menu_get_property(sd_bus *bus, const char *path,
                                 const char *interface, const char *property,
                                 sd_bus_message *reply, void *userdata,
                                 sd_bus_error *ret_error);
#endif

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_TRAY_INTERNAL_H */
