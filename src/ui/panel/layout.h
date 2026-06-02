/**
 * @file layout.h
 * @brief Candidate panel geometry: LRU layout cache and immutable geometry snapshots.
 */

#ifndef TYPIO_WL_PANEL_LAYOUT_H
#define TYPIO_WL_PANEL_LAYOUT_H

#include "theme.h"
#include "content.h"
#include "text_shaper.h"
#include "typio/abi/input_context.h"
#include "typio/runtime/instance.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* ── Constants ──────────────────────────────────────────────────────── */

#define PANEL_LAYOUT_CACHE_CAP  128  /* LRU cache capacity (entries)     */
#define PANEL_MAX_ROWS          16   /* max candidates shown per page     */
#define PANEL_MIN_WIDTH         220  /* minimum panel width (logical px)  */
#define PANEL_PAD_X             10
#define PANEL_PAD_Y             6
#define PANEL_ROW_PAD_X         6    /* horizontal padding inside each row */
#define PANEL_ROW_PAD_Y         3    /* vertical padding inside each row   */
#define PANEL_ROW_GAP           0    /* gap between rows (vertical layout) */
#define PANEL_COL_GAP           2    /* gap between columns (horizontal)   */
#define PANEL_SECTION_GAP       6
#define PANEL_LABEL_GAP         5    /* gap between index label and text   */
#define PANEL_DEFAULT_FONT_SIZE 16

/* ── Configuration ──────────────────────────────────────────────────── */

typedef enum {
    PANEL_LAYOUT_VERTICAL = 0,
    PANEL_LAYOUT_HORIZONTAL,
} PanelLayoutMode;

typedef struct {
    bool   has_bg;         double bg_r, bg_g, bg_b, bg_a;
    bool   has_border;     double border_r, border_g, border_b, border_a;
    bool   has_text;       double text_r, text_g, text_b;
    bool   has_muted;      double muted_r, muted_g, muted_b;
    bool   has_preedit;    double preedit_r, preedit_g, preedit_b;
    bool   has_selection;  double selection_r, selection_g, selection_b, selection_a;
    bool   has_sel_text;   double sel_text_r, sel_text_g, sel_text_b;
} PanelThemeVariant;

typedef struct {
    TypioPanelThemeMode theme_mode;
    PanelLayoutMode              layout_mode;
    int                          font_size;
    bool                         mode_indicator;
    char                         font_desc[128];
    char                         label_font_desc[128];
    char                         aux_font_desc[128];
    char                         font_family[80];
    PanelThemeVariant            light_custom;
    PanelThemeVariant            dark_custom;
} PanelConfig;

/* ── Per-row geometry ───────────────────────────────────────────────── */

typedef struct {
    /* Both layouts borrowed from PanelRenderCtx; do NOT free here. The
     * cached textures are colour-independent (R8 coverage); the text /
     * selection colour is applied at draw time via the tint, so there is
     * no separate selected-colour layout. */
    TypioTextShape *label_layout;
    TypioTextShape *layout;

    int label_w, label_h;
    int text_w,  text_h;

    int   x, y;   /* pixel-aligned row bounds (for damage regions / fills) */
    int   w, h;

    float label_x, label_y;   /* subpixel-accurate paint origins */
    float text_x,  text_y;

    float label_ink_y_offset;
    float text_ink_y_offset;
} PanelRow;

/* ── Geometry snapshot ──────────────────────────────────────────────── */

