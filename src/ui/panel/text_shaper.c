#include "text_shaper.h"
#include "device.h"
#include "fallback_cache.h"
#include "glyph_pack.h"

#include <flux/flux.h>
#include <flux/vulkan.h>

#include <fontconfig/fontconfig.h>
#include <harfbuzz/hb.h>
#include <harfbuzz/hb-ft.h>
#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_MULTIPLE_MASTERS_H

#include <flux/canvas.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

typedef struct {
    uint32_t glyph_id;   /* FreeType face glyph index */
    float    x;          /* pen x at layout creation */
    float    y_offset;   /* y offset from HarfBuzz positioning */
} GlyphEntry;

struct TypioTextShape {
    GlyphEntry *glyphs;
    size_t      count;
    FT_Face     face;     /* borrowed; valid while font cache holds it */
    uint32_t    font_id;  /* identity of (face, size, weight) for atlas keys */
    float       width;
    float       height;
    float       baseline;
};

static FT_Library   ft_library;

static bool ensure_ft_library(void)
{
    if (ft_library) return true;
    if (FT_Init_FreeType(&ft_library) != 0) {
        ft_library = NULL;
        return false;
    }
    return true;
}

/* ── Font file cache ────────────────────────────────────────────────── */
#define FONT_FILE_CACHE_CAP 32

typedef struct {
    char    family[128];
    int32_t weight;
    char   *path;
} FontFileEntry;

static FontFileEntry font_file_cache[FONT_FILE_CACHE_CAP];
static size_t        font_file_cache_count = 0;

static void font_file_cache_clear(void)
{
    for (size_t i = 0; i < font_file_cache_count; ++i) {
        free(font_file_cache[i].path);
        font_file_cache[i].path = NULL;
        font_file_cache[i].family[0] = '\0';
        font_file_cache[i].weight = 400;
    }
    font_file_cache_count = 0;
}

static char *font_file_cache_lookup(const char *family, int32_t weight)
{
    for (size_t i = 0; i < font_file_cache_count; ++i) {
        if (font_file_cache[i].weight == weight &&
            strcmp(font_file_cache[i].family, family) == 0) {
            return strdup(font_file_cache[i].path);
        }
    }
    return NULL;
}

static void font_file_cache_insert(const char *family, int32_t weight, const char *path)
{
    if (font_file_cache_count < FONT_FILE_CACHE_CAP) {
        FontFileEntry *e = &font_file_cache[font_file_cache_count++];
        snprintf(e->family, sizeof(e->family), "%s", family);
        e->weight = weight;
        e->path = strdup(path);
    } else {
        free(font_file_cache[0].path);
        for (size_t i = 1; i < FONT_FILE_CACHE_CAP; ++i)
            font_file_cache[i - 1] = font_file_cache[i];
        FontFileEntry *e = &font_file_cache[FONT_FILE_CACHE_CAP - 1];
        snprintf(e->family, sizeof(e->family), "%s", family);
        e->weight = weight;
        e->path = strdup(path);
    }
}

/* ── Font object cache (FT_Face + hb_font_t) ────────────────────────── */
/*
 * A font object (FT_Face + hb_font_t) must outlive every TypioTextShape and
 * every glyph-atlas entry that references it: layouts hold a raw FT_Face* and
 * the atlas is keyed by font_id and re-rasterises on miss. Freeing a face that
 * is still referenced is a use-after-free that renders glyphs blank.
 *
 * Distinct (path, size, weight) tuples are a bounded resource in practice
 * (a handful of system fonts × a few sizes × a few weights), so this cache
 * GROWS on demand and never frees a face at runtime — entries are released
 * only by font_obj_cache_clear() at shutdown. This deliberately replaces an
 * earlier fixed 64-slot LRU that FT_Done_Face'd the evicted slot 0, which —
 * since the primary font sat permanently at slot 0 — freed the in-use primary
 * face the moment a fallback font pushed the cache over capacity.
 */
#define FONT_OBJ_CACHE_INIT_CAP 64

typedef struct {
    char      *path;
    float      size;
    int32_t    weight;
    FT_Face    face;
    hb_font_t *hb_font;
    uint32_t   font_id;
} FontObjEntry;

static FontObjEntry *font_obj_cache = NULL;
static size_t        font_obj_cache_count = 0;
static size_t        font_obj_cache_cap = 0;
static uint32_t      next_font_id = 1;

static void font_obj_cache_clear(void)
{
    for (size_t i = 0; i < font_obj_cache_count; ++i) {
        if (font_obj_cache[i].hb_font)
            hb_font_destroy(font_obj_cache[i].hb_font);
        if (font_obj_cache[i].face)
            FT_Done_Face(font_obj_cache[i].face);
        free(font_obj_cache[i].path);
    }
    free(font_obj_cache);
    font_obj_cache = NULL;
    font_obj_cache_count = 0;
    font_obj_cache_cap = 0;
}

static FontObjEntry *font_obj_cache_lookup(const char *path, float size, int32_t weight)
{
    for (size_t i = 0; i < font_obj_cache_count; ++i) {
        if (font_obj_cache[i].size == size &&
            font_obj_cache[i].weight == weight &&
            strcmp(font_obj_cache[i].path, path) == 0) {
            return &font_obj_cache[i];
        }
    }
    return NULL;
}

