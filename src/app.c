#include "app.h"

#include "ipc/ipc_bus.h"
#include "plugin_loader.h"
#include "state/controller.h"
#include "typio/abi/config.h"
#include "typio/runtime/registry.h"
#include "typio/abi/engine.h"
#include "typio/abi/string.h"
#include "typio/typio.h"
#include "typio_build_config.h"
#include "typio/abi/log.h"

#include <dirent.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static TypiodApp *g_active_app = nullptr;

static const char *typiod_app_build_display_string(void) {
    static char buf[128];
    if (buf[0])
        return buf;
    if (TYPIO_BUILD_SOURCE_LABEL[0]) {
        snprintf(buf, sizeof(buf), "typio-wayland %s (%s)",
                 TYPIO_VERSION, TYPIO_BUILD_SOURCE_LABEL);
    } else {
        snprintf(buf, sizeof(buf), "typio-wayland %s", TYPIO_VERSION);
    }
    return buf;
}

#ifdef HAVE_SYSTRAY
static void typiod_update_tray_engine_status(TypiodApp *app);
#endif

static const char *typiod_signal_name(int sig) {
    switch (sig) {
        case SIGINT:
            return "SIGINT";
        case SIGTERM:
            return "SIGTERM";
#ifdef SIGHUP
        case SIGHUP:
            return "SIGHUP";
#endif
#ifdef SIGQUIT
        case SIGQUIT:
            return "SIGQUIT";
#endif
        default:
            return "UNKNOWN";
    }
}

static void typiod_signal_handler(int sig) {
    if (g_active_app) {
        g_active_app->shutdown_requested_by_signal = true;
        g_active_app->shutdown_signal = sig;
    }
#ifdef HAVE_WAYLAND
    if (g_active_app && g_active_app->wl_frontend) {
        typio_wl_frontend_stop(g_active_app->wl_frontend);
    }
#endif
}

static void typiod_log_callback(const TypioLogEvent *event,
                                      [[maybe_unused]] void *user_data) {
    if (!event) {
        return;
    }
    const char *level_str;
    struct timespec ts;
    struct tm tm;
    char timebuf[sizeof("YYYY-MM-DD HH:MM:SS")];

    switch (event->level) {
        case TYPIO_LOG_TRACE:
            level_str = "TRACE";
            break;
        case TYPIO_LOG_DEBUG:
            level_str = "DEBUG";
            break;
        case TYPIO_LOG_INFO:
            level_str = "INFO";
            break;
        case TYPIO_LOG_WARNING:
            level_str = "WARN";
            break;
        case TYPIO_LOG_ERROR:
            level_str = "ERROR";
            break;
        default:
            level_str = "UNKNOWN";
            break;
    }

    if (clock_gettime(CLOCK_REALTIME, &ts) == 0 &&
        localtime_r(&ts.tv_sec, &tm)) {
        if (strftime(timebuf, sizeof(timebuf), "%Y-%m-%d %H:%M:%S", &tm) == 0) {
            timebuf[0] = '\0';
        }
    } else {
        timebuf[0] = '\0';
    }

    const char *domain = event->domain ? event->domain : "typio";
    const char *message = event->message ? event->message : "";
    if (timebuf[0]) {
        fprintf(stderr, "[%s] [%s] [%s] %s\n", timebuf, domain, level_str, message);
    } else {
        fprintf(stderr, "[%s] [%s] %s\n", domain, level_str, message);
    }
}

static void typiod_request_stop(void *user_data) {
    TypiodApp *app = user_data;

    if (!app) {
        return;
    }

#ifdef HAVE_WAYLAND
    if (app->wl_frontend) {
        typio_wl_frontend_stop(app->wl_frontend);
    }
#endif
}

