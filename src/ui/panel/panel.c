/**
 * @file panel.c
 * @brief Wayland input-panel coordinator.
 *
 * The panel presents a flux (Vulkan) swapchain directly onto its
 * zwp_input_popup_surface_v2 wl_surface. Each candidate update records the
 * panel into a flux canvas and presents one frame; the swapchain owns frame
 * pacing and buffering, so there is no SHM buffer pool or manual frame
 * throttling.
 */

#define VK_USE_PLATFORM_WAYLAND_KHR
#include <flux/flux.h>
#include <flux/vulkan.h>

#include "internal.h"
#include "panel.h"
#include "layout.h"
#include "paint.h"
#include "theme.h"
#include "text_shaper.h"
#include "device.h"
#include "monotonic.h"
#include "preedit.h"
#include "typio/runtime/instance.h"
#include "typio/abi/input_context.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Render latency threshold for slow-render debug logging */
#define PANEL_SLOW_RENDER_MS 8

/* Bounded acquire/present timeout. The panel presents synchronously on the
 * single-threaded IME event loop, so a compositor that has stopped releasing
 * swapchain images (display asleep or surface occluded after a lock/suspend)
 * must never block the loop in vkAcquireNextImageKHR. ~2 vblanks @60Hz; the
 * healthy on-demand path acquires instantly and never approaches this. */
#define PANEL_PRESENT_TIMEOUT_NS     (32ull * 1000ull * 1000ull)
/* Recreate the swapchain after this many consecutive stalls. flux_surface_resize
 * rebuilds the chain and discards the per-frame semaphores left dangling by the
 * stalled acquires, so presentation resumes cleanly once the session is back. */
#define PANEL_PRESENT_RECOVER_STREAK 2

/* Hard cap on per-epoch retire-slot growth. During a long present stall the
 * epoch does not advance, so geometries/layouts retired across many CONTENT
 * deltas accumulate in the current slot. The cap converts pathological growth
 * into a bounded vkDeviceWaitIdle + inline drain, trading a one-off ms-scale
 * stall for predictable memory use. Picked well above the realistic worst
 * case (one geometry + a handful of layout evictions per delta). */
#define PANEL_RETIRE_SLOT_MAX 256

/* ── Output tracking ────────────────────────────────────────────────── */

typedef struct PanelOutputRef {
    struct wl_output    *output;
    struct PanelOutputRef *next;
} PanelOutputRef;

/* ── Frame-retire queue ─────────────────────────────────────────────── */

/* Historically, geometry and layouts owned per-run glyph flux_images, so a
 * geometry still referenced by an in-flight frame could not be freed until the
 * GPU had consumed that frame. This ring deferred the free by the present
 * epoch, avoiding a vkDeviceWaitIdle on the IME loop per delta.
 *
 * Since ADR-0012 glyph pixels live in the shared, PERSISTENT glyph atlas;
 * geometry and layouts own NO GPU resource, and their pixels are baked into the
 * frame's transient vertex buffer at record time — so the GPU never references
 * them after panel_record returns, and an immediate CPU free would be safe.
 * The deferral is therefore no longer required for correctness; it is retained
 * (a bounded, harmless CPU-free delay) and slated for removal once the atlas
 * change is runtime-verified, to avoid stacking two unverified hot-path changes
 * (see ADR-0012 "Residual legacy"). Depth = 3 = flux's frames_in_flight (2)
 * plus the just-presented frame. */
#define PANEL_RETIRE_DEPTH 3

typedef enum {
    PANEL_RETIRE_GEOMETRY = 0,
    PANEL_RETIRE_LAYOUT,
} PanelRetireKind;

typedef struct {
    PanelRetireKind kind;
    void           *ptr;
} PanelRetireItem;

typedef struct PanelRetireSlot {
    PanelRetireItem *items;
    size_t           count;
    size_t           cap;
} PanelRetireSlot;

/* ── Main panel struct ──────────────────────────────────────────────── */

struct TypioPanel {
    TypioWlFrontend *frontend;

    /* Wayland surface objects */
    struct wl_surface                  *surface;
    struct zwp_input_popup_surface_v2  *popup_surface;

    /* Optional HiDPI helpers. Both are nullptr when the compositor lacks
     * wp_fractional_scale_v1 / wp_viewporter; in that case we fall back
     * to the integer wl_surface buffer_scale path. */
    struct wp_viewport             *viewport;
    struct wp_fractional_scale_v1  *fractional_scale;

    /* flux GPU present pipeline (Vulkan swapchain on the panel wl_surface) */
    VkSurfaceKHR  vk_surface;
    flux_surface *fx_surface;
    flux_canvas  *fx_canvas;
    flux_arena    fx_arena;
    bool          fx_ready;
    int           surf_w, surf_h;   /* swapchain BUFFER extent, physical px.
                                     * With a viewport this is grow-only and
                                     * quantised (PANEL_SURFACE_QUANTUM): the
                                     * buffer is the high-water mark and the
                                     * exact content rect is cropped out via
                                     * wp_viewport_set_source, so per-candidate
                                     * width changes no longer rebuild the
                                     * swapchain. Without a viewport the buffer
                                     * maps 1:1 to the surface and tracks the
                                     * content size exactly. */

