/**
 * @file font_resolve.h
 * @brief Fontconfig-backed font resolution: descriptor parsing, family→file
 *        matching, and per-codepoint fallback resolution.
 *
 * All the expensive Fontconfig work lives here behind two caches so the layout
 * hot path pays for a resolution at most once:
 *
 *   - family→file matching is memoised by (family, weight);
 *   - per-codepoint fallback (an FcFontSort over every installed font) is
 *     memoised by (codepoint, weight). CJK text recreates the same handful of
 *     Han codepoints constantly under the layout LRU's eviction churn, so this
 *     collapses the dominant cost to once per script.
 *
 *   Bound:   fixed-cap LRU caches (file: 32, codepoint: 256 entries).
 *   Evict:   LRU.
 *   Reclaim: font_resolve_clear() at teardown / config reload.
 *   Observe: font_resolve_get_diag() (memo hit / miss counts).
 */
#ifndef TYPIO_WL_FONT_RESOLVE_H
#define TYPIO_WL_FONT_RESOLVE_H

#include "font_cache.h"

#include <fontconfig/fontconfig.h>

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FONT_CANDIDATES_MAX 8

/* An ordered set of fallback font candidates for one codepoint, each already
 * opened at the requested pixel size. Paths are owned; clear with
 * font_candidate_list_clear. */
typedef struct FontCandidateList {
    char    *paths[FONT_CANDIDATES_MAX];
    FontObj *objs[FONT_CANDIDATES_MAX];
    size_t   count;
} FontCandidateList;

void font_candidate_list_clear(FontCandidateList *list);

/* Parse a "Family [Weight] Size" descriptor into its parts. Defaults to
 * Sans/16px/400 for missing fields. Returns false only on a bad @family
 * buffer. */
bool font_resolve_parse_desc(const char *font_desc, char *family,
                             size_t family_size, float *size, int32_t *weight);

/* Resolve @family + @weight to a font file path (owned by the caller).
 * Prefers a CJK-covering face when the primary match lacks 中. Memoised. */
char *font_resolve_file(const char *family, int32_t weight);

/* Fill @out with the fonts (excluding @primary_path) that cover @ch, opened at
 * @size_px, best match first. Memoised by (codepoint, weight). */
void font_resolve_codepoint_fonts(FcChar32 ch, int32_t weight, float size_px,
                                  FontCandidateList *out, const char *primary_path);

/* Drop the file + codepoint memos (teardown / config reload). */
void font_resolve_clear(void);

/* Report cumulative per-codepoint memo hit / miss counts. */
void font_resolve_get_diag(uint64_t *out_hits, uint64_t *out_misses);

/* Cumulative count of FcFini() drain invocations since process start. For
 * correlating candidate lag with Fontconfig cache churn. */
uint64_t font_resolve_purge_count(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FONT_RESOLVE_H */
