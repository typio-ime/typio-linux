/**
 * @file event_loop.c
 * @brief Wayland event loop with optional-subsystem strategy pattern.
 *
 * @see docs/explanation/timing-model.md
 * @see docs/explanation/control-surfaces.md
 * @see ADR-004: Event-loop scheduling and watchdog design
 */

#include "internal.h"

#include "frontend_aux.h"
#include "monotonic.h"
#include "panel.h"
#include "reconciler.h"
#include "state.h"
#include "trace.h"
#include "typio/abi/input_context.h"
#include "typio/abi/log.h"

#include <errno.h>
#include <inttypes.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>

typedef struct TypioWlLoopAuxFds {
    int display_fd;
    /* Legacy per-subsystem fds are now queried on-demand from aux_handlers */
} TypioWlLoopAuxFds;

static void event_loop_flush_pending_panel(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->session) {
        return;
    }

    if (frontend->positioned_ui_pending) {
        uint64_t now = typio_wl_monotonic_ms();
        int timeout_ms = typio_wl_panel_coordinator_anchor_timeout_ms(frontend);
        TypioWlPositionedUiPlan ui_plan =
            typio_wl_positioned_ui_plan(
                frontend->positioned_ui_pending,
                typio_wl_panel_coordinator_anchor_ready(frontend),
                frontend->positioned_ui_pending_since_ms,
                now,
                (uint64_t)timeout_ms);

        /* Probe timed out, but the compositor already positioned our popup at a
         * caret for this focus: clients like terminals set the cursor rectangle
         * once and do not re-emit it on the no-op probe commit, so the probe
         * never yields a fresh rect — yet the cached position is valid. Trust it
         * instead of dropping the UI. Browsers answer the probe before the
         * timeout and take the plain SHOW path. */
        bool caret_fallback = ui_plan == TYPIO_WL_POSITIONED_UI_CANCEL &&
                              frontend->position_anchor_has_caret;

        if (ui_plan == TYPIO_WL_POSITIONED_UI_SHOW || caret_fallback) {
            if (caret_fallback) {
                typio_wl_panel_coordinator_mark_anchor_ready(frontend,
                                                             "caret_rect_fallback");
            }
            if (frontend->positioned_ui_pending_owner ==
                TYPIO_WL_UI_OWNER_INDICATOR) {
                char label[TYPIO_POSITIONED_UI_LABEL_CAP];
                snprintf(label, sizeof(label), "%s",
                         frontend->positioned_ui_pending_label);
                typio_wl_panel_coordinator_cancel_pending(frontend);
                typio_wl_frontend_show_indicator(frontend, label);
            } else {
                typio_wl_panel_coordinator_flush_pending(frontend);
            }
        } else if (ui_plan == TYPIO_WL_POSITIONED_UI_CANCEL) {
            TypioWlUiOwner owner = frontend->positioned_ui_pending_owner;
            typio_wl_panel_coordinator_cancel_pending(frontend);
            typio_wl_trace(frontend,
                           "ui",
                           "action=skip owner=%d reason=position_anchor_timeout timeout_ms=%u generation=%" PRIu64,
                           owner,
                           (unsigned)timeout_ms,
                           frontend->position_anchor_generation);
        }
    }

    if (!typio_wl_panel_scheduler_should_flush(
            frontend->panel_schedule_state,
            true,
            frontend->session->ctx != nullptr,
            frontend->session->ctx &&
                typio_input_context_is_focused(frontend->session->ctx))) {
        return;
    }

    typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_PANEL_UPDATE);
    typio_wl_session_flush_scheduled_ui_update(frontend->session);
    typio_wl_frontend_watchdog_heartbeat(frontend);
    typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
}

static int event_loop_prepare_and_flush(TypioWlFrontend *frontend) {
    while (wl_display_prepare_read(frontend->display) != 0) {
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_PREPARE_READ);
        wl_display_dispatch_pending(frontend->display);
        typio_wl_frontend_watchdog_heartbeat(frontend);
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
    }

    typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_FLUSH);
    if (wl_display_flush(frontend->display) < 0 && errno != EAGAIN) {
        typio_log_error("Wayland display flush failed: %s",
                  strerror(errno));
        wl_display_cancel_read(frontend->display);
        /* Connection lost — let the caller drive reconnect rather than
         * forcing shutdown. Don't clear running here. */
        return -1;
    }

    typio_wl_frontend_watchdog_heartbeat(frontend);
    return 0;
}

static TypioWlLoopAuxFds event_loop_init_aux_fds(TypioWlFrontend *frontend) {
    TypioWlLoopAuxFds aux = {
        .display_fd = wl_display_get_fd(frontend->display),
    };
    (void)frontend;
    return aux;
}

/**
 * @brief Build the pollfd array from Wayland display + aux handlers + internal fds.
 *
 * On return @c aux_indices[i] contains the pollfd index for frontend->aux_handlers[i],
 * or -1 if that handler reported no fd.
 */
