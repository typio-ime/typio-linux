/**
 * @file session_controller.c
 * @brief Pure lifecycle decision functions: reduce, diff, done classifier,
 *        and guard predicates.
 *
 * These functions reason only about enums and structs — no frontend, no
 * Wayland, no I/O. They are the single source of truth for how input facts
 * derive desired resource configuration, and how desired-vs-actual gaps
 * project to minimal idempotent effects.
 */

#include "session_controller.h"

/* ── Name helpers (pure, for tracing) ──────────────────────────────────── */

const char *
typio_wl_grab_want_name(TypioWlGrabWant want)
{
    switch (want) {
    case TYPIO_WL_GRAB_WANT_NONE:      return "NONE";
    case TYPIO_WL_GRAB_WANT_SOFT_PAUSE: return "SOFT_PAUSE";
    case TYPIO_WL_GRAB_WANT_YES:        return "YES";
    }
    return "UNKNOWN";
}

const char *
typio_wl_grab_resource_state_name(TypioWlGrabResourceState state)
{
    switch (state) {
    case TYPIO_WL_GRAB_RES_ABSENT:        return "ABSENT";
    case TYPIO_WL_GRAB_RES_NEEDS_KEYMAP:  return "NEEDS_KEYMAP";
    case TYPIO_WL_GRAB_RES_READY:          return "READY";
    case TYPIO_WL_GRAB_RES_BROKEN:         return "BROKEN";
    }
    return "UNKNOWN";
}

const char *
typio_wl_done_action_name(TypioWlDoneAction action)
{
    switch (action) {
    case TYPIO_WL_DONE_NOOP:           return "NOOP";
    case TYPIO_WL_DONE_FIRST_ACTIVATE: return "FIRST_ACTIVATE";
    case TYPIO_WL_DONE_DEACTIVATE:     return "DEACTIVATE";
    case TYPIO_WL_DONE_REACTIVATE:     return "REACTIVATE";
    }
    return "UNKNOWN";
}

/* ── Reduce: facts → desired ───────────────────────────────────────────── */

TypioWlDesiredState
typio_wl_session_reduce(const TypioWlInputFacts *facts,
                        const TypioWlDesiredState *prev)
{
    TypioWlDesiredState d = {
        .grab = TYPIO_WL_GRAB_WANT_NONE,
        .focus_in = false,
        .focus_out = false,
        .reactivate = false,
    };

    if (!facts || !prev)
        return d;

    /* Grab want: hard boundaries first, then soft, then activation.
     * Hard boundary (suspend / connection lost) wins over an in-flight
     * deactivate in the same batch. */
    if (!facts->connection_alive || facts->suspend_gap_detected) {
        d.grab = TYPIO_WL_GRAB_WANT_NONE;
    } else if (facts->im_deactivate_seen) {
        d.grab = TYPIO_WL_GRAB_WANT_SOFT_PAUSE;
    } else if (facts->im_activate_seen || facts->im_done_had_activate) {
        /* Skip the grab entirely if no engine is registered: focusing the
         * input context is still meaningful (state in sync), but a grab
         * with no consumer would be wasted work. */
        d.grab = facts->engine_present
                     ? TYPIO_WL_GRAB_WANT_YES
                     : TYPIO_WL_GRAB_WANT_NONE;
    } else {
        d.grab = prev->grab;
    }

    /* Edge-triggered focus events. */
    d.focus_in =
        (d.grab == TYPIO_WL_GRAB_WANT_YES && prev->grab != TYPIO_WL_GRAB_WANT_YES);
    d.focus_out =
        (d.grab != TYPIO_WL_GRAB_WANT_YES && prev->grab == TYPIO_WL_GRAB_WANT_YES);

    /* Reactivate: a fresh activate inside a done batch while we are stably
     * YES means the compositor moved us to a new caret without an
     * intervening deactivate. Preserve the grab (and the in-flight
     * composition); re-anchor the panel to the new caret. */
    d.reactivate =
        (d.grab == TYPIO_WL_GRAB_WANT_YES &&
         prev->grab == TYPIO_WL_GRAB_WANT_YES &&
         facts->im_done_had_activate);

    return d;
}

