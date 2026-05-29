# ADR-0011: Colour-independent coverage glyph textures (draw-time tint)

- **Status**: Accepted — coverage+tint model in force (texture model superseded by [ADR-0012](0012-glyph-atlas-shared-texture.md); lag-cause attribution corrected by [ADR-0013](0013-grow-only-popup-swapchain.md))
- **Date**: 2026-05-29
- **Deciders**: Project maintainers
- **Relates to**: [ADR-0006](0006-resilient-candidate-popup-present.md), [ADR-0010](0010-non-blocking-candidate-popup-present.md)

> **Scope correction (2026-05-29, see [ADR-0012](0012-glyph-atlas-shared-texture.md)).**
> This ADR removed the *per-colour* duplication of glyph textures (a real
> improvement, retained: coverage + draw-time tint). But it left the deeper
> cause in place — each text run was still rasterised into **one texture and
> uploaded synchronously**. A live gdb profile taken *while paging candidates*
> on this build still caught `build_layout_image → flux_image_create →
> submit_one_shot_and_wait → vkWaitForFences`. The texture model is replaced by
> a shared glyph atlas in ADR-0012; this ADR's coverage/tint colour model is
> carried forward unchanged.

## Context — and a correction to the earlier attribution

The candidate popup felt laggy "after a while," and the investigation reached
the right answer only in layers. Recording the full chain here because the
intermediate conclusions were **incompletely attributed** and the earlier docs
(ADR-0010, the maintenance "Monitoring" playbook) over-credited the first fix:

1. **FIFO present blocking (real, fixed by [ADR-0010](0010-non-blocking-candidate-popup-present.md)).**
   On the original build, gdb user-space sampling showed the main thread ~86 %
   in `vkQueuePresentKHR` — a vsync/FIFO present waiting on the compositor.
   Switching to a non-blocking present mode removed that and was confirmed
   (`QueuePresent` 86 % → 0). **But it was not the whole cause**: the lag came
   back.

2. **The persistent cause — found by a precise behavioural repro.** The user
   observed that **typing (popup content changing constantly) was smooth, but
   moving the highlight up/down through candidates lagged**, and that **space
   still committed the correct candidate** even while the highlight visibly
   lagged. That signature — logical selection instant and correct, only the
   *visible* glyphs lagging — pointed away from the input/RIME/libtypio path
   (which was idle in every profile) and at the popup's own GPU work.

   gdb sampling **during up/down navigation specifically** caught the main
   thread in:

   ```
   build_layout_image → flux_image_create → flux_vk_upload_to_image
     → submit_one_shot_and_wait → vkWaitForFences → drmSyncobjWait → ioctl
   ```

   i.e. a **synchronous, per-glyph GPU texture upload** on the IME event loop.

Root cause: glyph textures **baked the text colour** into a premultiplied-RGBA
image, and the cache keyed on colour. So each candidate had *two* textures — a
normal-colour and a selection-colour variant — and the selection-colour one was
built **lazily the first time that candidate became highlighted**. Navigating
the highlight down the list therefore triggered a **serial chain of blocking
glyph uploads** (`vkWaitForFences` per glyph), one per newly-highlighted
candidate. Typing kept the highlight on the first row, so it rarely paid this.
The GPU-memory plateau (~192 MB of flux's 64 MiB pool blocks; bounded, *not* a
leak — verified via `/proc/<pid>/fdinfo` `drm-total-system0` staying flat) was
the same duplication seen from the allocator side, not a separate bug.

## Decision

Make glyph textures **colour-independent** and apply colour at draw time:

1. `build_layout_image` rasterises into a single-channel **`FLUX_FORMAT_R8_UNORM`
   coverage** texture (alpha only), not premultiplied RGBA. Colour is no longer
   baked in, and is no longer stored on `TypioTextLayout` or part of the LRU
   cache key (`layout_cache_key`). One entry/texture per (text, font) now serves
   every colour.
2. flux gains `flux_canvas_draw_image_coverage(canvas, image, dst, tint)`
   (kind 4 in `canvas_image.frag`): it samples the texture's `.r` as alpha
   coverage and multiplies by `tint` (a premultiplied `flux_color` carried on
   the vertices), yielding a premultiplied tinted glyph.
3. `typio_flux_fill_layout` takes a `tint`; `paint.c` supplies it per state —
   muted/text for unselected rows, selection-text for the highlighted row,
   preedit/mode colours for those zones. Selecting a row now only adds the
   highlight rect and changes the tint; **no new texture is built or uploaded**.

Consequences for the layered fixes: ADR-0010 (non-blocking present) and
ADR-0006 (bounded acquire) remain in force and complementary. ADR-0011 removes
the per-navigation upload stall that those two did not address.

## Alternatives considered

- **Frame-batched/deferred upload in flux.** Would remove the synchronous wait
  for genuinely new glyphs, but single-step navigation creates only one new
  texture, so batching does not help the reported case. Orthogonal; could still
  be done later for paging/typing.
- **Drop the distinct selected-text colour (highlight bar only).** Simplest,
  no shader change, but a visible appearance downgrade. Rejected in favour of
  preserving the selection-text colour via tint.
- **Full packed glyph atlas (one texture, per-glyph quads).** The textbook end
  state — also amortises uploads for new glyphs and removes pool fragmentation
  entirely. Larger renderer rewrite (per-glyph quad recording, atlas
  packing/eviction). This ADR is the foundational half (coverage + tint); the
  packed atlas is a recommended follow-up.

## Consequences

- Positive: up/down candidate navigation no longer builds or uploads a texture
  per step — it re-tints cached coverage textures. Removes the measured
  `build_layout_image → vkWaitForFences` stall for that path.
- Positive: ~half as many glyph textures (no per-colour variant) and R8 is ¼
  the bytes of RGBA, so GPU memory and pool fragmentation drop sharply.
- Trade-off: spans flux (shader recompiled via `glslangValidator`, new draw
  API) and the host renderer; `flux_canvas_draw_image` (kind 3, RGBA) is
  retained for other callers.
- **Verification gate**: this ADR stays **Proposed** until runtime confirms (a)
  the popup renders correctly in all colours (normal/muted/selection/preedit),
  and (b) up/down navigation no longer shows `build_layout_image` /
  `vkWaitForFences` in a gdb sample. GPU/shader correctness cannot be unit
  tested; only a real session validates it.
