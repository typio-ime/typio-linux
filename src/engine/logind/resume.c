/**
 * @file resume.c
 * @brief System-resume detector implementation
 */

#include "resume.h"

#include "clock.h"
#include "resume_model.h"
#include "typio_build_config.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <stdlib.h>
#include <string.h>

#ifdef HAVE_LIBDBUS
#  include <systemd/sd-bus.h>
#endif

/*
 * A boottime/monotonic delta below this is treated as normal scheduler
 * jitter (debugger pauses, very long stop-the-world GCs in dependent
 * libraries, brief container freezes). Above it, we conclude the kernel
 * suspended the process. 2000 ms is comfortably larger than any
 * legitimate single-tick pause but small enough to catch even very short
 * S3 cycles on modern hardware.
 */
#define TYPIO_WL_RESUME_GAP_THRESHOLD_MS 2000ULL

/*
 * After firing, ignore further fires for this many ms. Lets logind and
 * the gap detector coincide cleanly: whichever observes the resume first
 * triggers the callback, the other sees the cooldown and stays silent.
 */
#define TYPIO_WL_RESUME_COOLDOWN_MS 5000ULL

struct TypioWlResumeSignal {
    TypioWlResumeCallback cb;
    void *user_data;

    uint64_t last_monotonic_ms;
    uint64_t last_boottime_ms;
    uint64_t last_fire_monotonic_ms;

#ifdef HAVE_LIBDBUS
    sd_bus *bus;
    sd_bus_slot *match_slot;
#endif
};

static void resume_signal_fire(TypioWlResumeSignal *rs,
                               const char *reason,
                               uint64_t sleep_ms) {
    uint64_t now_ms;

    if (!rs)
        return;

    now_ms = typio_wl_monotonic_ms();

    /* Cooldown: if either detector already fired recently, drop this
     * one. Use monotonic, not boottime, so the cooldown window itself
     * is measured in active time. */
    if (typio_wl_resume_in_cooldown(now_ms, rs->last_fire_monotonic_ms,
                                    TYPIO_WL_RESUME_COOLDOWN_MS)) {
        typio_log_debug("Resume signal suppressed by cooldown: reason=%s sleep_ms=%" PRIu64,
                  reason ? reason : "unknown",
                  sleep_ms);
        return;
    }

    rs->last_fire_monotonic_ms = now_ms;
    typio_log_info("Resume detected: source=%s sleep_ms=%" PRIu64,
              reason ? reason : "unknown",
              sleep_ms);

    if (rs->cb)
        rs->cb(rs->user_data, reason, sleep_ms);
}

#ifdef HAVE_LIBDBUS
static int resume_signal_match(sd_bus_message *m,
                               void *user_data,
                               sd_bus_error *ret_error) {
    TypioWlResumeSignal *rs = user_data;
    int going_to_sleep = 0;

    (void)ret_error;

    if (sd_bus_message_read(m, "b", &going_to_sleep) < 0) {
        typio_log_warning("PrepareForSleep signal missing boolean arg");
        return 0;
    }

    if (going_to_sleep) {
        /* Pre-sleep notification. We could use an inhibitor lock here
         * to flush state before suspend completes, but a minimal Stage 1
         * implementation just logs and waits for the resume edge. The
         * gap detector covers any compositor that doesn't redeliver
         * focus events on wake. */
        typio_log_info("logind: system preparing to sleep");
    } else {
        resume_signal_fire(rs, "logind", 0);
    }
    return 0;
}

static bool resume_signal_init_dbus(TypioWlResumeSignal *rs) {
    int r;

    r = sd_bus_open_system(&rs->bus);
    if (r < 0) {
        typio_log_info("logind resume-signal disabled (system bus unavailable: %s)",
                  strerror(-r));
        rs->bus = nullptr;
        return false;
    }

    r = sd_bus_match_signal(rs->bus,
                            &rs->match_slot,
                            "org.freedesktop.login1",
                            "/org/freedesktop/login1",
                            "org.freedesktop.login1.Manager",
                            "PrepareForSleep",
                            resume_signal_match,
                            rs);
    if (r < 0) {
        typio_log_warning("Failed to add PrepareForSleep match rule: %s",
                  strerror(-r));
        sd_bus_close(rs->bus);
        sd_bus_unref(rs->bus);
        rs->bus = nullptr;
        rs->match_slot = nullptr;
        return false;
    }

    typio_log_info("logind PrepareForSleep subscriber active");
    return true;
}