/* ── Diff: desired vs actual → effects ─────────────────────────────────── */

TypioWlEffectSet
typio_wl_session_diff(const TypioWlDesiredState *desired,
                      const TypioWlActualState *actual)
{
    TypioWlEffectSet e = {0};

    if (!desired || !actual)
        return e;

    /* Hard teardown: we do not want the grab at all.
     * Scrub the key generation as well: any transition to NONE is a
     * hard boundary that must fence stale in-flight key state. */
    if (desired->grab == TYPIO_WL_GRAB_WANT_NONE &&
        actual->grab != TYPIO_WL_GRAB_RES_ABSENT) {
        e.destroy_grab = true;
        e.scrub_generation = true;
        e.clear_preedit = true;
        e.commit = true;
    }

    /* Creation: we need a grab but it is absent. Covers normal activation
     * (YES → ABSENT), the soft-pause recovery case where the grab was
     * silently dropped while paused, and the "no engine, no grab"
     * degenerate path (handled in reduce, this branch won't fire there). */
    if ((desired->grab == TYPIO_WL_GRAB_WANT_YES ||
         desired->grab == TYPIO_WL_GRAB_WANT_SOFT_PAUSE) &&
        actual->grab == TYPIO_WL_GRAB_RES_ABSENT) {
        e.create_grab = true;
        e.scrub_generation = true;
    }

    /* Broken recovery: tear down and rebuild in the same tick. */
    if (desired->grab == TYPIO_WL_GRAB_WANT_YES &&
        actual->grab == TYPIO_WL_GRAB_RES_BROKEN) {
        e.destroy_grab = true;
        e.create_grab = true;
        e.scrub_generation = true;
    }

    /* Focus edges. */
    if (desired->focus_in)
        e.send_focus_in = true;
    if (desired->focus_out)
        e.send_focus_out = true;

    /* Reactivate: re-anchor the panel to the new caret. The grab and the
     * engine state are preserved. */
    if (desired->reactivate)
        e.reactivate = true;

    return e;
}

/* ── Done classifier (pure helper, for tracing) ───────────────────────── */

TypioWlDoneAction
typio_wl_session_classify_done(bool was_active,
                               bool now_active,
                               bool activate_seen)
{
    if (now_active && !was_active)
        return TYPIO_WL_DONE_FIRST_ACTIVATE;
    if (was_active && !now_active)
        return TYPIO_WL_DONE_DEACTIVATE;
    /* Still active. Only a fresh `activate` in this batch means a genuine
     * re-activation (a move to a new field); otherwise this `done` is
     * just a text-state update and must leave focus state untouched. */
    if (was_active && now_active && activate_seen)
        return TYPIO_WL_DONE_REACTIVATE;
    return TYPIO_WL_DONE_NOOP;
}

/* ── Guard predicates (pure, on a snapshotted actual) ────────────────── */

bool
typio_wl_session_can_route_keys(const TypioWlActualState *actual)
{
    if (!actual)
        return false;
    return actual->ic_focused && actual->grab == TYPIO_WL_GRAB_RES_READY;
}

bool
typio_wl_session_can_route_modifiers(const TypioWlActualState *actual)
{
    if (!actual)
        return false;
    /* Modifiers (and their keymap handoff) can flow during the
     * NEEDS_KEYMAP window; only ABSENT or BROKEN forbids them. */
    return actual->grab == TYPIO_WL_GRAB_RES_NEEDS_KEYMAP ||
           actual->grab == TYPIO_WL_GRAB_RES_READY;
}

bool
typio_wl_session_is_transitioning(const TypioWlActualState *actual)
{
    if (!actual)
        return false;
    /* Mid-handshake: the input context is focused but the grab is not
     * yet ready, or the keymap path is broken. The keyboard guard's
     * stuck-press failsafe uses this to avoid tearing down the daemon
     * while a normal activation is still in flight. */
    return actual->ic_focused &&
           (actual->grab == TYPIO_WL_GRAB_RES_ABSENT ||
            actual->grab == TYPIO_WL_GRAB_RES_NEEDS_KEYMAP ||
            actual->grab == TYPIO_WL_GRAB_RES_BROKEN);
}
