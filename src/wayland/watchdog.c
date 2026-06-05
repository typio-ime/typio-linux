/**
 * @file watchdog.c
 * @brief Watchdog thread and stage tracking for Wayland frontend
 */

#include "internal.h"
#include "trace.h"
#include "clock.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* While armed, the watchdog samples loop progress at this interval. The stuck
 * threshold is much larger, so a coarse sample keeps wakeups low (≈1 Hz when
 * actively typing, zero when idle) without meaningfully delaying detection. */
#define TYPIO_WL_WATCHDOG_SAMPLE_MS 1000
#define TYPIO_WL_WATCHDOG_STUCK_MS  3000

static const char *stage_name(TypioWlLoopStage stage) {
    switch (stage) {
    case TYPIO_WL_LOOP_STAGE_IDLE:              return "idle";
    case TYPIO_WL_LOOP_STAGE_PANEL_UPDATE:      return "panel_update";
    case TYPIO_WL_LOOP_STAGE_PREPARE_READ:      return "prepare_read";
    case TYPIO_WL_LOOP_STAGE_FLUSH:             return "flush";
    case TYPIO_WL_LOOP_STAGE_POLL:              return "poll";
    case TYPIO_WL_LOOP_STAGE_READ_EVENTS:       return "read_events";
    case TYPIO_WL_LOOP_STAGE_DISPATCH_PENDING:  return "dispatch_pending";
    case TYPIO_WL_LOOP_STAGE_AUX_IO:            return "aux_io";
    case TYPIO_WL_LOOP_STAGE_REPEAT:            return "repeat";
    case TYPIO_WL_LOOP_STAGE_CONFIG_RELOAD:     return "config_reload";
    default:                                    return "unknown";
    }
}

void typio_wl_frontend_watchdog_heartbeat(TypioWlFrontend *frontend) {
    if (!frontend) return;
    uint64_t now = typio_wl_monotonic_ms();
    atomic_store(&frontend->watchdog->heartbeat_ms, now);
    atomic_store(&frontend->watchdog->stage_since_ms, now);
}

void typio_wl_frontend_watchdog_set_stage(TypioWlFrontend *frontend,
                                           TypioWlLoopStage stage) {
    if (!frontend) return;
    atomic_store(&frontend->watchdog->loop_stage, (int)stage);
    atomic_store(&frontend->watchdog->stage_since_ms, typio_wl_monotonic_ms());
}

void typio_wl_frontend_watchdog_stage_done(TypioWlFrontend *frontend) {
    if (!frontend) return;
    typio_wl_frontend_watchdog_heartbeat(frontend);
    typio_wl_frontend_watchdog_set_stage(frontend, TYPIO_WL_LOOP_STAGE_IDLE);
}

void typio_wl_frontend_watchdog_set_armed(TypioWlFrontend *frontend, bool armed) {
    if (!frontend || !frontend->watchdog) return;
    /* Hold the lock across the store + signal so a concurrent cond_wait in the
     * watchdog thread cannot miss the transition (no lost wakeup). */
    pthread_mutex_lock(&frontend->watchdog->lock);
    atomic_store(&frontend->watchdog->armed, armed);
    pthread_cond_signal(&frontend->watchdog->cond);
    pthread_mutex_unlock(&frontend->watchdog->lock);
}

/* POLL (blocked on fds) and IDLE (between work stages) are legitimate waiting
 * states, never hangs. Excluding them lets the main loop block indefinitely
 * when idle without the watchdog mistaking quiescence for a stall. Only work
 * stages are expected to make progress within the stuck threshold. */
static bool stage_is_restful(int stage) {
    return stage == TYPIO_WL_LOOP_STAGE_POLL ||
           stage == TYPIO_WL_LOOP_STAGE_IDLE;
}

static uint32_t runtime_age_ms(uint64_t now_ms, uint64_t start_ms) {
    return (now_ms >= start_ms) ? (uint32_t)(now_ms - start_ms) : 0;
}

static int32_t runtime_deadline_remaining_ms(uint64_t now_ms,
                                              uint64_t deadline_ms) {
    if (deadline_ms == 0) return -1;
    return (deadline_ms > now_ms) ? (int32_t)(deadline_ms - now_ms) : 0;
}