#ifdef HAVE_SYSTRAY
static void typiod_update_tray_tooltip(TypiodApp *app) {
    const char *keyboard_label = nullptr;
    const char *voice_label = nullptr;
    bool keyboard_label_owned = false;
    bool voice_label_owned = false;
    char description[256];

    if (!app || !app->tray) {
        return;
    }

    if (app->state_controller) {
        keyboard_label =
            typio_state_controller_get_active_engine_display_name(
                app->state_controller);
        voice_label =
            typio_state_controller_get_active_voice_engine_display_name(
                app->state_controller);
        if (!keyboard_label || !*keyboard_label) {
            keyboard_label =
                typio_state_controller_get_active_engine_name(
                    app->state_controller);
        }
        if (!voice_label || !*voice_label) {
            voice_label =
                typio_state_controller_get_active_voice_engine_name(
                    app->state_controller);
        }
    } else if (app->instance) {
        TypioRegistry *registry = typio_instance_get_registry(app->instance);
        char *kb_name = registry
            ? typio_registry_get_active_keyboard(registry) : nullptr;
        char *voice_name = registry
            ? typio_registry_get_active_voice(registry) : nullptr;
        char *kb_label_copy = (kb_name && registry)
            ? typio_registry_get_engine_display_name(registry, kb_name) : nullptr;
        char *voice_label_copy = (voice_name && registry)
            ? typio_registry_get_engine_display_name(registry, voice_name) : nullptr;
        if (kb_label_copy && !*kb_label_copy) {
            typio_free_string(kb_label_copy);
            kb_label_copy = kb_name ? typio_strdup(kb_name) : nullptr;
        }
        if (voice_label_copy && !*voice_label_copy) {
            typio_free_string(voice_label_copy);
            voice_label_copy = voice_name ? typio_strdup(voice_name) : nullptr;
        }
        typio_free_string(kb_name);
        typio_free_string(voice_name);
        keyboard_label = kb_label_copy;
        voice_label = voice_label_copy;
        keyboard_label_owned = kb_label_copy != nullptr;
        voice_label_owned = voice_label_copy != nullptr;
    }

    if (!keyboard_label || !*keyboard_label) {
        keyboard_label = "Unavailable";
    }
    if (!voice_label || !*voice_label) {
        voice_label = "Disabled";
    }

    snprintf(description, sizeof(description),
             "Keyboard: %s\nVoice: %s",
             keyboard_label,
             voice_label);
    typio_tray_set_tooltip(app->tray, "Typio", description);

    if (keyboard_label_owned) {
        typio_free_string((char *)keyboard_label);
    }
    if (voice_label_owned) {
        typio_free_string((char *)voice_label);
    }
}
#endif

static void typiod_sync_runtime_surfaces(TypiodApp *app) {
#ifdef HAVE_SYSTRAY
    typiod_update_tray_engine_status(app);
#endif
    /* IPC bus pushes events.* notifications via the state-controller listener. */
}

static void typiod_print_startup_banner(TypiodApp *app) {
    TypioRegistry *registry;
    char *kb_name;
    char *voice_name;

    typio_log_info("Starting %s", typiod_app_build_display_string());
    typio_log_info("Configuration: %s", typio_instance_get_config_dir(app->instance));
    typio_log_info("Data: %s", typio_instance_get_data_dir(app->instance));

    registry = typio_instance_get_registry(app->instance);
    kb_name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    voice_name = registry ? typio_registry_get_active_voice(registry) : nullptr;
    if (kb_name) {
        typio_log_info("Active keyboard engine: %s", kb_name);
    } else {
        typio_log_info("No active keyboard engine");
    }
    typio_log_info("Active voice engine: %s",
           voice_name ? voice_name : "(disabled)");
    typio_free_string(kb_name);
    typio_free_string(voice_name);
}

