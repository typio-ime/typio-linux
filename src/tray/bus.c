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

#ifdef HAVE_LIBSYSTEMD
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

#ifdef HAVE_LIBSYSTEMD

/* Forward declarations of the per-(path, interface) sd-bus method
 * handlers. Each returns 0 on success (sending a reply on the message),
 * negative sd-bus error on failure. The first void* argument is the
 * TypioTray* userdata registered with sd_bus_add_object_vtable().
 * Definitions live in sni.c; declared in tray_internal.h. */

static int watcher_owner_changed(sd_bus_message *m, void *userdata,
                                 sd_bus_error *ret_error);

/* org.kde.StatusNotifierItem on /StatusNotifierItem. Properties are
 * declared as SD_BUS_PROPERTY rows so sd-bus synthesises
 * org.freedesktop.DBus.Properties (Get / GetAll) and the
 * Introspectable interface for us — those reserved interfaces must NOT
 * be registered by hand (sd_bus_add_object_vtable returns -EINVAL for
 * them). The custom NewIcon / NewStatus / ... signals are declared so
 * they appear in the generated introspection XML; they are emitted
 * from sni.c via sd_bus_message_new_signal. */
static const sd_bus_vtable sni_item_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("ContextMenu", "ii", "", typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("Activate", "ii", "", typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("SecondaryActivate", "ii", "", typio_tray_sni_method_call, 0),
    SD_BUS_METHOD("Scroll", "is", "", typio_tray_sni_method_call, 0),
    SD_BUS_PROPERTY("Category", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("Id", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("Title", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("Status", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("IconName", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("IconThemePath", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("IconPixmap", "a(iiay)", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("OverlayIconName", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("OverlayIconPixmap", "a(iiay)", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("AttentionIconName", "s", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("AttentionIconPixmap", "a(iiay)", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("ToolTip", "(sa(iiay)ss)", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("ItemIsMenu", "b", typio_tray_sni_get_property, 0, 0),
    SD_BUS_PROPERTY("Menu", "o", typio_tray_sni_get_property, 0, 0),
    SD_BUS_SIGNAL("NewTitle", NULL, 0),
    SD_BUS_SIGNAL("NewIcon", NULL, 0),
    SD_BUS_SIGNAL("NewAttentionIcon", NULL, 0),
    SD_BUS_SIGNAL("NewOverlayIcon", NULL, 0),
    SD_BUS_SIGNAL("NewToolTip", NULL, 0),
    SD_BUS_SIGNAL("NewStatus", "s", 0),
    SD_BUS_VTABLE_END,
};

static const sd_bus_vtable menu_vtable[] = {
    SD_BUS_VTABLE_START(0),
    SD_BUS_METHOD("GetLayout", "iias", "u(ia{sv}av)", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("Event", "isvu", "", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("GetProperty", "is", "v", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("GetGroupProperties", "aias", "a(ia{sv})", typio_tray_menu_method_call, 0),
    SD_BUS_METHOD("AboutToShow", "i", "b", typio_tray_menu_method_call, 0),
    SD_BUS_PROPERTY("Version", "u", typio_tray_menu_get_property, 0, 0),
    SD_BUS_PROPERTY("TextDirection", "s", typio_tray_menu_get_property, 0, 0),
    SD_BUS_PROPERTY("Status", "s", typio_tray_menu_get_property, 0, 0),
    SD_BUS_PROPERTY("IconThemePath", "as", typio_tray_menu_get_property, 0, 0),
    SD_BUS_SIGNAL("ItemsPropertiesUpdated", "a(ia{sv})a(ias)", 0),
    SD_BUS_SIGNAL("LayoutUpdated", "ui", 0),
    SD_BUS_SIGNAL("ItemActivationRequested", "iu", 0),
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

#endif /* HAVE_LIBSYSTEMD */


TypioTray *typio_tray_new(TypioInstance *instance, const TypioTrayConfig *config) {
    pid_t pid;
    static int instance_counter = 0;
    TypioTray *tray;
    int service_name_len;
#ifdef HAVE_LIBSYSTEMD
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

#ifdef HAVE_LIBSYSTEMD
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

    /* Register one object vtable per path. sd-bus derives the standard
     * org.freedesktop.DBus.Properties / Introspectable / Peer interfaces
     * from the SD_BUS_PROPERTY / method rows automatically, so we only
     * register the SNI item and dbusmenu interfaces themselves. The
     * SNI slot is anchored on tray->vtable_slot; the menu registration
     * passes NULL because that slot is owned by the bus. Both are torn
     * down when vtable_slot (and ultimately the bus) is unref'd. */
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
    r = sd_bus_add_object_vtable(tray->bus, nullptr, DBUSMENU_PATH,
                                 DBUSMENU_INTERFACE,
                                 menu_vtable, tray);
    if (r < 0) {
        typio_log_error("Failed to register menu object path: %s", strerror(-r));
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

#ifdef HAVE_LIBSYSTEMD
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

#ifdef HAVE_LIBSYSTEMD
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
    free(tray->overlay_icon_name);
    free(tray->tooltip_title);
    free(tray->tooltip_description);
    free(tray->title);
    free(tray->engine_name);

    /* Release rendered badge bitmaps + cached text (ADR-0032). */
    for (size_t i = 0; i < tray->badge_pixmap_count; i++) {
        typio_badge_pixmap_free(&tray->badge_pixmaps[i]);
    }
    free(tray->badge_text);
    typio_icon_badge_shutdown();

    typio_log_info("System tray destroyed");
    free(tray);
}

int typio_tray_get_fd(TypioTray *tray) {
#ifdef HAVE_LIBSYSTEMD
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
#ifdef HAVE_LIBSYSTEMD
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

/* Assemble the tray tooltip from controller state. The tooltip carries the
 * information the icon does NOT: the keyboard engine + active profile, and
 * the voice engine when one is configured. The active language is already
 * encoded by the tray icon (badge or per-language icon, ADR-0033), so it is
 * not repeated here unless no language is active (legacy engine-cycling
 * installs, where there is no icon-level language cue). The tray bus is the
 * single owner of all state-driven tray mutations (icon, engine, tooltip). */
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

    const char *lang_label =
        typio_language_endonym(typio_state_controller_get_active_language(ctrl));

    if (!kb || !*kb) {
        kb = typio_state_controller_get_active_engine_name(ctrl);
    }
    if (!kb || !*kb) {
        /* Distinguish a deliberately empty (layout-only) slot from a genuinely
         * unavailable engine: when a language is active, the empty slot is by
         * design — keys pass through to the system layout. */
        kb = lang_label ? "— (layout-only)" : "Unavailable";
    }
    /* Voice engine is orthogonal (ADR-0026); only surface it when one is
     * configured, so the common "no voice" case is not cluttered with a
     * "Voice: Disabled" line. */
    const char *voice_line = nullptr;
    char voice_buf[160];
    if (!voice || !*voice) {
        voice = typio_state_controller_get_active_voice_engine_name(ctrl);
    }
    if (voice && *voice) {
        snprintf(voice_buf, sizeof(voice_buf), "\nVoice: %s", voice);
        voice_line = voice_buf;
    }

    /* No language active: there is no language badge on the icon, so the
     * tooltip must say what the keyboard slot actually is. Otherwise the
     * language is redundant with the icon and is omitted. */
    if (!lang_label) {
        if (profile) {
            snprintf(desc, sizeof(desc), "Keyboard: %s (%s)%s",
                     kb, profile, voice_line ? voice_line : "");
        } else {
            snprintf(desc, sizeof(desc), "Keyboard: %s%s",
                     kb, voice_line ? voice_line : "");
        }
    } else if (profile) {
        snprintf(desc, sizeof(desc), "Keyboard: %s (%s)%s",
                 kb, profile, voice_line ? voice_line : "");
    } else {
        snprintf(desc, sizeof(desc), "Keyboard: %s%s",
                 kb, voice_line ? voice_line : "");
    }
    typio_tray_set_tooltip(tray, "Typio", desc);
}

/* Apply the resolved base icon: a rendered language badge when the icon falls
 * to the language floor (ADR-0032), otherwise the named icon. */
static void tray_apply_icon(TypioTray *tray, TypioStateController *ctrl) {
    if (typio_state_controller_get_status_icon_is_badge(ctrl)) {
        typio_tray_set_badge(
            tray, typio_state_controller_get_status_badge_text(ctrl));
    } else {
        typio_tray_set_icon(tray, typio_state_controller_get_status_icon(ctrl));
    }
}

/* Apply the voice-slot overlay: presence only (ADR-0032), a stock microphone
 * glyph when a voice engine is active. Brand identity stays in the menu. */
static void tray_apply_overlay(TypioTray *tray, TypioStateController *ctrl) {
    const char *voice =
        typio_state_controller_get_active_voice_engine_name(ctrl);
    typio_tray_set_overlay_icon(
        tray, voice ? "audio-input-microphone-symbolic" : nullptr);
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
            /* A layout-only language (empty keyboard slot, e.g. Darija) is a
             * legitimate active state: keys pass through to the system layout.
             * Treat "a language is active" as active so the tray does not show
             * the off/passive icon for it (ADR-0031). */
            bool is_active =
                typio_state_controller_get_engine_active(ctrl) ||
                typio_state_controller_get_active_language(ctrl) != nullptr;
            tray_apply_icon(tray, ctrl);
            tray_apply_overlay(tray, ctrl);
            typio_tray_update_engine(tray, engine_name, is_active);
            tray_refresh_tooltip(tray, ctrl);
            break;
        }
        case TYPIO_STATE_CHANGE_LANGUAGE:
            tray_apply_icon(tray, ctrl);
            tray_refresh_tooltip(tray, ctrl);
            break;
        case TYPIO_STATE_CHANGE_STATUS: {
            /* Engine mode/schema changes used to overwrite the tray base icon
             * with the mode's `icon_name`. The tray icon is language-only now
             * (ADR-0033); the mode label still rides the tooltip, so just
             * refresh it. */
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
