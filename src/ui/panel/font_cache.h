/**
 * @file font_cache.h
 * @brief Cache of opened font objects (FT_Face + hb_font_t), keyed by
 *        (path, size, weight).
 *
 * A font object must outlive every TypioTextShape and every glyph-atlas entry
 * that references it: shapes hold a borrowed FT_Face* and the atlas is keyed by
 * font_id and re-rasterises on miss. Distinct (path, size, weight) tuples are a
 * bounded resource in practice (a handful of system fonts × a few sizes × a few
 * weights), so the cache GROWS on demand and never frees a face at runtime —
 * entries are released only by font_cache_clear() at teardown / reload.
 *
 * FT_Face objects are shared by file path: different (size, weight) tuples for
 * the same font file reuse a single FT_Face (one 17 MB mmap instead of N).
 * The face's pixel size and variable-font weight are applied before each use
 * by font_cache_apply(), which the glyph atlas calls prior to FT_Load_Glyph.
 *
 *   Bound:   unbounded but naturally small (system fonts × sizes × weights).
 *   Evict:   none — faces are borrowed by live shapes; freeing one mid-session
 *            is a use-after-free.
 *   Reclaim: font_cache_clear() at teardown / config reload only.
 *   Observe: font_id values are monotonic; the atlas keys on them.
 */
#ifndef TYPIO_WL_FONT_CACHE_H
#define TYPIO_WL_FONT_CACHE_H

#include <harfbuzz/hb.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include <stdbool.h>
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

/* Initialise the shared FreeType library. Idempotent; returns false only if
 * FreeType fails to initialise. */
bool font_cache_init(void);

/* Look up — or open and cache — the font object for (@path, @size, @weight).
 * The returned pointer is stable for the cache's lifetime (entries are never
 * freed at runtime). Returns NULL on open / allocation failure.
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

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FONT_CACHE_H */
