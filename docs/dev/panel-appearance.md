# Panel Appearance Development Notes

Rendering pipeline for the candidate Panel: GPU present, stall recovery, font loading, theme resolution, and cache invalidation.

---

## GPU present pipeline

The Panel presents a flux (Vulkan) swapchain onto its `zwp_input_popup_surface_v2` `wl_surface`. The swapchain owns frame pacing and buffering — no SHM buffer pool, no CPU pixel buffer, no readback.

`TypioPanelSurface` (`surface.c`) drives the present pipeline:

- `ensure_fx_surface()` creates the `VkSurfaceKHR` (`vkCreateWaylandSurfaceKHR`),
  the `flux_surface` (non-blocking present — MAILBOX/IMMEDIATE, falling back to
  FIFO; see [ADR-0010](../adr/0010-non-blocking-candidate-popup-present.md)), a
  `flux_canvas`, and a small arena; it grows the swapchain when the panel's
  physical extent (logical × scale) exceeds the current buffer
  ([ADR-0013](../adr/0013-grow-only-popup-swapchain.md)).
- `panel_surface_present()` records one frame: `flux_surface_begin_frame` →
  `flux_canvas_begin` (clear to the premultiplied background colour) →
  `panel_record` (paint) → `flux_canvas_end` → `flux_frame_submit` →
  `flux_frame_present`.
- `wl_surface_set_buffer_scale` (or a `wp_viewport` on fractional-scale setups)
  is derived from `geom->scale` so a HiDPI panel shows at the correct logical
  size and stays crisp.

Text is drawn from a **shared, colour-independent glyph atlas**
([ADR-0012](../adr/0012-glyph-atlas-shared-texture.md)). Each glyph is rasterised
by FreeType once into a single long-lived R8 *coverage* texture;
`typio_text_shape_fill` then draws one tinted quad per glyph sampling that
sub-rect, so the colour (normal / muted / selection) is a **draw-time tint**
([ADR-0011](../adr/0011-colour-independent-coverage-glyphs.md)) and no per-text
GPU upload happens during candidate navigation. Solid fills (background, border,
selection) use premultiplied RGBA via `flux_color_rgba_premul`.

The atlas hash table is automatically compacted when its load exceeds 75 %
([ADR-0019](../adr/0019-atlas-hash-compaction.md)), preventing lookup degradation
during extended CJK input sessions.

---

## Present pacing and stall recovery (lock / suspend)

`panel_surface_present` runs synchronously on the single-threaded event loop. To keep the loop responsive when a compositor stops releasing swapchain images (display asleep, surface occluded), the acquire is bounded and self-recovering.

- `flux_surface_begin_frame` is called with `PANEL_PRESENT_TIMEOUT_NS` (2 ms)
  instead of an infinite wait. The healthy on-demand path acquires in <100 μs, so
  this budget is only consumed during a real stall.
- On `FLUX_ERROR_TIMEOUT`, `panel_surface_present` returns `PANEL_PRESENT_RETRY`:
  `selected`/`visible` are **not** updated and `present_retry` is set, which
  re-arms `panel_update_pending` (via
  `typio_panel_present_retry_pending`) so the loop re-presents the
  same state until the compositor resumes and the visible highlight catches up.
- After `PANEL_PRESENT_RECOVER_STREAK` consecutive timeouts the swapchain is
  rebuilt with `flux_surface_resize` (to its current extent), discarding the
  per-frame semaphores left dangling by the stalled acquires.
- The same `flux_surface_resize` recovery is used for `FLUX_ERROR_SURFACE_LOST`
  (driver-reported `OUT_OF_DATE`/`SUBOPTIMAL`).

A stalled present never freezes key handling: input events queue on the Wayland fd while a frame is skipped, so navigation stays correct even while the on-screen highlight is briefly behind.

---

## Font loading and variable fonts

### Font description parsing

`parse_font_desc` in `text_shaper.c` understands descriptions such as:

```
"Noto Sans SemiBold 16"
```

It extracts:
- family: `"Noto Sans"`
- weight: `600` (SemiBold)
- size: `16`

### Font file selection via FontConfig

`match_font_file` asks FontConfig for a file matching `(family, weight)`. For traditional static fonts this returns different files (`NotoSans-Regular.ttf`, `NotoSans-Bold.ttf`, etc.).

### The variable-font trap

Modern systems often ship **variable fonts** — a single `.ttf` file (e.g. `NotoSans-VariableFont_wdth,wght.ttf`) that contains every weight from 100 to 900. FontConfig returns this one file for *all* weights, but FreeType loads it as the **default instance** (usually Regular, `wght = 400`).

If you do not set the variable axis, asking for SemiBold (600) or Bold (700) renders identically to Regular (400).

**Fix:** after `FT_New_Face`, detect a variable font via `FT_Get_MM_Var`, find the `wght` axis, and set it with `FT_Set_Var_Design_Coordinates`.

Call this **before** `FT_Set_Pixel_Sizes`.

---

## Font object caching

`font_obj_cache` stores `(path, size, weight)` → `(FT_Face, hb_font_t)`. The cache key **must include weight** — variable fonts mutate the face's `wght` axis in place, so omitting weight would alias Medium and SemiBold to the same `FT_Face`.

`TypioTextShape` borrows the cached `FT_Face`. Glyphs are rasterised once via `FT_Load_Glyph` into the atlas on first sight; subsequent draws are atlas hits with no FreeType call. Shapes must not outlive their font cache entry — `panel_render_ctx_invalidate` frees all shapes before eviction (draining the retire ring behind a device fence first).

---

## Theme resolution

The Panel supports three modes:

| Mode | Behaviour |
|---|---|
| `auto` | Detects desktop dark/light from GTK_THEME, gtk-3.0/4.0 settings.ini, or KDE kdeglobals |
| `light` | Built-in light palette |
| `dark` | Built-in dark palette |

The resolved palette is cached with a 5-second TTL to avoid repeated filesystem reads during rapid render cycles.

Users can override individual channels per mode via `display.colors.light.*` and `display.colors.dark.*` in the config file. The `panel_config_build_palette` function applies these overrides on top of the built-in base palette.

### When adding a new colour channel

1. Add the fields to `TypioPanelPalette` in `theme.h`
2. Add defaults to `palette_light` and `palette_dark` in `theme.c`
3. Add parsing support in `panel_config_load` (`LOAD_VARIANT` macro)
4. Add override application in `panel_config_build_palette`
5. Use the new colour in `paint.c`
6. Update user-facing configuration documentation

---

## Layout cache invalidation

`PanelRenderCtx` maintains an LRU layout cache keyed by candidate label + text + font description (label and main). Colour is not part of the key — glyphs are colour-independent R8 coverage ([ADR-0011](../adr/0011-colour-independent-coverage-glyphs.md)), so the selected and unselected states of a row share one cache entry.

Changing the font weight, size, or family produces a different cache key. The cache does **not** survive `panel_render_ctx_invalidate`, which happens on theme or config changes.