    /* Present stall recovery (lock/suspend). When the compositor stops
     * releasing swapchain images, the bounded acquire times out: count the
     * consecutive stalls to drive swapchain recreation, and flag a retry so
     * the event loop re-presents once presentation resumes. */
    int  present_timeout_streak;
    bool present_retry;

    /* Frame-retire ring. `present_epoch` advances on every successful present;
     * `retire[epoch % depth]` holds geometries/layouts that were live during
     * that epoch, freed when the slot is reused. These no longer own GPU
     * resources (glyphs live in the persistent atlas, ADR-0012), so this is now
     * a conservative CPU-free deferral — see PANEL_RETIRE_DEPTH. */
    PanelRetireSlot retire[PANEL_RETIRE_DEPTH];
    uint64_t        present_epoch;

    /* Per-panel text engine context + LRU layout cache */
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

    /* Whether the panel surface is currently visible */
    bool visible;

    /* Transient status text (e.g. "[Recording...]"). Owned; freed on destroy
     * or when status is cleared.  Phase 1 of unified panel backend. */
    char *status_text;

    /* Output tracking (for scale resolution; fallback path) */
    PanelOutputRef *entered_outputs;

    /* Scale signals — resolved in priority order:
     *   fractional_scale_120 (set by wp_fractional_scale_v1.preferred_scale, 24.8 fixed in 120ths)
     *   preferred_buffer_scale (set by wl_surface v6, integer)
     *   entered_outputs->scale (set by wl_surface.enter, integer)
     *   max(frontend->outputs[].scale) (initial guess before any signal)
     */
    uint32_t fractional_scale_120;       /* 0 when no fractional signal yet */
    int      preferred_buffer_scale;     /* 0 when no v6 hint yet */

    /* Text-input cursor rectangle (informational; set by compositor) */
    int text_input_x, text_input_y, text_input_w, text_input_h;
};

/* ── Retire helpers (defined here so panel methods below can call them) */

static void retire_item_free(PanelRetireItem *it) {
    if (!it || !it->ptr) return;
    switch (it->kind) {
    case PANEL_RETIRE_GEOMETRY:
        panel_geometry_free((PanelGeometry *)it->ptr);
        break;
    case PANEL_RETIRE_LAYOUT:
        typio_text_shape_free((TypioTextShape *)it->ptr);
        break;
    }
    it->ptr = nullptr;
}

static void retire_slot_drain(PanelRetireSlot *slot);

static void retire_slot_push(PanelRetireSlot *slot,
                              PanelRetireKind kind, void *ptr) {
    if (!ptr) return;

    /* Cap reached: fence the GPU and drain everything we've parked so far
     * in this slot inline. This converts the worst-case "RETRY-storm while
     * the user keeps paging candidates" into a bounded one-off stall
     * instead of unbounded memory growth. */
    if (slot->count >= PANEL_RETIRE_SLOT_MAX) {
        flux_device *dev = typio_render_device_get();
        if (dev) flux_device_wait_idle(dev);
        retire_slot_drain(slot);
    }

    if (slot->count == slot->cap) {
        size_t ncap = slot->cap ? slot->cap * 2 : 4;
        if (ncap > PANEL_RETIRE_SLOT_MAX) ncap = PANEL_RETIRE_SLOT_MAX;
        PanelRetireItem *n = (PanelRetireItem *)realloc(slot->items, ncap * sizeof(*n));
        if (!n) {
            /* Realloc failure: fall back to a device-wide fence so the
             * release is still safe. */
            flux_device *dev = typio_render_device_get();
            if (dev) flux_device_wait_idle(dev);
            PanelRetireItem it = { kind, ptr };
            retire_item_free(&it);
            return;
        }
        slot->items = n;
        slot->cap   = ncap;
    }
    slot->items[slot->count].kind = kind;
    slot->items[slot->count].ptr  = ptr;
    slot->count++;
}

static void retire_slot_drain(PanelRetireSlot *slot) {
    for (size_t i = 0; i < slot->count; ++i) {
        retire_item_free(&slot->items[i]);
    }
    slot->count = 0;
}

static void retire_slot_free(PanelRetireSlot *slot) {
    retire_slot_drain(slot);
    free(slot->items);
    slot->items = nullptr;
    slot->cap = 0;
}

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

/* ── Output helpers ─────────────────────────────────────────────────── */

static const TypioWlOutput *find_frontend_output(const TypioPanel *panel,
                                                   struct wl_output *output) {
    for (TypioWlOutput *o = panel->frontend ? panel->frontend->outputs : nullptr;
         o; o = o->next) {
        if (o->output == output) return o;
    }
    return nullptr;
}

