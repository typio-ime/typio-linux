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
 *
 * The FontObj table is a bounded open-addressing hash with LRU eviction so
 * the per-keystroke lookup cost stays O(1) regardless of how many fractional
 * scales, weights, and fallback fonts accumulate over a long session. Eviction
 * is safe: TypioTextShape borrows only the FT_Face (kept alive in the face
 * table) and carries a font_id that the atlas treats as opaque — a retired
 * id becomes bounded dead weight until the next atlas rebuild, never a crash.
 */
#include "font_cache.h"

#include <typio/abi/log.h>

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

/* ── FontObj table: open-addressing hash + LRU eviction ───────────────────
 *
 * Layout: a fixed-size array (FONT_OBJ_CACHE_CAP slots, must be a power of
 * two). Lookup hashes (path, size, weight) and linear-probes for a matching
 * occupied slot or the first empty slot. Each slot carries an lru_tick that is
 * bumped on every hit; insertions on a full table evict the slot with the
 * smallest lru_tick.
 *
 * Eviction frees hb_font and the path string but deliberately does NOT touch
 * the FT_Face (it lives in the face table above and is borrowed by TypioTextShape).
 */
typedef struct {
    FontObj  obj;        /* zero-initialised → unoccupied (path == NULL)       */
    uint32_t lru_tick;   /* 0 for unoccupied slots; bumped on every hit        */
    bool     occupied;
} FontObjSlot;

static FontObjSlot g_obj_table[FONT_OBJ_CACHE_CAP];
static uint32_t    g_obj_count;
static uint32_t    g_obj_tick;
static uint32_t    g_obj_evictions;   /* cumulative LRU evictions (diagnostics) */
static uint32_t    next_font_id = 1;   /* monotonic; never reset (see header) */

/* FNV-1a over @path, then mix in @size and @weight so the table spreads
 * different (size, weight) variants of the same file across slots instead of
 * clustering them on the same home index. */
static uint32_t obj_hash(const char *path, float size, int32_t weight)
{
    uint32_t h = 2166136261u;
    for (const unsigned char *p = (const unsigned char *)path; *p; ++p) {
        h ^= *p;
        h *= 16777619u;
    }
    /* Bit-mix float + int32 into the hash without UB on float reinterpretation. */
    uint32_t size_bits;
    memcpy(&size_bits, &size, sizeof(size_bits));
    h ^= size_bits;
    h *= 16777619u;
    h ^= (uint32_t)weight;
    h *= 16777619u;
    return h;
}

static FontObjSlot *obj_find(const char *path, float size, int32_t weight)
{
    const uint32_t mask = FONT_OBJ_CACHE_CAP - 1u;
    uint32_t i = obj_hash(path, size, weight) & mask;
    for (uint32_t probe = 0; probe < FONT_OBJ_CACHE_CAP; ++probe) {
        FontObjSlot *s = &g_obj_table[i];
        if (!s->occupied) return s;                          /* insertion point */
        if (s->obj.size == size && s->obj.weight == weight &&
            strcmp(s->obj.path, path) == 0) {
            return s;                                        /* hit */
        }
        i = (i + 1u) & mask;
    }
    return NULL;   /* table pathologically full (cannot happen at <75% load) */
}

/* Release the wrapper's owned state. The FT_Face is NOT freed here — it lives
 * in the face table and may be borrowed by live TypioTextShapes. */
static void obj_slot_release(FontObjSlot *s)
{
    if (!s->occupied) return;
    if (s->obj.hb_font) hb_font_destroy(s->obj.hb_font);
    free(s->obj.path);
    s->obj = (FontObj){0};
    s->lru_tick = 0;
    s->occupied = false;
    g_obj_count--;
}

/* Insert into the table, evicting the LRU victim if full. Steals ownership of
 * @path_dup and @hb_font. Returns a pointer to the populated slot or NULL on
 * allocation failure (in which case @path_dup and @hb_font are consumed). */
