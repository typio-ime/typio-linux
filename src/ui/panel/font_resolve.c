/**
 * @file font_resolve.c
 * @brief Fontconfig font resolution + memos (see header).
 */
#include "font_resolve.h"

#include <typio/abi/log.h>

#include <stdlib.h>
#include <string.h>
#include <strings.h>

/* Cumulative per-codepoint memo diagnostics. */
static uint64_t g_fb_resolve_hits;
static uint64_t g_fb_resolve_misses;

/* ── Periodic Fontconfig cache drain ────────────────────────────────────────
 *
 * Fontconfig's internal caches (object pools, pattern caches, FcFontSet
 * indices) grow monotonically across the process lifetime: every
 * FcFontMatch / FcFontSort call inflates them, and we never call FcFini() on
 * the hot path because it tears down every FcPattern we still borrow. Long
 * CJK-heavy sessions accumulate noticeable per-query cost this way.
 *
 * Strategy: count cache misses (each miss is exactly one FcFontSort run that
 * inflated Fontconfig's state) and call FcFini() once the count crosses
 * FONTCONFIG_PURGE_PERIOD. The function is safe to call from any
 * single-threaded caller — it re-initialises lazily via FcInit() on the next
 * lookup, and our own fb_cp_cache (which holds the *resolved* paths as our
 * own copies) stays hot across the drain, so only genuinely new codepoints
 * re-query Fontconfig.
 *
 * The period matches FB_CP_CACHE_CAP so the drain fires roughly once per
 * "the cache has fully churned" — bounded, self-throttling, and aligned with
 * the working-set reality of CJK fallback expansion.
 */
#define FONTCONFIG_PURGE_PERIOD 256u
static uint32_t g_misses_since_fc_purge;
static uint64_t g_fc_purge_count;   /* cumulative FcFini() invocations (diagnostics) */

static void maybe_drain_fontconfig(void)
{
    if (g_misses_since_fc_purge < FONTCONFIG_PURGE_PERIOD) return;
    typio_log_debug("font_resolve: draining Fontconfig internal caches "
                    "(misses=%u purges=%llu)",
                    g_misses_since_fc_purge,
                    (unsigned long long)(g_fc_purge_count + 1));
    FcFini();
    g_misses_since_fc_purge = 0;
    g_fc_purge_count++;
}

/* ── Weight helpers ─────────────────────────────────────────────────────── */

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

static int weight_to_fc(int32_t weight)
{
    if (weight >= 900) return FC_WEIGHT_BLACK;
    if (weight >= 800) return FC_WEIGHT_EXTRABOLD;
    if (weight >= 700) return FC_WEIGHT_BOLD;
    if (weight >= 600) return FC_WEIGHT_DEMIBOLD;
    if (weight >= 500) return FC_WEIGHT_MEDIUM;
    if (weight >= 400) return FC_WEIGHT_REGULAR;
    if (weight >= 300) return FC_WEIGHT_LIGHT;
    if (weight >= 200) return FC_WEIGHT_EXTRALIGHT;
    return FC_WEIGHT_THIN;
}

bool font_resolve_parse_desc(const char *font_desc, char *family,
                             size_t family_size, float *size, int32_t *weight)
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

/* ── Family → file cache ────────────────────────────────────────────────── */
#define FONT_FILE_CACHE_CAP 32

typedef struct {
    char    family[128];
    int32_t weight;
    char   *path;
} FontFileEntry;

static FontFileEntry font_file_cache[FONT_FILE_CACHE_CAP];
static size_t        font_file_cache_count;

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
    FontFileEntry *e;
    if (font_file_cache_count < FONT_FILE_CACHE_CAP) {
        e = &font_file_cache[font_file_cache_count++];
    } else {
        free(font_file_cache[0].path);
        for (size_t i = 1; i < FONT_FILE_CACHE_CAP; ++i)
            font_file_cache[i - 1] = font_file_cache[i];
        e = &font_file_cache[FONT_FILE_CACHE_CAP - 1];
    }
    snprintf(e->family, sizeof(e->family), "%s", family);
    e->weight = weight;
    e->path = strdup(path);
}

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

static bool fc_font_covers_cjk(FcPattern *font)
{
    FcCharSet *cs = NULL;
    if (FcPatternGetCharSet(font, FC_CHARSET, 0, &cs) != FcResultMatch || !cs)
        return false;
    return FcCharSetHasChar(cs, 0x4E2D) == FcTrue;
}

static char *fc_match_extract(FcPattern *pat)
{
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
    return result;
}

