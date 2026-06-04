/**
 * @file session_controller.h
 * @brief Desired-vs-actual session lifecycle controller.
 *
 * There is no stored lifecycle phase. The only persisted things are raw input
 * facts and live resource handles. Every event-loop tick runs one step:
 *
 *   facts   = record(inputs)
 *   desired = reduce(facts, prev)   pure
 *   actual  = observe(resources)    live snapshot
 *   effects = diff(desired, actual) pure, minimal, idempotent
 *   apply(effects)                  effectful
 *
 * This module owns the pure half: facts, desired, actual, effects, reduce,
 * and diff. The effectful half (observe + apply) lives in
 * src/frontend/session_effects.c because it must read and mutate the
 * TypioWlFrontend struct.
 *
 * @see docs/explanation/session-controller.md
 * @see ADR-0003: Session Controller — Derived State, Idempotent Diff
 */

#ifndef TYPIO_WL_SESSION_CONTROLLER_H
#define TYPIO_WL_SESSION_CONTROLLER_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Input facts ───────────────────────────────────────────────────────── */

/** Raw input facts recorded from Wayland events and environment detectors.
 *  Each fact has exactly one source. Facts are recorded, never interpreted. */
typedef struct TypioWlInputFacts {
    /** An activate event arrived in the current dispatch batch. */
    bool im_activate_seen;
    /** A deactivate event arrived in the current dispatch batch. */
    bool im_deactivate_seen;
    /** The current done() batch included an activate (distinguishes
     *  reactivation from a plain text-state update). */
    bool im_done_had_activate;
    /** Serial from the most recent done() event. */
    uint32_t im_done_serial;
    /** Wayland connection is alive (no POLLHUP observed). */
    bool connection_alive;
    /** The system-resume detector fired (logind PrepareForSleep or
     *  boottime-gap heuristic). */
    bool suspend_gap_detected;
    /** A keyboard engine is registered and ready to process input. When
     *  false, the controller still focuses the input context but skips
     *  the keyboard grab (no consumer for the key stream). */
    bool engine_present;
} TypioWlInputFacts;

/* ── Desired state ─────────────────────────────────────────────────────── */

/** Whether the session wants a keyboard grab. */
typedef enum {
    TYPIO_WL_GRAB_WANT_NONE = 0,
    /** Normal deactivate: release keys, reset tracking, but retain the grab
     *  object so the next activation can reuse it (soft pause). */
    TYPIO_WL_GRAB_WANT_SOFT_PAUSE,
    /** Focus is established: grab must exist and be ready for key routing. */
    TYPIO_WL_GRAB_WANT_YES,
} TypioWlGrabWant;

/** Desired resource configuration derived from facts.
 *
 *  focus_in / focus_out / reactivate are edge-triggered: they are true only
 *  on the tick when the relevant transition crosses a boundary. This
 *  prevents repeated calls while the state is stable. */
typedef struct TypioWlDesiredState {
    TypioWlGrabWant grab;
    /** YES edge: was not YES before, is YES now. Triggers engine focus_in. */
    bool focus_in;
    /** YES→non-YES edge: was YES, is not YES now. Triggers engine focus_out. */
    bool focus_out;
    /** YES→YES with an activate_seen in the same done batch: re-anchor the
     *  panel to the new caret. The grab, engine state, and in-flight
     *  composition are preserved. */
    bool reactivate;
} TypioWlDesiredState;

/* ── Actual state ──────────────────────────────────────────────────────── */

/** Unified readiness of the grab + virtual-keyboard-keymap resource.
 *  This is a single resource with one state, not a phase plus a separate
 *  vk state machine. */
typedef enum {
    TYPIO_WL_GRAB_RES_ABSENT = 0,
    /** Grab exists but the current epoch has not completed the keymap
     *  handoff to the virtual keyboard. Modifier updates may proceed;
     *  key presses may not. */
    TYPIO_WL_GRAB_RES_NEEDS_KEYMAP,
    /** Grab exists and the compositor keymap has been forwarded to vk in
     *  the current epoch. Keys may be routed to the engine. */
    TYPIO_WL_GRAB_RES_READY,
    /** The keymap path is unhealthy (timeout, repeated cancellation,
     *  fd dup failure). The grab must be torn down and rebuilt. */
    TYPIO_WL_GRAB_RES_BROKEN,
} TypioWlGrabResourceState;

/** Read-only snapshot of live resources. Not a second source of truth. */
typedef struct TypioWlActualState {
    bool connection_alive;
    bool ic_focused;
    TypioWlGrabResourceState grab;
} TypioWlActualState;

/* ── Effects ───────────────────────────────────────────────────────────── */

/** Minimal, idempotent effect set produced by diff().
 *
 *  Applying the same effect set twice is a no-op (or harmless). This is
 *  what makes recovery free: suspend, reconnect, and reconcile-repair all
 *  funnel into diff → apply rather than bespoke scrub paths. */
typedef struct TypioWlEffectSet {
    bool destroy_grab;
    bool create_grab;
    bool scrub_generation;
    bool send_focus_in;
    bool send_focus_out;
    bool clear_preedit;
    bool commit;
    bool reactivate;
} TypioWlEffectSet;

/* ── Pure functions ────────────────────────────────────────────────────── */