static FontObjSlot *obj_insert(char *path_dup, float size, int32_t weight,
                                FT_Face face, hb_font_t *hb_font, uint32_t font_id)
{
    FontObjSlot *victim = NULL;
    if (g_obj_count >= FONT_OBJ_CACHE_CAP) {
        /* Find LRU victim among occupied slots. Linear scan over a fixed
         * power-of-two table is bounded (FONT_OBJ_CACHE_CAP iterations) and
         * amortised across many cache hits, matching the pattern already used
         * by the codepoint-fallback memo (font_resolve.c:fb_cp_cache_insert). */
        uint32_t oldest = UINT32_MAX;
        for (uint32_t i = 0; i < FONT_OBJ_CACHE_CAP; ++i) {
            FontObjSlot *s = &g_obj_table[i];
            if (!s->occupied) { victim = s; break; }
            if (s->lru_tick < oldest) {
                oldest = s->lru_tick;
                victim = s;
            }
        }
        if (victim) {
            obj_slot_release(victim);
            g_obj_evictions++;
            typio_log_debug("font_cache: LRU evict (font_obj_count=%u evictions=%u)",
                            (unsigned)g_obj_count, (unsigned)g_obj_evictions);
        }
    }

    /* Re-walk from the hash home to find an empty slot (either the victim's
     * slot, naturally near the home index, or another empty slot encountered
     * during probing). obj_find returns the first empty slot when no key match
     * exists, which is exactly the insertion site. */
    FontObjSlot *slot = obj_find(path_dup, size, weight);
    if (!slot) {
        /* Pathological: every probe collided. Should be unreachable at the
         * 75% load ceiling; degrade to a synchronous free of the input. */
        free(path_dup);
        if (hb_font) hb_font_destroy(hb_font);
        return NULL;
    }
    if (slot->occupied) {
        /* Key collision with an existing entry (e.g. concurrent insert of the
         * same key from another code path). Free the old wrapper first. */
        obj_slot_release(slot);
    }
    slot->obj.path    = path_dup;
    slot->obj.size    = size;
    slot->obj.weight  = weight;
    slot->obj.face    = face;
    slot->obj.hb_font = hb_font;
    slot->obj.font_id = font_id;
    slot->lru_tick    = ++g_obj_tick;
    slot->occupied    = true;
    g_obj_count++;
    return slot;
}

void font_cache_clear(void)
{
    for (uint32_t i = 0; i < FONT_OBJ_CACHE_CAP; ++i) {
        obj_slot_release(&g_obj_table[i]);
    }
    g_obj_count = 0;
    g_obj_tick  = 0;
    /* g_obj_evictions is cumulative across the process lifetime (matching the
     * atlas rebuild / glyph rasterised counters) so a slow-render log can
     * correlate churn across config reloads. */

    for (size_t i = 0; i < face_count; ++i) {
        if (faces[i].face) FT_Done_Face(faces[i].face);
        free(faces[i].path);
    }
    free(faces);
    faces = NULL;
    face_count = 0;
    face_cap = 0;
}

uint32_t font_cache_obj_count(void)    { return g_obj_count; }
uint32_t font_cache_face_count(void)   { return (uint32_t)face_count; }
uint32_t font_cache_eviction_count(void) { return g_obj_evictions; }

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
    FontObjSlot *hit = obj_find(path, size, weight);
    if (hit && hit->occupied) {
        hit->lru_tick = ++g_obj_tick;
        font_cache_apply(hit->obj.face, size, weight);
        return &hit->obj;
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

    char *path_dup = strdup(path);
    if (!path_dup) {
        hb_font_destroy(hb_font);
        return NULL;
    }

    uint32_t font_id = next_font_id++;
    FontObjSlot *slot = obj_insert(path_dup, size, weight, face, hb_font, font_id);
    if (!slot) return NULL;   /* obj_insert already freed path_dup + hb_font */
    return &slot->obj;
}
