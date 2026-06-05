# ADR-0024: Idle-Driven Event Loop and Demand-Gated Watchdog

- **Status**: Accepted
- **Date**: 2026-06-05
- **Deciders**: Project maintainers

## Context

ADR-0004 established a single `poll()` loop with a per-stage heartbeat watchdog. As first implemented, the loop used a 100 ms baseline poll timeout and the watchdog thread slept on a fixed 100 ms `nanosleep`. Both ran continuously regardless of whether the daemon had work, so an idle, backgrounded daemon woke ~20 times per second. For an always-resident input method this is a measurable, needless drain on laptop idle power — every timer wakeup pulls the CPU package out of its deepest idle (C-)states — and it is exactly what `powertop` flags.

The 100 ms loop tick was not arbitrary: it kept the watchdog heartbeat advancing so the watchdog would not mistake an idle, poll-blocked loop for a hang. The two costs were therefore **coupled** — the loop could not block indefinitely because the watchdog required liveness, and the watchdog polled continuously to observe that liveness.

The watchdog's value is real and specific to an IME. It holds the keyboard grab, so a stall in any *work* stage (GPU/driver, D-Bus peer, compositor) locks the user out of all typing — including out of any terminal they would open to kill it. On a stall ≥ 3 s it `SIGKILL`s the process; the systemd user service (ADR-0021) restarts it. Disabling it in production would remove the one self-heal for the worst-case failure, on exactly the diverse hardware/compositors never exercised in development. The goal is therefore not to remove the watchdog but to stop it — and the loop — from spending power when there is nothing to guard.

## Decision

Make idle genuinely idle, and gate the watchdog by demand.

1. **The event loop blocks indefinitely when idle.** The baseline poll timeout is `-1`. Sources that wake the loop on their own — key repeat, the indicator timer, and the config-reload debounce — are `timerfd`s already in the poll set and need no timeout. Only three deadlines are driven by the poll timeout itself, and each lowers it through a `-1`-aware minimum (`poll_timeout_min`):
   - panel retry cadence (ADR-0023),
   - the positioned-UI anchor-probe deadline (ADR-0017),
   - the virtual-keyboard keymap deadline.
   The anchor-probe deadline was previously covered only *implicitly* by the 100 ms tick; it is now explicit.

2. **The watchdog is exempt on restful stages.** `POLL` (blocked on fds) and `IDLE` (between work stages) are legitimate waiting states and can never count as "stuck". Only work stages carry a deadline. This removes the need for a liveness tick during idle and decouples the loop's poll timeout from the watchdog.

3. **The watchdog blocks while disarmed.** It is armed only while an input field is focused. Disarmed, it waits on a condition variable (zero wakeups) and is woken immediately when `armed`/`stop` flip via `watchdog_set_armed`. Armed, it samples at a coarse 1 s interval (was 100 ms) — ample against the 3 s stuck threshold.

4. **Convergence of scattered instrumentation.** Arming routes through one `watchdog_set_armed` setter (was three raw atomic stores at the call sites). The repeated `heartbeat(); set_stage(IDLE);` stage-close tail collapses into one `watchdog_stage_done`.

## Alternatives considered

- **Disable the watchdog in production.** Rejected: removes the only recovery for a grab-holding IME wedge, in the environment where it matters most.
- **Keep the 100 ms tick.** Rejected: continuous idle wakeups for a resident daemon, with no liveness benefit once restful stages are exempt.
- **Coarser fixed `nanosleep` (e.g. 1 s) without condvar gating.** Rejected as a half-measure: still ~1 wakeup/s while idle versus zero. The condition variable reaches true event-driven idle.
- **Per-stage deadlines on the watchdog instead of a single heartbeat age.** Deferred: the single restful/work split plus heartbeat age is sufficient for SIGKILL-on-hang; per-stage timeouts add tuning surface without a current need.

## Consequences

- Positive: idle wakeups from the loop and the watchdog both approach zero, observable with `powertop` (per-process wakeups/s).
- Positive: watchdog coverage is unchanged for the cases that matter — a hang in a work stage still `SIGKILL`s within ~3–4 s → systemd restart.
- Positive: the anchor-probe deadline is now explicit rather than a side effect of the tick.
- Trade-off: stuck-detection latency widens slightly (sample interval 100 ms → 1 s), negligible against the 3 s threshold.
- Negative (accepted): a mutex + condition variable are added to the watchdog and one cross-module setter; the watchdog thread now has two wait states (disarmed-block, armed-sample).

## Related

- Amends [ADR-0004](0004-event-loop-scheduling-and-watchdog.md) — introduced the loop and watchdog; this ADR changes their idle behaviour and cadence.
- [ADR-0021](0021-systemd-user-service-daemon-lifecycle.md) — the restart path the watchdog's `SIGKILL` relies on.
- [ADR-0023](0023-panel-scheduler-state-machine.md) — the panel scheduler state driving the retry-cadence deadline.
- [ADR-0017](0017-positioned-ui-arbitration.md) — the positioned-UI anchor probe whose deadline is now folded into the poll timeout.
- Explanation: [Watchdog](../explanation/watchdog.md) · [Performance & Idle-Power Strategy](../explanation/performance-strategy.md).
