# ADR-0012: Shared glyph atlas (rasterise once, reference sub-rects)

- **Status**: Accepted — atlas in force as a memory/architecture improvement (lag-cause attribution corrected by [ADR-0013](0013-grow-only-popup-swapchain.md); see the scope-correction block below)
- **Date**: 2026-05-29
- **Deciders**: Project maintainers
- **Supersedes the texture model of**: [ADR-0011](0011-colour-independent-coverage-glyphs.md)
- **Relates to**: [ADR-0006](0006-resilient-candidate-popup-present.md), [ADR-0010](0010-non-blocking-candidate-popup-present.md)

> **Scope correction (2026-05-29, see [ADR-0013](0013-grow-only-popup-swapchain.md)):**
> the central claim below — that the **synchronous per-text-run texture upload
> was the root cause of the candidate-switch lag** — is **wrong**. After this
> atlas shipped in the running binary (verified via `nm` on the live process
> inode: `glyph_atlas_*` present, `build_layout_image` absent) the lag *still*
> occurred. An on-CPU profile then showed <1 % on-CPU (so the lag is a
> *blocking* wait, not the upload) and the actual cause was found to be a
> **per-candidate-page swapchain rebuild** (`flux_surface_resize` →
> `vkDeviceWaitIdle` + WSI roundtrips on the IME loop), fixed in ADR-0013.
> This ADR stands only as a **memory/architecture improvement** (bounded GPU
> memory, no per-run texture churn, no first-class synchronous uploads), not as
> the lag fix. The reasoning preserved below is a cautionary record of a
> plausible-but-unconfirmed root cause that measurement later disproved.

## Context — the cause the earlier ADRs kept circling

The candidate popup lagged when switching candidates "after a while." The
investigation reached the real cause only after three passes, and the earlier
two fixes — though each addressed a *real* problem — were mis-credited as the
cure. This ADR records the full chain so the pattern is not repeated.

1. **FIFO present block** (real, fixed by [ADR-0010](0010-non-blocking-candidate-popup-present.md)).
   gdb sampling showed the main thread ~86 % in `vkQueuePresentKHR`. Switching
   to a non-blocking present mode removed it. The lag returned.

2. **Per-colour glyph duplication** (real, fixed by [ADR-0011](0011-colour-independent-coverage-glyphs.md)).
   Glyph textures baked the text colour, so each candidate had a normal- and a
   selection-colour texture. ADR-0011 made textures colour-independent (R8
   coverage + draw-time tint), halving them. The lag *still* returned.

3. **Synchronous per-text-run texture upload** (the actual root cause, fixed
   here). The decisive new evidence was twofold:

   - **A behavioural clue from the user:** the same lag had appeared under
     *several different graphics libraries*. A symptom that survives swapping
     the entire renderer is not in the renderer — it is in how the frontend
     *uses* it.
   - **A live gdb profile taken while paging candidates** (on the
     already-fixed ADR-0010+0011 build, confirmed via `nm` that
     `flux_canvas_draw_image_coverage` was linked and the shader recompiled)
     caught the main thread in:

     ```
     typio_flux_fill_layout → build_layout_image → flux_image_create
       → flux_vk_upload_to_image → submit_one_shot_and_wait
         → vkWaitForFences → drmSyncobjWait → ioctl
     ```

   Root cause: `build_layout_image` rasterised an **entire text run into one R8
   texture** and uploaded it **synchronously** (`flux_image_create` blocks on a
   fence). The popup builds these lazily at draw time, and RIME candidate
   navigation **pages** the list, so every page was a set of all-new candidate
   strings → ~20 blocking `vkQueueSubmit + vkWaitForFences` on the
   single-threaded IME event loop. Holding the arrow key paged faster than the
   compositor retired frames, so the loop fell visibly behind — while the
   logical selection (and therefore space-to-commit) stayed instant and
   correct, exactly as reported. Typing was smooth only because human typing
   speed left gaps that absorbed the uploads.

   This is the library-independent anti-pattern: **"new text run ⇒ new texture
   ⇒ synchronous upload."** Any renderer used that way stalls; ADR-0011 reduced
   *how many* such textures, but not the per-run synchronous upload itself.

## Decision

Replace per-text-run textures with a **shared, persistent glyph atlas**, the
standard architecture for text rendering:

1. **One long-lived R8 atlas texture** (`GLYPH_ATLAS_DIM` = 2048 → 4 MiB),
   created once and cleared so inter-glyph gutters sample as zero coverage
   (`src/ui/renderer.c`, `glyph_atlas_*`).
