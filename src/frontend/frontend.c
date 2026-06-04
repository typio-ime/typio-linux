/**
 * @file frontend.c
 * @brief Wayland input method frontend — lifecycle (new, destroy, stop, reconnect).
 *
 * This file owns the public TypioWlFrontend object: its allocation and
 * cross-cutting resources (sub-struct alloc/free, config watch, resume
 * signal, voice service, IPC bus wiring, reconnect orchestration). The
 * Wayland-specific bind/unbind and the IPC runtime-state serialization
 * live in sibling files (frontend_bind.c, frontend_runtime.c) so this
 * file is the right place to read when asking "what does the daemon do
 * at startup, teardown, and on disconnect".
 */

#include "frontend.h"
#include "frontend/foreign/identity.h"
#include "input/policy/tracker.h"
#include "monotonic.h"
#include "backoff.h"
#include "internal.h"
#include "panel.h"
#include "aux_adapters.h"
#include "ipc/ipc_bus.h"
#include "typio/typio.h"
#include "typio/abi/log.h"
#include "typio/abi/string.h"
#include "typio/runtime/registry.h"

#include <time.h>
#ifdef HAVE_VOICE
#include "typio/abi/voice.h"
#include "../voice/pw_capture.h"
#endif

#include <wayland-client.h>
#include <inttypes.h>
#include <signal.h>
#include <errno.h>
#include <sys/inotify.h>
#include <sys/timerfd.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>

/* Extern from frontend_bind.c: declared there so this file is the only
 * place that needs the public lifecycle API. */
bool typio_wl_frontend_wayland_bind(TypioWlFrontend *frontend);
void typio_wl_frontend_wayland_unbind(TypioWlFrontend *frontend);

/* ── Helpers ───────────────────────────────────────────────────────────── */

static TypioWlFrontend *frontend_init_failed(TypioWlFrontend *frontend,
                                             const char *message) {
    if (!frontend) return nullptr;
    if (message) {
        snprintf(frontend->error_msg, sizeof(frontend->error_msg), "%s", message);
    }
    typio_wl_frontend_destroy(frontend);
    return nullptr;
}

