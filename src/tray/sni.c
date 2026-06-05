/**
 * @file sni.c
 * @brief StatusNotifierItem D-Bus implementation using sd-bus
 */

#include "typio_build_config.h"
#include "tray_internal.h"

#include "typio/abi/config.h"
#include "typio/runtime/instance.h"
#include "typio/runtime/registry.h"
#include "typio/typio.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"

#ifdef HAVE_LIBSYSTEMD
#  include <systemd/sd-bus.h>
#endif

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* Generic engine-control menu IDs. Enum-property choices are addressed as
 * PROP_BASE + property_index*PROP_STRIDE + choice_index; commands as
 * CMD_BASE + command_index. The layout is recomputed from the active
 * engine's control surface on both build and click, so the ids are stable
 * within one menu render. */
#define TYPIO_TRAY_PROP_BASE    200
#define TYPIO_TRAY_PROP_STRIDE  32
#define TYPIO_TRAY_PROP_MAX     8
#define TYPIO_TRAY_CMD_BASE     600
#define TYPIO_TRAY_CMD_MAX      32

/* ── small sd-bus a{sv} helpers ────────────────────────────────────────── */

#ifdef HAVE_LIBSYSTEMD

/*
 * Append an empty pixmap array (a(iiay)) — used as the value for
 * IconPixmap / OverlayIconPixmap / AttentionIconPixmap in SNI.
 */
static int append_empty_pixmap_array(sd_bus_message *m) {
    int r;
    r = sd_bus_message_open_container(m, 'a', "(iiay)");
    if (r < 0) return r;
    return sd_bus_message_close_container(m);
}

static const char *tray_status_str(TypioTrayStatus status) {
    switch (status) {
        case TYPIO_TRAY_STATUS_ACTIVE: return "Active";
        case TYPIO_TRAY_STATUS_NEEDS_ATTENTION: return "NeedsAttention";
        default: return "Passive";
    }
}

/*
 * a{sv} dict entry appenders — the sd-bus equivalent of the old
 * typio_dbus_append_dict_entry_{string,bool,object_path} helpers.
 */
static int append_dict_str(sd_bus_message *m, const char *key, const char *value) {
    int r;
    r = sd_bus_message_open_container(m, 'e', "sv");
    if (r < 0) return r;
    r = sd_bus_message_append_basic(m, 's', key);
    if (r < 0) return r;
    r = sd_bus_message_open_container(m, 'v', "s");
    if (r < 0) return r;
    r = sd_bus_message_append_basic(m, 's', value ? value : "");
    if (r < 0) return r;
    r = sd_bus_message_close_container(m); /* v */
    if (r < 0) return r;
    return sd_bus_message_close_container(m); /* e */
}

static int append_dict_bool(sd_bus_message *m, const char *key, int value) {
    int r;
    r = sd_bus_message_open_container(m, 'e', "sv");
    if (r < 0) return r;
    r = sd_bus_message_append_basic(m, 's', key);
    if (r < 0) return r;
    r = sd_bus_message_open_container(m, 'v', "b");
    if (r < 0) return r;
    r = sd_bus_message_append_basic(m, 'b', &value);
    if (r < 0) return r;
    r = sd_bus_message_close_container(m); /* v */
    if (r < 0) return r;
    return sd_bus_message_close_container(m); /* e */
}

/* ── SNI property value appender ───────────────────────────────────────── */