2. **Each glyph is rasterised by FreeType once**, keyed `(font_id, glyph_id)`
   in an open-addressed hash, packed by a skyline shelf allocator
   (`src/ui/glyph_pack.c`, unit-tested in `tests/test_glyph_pack.c`), and its
   pixels uploaded to a sub-rectangle via `flux_image_update_region`. A warmed
   atlas uploads **nothing** while paging — CJK glyphs are shared across every
   candidate and page.
3. **Text draws as one tinted quad per glyph** sampling its atlas sub-rect, via
   the new `flux_canvas_draw_image_coverage_sub(canvas, atlas, dst, src, tint)`
   (flux: a `vec4 image_src` normalised sub-rect added to the image shader and
   push constants). ADR-0011's coverage-and-tint model is **retained and
   extended** — colour is still a draw-time tint, so selection re-tints with
   zero GPU work.
4. `TypioTextLayout` no longer owns a GPU image; it carries shaped glyphs +
   `font_id` only. Freeing a layout is now a pure CPU free.

ADR-0010 (non-blocking present) and ADR-0006 (bounded acquire) remain in force.

## Alternatives considered

- **Frame-batched / async upload, keeping per-run textures.** Removes the CPU
  fence wait but still allocates and churns a texture per text run (the ~192 MB
  pool plateau and fragmentation persist). The atlas removes the churn *and*
  the uploads; it is the better long-term target. Frame-integrated async upload
  of *new* glyphs (to also hide the first-sight cost) is a clean follow-up on
  top of the atlas.
- **Keep ADR-0011 only.** Rejected: the live profile proves the synchronous
  per-run upload survived it.
- **Per-glyph individual textures.** Rejected: thousands of tiny images,
  descriptor churn, and the same upload cost — the atlas is one texture, one
  bindless handle.

## Consequences

- Positive: steady-state candidate navigation and typing build and upload
  nothing once glyphs are warm — the measured `build_layout_image →
  vkWaitForFences` stall is removed at the source, for any text content.
- Positive: GPU memory is now one bounded 4 MiB atlas instead of an
  ever-churning set of per-run textures; the pool-fragmentation plateau is gone.
- Positive: the packing geometry (the part that must be correct for glyphs not
  to overlap or bleed) is pure integer logic, unit-tested without a GPU.
- Trade-off: spans flux (push-constant `image_src`, shader recompiled via
  `glslangValidator`, new `_sub` draw API, `FLUX_DEVICE_REQUIRED_PUSH_BYTES`
  144 → 160; the device must advertise ≥160 push bytes — Intel/NVIDIA/AMD/Apple
  report ≥256) and the host renderer.
- Trade-off: the *first* sighting of each unique glyph still does one small
  synchronous sub-rect upload (sub-millisecond for one glyph). Rare and
  amortised; the optional async-upload follow-up removes even that.
- Overflow is handled **non-destructively**: when the 2048² atlas is physically
  full (thousands of distinct glyphs in one session), a further new glyph is
  recorded as a non-drawable slot and the live texture is left untouched. We
  deliberately do **not** zero/repack a live atlas mid-frame — that would blank
  glyphs already recorded into the current command buffer and force a GPU drain
  on the IME loop. The rare exotic glyph that overflows renders blank until the
  atlas is rebuilt at teardown; for the popup's glyph budget this is unreachable
  in practice.
- Atlas lifetime is the **process**, not the font cache. It is *not* freed by
  `typio_flux_engine_purge_font_caches()`: cached popup layouts borrow FT_Face
  pointers and a `font_id`, and freeing the atlas would force re-rasterisation
  through faces that the same purge just freed (a use-after-free on any reload
  that does not also invalidate the layout cache). The atlas is fixed-size and
  keyed on the monotonic `font_id`, so stale slots after a purge are bounded
  dead weight, not a growing leak. It is released only at engine teardown, with
  the device idle.
- Residual legacy: the popup's frame-retire ring
  (`src/ui/popup/panel.c`) was built to defer freeing layouts/geometry that
  *owned GPU images*. Layouts now own none, so it is redundant and may be
  removed in a follow-up — left in place here to keep this change focused.
- **Verification gate**: stays **Proposed** until a real session confirms (a)
  the popup renders correctly in all colours (normal / muted / selection /
  preedit) and at fractional scale, and (b) a gdb profile *while paging
  candidates* no longer shows `build_layout_image` / `submit_one_shot_and_wait`
  / `vkWaitForFences`. GPU/shader correctness cannot be unit-tested; only a
  real session validates it.