static void *watchdog_thread(void *data) {
    TypioWlFrontend *frontend = data;
    uint64_t last_heartbeat_ms = 0;
    int last_stage = TYPIO_WL_LOOP_STAGE_IDLE;
    uint64_t last_stage_since_ms = 0;

    typio_log_debug("Watchdog thread started");

    pthread_mutex_lock(&frontend->watchdog->lock);
    while (!atomic_load(&frontend->watchdog->stop)) {
        /* Disarmed (no focused input): block until armed or stopped. The daemon
         * is quiescent here, so the watchdog causes zero wakeups. */
        while (!atomic_load(&frontend->watchdog->armed) &&
               !atomic_load(&frontend->watchdog->stop)) {
            pthread_cond_wait(&frontend->watchdog->cond, &frontend->watchdog->lock);
        }
        if (atomic_load(&frontend->watchdog->stop)) {
            break;
        }

        /* Armed: sample at a coarse interval, but wake immediately if disarmed
         * or stopped via the condition variable. */
        struct timespec deadline;
        clock_gettime(CLOCK_MONOTONIC, &deadline);
        deadline.tv_nsec += (long)TYPIO_WL_WATCHDOG_SAMPLE_MS * 1000000L;
        deadline.tv_sec += deadline.tv_nsec / 1000000000L;
        deadline.tv_nsec %= 1000000000L;
        pthread_cond_timedwait(&frontend->watchdog->cond, &frontend->watchdog->lock,
                               &deadline);

        if (atomic_load(&frontend->watchdog->stop)) {
            break;
        }
        if (!atomic_load(&frontend->watchdog->armed)) {
            continue;
        }

        uint64_t heartbeat_ms = atomic_load(&frontend->watchdog->heartbeat_ms);
        int stage = atomic_load(&frontend->watchdog->loop_stage);
        uint64_t stage_since_ms = atomic_load(&frontend->watchdog->stage_since_ms);

        bool unchanged = heartbeat_ms == last_heartbeat_ms &&
                         stage == last_stage &&
                         stage_since_ms == last_stage_since_ms;
        if (unchanged && !stage_is_restful(stage)) {
            uint64_t now = typio_wl_monotonic_ms();
            uint32_t stuck_ms = runtime_age_ms(now, heartbeat_ms);
            /* The vk fields below are read without synchronisation. This is a
             * deliberate best-effort diagnostic on the fatal SIGKILL path: a
             * torn read at worst garbles one log line, which does not justify
             * making the whole virtual-keyboard state machine atomic. */
            int32_t deadline_remaining = runtime_deadline_remaining_ms(
                now, frontend->vk ? frontend->vk->keymap_deadline_ms : 0);

            if (stuck_ms >= TYPIO_WL_WATCHDOG_STUCK_MS) {
                typio_log_error("Watchdog: loop stuck for %u ms in stage=%s "
                    "vk_state=%s vk_deadline=%d ms age=%u ms",
                    stuck_ms, stage_name(stage),
                    typio_wl_vk_state_name(frontend->vk ? frontend->vk->state
                                                        : TYPIO_WL_VK_STATE_ABSENT),
                    deadline_remaining,
                    runtime_age_ms(now, frontend->vk ? frontend->vk->state_since_ms
                                                     : 0));
                kill(getpid(), SIGKILL);
                break;
            }
        }

        last_heartbeat_ms = heartbeat_ms;
        last_stage = stage;
        last_stage_since_ms = stage_since_ms;
    }
    pthread_mutex_unlock(&frontend->watchdog->lock);

    typio_log_debug("Watchdog thread stopped");
    return nullptr;
}

void typio_wl_frontend_watchdog_start(TypioWlFrontend *frontend) {
    if (!frontend || frontend->watchdog->thread_started) return;

    atomic_store(&frontend->watchdog->stop, false);
    atomic_store(&frontend->watchdog->armed, false);
    frontend->watchdog->heartbeat_ms = 0;
    frontend->watchdog->stage_since_ms = 0;
    frontend->watchdog->loop_stage = TYPIO_WL_LOOP_STAGE_IDLE;

    pthread_mutex_init(&frontend->watchdog->lock, nullptr);
    pthread_condattr_t cattr;
    pthread_condattr_init(&cattr);
    /* Match clock_gettime(CLOCK_MONOTONIC) used for cond_timedwait deadlines so
     * wall-clock jumps (NTP, suspend/resume) cannot skew the sample interval. */
    pthread_condattr_setclock(&cattr, CLOCK_MONOTONIC);
    pthread_cond_init(&frontend->watchdog->cond, &cattr);
    pthread_condattr_destroy(&cattr);

    if (pthread_create(&frontend->watchdog->thread, nullptr,
                       watchdog_thread, frontend) != 0) {
        typio_log_warning("Failed to start watchdog thread");
        pthread_mutex_destroy(&frontend->watchdog->lock);
        pthread_cond_destroy(&frontend->watchdog->cond);
        return;
    }
    frontend->watchdog->thread_started = true;
}

void typio_wl_frontend_watchdog_stop(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->watchdog->thread_started) return;

    pthread_mutex_lock(&frontend->watchdog->lock);
    atomic_store(&frontend->watchdog->stop, true);
    pthread_cond_signal(&frontend->watchdog->cond);
    pthread_mutex_unlock(&frontend->watchdog->lock);

    pthread_join(frontend->watchdog->thread, nullptr);
    frontend->watchdog->thread_started = false;

    pthread_mutex_destroy(&frontend->watchdog->lock);
    pthread_cond_destroy(&frontend->watchdog->cond);
}
