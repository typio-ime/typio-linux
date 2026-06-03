# Timing Model

## Purpose

This document defines the timing model for typio-wayland's Wayland input-method path. It exists to keep event ordering, ownership, and cleanup rules explicit.

If a keyboard or focus bug appears "sometimes", treat it as a timing-model problem first, not as a one-off key handling bug.

The most failure-sensitive chain is the **build-up order** that lifecycle transitions and the reconciler must preserve when bringing a focused session to a state where keys reach the engine and unhandled keys reach the app:

1. `zwp_input_method_v2` activation (a focus fact arrives)
2. keyboard-grab creation
3. compositor keymap delivery on the grab
4. keymap forwarding into `zwp_virtual_keyboard_v1`
5. virtual-keyboard transition to `ready`
6. only then: unhandled-key forwarding to the focused application

If that chain is incomplete or reordered, the frontend must not behave as if virtual-keyboard forwarding is healthy.

## The model: declared phase, observed axes, reconciled by policy

The frontend stores a declared `TypioWlLifecyclePhase` and observes a separate
read-only lifecycle snapshot from live resources:

- **declared phase** — `INACTIVE`, `ACTIVATING`, `ACTIVE`, or `DEACTIVATING`
- **observed axes** — connection, focus, grab readiness, and composition state
- **live resources** — the grab object, virtual-keyboard keymap generation, per-key tracking, and the last composition sent on the wire

Every event and every event-loop iteration can compare the declared phase
against the observed axes:

```text
declared = frontend->lifecycle_phase
observed = typio_wl_lifecycle_observe(frontend)
agrees   = typio_wl_lifecycle_state_agrees(&observed, declared)
effects  = lifecycle / reconciler repair when they diverge
```

The observed axes are not a second source of truth. They are a diagnostic and
repair view used to detect drift, such as a phase that still says `ACTIVE`
after the grab has disappeared. Imperative effects still happen at named
lifecycle boundaries (`transition_to_active`, `transition_to_reactivate`,
`transition_to_inactive`, hard reset, and reconnect handling).

### Lifecycle phases

The declared phase is stored and exported through `RuntimeState`:

| Phase | Condition | Keys to engine? |
|---|---|---|
| `INACTIVE` | No focused input-method session, disconnected frontend, or no routable keyboard path | no |
| `ACTIVATING` | Focus is being established and the grab/keymap path is not ready | no (modifiers only, see below) |
| `ACTIVE` | Focused session and current grab/keymap path are ready | yes, if the active keyboard engine is `TYPIO_ENGINE_READY` |
| `DEACTIVATING` | Focus loss or teardown is being applied | no |

A re-activate while active is **not** a no-op. It is recorded as an
`activate_seen` fact and classified at `done` as a `REACTIVATE`, which keeps the
grab and composition but re-anchors the Panel. The indicator is not re-shown on
reactivation because engine state has not changed. The old "deferred
reactivation" flag and its predicate helpers were removed; see
[ADR-0018](../adr/0018-focus-transition-classification.md).

## Truth Sources

Each input fact has exactly one source. Facts are recorded, never interpreted at arrival:

- `activate / deactivate / done(serial)`: focus + the compositor double-buffer commit point
- `key press / release`: physical key truth (carries the current grab epoch)
- `modifiers`: modifier-mask truth
- `repeat_info / repeat timer`: repeat truth
- `surrounding_text / content_type`: client editing context
- suspend gap, connection up/down: environment truth
- virtual keyboard output: **side effect only, never a source of internal truth**

Do not derive lifecycle truth from forwarded virtual-keyboard output. Additionally:

- a live keyboard grab is not proof that the virtual keyboard is ready
- a previously healthy virtual keyboard is not proof that the current grab has a current keymap

`typio_wl_lifecycle_observe()` is a **view of reality, never a stored second source of truth**, so the observed snapshot cannot drift from the resources it describes.

## Ownership

