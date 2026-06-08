# ADR-0010: Non-blocking present mode for the candidate popup

- **Status**: Accepted — in force (lag-cause attribution corrected by [ADR-0013](0013-grow-only-popup-swapchain.md); the non-blocking present itself is retained)
- **Date**: 2026-05-29
- **Deciders**: Project maintainers
- **Amends**: [ADR-0006](0006-resilient-candidate-popup-present.md)

> **Scope correction (2026-05-29, see [ADR-0011](0011-colour-independent-coverage-glyphs.md)).**
> This ADR fixed a *real* present-side block (FIFO `vkQueuePresentKHR`
> waiting on the compositor, measured at ~86 %), and that fix stands. But it
> over-credited itself as the cause of the "candidate switching is laggy"
> report: the lag returned. The **persistent** candidate-switch cause was a
> separate one — a synchronous per-glyph texture upload triggered by the
> per-colour (selected vs unselected) glyph duplication — diagnosed and fixed
> in ADR-0011. Read the two together: 0010 unblocked present; 0011 removed the
> per-navigation upload.

## Context

[ADR-0006](0006-resilient-candidate-popup-present.md) bounded the popup's GPU
present so a stalled compositor cannot freeze the event loop, and explicitly
**retained FIFO (vsync)**, concluding: *"the bound — not the present mode — is
what guarantees loop responsiveness … present mode is orthogonal."* That
analysis was scoped to the **acquire** side: `flux_surface_begin_frame` /
`vkAcquireNextImageKHR` blocking after a screen lock, DPMS-off, or suspend.

A later report surfaced a different symptom: **candidate switching becomes very
laggy after the daemon has been running for a while, and a fresh instance is
fine.** It was diagnosed on the live (already-laggy) process with system tools,
not by reading code:

1. `perf record -p <pid> -g` over a 30 s window of active candidate paging
   captured only ~29 on-CPU samples → **not CPU-bound**; neither the font path
   nor RIME was the hotspot.
2. `/proc/<pid>/stack` sampled 300× showed **300/300 in `do_sys_poll`** — the
   thread was blocked, but the kernel stack alone cannot tell the idle 100 ms
   loop `poll` from a `poll` *inside* a present.
3. A gdb user-space "poor-man's profiler" (`gdb -p <pid> -batch -ex
   'thread apply all bt'` in a loop) was decisive: **69 of 80 main-thread
   samples (~86 %)** sat in
   `popup_present → flux_frame_present → anv_QueuePresentKHR →
   wsi_wl_swapchain_queue_present → wl_display_dispatch_queue → ppoll`.

Root cause: under FIFO, `vkQueuePresentKHR` blocks until the compositor releases
a swapchain image. ADR-0006's `POPUP_PRESENT_TIMEOUT_NS` bounds
`flux_surface_begin_frame` (acquire) **only** — it does not bound
`flux_frame_present`. The popup is an **on-demand, event-driven** surface (it
commits a frame only when candidates change), so FIFO — a model built for a
continuous per-vblank render loop — is a model mismatch: the compositor has no
reason to release buffers promptly for a surface it is not scheduling frames
for, and the present-side wait is unbounded. Because the present runs
synchronously on the single-threaded IME loop, that wait becomes input latency.
ADR-0006's claim that present mode is orthogonal holds for the acquire stall but
is **wrong for the steady-state present-side block** measured here.

## Decision

Create the popup swapchain with `vsync = false` (`src/ui/popup/panel.c`,
`ensure_fx_surface`). flux then selects `MAILBOX`, else `IMMEDIATE`, else
`FIFO` (`subprojects/flux/src/core/surface.c`, `pick_present_mode`). A
non-blocking present returns without waiting on buffer release, so
`vkQueuePresentKHR` no longer stalls the loop.

ADR-0006's bounded acquire, retry, and swapchain-recovery logic are **retained
unchanged**. The two decisions are complementary, not alternatives:

| Concern | Bounded by |
|---|---|
| Acquire stall after lock/suspend (compositor holds every image) | ADR-0006 — `begin_frame` timeout + recover streak |
| Steady-state present throttle while switching candidates | ADR-0010 — non-blocking present mode |

## Alternatives considered

- **Keep FIFO + bounded acquire only (ADR-0006 status quo).** Rejected: the
  acquire timeout does not bound `vkQueuePresentKHR`; the present-side block was
  measured at ~86 % of wall-clock during normal use.
- **Add a timeout to the present.** Not possible — `vkQueuePresentKHR` takes no
  timeout. Emulating one needs a present thread + fence handshake; the
  non-blocking present mode achieves the same with one flag.
- **Dedicated present thread.** Rejected, consistent with
  [ADR-0004](0004-event-loop-scheduling-and-watchdog.md) and ADR-0006: surface
  state is loop-owned, and cross-thread Wayland/flux access adds locking and
  protocol-sequencing risk. Unnecessary once the present is non-blocking.

## Consequences

- Positive: removes the measured ~86 % present-side block; candidate switching
  stays responsive regardless of how the compositor throttles frame callbacks
  for the popup surface.
- Positive: complements [ADR-0004](0004-event-loop-scheduling-and-watchdog.md)'s
  watchdog — the `POPUP_UPDATE` stage is no longer dominated by a present wait.
- Trade-off: `MAILBOX` keeps ≥3 swapchain images (marginally more GPU memory;
  negligible for a tiny popup) and `IMMEDIATE` can tear — irrelevant for a
  candidate popup.
- Trade-off: depends on the driver advertising `MAILBOX`/`IMMEDIATE`. flux falls
  back to FIFO otherwise — no regression, but the lag would persist; confirm
  with the gdb method above.
- **Verification gate**: this ADR stays **Proposed** until a post-change gdb
  sample on an affected setup shows main-thread `anv_QueuePresentKHR` occupancy
  dropping from ~86 % toward ~0. Flip to **Accepted** once confirmed.
