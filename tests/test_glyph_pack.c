/*
 * Regression tests for the glyph-atlas skyline packer (src/ui/glyph_pack.c).
 *
 * The bug these guard against: candidate-switch lag that returned across
 * multiple graphics libraries. The root cause was "one texture per text run,
 * uploaded synchronously" — every candidate page built ~20 new R8 textures and
 * blocked the IME loop on vkWaitForFences. The fix rasterises each glyph once
 * into a shared, persistent atlas and references sub-rects, so a warmed atlas
 * uploads nothing while paging. This file exercises the packing geometry that
 * makes that sharing correct: placements must stay in-bounds, never overlap,
 * keep a gutter, wrap to new shelves, and report "full" so the atlas can reset.
 *
 * Pure integer geometry — no GPU, no fonts.
 */

#include "glyph_pack.h"

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

#define CHECK(cond) \
    do { if (!(cond)) { printf("FAILED\n    %s:%d: %s\n", __FILE__, __LINE__, #cond); exit(1); } } while (0)

/* ── A coverage grid to prove non-overlap across many placements ───────── */

typedef struct { uint8_t *cells; uint32_t dim; } Grid;

static Grid grid_new(uint32_t dim) {
    Grid g = { .cells = calloc((size_t)dim * dim, 1), .dim = dim };
    return g;
}
static void grid_free(Grid *g) { free(g->cells); g->cells = NULL; }

/* Mark a w×h cell and assert it was previously empty (no overlap) and within
 * bounds. */
static void grid_claim(Grid *g, uint32_t u, uint32_t v, uint32_t w, uint32_t h) {
    CHECK(u + w <= g->dim);
    CHECK(v + h <= g->dim);
    for (uint32_t y = v; y < v + h; ++y)
        for (uint32_t x = u; x < u + w; ++x) {
            CHECK(g->cells[(size_t)y * g->dim + x] == 0);  /* no overlap */
            g->cells[(size_t)y * g->dim + x] = 1;
        }
}

/* ── Tests ─────────────────────────────────────────────────────────────── */

TEST(first_placement_is_origin) {
    GlyphPacker p = {0};
    uint32_t u = 99, v = 99;
    CHECK(glyph_packer_place(&p, 64, 1, 10, 12, &u, &v));
    CHECK(u == 0 && v == 0);
}

TEST(advances_with_gutter) {
    GlyphPacker p = {0};
    uint32_t u, v;
    CHECK(glyph_packer_place(&p, 256, 1, 10, 12, &u, &v));
    CHECK(u == 0 && v == 0);
    CHECK(glyph_packer_place(&p, 256, 1, 8, 12, &u, &v));
    CHECK(u == 11 && v == 0);   /* prev width 10 + 1 px gutter */
}

TEST(wraps_to_new_shelf) {
    /* dim 20, pad 1: two 9-wide glyphs fit a row (0..9, 10..19), the third
     * wraps to a new shelf below the tallest of the first row. */
    GlyphPacker p = {0};
    uint32_t u, v;
    CHECK(glyph_packer_place(&p, 20, 1, 9, 5, &u, &v));
    CHECK(u == 0 && v == 0);
    CHECK(glyph_packer_place(&p, 20, 1, 9, 7, &u, &v));
    CHECK(u == 10 && v == 0);
    CHECK(glyph_packer_place(&p, 20, 1, 9, 4, &u, &v));
    CHECK(u == 0 && v == 8);    /* new shelf at 7 (tallest) + 1 gutter */
}

TEST(reports_full_when_exhausted) {
    /* A small atlas eventually rejects further placements (signal to reset). */
    GlyphPacker p = {0};
    uint32_t u, v;
    int placed = 0;
    for (int i = 0; i < 100000; ++i) {
        if (!glyph_packer_place(&p, 32, 1, 7, 7, &u, &v)) break;
        placed++;
    }
    CHECK(placed > 0);
    CHECK(placed <= 16);                       /* 4×4 cells of 8px in a 32px atlas */
    CHECK(!glyph_packer_place(&p, 32, 1, 7, 7, &u, &v));  /* stays full */
}

TEST(oversized_glyph_never_fits) {
    GlyphPacker p = {0};
    uint32_t u, v;
    CHECK(!glyph_packer_place(&p, 16, 1, 16, 4, &u, &v));  /* w + pad > dim */
    CHECK(!glyph_packer_place(&p, 16, 1, 4, 16, &u, &v));  /* h + pad > dim */
    CHECK(!glyph_packer_place(&p, 16, 1, 0, 4, &u, &v));   /* zero extent */
}

TEST(many_mixed_glyphs_never_overlap) {
    /* Stream a few thousand varied glyphs (mimicking a long CJK session),
     * resetting on "full", and prove every placement stays in-bounds and
     * collision-free within each atlas generation. */
    const uint32_t dim = 256;
    GlyphPacker p = {0};
    Grid g = grid_new(dim);
    uint32_t seed = 1234567u;
    int total = 0, generations = 1;

    for (int i = 0; i < 6000; ++i) {
        seed = seed * 1103515245u + 12345u;
        uint32_t w = 4 + (seed >> 16) % 24;        /* 4..27 */
        seed = seed * 1103515245u + 12345u;
        uint32_t h = 4 + (seed >> 16) % 24;        /* 4..27 */

        uint32_t u, v;
        if (!glyph_packer_place(&p, dim, 1, w, h, &u, &v)) {
            /* Atlas full → reset (a fresh generation reuses the surface). */
            p = (GlyphPacker){0};
            memset(g.cells, 0, (size_t)dim * dim);
            generations++;
            CHECK(glyph_packer_place(&p, dim, 1, w, h, &u, &v));
        }
        grid_claim(&g, u, v, w, h);
        total++;
    }
    CHECK(total == 6000);
    CHECK(generations > 1);   /* the stream really did overflow + reset */
    grid_free(&g);
}

int main(void) {
    printf("glyph_pack tests:\n");
    run_test_first_placement_is_origin();
    run_test_advances_with_gutter();
    run_test_wraps_to_new_shelf();
    run_test_reports_full_when_exhausted();
    run_test_oversized_glyph_never_fits();
    run_test_many_mixed_glyphs_never_overlap();
    printf("%d/%d passed\n", tests_passed, tests_run);
    return tests_passed == tests_run ? 0 : 1;
}
