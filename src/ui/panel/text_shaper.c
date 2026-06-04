/**
 * @file text_shaper.c
 * @brief Text shaper: HarfBuzz shaping + glyph-atlas draw, orchestrating the
 *        font_cache / font_resolve / glyph_atlas modules.
 *
 * This file owns only the TypioTextShape (a shaped glyph run) and the glue that
 * turns "string + font_desc" into one: it resolves a font (font_resolve), opens
 * it (font_cache), shapes it (HarfBuzz), assigns per-glyph fallbacks, and draws
 * via the shared coverage atlas (glyph_atlas). Each cache's bound / eviction /
 * reclamation / observability contract lives in its own module header.
 */
#include "text_shaper.h"
#include "glyph_atlas.h"
#include "font_cache.h"
#include "font_resolve.h"

#include <typio/abi/log.h>

#include <flux/flux.h>
#include <flux/canvas.h>

#include <fontconfig/fontconfig.h>
#include <harfbuzz/hb.h>
#include <harfbuzz/hb-ft.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include <math.h>
#include <stdlib.h>
#include <string.h>

#define MAX_FALLBACK_FONTS 4

typedef struct {
    uint32_t glyph_id;     /* FreeType face glyph index */
    uint32_t fb_glyph_id;  /* fallback face glyph index (0 = no fallback) */
    uint8_t  fb_idx;       /* index into fallback_faces[] (0-based) */
    uint32_t cluster;      /* HarfBuzz cluster index (byte offset in UTF-8 text) */
    float    x;            /* pen x at layout creation */
    float    y_offset;     /* y offset from HarfBuzz positioning */
} GlyphEntry;

struct TypioTextShape {
    GlyphEntry *glyphs;
    size_t      count;
    FT_Face     face;       /* borrowed; valid while font cache holds it */
    uint32_t    font_id;    /* identity of (face, size, weight) for atlas keys */
    FT_Face     fallback_faces[MAX_FALLBACK_FONTS];
    uint32_t    fallback_font_ids[MAX_FALLBACK_FONTS];
    uint8_t     fallback_count;
    float       size_px;    /* retained for atlas rasterisation of shared face */
    int32_t     weight;     /* retained for atlas rasterisation of shared face */
    float       width;
    float       height;
    float       baseline;
    char       *text;       /* owned copy for per-glyph fallback */
};

static unsigned char to_u8(float v)
{
    if (v <= 0.0f) return 0;
    if (v >= 1.0f) return 255;
    return (unsigned char)(v * 255.0f + 0.5f);
}

/* ── Shaping ─────────────────────────────────────────────────────────────── */

static bool layout_has_missing_glyphs(const TypioTextShape *layout)
{
    if (!layout || !layout->glyphs) return false;
    for (size_t i = 0; i < layout->count; ++i) {
        /* HarfBuzz glyph ID 0 is .notdef — missing glyph */
        if (layout->glyphs[i].glyph_id == 0) return true;
    }
    return false;
}

static void free_layout_internal(TypioTextShape *layout)
{
    if (!layout) return;
    /* Layouts own no GPU resource — glyph pixels live in the shared, persistent
     * glyph atlas, not per-layout. Freeing one is a pure CPU free. */
    free(layout->glyphs);
    free(layout->text);
    free(layout);
}

static TypioTextShape *shape_text(FontObj *font, const char *text)
{
    TypioTextShape *layout = (TypioTextShape *)calloc(1, sizeof(*layout));
    if (!layout) return NULL;

    layout->face    = font->face;
    layout->font_id = font->font_id;
    layout->fallback_count = 0;
    layout->text    = text ? strdup(text) : NULL;

    {
        float ascender  = (float)font->face->size->metrics.ascender  / 64.0f;
        float descender = (float)font->face->size->metrics.descender / 64.0f;
        layout->baseline = ascender;
        layout->height   = ascender - descender;
    }

    hb_buffer_t *hb = hb_buffer_create();
    if (!hb) goto fail;
    hb_buffer_add_utf8(hb, text ? text : "", -1, 0, -1);
    hb_buffer_guess_segment_properties(hb);
    hb_shape(font->hb_font, hb, NULL, 0);

    unsigned int count = 0;
    hb_glyph_info_t     *infos     = hb_buffer_get_glyph_infos(hb, &count);
    hb_glyph_position_t *positions = hb_buffer_get_glyph_positions(hb, &count);

    if (count > 0) {
        layout->glyphs = (GlyphEntry *)calloc(count, sizeof(GlyphEntry));
        if (!layout->glyphs) { hb_buffer_destroy(hb); goto fail; }
    }
    layout->count = count;

    float pen_x = 0.0f;
    for (unsigned int i = 0; i < count; ++i) {
        layout->glyphs[i].glyph_id    = infos[i].codepoint;
        layout->glyphs[i].fb_glyph_id = 0;
        layout->glyphs[i].cluster     = infos[i].cluster;
        layout->glyphs[i].x           = pen_x + (float)positions[i].x_offset / 64.0f;
        layout->glyphs[i].y_offset    = -(float)positions[i].y_offset / 64.0f;
        pen_x += (float)positions[i].x_advance / 64.0f;
    }
    layout->width = pen_x;
    hb_buffer_destroy(hb);
    return layout;

fail:
    free_layout_internal(layout);
    return NULL;
}

