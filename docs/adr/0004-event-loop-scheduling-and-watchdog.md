# ADR-0004: Event-Loop Scheduling and Watchdog

- **Status**: Accepted
- **Date**: 2026-05-28
- **Deciders**: Project maintainers

## Context

The host is a single process that must service several event sources at once: Wayland display events, the keyboard-repeat timer, optional D-Bus surfaces (status bus, tray), voice completion, and config-file inotify plus a debounced reload timer.

- Wayland input is latency-critical and must never be starved by an auxiliary source.
- Rendering or IPC done inline on a callback can block the message loop.
- The daemon holds a keyboard grab, so a hang in any single stage is severe and must be detectable.

## Decision

A single-threaded `poll()` loop multiplexes every source, with coalescing and a staged watchdog.

- One loop uses `wl_display_prepare_read` / `read_events` / `dispatch_pending` for Wayland; auxiliary subsystems register through a generic *fd + on_ready handler* interface rather than being special-cased.
- Wayland dispatch is the primary path; D-Bus dispatchers process a bounded number of messages per tick so they cannot monopolise the loop.
- Candidate-popup updates are not rendered inline. Callbacks set a `popup_update_pending` flag and the render is flushed once per loop iteration, so a burst of keystrokes (e.g. auto-repeat) collapses into one render.
- Config filesystem events are debounced before a reload is applied; voice reloads are deferred while a job is active.
- A watchdog records an explicit per-stage heartbeat (poll, read, dispatch, popup update, aux IO, …) so a hang can be attributed to a specific stage.
- The poll timeout can be shortened by the virtual-keyboard keymap deadline.

## Alternatives considered

- **Thread-per-subsystem.** Rejected: Wayland and engine state are shared; required locking would add more risk than the single loop removes.
- **Render / commit inline in the input callback.** Rejected: blocks the message loop and couples render cost to input latency (this was a real source of popup jank).
- **No watchdog.** Rejected: a stuck stage would manifest only as a silent, unattributable freeze of an input method that holds the keyboard.

## Consequences

- Positive: predictable, lock-free scheduling and one place to reason about ordering and starvation.
- Positive: hangs are attributable to a named stage instead of "the daemon froze".
- Trade-off: any long-running work must be chunked or deferred to keep the single loop responsive.
- Negative (accepted): the per-stage heartbeat adds bookkeeping to every loop branch.

## Related

- [ADR-0003: Session controller](0003-session-controller-reduce-diff.md) — the per-step driver runs inside the loop
- [ADR-0006: Resilient candidate-popup GPU present](0006-resilient-candidate-popup-present.md) — how the `POPUP_UPDATE` stage avoids stalling the loop
