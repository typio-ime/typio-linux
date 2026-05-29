/**
 * @file candidate_panel_paint.c
 * @brief Record the candidate panel into a flux canvas (GPU).
 *
 * Rectangles are solid premultiplied fills; glyphs are filled outlines via
 * typio_text_shape_fill. All coordinates are converted from logical layout
 * pixels to physical surface pixels with the output scale factor.
 */

#include "paint.h"
#include "renderer.h"

#include <stdint.h>

/* ── Colour helpers ─────────────────────────────────────────────────── */

static inline uint8_t u8(double v)
{
    if (v <= 0.0) return 0;
    if (v >= 1.0) return 255;
    return (uint8_t)(v * 255.0 + 0.5);
}

/* flux canvas colours are premultiplied; flux_color_rgba_premul takes
 * straight 8-bit components and premultiplies by alpha for us. */
static inline flux_color pcol(double r, double g, double b, double a)
{
    return flux_color_rgba_premul(u8(r), u8(g), u8(b), u8(a));
}

static inline flux_rect rect_px(float x, float y, float w, float h, float s)
{
    return (flux_rect){ x * s, y * s, w * s, h * s };
}

/* ── Row + chrome ───────────────────────────────────────────────────── */

static inline TypioColor tcol(double r, double g, double b)
{
    return (TypioColor){ (float)r, (float)g, (float)b, 1.0f };
}

static void record_row(flux_canvas *cv, flux_arena *ar,
                       const PanelRow *row, bool selected,
                       const TypioPanelPalette *p, float s)
{
    /* One colour-independent coverage layout per row; the colour is the
     * tint applied here, so selecting a row only adds the highlight rect
     * and swaps the tint — no new glyph texture is built or uploaded. */
    TypioColor label_tint = selected ? tcol(p->selection_text_r, p->selection_text_g, p->selection_text_b)
                                     : tcol(p->muted_r, p->muted_g, p->muted_b);
    TypioColor text_tint  = selected ? label_tint
                                     : tcol(p->text_r, p->text_g, p->text_b);

    if (selected) {
        flux_canvas_fill_rect_color(
            cv, rect_px((float)row->x + 1, (float)row->y + 1,
                        (float)row->w - 2, (float)row->h - 2, s),
            pcol(p->selection_r, p->selection_g, p->selection_b, p->selection_a));
    }

    typio_text_shape_fill(cv, ar, row->label_layout, row->label_x * s, row->label_y * s, label_tint);
    typio_text_shape_fill(cv, ar, row->layout,       row->text_x  * s, row->text_y  * s, text_tint);
}

static void record_border(flux_canvas *cv, const PanelGeometry *g, float s)
{
    const TypioPanelPalette *p = g->palette;
    flux_color bc = pcol(p->border_r, p->border_g, p->border_b, p->border_a);
    float W = (float)g->panel_w;
    float H = (float)g->panel_h;

    flux_canvas_fill_rect_color(cv, rect_px(0,     0,     W, 1, s), bc);  /* top    */
    flux_canvas_fill_rect_color(cv, rect_px(0,     H - 1, W, 1, s), bc);  /* bottom */
    flux_canvas_fill_rect_color(cv, rect_px(0,     0,     1, H, s), bc);  /* left   */
    flux_canvas_fill_rect_color(cv, rect_px(W - 1, 0,     1, H, s), bc);  /* right  */
}

static void record_mode_label(flux_canvas *cv, flux_arena *ar,
                              const PanelGeometry *g, float s)
{
    const TypioPanelPalette *p = g->palette;
    if (!g->mode_layout || g->mode_h <= 0) return;

    if (g->mode_divider_y >= 0) {
        flux_canvas_fill_rect_color(
            cv, rect_px((float)PANEL_PAD_X, (float)g->mode_divider_y + 0.5f,
                        (float)(g->panel_w - 2 * PANEL_PAD_X), 1, s),
            pcol(p->border_r, p->border_g, p->border_b, p->border_a * 0.5));
    }
    typio_text_shape_fill(cv, ar, g->mode_layout, g->mode_x * s, g->mode_y * s,
                           tcol(p->muted_r, p->muted_g, p->muted_b));
}

/* ── Public API ─────────────────────────────────────────────────────── */

void panel_record(const PanelPaintTarget *target,
                  const PanelGeometry *geom,
                  int selected)
{
    if (!target || !target->canvas || !geom || !geom->palette) return;

    flux_canvas *cv = target->canvas;
    flux_arena  *ar = target->arena;
    float        s  = geom->scale > 0.0f ? geom->scale : 1.0f;

    record_border(cv, geom, s);

    if (geom->preedit_layout) {
        const TypioPanelPalette *p = geom->palette;
        typio_text_shape_fill(cv, ar, geom->preedit_layout,
                               geom->pre_x * s, geom->pre_y * s,
                               tcol(p->preedit_r, p->preedit_g, p->preedit_b));
    }

    for (size_t i = 0; i < geom->row_count; ++i) {
        bool sel = selected >= 0 && (size_t)selected == i;
        record_row(cv, ar, &geom->rows[i], sel, geom->palette, s);
    }

    record_mode_label(cv, ar, geom, s);
}
