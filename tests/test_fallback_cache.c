/*
 * Regression tests for the coverage-keyed fallback-font cache.
 *
 * The bug these guard against: the old cache keyed on the full candidate
 * string and stopped caching once full, so a long CJK session (an unbounded
 * stream of distinct phrases, all served by the same fallback font) collapsed
 * to a ~0% hit rate and re-ran the expensive resolver on every composition —
 * the source of the "lag after a while, including the panel" report.
 *
 * These tests stand in for "mock longtime / frequent composition": a mock
 * resolver counts how often the expensive path runs while we feed thousands
 * of distinct phrases. They use only FcCharSet data-structure operations, so
 * they need no installed fonts and no GPU.
 */

#include "fallback_cache.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int tests_run = 0;
static int tests_passed = 0;

#define TEST(name) \
    static void test_##name(void); \
    static void run_test_##name(void) { \
        printf("  Running %s... ", #name); \
        tests_run++; \
        test_##name(); \
        tests_passed++; \
        printf("OK\n"); \
    } \
    static void test_##name(void)

#define ASSERT(expr) \
    do { \
        if (!(expr)) { \
            printf("FAILED\n"); \
            printf("    Assertion failed: %s\n", #expr); \
            printf("    At %s:%d\n", __FILE__, __LINE__); \
            exit(1); \
        } \
    } while (0)

#define ASSERT_EQ(a, b) ASSERT((long)(a) == (long)(b))

/* ── Mock resolver ──────────────────────────────────────────────────────
 * Stands in for find_fallback_font: a fixed set of "installed fonts", each
 * with a coverage charset and a path. resolve() returns the first font that
 * covers the whole query and counts how many times it was invoked. */

typedef struct {
    FcCharSet  *coverage;
    const char *path;
} MockFont;

typedef struct {
    MockFont *fonts;
    size_t    font_count;
    int       calls;
} Mock;

static FcCharSet *charset_of(const char *text)
{
    FcCharSet *cs = FcCharSetCreate();
    const char *p = text;
    while (*p) {
        FcChar32 ch;
        int len = FcUtf8ToUcs4((const FcChar8 *)p, &ch, (int)strlen(p));
        if (len <= 0) break;
        FcCharSetAddChar(cs, ch);
        p += len;
    }
    return cs;
}

static char *mock_resolve(void *user, const char *text, int32_t weight,
                          FcCharSet **out_coverage)
{
    (void)weight;
    Mock *m = (Mock *)user;
    m->calls++;

    FcCharSet *want = charset_of(text);
    char *result = NULL;
    for (size_t i = 0; i < m->font_count; ++i) {
        if (FcCharSetIsSubset(want, m->fonts[i].coverage)) {
            result = strdup(m->fonts[i].path);
            if (out_coverage) *out_coverage = FcCharSetCopy(m->fonts[i].coverage);
            break;
        }
    }
    FcCharSetDestroy(want);
    return result;
}

/* ── Phrase generation ──────────────────────────────────────────────────── */

static void append_cp(char **p, FcChar32 cp)
{
    unsigned char *o = (unsigned char *)*p;
    if (cp < 0x80) {
        *o++ = (unsigned char)cp;
    } else if (cp < 0x800) {
        *o++ = (unsigned char)(0xC0 | (cp >> 6));
        *o++ = (unsigned char)(0x80 | (cp & 0x3F));
    } else {
        *o++ = (unsigned char)(0xE0 | (cp >> 12));
        *o++ = (unsigned char)(0x80 | ((cp >> 6) & 0x3F));
        *o++ = (unsigned char)(0x80 | (cp & 0x3F));
    }
    *p = (char *)o;
}

/* A distinct 2-codepoint phrase drawn from [base, base+span). */
static void make_phrase(char *buf, FcChar32 base, FcChar32 span, unsigned i)
{
    char *w = buf;
    append_cp(&w, base + (i % span));
    append_cp(&w, base + ((i * 13u + 7u) % span));
    *w = '\0';
}

static FcCharSet *range_charset(FcChar32 lo, FcChar32 hi)
{
    FcCharSet *cs = FcCharSetCreate();
    for (FcChar32 c = lo; c <= hi; ++c) FcCharSetAddChar(cs, c);
    return cs;
}

#define CJK_LO   0x4E00
#define CJK_HI   0x9FFF
#define CJK_SPAN 0x1000   /* keep phrases comfortably inside coverage */
#define LAT_LO   0x00C0
#define LAT_HI   0x017F
#define LAT_SPAN 0x0080

/* ── Tests ──────────────────────────────────────────────────────────────── */

/* The headline regression: a long session of all-distinct phrases in one
 * script must resolve exactly once, not once per phrase. */
TEST(resolves_once_under_frequent_distinct_composition)
{
    FcCharSet *cjk = range_charset(CJK_LO, CJK_HI);
    MockFont fonts[] = { { cjk, "NotoSansCJK" } };
    Mock mock = { fonts, 1, 0 };

    FallbackFontCache *c = fallback_cache_new(16, mock_resolve, &mock);
    ASSERT(c != NULL);

    for (unsigned i = 0; i < 5000; ++i) {
        char phrase[16];
        make_phrase(phrase, CJK_LO, CJK_SPAN, i);
        char *path = fallback_cache_lookup(c, phrase, 600);
        ASSERT(path != NULL);
        ASSERT(strcmp(path, "NotoSansCJK") == 0);
        free(path);
    }

    /* Resolver hit once; everything else served from coverage. */
    ASSERT_EQ(mock.calls, 1);
    ASSERT_EQ(fallback_cache_entry_count(c), 1);

    fallback_cache_free(c);
    FcCharSetDestroy(cjk);
}

/* Two scripts interleaved indefinitely resolve once each, sharing capacity. */
TEST(distinct_scripts_resolve_once_each)
{
    FcCharSet *cjk = range_charset(CJK_LO, CJK_HI);
    FcCharSet *lat = range_charset(LAT_LO, LAT_HI);
    MockFont fonts[] = { { lat, "NotoSans" }, { cjk, "NotoSansCJK" } };
    Mock mock = { fonts, 2, 0 };

    FallbackFontCache *c = fallback_cache_new(16, mock_resolve, &mock);
    ASSERT(c != NULL);

    for (unsigned i = 0; i < 2000; ++i) {
        char phrase[16];
        make_phrase(phrase, (i & 1) ? CJK_LO : LAT_LO,
                    (i & 1) ? CJK_SPAN : LAT_SPAN, i);
        char *path = fallback_cache_lookup(c, phrase, 600);
        ASSERT(path != NULL);
        free(path);
    }

    ASSERT_EQ(mock.calls, 2);
    ASSERT_EQ(fallback_cache_entry_count(c), 2);

    fallback_cache_free(c);
    FcCharSetDestroy(cjk);
    FcCharSetDestroy(lat);
}

/* When the working set exceeds capacity the cache must keep evicting (LRU),
 * never silently stop caching and re-resolve everything. */
TEST(over_capacity_evicts_lru_and_stays_bounded)
{
    /* Three disjoint single-codepoint "scripts", cache holds two. */
    FcCharSet *a = range_charset(0x3000, 0x3001);
    FcCharSet *b = range_charset(0x3100, 0x3101);
    FcCharSet *d = range_charset(0x3200, 0x3201);
    MockFont fonts[] = { { a, "A" }, { b, "B" }, { d, "D" } };
    Mock mock = { fonts, 3, 0 };

    FallbackFontCache *c = fallback_cache_new(2, mock_resolve, &mock);
    ASSERT(c != NULL);

    const char *qa = "\xe3\x80\x80"; /* U+3000 */
    const char *qb = "\xe3\x84\x80"; /* U+3100 */
    const char *qd = "\xe3\x88\x80"; /* U+3200 */

    char *p;
    p = fallback_cache_lookup(c, qa, 600); ASSERT(p && !strcmp(p, "A")); free(p);
    p = fallback_cache_lookup(c, qb, 600); ASSERT(p && !strcmp(p, "B")); free(p);
    ASSERT_EQ(mock.calls, 2);
    ASSERT_EQ(fallback_cache_entry_count(c), 2);

    /* A is the LRU; querying D evicts A but stays at capacity. */
    p = fallback_cache_lookup(c, qd, 600); ASSERT(p && !strcmp(p, "D")); free(p);
    ASSERT_EQ(mock.calls, 3);
    ASSERT_EQ(fallback_cache_entry_count(c), 2);

    /* B was used more recently than A, so it is still cached (no re-resolve). */
    p = fallback_cache_lookup(c, qb, 600); ASSERT(p && !strcmp(p, "B")); free(p);
    ASSERT_EQ(mock.calls, 3);

    /* A was evicted, so it must re-resolve now — caching did not stop. */
    p = fallback_cache_lookup(c, qa, 600); ASSERT(p && !strcmp(p, "A")); free(p);
    ASSERT_EQ(mock.calls, 4);
    ASSERT_EQ(fallback_cache_entry_count(c), 2);

    fallback_cache_free(c);
    FcCharSetDestroy(a);
    FcCharSetDestroy(b);
    FcCharSetDestroy(d);
}

/* A text covered by a superset of an already-cached coverage reuses it. */
TEST(superset_coverage_is_reused)
{
    FcCharSet *cjk = range_charset(CJK_LO, CJK_HI);
    MockFont fonts[] = { { cjk, "NotoSansCJK" } };
    Mock mock = { fonts, 1, 0 };

    FallbackFontCache *c = fallback_cache_new(16, mock_resolve, &mock);
    ASSERT(c != NULL);

    char *p;
    p = fallback_cache_lookup(c, "\xe4\xb8\x80", 600);          /* 一        */
    ASSERT(p != NULL); free(p);
    p = fallback_cache_lookup(c, "\xe4\xb8\x80\xe4\xba\x8c", 600); /* 一二   */
    ASSERT(p != NULL); free(p);

    ASSERT_EQ(mock.calls, 1);  /* second query is a coverage hit */

    fallback_cache_free(c);
    FcCharSetDestroy(cjk);
}

/* Unresolvable text returns NULL and is not cached (no negative caching). */
TEST(unresolvable_text_is_not_cached)
{
    MockFont fonts[] = { { range_charset(LAT_LO, LAT_HI), "NotoSans" } };
    Mock mock = { fonts, 1, 0 };

    FallbackFontCache *c = fallback_cache_new(16, mock_resolve, &mock);
    ASSERT(c != NULL);

    for (unsigned i = 0; i < 5; ++i) {
        char phrase[16];
        make_phrase(phrase, CJK_LO, CJK_SPAN, i);  /* no font covers CJK */
        char *path = fallback_cache_lookup(c, phrase, 600);
        ASSERT(path == NULL);
    }
    ASSERT_EQ(mock.calls, 5);                 /* retried, never cached */
    ASSERT_EQ(fallback_cache_entry_count(c), 0);

    fallback_cache_free(c);
    FcCharSetDestroy(fonts[0].coverage);
}

int main(void)
{
    printf("Running fallback_cache tests:\n");
    run_test_resolves_once_under_frequent_distinct_composition();
    run_test_distinct_scripts_resolve_once_each();
    run_test_over_capacity_evicts_lru_and_stays_bounded();
    run_test_superset_coverage_is_reused();
    run_test_unresolvable_text_is_not_cached();
    printf("\n%d/%d tests passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