static void frontend_setup_config_watch(TypioWlFrontend *frontend) {
    char engines_dir[512];
    const char *config_dir;

    if (!frontend || !frontend->instance) {
        return;
    }

    frontend->config->watch_fd = -1;
    frontend->config->dir_watch = -1;
    frontend->config->engines_watch = -1;
    frontend->config->reload_timer_fd = -1;
    frontend->config->reload_pending = false;

    config_dir = typio_instance_get_config_dir(frontend->instance);
    if (!config_dir || !*config_dir) {
        return;
    }

    frontend->config->watch_fd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (frontend->config->watch_fd < 0) {
        typio_log_warning("Failed to initialize configuration watch");
        return;
    }

    frontend->config->reload_timer_fd = timerfd_create(CLOCK_MONOTONIC,
                                                      TFD_NONBLOCK | TFD_CLOEXEC);
    if (frontend->config->reload_timer_fd < 0) {
        typio_log_warning("Failed to initialize configuration reload debounce timer");
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

/* ── Resume signal ─────────────────────────────────────────────────────── */

static void frontend_on_resume(void *user_data, const char *reason,
                               uint64_t sleep_ms) {
    TypioWlFrontend *frontend = user_data;
    (void)reason;
    (void)sleep_ms;
    if (!frontend)
        return;
    /* The per-tick driver in event_loop.c clears session_facts at the start
     * of every iteration; a fact recorded here is consumed by the same tick
     * once the resume-signal fd has been polled and reduce() runs. */
    frontend->session_facts.suspend_gap_detected = true;
}

static void frontend_init_resume_signal(TypioWlFrontend *frontend) {
    if (!frontend)
        return;

    frontend->resume_signal =
        typio_wl_resume_signal_create(frontend_on_resume, frontend);
    if (!frontend->resume_signal) {
        typio_log_warning("Failed to create resume signal detector");
        return;
    }

    if (frontend->aux_handler_count <
        sizeof(frontend->aux_handlers) / sizeof(frontend->aux_handlers[0])) {
        TypioWlAuxHandler *h =
            typio_wl_aux_handler_for_resume_signal(frontend->resume_signal);
        if (h)
            frontend->aux_handlers[frontend->aux_handler_count++] = h;
    }
}

/* ── Voice service (HAVE_VOICE) ────────────────────────────────────────── */

#ifdef HAVE_VOICE
static void frontend_init_voice(TypioWlFrontend *frontend, TypioInstance *instance);

static void pw_audio_cb(const float *samples, size_t count, void *user_data) {
    typio_voice_session_feed_audio((TypioVoiceSession *)user_data, samples, count);
}

static void voice_event_cb(const TypioVoiceSessionEvent *event, void *user_data) {
    TypioWlFrontend *frontend = user_data;
    switch (event->type) {
    case TYPIO_VOICE_EVENT_STATE_CHANGE:
        switch (event->state) {
        case TYPIO_VOICE_STATE_LOADING:
            typio_wl_panel_coordinator_show_status(frontend,
                                    TYPIO_WL_UI_OWNER_VOICE,
                                    "[Loading voice model...]");
            break;
        case TYPIO_VOICE_STATE_RECORDING:
            typio_wl_panel_coordinator_show_status(frontend,
                                    TYPIO_WL_UI_OWNER_VOICE,
                                    "[Recording...]");
            break;
        case TYPIO_VOICE_STATE_PROCESSING:
            typio_wl_panel_coordinator_show_status(frontend,
                                    TYPIO_WL_UI_OWNER_VOICE,
                                    "[Processing...]");
            break;
        case TYPIO_VOICE_STATE_IDLE:
        default:
            typio_wl_panel_coordinator_hide(frontend, TYPIO_WL_UI_OWNER_VOICE);
            break;
        }
        break;
    case TYPIO_VOICE_EVENT_RESULT:
        if (event->text && event->text[0]) {
            typio_log_info("Voice raw: \"%s\"", event->text);
            typio_voice_filter_tags_inplace(event->text);
            const char *p = event->text;
            while (*p == ' ') p++;
            if (*p != '\0') {
                typio_log_info("Voice result: \"%s\"", p);
                if (frontend->session && frontend->session->ctx) {
                    typio_input_context_commit(frontend->session->ctx, p);
                }
            }
            typio_free_string(event->text);
        }
        break;
    case TYPIO_VOICE_EVENT_ERROR:
        typio_wl_panel_coordinator_show_status(frontend,
                                TYPIO_WL_UI_OWNER_VOICE,
                                event->error);
        break;
    }
}
#endif

/* ── Public lifecycle API ──────────────────────────────────────────────── */

TypioWlFrontend *typio_wl_frontend_new(TypioInstance *instance,
                                        const TypioWlFrontendConfig *config) {
    if (!instance) {
        return nullptr;
    }

    TypioWlFrontend *frontend = calloc(1, sizeof(TypioWlFrontend));
    if (!frontend) {
        return nullptr;
    }

    frontend->panel_coord = calloc(1, sizeof(TypioWlPanelCoordinator));
    if (!frontend->panel_coord) {
        free(frontend);
        return nullptr;
    }
    frontend->panel_coord->frontend = frontend;

    frontend->vk = calloc(1, sizeof(TypioWlVirtualKeyboard));
    if (!frontend->vk) {
        free(frontend->panel_coord);
        free(frontend);
        return nullptr;
    }
    frontend->vk->frontend = frontend;

    frontend->watchdog = calloc(1, sizeof(TypioWlWatchdog));
    if (!frontend->watchdog) {
        free(frontend->vk);
        free(frontend->panel_coord);
        free(frontend);
        return nullptr;
    }
    frontend->watchdog->frontend = frontend;

    frontend->tracker = calloc(1, sizeof(TypioWlKeyTracker));
    if (!frontend->tracker) {
        free(frontend->watchdog);
        free(frontend->vk);
        free(frontend->panel_coord);
        free(frontend);
        return nullptr;
    }
    frontend->tracker->frontend = frontend;

    frontend->indicator = calloc(1, sizeof(TypioWlIndicator));
    if (!frontend->indicator) {
        free(frontend->tracker);
        free(frontend->watchdog);
        free(frontend->vk);
        free(frontend->panel_coord);
        free(frontend);
        return nullptr;
    }
    frontend->indicator->frontend = frontend;
    frontend->indicator->timer_fd = -1;

    frontend->config = calloc(1, sizeof(TypioWlConfigWatcher));
    if (!frontend->config) {
        free(frontend->indicator);
        free(frontend->tracker);
        free(frontend->watchdog);
        free(frontend->vk);
        free(frontend->panel_coord);
        free(frontend);
        return nullptr;
    }
    frontend->config->frontend = frontend;
    frontend->config->watch_fd = -1;
    frontend->config->dir_watch = -1;
    frontend->config->engines_watch = -1;
    frontend->config->reload_timer_fd = -1;

    frontend->instance = instance;
    /* Default to PREPARING: do NOT query engine availability eagerly during
     * init. Third-party engine plugins may be buggy or slow to initialise;
     * calling into their vtable here could crash the daemon. Instead, rely
     * on the push-based availability callback to transition to READY when
     * the engine finishes warm-up. The key router already handles
     * ENGINE_NOT_READY by consuming keys silently. */
    frontend->keyboard_availability = TYPIO_ENGINE_PREPARING;

    /* Load shortcut bindings from config */
    typio_shortcut_config_load(&frontend->shortcuts,
                               typio_instance_get_config(instance));
    typio_wl_frontend_log_shortcuts(frontend, "Shortcuts:");

    /* Remember the display name so the reconnect path can re-open it. */
    const char *display_name = config ? config->display_name : nullptr;
    frontend->display_name = display_name ? typio_strdup(display_name) : nullptr;

    /* Bind all Wayland-derived objects. */
    if (!typio_wl_frontend_wayland_bind(frontend)) {
        return frontend_init_failed(frontend, frontend->error_msg);
    }

    frontend->identity_provider = typio_wl_identity_provider_new(instance);

    typio_log_info("Wayland input method frontend initialized");
    frontend_setup_config_watch(frontend);
    frontend_init_resume_signal(frontend);
    typio_wl_frontend_init_indicator(frontend);
#ifdef HAVE_VOICE
    frontend_init_voice(frontend, instance);
#endif
    return frontend;
}

void typio_wl_frontend_set_keyboard_availability(TypioWlFrontend *frontend,
                                                  TypioEngineAvailability availability,
                                                  const char *reason) {
    if (!frontend) return;
    if (frontend->keyboard_availability == availability &&
        (reason == nullptr ||
         strcmp(frontend->keyboard_availability_reason, reason) == 0)) {
        return;
    }
    frontend->keyboard_availability = availability;
    if (reason) {
        snprintf(frontend->keyboard_availability_reason,
                 sizeof(frontend->keyboard_availability_reason),
                 "%s", reason);
    } else {
        frontend->keyboard_availability_reason[0] = '\0';
    }
    typio_wl_frontend_emit_runtime_state_changed(frontend);
}

void typio_wl_frontend_stop(TypioWlFrontend *frontend) {
    if (frontend) {
        frontend->running = false;
    }
}

bool typio_wl_frontend_reconnect(TypioWlFrontend *frontend) {
    uint32_t attempt = 0;

    if (!frontend)
        return false;

    typio_log_warning("Wayland connection lost; attempting to reconnect");

    /* No event-loop progress happens during the blocking backoff, so park
     * the watchdog. Destroying the keyboard in unbind also disarms it; this
     * is belt-and-suspenders for the case where no grab existed. */
    atomic_store(&frontend->watchdog->armed, false);

    /* Drop every Wayland-derived object. Engine/session state, aux handlers,
     * config watch, and the resume signal are intentionally preserved, so an
     * in-flight composition survives the compositor restart. */
    typio_wl_frontend_wayland_unbind(frontend);

    /* Reset the input-method state to a clean disconnected baseline. A held
     * key during the outage produced no key-up, so fence the key generation
     * and clear tracking; the fresh grab after reconnect starts from zero. */
    frontend->tracker->active_generation_owned_keys = false;
    if (frontend->vk) frontend->vk->carried_modifiers = false;
    frontend->tracker->active_generation++;
    if (frontend->tracker->active_generation == 0)
        frontend->tracker->active_generation = 1;
    typio_wl_key_tracking_reset(frontend->tracker->states, TYPIO_WL_MAX_TRACKED_KEYS);
    typio_wl_key_tracking_reset_generations(frontend->tracker->generations,
                                            TYPIO_WL_MAX_TRACKED_KEYS);

    while (typio_wl_reconnect_should_retry(attempt)) {
        uint32_t delay_ms = typio_wl_reconnect_delay_ms(attempt);
        struct timespec ts = {
            .tv_sec = delay_ms / 1000,
            .tv_nsec = (long)(delay_ms % 1000) * 1000000L,
        };

        /* Honor a stop request that arrived before/while we were down. */
        if (!frontend->running)
            return false;

        nanosleep(&ts, nullptr);

        if (!frontend->running)
            return false;

        if (typio_wl_frontend_wayland_bind(frontend)) {
            typio_log_info("Reconnected to Wayland display after %u attempt(s)",
                      attempt + 1);
            /* The fatal path that called us may have cleared running; the
             * connection is healthy again, so resume the loop. */
            frontend->running = true;
            return true;
        }

        /* Bind left partial state; clear it before the next attempt. */
        typio_wl_frontend_wayland_unbind(frontend);
        attempt++;
    }

    typio_log_error("Failed to reconnect to Wayland display after %u attempts; exiting",
              attempt);
    return false;
}

bool typio_wl_frontend_is_running(TypioWlFrontend *frontend) {
    return frontend && frontend->running;
}

void typio_wl_frontend_destroy(TypioWlFrontend *frontend) {
    if (!frontend) {
        return;
    }

    frontend->running = false;
    typio_wl_frontend_watchdog_stop(frontend);

    /* Clean up session */
    if (frontend->session) {
        typio_wl_session_destroy(frontend->session);
        frontend->session = nullptr;
    }
    typio_wl_frontend_clear_identity(frontend);
    typio_wl_identity_provider_free(frontend->identity_provider);
    frontend->identity_provider = nullptr;

    /* Clean up keyboard */
    if (frontend->keyboard) {
        typio_wl_keyboard_destroy(frontend->keyboard);
        frontend->keyboard = nullptr;
    }

    if (frontend->panel) {
        typio_panel_destroy(frontend->panel);
        frontend->panel = nullptr;
    }

    if (frontend->config->dir_watch >= 0 && frontend->config->watch_fd >= 0) {
        inotify_rm_watch(frontend->config->watch_fd, frontend->config->dir_watch);
    }
    if (frontend->config->engines_watch >= 0 && frontend->config->watch_fd >= 0) {
        inotify_rm_watch(frontend->config->watch_fd, frontend->config->engines_watch);
    }
    if (frontend->config->watch_fd >= 0) {
        close(frontend->config->watch_fd);
        frontend->config->watch_fd = -1;
    }
    if (frontend->config->reload_timer_fd >= 0) {
        close(frontend->config->reload_timer_fd);
        frontend->config->reload_timer_fd = -1;
    }
    typio_wl_frontend_destroy_indicator(frontend);

    /* Clean up optional subsystems */
#ifdef HAVE_VOICE
    {
        TypioVoiceSession *voice = typio_instance_get_voice_session(frontend->instance);
        if (voice) {
            /* Detach audio source (daemon-owned) before freeing session. */
            typio_voice_session_set_audio_source(voice, NULL);
            typio_voice_session_free(voice);
            typio_instance_set_voice_session(frontend->instance, NULL);
        }
    }
#endif
    for (size_t i = 0; i < frontend->aux_handler_count; i++) {
        typio_wl_aux_handler_free(frontend->aux_handlers[i]);
        frontend->aux_handlers[i] = nullptr;
    }
    frontend->aux_handler_count = 0;

    /* The resume-signal aux handler wrapper is freed above, but it does not
     * own the detector itself (free_fn was null), so release it here. */
    if (frontend->resume_signal) {
        typio_wl_resume_signal_destroy(frontend->resume_signal);
        frontend->resume_signal = nullptr;
    }

    /* Clean up all Wayland-derived objects (keyboard already destroyed
     * above; unbind tolerates the NULL). */
    typio_wl_frontend_wayland_unbind(frontend);

    free(frontend->display_name);

    typio_log_info("Wayland frontend destroyed");
    free(frontend->panel_coord);
    free(frontend->vk);
    free(frontend->watchdog);
    free(frontend->tracker);
    free(frontend->indicator);
    free(frontend->config);
    free(frontend);
}

const char *typio_wl_frontend_get_error(TypioWlFrontend *frontend) {
    if (!frontend || frontend->error_msg[0] == '\0') {
        return nullptr;
    }
    return frontend->error_msg;
}

void typio_wl_frontend_set_tray([[maybe_unused]] TypioWlFrontend *frontend,
                                [[maybe_unused]] void *tray) {
#ifdef HAVE_SYSTRAY
    if (!frontend || !tray) return;
    size_t cap = sizeof(frontend->aux_handlers) / sizeof(frontend->aux_handlers[0]);
    if (frontend->aux_handler_count >= cap) return;
    TypioWlAuxHandler *h = typio_wl_aux_handler_for_tray((TypioTray *)tray);
    if (h) frontend->aux_handlers[frontend->aux_handler_count++] = h;
#endif
}

void typio_wl_frontend_set_ipc_bus(TypioWlFrontend *frontend, void *ipc_bus) {
    /* The runtime-state callback lives in frontend_runtime.c as a static
     * function. The cast to the public callback type is the established
     * pattern in this file (no need to expose the symbol). */
    extern void frontend_fill_runtime_state(void *, TypioIpcBusRuntimeState *);
    if (!frontend || !ipc_bus) return;
    frontend->ipc_bus = (struct TypioIpcBus *)ipc_bus;
    typio_ipc_bus_set_runtime_state_callback(frontend->ipc_bus,
                                              frontend_fill_runtime_state,
                                              frontend);
    size_t cap = sizeof(frontend->aux_handlers) / sizeof(frontend->aux_handlers[0]);
    if (frontend->aux_handler_count >= cap) return;
    TypioWlAuxHandler *h =
        typio_wl_aux_handler_for_ipc_bus((struct TypioIpcBus *)ipc_bus);
    if (h) frontend->aux_handlers[frontend->aux_handler_count++] = h;
}

#ifdef HAVE_VOICE
static void frontend_init_voice(TypioWlFrontend *frontend, TypioInstance *instance) {
    TypioVoiceSession *voice = typio_voice_session_new(instance);
    TypioPwCapture *capture = nullptr;

    if (voice) {
        capture = typio_pw_capture_new(pw_audio_cb, voice);
        if (capture) {
            typio_voice_session_set_audio_source(voice,
                                                 typio_pw_capture_as_audio_source(capture));
        }
        typio_voice_session_set_callback(voice, voice_event_cb, frontend);
        typio_instance_set_voice_session(instance, voice);
    }
    if (voice && typio_voice_session_is_available(voice))
        typio_log_info("Voice input service ready");
    else if (voice)
        typio_log_info("Voice input service created but no model");
    else
        typio_log_warning("Failed to create voice input service");

    if (voice) {
        size_t cap = sizeof(frontend->aux_handlers) / sizeof(frontend->aux_handlers[0]);
        if (frontend->aux_handler_count < cap) {
            TypioWlAuxHandler *h = typio_wl_aux_handler_for_voice(voice, frontend);
            if (h)
                frontend->aux_handlers[frontend->aux_handler_count++] = h;
        }
    }
}
#endif