#ifdef HAVE_SYSTRAY
static void typiod_update_tray_engine_status(TypiodApp *app) {
    const char *engine_name = nullptr;
    const char *icon_name = nullptr;
    bool is_active = false;
    char *active_name = nullptr;

    if (!app || !app->tray) {
        return;
    }

    if (app->state_controller) {
        engine_name =
            typio_state_controller_get_active_engine_name(app->state_controller);
        icon_name =
            typio_state_controller_get_status_icon(app->state_controller);
        is_active =
            typio_state_controller_get_engine_active(app->state_controller);
    } else if (app->instance) {
        TypioRegistry *registry = typio_instance_get_registry(app->instance);
        active_name = registry
            ? typio_registry_get_active_keyboard(registry) : nullptr;
        char *engine_icon = (active_name && registry)
            ? typio_registry_get_engine_icon(registry, active_name) : nullptr;
        engine_name = active_name;
        icon_name = typio_instance_get_last_status_icon(app->instance);
        if (!icon_name || !*icon_name) {
            icon_name = (engine_icon && *engine_icon) ? engine_icon : "typio-keyboard-symbolic";
        }
        is_active = active_name != nullptr;
        typio_tray_set_icon(app->tray, icon_name);
        typio_tray_update_engine(app->tray, engine_name, is_active);
        typiod_update_tray_tooltip(app);
        typio_free_string(engine_icon);
        typio_free_string(active_name);
        return;
    }

    typio_tray_set_icon(app->tray, icon_name);
    typio_tray_update_engine(app->tray, engine_name, is_active);
    typiod_update_tray_tooltip(app);
}
#endif

static void typiod_on_mode_change(TypioInstance *instance,
                                        const TypioEngineMode *mode,
                                        void *user_data) {
    TypiodApp *app = user_data;
    TypioRegistry *registry;

    if (app && app->instance) {
        char *name;
        registry = typio_instance_get_registry(app->instance);
        name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
#ifdef HAVE_WAYLAND
        if (app->wl_frontend && name && mode && mode->mode_id && mode->mode_id[0]) {
            typio_wl_frontend_remember_active_mode(app->wl_frontend,
                                                   name,
                                                   mode->mode_id);
        }
#endif
        typio_free_string(name);
    }

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_mode_changed(app->state_controller, mode);
    } else {
        typiod_sync_runtime_surfaces(app);
    }
}

static void typiod_on_status_icon_change(TypioInstance *instance,
                                               const char *icon_name,
                                               void *user_data) {
    TypiodApp *app = user_data;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_status_icon_changed(app->state_controller,
                                                          icon_name);
    } else {
#ifdef HAVE_SYSTRAY
        if (app && app->tray && icon_name) {
            typio_tray_set_icon(app->tray, icon_name);
        }
#endif
        typiod_sync_runtime_surfaces(app);
    }
}

static void typiod_on_engine_change(TypioInstance *instance,
                                          const TypioEngineInfo *engine,
                                          void *user_data) {
    TypiodApp *app = user_data;
    TypioRegistry *registry;
    char *active_name;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_engine_changed(app->state_controller, engine);
    } else {
        typiod_sync_runtime_surfaces(app);
    }

    if (!app || !app->instance) {
        return;
    }

    registry = typio_instance_get_registry(app->instance);
    active_name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    if (active_name) {
#ifdef HAVE_WAYLAND
        if (app && app->wl_frontend) {
            typio_wl_frontend_remember_active_engine(app->wl_frontend,
                                                     active_name);
        }
#endif
        typio_log_info("Engine changed to: %s", active_name);
        typio_free_string(active_name);
    }
}

static void typiod_on_voice_engine_change(TypioInstance *instance,
                                                const TypioEngineInfo *engine,
                                                void *user_data) {
    TypiodApp *app = user_data;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_voice_engine_changed(app->state_controller,
                                                           engine);
    } else {
        typiod_sync_runtime_surfaces(app);
    }
    if (engine && engine->name) {
        typio_log_info("Voice engine changed to: %s", engine->name);
    }
}

static void typiod_on_state_changed(void *user_data,
                                          [[maybe_unused]] TypioStateChangeType change_type) {
    TypiodApp *app = user_data;
    typiod_sync_runtime_surfaces(app);
}