/* Returns false only on allocation failure (caller then disposes the face). */
static bool font_obj_cache_insert(const char *path, float size, int32_t weight,
                                  FT_Face face, hb_font_t *hb_font, uint32_t font_id)
{
    if (font_obj_cache_count == font_obj_cache_cap) {
        size_t newcap = font_obj_cache_cap ? font_obj_cache_cap * 2
                                           : FONT_OBJ_CACHE_INIT_CAP;
        FontObjEntry *grown =
            realloc(font_obj_cache, newcap * sizeof(*grown));
        if (!grown) {
            return false;
        }
        font_obj_cache = grown;
        font_obj_cache_cap = newcap;
    }

    FontObjEntry *e = &font_obj_cache[font_obj_cache_count++];
    e->path = strdup(path);
    e->size = size;
    e->weight = weight;
    e->face = face;
    e->hb_font = hb_font;
    e->font_id = font_id;
    return true;
}

/* Forward declarations — defined later in this file. */
static char *find_fallback_font(const char *text, int32_t weight,
                                FcCharSet **out_coverage);
static bool text_has_non_ascii(const char *text);

/* ── Fallback font cache ───────────────────────────────────────────────
 *
 * Resolving a fallback font (FcFontSort + per-codepoint coverage scan) is
 * expensive and runs synchronously on the IME event loop ahead of the panel
 * present.  The coverage-keyed LRU in fallback_cache.{c,h} keeps the resolve
 * to ~once per script even under an unbounded stream of distinct CJK phrases;
 * here we just own a process-global instance and feed it the FreeType-backed
 * resolver below. */
#define FALLBACK_FONT_CACHE_CAP 16

static FallbackFontCache *fallback_cache = NULL;

static char *fallback_resolve(void *user, const char *text, int32_t weight,
                              FcCharSet **out_coverage)
{
    (void)user;
    return find_fallback_font(text, weight, out_coverage);
}

static void fallback_font_cache_clear(void)
{
    if (fallback_cache) {
        fallback_cache_free(fallback_cache);
        fallback_cache = NULL;
    }
}

static char *find_fallback_font_cached(const char *text, int32_t weight)
{
    if (!text || !text[0]) return NULL;
    if (!text_has_non_ascii(text)) return NULL;
    if (!FcInit()) return NULL;

    if (!fallback_cache) {
        fallback_cache = fallback_cache_new(FALLBACK_FONT_CACHE_CAP,
                                            fallback_resolve, NULL);
    }
    if (!fallback_cache) return find_fallback_font(text, weight, NULL);

    return fallback_cache_lookup(fallback_cache, text, weight);
}

static bool set_face_weight(FT_Face face, int32_t weight)
{
    FT_MM_Var *amaster = NULL;
    FT_Fixed  *coords  = NULL;
    FT_Error   err;
    FT_UInt    i;
    bool       ok = false;

    err = FT_Get_MM_Var(face, &amaster);
    if (err != 0) return false;

    coords = (FT_Fixed *)calloc(amaster->num_axis, sizeof(FT_Fixed));
    if (!coords) goto done;

    err = FT_Get_Var_Design_Coordinates(face, amaster->num_axis, coords);
    if (err != 0) goto done;

    for (i = 0; i < amaster->num_axis; ++i) {
        if (amaster->axis[i].tag == ((FT_ULong)'w' << 24 |
                                     (FT_ULong)'g' << 16 |
                                     (FT_ULong)'h' << 8  | 't')) {
            coords[i] = (FT_Fixed)weight * 65536;
            ok = true;
            break;
        }
    }

    if (ok) {
        err = FT_Set_Var_Design_Coordinates(face, amaster->num_axis, coords);
        ok = (err == 0);
    }

done:
    free(coords);
    FT_Done_MM_Var(ft_library, amaster);
    return ok;
}

static FontObjEntry *get_or_create_font(const char *path, float size, int32_t weight)
{
    FontObjEntry *entry = font_obj_cache_lookup(path, size, weight);
    if (entry) return entry;

    FT_Face face = NULL;
    if (FT_New_Face(ft_library, path, 0, &face) != 0) return NULL;

    set_face_weight(face, weight);

    if (FT_Set_Pixel_Sizes(face, 0, (FT_UInt)(size + 0.5f)) != 0) {
        FT_Done_Face(face);
        return NULL;
    }

    hb_font_t *hb_font = hb_ft_font_create_referenced(face);
    if (!hb_font) {
        FT_Done_Face(face);
        return NULL;
    }

    uint32_t font_id = next_font_id++;
    if (!font_obj_cache_insert(path, size, weight, face, hb_font, font_id)) {
        hb_font_destroy(hb_font);
        FT_Done_Face(face);
        return NULL;
    }
    return font_obj_cache_lookup(path, size, weight);
}

/* ── Helpers ────────────────────────────────────────────────────────── */

static unsigned char to_u8(float v)
{
    if (v <= 0.0f) return 0;
    if (v >= 1.0f) return 255;
    return (unsigned char)(v * 255.0f + 0.5f);
}