static bool tracks_output(const TypioPanel *panel,
                           struct wl_output *output) {
    for (PanelOutputRef *r = panel->entered_outputs; r; r = r->next) {
        if (r->output == output) return true;
    }
    return false;
}

/* Resolve the logical-to-physical scale ratio for the panel.
 *
 * Priority (best signal first):
 *   1. wp_fractional_scale_v1.preferred_scale          (sub-integer, sent before commit)
 *   2. wl_surface v6 preferred_buffer_scale            (integer, sent before commit)
 *   3. wl_surface.enter ⇒ tracked output's wl_output.scale (legacy)
 *   4. max(frontend->outputs[].scale) as an initial guess so the very first
 *      present on a multi-output session doesn't render at 1× and trigger a
 *      reupload+recommit when enter arrives.
 *   5. 1.0f.
 */
static float render_scale(const TypioPanel *panel) {
    if (panel->fractional_scale_120 > 0) {
        return (float)panel->fractional_scale_120 / 120.0f;
    }
    if (panel->preferred_buffer_scale > 0) {
        return (float)panel->preferred_buffer_scale;
    }

    int best = 0;
    for (PanelOutputRef *r = panel->entered_outputs; r; r = r->next) {
        const TypioWlOutput *o = find_frontend_output(panel, r->output);
        if (o && o->scale > best) best = o->scale;
    }
    if (best > 0) return (float)best;

    /* Initial guess: the highest-DPI output the frontend has seen. */
    if (panel->frontend) {
        for (TypioWlOutput *o = panel->frontend->outputs; o; o = o->next) {
            if (o->scale > best) best = o->scale;
        }
    }
    return best > 0 ? (float)best : 1.0f;
}

static void track_output(TypioPanel *panel, struct wl_output *output);
static void untrack_output(TypioPanel *panel, struct wl_output *output);
static void refresh_visible(TypioPanel *panel);

/* ── Mode label ─────────────────────────────────────────────────────── */

