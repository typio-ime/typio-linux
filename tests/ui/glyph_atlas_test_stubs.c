/*
 * Stub definitions for symbols referenced by glyph_atlas.c that the
 * glyph_atlas_predicates test does NOT exercise. The test only touches the
 * pure-predicate and queue-management code paths (test_init_slots,
 * test_push_pending, flush with an injected upload fn); it never calls
 * glyph_atlas_get / glyph_atlas_ensure / glyph_atlas_shutdown, so the
 * device/upload/font/FreeType/flux functions those paths reference are
 * never actually invoked at runtime.
 *
 * Linker still needs symbol definitions, though, so we provide no-op / NULL
 * stubs. If glyph_atlas.c ever calls one of these from a path the test
 * exercises, the test will fail loudly (NULL deref, non-zero return, etc.)
 * rather than silently passing — which is the desired failure mode for a
 * stub that has drifted out of sync with the real implementation.
 */

#include "glyph_upload.h"
#include "font_cache.h"
#include "device.h"

#include <flux/flux.h>

#include <ft2build.h>
#include FT_FREETYPE_H

/* device.c */
flux_device *typio_render_device_get(void) { return NULL; }

/* glyph_upload.c */
void glyph_upload_shutdown(void) { }
bool glyph_upload_regions(flux_image *img,
                          const GlyphUploadRegion *regions, size_t count)
{
    (void)img; (void)regions; (void)count;
    return false;   /* unreachable in the predicate/queue tests */
}

/* font_cache.c */
void font_cache_apply(FT_Face face, float size, int32_t weight)
{
    (void)face; (void)size; (void)weight;
}

/* flux — image create/release are only called by glyph_atlas_ensure, which
 * the tests bypass via test_init_slots. */
flux_result flux_image_create(flux_device *dev, const flux_image_desc *desc,
                               flux_image **out)
{
    (void)dev; (void)desc;
    if (out) *out = NULL;
    return FLUX_ERROR_INVALID_STATE;   /* unreachable in the predicate/queue tests */
}

void flux_image_release(flux_image *img)
{
    (void)img;
}
