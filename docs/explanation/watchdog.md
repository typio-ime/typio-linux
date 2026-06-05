# Watchdog

## Purpose

The frontend runs a single-threaded `poll()` loop that holds the Wayland
keyboard grab. If any *work* stage of that loop wedges — a GPU/driver stall, a
blocked D-Bus peer, a compositor that stops responding — the user loses **all
typing**, including the ability to open a terminal to recover. The watchdog is
the self-heal for that worst case: an independent thread that detects a stalled
loop and `SIGKILL`s the process, after which the systemd user service
([ADR-0021](../adr/0021-systemd-user-service-daemon-lifecycle.md)) restarts it.

It is a **production** safety net, not a development assertion. The hangs it
covers come from the environment the daemon cannot control (drivers,
compositors, IPC peers), which is most varied on end-user machines that were
never tested in development.

## What counts as "stuck"

The main loop is instrumented with a *stage* (`TypioWlLoopStage`) and a
*heartbeat* timestamp:

- `watchdog_set_stage(stage)` marks the stage the loop is entering.
- `watchdog_heartbeat()` records that the loop made progress.
- `watchdog_stage_done()` is the common close of a work block: heartbeat, then
  return to `IDLE`.

The watchdog samples these and declares a stall only when **all** progress
signals (heartbeat, stage, stage-start time) are unchanged across a sample
*and* the heartbeat age reaches `TYPIO_WL_WATCHDOG_STUCK_MS` (3 s). The
multi-signal check means a stage *transition* counts as progress even if the
heartbeat has not yet caught up — it is a progress detector, not a single
timer.

### Restful stages are never stuck

`POLL` (blocked on file descriptors) and `IDLE` (between work stages) are
legitimate waiting states. Blocking in `poll()` with nothing to do is the
correct resting posture, not a hang, so these stages are **exempt** from the
stuck check (`stage_is_restful`). Only work stages — `FLUSH`, `PANEL_UPDATE`,
`READ_EVENTS`, `DISPATCH_PENDING`, `AUX_IO`, `REPEAT`, `CONFIG_RELOAD`,
`PREPARE_READ` — are expected to complete within the threshold.

This exemption is what lets the loop block indefinitely when idle (see
[Performance & Idle-Power Strategy](performance-strategy.md)); the watchdog no
longer needs a periodic tick to confirm liveness.

## Demand gating (armed / disarmed)

The watchdog only matters while the loop is doing input work, so it is **armed
only while an input field is focused**:

- A keyboard enter arms it; a focus loss, keyboard destroy, or unbind disarms
  it. All three route through one setter, `watchdog_set_armed`, which stores the
  flag and signals the condition variable under the lock (no lost wakeup).
- **Disarmed**, the watchdog thread blocks on its condition variable — zero
  wakeups while the daemon is idle.
- **Armed**, it samples at a coarse 1 s interval
  (`TYPIO_WL_WATCHDOG_SAMPLE_MS`), waking early if disarmed or stopped. One
  second is ample against the 3 s threshold and keeps wakeups negligible even
  during active typing (when the user is interacting and power is not the
  priority).

The condition variable uses `CLOCK_MONOTONIC` (via `pthread_condattr_setclock`)
so NTP steps or suspend/resume cannot skew the sample interval.

## Lifecycle

| Function | Role |
|----------|------|
| `watchdog_start` | Initialise the mutex/cond, start the thread (idempotent). |
| `watchdog_set_armed(f, on)` | Arm/disarm; wakes the thread. |
| `watchdog_set_stage` / `watchdog_heartbeat` / `watchdog_stage_done` | Loop instrumentation. |
| `watchdog_stop` | Signal stop, join, destroy the mutex/cond. |

On trigger the thread logs the stuck stage, virtual-keyboard state, and keymap
deadline (the usual culprit chain), then `kill(getpid(), SIGKILL)`.

## Tuning

| Constant | Value | Meaning |
|----------|-------|---------|
| `TYPIO_WL_WATCHDOG_STUCK_MS` | 3000 | Heartbeat age that declares a stall. |
| `TYPIO_WL_WATCHDOG_SAMPLE_MS` | 1000 | Sample interval while armed. |

Lowering the sample interval tightens detection latency at the cost of more
wakeups while typing; it does not affect idle power (the thread is blocked when
disarmed).

## See Also

- [ADR-0004: Event-Loop Scheduling and Watchdog](../adr/0004-event-loop-scheduling-and-watchdog.md) — original design.
- [ADR-0024: Idle-Driven Event Loop and Demand-Gated Watchdog](../adr/0024-idle-driven-loop-and-demand-gated-watchdog.md) — the current idle/cadence model.
- [Performance & Idle-Power Strategy](performance-strategy.md) — how the watchdog and the loop reach near-zero idle wakeups.
- [Event Loop Scheduling](event-loop-scheduling.md) — the loop the watchdog instruments.