static char *build_mode_label(TypioPanel *panel) {
    const TypioEngineMode *mode;

    if (!panel || !panel->frontend || !panel->frontend->instance) return nullptr;

    mode = typio_instance_get_last_mode(panel->frontend->instance);
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

/* ── Geometry retire (deferred GPU resource release) ─────────────────── */

/* Park `g` into the current present epoch's slot. The flux_image resources
 * owned by `g` will be freed when this slot is reused by a later present,
 * after the GPU has finished referencing them. Safe to call when `g` is
 * NULL.
 *
 * If the swapchain has never been built (fx_ready == false), nothing on
 * the GPU references `g`, so it can be freed immediately. */
static void retire_geometry(TypioPanel *panel, PanelGeometry *g) {
    if (!g) return;
    if (!panel || !panel->fx_ready) {
        panel_geometry_free(g);
        return;
    }
    PanelRetireSlot *slot = &panel->retire[panel->present_epoch % PANEL_RETIRE_DEPTH];
    retire_slot_push(slot, PANEL_RETIRE_GEOMETRY, g);
}

/* PanelRenderCtx evict callback. LRU evictions on the per-keystroke hot
 * path can drop layouts that are still referenced by the previous frame's
 * geometry — defer their release to the retire ring on the same epoch
 * cadence. */
static void panel_retire_layout(void *user, TypioTextShape *layout) {
    TypioPanel *panel = (TypioPanel *)user;
    if (!layout) return;
    if (!panel || !panel->fx_ready) {
        typio_text_shape_free(layout);
        return;
    }
    PanelRetireSlot *slot = &panel->retire[panel->present_epoch % PANEL_RETIRE_DEPTH];
    retire_slot_push(slot, PANEL_RETIRE_LAYOUT, layout);
}

/* ── flux swapchain lifecycle ───────────────────────────────────────── */

static inline uint8_t panel_u8(double v) {
    if (v <= 0.0) return 0;
    if (v >= 1.0) return 255;
    return (uint8_t)(v * 255.0 + 0.5);
}

static flux_color panel_bg_color(const TypioPanelPalette *p) {
    return flux_color_rgba_premul(panel_u8(p->bg_r), panel_u8(p->bg_g),
                                  panel_u8(p->bg_b), panel_u8(p->bg_a));
}

static void fx_teardown(TypioPanel *panel) {
    if (!panel) return;

    flux_device *dev = (panel->fx_surface || panel->vk_surface) ? typio_render_device_get() : nullptr;
    if (dev && panel->fx_ready) flux_device_wait_idle(dev);

    if (panel->fx_canvas) {
        flux_canvas_destroy(panel->fx_canvas);
        panel->fx_canvas = nullptr;
    }
    if (panel->fx_ready) {
        flux_arena_destroy(&panel->fx_arena);
    }
    if (panel->fx_surface) {
        flux_surface_release(panel->fx_surface);
        panel->fx_surface = nullptr;
    }
    if (panel->vk_surface != VK_NULL_HANDLE && dev) {
        vkDestroySurfaceKHR(flux_device_vk_instance(dev), panel->vk_surface, nullptr);
    }
    panel->vk_surface = VK_NULL_HANDLE;
    panel->fx_ready   = false;
    panel->surf_w = panel->surf_h = 0;
}

/* Swapchain buffer allocation quantum (physical px). Every candidate page
 * changes the panel's pixel width; rounding the buffer up to this quantum and
 * growing it only when content exceeds it means the swapchain is rebuilt a
 * handful of times during warm-up and then never again. flux_surface_resize
 * does a vkDeviceWaitIdle plus three blocking compositor roundtrips
 * (GetSurfaceCapabilities / Formats / PresentModes) on the single-threaded IME
 * loop, so eliminating per-page rebuilds is what removes the candidate-switch
 * lag (ADR-0013). Only meaningful with a viewport, which crops the oversized
 * buffer back to the exact content rect via wp_viewport_set_source. */
#define PANEL_SURFACE_QUANTUM 64

static inline int panel_quantize_up(int v) {
    if (v < 1) v = 1;
    return ((v + PANEL_SURFACE_QUANTUM - 1) / PANEL_SURFACE_QUANTUM) * PANEL_SURFACE_QUANTUM;
}

/* Create / resize the swapchain to cover (w, h) physical pixels. With a
 * viewport the buffer is grow-only and quantised (the content is cropped to
 * size at present time); without one it tracks (w, h) exactly. */
static bool ensure_fx_surface(TypioPanel *panel, int w, int h) {
    if (!panel || !panel->frontend || !panel->surface || w <= 0 || h <= 0) return false;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    if (panel->vk_surface == VK_NULL_HANDLE) {
        VkWaylandSurfaceCreateInfoKHR ci = {
            .sType   = VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
            .pNext   = nullptr,
            .flags   = 0,
            .display = panel->frontend->display,
            .surface = panel->surface,
        };
        if (vkCreateWaylandSurfaceKHR(flux_device_vk_instance(dev), &ci, nullptr,
                                      &panel->vk_surface) != VK_SUCCESS) {
            panel->vk_surface = VK_NULL_HANDLE;
            return false;
        }
    }

    /* Buffer extent to allocate. With a viewport, round up to the quantum so
     * the buffer outlives small per-page width changes; without one it must
     * equal the content exactly (the buffer maps 1:1 to the surface). */
    int bw = w, bh = h;
    if (panel->viewport) {
        bw = panel_quantize_up(w);
        bh = panel_quantize_up(h);
    }

    if (!panel->fx_surface) {
        flux_surface_desc sd = {};
        sd.type           = FLUX_TYPE_SURFACE_DESC;
        sd.vk_surface_khr = panel->vk_surface;
        sd.width          = (uint32_t)bw;
        sd.height         = (uint32_t)bh;
        /* Non-blocking present (MAILBOX/IMMEDIATE, falls back to FIFO if the
         * driver offers neither). The panel presents synchronously on the
         * single-threaded IME event loop, so a vsync/FIFO present blocks
         * vkQueuePresentKHR until the compositor releases a swapchain buffer
         * (≈ a refresh interval, and far longer when the compositor throttles
         * frame callbacks for this surface) — which stalls key handling and
         * makes candidate switching lag. Measured with gdb user-space stack
         * sampling: the main thread sat in anv_QueuePresentKHR ->
         * wsi_wl_swapchain_queue_present for ~86% of wall-clock while paging
         * candidates. Tearing on a small candidate panel is irrelevant. */
        sd.vsync          = false;
        if (flux_surface_create(dev, &sd, &panel->fx_surface) != FLUX_OK) {
            panel->fx_surface = nullptr;
            return false;
        }

        flux_canvas_desc cd = {};
        cd.type    = FLUX_TYPE_CANVAS_DESC;
        cd.surface = panel->fx_surface;
        if (flux_canvas_create(&cd, &panel->fx_canvas) != FLUX_OK) {
            panel->fx_canvas = nullptr;
            flux_surface_release(panel->fx_surface);
            panel->fx_surface = nullptr;
            return false;
        }

        if (flux_arena_init(&panel->fx_arena, 256 * 1024, nullptr) != FLUX_OK) {
            flux_canvas_destroy(panel->fx_canvas);
            panel->fx_canvas = nullptr;
            flux_surface_release(panel->fx_surface);
            panel->fx_surface = nullptr;
            return false;
        }

        panel->surf_w   = bw;
        panel->surf_h   = bh;
        panel->fx_ready = true;
    } else if (panel->viewport) {
        /* Grow-only: rebuild only when content exceeds the current buffer.
         * Shrinks and sub-quantum widenings reuse the existing swapchain and
         * are cropped to size by wp_viewport_set_source at present time. */
        int nw = panel->surf_w, nh = panel->surf_h;
        if (w > nw) nw = panel_quantize_up(w);
        if (h > nh) nh = panel_quantize_up(h);
        if ((nw != panel->surf_w || nh != panel->surf_h) &&
            flux_surface_resize(panel->fx_surface, (uint32_t)nw, (uint32_t)nh) == FLUX_OK) {
            panel->surf_w = nw;
            panel->surf_h = nh;
        }
    } else if (panel->surf_w != w || panel->surf_h != h) {
        /* No viewport: buffer maps 1:1, must match content exactly. */
        if (flux_surface_resize(panel->fx_surface, (uint32_t)w, (uint32_t)h) == FLUX_OK) {
            panel->surf_w = w;
            panel->surf_h = h;
        }
    }

    return panel->fx_ready;
}

typedef enum {
    PANEL_PRESENT_OK,     /* frame presented */
    PANEL_PRESENT_RETRY,  /* transient stall; skip this frame, re-present later */
    PANEL_PRESENT_FAIL,   /* hard failure */
} PanelPresentResult;

/* Record + present one frame of the panel.
 *
 * The acquire/fence wait is bounded (PANEL_PRESENT_TIMEOUT_NS) so a compositor
 * that has stopped releasing swapchain images — e.g. while the display is
 * asleep or the surface is occluded behind a lock screen — cannot block the
 * single-threaded IME event loop. On a stall we return PANEL_PRESENT_RETRY and,
 * after a few consecutive stalls, recreate the swapchain (which also clears the
 * per-frame semaphores left dangling by the stalled acquires) so presentation
 * resumes cleanly once the session is back. */
static PanelPresentResult panel_present(TypioPanel *panel,
                                        const PanelGeometry *geom, int selected) {
    if (!panel->fx_ready || !geom || !geom->palette) return PANEL_PRESENT_FAIL;

    flux_frame_begin_desc bd = {};
    bd.type       = FLUX_TYPE_FRAME_BEGIN_DESC;
    bd.timeout_ns = PANEL_PRESENT_TIMEOUT_NS;

    flux_frame *frame = nullptr;
    flux_result r = flux_surface_begin_frame(panel->fx_surface, &bd, &frame);
    if (r == FLUX_ERROR_SURFACE_LOST) {
        (void)flux_surface_resize(panel->fx_surface,
                                  (uint32_t)panel->surf_w, (uint32_t)panel->surf_h);
        panel->present_timeout_streak = 0;
        r = flux_surface_begin_frame(panel->fx_surface, &bd, &frame);
    }
    if (r == FLUX_ERROR_TIMEOUT) {
        if (++panel->present_timeout_streak >= PANEL_PRESENT_RECOVER_STREAK) {
            /* Stalled acquires leave stale per-frame semaphores; resizing the
             * surface to its current extent rebuilds the swapchain and resets
             * them. vkDeviceWaitIdle inside resize waits on GPU work (which
             * completes regardless of presentation), so it does not block. */
            (void)flux_surface_resize(panel->fx_surface,
                                      (uint32_t)panel->surf_w, (uint32_t)panel->surf_h);
            panel->present_timeout_streak = 0;
        }
        return PANEL_PRESENT_RETRY;
    }
    if (r != FLUX_OK) return PANEL_PRESENT_FAIL;

    panel->present_timeout_streak = 0;

    flux_color clear = panel_bg_color(geom->palette);
    if (flux_canvas_begin(panel->fx_canvas, frame, &clear) != FLUX_OK) return PANEL_PRESENT_FAIL;

    PanelPaintTarget target = { panel->fx_canvas, &panel->fx_arena };
    panel_record(&target, geom, selected);

    flux_arena_reset(&panel->fx_arena);
    flux_canvas_end(panel->fx_canvas);

    if (flux_frame_submit(frame) != FLUX_OK) return PANEL_PRESENT_FAIL;

    r = flux_frame_present(frame);
    if (r == FLUX_ERROR_SURFACE_LOST) {
        (void)flux_surface_resize(panel->fx_surface,
                                  (uint32_t)panel->surf_w, (uint32_t)panel->surf_h);
        return PANEL_PRESENT_RETRY;  /* next update repaints at the new extent */
    }
    return r == FLUX_OK ? PANEL_PRESENT_OK : PANEL_PRESENT_FAIL;
}

/* ── Surface hide ───────────────────────────────────────────────────── */

static void hide_surface(TypioPanel *panel) {
    if (!panel || !panel->surface || !panel->visible) return;

    /* Unmap by committing a null buffer. The flux swapchain stays alive so a
     * later show only needs a present, not a swapchain rebuild. */
    wl_surface_attach(panel->surface, nullptr, 0, 0);
    wl_surface_commit(panel->surface);

    panel->visible       = false;
    panel->selected      = -1;
    panel->present_retry = false;

    retire_geometry(panel, panel->geom);
    panel->geom = nullptr;
}

/* ── Core render ─────────────────────────────────────────────────────── */

static bool panel_render(TypioPanel *panel,
                          const TypioCandidateList *cands,
                          const char *preedit_text,
                          const char *mode_label) {
    const PanelConfig          *cfg;
    TypioPanelPalette   palette;
    uint64_t                     pal_sig;
    float                        scale;
    int                          new_selected;
    PanelDelta                   delta;
    uint64_t                     t0, t1;
    const char                  *delta_name = "unknown";
    static const TypioCandidateList empty_cands = {};

    if (!panel || !panel->surface) return false;
    if (!cands) {
        cands = &empty_cands;
    }

    panel->present_retry = false;

    t0  = typio_wl_monotonic_ms();
    cfg = get_config(panel);

    panel_config_build_palette(cfg, &panel->theme_cache, &palette);
    pal_sig      = typio_panel_palette_hash(&palette);
    scale        = render_scale(panel);
    new_selected = cands->count > 0 ? cands->selected : -1;

    delta = classify_delta(panel->geom, cands, preedit_text, mode_label,
                            cfg, pal_sig, scale, new_selected);

    if (delta == PANEL_DELTA_SELECTION && new_selected == panel->selected &&
        panel->visible) {
        return true;
    }

    /* Geometry recomputation may evict LRU layout entries and free the old
     * geometry. These are now pure CPU structures (glyph pixels live in the
     * persistent atlas, ADR-0012); the deferral to the frame-retire ring is no
     * longer required for GPU safety but is kept conservatively. The
     * selection-only hot path frees nothing and stays out of the ring. */
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
            retire_geometry(panel, panel->geom);
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
        retire_geometry(panel, panel->geom);
        panel->geom = new_geom;
    }

    if (!panel->geom) return false;

    float s = panel->geom->scale > 0.0f ? panel->geom->scale : 1.0f;
    int pw = (int)ceilf((float)panel->geom->panel_w * s);
    int ph = (int)ceilf((float)panel->geom->panel_h * s);
    if (pw < 1) pw = 1;
    if (ph < 1) ph = 1;
    if (!ensure_fx_surface(panel, pw, ph)) {
        typio_log_warning("Panel: flux surface unavailable");
        return false;
    }

    /* Tell the compositor how to interpret the buffer. With wp_viewporter
     * + wp_fractional_scale_v1 we publish the buffer at scale=1 and map it
     * to the logical rect via the viewport — that covers sub-integer
     * scales correctly. Without those globals we fall back to the legacy
     * integer wl_surface buffer_scale path, rounding up to the nearest
     * integer (a small over-sample, but always crisp). */
    if (panel->viewport) {
        wl_surface_set_buffer_scale(panel->surface, 1);
        /* The swapchain buffer (surf_w × surf_h) is grow-only and usually
         * larger than this frame's content. Crop the source to the exact
         * content rect — rendered at the buffer's top-left — so the oversized
         * margin is never shown, then scale that rect to the logical size. */
        wp_viewport_set_source(panel->viewport,
                               wl_fixed_from_int(0), wl_fixed_from_int(0),
                               wl_fixed_from_int(pw), wl_fixed_from_int(ph));
        wp_viewport_set_destination(panel->viewport,
                                    panel->geom->panel_w,
                                    panel->geom->panel_h);
    } else {
        int isc = (int)ceilf(s);
        if (isc < 1) isc = 1;
        wl_surface_set_buffer_scale(panel->surface, isc);
    }

    PanelPresentResult pres = panel_present(panel, panel->geom, new_selected);
    bool ok = (pres == PANEL_PRESENT_OK);
    if (pres == PANEL_PRESENT_OK) {
        panel->selected = new_selected;
        panel->visible  = true;
        /* Advance the retire ring: anything pushed during the previous
         * sweep at (epoch - PANEL_RETIRE_DEPTH + 1) is now safe to free. */
        panel->present_epoch++;
        retire_slot_drain(&panel->retire[panel->present_epoch % PANEL_RETIRE_DEPTH]);
    } else if (pres == PANEL_PRESENT_RETRY) {
        /* Compositor isn't releasing swapchain images yet (display asleep or
         * surface occluded after a lock/suspend). Skip this frame so the IME
         * event loop stays responsive, and ask it to re-present so the visible
         * highlight catches up once presentation resumes. selected/visible are
         * left unchanged so the retry re-renders this exact state. */
        panel->present_retry = true;
    } else {
        typio_log_warning("Panel: present failed");
    }

    t1 = typio_wl_monotonic_ms();
    if (ok && (t1 - t0) >= PANEL_SLOW_RENDER_MS) {
        typio_log_debug("Panel slow render: %" PRIu64 "ms delta=%s candidates=%zu "
                        "selected=%d w=%d h=%d scale=%.3f sig=%" PRIu64,
                        t1 - t0, delta_name, cands->count, new_selected,
                        panel->geom ? panel->geom->panel_w : 0,
                        panel->geom ? panel->geom->panel_h : 0,
                        (double)scale, cands->content_signature);
    }

    return ok;
}

