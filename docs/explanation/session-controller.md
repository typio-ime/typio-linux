# Session Controller

## Purpose

This document describes the session-controller lifecycle model for typio-wayland's Wayland input-method path. It replaces the older stored-phase FSM (`INACTIVE → ACTIVATING → ACTIVE → DEACTIVATING`) plus debounced reconciler with a derived-state, idempotent-diff pipeline.

The session controller is not a new subsystem added on top of the existing code. It is a **restructuring** of the same responsibilities — grab create/destroy, focus_in/focus_out, keymap epoch scrubbing — into a shape that eliminates drift between stored state and observed reality.

## Current Status: Shadow Run

The session controller pipeline (`reduce` → `observe` → `diff`) runs on every
event-loop tick alongside the old reconciler. Its effects are **logged but not
applied** while parity is validated. The old `transition_to_active`,
`transition_to_inactive`, and `reconciler_tick` paths still own the imperative
behavior. Once shadow-run logs show zero unexpected divergence, `apply()` will
be wired in and the stored-phase FSM removed.

## Design Declaration

1. **No stored lifecycle phase.** The only persisted things are raw input facts and live resource handles. Every tick derives what the resources *should* be from what has happened.
2. **All effects are idempotent and applied as a diff.** Applying the same `desired` twice is a no-op. Recovery is not a separate code path; it is the normal path run against changed facts.
3. **Decision logic is pure and testable.** `reduce(inputs)` and `diff(desired, actual)` are pure functions. They do not touch the frontend, Wayland, or I/O.
4. **Observe reads presence, not liveness.** The diff converges the host's own state. It cannot detect a resource that is dead but still present as a client-side proxy. That is a structural limitation of the observation layer, not a bug to be patched inside the diff.

## Data Flow

Every event-loop iteration runs one step:

```c
facts   = record(inputs)        /* IM events, suspend gap, POLLHUP */
desired = reduce(facts, prev)   /* pure: what resource config do we want? */
actual  = observe(resources)    /* live snapshot: what do we have? */
effects = diff(desired, actual) /* pure: minimal ops to converge */
apply(effects)                  /* effectful: create/destroy grab, etc. */
```

### Facts

A fact is a recorded event with exactly one source. Facts are never interpreted at arrival.

| Fact | Source |
|------|--------|
| `im_activate_seen` | `zwp_input_method_v2::activate` event handler |
| `im_deactivate_seen` | `zwp_input_method_v2::deactivate` event handler |
| `im_done_had_activate` | `done()` batch classification (distinguishes reactivation from text-state update) |
| `im_done_serial` | `zwp_input_method_v2::done(serial)` |
| `connection_alive` | `POLLHUP` absence on the Wayland socket |
| `suspend_gap_detected` | System resume detector (logind `PrepareForSleep` or boottime-gap heuristic) |

### Desired State

`reduce(facts, prev)` derives the wanted resource configuration. It uses the previous tick's desired state for edge detection on focus_in/focus_out.

```c
typedef enum {
    GRAB_WANT_NONE,        /* Hard teardown: suspend, reconnect, POLLHUP */
    GRAB_WANT_SOFT_PAUSE,  /* Normal deactivate: retain grab for reuse */
    GRAB_WANT_YES,         /* Focus established: grab must exist and be ready */
} TypioWlGrabWant;
```

**Reduce rules:**

- `!connection_alive || suspend_gap_detected` → `NONE`
- `im_deactivate_seen` → `SOFT_PAUSE`
- `im_activate_seen || im_done_had_activate` → `YES`
- Otherwise → preserve `prev.grab`

**Soft pause.** A normal `deactivate` does **not** destroy the grab. The
daemon enters `SOFT_PAUSE`, releases forwarded keys, stops repeat, resets
per-key tracking, and zeros the XKB modifier state — but keeps the grab
object alive. The next `activate` reuses the existing grab, skipping the
expensive xkb keymap compile and the `NEEDS_KEYMAP` window that drops keys.
This is the behavior previously encoded in `keyboard_pause()`.

Only hard boundaries (`suspend_gap_detected` or `connection_alive = false`)
force `NONE`, which triggers a full teardown.

**Focus edge detection:**

- `focus_in`  = (`grab == YES` && `prev.grab != YES`)
- `focus_out` = (`grab != YES` && `prev.grab == YES`)

The edge detection prevents repeated `focus_in`/`focus_out` calls while the state is stable across multiple ticks.

### Actual State

`observe(resources)` returns a read-only snapshot of live frontend fields. It is **not** a second source of truth.

```c
typedef enum {
    GRAB_RES_ABSENT,       /* No keyboard grab object */
    GRAB_RES_NEEDS_KEYMAP, /* Grab exists, keymap handoff pending */
    GRAB_RES_READY,        /* Grab exists, keymap synced this epoch */
    GRAB_RES_BROKEN,       /* Keymap path unhealthy (timeout, cancellation) */
} TypioWlGrabResourceState;
```

The grab resource state merges the keyboard grab object presence with the virtual-keyboard keymap readiness from `src/wayland/keyboard/bridge.c`. This is one resource with one state, not a phase plus a separate vk state machine.

### Effects

`diff(desired, actual)` produces a minimal, idempotent effect set.