- `lifecycle.c` owns phase transitions, resume handling, and hard reset
- `reconciler.c` owns observed-axis comparison and repair for declared-phase drift
- `tracker.{c,h}` owns the per-key generation stamp and symmetric press/release tracking — mutable, and **never** the routing decision
- `router.{c,h}` owns the pure routing decision `(key, mods, state) → {action, reason}`
- `keyboard.c` owns key-event interpretation (XKB → `TypioKeyEvent`) while the declared phase is `ACTIVE`
- `bridge.c` owns virtual-keyboard health, keymap deadlines, readiness gating, and fail-safe downgrade
- `event_loop.c` owns poll scheduling, bounded auxiliary-fd dispatch, and deadline-aware wakeups
- `runtime_config.c` owns config-watch events, debounce timing, watch rearming, and the runtime reload boundary
- `pw_capture.c` owns voice recording/inference state and deferred voice reload application
- `xkb_state` owns the logical modifier view
- engine implementations own only engine/composition behavior

The status D-Bus surface exports this state but does not own it. `RuntimeState` is a read-only projection of `observe()`, not an independent tracker.

## Grab + keymap: one resource, one readiness

The keyboard grab and its virtual-keyboard keymap handshake are **one resource** with a single readiness state. There is no separate phase plus vk state machine plus "non-routable grab" rescue branch:

- `absent`: no grab object exists
- `needs_keymap`: a grab exists, but the current grab epoch has not completed the keymap handoff
- `ready`: the current epoch delivered a compositor keymap to the virtual keyboard; key/repeat processing and unhandled-key forwarding may proceed
- `broken`: the path is unhealthy and must not be trusted; a fail-safe condition

Rules:

- creating/rebuilding the grab starts a new epoch and forces `needs_keymap`
- old `ready` must never survive into a new grab epoch
- `ready` requires a compositor keymap observed in the current epoch
- a timeout in `needs_keymap`, or any `broken`, is a fail-safe condition — prefer releasing the grab over forwarding through a partially broken path
- modifier-mask updates may apply while `needs_keymap` (the derived `activating` case) so held Ctrl/Alt/Super survive grab creation before the first new key press; key presses may not

## Engine Availability

Grab readiness is necessary but not sufficient. The active keyboard engine also
has an availability state from `libtypio`:

- `TYPIO_ENGINE_READY`: key routing may call `typio_input_context_process_key`
- any other value: key press/release is consumed locally as
  `TYPIO_KEY_TRACK_ENGINE_NOT_READY`

This prevents an engine that is still deploying or loading data from returning
`NOT_HANDLED` and leaking raw keys to the focused application. A not-ready key
cycle never starts repeat and never emits virtual-keyboard events.

## One Generation Fence

"A key from before this grab is untrusted" is enforced by the active key
generation. Every new grab generation increments `active_key_generation`; a key
press claims that generation, and the matching release is accepted only when the
stored `key_generations[key]` still matches. A compositor re-send of an
already-held key across a rebuild, suspend, or reconnect is dropped because its
generation does not match the active grab.

`created_at_epoch` remains only as a short orphan-release cleanup window for
keys that were physically held before the grab existed. It must not be used to
suppress fresh key presses.

## Teardown is one operation

Every transition that ends a grab — focus loss, suspend, reconnect, fail-safe, or observed-axis repair — runs the **same** teardown path:

- forwarded keys are released to the virtual keyboard
- virtual-keyboard modifiers are reset to zero (exception below)
- key repeat is cancelled
- the grab object is destroyed and a new epoch is begun
- per-key tracking is cleared
- any stale assumption that vk is `ready` is discarded; the next epoch must re-earn `ready`

The one exception is a focus handoff (the derived `activating`-from-focused case): the last compositor-reported modifier mask may be carried to the virtual keyboard so the newly focused client can still observe a held shortcut modifier. Carried modifier state must be cleared before the next grab is built. A suspend/reconnect teardown carries nothing — a modifier held across the boundary produced no key-up and is dropped unconditionally.

## Recovery

Recovery shares the normal path **only for divergences the observed axes can
see**. Observation reads resource *presence*, not external liveness, so the
reconciler is a backstop for internal state drift, not a detector of silent
compositor-side grab death:

- **Internal divergence** — the grab object is missing while the declared phase
  still expects a routable session. The reconciler observes the mismatch and
  runs the repair path.
