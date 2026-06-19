/**
 * @file font_cache.h
 * @brief Cache of opened font objects (FT_Face + hb_font_t), keyed by
 *        (path, size, weight).
 *
 * A font object must outlive every TypioTextShape and every glyph-atlas entry
 * that references it: shapes hold a borrowed FT_Face* and the atlas is keyed by
 * font_id and re-rasterises on miss.
 *
 * Two-tier cache:
 *
 *   - **Face table** (one FT_Face per font file path). Unbounded but tiny in
 *     practice (a handful of system fonts plus per-codepoint CJK fallbacks).
 *     Each entry mmaps its font file once (~5–17 MB) and is shared by every
 *     (size, weight) variant of that file. Never freed at runtime — TypioTextShape
 *     borrows FT_Face pointers, and freeing one mid-session is a use-after-free.
 *
 *   - **Font object table** (one FontObj per (path, size, weight) tuple).
 *     Bounded open-addressing hash table with LRU eviction, capped at
 *     @ref FONT_OBJ_CACHE_CAP entries. Eviction frees only the per-tuple
 *     wrapper (hb_font_t + path string); the underlying FT_Face stays alive in
 *     the face table. The retired font_id cannot alias a newly opened face
 *     (font_id values are monotonic across the process lifetime), so a stale
 *     atlas slot keyed on a freed font_id is bounded dead weight reclaimed by
 *     the normal atlas rebuild path.
 *
 *   Bound:   face table unbounded (naturally small); FontObj table LRU-capped.
 *   Evict:   FontObj only (LRU); never the FT_Face.
 *   Reclaim: font_cache_clear() at teardown / config reload only.
 *   Observe: font_id values are monotonic; the atlas keys on them.
 */
#ifndef TYPIO_WL_FONT_CACHE_H
#define TYPIO_WL_FONT_CACHE_H

#include <harfbuzz/hb.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FontObj {
    char      *path;
    float      size;
    int32_t    weight;
    FT_Face    face;       /* shared by file path; owned by face cache */
    hb_font_t *hb_font;    /* owned by the cache */
    uint32_t   font_id;    /* monotonic identity of (face, size, weight) */
} FontObj;

/* Hard cap on the FontObj table. With a handful of system fonts × a handful of
 * fractional scales × a couple of weights × CJK fallback expansion, the working
 * set stays well under this in practice. When exceeded, the LRU victim's
 * hb_font and path string are freed; the FT_Face stays alive in the face
 * table. Power of two so the hash table mask is a single subtract. */
#define FONT_OBJ_CACHE_CAP 256u

/* Initialise the shared FreeType library. Idempotent; returns false only if
 * FreeType fails to initialise. */
bool font_cache_init(void);

/* Look up — or open and cache — the font object for (@path, @size, @weight).
 * The returned pointer is borrowed and remains valid until the next call to
 * any font_cache_* function (LRU eviction may free the wrapper on insert; the
 * underlying FT_Face stays alive for the process lifetime). Returns NULL on
 * open / allocation failure.
 * Sets the face's pixel size and variable-font weight before returning so the
 * caller can immediately shape or rasterise. */
FontObj *font_cache_get_or_create(const char *path, float size, int32_t weight);

/* Set @face's pixel size and variable-font weight for (@size, @weight).
 * Call before every FT_Load_Glyph when the face is shared across FontObjs. */
void font_cache_apply(FT_Face face, float size, int32_t weight);

/* Release every cached face + hb_font. Caller guarantees no live shape still
 * borrows them (teardown, or a config reload that also drops the layout LRU).
 * font_id allocation stays monotonic across clears so stale atlas slots keyed
 * on a freed font_id can never alias a newly opened face. */
void font_cache_clear(void);

/* Diagnostics: current occupancy of the FontObj table, total face count, and
 * cumulative LRU evictions across the process lifetime. For correlating
 * candidate lag with cache churn — a climbing eviction count alongside a
 * climbing obj_count means the working set exceeds the cap and hot entries
 * are being recycled; consider raising FONT_OBJ_CACHE_CAP. */
uint32_t font_cache_obj_count(void);
uint32_t font_cache_face_count(void);
uint32_t font_cache_eviction_count(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FONT_CACHE_H */
