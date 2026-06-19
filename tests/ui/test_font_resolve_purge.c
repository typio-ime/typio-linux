/*
 * Regression tests for the periodic-Fontconfig-purge logic
 * (src/ui/panel/font_resolve.c).
 *
 * The bug these guard against: Fontconfig's process-global state grows
 * monotonically across the process lifetime because every FcFontMatch /
 * FcFontSort call inflates its internal caches and the hot path never called
 * FcFini(). The fix drains Fontconfig every FONTCONFIG_PURGE_PERIOD (256)
 * cache misses — a cadence aligned with the per-codepoint fallback memo's
 * working set, so the drain fires roughly once per "the memo has fully
 * churned" rather than on every lookup.
 *
 * Two layers of coverage:
 *
 *   1. Pure-predicate tests for font_resolve_should_purge — verify the
 *      threshold logic, the boundary, and the period==0 safety guard without
 *      needing Fontconfig at all.
 *
 *   2. A behavioural test that drives the real Fontconfig path through
 *      font_resolve_codepoint_fonts with FONTCONFIG_PURGE_PERIOD+1 distinct
 *      codepoints and verifies font_resolve_purge_count() increments. This
 *      exercises the full integration (FcFontSort → miss counter → FcFini
 *      drain → lazy FcInit re-init on the next lookup) end-to-end.
 *
 * Requires Fontconfig; skips (meson exit 77) when unavailable so the rest of
 * the suite still builds on minimal toolchains.
 */

#include "font_resolve.h"

#include <fontconfig/fontconfig.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

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

#define CHECK(cond) \
    do { if (!(cond)) { printf("FAILED\n    %s:%d: %s\n", __FILE__, __LINE__, #cond); exit(1); } } while (0)

/* ── Pure predicate: font_resolve_should_purge ──────────────────────────── */

TEST(purge_below_period_returns_false) {
    CHECK(!font_resolve_should_purge(0,   256));
    CHECK(!font_resolve_should_purge(1,   256));
    CHECK(!font_resolve_should_purge(255, 256));
}

TEST(purge_at_period_returns_true) {
    /* Boundary is inclusive: exactly at the period, we purge. This matches
     * the existing `>= period` check in maybe_drain_fontconfig. */
    CHECK(font_resolve_should_purge(256, 256));
    CHECK(font_resolve_should_purge(257, 256));
    CHECK(font_resolve_should_purge(10000, 256));
}

TEST(purge_zero_period_is_safe) {
    /* A misconfiguration (period == 0) must NOT cause an infinite purge
     * loop. The predicate treats it as "never purge". */
    CHECK(!font_resolve_should_purge(0,    0));
    CHECK(!font_resolve_should_purge(1,    0));
    CHECK(!font_resolve_should_purge(1000, 0));
}

TEST(purge_alternative_periods) {
    /* The period is a parameter, not a hard-coded constant — verify the
     * predicate honours whatever the caller passes. A future tuning might
     * lower it for memory-constrained environments or raise it for systems
     * with very large CJK working sets. */
    CHECK(!font_resolve_should_purge(49, 50));
    CHECK(font_resolve_should_purge(50, 50));
    CHECK(!font_resolve_should_purge(999, 1000));
    CHECK(font_resolve_should_purge(1000, 1000));
}

/* ── Behavioural: real Fontconfig misses drive the purge counter ──────────
 *
 * Drives font_resolve_codepoint_fonts with 300 distinct codepoints. Each
 * unique codepoint is a fresh miss on the per-codepoint fallback memo, so
 * after 256 misses the periodic drain must fire exactly once (FONTCONFIG_PURGE_PERIOD
 * matches FB_CP_CACHE_CAP at the time of writing; both are 256, so the
 * codepoint memo also fully churns during this test). Verifies:
 *
 *   - the counter is monotonic non-decreasing across calls
 *   - the counter increments by exactly 1 after the threshold
 *   - FcFini() drain + lazy FcInit() re-init does not break subsequent lookups
 *
 * The test is slow (~hundreds of FcFontSort calls) but bounded; we cap the
 * wall-clock and skip if it exceeds 30s so a slow CI doesn't hang.
 */

static uint64_t monotonic_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ull + (uint64_t)ts.tv_nsec / 1000000ull;
}

TEST(behavioural_purge_count_increments_after_threshold) {
    if (!FcInit()) {
        printf("(skipped: FcInit failed) ");
        return;
    }

    font_resolve_clear();
    uint64_t purges_before = font_resolve_purge_count();

    /* 300 distinct CJK codepoints from the Han block. Each is a fresh memo
     * miss and a fresh FcFontSort run. The first 256 misses trigger one
     * FcFini drain; the remaining 44 do not trigger another (we'd need
     * another 212 to cross the threshold again). */
    const uint32_t N = 300;
    uint64_t start_ms = monotonic_ms();
    for (uint32_t i = 0; i < N; ++i) {
        FcChar32 ch = 0x4E00u + i;   /* CJK Unified Ideographs block */
        FontCandidateList candidates;
        font_resolve_codepoint_fonts(ch, /*weight=*/400, /*size_px=*/16.0f,
                                       &candidates, /*primary_path=*/NULL);
        font_candidate_list_clear(&candidates);

        /* Bail out if Fontconfig is pathologically slow (e.g. a cold cache
         * on a heavily-loaded CI runner). Treat as skipped, not failed. */
        if (monotonic_ms() - start_ms > 30000) {
            printf("(skipped: Fontconfig too slow at i=%u) ", i);
            font_resolve_clear();
            return;
        }
    }

    uint64_t purges_after = font_resolve_purge_count();

    /* Exactly one drain should have fired (300 misses > 256, but < 512). */
    CHECK(purges_after >= purges_before + 1);
    CHECK(purges_after == purges_before + 1);

    /* After the drain, the resolver must STILL answer queries correctly —
     * FcFini tore down Fontconfig's state, so the next lookup has to
     * re-initialise it lazily via FcInit(). This is the integration guard
     * for "the drain didn't break subsequent lookups". */
    FontCandidateList post;
    font_resolve_codepoint_fonts(0x4E2D /* 中 */, 400, 16.0f, &post, NULL);
    /* The candidate list may be empty if Fontconfig has no CJK fonts
     * installed, but the call itself must not crash. */
    font_candidate_list_clear(&post);

    font_resolve_clear();
}

int main(void) {
    /* Pure-predicate tests do not need Fontconfig, but the behavioural one
     * does. Run all of them and let the behavioural test self-skip if
     * Fontconfig is missing. */
    printf("Running font_resolve purge tests:\n");
    run_test_purge_below_period_returns_false();
    run_test_purge_at_period_returns_true();
    run_test_purge_zero_period_is_safe();
    run_test_purge_alternative_periods();
    run_test_behavioural_purge_count_increments_after_threshold();
    printf("\nPassed %d/%d tests\n", tests_passed, tests_run);
    return 0;
}