- **Suspend/resume** — a grab dead across suspend can leave a live proxy, which
  observation cannot distinguish from a healthy one. A resume detector records
  the gap fact and invalidates the grab generation; the next lifecycle step
  rebuilds. The input context is never `focus_out`'d, so the engine's in-flight
  composition survives.
- **Compositor reconnect** — connection death surfaces as `POLLHUP`; teardown
  returns the declared phase to inactive, and the fresh `activate` on reconnect
  drives the rebuild. Engine/session state, aux handlers, the config watch, and
  the resume detector are preserved.

A grab the compositor orphans with *no* protocol event, suspend, or disconnect
is invisible to observation and is **not** auto-recovered. The reconciler can
only act on facts it can see.

## Shortcut policy

Application shortcuts are decided in the Wayland frontend, as a pure routing decision:

- routing yields two independent dimensions: `action` (`consume` / `forward`) and `reason`; the per-key tracker records lifecycle history (forwarded, app-shortcut) for symmetric release, and is **not** the routing model
- non-modifier keys with Ctrl, Alt, or Super bypass the engine; the matching release must also bypass it
- Typio-reserved shortcuts (emergency exit, voice PTT) are consumed internally and never treated as virtual-keyboard forwarding
- emergency exit is the highest-priority reserved decision on key press: dump recent logs, release the grab, stop the frontend — it forwards no key
- engines do not each implement shortcut bypass; `Ctrl+Shift`-style modifier-only shortcuts stay transparent to the app/compositor
- on `Ctrl+Shift` engine switch completion, the arbiter clears the old engine's composition, the compositor-facing preedit, and the candidate panel before activating the new engine

## Event Loop Scheduling

The frontend uses one poll loop for Wayland and auxiliary runtime sources. Auxiliary fds are part of the timing model because they can otherwise delay keymap deadlines, lifecycle cleanup, or user-visible config changes.

- the candidate Panel is rendered once per loop iteration from the Panel
  Scheduler's `DIRTY` / `RETRY` state, never inline in the composition callback
  or key routing path
- the Panel's GPU present runs on the loop thread and must stay bounded on **both** the acquire and the present side. Acquire: `flux_surface_begin_frame` uses a finite timeout so a compositor that stops releasing swapchain images (display asleep / occluded after a lock or suspend) cannot block the loop; a timed-out present skips the frame and re-arms the panel update, and repeated stalls recreate the swapchain (ADR-0006). Present: the swapchain uses a **non-blocking present mode** (`vsync=false` → MAILBOX/IMMEDIATE) so `vkQueuePresentKHR` does not block waiting for the compositor to release a buffer (ADR-0010).
- glyphs are drawn from a **shared, persistent glyph atlas** — each glyph is rasterised once, packed into one R8 texture, and referenced by sub-rect (ADR-0012). The Panel draw path must not build or synchronously upload a texture per text run: that made every candidate **page** ~20 blocking `flux_image_create → submit_one_shot_and_wait → vkWaitForFences` calls on the loop, the cause of candidate-switch lag (and library-independent — it recurred across graphics backends). Colour stays a **draw-time tint** over the atlas's R8 coverage (R8 coverage + tint, ADR-0011), so changing the highlighted candidate only re-tints — no GPU upload.
- while the grab resource is `needs_keymap`, the poll timeout must not sleep past the current keymap deadline
- status and tray D-Bus dispatch are bounded per tick so a busy bus cannot starve Wayland dispatch, voice completion, repeat, or config reload
- config watch events schedule a debounced reload instead of reloading per inotify event; watches are rearmed after the watched file is deleted, moved, or replaced by an editor save
- voice reload is deferred while recording/inference owns the engine snapshot, then applied once the job completes; the voice fd is refreshed when runtime config changes

## Invariants

- lifecycle transitions must go through the lifecycle helpers and stay valid under `typio_wl_lifecycle_transition_is_valid`
- observed lifecycle axes must be used to detect declared-phase drift, not as a second mutable phase model
- no key press/release is processed unless the grab resource is `ready`
- no key press reaches the engine unless the active keyboard engine is `TYPIO_ENGINE_READY`
- modifier-mask updates may be processed while `needs_keymap` to resynchronize held modifiers
- no virtual-keyboard forwarding happens unless vk is explicitly `ready`
- a key whose stored generation does not match the current grab generation is dropped at routing
- no per-key tracking state survives a teardown
- application shortcut press/release stays symmetric
- a rebuilt grab never inherits prior-epoch keymap health
- fail-safe paths prefer releasing the grab over running partially broken
- config reload bursts coalesce into a single runtime reload once the filesystem settles
- an engine-switch failure must not silently clear the previously active engine in that category
- engine switch clears composition, preedit, and candidate panel before the new engine activates — no stale underlined text survives an engine boundary