static int event_loop_poll(TypioWlFrontend *frontend,
                           TypioWlLoopAuxFds *aux,
                           struct pollfd *fds,
                           int *idx_display,
                           int *idx_repeat,
                           int *idx_config,
                           int *idx_config_reload,
                           int *idx_indicator,
                           int *aux_indices,
                           size_t aux_indices_capacity) {
    int nfds = 0;

    *idx_display = nfds;
    fds[nfds++] = (struct pollfd){ .fd = aux->display_fd, .events = POLLIN };

    /* Poll optional aux handlers (status bus, tray, voice, etc.) */
    for (size_t i = 0; i < frontend->aux_handler_count && i < aux_indices_capacity; i++) {
        TypioWlAuxHandler *h = frontend->aux_handlers[i];
        int fd = h ? h->get_fd(h) : -1;
        if (fd >= 0) {
            aux_indices[i] = nfds;
            fds[nfds++] = (struct pollfd){ .fd = fd, .events = POLLIN };
        } else {
            aux_indices[i] = -1;
        }
    }

    *idx_repeat = -1;
    if (frontend->keyboard) {
        int repeat_fd = typio_wl_keyboard_get_repeat_fd(frontend->keyboard);
        if (repeat_fd >= 0) {
            *idx_repeat = nfds;
            fds[nfds++] = (struct pollfd){ .fd = repeat_fd, .events = POLLIN };
        }
    }

    *idx_config = -1;
    if (frontend->config_watch_fd >= 0) {
        *idx_config = nfds;
        fds[nfds++] = (struct pollfd){ .fd = frontend->config_watch_fd, .events = POLLIN };
    }

    *idx_config_reload = -1;
    {
        int config_reload_fd = typio_wl_frontend_get_config_reload_fd(frontend);
        if (config_reload_fd >= 0) {
            *idx_config_reload = nfds;
            fds[nfds++] = (struct pollfd){ .fd = config_reload_fd, .events = POLLIN };
        }
    }

    *idx_indicator = -1;
    {
        int indicator_fd = typio_wl_frontend_get_indicator_fd(frontend);
        if (indicator_fd >= 0) {
            *idx_indicator = nfds;
            fds[nfds++] = (struct pollfd){ .fd = indicator_fd, .events = POLLIN };
        }
    }

    typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_POLL);
    int timeout_ms = 100;
    bool panel_flushable = typio_wl_panel_scheduler_should_flush(
        frontend->panel_schedule_state,
        frontend->session != nullptr,
        frontend->session && frontend->session->ctx != nullptr,
        frontend->session && frontend->session->ctx &&
            typio_input_context_is_focused(frontend->session->ctx));
    timeout_ms = typio_wl_panel_scheduler_poll_timeout_ms(
        frontend->panel_schedule_state,
        panel_flushable,
        timeout_ms);
    if (frontend->virtual_keyboard_state == TYPIO_WL_VK_STATE_NEEDS_KEYMAP &&
        frontend->virtual_keyboard_keymap_deadline_ms > 0) {
        uint64_t now_ms = typio_wl_monotonic_ms();
        int deadline_ms = frontend->virtual_keyboard_keymap_deadline_ms <= now_ms
                              ? 0
                              : (int)(frontend->virtual_keyboard_keymap_deadline_ms - now_ms);
        if (deadline_ms < timeout_ms) {
            timeout_ms = deadline_ms;
        }
    }
    return poll(fds, (nfds_t)nfds, timeout_ms);
}

static int event_loop_handle_wayland(TypioWlFrontend *frontend,
                                     const struct pollfd *display_fd) {
    if (display_fd->revents & POLLIN) {
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_READ_EVENTS);
        if (wl_display_read_events(frontend->display) < 0) {
            typio_log_error("Failed to read Wayland events: %s",
                      strerror(errno));
            /* Connection lost — caller drives reconnect. */
            return -1;
        }
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_DISPATCH_PENDING);
        wl_display_dispatch_pending(frontend->display);
        frontend->dispatch_epoch++;
        typio_wl_frontend_watchdog_heartbeat(frontend);
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
    } else {
        wl_display_cancel_read(frontend->display);
    }

    if (display_fd->revents & (POLLERR | POLLHUP)) {
        typio_log_error("Wayland display connection error");
        /* Connection lost — caller drives reconnect. */
        return -1;
    }

    return 0;
}

/**
 * @brief Dispatch every registered TypioWlAuxHandler whose fd became readable.
 */
