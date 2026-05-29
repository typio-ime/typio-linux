/**
 * @file renderer.h
 * @brief Minimal text-rendering interface used by the candidate panel
 *
 * libtypio used to ship `typio/abi/renderer.h` so any host could plug a
 * renderer (cairo/skia/flux) into the panel composer. After the
 * orphan-renderer cleanup these types are no longer part of libtypio's
 * public ABI and now live here, alongside the only host that uses them.
 */

#ifndef TYPIO_WL_RENDERER_H
#define TYPIO_WL_RENDERER_H

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TypioColor {
    float r;
    float g;
    float b;
    float a;
} TypioColor;

/* Opaque text-layout handle owned by the renderer implementation. */
typedef struct TypioTextShape TypioTextShape;

typedef struct TypioTextShaperVTable {
    /* Build a colour-independent text layout. Colour is applied at draw
     * time (see typio_text_shape_fill's tint), so it is neither stored on
     * the layout nor part of its LRU cache identity. */
    TypioTextShape *(*create_layout)(void *engine,
                                      const char *text,
                                      const char *font_desc);
    void (*get_metrics)(TypioTextShape *layout, float *out_w, float *out_h);
    float (*get_baseline)(TypioTextShape *layout);
    void (*free_layout)(TypioTextShape *layout);
} TypioTextShaperVTable;

typedef struct TypioTextShaper {
    void *priv;
    const TypioTextShaperVTable *vtable;
} TypioTextShaper;

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_RENDERER_H */