| Condition | Effects |
|-----------|---------|
| `grab == NONE`, actual != `ABSENT` | `destroy_grab`, `clear_preedit`, `commit` |
| `grab == YES \| SOFT_PAUSE`, actual == `ABSENT` | `create_grab`, `scrub_generation` |
| `grab == YES`, actual == `BROKEN` | `destroy_grab`, `create_grab`, `scrub_generation` |
| `focus_in == true` | `send_focus_in` |
| `focus_out == true` | `send_focus_out` |

### Apply

`apply(effects)` executes in fixed order:

1. `send_focus_out`
2. `destroy_grab` (runs the same teardown path as `hard_reset_keyboard`)
3. `clear_preedit` + `commit`
4. `scrub_generation`
5. `create_grab`
6. `send_focus_in`

The order matters: focus leaves before teardown, preedit is cleared before the grab is recreated, and focus enters after the new grab is ready.

## Per-Tick Workflow

```text
[Wayland dispatch] ──▶ [record facts] ──▶ [reduce] ──▶ [observe] ──▶ [diff] ──▶ [apply]
                                                            ▲
                                                            │
                                                     [live resources]
```

The pipeline runs once per event-loop iteration, after Wayland events have been dispatched and before auxiliary I/O (D-Bus, config reload, voice). This ordering ensures that input facts are fresh before any non-input work can delay the diff.

## Behavioral Parity with Old Paths

| Old Path | Old Behavior | New Model Equivalent |
|----------|-------------|---------------------|
| `transition_to_active` | `focus_in()` → create/reuse grab → `PHASE_ACTIVE` | `reduce: NONE/PAUSE → YES` → `focus_in=true` → `create_grab` |
| `transition_to_inactive` | `focus_out()` → `keyboard_pause()` → retain grab → `PHASE_INACTIVE` | `reduce: YES → SOFT_PAUSE` → `focus_out=true` → **no destroy** |
| `transition_to_reactivate` | No focus_in/out, retain grab, re-anchor panel | `reduce: YES → YES` → no edges → **no effects** |
| `lifecycle_on_resume` | `hard_reset_keyboard()` → destroy → scrub → `PHASE_INACTIVE` | `facts.suspend_gap=true` → `reduce: → NONE` → `destroy_grab + scrub` |
| `frontend_reconnect` | Teardown → `PHASE_INACTIVE` | `facts.connection_alive=false` → `reduce: → NONE` → `destroy_grab` |
| reconciler 2s repair | `handle_resume("reconcile", 0)` | Not needed; diff converges every tick |

## Blind Spot: Dead-but-Present Resources

The diff is the backstop for the host's own state, not a detector of external silent loss.

### What the diff cannot see

A resource whose client-side proxy still exists but whose compositor-side state has been silently discarded. The canonical example is a `zwp_input_method_keyboard_grab_v2` object that survives a compositor restart: the client sees a non-null pointer, so `observe()` reports `GRAB_RES_READY`, but the compositor no longer routes key events through it.

Another example is a stuck Wayland connection: the socket is open (`POLLHUP` absent) but the compositor has stopped processing events. `observe()` reports `connection_alive = true`, so `reduce()` keeps `grab = YES`, and the diff produces no effects. The user cannot type, and the controller sees no reason to act.

### Why this is accepted

Detecting silent death requires a liveness probe (heartbeat, roundtrip timeout, or harmless request echo). Any such probe has trade-offs:

- **False positives** during legitimate idle periods (user away from keyboard) cause unnecessary grab teardown and rebuild, producing visible input stalls.
- **Protocol interference**: a periodic harmless request may still mutate client state or increase power use.
- **Threshold problem**: the timeout must be longer than any normal stall (GPU-heavy compositor frame) but short enough that users do not notice the wedge. No single threshold satisfies both.

The accepted mitigation is external fact sources, not the diff:

- **Resume detector**: system suspend/resume is a strong signal that compositor state may be stale. It sets `suspend_gap_detected = true`, forcing a full scrub and rebuild.
- **POLLHUP**: socket death is unambiguous. It sets `connection_alive = false`, forcing teardown.
- **Emergency exit shortcut**: a user-facing escape hatch (`release grab + stop daemon`) for the rare case where silent death occurs without suspend or disconnect.

A future liveness probe may be added as a new fact source feeding into `reduce()`, but it does not belong inside `observe()` or `diff()`.

## Module Boundaries

| Module | Responsibility |
|--------|---------------|
| `src/engine/session_controller.{c,h}` | `reduce`, `diff`, data structures. Pure, testable without frontend or Wayland. |
| `src/wayland/session_effects.c` | `observe` (reads frontend fields) and `apply` (mutates frontend/Wayland state). Effectful, tied to `TypioWlFrontend`. |
| `src/wayland/event_loop.c` | Per-tick driver: records facts, calls reduce/observe/diff/apply in order. |
| `src/wayland/keyboard/bridge.c` | Virtual-keyboard state machine (`ABSENT → NEEDS_KEYMAP → READY → BROKEN`). The session controller reads this state but does not own the transitions. |

## See Also

- [ADR-0003: Session Controller — Derived State, Idempotent Diff](../adr/0003-session-controller-reduce-diff.md) — the architectural decision that introduced this model
- [Input-Method Session](input-method-session.md) — the three layers of "session", build-up chain, and lifecycle rules
- [Event Loop Scheduling](event-loop-scheduling.md) — GPU bounds, D-Bus dispatch, and poll deadlines
- [Wayland Input Method Protocol](wayland-input-method.md) — the protocol whose events feed `reduce`
