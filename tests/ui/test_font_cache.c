/*
 * Regression tests for the font object cache (src/ui/panel/font_cache.c).
 *
 * The bug these guard against: candidate-selection lag that worsened over
 * long sessions. The root cause was an O(N) linear scan over an unbounded
 * array of (path × size × weight × fallback) FontObj entries — every unique
 * fractional scale, weight, or per-codepoint fallback font added a permanent
 * entry, and after hours of mixed-scale CJK typing the per-keystroke lookup
 * cost grew linearly with cache size. The fix caps the table at
 * FONT_OBJ_CACHE_CAP with an LRU-evicting open-addressing hash so the lookup
 * stays O(1) regardless of how many (size, weight) tuples accumulate.
 *
 * These tests verify:
 *   - the cap is enforced (no unbounded growth)
 *   - the underlying FT_Face stays alive across LRU eviction (critical:
 *     TypioTextShape borrows FT_Face pointers, so evicting one mid-session
 *     is a use-after-free)
 *   - the same (path, size, weight) tuple always resolves to a stable
 *     FontObj while it remains in the cache
 *   - distinct (size, weight) variants of the same font file share one FT_Face
 *
 * Uses the system Inter font; skips (exit 77) if not present so the suite
 * still builds on minimal toolchains.
 */

#include "font_cache.h"

#include <ft2build.h>
#include FT_FREETYPE_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define TEST_FONT_PATH "/usr/share/fonts/inter/Inter-Regular.ttf"

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

TEST(obj_table_cap_enforced) {
    /* Insert CAP + 32 distinct (size) variants of one font file. Without the
     * LRU cap this would grow the table without bound; with it, the occupancy
     * stays at exactly FONT_OBJ_CACHE_CAP and the LRU victim's hb_font is
     * freed to make room for each new entry. */
    CHECK(font_cache_init());

    uint32_t initial_faces = font_cache_face_count();
    CHECK(initial_faces == 0);

    const uint32_t to_insert = FONT_OBJ_CACHE_CAP + 32;
    for (uint32_t i = 0; i < to_insert; ++i) {
        float size = 8.0f + (float)i;   /* unique size per entry */
        FontObj *o = font_cache_get_or_create(TEST_FONT_PATH, size, 400);
        CHECK(o != NULL);
        CHECK(o->face != NULL);
        CHECK(o->hb_font != NULL);
        CHECK(o->font_id != 0);
    }

    CHECK(font_cache_obj_count() == FONT_OBJ_CACHE_CAP);

    /* The face table is unbounded but tiny in practice. All CAP+32 inserts
     * above used the same path, so exactly one FT_Face was opened and shared
     * across every (size, weight) variant. This is the critical invariant:
     * evicting a FontObj wrapper is safe ONLY because the FT_Face it borrows
     * stays alive in the face table for the process lifetime. */
    CHECK(font_cache_face_count() == 1);

    font_cache_clear();
    CHECK(font_cache_obj_count() == 0);
    CHECK(font_cache_face_count() == 0);
}

TEST(distinct_faces_do_not_alias) {
    /* Two different font files must open two distinct FT_Face handles and
     * report two face-table entries. Verifies the face table keying. */
    CHECK(font_cache_init());

    FontObj *a = font_cache_get_or_create(TEST_FONT_PATH, 16.0f, 400);
    FontObj *b = font_cache_get_or_create(
        "/usr/share/fonts/inter/Inter-Bold.ttf", 16.0f, 700);
    if (!b) {
        /* Inter-Bold.ttf may be absent on a stripped toolchain; the first
         * part of the assertion still stands. */
        CHECK(a != NULL);
        printf("(skipped bold half: file absent) ");
    } else {
        CHECK(a != b);
        CHECK(a->face != b->face);
        CHECK(font_cache_face_count() >= 2);
    }

    font_cache_clear();
}

TEST(same_key_returns_stable_pointer) {
    /* Repeated lookups for the same (path, size, weight) must return the same
     * FontObj pointer (and the same font_id) while the entry remains cached.
     * This is the contract the atlas relies on: its (font_id, glyph_id) key
     * must resolve to the same slot across calls. */
    CHECK(font_cache_init());

    FontObj *first = font_cache_get_or_create(TEST_FONT_PATH, 16.0f, 400);
    CHECK(first != NULL);
    uint32_t first_id = first->font_id;

    for (int i = 0; i < 16; ++i) {
        FontObj *again = font_cache_get_or_create(TEST_FONT_PATH, 16.0f, 400);
        CHECK(again == first);
        CHECK(again->font_id == first_id);
    }

    font_cache_clear();
}

TEST(face_survives_obj_eviction) {
    /* The headline regression: churning many (size, weight) variants must
     * evict FontObj wrappers but must NOT close their shared FT_Face. After
     * evicting past the cap, every surviving/re-created FontObj still has a
     * non-NULL face pointer that is byte-identical to the original face. */
    CHECK(font_cache_init());

    FontObj *first = font_cache_get_or_create(TEST_FONT_PATH, 12.0f, 400);
    CHECK(first != NULL);
    FT_Face shared_face = first->face;
    CHECK(shared_face != NULL);

    /* Push enough distinct entries to force first (12.0f, 400) out of the
     * LRU table. The face table is unaffected. */
    for (uint32_t i = 0; i < FONT_OBJ_CACHE_CAP + 4; ++i) {
        float size = 100.0f + (float)i;
        FontObj *o = font_cache_get_or_create(TEST_FONT_PATH, size, 400);
        CHECK(o != NULL);
        /* Every variant of the same file must share the one FT_Face. */
        CHECK(o->face == shared_face);
    }

    /* The face table is unchanged: still exactly one mmap'd file. */
    CHECK(font_cache_face_count() == 1);
    /* The obj table is still capped. */
    CHECK(font_cache_obj_count() == FONT_OBJ_CACHE_CAP);

    /* Re-requesting the long-evicted (12.0f, 400) re-opens a wrapper but
     * reuses the same FT_Face; only the font_id is freshly minted. */
    FontObj *reopened = font_cache_get_or_create(TEST_FONT_PATH, 12.0f, 400);
    CHECK(reopened != NULL);
    CHECK(reopened->face == shared_face);
    CHECK(reopened->font_id != first->font_id);   /* monotonic ids */

    font_cache_clear();
}

TEST(clear_resets_all_counters) {
    CHECK(font_cache_init());

    for (uint32_t i = 0; i < 8; ++i) {
        font_cache_get_or_create(TEST_FONT_PATH, 10.0f + (float)i, 400);
    }
    CHECK(font_cache_obj_count() > 0);
    CHECK(font_cache_face_count() > 0);

    font_cache_clear();
    CHECK(font_cache_obj_count() == 0);
    CHECK(font_cache_face_count() == 0);

    /* After clear the table is reusable. */
    FontObj *o = font_cache_get_or_create(TEST_FONT_PATH, 16.0f, 400);
    CHECK(o != NULL);
    CHECK(font_cache_obj_count() == 1);

    font_cache_clear();
}

int main(void) {
    if (access(TEST_FONT_PATH, R_OK) != 0) {
        printf("SKIP: %s unavailable on this system\n", TEST_FONT_PATH);
        return 77;   /* meson treats 77 as "skipped" */
    }

    printf("Running font_cache LRU tests:\n");
    run_test_obj_table_cap_enforced();
    run_test_distinct_faces_do_not_alias();
    run_test_same_key_returns_stable_pointer();
    run_test_face_survives_obj_eviction();
    run_test_clear_resets_all_counters();
    printf("\nPassed %d/%d tests\n", tests_passed, tests_run);
    return 0;
}
