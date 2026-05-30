/**
 * @file panel.c
 * @brief Panel orchestrator — the floating IME UI.
 *
 * Owns the content/layout side of the candidate Panel: the render config,
 * theme, the shaper + LRU layout cache, and the current geometry. It composes
 * a TypioPanelContent into a PanelGeometry and hands it to its TypioPanelSurface
 * (surface.c) to put on screen. The orchestrator never touches Vulkan/Wayland
 * directly — that all lives behind panel_surface_*.
 */

#include "internal.h"
#include "panel.h"
#include "surface.h"
#include "layout.h"
#include "theme.h"
#include "monotonic.h"
#include "typio/runtime/instance.h"
#include "typio/abi/input_context.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Render latency threshold for slow-render debug logging */
#define PANEL_SLOW_RENDER_MS 8

/* ── Main panel struct ──────────────────────────────────────────────── */

struct TypioPanel {
    TypioWlFrontend   *frontend;
    TypioPanelSurface *surface;   /* on-screen presentation backend */

    /* Per-panel text shaper context + LRU layout cache */
    PanelRenderCtx render;

    /* Current computed geometry (owned; NULL if not yet rendered) */
    PanelGeometry *geom;

    /* Render configuration */
    PanelConfig config;
    bool        config_valid;

    /* Theme cache */
    TypioPanelThemeCache theme_cache;

    /* Currently displayed selection index */
    int selected;

    /* Whether the panel is currently visible */
    bool visible;

    /* Transient status text (e.g. "[Recording...]"). Owned; freed on destroy
     * or when status is cleared. */
    char *status_text;
};

/* ── Delta classification ───────────────────────────────────────────── */

typedef enum {
    PANEL_DELTA_NONE,
    PANEL_DELTA_SELECTION,
    PANEL_DELTA_AUX,
    PANEL_DELTA_CONTENT,
    PANEL_DELTA_STYLE,
} PanelDelta;

static PanelDelta classify_delta(const PanelGeometry *geom,
                                  const TypioCandidateList *cands,
                                  const char *preedit,
                                  const char *mode_label,
                                  const PanelConfig *cfg,
                                  uint64_t palette_sig,
                                  float scale,
                                  int new_selected) {
    (void)new_selected;
    if (!geom) return PANEL_DELTA_CONTENT;

    /* Float compare with a slack tolerance: fractional-scale jitter (e.g.
     * 1.2500000 vs 1.2500001 from successive preferred_scale events on the
     * same physical setting) must not force a STYLE rebuild. */
    if (fabsf(geom->scale - scale) > 1e-4f ||
        geom->palette_sig != palette_sig ||
        geom->config.theme_mode != cfg->theme_mode ||
        geom->config.layout_mode != cfg->layout_mode ||
        geom->config.font_size != cfg->font_size ||
        geom->config.mode_indicator != cfg->mode_indicator ||
        strcmp(geom->config.font_desc, cfg->font_desc) != 0 ||
        strcmp(geom->config.aux_font_desc, cfg->aux_font_desc) != 0) {
        return PANEL_DELTA_STYLE;
    }

    if (geom->content_sig != cands->content_signature) {
        /* If count changed, it's a full content change. */
        if (geom->row_count != cands->count) {
            return PANEL_DELTA_CONTENT;
        }

        /* Without per-row signatures in the core API, we cannot prove that only
         * one row changed. Keep the conservative full-content path. */
        return PANEL_DELTA_CONTENT;
    }

    const char *cur_pre = preedit ? preedit : "";
    const char *cur_mode = mode_label ? mode_label : "";
    if (strcmp(geom->preedit_text, cur_pre) != 0 ||
        strcmp(geom->mode_label, cur_mode) != 0) {
        return PANEL_DELTA_AUX;
    }

    return PANEL_DELTA_SELECTION;
}

/* ── Mode label ─────────────────────────────────────────────────────── */