#ifdef HAVE_SYSTRAY
static void typiod_tray_menu_callback([[maybe_unused]] TypioTray *tray,
                                            const char *action,
                                            void *user_data) {
    TypiodApp *app = user_data;
    TypioRegistry *registry;

    if (!app || !action) {
        return;
    }

    if (strcmp(action, "quit") == 0) {
#ifdef HAVE_WAYLAND
        if (app->wl_frontend) {
            typio_wl_frontend_stop(app->wl_frontend);
        }
#endif
        return;
    }

    if (strcmp(action, "restart") == 0) {
        app->restart_requested = true;
#ifdef HAVE_WAYLAND
        if (app->wl_frontend) {
            typio_wl_frontend_stop(app->wl_frontend);
        }
#endif
        return;
    }

    registry = typio_instance_get_registry(app->instance);
    if (!registry) {
        return;
    }

    if (strcmp(action, "activate") == 0) {
        TypioResult result = typio_registry_next_keyboard(registry);
        if (result == TYPIO_OK) {
            typio_log_info("Switched to next engine");
        } else {
            typio_log_error("Failed to switch to next engine: error %d", result);
        }
        return;
    }

    if (strcmp(action, "scroll_up") == 0) {
        TypioResult result = typio_registry_prev_keyboard(registry);
        if (result != TYPIO_OK) {
            typio_log_error("Failed to switch to previous engine: error %d", result);
        }
        return;
    }

    if (strcmp(action, "scroll_down") == 0) {
        TypioResult result = typio_registry_next_keyboard(registry);
        if (result != TYPIO_OK) {
            typio_log_error("Failed to switch to next engine (scroll): error %d", result);
        }
        return;
    }

    if (strncmp(action, "engine:", 7) == 0) {
        const char *engine_name = action + 7;

        TypioResult result = typio_registry_set_active_keyboard(registry, engine_name);
        if (result == TYPIO_OK) {
            typio_log_info("Switched to engine: %s", engine_name);
        } else {
            typio_log_error("Failed to switch to engine '%s': error %d", engine_name, result);
        }
        return;
    }

    /* Engine-prop / engine-cmd routing: the new TypioRegistry API does not
     * expose direct property/command invocation on the active engine; engines
     * are addressed through their own D-Bus/IPC surface. The tray entries
     * remain harmless no-ops until the host gains a registry-level wrapper. */
    if (strncmp(action, "engine-prop:", 12) == 0 ||
        strncmp(action, "engine-cmd:", 11) == 0) {
        typio_log_warning("Engine property/command actions are not wired up to "
                          "the registry API yet: %s", action);
        return;
    }
}
#endif

static void typiod_install_signal_handlers(TypiodApp *app) {
    g_active_app = app;
    signal(SIGINT, typiod_signal_handler);
    signal(SIGTERM, typiod_signal_handler);
}

static bool typiod_is_legacy_recent_log_name(const char *name) {
    size_t len;
    const char prefix[] = "typio-recent-";
    const char suffix[] = ".log";
    size_t prefix_len = strlen(prefix);
    size_t suffix_len = strlen(suffix);

    if (!name || !*name) {
        return false;
    }

    if (strcmp(name, "typio-recent.log") == 0) {
        return true;
    }

    len = strlen(name);
    return len > prefix_len + suffix_len &&
           strncmp(name, prefix, prefix_len) == 0 &&
           strcmp(name + len - suffix_len, suffix) == 0;
}