static int32_t parse_weight_keyword(const char *s, size_t len)
{
    if (len == 6 && strncasecmp(s, "Medium", 6) == 0)      return 500;
    if (len == 4 && strncasecmp(s, "Bold", 4) == 0)        return 700;
    if (len == 6 && strncasecmp(s, "Normal", 6) == 0)      return 400;
    if (len == 7 && strncasecmp(s, "Regular", 7) == 0)     return 400;
    if (len == 5 && strncasecmp(s, "Light", 5) == 0)       return 300;
    if (len == 4 && strncasecmp(s, "Thin", 4) == 0)        return 100;
    if (len == 9 && strncasecmp(s, "ExtraBold", 9) == 0)   return 800;
    if (len == 5 && strncasecmp(s, "Black", 5) == 0)       return 900;
    if (len == 8 && strncasecmp(s, "SemiBold", 8) == 0)    return 600;
    if (len == 10 && strncasecmp(s, "ExtraLight", 10) == 0) return 200;
    {
        int v = atoi(s);
        if (v >= 100 && v <= 1000) return v;
    }
    return 0;
}

static bool parse_font_desc(const char *font_desc,
                            char *family,
                            size_t family_size,
                            float *size,
                            int32_t *weight)
{
    if (!family || family_size == 0 || !size) return false;

    snprintf(family, family_size, "Sans");
    *size = 16.0f;
    if (weight) *weight = 400;

    if (!font_desc || !font_desc[0]) return true;

    const char *last_space = strrchr(font_desc, ' ');
    if (!last_space || !last_space[1]) {
        snprintf(family, family_size, "%s", font_desc);
        return true;
    }

    float parsed = (float)atof(last_space + 1);
    if (parsed <= 0.0f) {
        snprintf(family, family_size, "%s", font_desc);
        return true;
    }
    *size = parsed * (96.0f / 72.0f);

    const char *family_end = last_space;

    if (last_space > font_desc) {
        const char *p = last_space - 1;
        while (p > font_desc && *p != ' ') p--;
        if (*p == ' ') {
            const char *wstart = p + 1;
            size_t wlen = (size_t)(last_space - wstart);
            int32_t w = parse_weight_keyword(wstart, wlen);
            if (w > 0) {
                if (weight) *weight = w;
                family_end = p;
            }
        }
    }

    size_t flen = (size_t)(family_end - font_desc);
    if (flen >= family_size) flen = family_size - 1;
    memcpy(family, font_desc, flen);
    family[flen] = '\0';
    return true;
}

static char *match_font_file(const char *family, int32_t weight)
{
    char *cached = font_file_cache_lookup(family, weight);
    if (cached) return cached;

    if (!FcInit()) return NULL;

    FcPattern *pat = FcPatternCreate();
    if (!pat) return NULL;

    FcPatternAddString(pat, FC_FAMILY,
                       (const FcChar8 *)(family && family[0] ? family : "Sans"));
    int fc_weight = FC_WEIGHT_REGULAR;
    if (weight >= 900)      fc_weight = FC_WEIGHT_BLACK;
    else if (weight >= 800) fc_weight = FC_WEIGHT_EXTRABOLD;
    else if (weight >= 700) fc_weight = FC_WEIGHT_BOLD;
    else if (weight >= 600) fc_weight = FC_WEIGHT_DEMIBOLD;
    else if (weight >= 500) fc_weight = FC_WEIGHT_MEDIUM;
    else if (weight >= 400) fc_weight = FC_WEIGHT_REGULAR;
    else if (weight >= 300) fc_weight = FC_WEIGHT_LIGHT;
    else if (weight >= 200) fc_weight = FC_WEIGHT_EXTRALIGHT;
    else                    fc_weight = FC_WEIGHT_THIN;
    FcPatternAddInteger(pat, FC_WEIGHT, fc_weight);
    FcConfigSubstitute(NULL, pat, FcMatchPattern);
    FcDefaultSubstitute(pat);

    FcResult fc_result;
    FcPattern *match = FcFontMatch(NULL, pat, &fc_result);
    char *result = NULL;
    if (match) {
        FcChar8 *file = NULL;
        if (FcPatternGetString(match, FC_FILE, 0, &file) == FcResultMatch && file) {
            result = strdup((const char *)file);
        }
        FcPatternDestroy(match);
    }
    FcPatternDestroy(pat);

    if (result) {
        font_file_cache_insert(family, weight, result);
    }
    return result;
}

static bool text_has_non_ascii(const char *text)
{
    const unsigned char *p = (const unsigned char *)text;
    while (*p) {
        if (*p > 127) return true;
        p++;
    }
    return false;
}