/* ── Surface / output event handlers ───────────────────────────────── */

static void on_text_input_rectangle(void *data,
                                     [[maybe_unused]] struct zwp_input_popup_surface_v2 *s,
                                     int32_t x, int32_t y, int32_t w, int32_t h) {
    TypioPanel *panel = (TypioPanel *)data;
    panel->text_input_x = x;
    panel->text_input_y = y;
    panel->text_input_w = w;
    panel->text_input_h = h;
}

static const struct zwp_input_popup_surface_v2_listener popup_surface_listener = {
    .text_input_rectangle = on_text_input_rectangle,
};

static void on_surface_enter(void *data,
                               [[maybe_unused]] struct wl_surface *surface,
                               struct wl_output *output) {
    track_output((TypioPanel *)data, output);
}

static void on_surface_leave(void *data,
                               [[maybe_unused]] struct wl_surface *surface,
                               struct wl_output *output) {
    untrack_output((TypioPanel *)data, output);
}

/* wl_surface v6: integer scale hint emitted before the first commit. We
 * prefer it over the legacy enter-based output scan. wp_fractional_scale_v1
 * still wins above this when both are present. */
static void on_surface_preferred_buffer_scale(void *data,
                                              [[maybe_unused]] struct wl_surface *surface,
                                              int32_t factor) {
    TypioPanel *panel = (TypioPanel *)data;
    if (!panel || factor <= 0) return;
    if (panel->preferred_buffer_scale == factor) return;
    panel->preferred_buffer_scale = factor;
    refresh_visible(panel);
}