static void typiod_remove_legacy_recent_logs(const char *state_dir) {
    DIR *dir;
    struct dirent *entry;
    size_t removed_count = 0;

    if (!state_dir || !*state_dir) {
        return;
    }

    dir = opendir(state_dir);
    if (!dir) {
        return;
    }

    while ((entry = readdir(dir)) != nullptr) {
        char legacy_path[1024];

        if (!typiod_is_legacy_recent_log_name(entry->d_name)) {
            continue;
        }

        if (snprintf(legacy_path, sizeof(legacy_path), "%s/%s",
                     state_dir, entry->d_name) >= (int)sizeof(legacy_path)) {
            typio_log_warning("Skipping oversized legacy log cleanup path in "
                              "state dir: %s",
                              entry->d_name);
            continue;
        }

        if (unlink(legacy_path) == 0) {
            removed_count++;
        } else if (errno != ENOENT) {
            typio_log_warning("Failed to remove legacy recent log %s: %s",
                              legacy_path, strerror(errno));
        }
    }

    closedir(dir);

    if (removed_count > 0) {
        typio_log_info("Removed %zu legacy recent log file(s) from %s",
                       removed_count, state_dir);
    }
}

static void typiod_configure_recent_log_dump(TypiodApp *app) {
    const char *state_dir;

    if (!app || !app->instance) {
        return;
    }

    state_dir = typio_instance_get_state_dir(app->instance);
    if (!state_dir || !*state_dir) {
        return;
    }

    typiod_remove_legacy_recent_logs(state_dir);

    if (snprintf(app->recent_log_dump_path, sizeof(app->recent_log_dump_path),
                 "%s/%s", state_dir, "logs/latest.log") >=
        (int)sizeof(app->recent_log_dump_path)) {
        app->recent_log_dump_path[0] = '\0';
        return;
    }

    /* libtypio no longer holds a "configured path"; the host stamps the
     * path here and calls typio_logger_dump_recent on demand. */
    (void)app->recent_log_dump_path;
}

void typiod_dump_recent_log(void) {
    if (!g_active_app || !g_active_app->recent_log_dump_path[0])
        return;
    typio_logger_dump_recent(g_active_app->recent_log_dump_path);
}

bool typiod_app_init(TypiodApp *app,
                           const TypioInstanceConfig *config,
                           bool verbose,
                           char *argv[]) {
    TypioInstanceConfig instance_config = {};
    TypioResult result;

    if (!app) {
        return false;
    }

    memset(app, 0, sizeof(*app));
    app->argv = argv;

    if (config) {
        instance_config = *config;
    }

    /*
     * Initialise the logger before creating the instance so we capture the
     * "Initializing Typio instance" trace.  Per libtypio's ABI:
     *   1. typio_logger_init() — wires libtypio into the `log` crate
     *   2. typio_logger_set_callback() — forwards records to this host
     *   3. typio_logger_set_level() — gate everything below this level
     */
    typio_logger_init();
    typio_logger_set_callback(typiod_log_callback, app);
    typio_logger_set_level(verbose ? TYPIO_LOG_DEBUG : TYPIO_LOG_INFO);

    app->instance = typio_instance_new_with_config(&instance_config);
    if (!app->instance) {
        typio_log_error("Failed to create Typio instance");
        return false;
    }

    result = typio_instance_init(app->instance);
    if (result != TYPIO_OK) {
        typio_log_error("Failed to initialize Typio instance: %d", result);
        typio_instance_free(app->instance);
        app->instance = nullptr;
        return false;
    }

    app->state_controller = typio_state_controller_new(app->instance);
    if (!app->state_controller) {
        typio_log_error("Failed to create state controller");
        typio_instance_free(app->instance);
        app->instance = nullptr;
        return false;
    }

    typiod_configure_recent_log_dump(app);

    return true;
}

static void typiod_list_engines_of_kind(TypioRegistry *registry,
                                        char **engines,
                                        size_t count,
                                        const char *kind_label) {
    for (size_t i = 0; i < count; i++) {
        const TypioEngineInfo *info =
            typio_registry_get_engine_info(registry, engines[i]);

        if (!info) {
            continue;
        }

        printf("  %s\n", info->name);
        printf("    Display name: %s\n", info->display_name ? info->display_name : "");
        printf("    Description:  %s\n", info->description ? info->description : "");
        printf("    Author:       %s\n", info->author ? info->author : "");
        printf("    Type:         %s\n", kind_label);
        printf("    Language:     %s\n", info->language ? info->language : "");
        printf("\n");
        typio_engine_info_free((TypioEngineInfo *)info);
    }
}