static char *find_fallback_font(const char *text, int32_t weight,
                                FcCharSet **out_coverage)
{
    if (out_coverage) *out_coverage = NULL;
    if (!text || !text[0]) return NULL;
    if (!text_has_non_ascii(text)) return NULL;
    if (!FcInit()) return NULL;

    FcPattern *pat = FcPatternCreate();
    if (!pat) return NULL;

    FcCharSet *cs = FcCharSetCreate();
    const char *p = text;
    while (*p) {
        FcChar32 ch;
        int len = FcUtf8ToUcs4((const FcChar8 *)p, &ch, (int)strlen(p));
        if (len <= 0) break;
        FcCharSetAddChar(cs, ch);
        p += len;
    }

    int fc_weight = FC_WEIGHT_REGULAR;
    if (weight >= 900)      fc_weight = FC_WEIGHT_BLACK;
    else if (weight >= 800) fc_weight = FC_WEIGHT_EXTRABOLD;
    else if (weight >= 700) fc_weight = FC_WEIGHT_BOLD;
    else if (weight >= 600) fc_weight = FC_WEIGHT_DEMIBOLD;
    else if (weight >= 500) fc_weight = FC_WEIGHT_MEDIUM;
    else if (weight >= 400) fc_weight = FC_WEIGHT_REGULAR;
    else if (weight >= 300) fc_weight = FC_WEIGHT_LIGHT;
    else if (weight >= 200) fc_weight = FC_WEIGHT_EXTRALIGHT;
    else                    fc_weight = FC_WEIGHT_THIN;
    FcPatternAddInteger(pat, FC_WEIGHT, fc_weight);
    FcConfigSubstitute(NULL, pat, FcMatchPattern);
    FcDefaultSubstitute(pat);

    FcResult fc_result;
    FcFontSet *set = FcFontSort(NULL, pat, FcTrue, NULL, &fc_result);
    char *result = NULL;

    if (set) {
        for (int i = 0; i < set->nfont; i++) {
            FcPattern *font = set->fonts[i];
            FcCharSet *font_cs = NULL;
            if (FcPatternGetCharSet(font, FC_CHARSET, 0, &font_cs) == FcResultMatch && font_cs) {
                bool covers_all = true;
                const char *cp = text;
                while (*cp) {
                    FcChar32 ch;
                    int len = FcUtf8ToUcs4((const FcChar8 *)cp, &ch, (int)strlen(cp));
                    if (len <= 0) break;
                    if (!FcCharSetHasChar(font_cs, ch)) {
                        covers_all = false;
                        break;
                    }
                    cp += len;
                }
                if (covers_all) {
                    FcChar8 *file = NULL;
                    if (FcPatternGetString(font, FC_FILE, 0, &file) == FcResultMatch && file) {
                        result = strdup((const char *)file);
                        if (out_coverage) *out_coverage = FcCharSetCopy(font_cs);
                        break;
                    }
                }
            }
        }
        FcFontSetDestroy(set);
    }

    FcCharSetDestroy(cs);
    FcPatternDestroy(pat);
    return result;
}

static bool layout_has_missing_glyphs(const TypioTextShape *layout)
{
    if (!layout || !layout->glyphs) return false;
    for (size_t i = 0; i < layout->count; ++i) {
        /* HarfBuzz glyph ID 0 is .notdef — missing glyph */
        if (layout->glyphs[i].glyph_id == 0) return true;
    }
    return false;
}

static void flux_free_layout_internal(TypioTextShape *layout)
{
    if (!layout) return;
    /* Layouts own no GPU resource — glyph pixels live in the shared, persistent
     * glyph atlas, not per-layout. Freeing one is a pure CPU free. */
    free(layout->glyphs);
    free(layout);
}

static TypioTextShape *flux_shape_text(FontObjEntry *font,
                                        const char *text)
{
    TypioTextShape *layout = (TypioTextShape *)calloc(1, sizeof(*layout));
    if (!layout) return NULL;

    layout->face    = font->face;
    layout->font_id = font->font_id;

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
        layout->glyphs[i].glyph_id = infos[i].codepoint;
        layout->glyphs[i].x        = pen_x + (float)positions[i].x_offset / 64.0f;
        layout->glyphs[i].y_offset = -(float)positions[i].y_offset / 64.0f;
        pen_x += (float)positions[i].x_advance / 64.0f;
    }
    layout->width = pen_x;
    hb_buffer_destroy(hb);
    return layout;

fail:
    flux_free_layout_internal(layout);
    return NULL;
}

static TypioTextShape *flux_create_layout(void *engine,
                                           const char *text,
                                           const char *font_desc)
{
    char family[128];
    char *font_file = NULL;
    char *fb_file   = NULL;
    float size_px;
    FontObjEntry *font = NULL;
    TypioTextShape *layout    = NULL;
    TypioTextShape *fb_layout = NULL;
    int32_t weight = 400;

    (void)engine;
    if (!parse_font_desc(font_desc, family, sizeof(family), &size_px, &weight)) return NULL;

    font_file = match_font_file(family, weight);
    if (!font_file) return NULL;

    font = get_or_create_font(font_file, size_px, weight);
    if (!font) goto fail;

    layout = flux_shape_text(font, text);
    if (!layout) goto fail;

    if (layout_has_missing_glyphs(layout)) {
        fb_file = find_fallback_font_cached(text, weight);
        if (fb_file && strcmp(fb_file, font_file) != 0) {
            FontObjEntry *fb_font = get_or_create_font(fb_file, size_px, weight);
            if (fb_font) {
                fb_layout = flux_shape_text(fb_font, text);
                if (fb_layout && !layout_has_missing_glyphs(fb_layout)) {
                    flux_free_layout_internal(layout);
                    layout    = fb_layout;
                    fb_layout = NULL;
                } else {
                    flux_free_layout_internal(fb_layout);
                    fb_layout = NULL;
                }
            }
        }
        free(fb_file);
    }

    free(font_file);
    return layout;

fail:
    free(font_file);
    flux_free_layout_internal(layout);
    flux_free_layout_internal(fb_layout);
    return NULL;
}

