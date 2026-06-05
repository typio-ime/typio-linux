#ifndef TYPIO_APP_H
#define TYPIO_APP_H

#include "typio_build_config.h"
#include "typio/runtime/instance.h"

#include <signal.h>

#ifdef HAVE_WAYLAND
#include "wayland/frontend.h"
#endif

#include "state/controller.h"

#ifdef HAVE_SYSTRAY
#include "tray/tray.h"
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioApp {
    TypioInstance *instance;
    TypioStateController *state_controller;
    char **argv;
    bool restart_requested;
    bool shutdown_requested_by_signal;
    volatile sig_atomic_t shutdown_signal;
#ifdef HAVE_WAYLAND
    TypioWlFrontend *wl_frontend;
#endif
    struct TypioIpcBus *ipc_bus;
#ifdef HAVE_SYSTRAY
    TypioTray *tray;
#endif
} TypioApp;

bool typio_app_init(TypioApp *app,
                           const TypioInstanceConfig *config,
                           bool verbose,
                           char *argv[]);
int typio_app_run(TypioApp *app);
void typio_app_shutdown(TypioApp *app);
int typio_app_finish(TypioApp *app, int exit_code);


#ifdef TYPIO_DAEMON_TEST
void typio_test_update_tray_engine_status(TypioApp *app);
void typio_test_on_engine_change(TypioInstance *instance,
                                        const TypioEngineInfo *engine,
                                        void *user_data);
void typio_test_on_voice_engine_change(TypioInstance *instance,
                                              const TypioEngineInfo *engine,
                                              void *user_data);
void typio_test_on_status_icon_change(TypioInstance *instance,
                                             const char *icon_name,
                                             void *user_data);
#endif

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_APP_H */
