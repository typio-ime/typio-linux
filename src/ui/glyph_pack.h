#pragma once

#include <stdbool.h>
#include <stdint.h>

/*
 * Skyline shelf packer for the glyph atlas (see renderer.c).
 *
 * Online rectangle packing into a square atlas of side `dim`, leaving a `pad`
 * gutter so bilinear sampling at a glyph edge cannot bleed into its neighbour.
 * Glyphs of a single font size have near-uniform height, so shelf packing is
 * both simple and tight here.
 *
 * State is plain data, caller-owned, and zero-initialised for an empty atlas.
 * Extracted from the renderer purely so it can be unit-tested without a GPU.
 */
typedef struct {
    uint32_t shelf_x;   /* x cursor within the current shelf      */
    uint32_t shelf_y;   /* y of the current shelf's top edge      */
    uint32_t shelf_h;   /* height of the current (tallest) shelf  */
} GlyphPacker;

/*
 * Reserve a w×h cell. On success, writes the top-left placement to
 * (*out_u, *out_v), advances the packer, and returns true.
 *
 * Returns false when the cell does not fit in the remaining atlas. The caller
 * distinguishes the two false cases by retrying after a reset: a transient
 * "shelf exhausted" succeeds on a fresh packer, while a cell that exceeds the
 * atlas (w/h + pad > dim) fails again and must be skipped.
 */
bool glyph_packer_place(GlyphPacker *p, uint32_t dim, uint32_t pad,
                        uint32_t w, uint32_t h,
                        uint32_t *out_u, uint32_t *out_v);