static void flux_get_metrics(TypioTextShape *layout, float *out_w, float *out_h)
{
    if (out_w) *out_w = layout ? layout->width    : 0.0f;
    if (out_h) *out_h = layout ? layout->height   : 0.0f;
}

static float flux_get_baseline(TypioTextShape *layout)
{
    return layout ? layout->baseline : 0.0f;
}

void typio_text_shape_free(TypioTextShape *layout)
{
    flux_free_layout_internal(layout);
}

/* ── Glyph atlas (shared, persistent R8 coverage texture) ────────────────
 *
 * Every glyph is rasterised by FreeType ONCE, packed into a single long-lived
 * atlas texture, and thereafter referenced by a sub-rectangle. Text draws as
 * one tinted quad per glyph sampling that sub-rect. This is the standard
 * text-rendering architecture and the fix for the candidate-switch stall:
 *
 *   The old design built and *synchronously uploaded* a whole-text-run R8
 *   texture on every content change (flux_image_create → submit_one_shot_and
 *   _wait → vkWaitForFences). A candidate page = ~20 such blocking uploads on
 *   the single-threaded IME event loop. Holding the arrow key paged faster
 *   than the compositor retired frames, so the loop fell visibly behind —
 *   independent of which graphics library was used, because the anti-pattern
 *   is "new text run ⇒ new texture ⇒ synchronous upload".
 *
 * With the atlas, CJK glyphs are shared across every candidate and page, so a
 * warmed atlas uploads NOTHING during navigation. Colour stays a draw-time
 * tint (coverage × tint), so selection re-tints without any GPU work. */

#define GLYPH_ATLAS_DIM   2048u   /* R8 → 4 MiB; thousands of glyphs           */
#define GLYPH_ATLAS_PAD   1u      /* transparent gutter to stop bilinear bleed */
#define GLYPH_SLOT_CAP    32768u  /* power of two; comfortably exceeds how many
                                   * glyphs the atlas image can hold, so the
                                   * open-addressed table stays sparse (fast)   */

typedef struct {
    uint64_t key;          /* (font_id << 32) | glyph_id; 0 == empty slot     */
    uint16_t u, v, w, h;   /* atlas sub-rect, pixels                          */
    int16_t  left, top;    /* FreeType bearings                               */
    bool     occupied;
    bool     drawable;     /* false for whitespace / load failure / no fit    */
} GlyphSlot;

typedef struct {
    flux_image  *image;
    GlyphSlot   *slots;     /* GLYPH_SLOT_CAP entries                          */
    GlyphPacker  packer;    /* skyline shelf cursor (see glyph_pack.h)         */
} GlyphAtlas;

static GlyphAtlas g_atlas;

/* ── Persistent glyph upload context ────────────────────────────────────
 *
 * Reuses a command pool, staging buffer, and fence across glyph uploads
 * instead of creating/destroying them per upload (the default path in
 * flux_vk_upload_to_image). This eliminates ~50us of per-glyph driver
 * overhead from VkCommandPool + VkBuffer + VkFence create/destroy.
 *
 * The upload is synchronous from the CPU's perspective (fence-wait after
 * submit) — the fence is reused via vkResetFences instead of being
 * recreated, and the command pool is reset via vkResetCommandPool instead
 * of being destroyed and recreated. The staging buffer is grown as needed
 * and reused across calls. */

#define UPLOAD_STAGING_INITIAL  (16u * 1024u)  /* 16 KiB — covers most CJK glyphs */

typedef struct {
    VkCommandPool   pool;
    VkCommandBuffer cmd;
    VkFence         fence;
    VkBuffer        staging;
    void           *staging_mapped;
    VkDeviceSize    staging_size;
    VkDeviceMemory  staging_memory;
    bool            initialized;
} GlyphUploadCtx;

static GlyphUploadCtx g_upload_ctx;

static void glyph_upload_ctx_destroy(void)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev || !ctx->initialized) {
        ctx->initialized = false;
        return;
    }
    VkDevice vkd = flux_device_vk_device(dev);
    if (ctx->fence)         vkDestroyFence(vkd, ctx->fence, nullptr);
    if (ctx->cmd)           { /* freed with pool */ }
    if (ctx->pool)          vkDestroyCommandPool(vkd, ctx->pool, nullptr);
    if (ctx->staging)       vkDestroyBuffer(vkd, ctx->staging, nullptr);
    if (ctx->staging_memory) vkFreeMemory(vkd, ctx->staging_memory, nullptr);
    *ctx = (GlyphUploadCtx){0};
}

