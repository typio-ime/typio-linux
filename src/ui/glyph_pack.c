#include "glyph_pack.h"

bool glyph_packer_place(GlyphPacker *p, uint32_t dim, uint32_t pad,
                        uint32_t w, uint32_t h,
                        uint32_t *out_u, uint32_t *out_v)
{
    if (!p || !out_u || !out_v) return false;
    if (w == 0 || h == 0) return false;
    if (w + pad > dim || h + pad > dim) return false;   /* never fits */

    /* Wrap to a new shelf when the row is exhausted. */
    if (p->shelf_x + w + pad > dim) {
        p->shelf_y += p->shelf_h + pad;
        p->shelf_x  = 0;
        p->shelf_h  = 0;
    }
    /* Atlas exhausted vertically — caller resets and retries. */
    if (p->shelf_y + h + pad > dim) return false;

    *out_u = p->shelf_x;
    *out_v = p->shelf_y;

    p->shelf_x += w + pad;
    if (h > p->shelf_h) p->shelf_h = h;
    return true;
}