char *font_resolve_file(const char *family, int32_t weight)
{
    char *cached = font_file_cache_lookup(family, weight);
    if (cached) return cached;

    if (!FcInit()) return NULL;

    const char *fam = (family && family[0]) ? family : "Sans";
    int fc_weight = weight_to_fc(weight);

    FcPattern *pat = FcPatternCreate();
    if (!pat) return NULL;

    FcPatternAddString(pat, FC_FAMILY, (const FcChar8 *)fam);
    FcPatternAddInteger(pat, FC_WEIGHT, fc_weight);
    FcConfigSubstitute(NULL, pat, FcMatchPattern);
    FcDefaultSubstitute(pat);

    char *result = fc_match_extract(pat);
    FcPatternDestroy(pat);

    if (result) {
        FcPattern *check = FcPatternCreate();
        if (check) {
            FcPatternAddString(check, FC_FILE, (const FcChar8 *)result);
            FcConfigSubstitute(NULL, check, FcMatchPattern);
            FcDefaultSubstitute(check);
            FcResult cr;
            FcPattern *cm = FcFontMatch(NULL, check, &cr);
            bool has_cjk = cm && fc_font_covers_cjk(cm);
            if (cm) FcPatternDestroy(cm);
            FcPatternDestroy(check);

            if (!has_cjk) {
                FcPattern *cjk_pat = FcPatternCreate();
                if (cjk_pat) {
                    FcPatternAddString(cjk_pat, FC_FAMILY, (const FcChar8 *)fam);
                    FcPatternAddInteger(cjk_pat, FC_WEIGHT, fc_weight);
                    FcCharSet *cs = FcCharSetCreate();
                    if (cs) {
                        FcCharSetAddChar(cs, 0x4E2D);
                        FcPatternAddCharSet(cjk_pat, FC_CHARSET, cs);
                        FcCharSetDestroy(cs);
                    }
                    FcConfigSubstitute(NULL, cjk_pat, FcMatchPattern);
                    FcDefaultSubstitute(cjk_pat);

                    char *cjk_result = fc_match_extract(cjk_pat);
                    FcPatternDestroy(cjk_pat);

                    if (cjk_result) {
                        free(result);
                        result = cjk_result;
                    }
                }
            }
        }
    }

    if (result) {
        font_file_cache_insert(family, weight, result);
    }
    return result;
}

/* ── Per-codepoint fallback resolution memo ─────────────────────────────────
 *
 * FcFontSort over every installed font is the single most expensive step on the
 * layout path. The resolved covering-font path list is independent of pixel
 * size, so it is memoised by (codepoint, weight); a hit skips the sort and only
 * re-derives the per-size FontObj (itself a font_cache hit). */
#define FB_CP_CACHE_CAP    256

typedef struct {
    FcChar32 cp;
    int32_t  weight;
    char    *paths[FONT_CANDIDATES_MAX];   /* Fc-sorted covering files (owned) */
    uint8_t  count;
    uint32_t lru_tick;
    bool     occupied;
} FbCpEntry;

static FbCpEntry fb_cp_cache[FB_CP_CACHE_CAP];
static uint32_t  fb_cp_tick;

static void fb_cp_cache_clear(void)
{
    for (size_t i = 0; i < FB_CP_CACHE_CAP; ++i) {
        for (uint8_t k = 0; k < fb_cp_cache[i].count; ++k)
            free(fb_cp_cache[i].paths[k]);
        fb_cp_cache[i] = (FbCpEntry){0};
    }
    fb_cp_tick = 0;
}

static FbCpEntry *fb_cp_cache_lookup(FcChar32 cp, int32_t weight)
{
    for (size_t i = 0; i < FB_CP_CACHE_CAP; ++i) {
        if (fb_cp_cache[i].occupied &&
            fb_cp_cache[i].cp == cp && fb_cp_cache[i].weight == weight) {
            fb_cp_cache[i].lru_tick = ++fb_cp_tick;
            g_fb_resolve_hits++;
            return &fb_cp_cache[i];
        }
    }
    return NULL;
}

/* Take ownership of @paths[0..count) into a fresh (LRU-evicted) slot and return
 * it (so the caller need not re-look-up, which would skew the hit counter). */
static FbCpEntry *fb_cp_cache_insert(FcChar32 cp, int32_t weight,
                                     char **paths, uint8_t count)
{
    size_t   victim = 0;
    uint32_t oldest = UINT32_MAX;
    for (size_t i = 0; i < FB_CP_CACHE_CAP; ++i) {
        if (!fb_cp_cache[i].occupied) { victim = i; break; }
        if (fb_cp_cache[i].lru_tick < oldest) {
            oldest = fb_cp_cache[i].lru_tick;
            victim = i;
        }
    }
    FbCpEntry *e = &fb_cp_cache[victim];
    for (uint8_t k = 0; k < e->count; ++k) free(e->paths[k]);

    e->cp       = cp;
    e->weight   = weight;
    e->count    = count;
    for (uint8_t k = 0; k < count; ++k) e->paths[k] = paths[k];
    e->lru_tick = ++fb_cp_tick;
    e->occupied = true;
    return e;
}