## Observability Contract

Logs and `RuntimeState` serve different purposes and stay layered:

- `RuntimeState` is the authoritative live snapshot — a projection of frontend fields and observed lifecycle axes
- logs are the ordered event history explaining how the frontend reached that state
- trace topics are a `debug` surface, not a second state model

Responsibility split:

- lifecycle-edge summaries belong to `lifecycle.c`
- teardown-cause and grab create/destroy logs belong to `reconciler.c`
- virtual-keyboard health and fail-safe logs belong to `bridge.c`
- per-key sequencing and modifier-path traces belong to `keyboard.c`
- watchdog and dispatch-path logs belong to `event_loop.c`

Do not duplicate one transition across layers at the same log level. Prefer `debug` detail in a helper and one `info` summary at the boundary owner.

### Runtime fields for timing diagnosis

`RuntimeState` exports the projection of frontend fields and observed lifecycle axes. The highest-value fields:

- `lifecycle_phase` (`inactive` / `activating` / `active` / `deactivating`)
- `grab_state` (`absent` / `needs_keymap` / `ready` / `broken`)
- `active_key_generation`
- `keyboard_grab_active`
- `virtual_keyboard_state`, `virtual_keyboard_has_keymap`, `virtual_keyboard_keymap_generation`
- `virtual_keyboard_drop_count`, `virtual_keyboard_state_age_ms`
- `virtual_keyboard_keymap_deadline_remaining_ms`

A healthy active session: `lifecycle_phase=active`, `grab_state=ready`, `keyboard_grab_active=true`, `virtual_keyboard_state=ready`, `drop_count` stable. `grab_state=needs_keymap` while focused for longer than the keymap deadline is the primary clue that the grab→keymap→vk chain did not close.

## Log Level Policy

- `debug` — per-event sequencing, repeated grab/keymap churn, routing internals, trace-topic output
- `info` — low-frequency, user-relevant boundaries: focus changes, grab create/destroy summaries, vk epoch transitions, recovery to `ready`
- `warning` — recoverable anomalies: repeated grab rebuilds, repeated keymap cancellation before readiness, growing drop counts, fallback paths
- `error` — fail-safe entry, timeout shutdown, broken invariants, display/protocol failures that stop forwarding

Operational rules:

- a high-frequency path should not emit one `info` per event
- repeated anomalies prefer one aggregated `warning` plus `debug` detail
- `info` answers "what durable boundary did the frontend just cross?"; `debug` answers "why, and in what sequence?"

## Trace Capture

For shortcut-routing or repeat bugs:

```sh
typio --verbose 2>&1 | tee typio-trace.log
```

Read traces in this order: sort by `seq`, group by `topic`, compare `grab_state`, `active_key_generation`, `mods`, `phys`, and `xkb`. For `Ctrl-T`-style bugs, inspect `TRACE key`, `TRACE vk_key`, and `TRACE vk_modifiers`. A release whose stored key generation does not match `active_key_generation` is a cross-boundary orphan and is expected to be dropped at routing.

## Test Expectations

Timing-model regressions should be covered by:

- `lifecycle` tests: valid phase transitions and done-event classification
- `reconciler` tests: observed-axis divergence detection and repair decisions
- `key_tracking` tests: generation fencing and symmetric press/release across teardown
- routing tests: pure `(key, mods, state)` decisions including reserved shortcuts
- vk state-machine tests: `needs_keymap` / `ready` / `broken` / keymap-timeout transitions
- repeat tests: states that must not repeat, including `ENGINE_NOT_READY`

Every guard deleted from the old model (startup suppression, boundary carry,
divergence repair) must first be re-expressed as a failing lifecycle,
reconciler, or helper-policy test before its imperative code is removed.
