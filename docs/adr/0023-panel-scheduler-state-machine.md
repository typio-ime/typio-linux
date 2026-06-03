# ADR-0023: Panel Scheduler State Machine

- **Status**: Accepted
- **Date**: 2026-06-03
- **Deciders**: Typio maintainers

## Context

The Candidate Panel update path had accumulated several independent fixes for
long-running input lag:

- ADR-0013 removed per-page swapchain rebuilds.
- ADR-0020 reclaimed saturated glyph atlas resources.
- ADR-0022 removed durable retry state from `TypioPanelSurface`.

ADR-0022 still left one architectural weakness in the frontend:
`panel_update_pending` represented both a dirty candidate snapshot and a present
retry. The event loop shortened its poll timeout to 16 ms whenever that boolean
was set. Under focus churn or stalled presentation, an ordinary dirty update
could therefore behave like a retry and keep the daemon waking at retry cadence
even when no retry was flushable.

The host-managed candidate navigation path also called the session UI flush
helper directly from key routing. That pulled Panel rendering, GPU present, and
input-method protocol commits back into the key handling path.

## Decision

Replace the boolean pending flag with an explicit Panel schedule state:

- `TYPIO_WL_PANEL_SCHEDULE_IDLE`: no pending Panel work.
- `TYPIO_WL_PANEL_SCHEDULE_DIRTY`: the candidate snapshot changed and needs one
  event-loop flush.
- `TYPIO_WL_PANEL_SCHEDULE_RETRY`: the last Panel present returned retry and
  needs another present attempt when the focused context is flushable.

Candidate composition callbacks and host-managed navigation call
`typio_wl_session_request_ui_update()`, which sets the state to `DIRTY`.
The key routing path no longer calls any Panel flush helper.

Only the event loop may flush scheduled Candidate Panel updates. After a flush,
the Panel update result maps back into schedule state:

- present success or failure clears the scheduler to `IDLE`;
- present retry sets the scheduler to `RETRY`;
- a new candidate update supersedes retry and sets the scheduler to `DIRTY`.

The 16 ms retry poll cadence is enabled only when the state is `RETRY` and the
current input context is focused and flushable.

## Alternatives considered

- **Keep the boolean and add another guard**: Rejected because the boolean still
  hides two different meanings. Future changes would have to remember which
  call sites mean dirty work and which mean present retry.
- **Flush Panel updates synchronously on navigation keys**: Rejected because it
  lets GPU present or compositor stalls block key routing, which is the wrong
  ownership boundary for input latency.
- **Always use a fixed short poll timeout while candidates are visible**:
  Rejected because it turns a rare recovery path into normal idle behavior and
  wastes wakeups during sustained use.

## Consequences

- Positive: Dirty candidate updates, present retries, and cancellation have
  separate state transitions and can be tested without a compositor.
- Positive: Key routing updates only the candidate snapshot; Panel rendering
  belongs to the event loop.
- Positive: Retry cadence cannot persist across focus loss or ordinary dirty
  updates.
- Trade-off: Candidate navigation may wait until the next event-loop Panel
  stage instead of flushing inside the key callback.
- Negative (accepted): The frontend has a small scheduler state machine instead
  of a single boolean flag.