static void event_loop_handle_aux_handlers(TypioWlFrontend *frontend,
                                            struct pollfd *fds,
                                            const int *aux_indices) {
    for (size_t i = 0; i < frontend->aux_handler_count; i++) {
        int idx = aux_indices[i];
        if (idx < 0) continue;

        struct pollfd *pfd = &fds[idx];
        if (!pfd->revents) continue;

        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_AUX_IO);
        TypioWlAuxHandler *h = frontend->aux_handlers[i];
        if (pfd->revents & (POLLERR | POLLHUP | POLLNVAL)) {
            typio_log_warning("Aux handler '%s' disabled after poll error (revents=0x%x)",
                      h ? h->name : "?", pfd->revents);
        } else if (pfd->revents & POLLIN) {
            if (h && h->on_ready) {
                h->on_ready(h);
            }
        }
        typio_wl_frontend_watchdog_heartbeat(frontend);
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
    }
}

/* Drive reconnect after a lost display. On success the display fd has
 * changed, so refresh the cached aux fds and tell the caller to continue the
 * loop. On failure (gave up or stopped) clear running so the loop exits. */
static bool event_loop_recover(TypioWlFrontend *frontend, TypioWlLoopAuxFds *aux) {
    if (typio_wl_frontend_reconnect(frontend)) {
        *aux = event_loop_init_aux_fds(frontend);
        return true;
    }
    frontend->running = false;
    return false;
}

int typio_wl_frontend_run(TypioWlFrontend *frontend) {
    TypioWlLoopAuxFds aux;
    int aux_indices[6];

    if (!frontend || !frontend->display) {
        return -1;
    }

    typio_wl_frontend_watchdog_start(frontend);
    frontend->running = true;
    typio_wl_frontend_watchdog_heartbeat(frontend);
    typio_log_info("Starting Wayland event loop");
    aux = event_loop_init_aux_fds(frontend);

    while (frontend->running) {
        struct pollfd fds[12];
        int idx_display;
        int idx_repeat;
        int idx_config;
        int idx_config_reload;
        int idx_indicator;
        int ret;

        typio_wl_frontend_watchdog_heartbeat(frontend);
        typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);

        /* Detect a suspend/resume gap before doing any work this turn.
         * Cheap (two clock reads); the callback path scrubs stale keyboard
         * state and rebuilds the grab when the kernel woke us up. */
        typio_wl_resume_signal_tick(frontend->resume_signal);

        /* Reconcile our believed phase against observed reality. Catches
         * the cases the resume signal misses: a compositor that silently
         * dropped our grab without a deactivate, leaving us wedged. */
        typio_wl_reconcile_tick(frontend);

        event_loop_flush_pending_panel(frontend);

        if (event_loop_prepare_and_flush(frontend) < 0) {
            if (event_loop_recover(frontend, &aux))
                continue;
            return -1;
        }

        ret = event_loop_poll(frontend, &aux, fds, &idx_display, &idx_repeat,
                              &idx_config, &idx_config_reload, &idx_indicator,
                              aux_indices,
                              sizeof(aux_indices) / sizeof(aux_indices[0]));

        if (ret < 0) {
            if (errno == EINTR) {
                wl_display_cancel_read(frontend->display);
                continue;
            }
            typio_log_error("poll failed: %s", strerror(errno));
            wl_display_cancel_read(frontend->display);
            frontend->running = false;
            return -1;
        }

        if (ret == 0) {
            wl_display_cancel_read(frontend->display);
            typio_wl_vk_health_check(frontend);
            typio_wl_frontend_watchdog_heartbeat(frontend);
            continue;
        }

        if (event_loop_handle_wayland(frontend, &fds[idx_display]) < 0) {
            if (event_loop_recover(frontend, &aux))
                continue;
            return -1;
        }

        /* Dispatch optional subsystems through the generic aux-handler path */
        event_loop_handle_aux_handlers(frontend, fds, aux_indices);

        if (idx_repeat >= 0 && (fds[idx_repeat].revents & POLLIN)) {
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_REPEAT);
            typio_wl_keyboard_dispatch_repeat(frontend->keyboard);
            typio_wl_frontend_watchdog_heartbeat(frontend);
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
        }

        if (idx_config >= 0 && (fds[idx_config].revents & POLLIN)) {
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_CONFIG_RELOAD);
            typio_wl_frontend_handle_config_watch(frontend);
            typio_wl_frontend_watchdog_heartbeat(frontend);
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
        }

        if (idx_config_reload >= 0 && (fds[idx_config_reload].revents & POLLIN)) {
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_CONFIG_RELOAD);
            typio_wl_frontend_dispatch_config_reload(frontend);
            typio_wl_frontend_watchdog_heartbeat(frontend);
            typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
        }

        if (idx_indicator >= 0 && (fds[idx_indicator].revents & POLLIN)) {
            typio_wl_frontend_dispatch_indicator_timer(frontend);
        }

        typio_wl_vk_health_check(frontend);
        if (!frontend->running) {
            break;
        }
        typio_wl_frontend_watchdog_heartbeat(frontend);
    }

    typio_log_info("Wayland event loop stopped");
    return 0;
}
