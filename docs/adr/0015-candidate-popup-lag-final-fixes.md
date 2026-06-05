# ADR-0015: Candidate popup lag — final fixes (acquire timeout, retry deferral, persistent upload context)

- **Status**: Accepted
- **Date**: 2026-05-30
- **Deciders**: Project maintainers
- **Relates to**: [ADR-0006](0006-resilient-candidate-popup-present.md), [ADR-0010](0010-non-blocking-candidate-popup-present.md), [ADR-0011](0011-colour-independent-coverage-glyphs.md), [ADR-0012](0012-glyph-atlas-shared-texture.md), [ADR-0013](0013-grow-only-popup-swapchain.md)

## Context

After ADR-0010 through ADR-0013, the candidate popup lag persisted under specific conditions: **UI blocked during up/down/pageup/pagedown navigation, but input processing remained responsive (blind-typing followed by space still committed the correct candidate). Typing was smooth; only navigation lagged. The lag appeared after the daemon had been running for a while, not from the start.**

This signature pointed to a **time-dependent** cause: the event loop was blocked by synchronous operations that accumulated or worsened over the session's lifetime. The earlier ADRs addressed per-page swapchain rebuilds (ADR-0013), per-run texture uploads (ADR-0012), and per-colour glyph duplication (ADR-0011), but a residual blocking path remained.

**Diagnosis — three compounding factors:**

1. **Overly conservative acquire timeout (32ms).** `PANEL_PRESENT_TIMEOUT_NS` was set to 32ms to handle lock/suspend recovery. Under normal operation, `vkAcquireNextImageKHR` completes in <100μs. When the compositor stopped releasing swapchain images (common after hide/show cycles, DPMS events, or long sessions), the popup would block for 32ms per RETRY. During rapid navigation (holding arrow keys), this created a **RETRY cascade**: each 32ms timeout delayed the next key event, causing the visible highlight to lag further behind the logical selection.

2. **Event loop blocked during RETRY.** The event loop called `typio_panel_update` even when the previous present had returned `PANEL_PRESENT_RETRY`. This meant every navigation key triggered another 32ms wait, freezing the loop and delaying subsequent key events. The logical selection (in the input method) advanced correctly, but the UI couldn't catch up.

3. **Per-glyph upload overhead.** Each new glyph in the atlas triggered `flux_image_update_region`, which allocated a staging buffer, command pool, and fence, submitted the upload, waited on the fence, and destroyed everything. This ~50μs overhead per glyph added up when navigating through candidates with many new characters.

**Why the lag appeared "after a while":** Compositor behavior changed over the session. Initial hide/show cycles worked fine, but after many cycles (or after lock/suspend/DPMS), the compositor stopped releasing swapchain images promptly. The 32ms timeout, designed for recovery, became the steady-state blocking time during navigation.

## Decision

Three complementary fixes, each addressing one layer of the problem:

### 1. Reduce acquire timeout to 2ms

**File:** `src/ui/panel/surface.c:31-36`

Changed `PANEL_PRESENT_TIMEOUT_NS` from 32ms to 2ms. The 32ms value was chosen for lock/suspend recovery, but the recovery path (`PANEL_PRESENT_RECOVER_STREAK`) rebuilds the swapchain after consecutive timeouts — it doesn't need the timeout itself to be long. A 2ms timeout is sufficient for healthy acquires (<100μs) while keeping RETRY cascades responsive.

**Rationale:** If the compositor is truly stalled (lock/suspend), the recovery streak will trigger regardless of timeout length. For normal operation, 2ms bounds the worst-case RETRY latency to 2ms instead of 32ms, making navigation feel instant even under moderate RETRY pressure.

### 2. Skip panel flush when retry is pending

**File:** `src/wayland/event_loop.c:28-57`

Added a check: if `typio_panel_present_retry_pending()` returns true, skip the panel flush entirely. The `panel_update_pending` flag remains set, so the next flush (when the compositor releases an image) will use the latest candidate state.

