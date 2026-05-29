# Lifecycle Resilience and Recovery

How the Wayland frontend stays correct across the events the protocol never announces cleanly — system suspend/resume, compositor restarts, daemon crashes, and silent grab loss. This is the counterpart to the [Timing Model](timing-model.md) (the derived reduce+diff state model) and the happy-path typing pipeline.

## The problem class

The input-method lifecycle is driven by `zwp_input_method_v2` events: `activate`, `deactivate`, `done`. That works only while the compositor narrates every state change. It does not narrate system suspend (no key-up for a held key; the grab may be silently dead on wake), compositor restart (the grab vanishes with no `deactivate`), a dropped `done`, or a daemon crash. The shared shape: *the event that would have triggered recovery never arrives.*

## What the diff can — and cannot — see

The [Timing Model](timing-model.md#the-model-one-derived-state-reconciled-by-diff) converges the running daemon on reality every step: `apply(diff(desired, observe(resources)))`. It is tempting to conclude that this makes all recovery free. It does not. **`observe()` reads resource *presence*, not *liveness*.** `frontend->keyboard != NULL` projects to "grab ready" — even if the compositor has silently stopped routing keys to a grab whose proxy still exists.

So the diff is a reliable backstop for exactly one class — **internal divergence**, where we hold (or fail to hold) a resource that does not match intent — and is blind to the rest:

| Class | `observe()` sees it? | What handles it |
|---|---|---|
| Internal divergence (resource missing/extra vs intent) | yes | the per-step diff — the free part |
| Compositor-invisible state change (suspend: dead-but-present grab) | **no** | a **resume detector** invalidates the grab epoch (a fact source) |
| Connection death (compositor restart) | yes — surfaces as `POLLHUP` | in-process **reconnect** |
| Process death (daemon crash) | n/a — memory is gone | no automatic recovery |

The honest boundary: the diff fixes *our own* state, not *external* silent grab death. When is the grab *pointer* null-while-wanted (the case the diff actually repairs)? Almost always only after one of our own scrubs (suspend/reconnect) already ran. A grab the compositor orphans without any of those signals leaves a live-looking proxy and is **not** auto-recovered — see [Known limits](#known-limits).

## Detecting suspend (a fact `observe()` cannot derive)

Because a grab dead across suspend leaves a live proxy, the diff cannot see it; an external fact must invalidate it. The kernel resume is detected two complementary ways, converging on one callback:

1. **logind `PrepareForSleep`** — a system-bus signal around every suspend/hibernate. Reliable where present; absent on non-systemd setups and minimal containers. Built only when libdbus is available (`HAVE_STATUS_BUS`), on a private system-bus connection with `exit_on_disconnect` disabled so logind churn never takes the IME down.
2. **Boottime/monotonic gap heuristic** — `CLOCK_BOOTTIME` advances during suspend, `CLOCK_MONOTONIC` does not. The event loop ticks once per iteration; a boot-minus-monotonic gap above `TYPIO_WL_RESUME_GAP_THRESHOLD_MS` (2 s) means we slept. Always built, so it covers compositors where logind is missing or its signal is lost.

Both fire one callback, de-duplicated by a 5 s monotonic cooldown so a coincident logind notice and detected gap recover only once. The pure decision logic (`resume_gap_exceeded`, `resume_in_cooldown`) is dependency-free and unit-tested.

On fire, the detector records facts and lets the normal step do the rest: it **invalidates the grab epoch** (a key held across suspend produced no key-up; the new epoch fences out stale re-sends — see [Timing Model §One epoch fence](timing-model.md#one-epoch-fence)), drops carried vk modifiers unconditionally, and drops the compositor-visible preedit. It does **not** force a phase or hand-rebuild the grab. The input context is never `focus_out`'d, so the engine's in-flight composition survives; the next step re-derives `desired` (still focused → grab wanted) and the diff rebuilds — the same rebuild used on first focus. A compositor that *does* redeliver the full handshake just makes that diff a no-op.

## Protocol-write safety

`apply` is the single chokepoint for every commit. It refuses to commit when the IM serial is still 0 — before the first `done`, when the input method is not established and the compositor would silently drop the staged preedit/commit_string. The last flushed serial is recorded as a diagnostic breadcrumb and a discontinuity hook for reconnect.

## Surviving connection death — in-process reconnect

`POLLHUP`/`POLLERR` (or a failed read/flush) on the display fd is not fatal. The display-loss paths route to an in-process reconnect instead of exiting:

- Wayland-object setup/teardown is factored into reusable bind/unbind, used by startup, shutdown, and reconnect. Reconnect unbinds, re-`wl_display_connect`s with capped exponential backoff (`reconnect_backoff.{c,h}`: 250 ms → 8 s, 12 attempts), rebinds globals, and recreates the input method, virtual keyboard, and text-UI surfaces. Engine/session state, aux handlers, the config watch, and the resume detector are preserved.
- Connection-down is recorded as a fact, so `reduce` makes `desired` inactive during the outage; the grab epoch is invalidated (a key held across the outage produced no key-up). The compositor's fresh `activate` on reconnect restores focus, and the normal diff rebuilds the grab. The serial chokepoint and epoch fence make committing against the new connection safe.
- If every attempt fails the daemon exits and hands off to the service manager rather than spinning forever. The backoff schedule is pure and unit-tested.

## Known limits

- **Silently orphaned grab.** A grab the compositor stops routing to *without* a protocol event, a suspend, or a disconnect leaves a live-looking proxy. `observe()` reports it healthy, so neither the diff nor any fact source recovers it; the user must re-focus. Closing this needs a **liveness probe** — e.g. a periodic round-trip, or treating a streak of expected-but-unrouted keys as a fact that invalidates the epoch. It is deliberately not built yet; add it only if this failure is observed in the wild, and as a fact source feeding the same `reduce`, not as a new bespoke path.

## Property tests

The pure decision functions are driven by long randomized sequences and checked against an independent re-derivation of their spec, with fixed seeds:

- `reduce` / `diff`: a persistent *internal* divergence always converges within one step and never thrashes; re-applying a satisfied `desired` is a no-op; teardown clears all per-key state.
- `resume`: gap detection and cooldown match spec across random deltas/thresholds; a cooldown-gated detector never fires twice within a window; a fire always implies a strictly positive gap.
- `key_tracking`: epoch fencing drops exactly the stale-epoch keys; press/release stays symmetric across a teardown boundary.
- `reconnect_backoff`: randomized monotonicity/bounds tests.

## See Also

- [Timing Model](timing-model.md) — the derived reduce+diff state model and the one epoch fence
- [Wayland Input Method Protocol](wayland-input-method.md) — protocol implementation details