void typiod_app_list_engines(TypiodApp *app) {
    TypioRegistry *registry;
    size_t kb_count = 0;
    size_t voice_count = 0;
    char **keyboards;
    char **voices;

    if (!app || !app->instance) {
        printf("No engine registry available\n");
        return;
    }

    registry = typio_instance_get_registry(app->instance);
    if (!registry) {
        printf("No engine registry available\n");
        return;
    }

    keyboards = typio_registry_list_keyboards(registry, &kb_count);
    voices = typio_registry_list_voices(registry, &voice_count);

    printf("Available engines (%zu keyboards, %zu voice):\n\n",
           kb_count, voice_count);

    typiod_list_engines_of_kind(registry, keyboards, kb_count, "Keyboard");
    typiod_list_engines_of_kind(registry, voices, voice_count, "Voice");

    typio_free_string_array(keyboards, kb_count);
    typio_free_string_array(voices, voice_count);
}

static void typiod_init_ipc_bus(TypiodApp *app) {
    app->ipc_bus = typio_ipc_bus_new(app->instance);
    if (app->ipc_bus) {
        typio_ipc_bus_set_stop_callback(app->ipc_bus,
                                         typiod_request_stop,
                                         app);
        if (app->state_controller) {
            typio_ipc_bus_bind_state_controller(app->ipc_bus,
                                                 app->state_controller);
        }
        typio_log_info("IPC bus initialized");
    } else {
        typio_log_warning("IPC bus not available");
    }
}

static void typiod_init_tray(TypiodApp *app) {
#ifdef HAVE_SYSTRAY
    TypioTrayConfig tray_config = {
        .icon_name = "typio-keyboard-off-symbolic",
        .tooltip = "Typio Input Method",
        .menu_callback = typiod_tray_menu_callback,
        .user_data = app,
    };

    app->tray = typio_tray_new(app->instance, &tray_config);
    if (app->tray && typio_tray_is_registered(app->tray)) {
        typiod_update_tray_engine_status(app);
        typio_log_info("System tray initialized");
    } else if (app->tray) {
        typiod_update_tray_engine_status(app);
        typio_log_info("System tray pending (StatusNotifierWatcher not running yet)");
    } else {
        typio_log_warning("System tray not available (StatusNotifierWatcher may not be running)");
    }
#else
    (void) app;
#endif
}

static void typiod_destroy_runtime_services(TypiodApp *app) {
#ifdef HAVE_SYSTRAY
    if (app->tray) {
        typio_tray_destroy(app->tray);
        app->tray = nullptr;
    }
#endif
    if (app->ipc_bus) {
        typio_ipc_bus_destroy(app->ipc_bus);
        app->ipc_bus = nullptr;
    }
}

static int typiod_run_wayland(TypiodApp *app) {
#ifdef HAVE_WAYLAND
    int wl_result;
    const char *wl_error;

    typio_instance_set_engine_changed_callback(app->instance,
                                               typiod_on_engine_change,
                                               app);
    typio_instance_set_voice_engine_changed_callback(app->instance,
                                                     typiod_on_voice_engine_change,
                                                     app);
    typio_instance_set_status_icon_changed_callback(app->instance,
                                                    typiod_on_status_icon_change,
                                                    app);
    typio_instance_set_mode_changed_callback(app->instance,
                                              typiod_on_mode_change,
                                              app);

    if (app->state_controller) {
        typio_state_controller_add_listener(
            app->state_controller,
            (TypioStateListener){ .user_data = app,
                                  .callback = typiod_on_state_changed });
        typio_state_controller_sync(app->state_controller);
    }

    app->wl_frontend = typio_wl_frontend_new(app->instance, nullptr);
    if (!app->wl_frontend) {
        typio_log_error("Failed to create Wayland frontend");
        typio_log_error("Make sure the session provides zwp_input_method_manager_v2 and a working text-input-v3 path");
        return 1;
    }

#ifdef HAVE_SYSTRAY
    if (app->tray) {
        typio_wl_frontend_set_tray(app->wl_frontend, app->tray);
    }
#endif
    if (app->ipc_bus) {
        typio_wl_frontend_set_ipc_bus(app->wl_frontend, app->ipc_bus);
    }

    typio_log_info("Wayland input method frontend started");

    wl_result = typio_wl_frontend_run(app->wl_frontend);
    wl_error = typio_wl_frontend_get_error(app->wl_frontend);
    if (wl_error) {
        typio_log_error("Wayland error: %s", wl_error);
    }

    typio_wl_frontend_destroy(app->wl_frontend);
    app->wl_frontend = nullptr;

    return wl_result < 0 ? 1 : 0;
#else
    (void) app;
    typio_log_error("This build does not include the Wayland frontend.");
    typio_log_error("Reconfigure with ENABLE_WAYLAND=ON to run Typio.");
    return 1;
#endif
}