static char *build_mode_label(TypioPanel *panel) {
    const TypioEngineStatus *mode;

    if (!panel || !panel->frontend || !panel->frontend->instance) return nullptr;

    mode = typio_instance_get_last_status(panel->frontend->instance);
    if (!mode || !mode->display_label || !mode->display_label[0]) return nullptr;

    return strdup(mode->display_label);
}

/* ── Config helpers ─────────────────────────────────────────────────── */

static const PanelConfig *get_config(TypioPanel *panel) {
    if (!panel->config_valid) {
        panel_config_load(&panel->config,
                           panel->frontend ? panel->frontend->instance : nullptr);
        panel->config_valid = true;
    }
    return &panel->config;
}

/* PanelRenderCtx evict callback. LRU evictions on the per-keystroke hot path
 * can drop shapes still referenced by the previous frame's geometry — defer
 * their release to the surface's retire ring on the present-epoch cadence. */
static void panel_evict_shape(void *user, TypioTextShape *shape) {
    TypioPanel *panel = (TypioPanel *)user;
    if (panel) panel_surface_retire_shape(panel->surface, shape);
}

/* ── Hide ───────────────────────────────────────────────────────────── */

static void panel_do_hide(TypioPanel *panel) {
    if (!panel || !panel->visible) return;
    panel_surface_hide(panel->surface);
    panel->visible  = false;
    panel->selected = -1;
    panel_surface_retire_geometry(panel->surface, panel->geom);
    panel->geom = nullptr;
}

/* ── Core render ─────────────────────────────────────────────────────── */

static bool panel_render(TypioPanel *panel,
                          const TypioCandidateList *cands,
                          const char *preedit_text,
                          const char *mode_label) {
    const PanelConfig  *cfg;
    TypioPanelPalette   palette;
    uint64_t            pal_sig;
    float               scale;
    int                 new_selected;
    PanelDelta          delta;
    uint64_t            t0, t_classify, t_geometry, t_present, t1;
    const char         *delta_name = "unknown";
    static const TypioCandidateList empty_cands = {};

    if (!panel || !panel->surface) return false;
    if (!cands) {
        cands = &empty_cands;
    }

    panel_surface_reset_retry(panel->surface);

    t0  = typio_wl_monotonic_ms();
    cfg = get_config(panel);

    panel_config_build_palette(cfg, &panel->theme_cache, &palette);
    pal_sig      = typio_panel_palette_hash(&palette);
    scale        = panel_surface_scale(panel->surface);
    new_selected = cands->count > 0 ? cands->selected : -1;

    delta = classify_delta(panel->geom, cands, preedit_text, mode_label,
                            cfg, pal_sig, scale, new_selected);

    if (delta == PANEL_DELTA_SELECTION && new_selected == panel->selected &&
        panel->visible) {
        return true;
    }

    switch (delta) {
    case PANEL_DELTA_NONE:
        return true;

    case PANEL_DELTA_SELECTION:
        delta_name = "selection";
        break;  /* geometry unchanged; re-present with new selection */

    case PANEL_DELTA_AUX: {
        delta_name = "aux";
        PanelGeometry *new_geom = panel_geometry_update_aux(&panel->render,
                                                             panel->geom,
                                                             preedit_text,
                                                             mode_label);
        if (new_geom) {
            panel_surface_retire_geometry(panel->surface, panel->geom);
            panel->geom = new_geom;
        } else {
            delta = PANEL_DELTA_CONTENT;  /* aux size changed; fall through */
        }
        break;
    }

    case PANEL_DELTA_STYLE:
        delta_name = "style";
        panel_render_ctx_invalidate(&panel->render);
        break;

    case PANEL_DELTA_CONTENT:
        delta_name = "content";
        break;
    }

    t_classify = typio_wl_monotonic_ms();

    if (delta == PANEL_DELTA_CONTENT || delta == PANEL_DELTA_STYLE) {
        PanelGeometry *new_geom = panel_geometry_compute(&panel->render,
                                                          cands,
                                                          preedit_text,
                                                          mode_label,
                                                          cfg, &palette, scale);
        if (!new_geom) {
            typio_log_warning("Panel: geometry computation failed");
            return false;
        }
        panel_surface_retire_geometry(panel->surface, panel->geom);
        panel->geom = new_geom;
    }

    t_geometry = typio_wl_monotonic_ms();

    if (!panel->geom) return false;

    PanelPresentResult pres = panel_surface_present(panel->surface, panel->geom, new_selected);
    t_present = typio_wl_monotonic_ms();

    bool ok = (pres == PANEL_PRESENT_OK);
    if (pres == PANEL_PRESENT_OK) {
        panel->selected = new_selected;
        panel->visible  = true;
    } else if (pres == PANEL_PRESENT_RETRY) {
        /* Compositor isn't releasing swapchain images yet (display asleep or
         * surface occluded after a lock/suspend). The retry flag is armed on
         * the surface; selected/visible are left unchanged so the re-present
         * renders this exact state once presentation resumes. */
    } else {
        typio_log_warning("Panel: present failed");
    }

    t1 = typio_wl_monotonic_ms();
    uint64_t total_ms = t1 - t0;
    if (ok && total_ms >= PANEL_SLOW_RENDER_MS) {
        typio_log_debug("Panel slow render: %" PRIu64 "ms "
                        "(classify=%" PRIu64 " geometry=%" PRIu64 " present=%" PRIu64 ") "
                        "delta=%s candidates=%zu selected=%d "
                        "w=%d h=%d scale=%.3f sig=%" PRIu64,
                        total_ms,
                        t_classify - t0,
                        t_geometry - t_classify,
                        t_present - t_geometry,
                        delta_name, cands->count, new_selected,
                        panel->geom ? panel->geom->panel_w : 0,
                        panel->geom ? panel->geom->panel_h : 0,
                        (double)scale, cands->content_signature);
    }

    return ok;
}