static int sni_property_value(sd_bus_message *m, const char *property,
                              TypioTray *tray) {
    if (strcmp(property, "Category") == 0) {
        return sd_bus_message_append_basic(m, 's', "ApplicationStatus");
    } else if (strcmp(property, "Id") == 0) {
        return sd_bus_message_append_basic(m, 's', "typio");
    } else if (strcmp(property, "Title") == 0) {
        const char *val = tray->title ? tray->title : "Typio";
        return sd_bus_message_append_basic(m, 's', val);
    } else if (strcmp(property, "Status") == 0) {
        return sd_bus_message_append_basic(m, 's', tray_status_str(tray->status));
    } else if (strcmp(property, "IconName") == 0) {
        const char *val = tray->icon_name ? tray->icon_name
                                          : "typio-keyboard-symbolic";
        return sd_bus_message_append_basic(m, 's', val);
    } else if (strcmp(property, "IconThemePath") == 0) {
        const char *val = tray->icon_theme_path ? tray->icon_theme_path : "";
        return sd_bus_message_append_basic(m, 's', val);
    } else if (strcmp(property, "IconPixmap") == 0 ||
               strcmp(property, "OverlayIconPixmap") == 0 ||
               strcmp(property, "AttentionIconPixmap") == 0) {
        return append_empty_pixmap_array(m);
    } else if (strcmp(property, "OverlayIconName") == 0 ||
               strcmp(property, "AttentionIconName") == 0) {
        return sd_bus_message_append_basic(m, 's', "");
    } else if (strcmp(property, "ToolTip") == 0) {
        const char *icon = tray->icon_name ? tray->icon_name
                                            : "typio-keyboard-symbolic";
        const char *title = tray->tooltip_title ? tray->tooltip_title : "Typio";
        const char *desc = tray->tooltip_description ? tray->tooltip_description : "";
        int r;
        r = sd_bus_message_open_container(m, 'r', "sa(iiay)ss");
        if (r < 0) return r;
        r = sd_bus_message_append_basic(m, 's', icon);
        if (r < 0) return r;
        r = append_empty_pixmap_array(m);
        if (r < 0) return r;
        r = sd_bus_message_append_basic(m, 's', title);
        if (r < 0) return r;
        r = sd_bus_message_append_basic(m, 's', desc);
        if (r < 0) return r;
        return sd_bus_message_close_container(m);
    } else if (strcmp(property, "ItemIsMenu") == 0) {
        int val = 0;
        return sd_bus_message_append_basic(m, 'b', &val);
    } else if (strcmp(property, "Menu") == 0) {
        return sd_bus_message_append_basic(m, 'o', DBUSMENU_PATH);
    }
    return -EINVAL;
}

/*
 * sd-bus property getter (sd_bus_property_get_t). sd-bus has already
 * opened the enclosing variant with the signature declared in the
 * vtable row, so we just append the bare value. GetAll and
 * Properties.Get are both synthesised by sd-bus from the SD_BUS_PROPERTY
 * rows — we never implement org.freedesktop.DBus.Properties ourselves.
 */
int typio_tray_sni_get_property(sd_bus *bus, const char *path,
                                const char *interface, const char *property,
                                sd_bus_message *reply, void *userdata,
                                sd_bus_error *ret_error) {
    TypioTray *tray = userdata;
    (void)bus;
    (void)path;
    (void)interface;
    (void)ret_error;
    return sni_property_value(reply, property, tray);
}

/* ── SNI method calls (Activate / ContextMenu / Scroll / etc.) ─────────── */

int typio_tray_sni_method_call(sd_bus_message *m, void *userdata,
                               sd_bus_error *ret_error) {
    TypioTray *tray = userdata;
    const char *member = sd_bus_message_get_member(m);
    int32_t x = 0, y = 0;
    int r;
    (void)ret_error;

    if (strcmp(member, "ContextMenu") == 0 ||
        strcmp(member, "Activate") == 0 ||
        strcmp(member, "SecondaryActivate") == 0) {
        /* Lenient: try (ii), fall back to () for panels that don't
         * pass coordinates. */
        r = sd_bus_message_read(m, "ii", &x, &y);
        if (r < 0) {
            r = sd_bus_message_read(m, "");
            if (r < 0) return r;
            x = 0;
            y = 0;
        }

        typio_log_debug("Tray %s at (%d, %d)", member, x, y);

        if (tray->menu_callback) {
            if (strcmp(member, "ContextMenu") == 0) {
                tray->menu_callback(tray, "context_menu", tray->user_data);
            } else if (strcmp(member, "Activate") == 0) {
                tray->menu_callback(tray, "activate", tray->user_data);
            } else {
                tray->menu_callback(tray, "secondary_activate", tray->user_data);
            }
        }
        return sd_bus_reply_method_return(m, NULL);
    } else if (strcmp(member, "Scroll") == 0) {
        int32_t delta = 0;
        const char *orientation = nullptr;
        r = sd_bus_message_read(m, "is", &delta, &orientation);
        if (r < 0) {
            return sd_bus_reply_method_errorf(m, SD_BUS_ERROR_INVALID_ARGS,
                                              "Invalid arguments");
        }
        typio_log_debug("Tray scroll: delta=%d, orientation=%s",
                        delta, orientation ? orientation : "");
        if (tray->menu_callback) {
            tray->menu_callback(tray, delta > 0 ? "scroll_up" : "scroll_down",
                                tray->user_data);
        }
        return sd_bus_reply_method_return(m, NULL);
    }
    return sd_bus_reply_method_errorf(m, SD_BUS_ERROR_UNKNOWN_METHOD,
                                      "Unknown method");
}

