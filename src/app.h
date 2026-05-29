#ifndef TYPIO_DAEMON_APP_H
#define TYPIO_DAEMON_APP_H

#include "typio_build_config.h"
#include "typio/runtime/instance.h"

#include <signal.h>

#ifdef HAVE_WAYLAND
#include "frontend/frontend.h"
#endif

#include "state/controller.h"

#ifdef HAVE_SYSTRAY
#include "tray/tray.h"
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypiodApp {
    TypioInstance *instance;
    TypioStateController *state_controller;
    char **argv;
    bool restart_requested;
    bool shutdown_requested_by_signal;
    volatile sig_atomic_t shutdown_signal;
    char recent_log_dump_path[1024];
#ifdef HAVE_WAYLAND
    TypioWlFrontend *wl_frontend;
#endif
    struct TypioIpcBus *ipc_bus;
#ifdef HAVE_SYSTRAY
    TypioTray *tray;
#endif
} TypiodApp;

bool typiod_app_init(TypiodApp *app,
                           const TypioInstanceConfig *config,
                           bool verbose,
                           char *argv[]);
void typiod_app_list_engines(TypiodApp *app);
int typiod_app_run(TypiodApp *app);
void typiod_app_shutdown(TypiodApp *app);
int typiod_app_finish(TypiodApp *app, int exit_code);


#ifdef TYPIO_DAEMON_TEST
void typiod_test_update_tray_engine_status(TypiodApp *app);
void typiod_test_on_engine_change(TypioInstance *instance,
                                        const TypioEngineInfo *engine,
                                        void *user_data);
void typiod_test_on_voice_engine_change(TypioInstance *instance,
                                              const TypioEngineInfo *engine,
                                              void *user_data);
void typiod_test_on_status_icon_change(TypioInstance *instance,
                                             const char *icon_name,
                                             void *user_data);
#endif

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_DAEMON_APP_H */