static void on_surface_preferred_buffer_transform(
    [[maybe_unused]] void *data,
    [[maybe_unused]] struct wl_surface *surface,
    [[maybe_unused]] uint32_t transform) {
    /* Panels are axis-aligned; no rotation handling needed. */
}

static const struct wl_surface_listener wl_surface_listener = {
    .enter = on_surface_enter,
    .leave = on_surface_leave,
    .preferred_buffer_scale = on_surface_preferred_buffer_scale,
    .preferred_buffer_transform = on_surface_preferred_buffer_transform,
};

/* wp_fractional_scale_v1: 24.8 fixed-point logical-to-physical ratio in
 * 120ths (so 120 = 1.0×, 150 = 1.25×, 180 = 1.5×). When this signal is
 * present we use it as the source of truth, sample the wl_surface buffer
 * at scale=1, and let wp_viewport handle the logical sizing. */
static void on_fractional_preferred_scale(void *data,
                                          [[maybe_unused]] struct wp_fractional_scale_v1 *scale,
                                          uint32_t scale_120) {
    TypioPanel *panel = (TypioPanel *)data;
    if (!panel || scale_120 == 0) return;
    if (panel->fractional_scale_120 == scale_120) return;
    panel->fractional_scale_120 = scale_120;
    refresh_visible(panel);
}