/* ── DBusMenu property value appender ──────────────────────────────────── */

static int menu_property_value(sd_bus_message *m, const char *property) {
    if (strcmp(property, "Version") == 0) {
        uint32_t val = 3;
        return sd_bus_message_append_basic(m, 'u', &val);
    } else if (strcmp(property, "TextDirection") == 0) {
        return sd_bus_message_append_basic(m, 's', "ltr");
    } else if (strcmp(property, "Status") == 0) {
        return sd_bus_message_append_basic(m, 's', "normal");
    } else if (strcmp(property, "IconThemePath") == 0) {
        int r = sd_bus_message_open_container(m, 'a', "s");
        if (r < 0) return r;
        return sd_bus_message_close_container(m);
    }
    return -EINVAL;
}

/* sd-bus property getter (sd_bus_property_get_t) for com.canonical.dbusmenu.
 * As with the SNI item, sd-bus opens the variant and synthesises
 * Get/GetAll; we only append the bare value. */
int typio_tray_menu_get_property(sd_bus *bus, const char *path,
                                 const char *interface, const char *property,
                                 sd_bus_message *reply, void *userdata,
                                 sd_bus_error *ret_error) {
    (void)bus;
    (void)path;
    (void)interface;
    (void)userdata;
    (void)ret_error;
    return menu_property_value(reply, property);
}

/* ── DBusMenu methods ──────────────────────────────────────────────────── */

/* Build a menu item into the current container. The item's signature
 * is (ia{sv}av); the inner v/av children is always empty. */
static int build_menu_item(sd_bus_message *parent, int32_t id,
                           const char *label, const char *type, int enabled) {
    int r;
    r = sd_bus_message_open_container(parent, 'v', "(ia{sv}av)");
    if (r < 0) return r;
    r = sd_bus_message_open_container(parent, 'r', "ia{sv}av");
    if (r < 0) return r;
    r = sd_bus_message_append_basic(parent, 'i', &id);
    if (r < 0) return r;
    r = sd_bus_message_open_container(parent, 'a', "{sv}");
    if (r < 0) return r;
    if (label) {
        r = append_dict_str(parent, "label", label);
        if (r < 0) return r;
    }
    if (type) {
        r = append_dict_str(parent, "type", type);
        if (r < 0) return r;
    }
    r = append_dict_bool(parent, "enabled", enabled);
    if (r < 0) return r;
    r = sd_bus_message_close_container(parent);
    if (r < 0) return r;
    r = sd_bus_message_open_container(parent, 'a', "v");
    if (r < 0) return r;
    r = sd_bus_message_close_container(parent);
    if (r < 0) return r;
    r = sd_bus_message_close_container(parent); /* r */
    if (r < 0) return r;
    r = sd_bus_message_close_container(parent); /* v */
    return r;
}

