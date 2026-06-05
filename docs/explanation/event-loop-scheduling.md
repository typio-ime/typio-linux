# Event Loop Scheduling

## Purpose

This document defines how the typio-linux event loop schedules work and
preserves responsiveness. It covers the ordering constraints between Wayland
dispatch, GPU rendering, D-Bus, config reload, and voice processing.

The frontend uses one poll loop for Wayland and auxiliary runtime sources.
Auxiliary fds are part of the scheduling model because they can otherwise
delay keymap deadlines, lifecycle cleanup, or user-visible config changes.

## Wayland Dispatch First

Every event-loop iteration dispatches Wayland events before any auxiliary
work. This ordering ensures that input facts are fresh before any non-input
work can delay the session controller's `reduce`/`diff`/`apply` pipeline.

```text
[Wayland dispatch] ──▶ [record facts] ──▶ [reduce] ──▶ [observe] ──▶ [diff] ──▶ [apply]
                                                            ▲
                                                            │
                                                     [live resources]
```

After the session pipeline completes, auxiliary work runs in a bounded
fashion so no single source can starve the others.

## GPU Rendering Bounds

### Panel render cycle

The candidate Panel is rendered once per loop iteration from the Panel
Scheduler's `DIRTY` / `RETRY` state, never inline in the composition callback
or key routing path.

### Swapchain acquire

`flux_surface_begin_frame` uses a finite timeout so a compositor that stops
releasing swapchain images (display asleep / occluded after a lock or suspend)
cannot block the loop. A timed-out present skips the frame and re-arms the
panel update, and repeated stalls recreate the swapchain (ADR-0006).

### Swapchain present

The swapchain uses a **non-blocking present mode** (`vsync=false` →
MAILBOX/IMMEDIATE) so `vkQueuePresentKHR` does not block waiting for the
compositor to release a buffer (ADR-0010).

### Glyph atlas

Glyphs are drawn from a **shared, persistent glyph atlas** — each glyph is
rasterised once, packed into one R8 texture, and referenced by sub-rect
(ADR-0012). The Panel draw path must not build or synchronously upload a
texture per text run: that made every candidate **page** ~20 blocking
`flux_image_create → submit_one_shot_and_wait → vkWaitForFences` calls on the
loop, the cause of candidate-switch lag (and library-independent — it recurred
across graphics backends). Colour stays a **draw-time tint** over the atlas's
R8 coverage (R8 coverage + tint, ADR-0011), so changing the highlighted
candidate only re-tints — no GPU upload.

## Poll and Deadline Management

The poll timeout defaults to **`-1` (block until an fd is ready)** so an idle
daemon causes zero wakeups. Sources backed by a `timerfd` — key repeat, the
indicator timer, the config-reload debounce — wake the loop themselves and need
no timeout. Only deadlines *not* backed by an fd shorten the timeout, each via a
`-1`-aware minimum (`poll_timeout_min`):

- the panel retry cadence while a deferred flush is pending (ADR-0023),
- the positioned-UI anchor-probe deadline while a popup awaits its caret anchor
  (ADR-0017) — previously covered only implicitly by a fixed baseline tick,
- the virtual-keyboard keymap deadline while the grab is `needs_keymap`.

The watchdog monitors the loop for stalls and `SIGKILL`s on an unrecoverable
hang (systemd then restarts). It exempts the restful `POLL`/`IDLE` stages, so an
indefinitely blocked idle loop is never mistaken for a stall — which is what
allows the `-1` timeout above. See [Watchdog](watchdog.md) and
[Performance & Idle-Power Strategy](performance-strategy.md) for the full model.

## Auxiliary Source Bounding

### D-Bus dispatch

Status and tray D-Bus dispatch are bounded per tick so a busy bus cannot
starve Wayland dispatch, voice completion, repeat, or config reload.

### Config reload

Config watch events schedule a debounced reload instead of reloading per
inotify event; watches are rearmed after the watched file is deleted, moved,
or replaced by an editor save. Config reload bursts coalesce into a single
runtime reload once the filesystem settles.

### Voice processing

Voice reload is deferred while recording/inference owns the engine snapshot,
then applied once the job completes; the voice fd is refreshed when runtime
config changes.

## Observability