static bool glyph_upload_ctx_ensure(void)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    if (ctx->initialized) return true;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);
    uint32_t gfx_family = flux_device_vk_graphics_family(dev);

    VkCommandPoolCreateInfo pci = {
        .sType            = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = gfx_family,
        .flags            = VK_COMMAND_POOL_CREATE_TRANSIENT_BIT |
                            VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    };
    if (vkCreateCommandPool(vkd, &pci, nullptr, &ctx->pool) != VK_SUCCESS)
        goto fail;

    VkCommandBufferAllocateInfo cbai = {
        .sType              = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool        = ctx->pool,
        .level              = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    if (vkAllocateCommandBuffers(vkd, &cbai, &ctx->cmd) != VK_SUCCESS)
        goto fail;

    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    if (vkCreateFence(vkd, &fci, nullptr, &ctx->fence) != VK_SUCCESS)
        goto fail;

    /* Create initial staging buffer. Grown lazily if a glyph exceeds this. */
    ctx->staging_size = UPLOAD_STAGING_INITIAL;
    VkBufferCreateInfo bci = {
        .sType       = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size        = ctx->staging_size,
        .usage       = VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    if (vkCreateBuffer(vkd, &bci, nullptr, &ctx->staging) != VK_SUCCESS)
        goto fail;

    VkMemoryRequirements mr;
    vkGetBufferMemoryRequirements(vkd, ctx->staging, &mr);

    VkPhysicalDevice phys = flux_device_vk_physical_device(dev);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(phys, &mp);

    uint32_t mem_type = UINT32_MAX;
    uint32_t wanted = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                      VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (uint32_t i = 0; i < mp.memoryTypeCount; ++i) {
        if ((mr.memoryTypeBits & (1u << i)) &&
            (mp.memoryTypes[i].propertyFlags & wanted) == wanted) {
            mem_type = i;
            break;
        }
    }
    if (mem_type == UINT32_MAX) goto fail;

    VkMemoryAllocateInfo mai = {
        .sType           = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize  = mr.size,
        .memoryTypeIndex = mem_type,
    };
    if (vkAllocateMemory(vkd, &mai, nullptr, &ctx->staging_memory) != VK_SUCCESS)
        goto fail;
    if (vkBindBufferMemory(vkd, ctx->staging, ctx->staging_memory, 0) != VK_SUCCESS)
        goto fail;
    if (vkMapMemory(vkd, ctx->staging_memory, 0, VK_WHOLE_SIZE, 0,
                    &ctx->staging_mapped) != VK_SUCCESS)
        goto fail;

    ctx->initialized = true;
    return true;

fail:
    glyph_upload_ctx_destroy();
    return false;
}

static bool glyph_upload_ctx_grow_staging(VkDeviceSize needed)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);

    if (ctx->staging_mapped) vkUnmapMemory(vkd, ctx->staging_memory);
    vkDestroyBuffer(vkd, ctx->staging, nullptr);
    vkFreeMemory(vkd, ctx->staging_memory, nullptr);

    VkDeviceSize new_size = ctx->staging_size;
    while (new_size < needed) new_size *= 2;
    ctx->staging_size = new_size;

    VkBufferCreateInfo bci = {
        .sType       = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size        = new_size,
        .usage       = VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    if (vkCreateBuffer(vkd, &bci, nullptr, &ctx->staging) != VK_SUCCESS)
        return false;

    VkMemoryRequirements mr;
    vkGetBufferMemoryRequirements(vkd, ctx->staging, &mr);

    VkPhysicalDevice phys = flux_device_vk_physical_device(dev);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(phys, &mp);

    uint32_t mem_type = UINT32_MAX;
    uint32_t wanted = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                      VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (uint32_t i = 0; i < mp.memoryTypeCount; ++i) {
        if ((mr.memoryTypeBits & (1u << i)) &&
            (mp.memoryTypes[i].propertyFlags & wanted) == wanted) {
            mem_type = i;
            break;
        }
    }
    if (mem_type == UINT32_MAX) return false;

    VkMemoryAllocateInfo mai = {
        .sType           = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize  = mr.size,
        .memoryTypeIndex = mem_type,
    };
    if (vkAllocateMemory(vkd, &mai, nullptr, &ctx->staging_memory) != VK_SUCCESS)
        return false;
    if (vkBindBufferMemory(vkd, ctx->staging, ctx->staging_memory, 0) != VK_SUCCESS)
        return false;
    if (vkMapMemory(vkd, ctx->staging_memory, 0, VK_WHOLE_SIZE, 0,
                    &ctx->staging_mapped) != VK_SUCCESS)
        return false;

    return true;
}

/* Upload a sub-region of the atlas image using the persistent context.
 * Same GPU result as flux_image_update_region but ~50% less CPU overhead
 * per call (no per-upload command pool / buffer / fence create+destroy). */
