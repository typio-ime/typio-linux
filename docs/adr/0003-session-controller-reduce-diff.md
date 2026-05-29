# ADR-0003: Session Controller — Derived State, Idempotent Diff

- **Status**: Accepted
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

The host must manage three intertwined things across compositor variation and platform churn (suspend/resume, screen lock, compositor restart, silent grab loss):

1. **Activation lifecycle.** `zwp_input_method_v2` fires `activate`, `deactivate`, and `done(serial)` in compositor-dependent orders.
2. **The keyboard grab.** A `zwp_input_method_keyboard_grab_v2` plus its `zwp_virtual_keyboard_v1` keymap form one logical resource: the host can only safely route keys when both are ready.
3. **Recovery.** The protocol does not announce grab loss or compositor restart. A naive implementation grows ad-hoc recovery paths (suspend detector, reconcile-repair, reconnect) each with its own scrub-and-rebuild logic, drifting from any single source of truth.

An earlier design used a stored phase FSM (`INACTIVE → ACTIVATING → ACTIVE → DEACTIVATING`) plus orthogonal observation axes plus a debounced reconciler to glue the two together. That arrangement had three failure modes:

- The stored phase drifted from observed reality; the reconciler existed only to repair drift.
- Per-key correctness was implemented three times (epoch arrays, startup suppression, created-at-epoch stamps).
- Recovery was duplicated per boundary (suspend, reconcile, reconnect) with subtly different scrubs.

## Decision

There is **no stored lifecycle phase.** The only persisted things are *raw input facts* and *live resource handles*. Every event and every loop tick runs one step:

```
inputs  : recorded facts        (IM activate/deactivate/done(serial), focus,
                                  surrounding text, content-type, engine-present,
                                  suspend gap, connection up/down)
desired = reduce(inputs)         pure: grab wanted? preedit-on-wire? which serial?
actual  = observe(resources)     live: grab object? keymap synced? last preedit sent?
effects = diff(desired, actual)  minimal, idempotent
          apply(effects)         create/destroy grab, sync keymap, send/clear
                                  preedit, commit
```

The reconciler is no longer a 2-second backstop — it **is** the normal event path. **Suspend, resume, reconnect, and silent grab-loss stop being special cases**; each is "an input changed or a resource vanished → recompute → apply."

### Invariants

1. **Lifecycle state is derived, never stored.** Only input facts and live resource handles persist. No code may hand-mutate a phase.
2. **All effects are idempotent and applied as a diff** against observed resources. Applying the same `desired` twice is a no-op — this is what makes recovery free rather than a separate path.
3. **One epoch fence.** Every key carries the current grab epoch; only current-epoch keys are trusted. There is one stamp-and-compare, not three generation arrays.
4. **Grab + vk-keymap is one resource with one readiness state** (`absent → needs_keymap → ready → broken`) — not a phase plus a separate vk state machine plus a "non-routable grab" rescue branch. Keys route to the engine only when the resource is `Ready`.
5. **Routing is pure `(key, mods, state) → decision`**; the per-key tracker is the only mutable part and exists solely for symmetric press/release. Routing and tracking never share a struct.

### What the diff can and cannot see

The idempotent diff converges the running daemon on reality, but only for divergences `observe()` can detect — and `observe()` reads resource *presence*, not *liveness*. A resource that is **dead but still present** (canonically, a grab whose compositor-side routing stopped while the client proxy survives) projects as healthy, so the diff is blind to it. Such cases need an external *fact source* (the resume detector, a `POLLHUP`-driven reconnect) or a future *liveness probe* that turns the condition into a fact feeding `reduce`. The diff is the backstop for the host's *own* state, not a detector of external silent loss.

### Safety backstops

Because the host holds a keyboard grab, a wedged state is severe: the user could end up unable to type anywhere. Two backstops:

- **Emergency-exit shortcut** that releases the grab and stops the daemon.
- **Guard failsafe** that does the same after a streak of rejected presses while not in a transition.

### Module shape

- `session_controller.{c,h}` — `reduce(inputs) → desired` (pure) + the per-step driver.
- `session_effects.{c,h}` — `observe(resources)` + `diff(desired, actual) → effects` + `apply`.
- `key_tracking.{c,h}` — epoch stamp + symmetric-release tracking (mutable, no routing).
- `key_route.{c,h}` — pure routing decision.

## Alternatives considered

- **Keep a stored phase FSM as a cache, with observed axes as a passenger.** Rejected: stored projections of lifecycle reintroduce exactly the drift the reconciler was built to chase. Derive, don't cache.
- **Per-boundary recovery (one path each for suspend, reconcile, reconnect).** Rejected: scrub-and-rebuild logic drifts apart over time; idempotent diff makes them one path.
- **Passive / no grab.** Rejected: without the key stream the IME cannot compose or run candidate navigation.

## Consequences

- Positive: suspend / resume / reconnect / silent grab-loss share the *one* code path; recovery is a property of idempotent diffing, not bespoke trees.
- Positive: the phase enum, the reconciler debounce, and three epoch fences all disappear — large net deletion of bug-dense code.
- Positive: the per-step driver is pure and property-testable.
- Trade-off: `reduce` and `diff` must be exhaustively property-tested; every retired guard becomes a failing test in the new model first.
- Negative (accepted): the diff cannot see a dead-but-present resource; that condition needs a fact source (resume detector, POLLHUP), not the diff.

## Related

- [ADR-0002: Wayland Input Method v2](0002-wayland-input-method-v2.md) — the protocol whose events feed `reduce`
- [ADR-0004: Event-loop scheduling and watchdog](0004-event-loop-scheduling-and-watchdog.md) — when the per-step driver runs
- [ADR-0006: Composition as state, commit as event](https://github.com/ming2k/libtypio/blob/main/docs/adr/0006-composition-state-and-commit-event.md) (`libtypio`) — the engine-side contract this controller pairs with
