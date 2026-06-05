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

#ifdef HAVE_WAYLAND
#include "wayland/internal.h"
#include "ui/state.h"
#endif

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static TypioApp *g_active_app = nullptr;

static const char *typio_app_build_display_string(void) {
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
static void typio_update_tray_engine_status(TypioApp *app);
#endif

static const char *typio_signal_name(int sig) {
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

static void typio_signal_handler(int sig) {
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

static void typio_log_callback(const TypioLogEvent *event,
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

static void typio_request_stop(void *user_data) {
    TypioApp *app = user_data;

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
static void typio_update_tray_tooltip(TypioApp *app) {
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

    /* Surface the active profile (e.g. Rime schema name) on the keyboard line
     * so the schema is visible in the UI panel and tray tooltip. */
    const char *profile_label = nullptr;
    if (app->state_controller) {
        const TypioKeyboardEngineMode *mode =
            typio_state_controller_get_current_status(app->state_controller);
        if (mode && mode->label && mode->label[0]) {
            profile_label = mode->label;
        }
    }

    if (profile_label) {
        snprintf(description, sizeof(description),
                 "Keyboard: %s (%s)\nVoice: %s",
                 keyboard_label,
                 profile_label,
                 voice_label);
    } else {
        snprintf(description, sizeof(description),
                 "Keyboard: %s\nVoice: %s",
                 keyboard_label,
                 voice_label);
    }
    typio_tray_set_tooltip(app->tray, "Typio", description);

    if (keyboard_label_owned) {
        typio_free_string((char *)keyboard_label);
    }
    if (voice_label_owned) {
        typio_free_string((char *)voice_label);
    }
}
#endif

static void typio_print_startup_banner(TypioApp *app) {
    TypioRegistry *registry;
    char *kb_name;
    char *voice_name;

    typio_log_info("Starting %s", typio_app_build_display_string());
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
static void typio_update_tray_engine_status(TypioApp *app) {
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
        typio_update_tray_tooltip(app);
        typio_free_string(engine_icon);
        typio_free_string(active_name);
        return;
    }

    typio_tray_set_icon(app->tray, icon_name);
    typio_tray_update_engine(app->tray, engine_name, is_active);
    typio_update_tray_tooltip(app);
}
#endif

static void typio_on_mode_change(TypioInstance *instance,
                                  const TypioKeyboardEngineMode *mode,
                                  void *user_data) {
    TypioApp *app = user_data;
    TypioRegistry *registry;

    if (app && app->instance) {
        char *name;
        registry = typio_instance_get_registry(app->instance);
        name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
#ifdef HAVE_WAYLAND
        if (app->wl_frontend && name && mode && mode->id && mode->id[0]) {
            typio_wl_frontend_remember_active_mode(app->wl_frontend,
                                                   name,
                                                   mode->id);
        }
        if (app->wl_frontend && mode && mode->display_label && mode->display_label[0]) {
            typio_wl_frontend_show_indicator_for_state(app->wl_frontend, mode);
        }
#endif
        typio_free_string(name);
    }

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_status_changed(app->state_controller, mode);
    }
}

static void typio_on_status_icon_change(TypioInstance *instance,
                                        const char *icon_name,
                                        void *user_data) {
    TypioApp *app = user_data;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_status_icon_changed(app->state_controller,
                                                          icon_name);
    }
}

static void typio_on_engine_availability_change(TypioInstance *instance,
                                                TypioEngineAvailability availability,
                                                const char *reason,
                                                void *user_data) {
    TypioApp *app = user_data;

    (void)instance;

#ifdef HAVE_WAYLAND
    if (app && app->wl_frontend) {
        typio_wl_frontend_set_keyboard_availability(app->wl_frontend,
                                                    availability,
                                                    reason);
    }
#else
    (void)app;
    (void)availability;
    (void)reason;
#endif
}

static void typio_on_engine_change(TypioInstance *instance,
                                   const TypioEngineInfo *engine,
                                   void *user_data) {
    TypioApp *app = user_data;
    TypioRegistry *registry;
    char *active_name;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_engine_changed(app->state_controller, engine);
    }

    if (!app || !app->instance) {
        return;
    }

    registry = typio_instance_get_registry(app->instance);
    active_name = registry ? typio_registry_get_active_keyboard(registry) : nullptr;
    if (active_name) {
#ifdef HAVE_WAYLAND
        if (app && app->wl_frontend) {
            TypioWlFrontend *fe = app->wl_frontend;
            /* Safety net: clear stale composition UI from any engine switch
             * path (tray menu, IPC, etc.) that did not pre-clean like the
             * arbiter does.  Idempotent when the arbiter already cleared. */
            typio_wl_set_preedit(fe, "", -1, -1);
            typio_wl_commit(fe);
            typio_wl_panel_coordinator_hide(fe, TYPIO_WL_UI_OWNER_CANDIDATE);
            if (fe->session) {
                typio_wl_session_cancel_ui_tracking(fe->session);
            }
            typio_wl_frontend_remember_active_engine(app->wl_frontend,
                                                     active_name);
            typio_wl_frontend_show_indicator_for_state(app->wl_frontend, nullptr);
        }
#endif
        typio_log_info("Engine changed to: %s", active_name);
        typio_free_string(active_name);
    }
}

static void typio_on_voice_engine_change(TypioInstance *instance,
                                                 const TypioEngineInfo *engine,
                                                 void *user_data) {
    TypioApp *app = user_data;

    (void) instance;

    if (app && app->state_controller) {
        typio_state_controller_notify_voice_engine_changed(app->state_controller,
                                                           engine);
    }
    if (engine && engine->name) {
        typio_log_info("Voice engine changed to: %s", engine->name);
    }
}

#ifdef HAVE_SYSTRAY
static void typio_tray_menu_callback([[maybe_unused]] TypioTray *tray,
                                            const char *action,
                                            void *user_data) {
    TypioApp *app = user_data;
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

static void typio_install_signal_handlers(TypioApp *app) {
    g_active_app = app;
    signal(SIGINT, typio_signal_handler);
    signal(SIGTERM, typio_signal_handler);
}

bool typio_app_init(TypioApp *app,
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
    typio_logger_set_callback(typio_log_callback, app);
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

    return true;
}

static void typio_init_ipc_bus(TypioApp *app) {
    app->ipc_bus = typio_ipc_bus_new(app->instance);
    if (app->ipc_bus) {
        typio_ipc_bus_set_stop_callback(app->ipc_bus,
                                         typio_request_stop,
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

static void typio_init_tray(TypioApp *app) {
#ifdef HAVE_SYSTRAY
    TypioTrayConfig tray_config = {
        .icon_name = "typio-keyboard-off-symbolic",
        .tooltip = "Typio Input Method",
        .menu_callback = typio_tray_menu_callback,
        .user_data = app,
    };

    app->tray = typio_tray_new(app->instance, &tray_config);
    if (app->tray && app->state_controller) {
        typio_tray_bind_state_controller(app->tray, app->state_controller);
    }
    if (app->tray && typio_tray_is_registered(app->tray)) {
        typio_update_tray_engine_status(app);
        typio_log_info("System tray initialized");
    } else if (app->tray) {
        typio_update_tray_engine_status(app);
        typio_log_info("System tray pending (StatusNotifierWatcher not running yet)");
    } else {
        typio_log_warning("System tray not available (StatusNotifierWatcher may not be running)");
    }
#else
    (void) app;
#endif
}

static void typio_destroy_runtime_services(TypioApp *app) {
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

static int typio_run_wayland(TypioApp *app) {
#ifdef HAVE_WAYLAND
    int wl_result;
    const char *wl_error;

    typio_instance_set_engine_changed_callback(app->instance,
                                               typio_on_engine_change,
                                               app);
    typio_instance_set_voice_engine_changed_callback(app->instance,
                                                     typio_on_voice_engine_change,
                                                     app);
    typio_instance_set_status_icon_changed_callback(app->instance,
                                                    typio_on_status_icon_change,
                                                    app);
    typio_instance_set_keyboard_mode_changed_callback(app->instance,
                                              typio_on_mode_change,
                                              app);
    typio_instance_set_engine_availability_changed_callback(
        app->instance,
        typio_on_engine_availability_change,
        app);

    if (app->state_controller) {
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

int typio_app_run(TypioApp *app) {
    int exit_code;

    if (!app || !app->instance) {
        return 1;
    }

    typio_install_signal_handlers(app);
    typio_print_startup_banner(app);
    typio_init_ipc_bus(app);
    typio_init_tray(app);
#ifdef HAVE_SYSTRAY
    const char *engine_icon_path = typio_plugin_discovered_icon_theme_path();
    if (engine_icon_path && app->tray) {
        typio_tray_set_icon_theme_path(app->tray, engine_icon_path);
    }
#endif
    exit_code = typio_run_wayland(app);

    if (exit_code == 0) {
        typio_log_info("Shutting down...");
    }

    return exit_code;
}

void typio_app_shutdown(TypioApp *app) {
    if (!app) {
        return;
    }

    if (g_active_app == app) {
        g_active_app = nullptr;
    }

    typio_destroy_runtime_services(app);
    if (app->state_controller) {
        typio_state_controller_free(app->state_controller);
        app->state_controller = nullptr;
    }
    if (app->instance) {
        typio_instance_free(app->instance);
        app->instance = nullptr;
    }
}

int typio_app_finish(TypioApp *app, int exit_code) {
    if (!app) {
        return exit_code;
    }

    if (app->shutdown_requested_by_signal) {
        int sig = (int)app->shutdown_signal;

        typio_log_warning("Shutdown requested by signal: signal=%d (%s)",
                          sig,
                          typio_signal_name(sig));
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
void typio_test_update_tray_engine_status(TypioApp *app) {
#ifdef HAVE_SYSTRAY
    typio_update_tray_engine_status(app);
#else
    (void)app;
#endif
}

void typio_test_on_engine_change(TypioInstance *instance,
                                        const TypioEngineInfo *engine,
                                        void *user_data) {
    typio_on_engine_change(instance, engine, user_data);
}

void typio_test_on_voice_engine_change(TypioInstance *instance,
                                              const TypioEngineInfo *engine,
                                              void *user_data) {
    typio_on_voice_engine_change(instance, engine, user_data);
}

void typio_test_on_status_icon_change(TypioInstance *instance,
                                             const char *icon_name,
                                             void *user_data) {
    typio_on_status_icon_change(instance, icon_name, user_data);
}
#endif