typedef struct {
    PanelRow rows[PANEL_MAX_ROWS];
    size_t   row_count;

    /* Preedit zone — shows either an IME preedit string or a transient
     * status message (e.g. "[Recording...]"); both use the same palette
     * colour and the same layout slot. May be NULL. */
    TypioTextShape *preedit_layout;
    float        pre_x, pre_y;   /* subpixel-accurate */
    int          pre_w, pre_h;

    /* Mode label — owned by this geometry (may be NULL) */
    TypioTextShape *mode_layout;
    float        mode_x, mode_y; /* subpixel-accurate */
    int          mode_w, mode_h;
    int          mode_divider_y; /* -1 if no divider */

    int panel_w, panel_h;   /* logical-pixel dimensions */
    /* Logical-to-physical pixel ratio. 1.0f on unscaled, integer for
     * legacy integer scale (e.g. 2.0f), arbitrary for fractional scale
     * (e.g. 1.25f). Drives font scaling, ink metrics, and the swapchain
     * physical extent. */
    float scale;

    uint64_t    content_sig;
    uint64_t    palette_sig;
    char        preedit_text[256];
    char        mode_label[128];
    PanelConfig config;

    TypioPanelPalette resolved_palette;
    const TypioPanelPalette *palette;
} PanelGeometry;

/* ── LRU layout cache ───────────────────────────────────────────────── */

typedef struct {
    uint64_t     key;
    char         label[64];
    char         text[512];
    char         label_font_desc[96];
    char         font_desc[96];
    TypioTextShape *label_layout;
    TypioTextShape *layout;
    float        label_pixel_w;
    float        label_pixel_h;
    float        label_pixel_baseline;  /* alphabetic baseline from top */
    float        pixel_w;
    float        pixel_h;
    float        pixel_baseline;        /* alphabetic baseline from top */
    uint32_t     lru_tick;
} PanelLayoutEntry;

/* ── Persistent text engine + LRU cache ───────────────────────────── */

/* When the LRU evicts an entry whose layout/label_layout may still be
 * referenced by a geometry parked in the panel's retire ring (or by an
 * in-flight GPU frame), the panel MUST take ownership of the freeing —
 * an immediate free here is a use-after-free. PanelRenderCtx defers to
 * this callback when set; if it is NULL the layout is freed eagerly. */
typedef struct PanelRenderCtx PanelRenderCtx;
typedef void (*PanelLayoutEvictFn)(void *user, TypioTextShape *layout);

struct PanelRenderCtx {
    TypioTextShaper  *engine;
    PanelLayoutEntry  entries[PANEL_LAYOUT_CACHE_CAP];
    uint32_t          tick;
    PanelLayoutEvictFn evict_cb;   /* NULL ⇒ free eagerly */
    void              *evict_user;
};

/* ── Functions ──────────────────────────────────────────────────────── */

void panel_render_ctx_init(PanelRenderCtx *pc);
void panel_render_ctx_set_evict(PanelRenderCtx *pc,
                                PanelLayoutEvictFn cb, void *user);
void panel_render_ctx_free(PanelRenderCtx *pc);
void panel_render_ctx_invalidate(PanelRenderCtx *pc);

PanelGeometry *panel_geometry_compute(PanelRenderCtx *pc,
                                      const TypioCandidateList *candidates,
                                      const char *preedit_text,
                                      const char *mode_label,
                                      const PanelConfig *config,
                                      const TypioPanelPalette *palette,
                                      float scale);

PanelGeometry *panel_geometry_update_aux(PanelRenderCtx *pc,
                                         const PanelGeometry *base,
                                         const char *preedit_text,
                                         const char *mode_label);

void panel_geometry_free(PanelGeometry *g);

void panel_config_load(PanelConfig *cfg, TypioInstance *instance);

void panel_config_build_palette(const PanelConfig *cfg,
                                 TypioPanelThemeCache *cache,
                                 TypioPanelPalette *out_palette);

/* ── LRU iteration (for atlas compaction) ───────────────────────────── */

/* Number of populated entries in the LRU cache. */
size_t panel_render_ctx_entry_count(const PanelRenderCtx *pc);

/* Get the TypioTextShape pointers of an LRU entry by index.
 * Returns false if the index is out of range or the slot is empty.
 * The returned pointers are borrowed (do not free). */
bool panel_render_ctx_entry_shapes(const PanelRenderCtx *pc, size_t idx,
                                     TypioTextShape **out_text_shape,
                                     TypioTextShape **out_label_shape);

#endif /* TYPIO_WL_PANEL_LAYOUT_H */
