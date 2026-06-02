# ADR-0019: Atlas hash-table compaction for sustained CJK input

- **Status**: Accepted
- **Date**: 2026-06-02
- **Deciders**: Project maintainers
- **Supersedes the hash-table strategy of**: [ADR-0012](0012-glyph-atlas-shared-texture.md)
- **Relates to**: [ADR-0009](0009-long-term-performance-optimizations.md)

## Context

After extended CJK input sessions (30+ minutes of continuous typing), the Panel UI degraded from smooth to noticeably laggy. Profiling traced the cost to `glyph_atlas_get()` in `text_shaper.c`.

ADR-0012 introduced an open-addressing hash table (`GLYPH_SLOT_CAP = 32768`) to look up glyph atlas entries. Every new glyph seen by the shaper inserted an `occupied = true` slot. **Slots were never evicted**, even after the owning layout was removed from the LRU cache. CJK input produces an unbounded stream of distinct characters, so the table filled steadily:

- Fresh session: ~2 % load → 1–2 probes per lookup → ~0 μs overhead.
- After 30 min: ~60 % load → ~15 probes per lookup → measurable per-frame cost.
- `typio_text_shape_fill()` calls `glyph_atlas_get()` once per glyph per frame. A candidate page with 50 glyphs means 50 degraded lookups per present.

The degradation was time-dependent and invisible without profiling — the panel worked correctly, just slower with each passing minute.

## Decision

### 1. Increase hash-table capacity 4×

`GLYPH_SLOT_CAP` raised from 32768 to **131072** (128 K slots). This pushes the 75 % compaction threshold from ~24 K to ~98 K entries, buying more time between compaction events for the same input volume.

Cost: `131072 × sizeof(GlyphSlot)` = ~1.5 MiB CPU memory (up from ~0.4 MiB). Negligible against the existing GPU footprint.

### 2. Automatic hash-table compaction

Track `live_count` on the `GlyphAtlas` struct, incremented on each new insertion.

When `live_count` reaches `GLYPH_SLOT_CAP × 75 %`:

1. `flux_device_wait_idle()` — ensure no in-flight GPU reads of the hash table.
2. Walk every entry in the panel's LRU layout cache (`PanelRenderCtx`, 128 entries). For each `TypioTextShape`, iterate its glyph array and mark the corresponding hash index as live.
3. Zero the entire hash table.
4. Re-insert only the marked slots from a snapshot of the old table.
5. Reset `live_count` to the actual number of live entries.

Atlas texture pixels are **not** relocated — only the lookup table is rebuilt. Dead entries' texture space is abandoned (the 2048² R8 atlas holds ~17 K CJK glyphs; hash compaction fires long before the texture fills).

### 3. Compaction trigger in `panel_render()`

`typio_text_atlas_compact(&panel->render)` is called at the top of every `panel_render()`, before geometry computation. It returns immediately (one comparison) when the load is below threshold.

Cost of a compaction event: ~100 μs for ~500 live glyphs — well within a single frame budget and triggered infrequently (only when the hash table actually degrades).

### 4. Minimal LRU iteration API

Two new functions in `layout.c` expose just enough of the cache for compaction without breaking encapsulation:

- `panel_render_ctx_entry_count()` — number of populated slots.
- `panel_render_ctx_entry_shapes()` — returns opaque `TypioTextShape *` pointers (the struct remains private to `text_shaper.c`).

Compaction iterates the shapes directly via the `TypioTextShape` fields (`glyphs[]`, `font_id`, fallback arrays) — all in `text_shaper.c`, which owns the struct definition.

## Alternatives considered

- **Robin Hood hashing / cuckoo hashing.** Reduces probe-chain length at high load, but does not eliminate dead entries. The fundamental problem is table growth, not probe quality.
- **Periodic full reset (clear table, re-rasterise all LRU glyphs).** Guarantees zero dead entries but forces `FT_Load_Glyph` + `glyph_upload_region` for every cached glyph — seconds of blocking GPU uploads vs. ~100 μs CPU-only rebuild.
- **Separate "live" bitmap alongside the hash table.** Avoids the mark phase but adds per-lookup overhead and doubles the per-slot memory.
- **Tombstone-based deletion on LRU eviction.** Requires the eviction callback to know the hash index, coupling the layout cache to the atlas internals. Also fragments the probe chains over time.

## Consequences

- Positive: glyph lookup time is now bounded regardless of session length. The hash table load never exceeds 75 %.
- Positive: compaction is pure CPU work (~100 μs) with no GPU involvement. No frame stalls, no device-idle waits during normal operation (the wait-idle only fires when compaction actually triggers).
- Positive: the 4× capacity increase delays the first compaction event to well beyond a typical session for most users.
- Trade-off: ~1.1 MiB additional CPU memory for the larger hash table.
- Trade-off: compaction calls `flux_device_wait_idle()`, which stalls the GPU pipeline. This is acceptable because compaction is infrequent (load-dependent, not time-dependent) and the stall is bounded (~100 μs).
- Negative (accepted): dead entries' atlas texture space is never reclaimed. The 4 MiB atlas is large enough that this is not a practical concern — the hash table compacts long before texture exhaustion would occur.