static void resume_signal_teardown_dbus(TypioWlResumeSignal *rs) {
    if (!rs->bus)
        return;

    /* sd_bus_slot_unref is a no-op on NULL; if we never registered a
     * match (init failed before match_signal), match_slot is NULL. */
    if (rs->match_slot) {
        sd_bus_slot_unref(rs->match_slot);
        rs->match_slot = nullptr;
    }
    sd_bus_close(rs->bus);
    sd_bus_unref(rs->bus);
    rs->bus = nullptr;
}
#endif /* HAVE_LIBDBUS */

TypioWlResumeSignal *typio_wl_resume_signal_create(TypioWlResumeCallback cb,
                                                   void *user_data) {
    TypioWlResumeSignal *rs;

    rs = calloc(1, sizeof(*rs));
    if (!rs)
        return nullptr;

    rs->cb = cb;
    rs->user_data = user_data;
    rs->last_monotonic_ms = typio_wl_monotonic_ms();
    rs->last_boottime_ms = typio_wl_boottime_ms();

#ifdef HAVE_LIBDBUS
    (void)resume_signal_init_dbus(rs); /* falls back to gap detector on failure */
#endif

    return rs;
}

void typio_wl_resume_signal_destroy(TypioWlResumeSignal *rs) {
    if (!rs)
        return;

#ifdef HAVE_LIBDBUS
    resume_signal_teardown_dbus(rs);
#endif

    free(rs);
}

int typio_wl_resume_signal_get_fd(TypioWlResumeSignal *rs) {
#ifdef HAVE_LIBDBUS
    int fd = -1;
    if (rs && rs->bus && sd_bus_get_fd(rs->bus) >= 0) {
        fd = sd_bus_get_fd(rs->bus);
    }
    return fd;
#else
    (void)rs;
    return -1;
#endif
}

int typio_wl_resume_signal_dispatch(TypioWlResumeSignal *rs) {
#ifdef HAVE_LIBDBUS
    int dispatched = 0;

    if (!rs || !rs->bus)
        return 0;

    /* Non-blocking drain so a flood on the system bus can't spin the
     * event loop (mirrors the status-bus per-tick cap). */
    while (dispatched < 16 && sd_bus_process(rs->bus, nullptr) > 0) {
        dispatched++;
    }
    return dispatched;
#else
    (void)rs;
    return 0;
#endif
}

void typio_wl_resume_signal_tick(TypioWlResumeSignal *rs) {
    uint64_t mono_now;
    uint64_t boot_now;
    uint64_t mono_delta;
    uint64_t boot_delta;
    uint64_t gap;

    if (!rs)
        return;

    mono_now = typio_wl_monotonic_ms();
    boot_now = typio_wl_boottime_ms();

    if (rs->last_monotonic_ms == 0 || rs->last_boottime_ms == 0) {
        rs->last_monotonic_ms = mono_now;
        rs->last_boottime_ms = boot_now;
        return;
    }

    mono_delta = (mono_now >= rs->last_monotonic_ms)
                     ? mono_now - rs->last_monotonic_ms : 0;
    boot_delta = (boot_now >= rs->last_boottime_ms)
                     ? boot_now - rs->last_boottime_ms : 0;

    rs->last_monotonic_ms = mono_now;
    rs->last_boottime_ms = boot_now;

    if (typio_wl_resume_gap_exceeded(mono_delta, boot_delta,
                                     TYPIO_WL_RESUME_GAP_THRESHOLD_MS, &gap)) {
        resume_signal_fire(rs, "boottime_gap", gap);
    }
}