/**
 * @brief Derive desired state from input facts.
 *
 * @param facts       Recorded facts for the current tick.
 * @param prev        The desired state from the previous tick. Used for
 *                    edge detection on focus_in / focus_out / reactivate.
 * @return Derived desired state.
 *
 * Rules (first match wins):
 *   - !connection_alive || suspend_gap_detected            → NONE
 *   - im_deactivate_seen                                    → SOFT_PAUSE
 *   - im_activate_seen || im_done_had_activate              → YES
 *   - otherwise                                             → preserve prev.grab
 *
 * Edge detection:
 *   - focus_in    = (grab == YES && prev.grab != YES)
 *   - focus_out   = (grab != YES && prev.grab == YES)
 *   - reactivate  = (grab == YES && prev.grab == YES && facts.im_done_had_activate)
 */
TypioWlDesiredState
typio_wl_session_reduce(const TypioWlInputFacts *facts,
                        const TypioWlDesiredState *prev);

/**
 * @brief Compute the minimal effect set needed to converge actual onto desired.
 *
 * @param desired  What we want.
 * @param actual   What the resources currently are.
 * @return Idempotent effect set.
 *
 * Rules (all evaluated independently; multiple can fire on one tick):
 *   - grab == NONE, actual != ABSENT   → destroy_grab + scrub_generation +
 *                                        clear_preedit + commit
 *   - grab == YES | SOFT_PAUSE, actual == ABSENT
 *                                      → create_grab + scrub_generation
 *   - grab == YES, actual == BROKEN    → destroy_grab + create_grab +
 *                                        scrub_generation
 *   - focus_in  edge                   → send_focus_in
 *   - focus_out edge                   → send_focus_out
 *   - reactivate                       → reactivate (panel re-anchor)
 */
TypioWlEffectSet
typio_wl_session_diff(const TypioWlDesiredState *desired,
                      const TypioWlActualState *actual);

/* ── Done-event classifier (kept as a pure helper) ────────────────────── */

/** Outcome of a `done` event, classified by the four relevant focus axes.
 *  Replaces the implicit "phase" enum from the legacy lifecycle module. */
typedef enum {
    TYPIO_WL_DONE_NOOP = 0,           /* no focus change                  */
    TYPIO_WL_DONE_FIRST_ACTIVATE,     /* was inactive, now active         */
    TYPIO_WL_DONE_DEACTIVATE,         /* was active, now inactive         */
    TYPIO_WL_DONE_REACTIVATE,         /* active, then a real (re)activate */
} TypioWlDoneAction;

/** Pure classifier. Identical to the result of:
 *  - (was_active XOR now_active)? (now_active? ACTIVATE : DEACTIVATE) : NONE
 *  - PLUS: if was_active && now_active && activate_seen → REACTIVATE
 *
 *  Kept for trace logging and for the focus-transition ADR. The session
 *  controller itself derives desired state from raw facts without going
 *  through this enum, so the four cases remain observable. */
TypioWlDoneAction
typio_wl_session_classify_done(bool was_active,
                               bool now_active,
                               bool activate_seen);

/* ── Guard predicates (pure, on a snapshotted actual) ────────────────── */

/** @brief Can the host safely route a key to the engine right now?
 *
 *  True iff the input context is focused AND the grab resource is READY.
 *  This replaces the legacy `phase_allows_key_events` predicate; it is
 *  derived from the actual resource state, not a stored phase.
 *
 *  Hot-path callers (key dispatch) may call this after a fresh observe()
 *  snapshot; the predicate is branch-only and has no I/O. */
bool
typio_wl_session_can_route_keys(const TypioWlActualState *actual);

/** @brief Can the host safely process modifier updates right now?
 *
 *  True iff the grab resource is anything other than ABSENT or BROKEN.
 *  Modifier updates (and their keymap handoff) can run during the
 *  keymap-loading window (NEEDS_KEYMAP); only a missing or broken
 *  resource forbids them. Replaces `phase_allows_modifier_events`. */
bool
typio_wl_session_can_route_modifiers(const TypioWlActualState *actual);

/** @brief Is the actual state mid-transition (between focus edges)?
 *
 *  True iff the engine is focused but the grab is not yet READY (or
 *  is BROKEN). This is the "ACTIVATING-shaped" state in the legacy
 *  model. The keyboard guard's stuck-press failsafe uses this to avoid
 *  tearing down the daemon while a normal activation handshake is
 *  still in flight. Replaces the `phase != ACTIVATING` guard. */
bool
typio_wl_session_is_transitioning(const TypioWlActualState *actual);

/* ── Naming helpers (pure, for tracing) ────────────────────────────────── */

const char *
typio_wl_grab_want_name(TypioWlGrabWant want);

const char *
typio_wl_grab_resource_state_name(TypioWlGrabResourceState state);

const char *
typio_wl_done_action_name(TypioWlDoneAction action);

/* ── Effectful half (implemented in src/frontend/session_effects.c) ───── */

struct TypioWlFrontend;

/**
 * @brief Observe live resources from the frontend.
 *
 * Read-only snapshot. Not a second source of truth.
 */
TypioWlActualState
typio_wl_session_observe(const struct TypioWlFrontend *frontend);

/**
 * @brief Apply an effect set to the frontend.
 *
 * Executes in fixed order: focus_out → destroy_grab → clear_preedit →
 * commit → scrub_generation → create_grab → focus_in → reactivate.
 *
 * The order is part of the contract. Reordering breaks the engine
 * boundary invariants: focus leaves before teardown, preedit is cleared
 * before the grab is recreated, and focus enters after the new grab is
 * ready. The ordering is enforced by the `TYPIO_WL_SESSION_EFFECT_ORDER`
 * static check in session_effects.c.
 */
void
typio_wl_session_apply(struct TypioWlFrontend *frontend,
                       const TypioWlEffectSet *effects);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_SESSION_CONTROLLER_H */