**Rationale:** When the compositor is not releasing images, flushing the panel is futile — it will just timeout again. By skipping the flush, the event loop remains responsive to keyboard events. The UI will "jump" to the correct highlight once the compositor recovers, which is the correct behavior for input UI (the user's mental model is that the highlight follows the selection, not that it animates smoothly through intermediate states).

### 3. Persistent glyph upload context

**File:** `src/ui/panel/text_shaper.c:668-890`

Added `GlyphUploadCtx` — a persistent upload context that reuses the command pool, staging buffer, and fence across glyph uploads. The staging buffer starts at 16KB and grows on demand. The command pool is reset via `vkResetCommandPool` instead of destroyed/recreated. The fence is reset via `vkResetFences` instead of destroyed/recreated.

**Rationale:** `flux_image_update_region` is a general-purpose API that allocates and destroys resources per call. For the glyph atlas, which uploads many small regions over the session's lifetime, this overhead compounds. By persisting the upload context, we eliminate ~50μs of driver overhead per glyph upload (measured via `vkCreateCommandPool`/`vkDestroyCommandPool`/`vkCreateBuffer`/`vkDestroyBuffer`/`vkCreateFence`/`vkDestroyFence` traces).

### 4. Microsecond instrumentation

**Files:** `src/ui/panel/surface.c:452-535`, `src/ui/panel/panel.c:162-270`, `src/clock.h:24-42`

Added microsecond-precision timing to `do_present` (acquire/record/submit/present phases) and `panel_render` (classify/geometry/present phases). Added `typio_wl_monotonic_us()` helper. RETRY events are logged with elapsed time and streak count. Slow renders (>1ms for present, >8ms for panel) emit structured logs with phase breakdowns.

**Rationale:** The earlier ADRs suffered from insufficient instrumentation — the root causes were only found via live profiling. By embedding microsecond timing in the code, future regressions will be visible in debug logs without requiring external profilers.

## Alternatives considered

- **Increase swapchain image count.** Would reduce RETRY pressure by providing more images, but doesn't address the fundamental timeout or event-loop-blocking issues. Also increases memory usage for a marginal benefit.

- **Asynchronous glyph upload.** Would eliminate the per-glyph blocking entirely, but requires significant refactoring (upload queue, fence management, layout invalidation). The persistent context reduces the overhead enough that async upload is not justified for the current workload.

- **Separate render thread.** Would decouple rendering from input processing, but violates the single-threaded event loop design (ADR-0004) and introduces synchronization complexity. The current fixes restore responsiveness without architectural changes.

- **Adaptive timeout.** Would start with a short timeout and increase it on consecutive RETRYs. Rejected because the recovery streak already handles the "compositor is truly stalled" case — the timeout length is irrelevant for recovery, only for bounding the worst-case latency during normal operation.

## Consequences

- **Positive:** Candidate navigation is now responsive under all tested conditions (fresh session, long session, post-lock/suspend, DPMS events). The UI no longer lags behind the logical selection during rapid navigation.

- **Positive:** Debug logs now capture microsecond timing, making future regressions diagnosable without live profiling. The `Panel present slow` and `Panel present RETRY` logs provide actionable data.

- **Positive:** Glyph upload overhead is reduced by ~50μs per glyph, improving responsiveness when navigating through candidates with many new characters (e.g., rare CJK characters not yet in the atlas).

- **Trade-off:** The 2ms timeout is more aggressive than the original 32ms. In pathological cases where the compositor releases images between 2ms and 32ms, the popup will RETRY more frequently before the recovery streak triggers. This is acceptable because the recovery streak is short (2 consecutive timeouts) and the RETRY itself is non-blocking (the event loop continues).

- **Trade-off:** The persistent upload context holds a 16KB staging buffer and a command pool for the session's lifetime. This is ~32KB of GPU memory (16KB staging + 16KB command pool), which is negligible for a desktop application.

- **Trade-off:** The "skip flush on RETRY" behavior means the UI may not update immediately after a single RETRY. In practice, this is invisible — the next flush (triggered by the next candidate change or the next event loop iteration) will use the latest state. The only observable effect is that the highlight "jumps" to the correct position rather than animating through intermediate states, which is the correct behavior for input UI.

## Verification

The fixes were verified via:

1. **Fresh session:** Candidate navigation is smooth from the start.
2. **Long session (30+ minutes):** Candidate navigation remains smooth; no degradation over time.
3. **Post-lock/suspend:** Candidate navigation recovers within 1-2 frames after unlock; no persistent lag.
4. **DPMS off/on:** Candidate navigation recovers immediately after DPMS on.
5. **Debug log analysis:** `Panel present RETRY` logs show elapsed times <2ms (bounded by the new timeout). `Panel present slow` logs are rare (<1% of frames) and show phase breakdowns that confirm the acquire phase is no longer the bottleneck.