static const struct wp_fractional_scale_v1_listener fractional_scale_listener = {
    .preferred_scale = on_fractional_preferred_scale,
};

/* ── Output tracking (refresh panel when scale changes) ─────────────── */

static void refresh_visible(TypioPanel *panel) {
    if (!panel || !panel->visible || !panel->frontend || !panel->frontend->session) return;
    TypioInputContext *ctx = panel->frontend->session->ctx;
    if (!ctx) return;
    typio_panel_update(panel, ctx);
}

static void track_output(TypioPanel *panel, struct wl_output *output) {
    if (!panel || !output || tracks_output(panel, output)) return;
    PanelOutputRef *r = (PanelOutputRef *)calloc(1, sizeof(*r));
    if (!r) return;
    r->output = output;
    r->next = panel->entered_outputs;
    panel->entered_outputs = r;
    refresh_visible(panel);
}

static void untrack_output(TypioPanel *panel, struct wl_output *output) {
    PanelOutputRef **link = &panel->entered_outputs;
    while (*link) {
        PanelOutputRef *r = *link;
        if (r->output == output) {
            *link = r->next;
            free(r);
            refresh_visible(panel);
            return;
        }
        link = &r->next;
    }
}

static void clear_outputs(TypioPanel *panel) {
    while (panel && panel->entered_outputs) {
        PanelOutputRef *r = panel->entered_outputs;
        panel->entered_outputs = r->next;
        free(r);
    }
}

/* ── Public API ─────────────────────────────────────────────────────── */

