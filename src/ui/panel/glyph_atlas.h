/**
 * @file glyph_atlas.h
 * @brief Shared, persistent R8 coverage atlas for rasterised glyphs.
 *
 * Every glyph is rasterised by FreeType ONCE, packed into a single long-lived
 * atlas texture, and thereafter referenced by a sub-rectangle. Text draws as
 * one tinted quad per glyph sampling that sub-rect, so a warmed atlas uploads
 * nothing during candidate navigation and selection re-tints with zero GPU
 * work. Lookup is an open-addressing hash keyed by (font_id, glyph_id).
 *
 * Lifecycle / reclamation: both the hash table and the texture accumulate dead
 * weight as the layout LRU evicts shapes — the table lengthens probe chains and
 * the shelf packer only ever advances, so the image eventually saturates. The
 * atlas tracks this and glyph_atlas_reclaim() rebuilds it wholesale (image,
 * slots, packer, counts), reclaiming the space; the next draw re-rasterises the
 * visible page lazily. See the .c for the device-idle safety argument.
 *
 * Upload batching: glyph_atlas_get() does NOT perform its own vkQueueSubmit.
 * Misses are queued into a per-call staging area and committed by
 * glyph_atlas_flush() in a single submit+fence, so a cold atlas re-warm after
 * a reclaim (or a fresh font-cache expansion) costs one GPU round-trip per
 * frame instead of one per glyph — important because a candidate panel can
 * surface tens of previously-unseen CJK glyphs in a single redraw.
 *
 *   Bound:   GLYPH_SLOT_CAP hash slots; GLYPH_ATLAS_DIM² texture.
 *   Evict:   none per-entry — reclaimed wholesale.
 *   Reclaim: glyph_atlas_reclaim() on 75% load OR packer exhaustion.
 *   Observe: glyph_atlas_entry_count(), glyph_atlas_get_diag().
 */
#ifndef TYPIO_WL_GLYPH_ATLAS_H
#define TYPIO_WL_GLYPH_ATLAS_H

#include <flux/flux.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define GLYPH_ATLAS_DIM   2048u   /* R8 → 4 MiB; thousands of glyphs           */

typedef struct GlyphSlot {
    uint64_t key;          /* (font_id << 32) | glyph_id; 0 == empty slot     */
    uint16_t u, v, w, h;   /* atlas sub-rect, pixels                          */
    int16_t  left, top;    /* FreeType bearings                               */
    bool     occupied;
    bool     drawable;     /* false for whitespace / load failure / no fit    */
} GlyphSlot;

typedef struct GlyphAtlasDiag {
    uint32_t live;          /* occupied hash slots                            */
    uint32_t slot_capacity; /* total hash slots                              */
    uint32_t shelf_y;       /* packer vertical fill, physical px             */
    uint32_t dim;           /* atlas side length, physical px                */
    bool     packer_full;   /* image saturated, awaiting reclaim             */
    uint64_t rebuilds;      /* cumulative reclaim count                      */
    uint64_t rasterized;    /* cumulative FT_Load_Glyph inserts              */
    uint64_t flushes;       /* cumulative glyph_atlas_flush() calls with work*/
    uint32_t flush_peak_batch;  /* largest single batch observed             */
    uint64_t flush_total_regions; /* cumulative regions across all batches   */
} GlyphAtlasDiag;

/* Look up a glyph's atlas slot, rasterising it on first sight. The pixel
 * upload is deferred to the next glyph_atlas_flush() so that all misses in a
 * frame coalesce into a single vkQueueSubmit; the returned slot's u/v/w/h are
 * valid immediately, and @drawable is set as soon as placement + rasterisation
 * succeed (the actual GPU write happens at flush time but completes before
 * the panel's render pass samples the atlas).
 * After warm-up every panel glyph is a hash hit with zero GPU work. Returns
 * NULL only if no render device exists yet or the table is pathologically
 * full; the slot may be non-drawable (whitespace / load failure / no fit).
 *
 * The caller MUST invoke glyph_atlas_flush() once after every batch of
 * glyph_atlas_get() calls that participates in the same rendered frame, before
 * the frame's render pass executes. The panel's present path does this.
 *
 * @size_px and @weight are applied to @face before FT_Load_Glyph on a miss;
 * required because the FT_Face is shared across (size, weight) tuples. */
const GlyphSlot *glyph_atlas_get(uint32_t font_id, FT_Face face, uint32_t glyph_id,
                                  float size_px, int32_t weight);

/* Commit every queued glyph upload in a single vkQueueSubmit + vkWaitForFences.
 * Safe to call when nothing is queued (no-op). Returns true if every queued
 * region uploaded successfully; on false, the queued slots are marked
 * non-drawable so the next draw skips them instead of sampling garbage. */
bool glyph_atlas_flush(void);

/* The atlas texture, for sampling sub-rects at draw time. Builds the atlas on
 * demand; returns NULL only when no render device exists yet. */
flux_image *glyph_atlas_image(void);

/* Rebuild the atlas if it has crossed the hash load factor OR the packer has
 * run out of texture space. Must be called at the top of the panel render path
 * (before any frame command recording); it fences the device idle. Returns
 * true if a rebuild occurred. */
bool glyph_atlas_reclaim(void);

/* Occupied hash-slot count (live glyph entries). */
uint32_t glyph_atlas_entry_count(void);

/* Snapshot the diagnostics counters. */
void glyph_atlas_get_diag(GlyphAtlasDiag *out);

/* Release the atlas image, slots, and upload context (engine teardown). */
void glyph_atlas_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_GLYPH_ATLAS_H */
