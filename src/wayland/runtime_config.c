#include "internal.h"
#include "panel.h"
#ifdef HAVE_FLUX
#include "text_shaper.h"
#endif

#ifdef HAVE_VOICE
#include "typio/abi/voice.h"
#endif

#include "typio/abi/config.h"
#include "typio/abi/engine.h"
#include "typio/abi/string.h"
#include "typio/runtime/registry.h"
#include "typio/typio.h"
#include "typio/abi/log.h"

#include <string.h>
#include <stdlib.h>
#include <sys/inotify.h>
#include <sys/timerfd.h>
#include <unistd.h>

#define TYPIO_CONFIG_RELOAD_DEBOUNCE_MS 100

void typio_wl_frontend_log_shortcuts(TypioWlFrontend *frontend,
                                     const char *prefix) {
    char *switch_engine;
    char *emergency_exit;
    char *voice_ptt;

    if (!frontend || !prefix) {
        return;
    }

    switch_engine = typio_shortcut_format(&frontend->shortcuts.switch_engine);
    emergency_exit = typio_shortcut_format(&frontend->shortcuts.emergency_exit);
    voice_ptt = typio_shortcut_format(&frontend->shortcuts.voice_ptt);
    typio_log_info("%s switch_engine=%s exit=%s voice_ptt=%s",
                   prefix,
                   switch_engine ? switch_engine : "(none)",
                   emergency_exit ? emergency_exit : "(none)",
                   voice_ptt ? voice_ptt : "(none)");
    typio_free_string(switch_engine);
    typio_free_string(emergency_exit);
    typio_free_string(voice_ptt);
}

static void runtime_config_refresh(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->instance) {
        return;
    }

    typio_log_debug("Config reload: begin");
    if (typio_instance_reload_config(frontend->instance) != TYPIO_OK) {
        typio_log_warning("Config reload: failed to reload instance config");
        return;
    }

    /* Fontconfig caches grow monotonically over a long session.  Purge them
     * whenever the user explicitly reloads config so memory use stays bounded
     * and any font installation changes are picked up. */
#ifdef HAVE_FLUX
    typio_text_shaper_purge_font_caches();
#endif

    typio_panel_invalidate_config(frontend->panel);

    TypioConfig *config = typio_instance_get_config(frontend->instance);
    typio_shortcut_config_load(&frontend->shortcuts, config);
    typio_wl_frontend_log_shortcuts(frontend, "Config reload: shortcuts");

#ifdef HAVE_VOICE
    {
        TypioRegistry *registry = typio_instance_get_registry(frontend->instance);
        const char *configured_voice = typio_config_get_string(config,
                                                                "voice.engine",
                                                                NULL);
        if (registry && configured_voice && *configured_voice) {
            char *cur = typio_registry_get_active_voice(registry);
            if (!cur || strcmp(configured_voice, cur) != 0) {
                TypioResult r =
                    typio_registry_set_active_voice(registry, configured_voice);
                if (r != TYPIO_OK) {
                    typio_log_warning(
                        "Config reload: voice engine '%s' not found",
                        configured_voice);
                }
            }
            typio_free_string(cur);
        }

        {
            TypioVoiceSession *voice =
                typio_instance_get_voice_session(frontend->instance);
            if (voice) {
                typio_voice_session_reload_engine(voice);
                typio_log_info("Config reload: voice available=%s",
                               typio_voice_session_is_available(voice) ? "yes" : "no");
            }
        }
    }
#endif

    /* Subscribed clients see a config.changed event via the IPC bus
     * state-controller listener; nothing to emit here. */

    typio_log_debug("Config reload: complete");
}

static void runtime_config_rearm_watch(TypioWlFrontend *frontend) {
    char engines_dir[512];
    const char *config_dir;

    if (!frontend || frontend->config->watch_fd < 0 || !frontend->instance) {
        return;
    }

    config_dir = typio_instance_get_config_dir(frontend->instance);
    if (!config_dir || !*config_dir) {
        return;
    }

    if (frontend->config->dir_watch >= 0) {
        inotify_rm_watch(frontend->config->watch_fd, frontend->config->dir_watch);
        frontend->config->dir_watch = -1;
    }
    if (frontend->config->engines_watch >= 0) {
        inotify_rm_watch(frontend->config->watch_fd, frontend->config->engines_watch);
        frontend->config->engines_watch = -1;
    }

    frontend->config->dir_watch = inotify_add_watch(frontend->config->watch_fd,
                                                   config_dir,
                                                   IN_CLOSE_WRITE | IN_MOVED_TO |
                                                   IN_CREATE | IN_DELETE |
                                                   IN_DELETE_SELF | IN_MOVE_SELF |
                                                   IN_ATTRIB);
    if (snprintf(engines_dir, sizeof(engines_dir), "%s/engines", config_dir) <
        (int)sizeof(engines_dir)) {
        frontend->config->engines_watch = inotify_add_watch(frontend->config->watch_fd,
                                                           engines_dir,
                                                           IN_CLOSE_WRITE | IN_MOVED_TO |
                                                           IN_CREATE | IN_DELETE |
                                                           IN_DELETE_SELF | IN_MOVE_SELF |
                                                           IN_ATTRIB);
    }
}