static int handle_menu_getlayout(sd_bus_message *m, TypioTray *tray) {
    int32_t parent_id;
    int32_t depth;
    int r;
    int32_t item_id = 1;
    char label[256];
    sd_bus_message *reply = nullptr;

    r = sd_bus_message_read(m, "ii", &parent_id, &depth);
    if (r < 0) {
        return sd_bus_reply_method_errorf(m, SD_BUS_ERROR_INVALID_ARGS,
                                          "Invalid arguments");
    }

    /* The reply is a fresh message — the incoming call message is
     * sealed and cannot be appended to. Reply: u (revision) +
     * (ia{sv}av) (root item). */
    r = sd_bus_message_new_method_return(m, &reply);
    if (r < 0) return r;

    r = sd_bus_message_append_basic(reply, 'u', &tray->menu_revision);
    if (r < 0) goto fail;

    r = sd_bus_message_open_container(reply, 'r', "ia{sv}av");
    if (r < 0) goto fail;
    { int32_t root_id = 0;
      r = sd_bus_message_append_basic(reply, 'i', &root_id);
      if (r < 0) goto fail; }
    r = sd_bus_message_open_container(reply, 'a', "{sv}");
    if (r < 0) goto fail;
    r = append_dict_str(reply, "children-display", "submenu");
    if (r < 0) goto fail;
    r = sd_bus_message_close_container(reply);
    if (r < 0) goto fail;

    r = sd_bus_message_open_container(reply, 'a', "v");
    if (r < 0) goto fail;

    TypioRegistry *registry = typio_instance_get_registry(tray->instance);
    if (registry) {
        size_t engine_count;
        char **engines = typio_registry_list_ordered_keyboards(registry, &engine_count);
        size_t shown = 0;
        for (size_t i = 0; i < engine_count && i < 10; i++) {
            const TypioEngineInfo *info = typio_registry_get_engine_info(registry, engines[i]);
            const char *display = (info && info->display_name && info->display_name[0])
                ? info->display_name : engines[i];
            bool is_current = tray->engine_name &&
                              strcmp(engines[i], tray->engine_name) == 0;
            if (is_current) {
                snprintf(label, sizeof(label), "● %s", display);
            } else {
                snprintf(label, sizeof(label), "  %s", display);
            }
            r = build_menu_item(reply, 100 + (int32_t)i, label, nullptr, 1);
            if (r < 0) {
                typio_engine_info_free((TypioEngineInfo *)info);
                typio_free_string_array(engines, engine_count);
                goto fail;
            }
            typio_engine_info_free((TypioEngineInfo *)info);
            shown++;
        }
        if (shown > 0) {
            r = build_menu_item(reply, item_id++, nullptr, "separator", 1);
            if (r < 0) {
                typio_free_string_array(engines, engine_count);
                goto fail;
            }
        }
        typio_free_string_array(engines, engine_count);
    }

    r = build_menu_item(reply, 98, "Restart", nullptr, 1);
    if (r < 0) goto fail;
    r = build_menu_item(reply, 99, "Quit", nullptr, 1);
    if (r < 0) goto fail;

    r = sd_bus_message_close_container(reply); /* av */
    if (r < 0) goto fail;
    r = sd_bus_message_close_container(reply); /* root struct r */
    if (r < 0) goto fail;

    r = sd_bus_send(nullptr, reply, nullptr);
    sd_bus_message_unref(reply);
    return r;

fail:
    sd_bus_message_unref(reply);
    return r;
}

static int handle_menu_event(sd_bus_message *m, TypioTray *tray) {
    int32_t id = 0;
    const char *event_type = nullptr;
    int r;

    r = sd_bus_message_read(m, "is", &id, &event_type);
    if (r < 0) {
        return sd_bus_reply_method_return(m, NULL);
    }
    (void)tray;
    typio_log_debug("Menu event: id=%d, type=%s", id, event_type ? event_type : "");

    if (event_type && strcmp(event_type, "clicked") == 0) {
        if (id == 98) {
            if (tray->menu_callback) {
                tray->menu_callback(tray, "restart", tray->user_data);
            }
        } else if (id == 99) {
            if (tray->menu_callback) {
                tray->menu_callback(tray, "quit", tray->user_data);
            }
        } else if (id >= 100 && id < 110) {
            int engine_idx = id - 100;
            TypioRegistry *registry = typio_instance_get_registry(tray->instance);
            if (registry) {
                size_t engine_count;
                char **engines = typio_registry_list_ordered_keyboards(registry, &engine_count);
                if ((size_t)engine_idx < engine_count && tray->menu_callback) {
                    char action[128];
                    snprintf(action, sizeof(action), "engine:%s", engines[engine_idx]);
                    tray->menu_callback(tray, action, tray->user_data);
                }
                typio_free_string_array(engines, engine_count);
            }
        }
    }
    return sd_bus_reply_method_return(m, NULL);
}