static bool glyph_upload_region(flux_image *img,
                                 uint32_t x, uint32_t y,
                                 uint32_t w, uint32_t h,
                                 const void *data, size_t bytes)
{
    if (!glyph_upload_ctx_ensure()) return false;

    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);
    VkQueue  gfx_queue = flux_device_vk_graphics_queue(dev);

    if ((VkDeviceSize)bytes > ctx->staging_size) {
        if (!glyph_upload_ctx_grow_staging((VkDeviceSize)bytes))
            return false;
    }

    memcpy(ctx->staging_mapped, data, bytes);

    vkResetCommandPool(vkd, ctx->pool, 0);

    VkCommandBufferBeginInfo cbbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    vkBeginCommandBuffer(ctx->cmd, &cbbi);

    VkImage dst = flux_image_vk_image(img);

    VkImageMemoryBarrier2 to_dst = {
        .sType         = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        .srcStageMask  = VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        .srcAccessMask = VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        .dstStageMask  = VK_PIPELINE_STAGE_2_COPY_BIT,
        .dstAccessMask = VK_ACCESS_2_TRANSFER_WRITE_BIT,
        .oldLayout     = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        .newLayout     = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .image         = dst,
        .subresourceRange = {
            .aspectMask = VK_IMAGE_ASPECT_COLOR_BIT,
            .levelCount = 1, .layerCount = 1,
        },
    };
    VkDependencyInfo di = {
        .sType = VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        .imageMemoryBarrierCount = 1,
        .pImageMemoryBarriers = &to_dst,
    };
    vkCmdPipelineBarrier2(ctx->cmd, &di);

    VkBufferImageCopy region = {
        .imageSubresource = {
            .aspectMask     = VK_IMAGE_ASPECT_COLOR_BIT,
            .mipLevel       = 0,
            .baseArrayLayer = 0, .layerCount = 1,
        },
        .imageOffset = { (int32_t)x, (int32_t)y, 0 },
        .imageExtent = { w, h, 1 },
    };
    vkCmdCopyBufferToImage(ctx->cmd, ctx->staging, dst,
                            VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);

    VkImageMemoryBarrier2 to_shader = {
        .sType         = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        .srcStageMask  = VK_PIPELINE_STAGE_2_COPY_BIT,
        .srcAccessMask = VK_ACCESS_2_TRANSFER_WRITE_BIT,
        .dstStageMask  = VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        .dstAccessMask = VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        .oldLayout     = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .newLayout     = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        .image         = dst,
        .subresourceRange = {
            .aspectMask = VK_IMAGE_ASPECT_COLOR_BIT,
            .levelCount = 1, .layerCount = 1,
        },
    };
    di.pImageMemoryBarriers = &to_shader;
    vkCmdPipelineBarrier2(ctx->cmd, &di);

    vkEndCommandBuffer(ctx->cmd);

    vkResetFences(vkd, 1, &ctx->fence);

    VkSubmitInfo si = {
        .sType              = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers    = &ctx->cmd,
    };
    if (vkQueueSubmit(gfx_queue, 1, &si, ctx->fence) != VK_SUCCESS)
        return false;

    vkWaitForFences(vkd, 1, &ctx->fence, VK_TRUE, UINT64_MAX);
    return true;
}

static void glyph_atlas_free(void)
{
    if (g_atlas.image) {
        /* The atlas may still be referenced by an in-flight panel frame. */
        flux_device *dev = typio_render_device_get();
        if (dev) flux_device_wait_idle(dev);
        flux_image_release(g_atlas.image);
    }
    glyph_upload_ctx_destroy();
    free(g_atlas.slots);
    g_atlas = (GlyphAtlas){0};
}

static bool glyph_atlas_ensure(void)
{
    if (g_atlas.image) return true;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    /* Clear so the gutters between glyphs sample as zero coverage. */
    uint8_t *zero = (uint8_t *)calloc((size_t)GLYPH_ATLAS_DIM * GLYPH_ATLAS_DIM, 1);
    if (!zero) return false;

    flux_image_desc id = {};
    id.type         = FLUX_TYPE_IMAGE_DESC;
    id.width        = GLYPH_ATLAS_DIM;
    id.height       = GLYPH_ATLAS_DIM;
    id.format       = FLUX_FORMAT_R8_UNORM;
    id.initial_data = zero;

    flux_image *img = NULL;
    flux_result r = flux_image_create(dev, &id, &img);
    free(zero);
    if (r != FLUX_OK || !img) return false;

    g_atlas.slots = (GlyphSlot *)calloc(GLYPH_SLOT_CAP, sizeof(GlyphSlot));
    if (!g_atlas.slots) { flux_image_release(img); return false; }

    g_atlas.image  = img;
    g_atlas.packer = (GlyphPacker){0};
    return true;
}

/* Look up a glyph's atlas slot, rasterising and uploading it on first sight.
 * After warm-up every panel glyph is a hash hit with zero GPU work.
 *
 * Overflow is handled NON-destructively: once the atlas is physically full a
 * new glyph is recorded as a non-drawable slot (so it is not retried every
 * frame) and the existing atlas is left untouched. We never zero/repack a live
 * texture mid-frame — doing so would blank glyphs already recorded into the
 * current command buffer and force a GPU drain on the IME loop. The atlas is
 * sized for thousands of glyphs and is rebuilt fresh at engine teardown; a
 * config reload that changes the font also rebuilds it via the STYLE path. */
