/**
 * @file text_shaper.h
 * @brief Text shaper: turns a string + font into a colour-independent shape.
 *
 * The shaper (FreeType + HarfBuzz) produces a TypioTextShape — a glyph run
 * plus a colour-independent R8 coverage mask in the shared glyph atlas. Colour
 * is applied at draw time as a tint (see typio_text_shape_fill), so a shape is
 * neither tied to a colour nor part of its LRU cache identity.
 *
 * libtypio used to ship `typio/abi/renderer.h` so any host could plug a shaper
 * (cairo/skia/flux) into the panel composer. After the orphan-renderer cleanup
 * these types are no longer part of libtypio's public ABI and live here,
 * alongside the only host that uses them.
 */

#ifndef TYPIO_WL_TEXT_SHAPER_H
#define TYPIO_WL_TEXT_SHAPER_H

#include "typio_build_config.h"

#ifdef HAVE_FLUX
#include <flux/flux.h>
#endif

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioColor {
    float r;
    float g;
    float b;
    float a;
} TypioColor;

/* Opaque shaped-text handle owned by the shaper implementation. */
typedef struct TypioTextShape TypioTextShape;

typedef struct TypioTextShaperVTable {
    /* Build a colour-independent text shape. Colour is applied at draw time
     * (see typio_text_shape_fill's tint), so it is neither stored on the
     * shape nor part of its LRU cache identity. */
    TypioTextShape *(*create_layout)(void *shaper,
                                     const char *text,
                                     const char *font_desc);
    void (*get_metrics)(TypioTextShape *shape, float *out_w, float *out_h);
    float (*get_baseline)(TypioTextShape *shape);
    void (*free_layout)(TypioTextShape *shape);
} TypioTextShaperVTable;

typedef struct TypioTextShaper {
    void *priv;
    const TypioTextShaperVTable *vtable;
} TypioTextShaper;

#ifdef HAVE_FLUX

struct PanelRenderCtx;

TypioTextShaper *typio_text_shaper_create(void);
void typio_text_shaper_destroy(TypioTextShaper *shaper);

/* Purge all font caches (file, object, fallback) and drain Fontconfig's
 * internal caches.  Safe to call periodically from the host event loop or
 * after a configuration reload.  Subsequent shaping operations will
 * re-populate caches on demand. */
void typio_text_shaper_purge_font_caches(void);

/* Atlas entry count — for diagnostics and compaction threshold checks. */
uint32_t typio_text_atlas_entry_count(void);

/*
 * Reclaim the glyph atlas when it has accumulated too much dead weight.
 *
 * Two resources grow as distinct glyphs accumulate over a session: the
 * open-addressing lookup table (dead entries lengthen probe chains toward
 * O(n)) and the atlas TEXTURE itself (the shelf packer only advances, so
 * evicted glyphs' pixels are never reclaimed and the image eventually
 * saturates — after which new glyphs render blank). When either the hash
 * load factor crosses 75% OR the packer reports the image is full, the atlas
 * is fully rebuilt: image, slots, packer cursor, and live count are reset and
 * the next draw re-rasterises the visible page lazily into a fresh atlas.
 *
 * Must be called at the top of the panel render path (before any geometry or
 * command recording for the frame); it fences the device idle, so tearing down
 * the in-use image is safe only there. @pc is currently unused (a full rebuild
 * needs no live-set walk) but retained for API stability.
 *
 * Returns true if a rebuild was performed, false if skipped.
 */
bool typio_text_atlas_compact(struct PanelRenderCtx *pc);

/*
 * Diagnostics snapshot for the glyph/font layer. Lets the host correlate panel
 * stalls with atlas saturation, rebuild churn, and fallback-resolution cost.
 */
typedef struct TypioTextShaperDiag {
    uint32_t atlas_live;          /* occupied hash slots right now             */
    uint32_t atlas_slot_capacity; /* total hash slots                          */
    uint32_t atlas_shelf_y;       /* packer vertical fill, physical px         */
    uint32_t atlas_dim;           /* atlas side length, physical px            */
    bool     atlas_packer_full;   /* image saturated, awaiting reclaim         */
    unsigned long long atlas_rebuilds;     /* cumulative reclaim count          */
    unsigned long long glyphs_rasterized;  /* cumulative FT_Load_Glyph inserts  */
    unsigned long long fb_resolve_hits;    /* per-codepoint memo hits           */
    unsigned long long fb_resolve_misses;  /* FcFontSort runs (memo misses)     */
    unsigned long long atlas_flushes;      /* batched flushes with ≥1 region    */
    unsigned long long atlas_flush_total_regions; /* cumulative regions flushed */
    uint32_t           atlas_flush_peak_batch;     /* largest single batch       */
    uint32_t           font_obj_count;     /* current FontObj table occupancy   */
    uint32_t           font_obj_cap;       /* FONT_OBJ_CACHE_CAP                */
    uint32_t           font_face_count;    /* unique FT_Faces opened            */
    unsigned long long font_obj_evictions; /* cumulative LRU evictions           */
    unsigned long long fc_purge_count;     /* cumulative FcFini() drains         */
} TypioTextShaperDiag;

/* Fill @out with the current diagnostics counters. */
void typio_text_shaper_get_diag(TypioTextShaperDiag *out);

/* Emit a one-line debug log of the diagnostics, prefixed with @tag. */
void typio_text_shaper_log_diag(const char *tag);

/*
 * Record a shaped text run into a flux canvas as a tinted coverage blit.
 *
 * The shape's glyphs reference the shared, colour-independent R8 coverage
 * atlas; `tint` supplies the colour at draw time, so the same shape can be
 * drawn in any colour (normal / muted / selection) without re-shaping or
 * re-uploading. Must be called between flux_canvas_begin / flux_canvas_end.
 *
 * x, y : top-left origin of the run in surface pixels (baseline is added
 *        internally from the shape metrics).
 */
bool typio_text_shape_fill(flux_canvas *canvas, flux_arena *arena,
                           TypioTextShape *shape, float x, float y,
                           TypioColor tint);

void typio_text_shape_free(TypioTextShape *shape);
#endif /* HAVE_FLUX */

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_TEXT_SHAPER_H */