int typio_tray_menu_method_call(sd_bus_message *m, void *userdata,
                                sd_bus_error *ret_error) {
    TypioTray *tray = userdata;
    const char *member = sd_bus_message_get_member(m);
    (void)ret_error;

    if (strcmp(member, "GetLayout") == 0) {
        typio_log_debug("Menu GetLayout called");
        return handle_menu_getlayout(m, tray);
    } else if (strcmp(member, "Event") == 0) {
        return handle_menu_event(m, tray);
    } else if (strcmp(member, "GetProperty") == 0) {
        /* GetProperty(i, s) -> v. We expose no per-item properties, so
         * reply with an empty-string variant to satisfy the signature. */
        sd_bus_message *reply = nullptr;
        int r = sd_bus_message_new_method_return(m, &reply);
        if (r < 0) return r;
        r = sd_bus_message_open_container(reply, 'v', "s");
        if (r >= 0) r = sd_bus_message_append_basic(reply, 's', "");
        if (r >= 0) r = sd_bus_message_close_container(reply);
        if (r < 0) { sd_bus_message_unref(reply); return r; }
        r = sd_bus_send(nullptr, reply, nullptr);
        sd_bus_message_unref(reply);
        return r;
    } else if (strcmp(member, "GetGroupProperties") == 0) {
        /* GetGroupProperties(ai, as) -> a(ia{sv}); reply with an
         * empty array. */
        sd_bus_message *reply = nullptr;
        int r = sd_bus_message_new_method_return(m, &reply);
        if (r < 0) return r;
        r = sd_bus_message_open_container(reply, 'a', "(ia{sv})");
        if (r >= 0) r = sd_bus_message_close_container(reply);
        if (r < 0) { sd_bus_message_unref(reply); return r; }
        r = sd_bus_send(nullptr, reply, nullptr);
        sd_bus_message_unref(reply);
        return r;
    } else if (strcmp(member, "AboutToShow") == 0) {
        /* AboutToShow(i) -> b. Returning false means "layout unchanged,
         * no need to re-fetch". */
        int val = 0;
        return sd_bus_reply_method_return(m, "b", val);
    }
    return sd_bus_reply_method_errorf(m, SD_BUS_ERROR_UNKNOWN_METHOD,
                                      "Unknown method");
}

#endif /* HAVE_LIBSYSTEMD */

/* ── Registration with the StatusNotifierWatcher ──────────────────────── */

int typio_tray_sni_register(TypioTray *tray) {
#ifdef HAVE_LIBSYSTEMD
    sd_bus_error err = SD_BUS_ERROR_NULL;
    sd_bus_message *reply = nullptr;
    int r;
#endif

    if (!tray
#ifdef HAVE_LIBSYSTEMD
        || !tray->bus
#endif
    ) {
        return -1;
    }

#ifdef HAVE_LIBSYSTEMD
    r = sd_bus_call_method(tray->bus,
                           SNI_WATCHER_SERVICE,
                           SNI_WATCHER_PATH,
                           SNI_WATCHER_INTERFACE,
                           "RegisterStatusNotifierItem",
                           &err,
                           &reply,
                           "s",
                           tray->service_name);
    if (r < 0) {
        typio_log_warning("Failed to register with StatusNotifierWatcher: %s",
                          err.message ? err.message : strerror(-r));
        sd_bus_error_free(&err);
        tray->registered = false;
        return -1;
    }

    sd_bus_message_unref(reply);
    tray->registered = true;
    typio_log_info("Registered with StatusNotifierWatcher as %s",
                   tray->service_name);
#endif
    return 0;
}