/* Assign per-glyph fallback faces for every .notdef in @layout. */
static void assign_fallbacks(TypioTextShape *layout, int32_t weight,
                             float size_px, const char *primary_path)
{
    FontObj *fb_objs[MAX_FALLBACK_FONTS];
    size_t   fb_count = 0;

    for (size_t i = 0; i < layout->count; i++) {
        if (layout->glyphs[i].glyph_id != 0) continue;

        const char *p = layout->text + layout->glyphs[i].cluster;
        if (!*p) continue;
        FcChar32 ch;
        int len = FcUtf8ToUcs4((const FcChar8 *)p, &ch, (int)strlen(p));
        if (len <= 0) continue;

        FontCandidateList candidates;
        font_resolve_codepoint_fonts(ch, weight, size_px, &candidates, primary_path);

        for (size_t k = 0; k < candidates.count; k++) {
            FontObj *fb_obj = candidates.objs[k];
            if (!fb_obj || !fb_obj->face) continue;

            FT_UInt fb_gid = FT_Get_Char_Index(fb_obj->face, ch);
            if (fb_gid == 0) continue;

            uint8_t fb_idx = UINT8_MAX;
            for (size_t j = 0; j < fb_count; j++) {
                if (fb_objs[j] == fb_obj) { fb_idx = (uint8_t)j; break; }
            }

            if (fb_idx == UINT8_MAX && fb_count < MAX_FALLBACK_FONTS) {
                fb_idx = (uint8_t)fb_count;
                fb_objs[fb_count] = fb_obj;
                layout->fallback_faces[fb_count]    = fb_obj->face;
                layout->fallback_font_ids[fb_count] = fb_obj->font_id;
                fb_count++;
            }

            if (fb_idx != UINT8_MAX) {
                layout->glyphs[i].fb_glyph_id = fb_gid;
                layout->glyphs[i].fb_idx = fb_idx;
                typio_log_debug("text_shaper: fallback U+%04X -> font[%u] glyph=%u via %s",
                               ch, fb_idx, fb_gid, candidates.paths[k]);
                break;
            }
        }

        font_candidate_list_clear(&candidates);
    }

    layout->fallback_count = (uint8_t)fb_count;
}

static TypioTextShape *create_layout(void *engine, const char *text,
                                     const char *font_desc)
{
    char family[128];
    float size_px;
    int32_t weight = 400;

    (void)engine;
    if (!font_resolve_parse_desc(font_desc, family, sizeof(family), &size_px, &weight))
        return NULL;

    char *font_file = font_resolve_file(family, weight);
    if (!font_file) return NULL;

    FontObj *font = font_cache_get_or_create(font_file, size_px, weight);
    if (!font) { free(font_file); return NULL; }

    TypioTextShape *layout = shape_text(font, text);
    if (!layout) { free(font_file); return NULL; }

    layout->size_px = size_px;
    layout->weight  = weight;

    if (layout_has_missing_glyphs(layout) && layout->text) {
        assign_fallbacks(layout, weight, size_px, font_file);
    }

    free(font_file);
    return layout;
}

static void get_metrics(TypioTextShape *layout, float *out_w, float *out_h)
{
    if (out_w) *out_w = layout ? layout->width  : 0.0f;
    if (out_h) *out_h = layout ? layout->height : 0.0f;
}

static float get_baseline(TypioTextShape *layout)
{
    return layout ? layout->baseline : 0.0f;
}

void typio_text_shape_free(TypioTextShape *layout)
{
    free_layout_internal(layout);
}

/* ── Draw ────────────────────────────────────────────────────────────────── */

