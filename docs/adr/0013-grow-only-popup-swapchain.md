# ADR-0013: Grow-only popup swapchain (stop rebuilding per candidate page)

- **Status**: Accepted (blocked-stack profile confirmation while paging still recommended; see Consequences)
- **Date**: 2026-05-29
- **Corrects the root-cause claim of**: [ADR-0012](0012-glyph-atlas-shared-texture.md)
- **Relates to**: [ADR-0006](0006-resilient-candidate-popup-present.md), [ADR-0010](0010-non-blocking-candidate-popup-present.md)

## Context — the lag that survived four ADRs

The candidate popup lag returned *again* after the glyph atlas (ADR-0012) was
built and shipped in the running binary. This time the diagnosis started from
ground truth on the live process rather than from the source, and that is what
finally located the cause the earlier ADRs had each missed.

**Disproving the previous theory first.** The running daemon was inspected
directly:

- `nm /proc/<pid>/exe` on the *actually-running* (deleted) inode showed
  `glyph_atlas_*` and `flux_canvas_draw_image_coverage_sub` present and
  `build_layout_image` **absent** — i.e. the ADR-0012 atlas was live, the
  synchronous per-text-run upload was gone, **and the lag still occurred.**
  ADR-0012's claim that the synchronous upload was *the* cause of the
  candidate-switch lag is therefore wrong. (The atlas remains a valid
  improvement — bounded GPU memory, no per-run texture churn — it just was not
  this bug.)

**Then measuring where the time actually goes.**

- An on-CPU `perf record -g` of the daemon during candidate activity collected
  very few samples (~260 over 35 s, <1 % on-CPU) — proof the lag is a
  **blocking** wait, not computation. The on-CPU samples that *did* land were
  dominated by Vulkan **Wayland-WSI swapchain/surface (re)initialisation**:

  ```
  wl_display_roundtrip_queue
    wsi_wl_display_init
      wsi_wl_surface_get_formats
      wsi_wl_surface_get_present_modes
  ```

  i.e. the popup was **recreating its swapchain** during normal candidate use.

**The code path that triggers it.** `popup_render` recomputes the popup's
pixel size on every CONTENT delta and calls `ensure_fx_surface(popup, pw, ph)`
(`src/ui/popup/panel.c`). The old `ensure_fx_surface` rebuilt the swapchain
whenever `surf_w/surf_h != requested`:

```c
} else if (popup->surf_w != w || popup->surf_h != h) {
    flux_surface_resize(popup->fx_surface, w, h);   /* per page! */
}
```

and `flux_surface_resize` (flux `src/core/surface.c`) is **heavy and fully
synchronous on the single-threaded IME event loop**:

```c
flux_result flux_surface_resize(...) {
    vkDeviceWaitIdle(device);                 /* full GPU stall */
    reset_frame_semaphores(s);
    return flux_surface_create_swapchain(...);/* GetSurfaceCapabilities,
                                               * pick_format, pick_present_mode,
                                               * destroy+create swapchain,
                                               * recreate all image views */
}
```

**Every candidate page changes the popup's width** (different candidate
strings ⇒ different pixel width), so holding the arrow key ran a
`vkDeviceWaitIdle` + swapchain teardown/rebuild (+ WSI compositor roundtrips)
**on every keypress**, on the loop that also handles key input. That is the
lag.

This finally explains the full history, including why it kept coming back:

- It is **blocking**, matching the <1 %-on-CPU profile.
- It is **per-page**, matching "lag while switching candidates."
- It is **library-independent in cause** — *any* GPU backend stalls if the
  frontend rebuilds its swapchain on every width change. This is exactly the
  user's long-standing clue that the same symptom appeared under several
  different graphics libraries. The fault is the **frontend mechanism**, not
  the renderer.
- It worsens **"after a while"** — it bites once you actively page long, varied
  candidate lists, not during the first few simple keystrokes.
- **No prior ADR touched this path.** ADR-0010 fixed the *present* block
  (FIFO → MAILBOX); ADR-0011/0012 reduced/removed texture work. The
  per-page `flux_surface_resize` was never addressed, so the lag returned after
  each of them.

## Decision

Make the popup swapchain **grow-only and size-quantised**, and crop the
oversized buffer to the exact content rect with the viewport we already bind.

1. **Quantise the buffer to `POPUP_SURFACE_QUANTUM` (64 px) and grow only.**
   `ensure_fx_surface` rebuilds the swapchain only when the content *exceeds*
   the current buffer; shrinks and sub-quantum widenings reuse it. After a
   short warm-up the buffer reaches the widest candidate row and
   `flux_surface_resize` is never called again during steady-state paging.
2. **Crop with `wp_viewport_set_source(0, 0, pw, ph)`.** The content is rendered
   at the buffer's top-left; the viewport source rect shows only the exact
   content pixels and scales them to the logical size, so an oversized buffer is
   invisible. `wp_viewporter` is already bound and already used for fractional
   scaling (`wp_viewport_set_destination`); this adds the source half.
3. **Legacy path unchanged.** Without a viewport the buffer maps 1:1 to the
   surface and must equal the content, so it still resizes exactly — but that
   path has no fractional scaling anyway and is the uncommon fallback.

ADR-0010 (non-blocking present), ADR-0006 (bounded acquire) and ADR-0012
(glyph atlas) all remain in force; this is independent of them.

## Alternatives considered

- **Keep per-size swapchains but make resize cheap / async.** Rejected: the
  resize is intrinsically a `vkDeviceWaitIdle` + swapchain rebuild + WSI
  roundtrips; there is no cheap synchronous form. The right move is to *not
  resize*.
- **Allocate one big max-size buffer up front.** Rejected: wasteful for the
  common small popup and still needs the viewport-source crop; grow-only
  reaches the same steady state without guessing a maximum.
- **Render off the IME thread.** Larger architectural change; the popup present
  is deliberately on the event loop (ADR-0006/0010). Not needed once the
  per-page rebuild is gone.

## Consequences

- Positive: steady-state candidate paging performs **zero** swapchain rebuilds
  and **zero** `vkDeviceWaitIdle` on the IME loop — the measured blocking path
  is removed at the source, independent of the graphics backend.
- Positive: the buffer is bounded (widest row, rounded to 64 px); the slightly
  larger clear is negligible for a candidate popup.
- Trade-off: a few rebuilds during warm-up (until the widest row is seen) and
  one whenever a still-wider row appears — rare and self-limiting.
- Trade-off: requires `wp_viewporter` for the grow-only path; the no-viewport
  fallback keeps exact-size resize (acceptable — it is the legacy integer-scale
  path).
- **Verification gate**: stays **Proposed** until a real session confirms (a) the
  popup renders correctly while paging, at integer and fractional scale, with no
  visible margin or clipping, and (b) a *blocked-stack* profile while paging
  (`gdb -p <pid> -batch -ex bt` in a loop) no longer shows
  `flux_surface_resize` / `vkDeviceWaitIdle` / `vkCreateSwapchainKHR` /
  `wsi_wl_*` on the main thread — only `epoll_wait`/`ppoll` between keystrokes.
  On-CPU `perf` cannot prove a blocking fix; only the wall-clock sampler can.
