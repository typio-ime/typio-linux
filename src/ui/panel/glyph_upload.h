/**
 * @file glyph_upload.h
 * @brief Persistent Vulkan staging context for glyph-atlas sub-region uploads.
 *
 * Owns a command pool, staging buffer, and fence reused across every glyph
 * upload, so a cache miss costs one buffer copy instead of a full
 * VkCommandPool + VkBuffer + VkFence create/destroy cycle (~50us each). The
 * upload is synchronous from the CPU's perspective (fence wait after submit);
 * the atlas batches misses so a warmed atlas uploads nothing during navigation.
 *
 * Two entry points:
 *   - glyph_upload_region:       single region, synchronous
 *   - glyph_upload_regions:      N regions in one vkQueueSubmit + vkWaitForFences,
 *                                used by the per-frame batched path so that a
 *                                cold atlas re-warm after a reclaim costs one
 *                                GPU round-trip per frame instead of one per glyph.
 */
#ifndef TYPIO_WL_GLYPH_UPLOAD_H
#define TYPIO_WL_GLYPH_UPLOAD_H

#include <flux/flux.h>

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* One region to upload. @data is borrowed for the duration of the call only;
 * the caller may free or reuse it as soon as glyph_upload_regions returns. */
typedef struct GlyphUploadRegion {
    uint32_t    x, y, w, h;   /* atlas sub-rect, pixels                       */
    const void *data;         /* tightly packed @w×@h R8 coverage (borrowed) */
    size_t      bytes;        /* must equal @w * @h                           */
} GlyphUploadRegion;

/* Upload an R8 sub-region (@w×@h at offset @x,@y, @bytes of tightly packed
 * coverage) into @img. The staging buffer grows on demand and is reused.
 * Returns false on any device/allocation failure. Synchronous: the copy has
 * completed on return. */
bool glyph_upload_region(flux_image *img,
                         uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                         const void *data, size_t bytes);

/* Upload @count sub-regions into @img in a SINGLE vkQueueSubmit + vkWaitForFences,
 * copying each region's @data into one shared staging buffer at successive
 * offsets. @regions may be NULL when @count is 0 (no-op). Returns false on any
 * failure; on partial failure (fence timeout) the regions are left untouched
 * in the destination image (caller should treat all @count regions as not
 * uploaded). Synchronous: every copy has completed on return. */
bool glyph_upload_regions(flux_image *img,
                          const GlyphUploadRegion *regions, size_t count);

/* Destroy the persistent context. The caller must have drained the device
 * (or there is no device yet). Safe to call when never initialised. */
void glyph_upload_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_GLYPH_UPLOAD_H */