TypioPanel *typio_panel_create(TypioWlFrontend *frontend) {
    if (!frontend || !frontend->compositor || !frontend->input_method) return nullptr;
    TypioPanel *panel = (TypioPanel *)calloc(1, sizeof(*panel));
    if (!panel) return nullptr;
    panel->frontend = frontend;
    panel->selected = -1;
    panel->vk_surface = VK_NULL_HANDLE;
    panel->surface = wl_compositor_create_surface(frontend->compositor);
    if (!panel->surface) { free(panel); return nullptr; }
    wl_surface_add_listener(panel->surface, &wl_surface_listener, panel);
    panel->popup_surface = zwp_input_method_v2_get_input_popup_surface(frontend->input_method, panel->surface);
    if (!panel->popup_surface) { wl_surface_destroy(panel->surface); free(panel); return nullptr; }
    zwp_input_popup_surface_v2_add_listener(panel->popup_surface, &popup_surface_listener, panel);

    /* HiDPI helpers — both optional. The fractional-scale event fires
     * before the first commit, eliminating the legacy "render at 1× then
     * reupload at N×" round-trip the old enter-based path required. */
    if (frontend->viewporter) {
        panel->viewport = wp_viewporter_get_viewport(frontend->viewporter, panel->surface);
    }
    if (frontend->fractional_scale_manager) {
        panel->fractional_scale = wp_fractional_scale_manager_v1_get_fractional_scale(
            frontend->fractional_scale_manager, panel->surface);
        if (panel->fractional_scale) {
            wp_fractional_scale_v1_add_listener(panel->fractional_scale,
                                                &fractional_scale_listener, panel);
        }
    }

    panel_render_ctx_init(&panel->render);
    /* Route LRU evictions through the retire ring (use-after-free guard:
     * the just-evicted layout's flux_image may still be referenced by the
     * frame the GPU is currently rendering). */
    panel_render_ctx_set_evict(&panel->render, panel_retire_layout, panel);
    return panel;
}

void typio_panel_destroy(TypioPanel *panel) {
    if (!panel) return;
    hide_surface(panel);
    fx_teardown(panel);
    /* fx_teardown waited the device idle (or there was never a swapchain),
     * so retire-ring contents and the current geometry are safe to free now. */
    for (size_t i = 0; i < PANEL_RETIRE_DEPTH; ++i) retire_slot_free(&panel->retire[i]);
    panel_geometry_free(panel->geom);
    panel->geom = nullptr;
    panel_render_ctx_free(&panel->render);
    clear_outputs(panel);
    free(panel->status_text);
    if (panel->fractional_scale) wp_fractional_scale_v1_destroy(panel->fractional_scale);
    if (panel->viewport) wp_viewport_destroy(panel->viewport);
    if (panel->popup_surface) zwp_input_popup_surface_v2_destroy(panel->popup_surface);
    if (panel->surface) wl_surface_destroy(panel->surface);
    free(panel);
}

bool typio_panel_update_content(TypioPanel *panel,
                                const TypioPanelContent *content) {
    if (!panel || !content) return false;

    /* Update persistent status only when the caller explicitly sets it.
     * InputContext-driven updates leave status.message == nullptr so they
     * do not clobber a voice indicator that may still be visible. */
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
        hide_surface(panel);
        return true;
    }

    /* When the IME has no candidates, surface the persistent voice-status
     * text (if any) through the preedit slot. Voice "[Recording...]" and
     * an IME preedit string share the same palette colour, same layout
     * slot, and the same delta-classification path — no second code path. */
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
    if (panel) hide_surface(panel);
}

bool typio_panel_is_available(TypioPanel *panel) {
    return panel && panel->surface && panel->popup_surface;
}

bool typio_panel_present_retry_pending(TypioPanel *panel) {
    return panel && panel->present_retry;
}

void typio_panel_invalidate_config(TypioPanel *panel) {
    if (!panel) return;
    panel->config_valid = false;
    memset(&panel->theme_cache, 0, sizeof(panel->theme_cache));
    /* Invalidating the LRU directly frees its layouts' flux_image resources
     * (TypioTextShape::image is released by typio_text_shape_free). Those
     * images may be referenced by an in-flight frame, so the LRU drain has
     * to happen behind a fence. Config changes are user-driven and rare,
     * so paying a device-idle wait here is acceptable; the per-keystroke
     * path goes through the retire ring instead. */
    if (panel->fx_ready) {
        flux_device *dev = typio_render_device_get();
        if (dev) flux_device_wait_idle(dev);
        /* The wait drained every in-flight frame, so any geometry parked
         * in the retire ring is also safe to free now — pull it out before
         * the LRU drop invalidates layouts those geometries reference. */
        for (size_t i = 0; i < PANEL_RETIRE_DEPTH; ++i) {
            retire_slot_drain(&panel->retire[i]);
        }
    }
    panel_render_ctx_invalidate(&panel->render);
    panel_geometry_free(panel->geom);
    panel->geom = nullptr;
}

void typio_panel_handle_output_change(TypioPanel *panel, struct wl_output *output) {
    if (!panel || !output) return;
    if (!tracks_output(panel, output)) return;
    if (!find_frontend_output(panel, output)) untrack_output(panel, output);
    else refresh_visible(panel);
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
