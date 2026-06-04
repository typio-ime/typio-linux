/**
 * @file font_cache.c
 * @brief FT_Face + hb_font_t object cache (see header).
 *
 * FT_Face objects are shared by file path: each unique font file is mmap'd
 * once (one FT_New_Face per file). Distinct (size, weight) tuples for the
 * same file create separate FontObj entries with their own hb_font and
 * font_id, but reference the shared FT_Face. The face's pixel size and
 * variable-font weight are applied on demand via font_cache_apply() before
 * each shaping or rasterisation call.
 */
#include "font_cache.h"

#include <harfbuzz/hb-ft.h>

#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_MULTIPLE_MASTERS_H
#include FT_TRUETYPE_TABLES_H

#include <stdlib.h>
#include <string.h>

static FT_Library ft_library;

bool font_cache_init(void)
{
    if (ft_library) return true;
    if (FT_Init_FreeType(&ft_library) != 0) {
        ft_library = NULL;
        return false;
    }
    return true;
}

/* ── Shared face table (one FT_Face per file path) ─────────────────────── */

typedef struct {
    char     *path;
    FT_Face   face;
    int32_t   last_weight;
} FaceEntry;

static FaceEntry *faces;
static size_t      face_count;
static size_t      face_cap;

static FT_Face face_lookup(const char *path)
{
    for (size_t i = 0; i < face_count; ++i) {
        if (strcmp(faces[i].path, path) == 0) return faces[i].face;
    }
    return NULL;
}

static FaceEntry *face_insert(const char *path, FT_Face face)
{
    if (face_count == face_cap) {
        size_t newcap = face_cap ? face_cap * 2 : 8;
        FaceEntry *grown = (FaceEntry *)realloc(faces, newcap * sizeof(*grown));
        if (!grown) return NULL;
        faces = grown;
        face_cap = newcap;
    }
    FaceEntry *e = &faces[face_count++];
    e->path        = strdup(path);
    e->face        = face;
    e->last_weight = 0;
    return e;
}

/* ── Object cache (one FontObj per path × size × weight) ───────────────── */
#define FONT_OBJ_CACHE_INIT_CAP 64

static FontObj  *cache;
static size_t    cache_count;
static size_t    cache_cap;
static uint32_t  next_font_id = 1;   /* monotonic; never reset (see header) */

void font_cache_clear(void)
{
    for (size_t i = 0; i < cache_count; ++i) {
        if (cache[i].hb_font) hb_font_destroy(cache[i].hb_font);
        free(cache[i].path);
    }
    free(cache);
    cache = NULL;
    cache_count = 0;
    cache_cap = 0;

    for (size_t i = 0; i < face_count; ++i) {
        if (faces[i].face) FT_Done_Face(faces[i].face);
        free(faces[i].path);
    }
    free(faces);
    faces = NULL;
    face_count = 0;
    face_cap = 0;
}

static FontObj *cache_lookup(const char *path, float size, int32_t weight)
{
    for (size_t i = 0; i < cache_count; ++i) {
        if (cache[i].size == size && cache[i].weight == weight &&
            strcmp(cache[i].path, path) == 0) {
            return &cache[i];
        }
    }
    return NULL;
}

static bool cache_insert(const char *path, float size, int32_t weight,
                         FT_Face face, hb_font_t *hb_font, uint32_t font_id)
{
    if (cache_count == cache_cap) {
        size_t newcap = cache_cap ? cache_cap * 2 : FONT_OBJ_CACHE_INIT_CAP;
        FontObj *grown = (FontObj *)realloc(cache, newcap * sizeof(*grown));
        if (!grown) return false;
        cache = grown;
        cache_cap = newcap;
    }

    FontObj *e = &cache[cache_count++];
    e->path    = strdup(path);
    e->size    = size;
    e->weight  = weight;
    e->face    = face;
    e->hb_font = hb_font;
    e->font_id = font_id;
    return true;
}

/* Drive a variable font's 'wght' axis to @weight, if it has one. */
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

void font_cache_apply(FT_Face face, float size, int32_t weight)
{
    if (!face) return;
    FT_Set_Pixel_Sizes(face, 0, (FT_UInt)(size + 0.5f));

    for (size_t i = 0; i < face_count; ++i) {
        if (faces[i].face == face) {
            if (faces[i].last_weight != weight) {
                set_face_weight(face, weight);
                faces[i].last_weight = weight;
            }
            return;
        }
    }
    set_face_weight(face, weight);
}

FontObj *font_cache_get_or_create(const char *path, float size, int32_t weight)
{
    FontObj *entry = cache_lookup(path, size, weight);
    if (entry) {
        font_cache_apply(entry->face, size, weight);
        return entry;
    }

    FT_Face face = face_lookup(path);
    if (!face) {
        if (!ft_library) return NULL;
        if (FT_New_Face(ft_library, path, 0, &face) != 0) return NULL;

        for (int i = 0; i < face->num_charmaps; i++) {
            if (FT_Get_CMap_Format(face->charmaps[i]) == 12) {
                FT_Set_Charmap(face, face->charmaps[i]);
                break;
            }
        }
        if (!face_insert(path, face)) {
            FT_Done_Face(face);
            return NULL;
        }
    }

    font_cache_apply(face, size, weight);

    hb_font_t *hb_font = hb_ft_font_create_referenced(face);
    if (!hb_font) return NULL;

    uint32_t font_id = next_font_id++;
    if (!cache_insert(path, size, weight, face, hb_font, font_id)) {
        hb_font_destroy(hb_font);
        return NULL;
    }
    return cache_lookup(path, size, weight);
}