void typio_panel_refresh(TypioPanel *panel) {
    if (!panel || !panel->visible || !panel->frontend || !panel->frontend->session) return;
    TypioInputContext *ctx = panel->frontend->session->ctx;
    if (!ctx) return;
    typio_panel_update(panel, ctx);
}

/* ── Public API ─────────────────────────────────────────────────────── */

TypioPanel *typio_panel_create(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->compositor || !frontend->input_method) return nullptr;
    TypioPanel *panel = (TypioPanel *)calloc(1, sizeof(*panel));
    if (!panel) return nullptr;
    panel->frontend = frontend;
    panel->selected = -1;
    panel->surface  = panel_surface_create(frontend, panel);
    if (!panel->surface) { free(panel); return nullptr; }

    panel_render_ctx_init(&panel->render);
    /* Route LRU evictions through the surface's retire ring (use-after-free
     * guard: the just-evicted shape may still be referenced by the frame the
     * GPU is currently rendering). */
    panel_render_ctx_set_evict(&panel->render, panel_evict_shape, panel);
    return panel;
}

void typio_panel_destroy(TypioPanel *panel) {
    if (!panel) return;
    if (panel->visible) panel_surface_hide(panel->surface);
    /* Park the current geometry into the retire ring (or free it immediately if
     * no swapchain was ever built), then drain the ring behind a device fence
     * before freeing the layout cache the geometries borrow shapes from. */
    panel_surface_retire_geometry(panel->surface, panel->geom);
    panel->geom = nullptr;
    panel_surface_drain_retire_fenced(panel->surface);
    panel_render_ctx_free(&panel->render);
    panel_surface_destroy(panel->surface);
    free(panel->status_text);
    free(panel);
}

