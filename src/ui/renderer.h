#ifndef TYPIO_WL_FLUX_RENDERER_H
#define TYPIO_WL_FLUX_RENDERER_H

#include "typio_build_config.h"
#include "text.h"

#ifdef HAVE_FLUX
#include <flux/flux.h>
#endif
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifdef HAVE_FLUX
TypioTextEngine *typio_flux_engine_create(void);
void typio_flux_engine_destroy(TypioTextEngine *engine);

/* Purge all font caches (file, object, fallback) and drain Fontconfig's
 * internal caches.  Safe to call periodically from the host event loop or
 * after a configuration reload.  Subsequent text layout operations will
 * re-populate caches on demand. */
void typio_flux_engine_purge_font_caches(void);

/* Shared, lazily-created flux device.
 *
 * Non-headless: created with the Wayland WSI instance extensions and the
 * swapchain device extension so the candidate popup can present a Vulkan
 * swapchain directly onto its zwp_input_popup_surface_v2 wl_surface.
 * Returns NULL if no Vulkan device is available. */
flux_device *typio_flux_device_get(void);

/*
 * Record a shaped text layout into a flux canvas as a tinted coverage blit.
 *
 * The layout owns one colour-independent R8 coverage texture (built lazily);
 * `tint` supplies the colour at draw time, so the same layout/texture can be
 * drawn in any colour (normal / muted / selection) without rebuilding or
 * re-uploading. Must be called between flux_canvas_begin / flux_canvas_end.
 *
 * x, y : top-left origin of the layout in surface pixels (baseline is added
 *        internally from the layout metrics).
 */
bool typio_flux_fill_layout(flux_canvas *canvas, flux_arena *arena,
                            TypioTextLayout *layout, float x, float y,
                            TypioColor tint);

void typio_flux_layout_free(TypioTextLayout *layout);
#endif /* HAVE_FLUX */

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_FLUX_RENDERER_H */