static const GlyphSlot *glyph_atlas_get(uint32_t font_id, FT_Face face, uint32_t glyph_id)
{
    if (!glyph_atlas_ensure()) return NULL;

    uint64_t key  = ((uint64_t)font_id << 32) | glyph_id;
    uint32_t mask = GLYPH_SLOT_CAP - 1u;
    uint32_t i    = (uint32_t)(key * 1099511628211ULL) & mask;

    /* Bounded linear probe: stop at the first empty slot (insertion point) or a
     * key match (hit). The bound makes a (pathologically) full table return
     * "skip this glyph" instead of spinning forever. */
    uint32_t probes = 0;
    while (g_atlas.slots[i].occupied) {
        if (g_atlas.slots[i].key == key) return &g_atlas.slots[i];
        if (++probes >= GLYPH_SLOT_CAP) return NULL;   /* table full, key absent */
        i = (i + 1u) & mask;
    }

    /* Miss — rasterise, pack, upload the glyph's sub-rect. The probe stopped at
     * the empty insertion slot `i`; nothing mutates the table before we write
     * it (overflow inserts a non-drawable slot, but never resets the table). */
    GlyphSlot slot = { .key = key, .occupied = true, .drawable = false };

    if (FT_Load_Glyph(face, glyph_id, FT_LOAD_RENDER | FT_LOAD_TARGET_NORMAL) == 0) {
        FT_GlyphSlot s = face->glyph;
        FT_Bitmap   *b = &s->bitmap;
        slot.left = (int16_t)s->bitmap_left;
        slot.top  = (int16_t)s->bitmap_top;

        uint32_t u, v;
        if (b->width > 0 && b->rows > 0 &&
            glyph_packer_place(&g_atlas.packer, GLYPH_ATLAS_DIM, GLYPH_ATLAS_PAD,
                               b->width, b->rows, &u, &v)) {
            /* Tightly repack the (pitch-padded) FreeType bitmap. */
            uint8_t *tight = (uint8_t *)malloc((size_t)b->width * b->rows);
            if (tight) {
                for (uint32_t row = 0; row < b->rows; ++row)
                    memcpy(tight + (size_t)row * b->width,
                           b->buffer + (size_t)row * (size_t)b->pitch, b->width);
                if (glyph_upload_region(g_atlas.image, u, v, b->width, b->rows,
                                         tight, (size_t)b->width * b->rows)) {
                    slot.u = (uint16_t)u;        slot.v = (uint16_t)v;
                    slot.w = (uint16_t)b->width; slot.h = (uint16_t)b->rows;
                    slot.drawable = true;
                }
                free(tight);
            }
        }
    }

    g_atlas.slots[i] = slot;
    return &g_atlas.slots[i];
}

bool typio_text_shape_fill(flux_canvas *canvas, flux_arena *arena,
                            TypioTextShape *layout, float x, float y,
                            TypioColor tint)
{
    (void)arena;
    if (!canvas || !layout || !layout->face || layout->count == 0) return true;
    if (!glyph_atlas_ensure()) return true;  /* no device yet */

    /* Premultiplied tint; the atlas .r channel is the glyph alpha so the tint
     * is fully opaque (a = 255) and premultiplication is a no-op on colour. */
    flux_color col = flux_color_rgba_premul(to_u8(tint.r), to_u8(tint.g),
                                            to_u8(tint.b), 255);

    const float inv_dim = 1.0f / (float)GLYPH_ATLAS_DIM;

    for (size_t i = 0; i < layout->count; ++i) {
        const GlyphSlot *g = glyph_atlas_get(layout->font_id, layout->face,
                                             layout->glyphs[i].glyph_id);
        if (!g || !g->drawable) continue;

        /* Integer glyph placement relative to the (fractional) draw origin,
         * reproducing the previous whole-run rasteriser's pixel grid. */
        float dx = x + floorf(layout->glyphs[i].x) + g->left;
        float dy = y + floorf(layout->baseline + layout->glyphs[i].y_offset) - g->top;

        flux_rect dst = { dx, dy, (float)g->w, (float)g->h };
        flux_rect src = { (float)g->u * inv_dim, (float)g->v * inv_dim,
                          (float)g->w * inv_dim, (float)g->h * inv_dim };
        flux_canvas_draw_image_coverage_sub(canvas, g_atlas.image, dst, src, col);
    }
    return true;
}

static TypioTextShaperVTable flux_engine_vtable = {
    .create_layout = flux_create_layout,
    .get_metrics   = flux_get_metrics,
    .get_baseline  = flux_get_baseline,
    .free_layout   = typio_text_shape_free,
};

TypioTextShaper *typio_text_shaper_create(void)
{
    TypioTextShaper *engine = (TypioTextShaper *)calloc(1, sizeof(*engine));
    if (!engine) return NULL;

    /* The text engine only needs FreeType (shaping + outlines). The Vulkan
     * device is created lazily by the panel when it first presents. */
    if (!ensure_ft_library()) {
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
    glyph_atlas_free();
    font_obj_cache_clear();
    font_file_cache_clear();
    fallback_font_cache_clear();
    free(engine);
}

void typio_text_shaper_purge_font_caches(void)
{
    /* The glyph atlas is intentionally NOT freed here. Cached panel layouts
     * borrow FT_Face pointers and carry a font_id; freeing the atlas would
     * force the next draw to re-rasterise via those faces, but this purge has
     * just freed them — a use-after-free on any reload that does not also
     * invalidate the layout cache (e.g. a reload that does not change the
     * font). The atlas is a fixed 4 MiB and keyed on the monotonic font_id, so
     * stale slots are bounded dead weight, not a growing leak; it is released
     * only at engine teardown (device fully idle). */
    font_obj_cache_clear();
    font_file_cache_clear();
    fallback_font_cache_clear();
    /* Drain Fontconfig's internal caches.  In a long-running process these
     * grow monotonically because we never call FcFini() on the hot path.
     * This is safe to call from an idle or reload callback; any subsequent
     * font lookup will re-initialise Fontconfig lazily via FcInit(). */
    FcFini();
}