/* Resolve the Fc-sorted list of font files that cover @ch (memoised). */
static FbCpEntry *resolve_codepoint_fonts(FcChar32 ch, int32_t weight)
{
    FbCpEntry *hit = fb_cp_cache_lookup(ch, weight);
    if (hit) return hit;

    if (!FcInit()) return NULL;

    g_fb_resolve_misses++;
    g_misses_since_fc_purge++;
    maybe_drain_fontconfig();
    typio_log_debug("font_resolve: fallback resolve U+%04X weight=%d (FcFontSort)",
                    ch, weight);

    FcPattern *pat = FcPatternCreate();
    if (!pat) return NULL;

    FcCharSet *cs = FcCharSetCreate();
    if (!cs) { FcPatternDestroy(pat); return NULL; }
    FcCharSetAddChar(cs, ch);

    FcPatternAddCharSet(pat, FC_CHARSET, cs);
    FcPatternAddInteger(pat, FC_WEIGHT, weight_to_fc(weight));
    FcConfigSubstitute(NULL, pat, FcMatchPattern);
    FcDefaultSubstitute(pat);

    FcResult fc_result;
    FcFontSet *set = FcFontSort(NULL, pat, FcTrue, NULL, &fc_result);

    char   *paths[FONT_CANDIDATES_MAX];
    uint8_t count = 0;
    if (set) {
        for (int i = 0; i < set->nfont && count < FONT_CANDIDATES_MAX; i++) {
            FcCharSet *font_cs = NULL;
            if (FcPatternGetCharSet(set->fonts[i], FC_CHARSET, 0, &font_cs) == FcResultMatch && font_cs) {
                if (!FcCharSetHasChar(font_cs, ch)) continue;
            }
            FcChar8 *file = NULL;
            if (FcPatternGetString(set->fonts[i], FC_FILE, 0, &file) != FcResultMatch || !file)
                continue;
            paths[count++] = strdup((const char *)file);
        }
        FcFontSetDestroy(set);
    }

    FcCharSetDestroy(cs);
    FcPatternDestroy(pat);

    return fb_cp_cache_insert(ch, weight, paths, count);
}

/* ── Candidate list ─────────────────────────────────────────────────────── */

void font_candidate_list_clear(FontCandidateList *list)
{
    for (size_t i = 0; i < list->count; i++) {
        free(list->paths[i]);
    }
    list->count = 0;
}

static bool font_candidate_list_add(FontCandidateList *list, const char *path,
                                    FontObj *obj)
{
    if (list->count >= FONT_CANDIDATES_MAX) return false;
    for (size_t i = 0; i < list->count; i++) {
        if (strcmp(list->paths[i], path) == 0) return true;
    }
    list->paths[list->count] = strdup(path);
    list->objs[list->count] = obj;
    list->count++;
    return true;
}

void font_resolve_codepoint_fonts(FcChar32 ch, int32_t weight, float size_px,
                                  FontCandidateList *out, const char *primary_path)
{
    memset(out, 0, sizeof(*out));

    FbCpEntry *resolved = resolve_codepoint_fonts(ch, weight);
    if (!resolved) return;

    for (uint8_t i = 0; i < resolved->count && out->count < FONT_CANDIDATES_MAX; i++) {
        const char *path = resolved->paths[i];
        if (primary_path && strcmp(path, primary_path) == 0) continue;

        FontObj *obj = font_cache_get_or_create(path, size_px, weight);
        if (obj) {
            font_candidate_list_add(out, path, obj);
        }
    }
}

void font_resolve_clear(void)
{
    font_file_cache_clear();
    fb_cp_cache_clear();
    /* typio_text_shaper_purge_font_caches() also calls FcFini(), so reset our
     * miss counter to keep the periodic-drain cadence aligned with the manual
     * purge rather than firing again on the very next miss. */
    g_misses_since_fc_purge = 0;
}

void font_resolve_get_diag(uint64_t *out_hits, uint64_t *out_misses)
{
    if (out_hits)   *out_hits   = g_fb_resolve_hits;
    if (out_misses) *out_misses = g_fb_resolve_misses;
}

uint64_t font_resolve_purge_count(void)
{
    return g_fc_purge_count;
}
