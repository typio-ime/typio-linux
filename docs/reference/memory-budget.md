# Memory Budget

Expected process memory footprint for `typio` under typical usage. All values are approximate upper bounds for a steady-state session (panel active, atlas warmed, RIME engine loaded).

## Quick reference

| Category | RSS (CPU) | GPU (DRM) | Notes |
|---|---|---|---|
| **flux Vulkan blocks** | — | 128–192 MiB | 2–3 × 64 MiB pool blocks (device-local + host-visible); bounded plateau, not a leak |
| **Transient ring** | — | 32 MiB | 16 MiB × 2 frames-in-flight per surface |
| **Swapchain images** | — | <4 MiB | 2–3 images at Panel size (grow-only, quantised to 64 px) |
| **Glyph atlas** | — | 4 MiB | 2048² × R8; one texture, process-lifetime |
| **RIME engine** | 20–50 MiB | — | Schema data, user dictionary; varies by schema |
| **Font/FreeType** | 5–10 MiB | — | Font file data, HarfBuzz faces |
| **Process overhead** | 10–15 MiB | — | libc, shared libs, stacks, TLS |
| **Total typical** | **40–80 MiB** | **164–228 MiB** | |

Combined RSS (CPU + mapped GPU) as reported by `/proc/<pid>/smaps_rollup` or `top`: **120–200 MiB**.

## How to verify

```bash
# RSS (CPU-side resident memory)
cat /proc/$(pidof typio)/smaps_rollup | grep Rss

# GPU (DRM) memory — requires kernel 5.19+
cat /proc/$(pidof typio)/fdinfo/* 2>/dev/null | grep drm-total

# Quick combined view
ps -o pid,rss,vsz -p $(pidof typio)
```

## Leak vs plateau

| Pattern | Diagnosis |
|---|---|
| RSS grows, then stabilises within the ranges above | **Normal.** Caches warm on first use; flux allocates 64 MiB blocks lazily. |
| `drm-total` grows, then stabilises | **Normal.** flux pool blocks are allocated on demand and never freed until process exit. |
| RSS grows monotonically over hours, exceeds 200 MiB | **Suspect leak.** Check `smaps` for growing segments. Likely cause: RIME userdb or unbounded cache. |
| `drm-total` grows monotonically | **Suspect leak.** Likely cause: atlas overflow not handled, or retire ring not draining. |

## Bounded resources

All GPU and CPU caches in typio-wayland are bounded. Nothing grows without limit.

| Resource | Cap | Source |
|---|---|---|
| Glyph atlas texture | 4 MiB (2048² × R8) | `GLYPH_ATLAS_DIM` in `text_shaper.c` |
| Glyph slot table | 131072 entries (~1.5 MiB) | `GLYPH_SLOT_CAP` in `text_shaper.c` (compaction at 75 % load) |
| Layout LRU cache | 128 entries | `PANEL_LAYOUT_CACHE_CAP` in `layout.h` |
| Font object cache | 64 entries | `FONT_OBJ_CACHE_CAP` in `text_shaper.c` |
| Font file cache | 32 entries | `FONT_FILE_CACHE_CAP` in `text_shaper.c` |
| Fallback font cache | 16 entries | `FALLBACK_FONT_CACHE_CAP` in `src/ui/panel/fallback_cache.c` |
| Frame retire ring | 256 items × 3 slots | `PANEL_RETIRE_SLOT_MAX` × `PANEL_RETIRE_DEPTH` in `surface.c` |
| Panel draw arena | 256 KiB | `flux_arena_init` in `surface.c` |
| Upload staging buffer | 16 KiB initial, grows on demand | `UPLOAD_STAGING_INITIAL` in `text_shaper.c` |
| Swapchain buffer | Grow-only, quantised to 64 px | `PANEL_SURFACE_QUANTUM` in `surface.c` |
| Transient ring | 32 MiB (16 MiB × 2 frames) | flux internal, `per_frame = 16 MiB` |
| flux pool blocks | 64 MiB each, never freed | `FLUX_VK_BLOCK_SIZE` in flux `internal.h` |

## flux pool block breakdown

The largest contributor to the footprint. flux's allocator (`subprojects/flux/src/core/vk_allocator.c`) allocates GPU memory in 64 MiB blocks. Each block is one `VkDeviceMemory`. Blocks are never freed until process exit.

Typical allocation pattern:

| Memory type | Blocks | Size | Contents |
|---|---|---|---|
| Device-local (image) | 1 | 64 MiB | Atlas, swapchain images |
| Device-local (buffer) | 0–1 | 0–64 MiB | Transient ring if not host-visible |
| Host-visible + coherent | 1 | 64 MiB | Staging buffers, transient ring, uniform buffers |
| Host-visible + device-address | 0–1 | 0–64 MiB | Transient ring (BDA path) |

Total: **2–3 blocks = 128–192 MiB GPU memory.**

The `drm-total` value reflects this. It does not indicate actual usage — most of the 64 MiB blocks are free space reserved by the allocator.

## Reducing the footprint

| Lever | Effect | Risk |
|---|---|---|
| Reduce `FLUX_VK_BLOCK_SIZE` to 16–32 MiB in flux | GPU memory drops to 32–64 MiB | More blocks allocated under heavy use; marginal perf cost |
| Reduce `GLYPH_ATLAS_DIM` to 1024 | GPU memory drops by 3 MiB | Fewer glyphs fit; overflow → blank glyphs until rebuild |
| Reduce `PANEL_LAYOUT_CACHE_CAP` to 64 | Negligible (CPU only) | More cache misses during paging |

None of these are recommended unless the process runs on memory-constrained hardware (<1 GiB RAM).

## See also

- [ADR-0012](../adr/0012-glyph-atlas-shared-texture.md) — glyph atlas architecture
- [ADR-0013](../adr/0013-grow-only-popup-swapchain.md) — grow-only swapchain
- [Maintenance](../dev/maintenance.md) — font cache purge, monitoring, and stall diagnosis