### Log Level Policy

| Level | Purpose |
|-------|---------|
| `debug` | per-event sequencing, repeated grab/keymap churn, routing internals, trace-topic output |
| `info` | low-frequency, user-relevant boundaries: focus changes, grab create/destroy summaries, vk epoch transitions, recovery to `ready` |
| `warning` | recoverable anomalies: repeated grab rebuilds, repeated keymap cancellation before readiness, growing drop counts, fallback paths |
| `error` | fail-safe entry, timeout shutdown, broken invariants, display/protocol failures that stop forwarding |

Operational rules:

- a high-frequency path should not emit one `info` per event
- repeated anomalies prefer one aggregated `warning` plus `debug` detail
- `info` answers "what durable boundary did the frontend just cross?";
  `debug` answers "why, and in what sequence?"

Responsibility split:

- session-controller effect summaries belong to `event_loop.c`
- teardown-cause and grab create/destroy logs belong to `session_effects.c`
- virtual-keyboard health and fail-safe logs belong to `bridge.c`
- per-key sequencing and modifier-path traces belong to `keyboard.c`
- watchdog and dispatch-path logs belong to `event_loop.c`

Do not duplicate one transition across layers at the same log level. Prefer
`debug` detail in a helper and one `info` summary at the boundary owner.

### Trace Capture

For shortcut-routing or repeat bugs:

```sh
typio --verbose 2>&1 | tee typio-trace.log
```

Read traces in this order: sort by `seq`, group by `topic`, compare
`grab_state`, `active_key_generation`, `mods`, `phys`, and `xkb`. For
`Ctrl+T`-style bugs, inspect `TRACE key`, `TRACE vk_key`, and
`TRACE vk_modifiers`. A release whose stored key generation does not match
`active_key_generation` is a cross-boundary orphan and is expected to be
dropped at routing.

### Runtime Diagnostics

`RuntimeState` exports the projection of frontend fields and observed
lifecycle axes. The highest-value fields:

| Field | Meaning |
|-------|---------|
| `lifecycle_phase` | `inactive` / `activating` / `active` / `deactivating` |
| `grab_state` | `absent` / `needs_keymap` / `ready` / `broken` |
| `active_key_generation` | current grab epoch |
| `keyboard_grab_active` | whether a grab object exists |
| `virtual_keyboard_state` | vk readiness |
| `virtual_keyboard_has_keymap` | vk has received a keymap |
| `virtual_keyboard_keymap_generation` | keymap epoch |
| `virtual_keyboard_drop_count` | cumulative dropped forwards |
| `virtual_keyboard_state_age_ms` | time since last vk state change |
| `virtual_keyboard_keymap_deadline_remaining_ms` | time until keymap timeout |

A healthy active session: `lifecycle_phase=active`, `grab_state=ready`,
`keyboard_grab_active=true`, `virtual_keyboard_state=ready`, `drop_count`
stable. `grab_state=needs_keymap` while focused for longer than the keymap
deadline is the primary clue that the grab→keymap→vk chain did not close.

## Invariants

- config reload bursts coalesce into a single runtime reload once the
  filesystem settles
- the Panel's GPU present runs on the loop thread and must stay bounded on
  both the acquire and the present side
- glyphs are drawn from a shared, persistent glyph atlas; no synchronous
  upload per text run

## See Also

- [Input-Method Session](input-method-session.md) — the three layers of
  "session", build-up chain, and lifecycle rules
- [Session Controller](session-controller.md) — the reduce/diff/apply pipeline
  that runs inside the event loop
- [Wayland Input Method Protocol](wayland-input-method.md) — protocol
  implementation and event handlers
- [Panel Architecture](panel-architecture.md) — Panel content, zones, and
  rendering
- [Vulkan and Flux Rendering](vulkan-flux-rendering.md) — GPU backend details
- [ADR-0004: Event Loop Scheduling and Watchdog](../adr/0004-event-loop-scheduling-and-watchdog.md)
- [ADR-0024: Idle-Driven Event Loop and Demand-Gated Watchdog](../adr/0024-idle-driven-loop-and-demand-gated-watchdog.md)
- [Watchdog](watchdog.md) · [Performance & Idle-Power Strategy](performance-strategy.md)