int typiod_app_run(TypiodApp *app) {
    int exit_code;

    if (!app || !app->instance) {
        return 1;
    }

    typiod_install_signal_handlers(app);
    typiod_print_startup_banner(app);
    typiod_init_ipc_bus(app);
    typiod_init_tray(app);
#ifdef HAVE_SYSTRAY
    const char *engine_icon_path = typiod_plugin_discovered_icon_theme_path();
    if (engine_icon_path && app->tray) {
        typio_tray_set_icon_theme_path(app->tray, engine_icon_path);
    }
#endif
    exit_code = typiod_run_wayland(app);

    if (exit_code == 0) {
        typio_log_info("Shutting down...");
    }

    return exit_code;
}

void typiod_app_shutdown(TypiodApp *app) {
    if (!app) {
        return;
    }

    if (g_active_app == app) {
        g_active_app = nullptr;
    }

    typiod_destroy_runtime_services(app);
    if (app->state_controller) {
        typio_state_controller_free(app->state_controller);
        app->state_controller = nullptr;
    }
    if (app->instance) {
        typio_instance_free(app->instance);
        app->instance = nullptr;
    }
}

int typiod_app_finish(TypiodApp *app, int exit_code) {
    if (!app) {
        return exit_code;
    }

    if (app->shutdown_requested_by_signal) {
        int sig = (int)app->shutdown_signal;

        typio_log_warning("Shutdown requested by signal: signal=%d (%s)",
                          sig,
                          typiod_signal_name(sig));
    }

    if ((exit_code != 0 || app->shutdown_requested_by_signal) &&
        app->recent_log_dump_path[0] != '\0') {
        typio_logger_dump_recent(app->recent_log_dump_path);
    }

    if (app->restart_requested && exit_code == 0) {
        typio_log_info("Restarting...");
        execv(app->argv[0], app->argv);
        perror("execv");
        return 1;
    }

    if (exit_code == 0) {
        typio_log_info("Goodbye!");
    }

    return exit_code;
}

#ifdef TYPIO_DAEMON_TEST
void typiod_test_update_tray_engine_status(TypiodApp *app) {
#ifdef HAVE_SYSTRAY
    typiod_update_tray_engine_status(app);
#else
    (void)app;
#endif
}

void typiod_test_on_engine_change(TypioInstance *instance,
                                        const TypioEngineInfo *engine,
                                        void *user_data) {
    typiod_on_engine_change(instance, engine, user_data);
}

void typiod_test_on_voice_engine_change(TypioInstance *instance,
                                              const TypioEngineInfo *engine,
                                              void *user_data) {
    typiod_on_voice_engine_change(instance, engine, user_data);
}

void typiod_test_on_status_icon_change(TypioInstance *instance,
                                             const char *icon_name,
                                             void *user_data) {
    typiod_on_status_icon_change(instance, icon_name, user_data);
}
#endif