#ifdef HAVE_LIBSYSTEMD
void typio_tray_sni_emit_signal(TypioTray *tray, const char *signal_name) {
    sd_bus_message *sig = nullptr;
    int r;

    if (!tray || !tray->bus || !tray->registered) {
        return;
    }

    r = sd_bus_message_new_signal(tray->bus,
                                  &sig,
                                  SNI_ITEM_PATH,
                                  SNI_ITEM_INTERFACE,
                                  signal_name);
    if (r < 0) {
        typio_log_warning("Failed to build signal %s: %s",
                          signal_name, strerror(-r));
        return;
    }

    if (strcmp(signal_name, "NewStatus") == 0) {
        r = sd_bus_message_append_basic(sig, 's', tray_status_str(tray->status));
        if (r < 0) {
            sd_bus_message_unref(sig);
            return;
        }
    }

    r = sd_bus_send(tray->bus, sig, nullptr);
    if (r < 0) {
        typio_log_warning("Failed to send signal %s: %s",
                          signal_name, strerror(-r));
        sd_bus_message_unref(sig);
        return;
    }
    /* The tray's FD is only woken by incoming traffic from the SNI host,
     * so the connection's outgoing queue is otherwise never drained.
     * Force a flush after every signal so the host actually sees the
     * NewIcon / NewStatus / NewToolTip notifications; without this,
     * the icon (and any subsequent state change) silently fails to
     * update. */
    sd_bus_flush(tray->bus);
    sd_bus_message_unref(sig);
}
#else
void typio_tray_sni_emit_signal(TypioTray *tray, const char *signal_name) {
    (void)tray;
    (void)signal_name;
}
#endif

void typio_tray_set_status(TypioTray *tray, TypioTrayStatus status) {
    if (!tray || tray->status == status) {
        return;
    }

    tray->status = status;
    typio_tray_sni_emit_signal(tray, "NewStatus");
}

void typio_tray_set_icon(TypioTray *tray, const char *icon_name) {
    if (!tray) {
        return;
    }

    const char *proposed = icon_name && *icon_name
        ? icon_name : "typio-keyboard-symbolic";
    if (tray->icon_name && strcmp(tray->icon_name, proposed) == 0) {
        return;
    }

    free(tray->icon_name);
    tray->icon_name = typio_strdup(proposed);
    typio_tray_sni_emit_signal(tray, "NewIcon");
}

void typio_tray_set_icon_theme_path(TypioTray *tray, const char *icon_theme_path) {
    if (!tray) {
        return;
    }

    free(tray->icon_theme_path);
    tray->icon_theme_path = icon_theme_path ? typio_strdup(icon_theme_path) : typio_strdup("");
    typio_tray_sni_emit_signal(tray, "NewIcon");
}

void typio_tray_set_tooltip(TypioTray *tray, const char *title,
                            const char *description) {
    if (!tray) {
        return;
    }

    free(tray->tooltip_title);
    free(tray->tooltip_description);
    tray->tooltip_title = title ? typio_strdup(title) : nullptr;
    tray->tooltip_description = description ? typio_strdup(description) : nullptr;
    typio_tray_sni_emit_signal(tray, "NewToolTip");
}

void typio_tray_update_engine(TypioTray *tray, const char *engine_name,
                              bool is_active) {
    if (!tray) {
        return;
    }

    free(tray->engine_name);
    tray->engine_name = engine_name ? typio_strdup(engine_name) : nullptr;
    tray->engine_active = is_active;

    tray->menu_revision++;

#ifdef HAVE_LIBSYSTEMD
    if (tray->bus && tray->registered) {
        sd_bus_message *sig = nullptr;
        int r;
        r = sd_bus_message_new_signal(tray->bus, &sig, DBUSMENU_PATH,
                                      DBUSMENU_INTERFACE, "LayoutUpdated");
        if (r >= 0) {
            uint32_t rev = tray->menu_revision;
            int32_t parent = 0;
            r = sd_bus_message_append(sig, "ui", rev, parent);
            if (r >= 0) {
                r = sd_bus_send(tray->bus, sig, nullptr);
                if (r >= 0) {
                    sd_bus_flush(tray->bus);
                }
            }
            sd_bus_message_unref(sig);
        }
    }
#endif

    char tooltip[256];
    if (engine_name) {
        snprintf(tooltip, sizeof(tooltip), "Typio - %s%s",
                 engine_name, is_active ? " (active)" : "");
    } else {
        snprintf(tooltip, sizeof(tooltip), "Typio - No engine");
    }
    typio_tray_set_tooltip(tray, tooltip, nullptr);

    typio_tray_set_status(tray, is_active ? TYPIO_TRAY_STATUS_ACTIVE
                                          : TYPIO_TRAY_STATUS_PASSIVE);
}

bool typio_tray_is_registered(TypioTray *tray) {
    return tray && tray->registered;
}
