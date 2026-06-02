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
 * Compact the glyph atlas hash table by removing entries no longer referenced
 * by any layout in @pc's LRU cache.
 *
 * The atlas uses an open-addressing hash table for glyph lookup.  Over time,
 * entries for evicted layouts accumulate as dead weight, degrading lookup from
 * O(1) toward O(n) via linear-probe chains.  Compaction rebuilds the table
 * with only live entries — O(live_glyphs) CPU work, no GPU involvement.
 *
 * Atlas texture pixels are NOT relocated; only the hash table is rebuilt.
 * Dead entries' texture space is abandoned (the atlas is large enough that
 * texture exhaustion is practically unreachable before hash degradation).
 *
 * Returns true if compaction was performed, false if skipped (load below
 * threshold or prerequisites not met).
 */
bool typio_text_atlas_compact(struct PanelRenderCtx *pc);

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
