#include "internal.h"
#include "ui/panel/panel.h"
#include "typio/runtime/instance.h"
#include "typio/abi/config.h"
#include "typio/abi/log.h"

#include <sys/timerfd.h>
#include <unistd.h>
#include <string.h>
#include <errno.h>

#define TYPIO_INDICATOR_DEFAULT_DURATION_MS 1500

static int indicator_duration_ms(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->instance) {
        return TYPIO_INDICATOR_DEFAULT_DURATION_MS;
    }
    TypioConfig *cfg = typio_instance_get_config(frontend->instance);
    if (!cfg) {
        return TYPIO_INDICATOR_DEFAULT_DURATION_MS;
    }
    int ms = typio_config_get_int(cfg, "display.indicator_duration_ms",
                                  TYPIO_INDICATOR_DEFAULT_DURATION_MS);
    typio_config_free(cfg);
    if (ms < 100) ms = 100;
    if (ms > 10000) ms = 10000;
    return ms;
}

static bool indicator_enabled(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->instance) {
        return true;
    }
    TypioConfig *cfg = typio_instance_get_config(frontend->instance);
    if (!cfg) {
        return true;
    }
    bool enabled = typio_config_get_bool(cfg, "display.indicator_enabled", true);
    typio_config_free(cfg);
    return enabled;
}

bool typio_wl_frontend_init_indicator(TypioWlFrontend *frontend) {
    if (!frontend) return false;
    frontend->indicator_timer_fd = timerfd_create(CLOCK_MONOTONIC,
                                                   TFD_CLOEXEC | TFD_NONBLOCK);
    if (frontend->indicator_timer_fd < 0) {
        typio_log_warning("Failed to create indicator timer: %s",
                          strerror(errno));
        return false;
    }
    frontend->indicator_active = false;
    return true;
}

void typio_wl_frontend_show_indicator(TypioWlFrontend *frontend,
                                       const char *text) {
    struct itimerspec its;

    if (!frontend || !text || !text[0]) return;
    if (!indicator_enabled(frontend)) return;
    if (!frontend->panel) return;

    typio_panel_show_status(frontend->panel, text);
    frontend->indicator_active = true;

    memset(&its, 0, sizeof(its));
    int ms = indicator_duration_ms(frontend);
    its.it_value.tv_sec = ms / 1000;
    its.it_value.tv_nsec = (long)(ms % 1000) * 1000000L;

    if (frontend->indicator_timer_fd >= 0) {
        timerfd_settime(frontend->indicator_timer_fd, 0, &its, NULL);
    }
}

void typio_wl_frontend_hide_indicator(TypioWlFrontend *frontend) {
    if (!frontend) return;
    if (frontend->indicator_active && frontend->panel) {
        typio_panel_hide_status(frontend->panel);
    }
    frontend->indicator_active = false;
}

int typio_wl_frontend_get_indicator_fd(TypioWlFrontend *frontend) {
    return frontend ? frontend->indicator_timer_fd : -1;
}

void typio_wl_frontend_dispatch_indicator_timer(TypioWlFrontend *frontend) {
    uint64_t expirations;

    if (!frontend || frontend->indicator_timer_fd < 0) return;

    if (read(frontend->indicator_timer_fd, &expirations, sizeof(expirations)) < 0) {
        return;
    }
    typio_wl_frontend_hide_indicator(frontend);
}

void typio_wl_frontend_destroy_indicator(TypioWlFrontend *frontend) {
    if (!frontend) return;
    if (frontend->indicator_timer_fd >= 0) {
        close(frontend->indicator_timer_fd);
        frontend->indicator_timer_fd = -1;
    }
    frontend->indicator_active = false;
}