bool typio_text_shape_fill(flux_canvas *canvas, flux_arena *arena,
                            TypioTextShape *layout, float x, float y,
                            TypioColor tint)
{
    (void)arena;
    if (!canvas || !layout || !layout->face || layout->count == 0) return true;

    flux_image *atlas = glyph_atlas_image();  /* builds on demand */
    if (!atlas) return true;  /* no device yet */

    /* Premultiplied tint; the atlas .r channel is the glyph alpha so the tint
     * is fully opaque (a = 255) and premultiplication is a no-op on colour. */
    flux_color col = flux_color_rgba_premul(to_u8(tint.r), to_u8(tint.g),
                                            to_u8(tint.b), 255);

    const float inv_dim = 1.0f / (float)GLYPH_ATLAS_DIM;

    for (size_t i = 0; i < layout->count; ++i) {
        uint32_t glyph_id = layout->glyphs[i].glyph_id;
        uint32_t font_id  = layout->font_id;
        FT_Face  face     = layout->face;

        if (glyph_id == 0 && layout->glyphs[i].fb_glyph_id != 0) {
            uint8_t fb_idx = layout->glyphs[i].fb_idx;
            if (fb_idx < layout->fallback_count) {
                glyph_id = layout->glyphs[i].fb_glyph_id;
                font_id  = layout->fallback_font_ids[fb_idx];
                face     = layout->fallback_faces[fb_idx];
            }
        }

        const GlyphSlot *g = glyph_atlas_get(font_id, face, glyph_id,
                                               layout->size_px, layout->weight);
        if (!g || !g->drawable) continue;

        /* Integer glyph placement relative to the (fractional) draw origin,
         * reproducing the previous whole-run rasteriser's pixel grid. */
        float dx = x + floorf(layout->glyphs[i].x) + g->left;
        float dy = y + floorf(layout->baseline + layout->glyphs[i].y_offset) - g->top;

        flux_rect dst = { dx, dy, (float)g->w, (float)g->h };
        flux_rect src = { (float)g->u * inv_dim, (float)g->v * inv_dim,
                          (float)g->w * inv_dim, (float)g->h * inv_dim };
        flux_canvas_draw_image_coverage_sub(canvas, atlas, dst, src, col);
    }
    return true;
}

/* ── Atlas facade (panel-facing; delegates to glyph_atlas) ───────────────── */

uint32_t typio_text_atlas_entry_count(void)
{
    return glyph_atlas_entry_count();
}

bool typio_text_atlas_compact(struct PanelRenderCtx *pc)
{
    (void)pc;   /* a full rebuild needs no live-set walk */
    return glyph_atlas_reclaim();
}

void typio_text_shaper_get_diag(TypioTextShaperDiag *out)
{
    if (!out) return;

    GlyphAtlasDiag a;
    glyph_atlas_get_diag(&a);
    out->atlas_live          = a.live;
    out->atlas_slot_capacity = a.slot_capacity;
    out->atlas_shelf_y       = a.shelf_y;
    out->atlas_dim           = a.dim;
    out->atlas_packer_full   = a.packer_full;
    out->atlas_rebuilds      = a.rebuilds;
    out->glyphs_rasterized   = a.rasterized;

    uint64_t hits = 0, misses = 0;
    font_resolve_get_diag(&hits, &misses);
    out->fb_resolve_hits   = hits;
    out->fb_resolve_misses = misses;
}

void typio_text_shaper_log_diag(const char *tag)
{
    TypioTextShaperDiag d;
    typio_text_shaper_get_diag(&d);
    unsigned long long fb_total = d.fb_resolve_hits + d.fb_resolve_misses;
    typio_log_debug("text_shaper diag [%s]: atlas live=%u/%u shelf=%u/%upx full=%d "
                    "rebuilds=%llu raster=%llu | fb_resolve hit=%llu miss=%llu (%llu total)",
                    tag ? tag : "",
                    d.atlas_live, d.atlas_slot_capacity,
                    d.atlas_shelf_y, d.atlas_dim, (int)d.atlas_packer_full,
                    d.atlas_rebuilds, d.glyphs_rasterized,
                    d.fb_resolve_hits, d.fb_resolve_misses, fb_total);
}

/* ── Engine lifecycle ────────────────────────────────────────────────────── */

static TypioTextShaperVTable flux_engine_vtable = {
    .create_layout = create_layout,
    .get_metrics   = get_metrics,
    .get_baseline  = get_baseline,
    .free_layout   = typio_text_shape_free,
};

TypioTextShaper *typio_text_shaper_create(void)
{
    TypioTextShaper *engine = (TypioTextShaper *)calloc(1, sizeof(*engine));
    if (!engine) return NULL;

    /* The text engine only needs FreeType (shaping + outlines). The Vulkan
     * device is created lazily by the panel when it first presents. */
    if (!font_cache_init()) {
        free(engine);
        return NULL;
    }

    engine->priv   = NULL;
    engine->vtable = &flux_engine_vtable;
    return engine;
}

void typio_text_shaper_destroy(TypioTextShaper *engine)
{
    if (!engine) return;
    glyph_atlas_shutdown();
    font_cache_clear();
    font_resolve_clear();
    free(engine);
}

void typio_text_shaper_purge_font_caches(void)
{
    /* The glyph atlas is intentionally NOT freed here. Cached panel layouts
     * borrow FT_Face pointers and carry a font_id; freeing the atlas would
     * force the next draw to re-rasterise via those faces, but this purge has
     * just freed them — a use-after-free on any reload that does not also
     * invalidate the layout cache (e.g. a reload that does not change the
     * font). The atlas is keyed on the monotonic font_id, so stale slots are
     * bounded dead weight reclaimed by the normal atlas rebuild path; it is
     * released only at engine teardown (device fully idle). */
    font_cache_clear();
    font_resolve_clear();
    /* Drain Fontconfig's internal caches.  In a long-running process these
     * grow monotonically because we never call FcFini() on the hot path.
     * This is safe to call from an idle or reload callback; any subsequent
     * font lookup will re-initialise Fontconfig lazily via FcInit(). */
    FcFini();
}