bool typio_panel_update_content(TypioPanel *panel,
                                const TypioPanelContent *content) {
    if (!panel || !content) return false;

    /* Update persistent status only when the caller explicitly sets it.
     * InputContext-driven updates leave status.message == nullptr so they do
     * not clobber a voice indicator that may still be visible. */
    if (content->status.active) {
        free(panel->status_text);
        panel->status_text = content->status.message ? strdup(content->status.message) : nullptr;
    } else if (content->status.message != nullptr) {
        /* Explicit clear request: hide_status passes active=false with an
         * empty-string message to distinguish "clear" from "don't care". */
        free(panel->status_text);
        panel->status_text = nullptr;
    }

    const TypioCandidateList *cands = content->input.candidates;
    const char *preedit = nullptr;

    /* No candidates and no persistent status → hide. */
    if ((!cands || cands->count == 0) && (!panel->status_text || !panel->status_text[0])) {
        panel_do_hide(panel);
        return true;
    }

    /* When the IME has no candidates, surface the persistent voice-status text
     * (if any) through the preedit slot. Voice "[Recording...]" and an IME
     * preedit string share the same palette colour, the same layout slot, and
     * the same delta-classification path — no second code path. */
    if (!cands || cands->count == 0) {
        preedit = panel->status_text;
    }

    char *mode_label = build_mode_label(panel);
    bool ok = panel_render(panel, cands, preedit, mode_label);
    free(mode_label);
    return ok;
}

/* Convenience wrapper: build a content descriptor from the input context plus
 * the host-side candidate snapshot. Candidates are no longer exposed via the
 * libtypio input-context surface; the composition callback in input_method.c
 * maintains a deep copy on the session. */
bool typio_panel_update(TypioPanel *panel, TypioInputContext *ctx) {
    if (!panel) return false;

    TypioPanelContent content;
    typio_panel_content_init(&content);
    if (ctx) {
        if (panel->frontend && panel->frontend->session) {
            content.input.candidates = &panel->frontend->session->candidate_snapshot;
        }
        content.input.preedit = typio_input_context_get_preedit(ctx);
    }
    return typio_panel_update_content(panel, &content);
}

void typio_panel_hide(TypioPanel *panel) {
    panel_do_hide(panel);
}

bool typio_panel_is_available(TypioPanel *panel) {
    return panel && panel_surface_is_available(panel->surface);
}

bool typio_panel_present_retry_pending(TypioPanel *panel) {
    return panel && panel_surface_present_retry_pending(panel->surface);
}

void typio_panel_invalidate_config(TypioPanel *panel) {
    if (!panel) return;
    panel->config_valid = false;
    memset(&panel->theme_cache, 0, sizeof(panel->theme_cache));
    /* Invalidating the LRU frees its shapes; those may be referenced by an
     * in-flight frame, so drain the retire ring behind a device fence first.
     * Config changes are user-driven and rare, so the device-idle wait is
     * acceptable; the per-keystroke path goes through the retire ring. */
    panel_surface_drain_retire_fenced(panel->surface);
    panel_render_ctx_invalidate(&panel->render);
    panel_geometry_free(panel->geom);
    panel->geom = nullptr;
}

void typio_panel_handle_output_change(TypioPanel *panel, struct wl_output *output) {
    if (!panel) return;
    panel_surface_handle_output_change(panel->surface, output);
}

/* ── Status indicator (unified panel backend) ───────────────────────── */

bool typio_panel_show_status(TypioPanel *panel, const char *text) {
    if (!panel) return false;

    TypioPanelContent content;
    typio_panel_content_init(&content);
    if (text && text[0]) {
        content.status.active  = true;
        content.status.message = text;
    }
    return typio_panel_update_content(panel, &content);
}

void typio_panel_hide_status(TypioPanel *panel) {
    if (!panel) return;

    TypioPanelContent content;
    typio_panel_content_init(&content);
    content.status.active  = false;
    content.status.message = "";  /* empty string signals explicit clear */
    typio_panel_update_content(panel, &content);
}
