#ifndef TYPIO_WL_FALLBACK_CACHE_H
#define TYPIO_WL_FALLBACK_CACHE_H

/*
 * Coverage-keyed LRU cache for fallback-font resolution.
 *
 * Resolving a fallback font (FcFontSort over the whole system font set plus a
 * per-codepoint coverage scan) is expensive and runs synchronously on the IME
 * event loop, ahead of the popup present. CJK input produces an unbounded
 * stream of distinct candidate strings, so a cache keyed on the text itself
 * collapses to a ~0% hit rate and re-runs the resolve on every keystroke.
 *
 * This cache keys on *coverage* instead: each resolved font remembers the
 * FcCharSet it covers, and a later text reuses it whenever the text's
 * codepoints are a subset of that coverage (FcCharSetIsSubset). One Noto CJK
 * face then satisfies every later phrase, so the resolve runs ~once per script
 * regardless of how many distinct phrases have been typed. Eviction is true
 * LRU, so the hit rate does not degrade as the cache fills.
 */

#include <fontconfig/fontconfig.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FallbackFontCache FallbackFontCache;

/*
 * Resolve the fallback font that covers @text at @weight.
 *
 * On success returns a malloc'd font path (ownership transfers to the caller)
 * and, when @out_coverage is non-NULL, stores a newly-allocated FcCharSet of
 * the resolved font's coverage (ownership transfers to the caller). On failure
 * returns NULL and leaves *@out_coverage NULL.
 */
typedef char *(*FallbackResolveFn)(void *user, const char *text, int32_t weight,
                                   FcCharSet **out_coverage);

FallbackFontCache *fallback_cache_new(size_t cap, FallbackResolveFn resolve,
                                      void *user);
void fallback_cache_free(FallbackFontCache *cache);

/*
 * Return the fallback font path covering @text at @weight, as a malloc'd string
 * the caller frees, or NULL. Reuses a cached entry when @text's codepoints are
 * a subset of its coverage; otherwise calls the resolver once and caches the
 * result (evicting the least-recently-used entry when full).
 */
char *fallback_cache_lookup(FallbackFontCache *cache, const char *text,
                            int32_t weight);

/* Number of populated entries — test/introspection only. */
size_t fallback_cache_entry_count(const FallbackFontCache *cache);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FALLBACK_CACHE_H */
