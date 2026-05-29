/**
 * @file renderer.h
 * @brief Minimal text-rendering interface used by the candidate popup
 *
 * libtypio used to ship `typio/abi/renderer.h` so any host could plug a
 * renderer (cairo/skia/flux) into the popup composer. After the
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
typedef struct TypioTextLayout TypioTextLayout;

typedef struct TypioTextEngineVTable {
    /* Build a colour-independent text layout. Colour is applied at draw
     * time (see typio_flux_fill_layout's tint), so it is neither stored on
     * the layout nor part of its LRU cache identity. */
    TypioTextLayout *(*create_layout)(void *engine,
                                      const char *text,
                                      const char *font_desc);
    void (*get_metrics)(TypioTextLayout *layout, float *out_w, float *out_h);
    float (*get_baseline)(TypioTextLayout *layout);
    void (*free_layout)(TypioTextLayout *layout);
} TypioTextEngineVTable;

typedef struct TypioTextEngine {
    void *priv;
    const TypioTextEngineVTable *vtable;
} TypioTextEngine;

#ifdef __cplusplus
}
#endif

#endif /* TYPIO_WL_RENDERER_H */