static void runtime_config_schedule_reload(TypioWlFrontend *frontend) {
    struct itimerspec timer = {};

    if (!frontend) {
        return;
    }

    frontend->config->reload_pending = true;
    if (frontend->config->reload_timer_fd < 0) {
        runtime_config_refresh(frontend);
        frontend->config->reload_pending = false;
        return;
    }

    timer.it_value.tv_sec = TYPIO_CONFIG_RELOAD_DEBOUNCE_MS / 1000;
    timer.it_value.tv_nsec =
        (long)(TYPIO_CONFIG_RELOAD_DEBOUNCE_MS % 1000) * 1000000L;
    if (timerfd_settime(frontend->config->reload_timer_fd, 0, &timer, NULL) != 0) {
        typio_log_warning(
            "Config reload: failed to arm debounce timer; reloading immediately");
        runtime_config_refresh(frontend);
        frontend->config->reload_pending = false;
    }
}

/*
 * Only these files in the config directory should trigger a reload. Watching
 * the directory (required to catch atomic-rename saves) also surfaces editor
 * swap/backup files (`.core.toml.swp`, `core.toml~`, `.#core.toml`, …) and any
 * other churn; matching the final name keeps those out. Atomic-rename saves
 * land as IN_MOVED_TO carrying the destination name, so matching the final
 * name covers both in-place writes and temp-file-plus-rename saves.
 */
static bool config_event_is_relevant(const char *name, uint32_t len) {
    if (len == 0) {
        return false;
    }
    return strcmp(name, "core.toml") == 0 ||
           strcmp(name, "platform.toml") == 0;
}

void typio_wl_frontend_handle_config_watch(TypioWlFrontend *frontend) {
    char buffer[4096] __attribute__((aligned(__alignof__(struct inotify_event))));
    ssize_t nread;
    bool should_reload = false;
    bool should_rearm = false;

    if (!frontend || frontend->config->watch_fd < 0) {
        return;
    }

    const uint32_t relevant_mask = IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE |
                                   IN_DELETE | IN_DELETE_SELF | IN_MOVE_SELF |
                                   IN_ATTRIB;

    while ((nread = read(frontend->config->watch_fd, buffer, sizeof(buffer))) > 0) {
        ssize_t offset = 0;

        while (offset < nread) {
            const struct inotify_event *event =
                (const struct inotify_event *)(buffer + offset);

            if ((event->mask & (IN_DELETE_SELF | IN_MOVE_SELF)) != 0) {
                /* The watched directory itself was removed or moved: re-add the
                 * watch and reload, since its contents may have changed. */
                should_rearm = true;
                should_reload = true;
            } else if ((event->mask & relevant_mask) != 0) {
                if (event->wd == frontend->config->engines_watch) {
                    /* Engine manifest directory: keep the broad trigger. */
                    should_reload = true;
                } else if (config_event_is_relevant(event->name, event->len)) {
                    should_reload = true;
                }
            }
            offset += (ssize_t)sizeof(struct inotify_event) + event->len;
        }
    }

    if (should_rearm) {
        runtime_config_rearm_watch(frontend);
    }
    if (should_reload) {
        runtime_config_schedule_reload(frontend);
    }
}

int typio_wl_frontend_get_config_reload_fd(TypioWlFrontend *frontend) {
    return frontend ? frontend->config->reload_timer_fd : -1;
}

void typio_wl_frontend_dispatch_config_reload(TypioWlFrontend *frontend) {
    uint64_t expirations;

    if (!frontend || frontend->config->reload_timer_fd < 0) {
        return;
    }

    if (read(frontend->config->reload_timer_fd,
             &expirations,
             sizeof(expirations)) < 0) {
        return;
    }

    if (!frontend->config->reload_pending) {
        return;
    }

    frontend->config->reload_pending = false;
    runtime_config_refresh(frontend);
}
