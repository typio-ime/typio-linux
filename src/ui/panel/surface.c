/**
 * @file surface.c
 * @brief TypioPanelSurface — input-popup wl_surface + Vulkan swapchain present.
 *
 * Presents a flux (Vulkan) swapchain directly onto the panel's
 * zwp_input_popup_surface_v2 wl_surface. The swapchain owns frame pacing and
 * buffering, so there is no SHM buffer pool or manual frame throttling. The
 * orchestrating TypioPanel composes a PanelGeometry and hands it to
 * panel_surface_present(); this module only puts pixels on screen.
 */

#define VK_USE_PLATFORM_WAYLAND_KHR
#include <flux/flux.h>
#include <flux/vulkan.h>

#include "internal.h"
#include "surface.h"
#include "panel.h"
#include "paint.h"
#include "theme.h"
#include "text_shaper.h"
#include "device.h"
#include "clock.h"
#include "typio/abi/log.h"

#include <inttypes.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Bounded acquire/present timeout. The panel presents synchronously on the
 * single-threaded IME event loop, so a compositor that has stopped releasing
 * swapchain images (display asleep or surface occluded after a lock/suspend)
 * must never block the loop in vkAcquireNextImageKHR. 2ms is tight enough to
 * keep navigation responsive under RETRY-storm while still allowing healthy
 * acquires (which complete in <100us) to succeed. The healthy on-demand path
 * acquires instantly and never approaches this. (ADR-0010 scope correction:
 * shortened from 32ms to reduce RETRY-cascade latency during navigation.) */
#define PANEL_PRESENT_TIMEOUT_NS     (2ull * 1000ull * 1000ull)
/* Recreate the swapchain after this many consecutive stalls. flux_surface_resize
 * rebuilds the chain and discards the per-frame semaphores left dangling by the
 * stalled acquires, so presentation resumes cleanly once the session is back. */
#define PANEL_PRESENT_RECOVER_STREAK 2

/* Hard cap on per-epoch retire-slot growth. During a long present stall the
 * epoch does not advance, so geometries/layouts retired across many CONTENT
 * deltas accumulate in the current slot. The cap converts pathological growth
 * into a bounded vkDeviceWaitIdle + inline drain, trading a one-off ms-scale
 * stall for predictable memory use. */
#define PANEL_RETIRE_SLOT_MAX 256

/* Depth = 3 = flux's frames_in_flight (2) plus the just-presented frame. */
#define PANEL_RETIRE_DEPTH 3

/* Swapchain buffer allocation quantum (physical px). Rounding the buffer up to
 * this quantum and growing it only when content exceeds it means the swapchain
 * is rebuilt a handful of times during warm-up and then never again (ADR-0013).
 * Only meaningful with a viewport, which crops the oversized buffer back to the
 * exact content rect via wp_viewport_set_source. */
#define PANEL_SURFACE_QUANTUM 64

/* ── Output tracking ────────────────────────────────────────────────── */

typedef struct PanelOutputRef {
    struct wl_output      *output;
    struct PanelOutputRef *next;
} PanelOutputRef;

/* ── Frame-retire queue ─────────────────────────────────────────────── */

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

/* ── Surface struct ─────────────────────────────────────────────────── */

struct TypioPanelSurface {
    TypioWlFrontend *frontend;
    TypioPanel      *owner;   /* re-rendered on a scale/output change */

    /* Wayland surface objects */
    struct wl_surface                  *surface;
    struct zwp_input_popup_surface_v2  *popup_surface;

    /* Optional HiDPI helpers. Both are nullptr when the compositor lacks
     * wp_fractional_scale_v1 / wp_viewporter; in that case we fall back to the
     * integer wl_surface buffer_scale path. */
    struct wp_viewport             *viewport;
    struct wp_fractional_scale_v1  *fractional_scale;

    /* flux GPU present pipeline (Vulkan swapchain on the panel wl_surface) */
    VkSurfaceKHR  vk_surface;
    flux_surface *fx_surface;
    flux_canvas  *fx_canvas;
    flux_arena    fx_arena;
    bool          fx_ready;
    int           surf_w, surf_h;   /* swapchain BUFFER extent, physical px */

    /* Present stall recovery (lock/suspend). */
    int  present_timeout_streak;

    /* Frame-retire ring. `present_epoch` advances on every successful present;
     * `retire[epoch % depth]` holds geometries/layouts that were live during
     * that epoch, freed when the slot is reused. */
    PanelRetireSlot retire[PANEL_RETIRE_DEPTH];
    uint64_t        present_epoch;

    /* Output tracking (for scale resolution; fallback path) */
    PanelOutputRef *entered_outputs;

    /* Scale signals — resolved in priority order:
     *   fractional_scale_120 (wp_fractional_scale_v1.preferred_scale, 120ths)
     *   preferred_buffer_scale (wl_surface v6, integer)
     *   entered_outputs->scale (wl_surface.enter, integer)
     *   max(frontend->outputs[].scale) (initial guess)
     */
    uint32_t fractional_scale_120;       /* 0 when no fractional signal yet */
    int      preferred_buffer_scale;     /* 0 when no v6 hint yet */

    /* Text-input cursor rectangle (informational; set by compositor) */
    int text_input_x, text_input_y, text_input_w, text_input_h;
};

static void track_output(TypioPanelSurface *s, struct wl_output *output);
static void untrack_output(TypioPanelSurface *s, struct wl_output *output);

/* ── Retire helpers ─────────────────────────────────────────────────── */

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

    /* Cap reached: fence the GPU and drain everything we've parked so far in
     * this slot inline. Converts the worst-case "RETRY-storm while the user
     * keeps paging candidates" into a bounded one-off stall. */
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

void panel_surface_retire_geometry(TypioPanelSurface *s, PanelGeometry *g) {
    if (!g) return;
    if (!s || !s->fx_ready) {
        panel_geometry_free(g);
        return;
    }
    PanelRetireSlot *slot = &s->retire[s->present_epoch % PANEL_RETIRE_DEPTH];
    retire_slot_push(slot, PANEL_RETIRE_GEOMETRY, g);
}

void panel_surface_retire_shape(TypioPanelSurface *s, TypioTextShape *shape) {
    if (!shape) return;
    if (!s || !s->fx_ready) {
        typio_text_shape_free(shape);
        return;
    }
    PanelRetireSlot *slot = &s->retire[s->present_epoch % PANEL_RETIRE_DEPTH];
    retire_slot_push(slot, PANEL_RETIRE_LAYOUT, shape);
}

void panel_surface_drain_retire_fenced(TypioPanelSurface *s) {
    if (!s || !s->fx_ready) return;
    flux_device *dev = typio_render_device_get();
    if (dev) flux_device_wait_idle(dev);
    for (size_t i = 0; i < PANEL_RETIRE_DEPTH; ++i) {
        retire_slot_drain(&s->retire[i]);
    }
}

/* ── Output helpers ─────────────────────────────────────────────────── */

static const TypioWlOutput *find_frontend_output(const TypioPanelSurface *s,
                                                   struct wl_output *output) {
    for (TypioWlOutput *o = s->frontend ? s->frontend->outputs : nullptr;
         o; o = o->next) {
        if (o->output == output) return o;
    }
    return nullptr;
}

static bool tracks_output(const TypioPanelSurface *s, struct wl_output *output) {
    for (PanelOutputRef *r = s->entered_outputs; r; r = r->next) {
        if (r->output == output) return true;
    }
    return false;
}

float panel_surface_scale(const TypioPanelSurface *s) {
    if (!s) return 1.0f;
    if (s->fractional_scale_120 > 0) {
        return (float)s->fractional_scale_120 / 120.0f;
    }
    if (s->preferred_buffer_scale > 0) {
        return (float)s->preferred_buffer_scale;
    }

    int best = 0;
    for (PanelOutputRef *r = s->entered_outputs; r; r = r->next) {
        const TypioWlOutput *o = find_frontend_output(s, r->output);
        if (o && o->scale > best) best = o->scale;
    }
    if (best > 0) return (float)best;

    /* Initial guess: the highest-DPI output the frontend has seen. */
    if (s->frontend) {
        for (TypioWlOutput *o = s->frontend->outputs; o; o = o->next) {
            if (o->scale > best) best = o->scale;
        }
    }
    return best > 0 ? (float)best : 1.0f;
}

static void track_output(TypioPanelSurface *s, struct wl_output *output) {
    if (!s || !output || tracks_output(s, output)) return;
    PanelOutputRef *r = (PanelOutputRef *)calloc(1, sizeof(*r));
    if (!r) return;
    r->output = output;
    r->next = s->entered_outputs;
    s->entered_outputs = r;
    typio_panel_refresh(s->owner);
}

static void untrack_output(TypioPanelSurface *s, struct wl_output *output) {
    PanelOutputRef **link = &s->entered_outputs;
    while (*link) {
        PanelOutputRef *r = *link;
        if (r->output == output) {
            *link = r->next;
            free(r);
            typio_panel_refresh(s->owner);
            return;
        }
        link = &r->next;
    }
}

static void clear_outputs(TypioPanelSurface *s) {
    while (s && s->entered_outputs) {
        PanelOutputRef *r = s->entered_outputs;
        s->entered_outputs = r->next;
        free(r);
    }
}

void panel_surface_handle_output_change(TypioPanelSurface *s, struct wl_output *output) {
    if (!s || !output) return;
    if (!tracks_output(s, output)) return;
    if (!find_frontend_output(s, output)) untrack_output(s, output);
    else typio_panel_refresh(s->owner);
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

static void fx_teardown(TypioPanelSurface *s) {
    if (!s) return;

    flux_device *dev = (s->fx_surface || s->vk_surface) ? typio_render_device_get() : nullptr;
    if (dev && s->fx_ready) flux_device_wait_idle(dev);

    if (s->fx_canvas) {
        flux_canvas_destroy(s->fx_canvas);
        s->fx_canvas = nullptr;
    }
    if (s->fx_ready) {
        flux_arena_destroy(&s->fx_arena);
    }
    if (s->fx_surface) {
        flux_surface_release(s->fx_surface);
        s->fx_surface = nullptr;
    }
    if (s->vk_surface != VK_NULL_HANDLE && dev) {
        vkDestroySurfaceKHR(flux_device_vk_instance(dev), s->vk_surface, nullptr);
    }
    s->vk_surface = VK_NULL_HANDLE;
    s->fx_ready   = false;
    s->surf_w = s->surf_h = 0;
}

static inline int panel_quantize_up(int v) {
    if (v < 1) v = 1;
    return ((v + PANEL_SURFACE_QUANTUM - 1) / PANEL_SURFACE_QUANTUM) * PANEL_SURFACE_QUANTUM;
}

/* Create / resize the swapchain to cover (w, h) physical pixels. With a
 * viewport the buffer is grow-only and quantised (the content is cropped to
 * size at present time); without one it tracks (w, h) exactly. */
static bool ensure_fx_surface(TypioPanelSurface *s, int w, int h) {
    if (!s || !s->frontend || !s->surface || w <= 0 || h <= 0) return false;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    if (s->vk_surface == VK_NULL_HANDLE) {
        VkWaylandSurfaceCreateInfoKHR ci = {
            .sType   = VK_STRUCTURE_TYPE_WAYLAND_SURFACE_CREATE_INFO_KHR,
            .pNext   = nullptr,
            .flags   = 0,
            .display = s->frontend->display,
            .surface = s->surface,
        };
        if (vkCreateWaylandSurfaceKHR(flux_device_vk_instance(dev), &ci, nullptr,
                                      &s->vk_surface) != VK_SUCCESS) {
            s->vk_surface = VK_NULL_HANDLE;
            return false;
        }
    }

    /* Buffer extent to allocate. With a viewport, round up to the quantum so
     * the buffer outlives small per-page width changes; without one it must
     * equal the content exactly (the buffer maps 1:1 to the surface). */
    int bw = w, bh = h;
    if (s->viewport) {
        bw = panel_quantize_up(w);
        bh = panel_quantize_up(h);
    }

    if (!s->fx_surface) {
        flux_surface_desc sd = {};
        sd.type           = FLUX_TYPE_SURFACE_DESC;
        sd.vk_surface_khr = s->vk_surface;
        sd.width          = (uint32_t)bw;
        sd.height         = (uint32_t)bh;
        /* Non-blocking present (MAILBOX/IMMEDIATE, falls back to FIFO). The
         * panel presents synchronously on the single-threaded IME event loop,
         * so a vsync/FIFO present blocks vkQueuePresentKHR until the compositor
         * releases a swapchain buffer — which stalls key handling and makes
         * candidate switching lag (ADR-0010). Tearing on a small candidate
         * panel is irrelevant. */
        sd.vsync          = false;
        if (flux_surface_create(dev, &sd, &s->fx_surface) != FLUX_OK) {
            s->fx_surface = nullptr;
            return false;
        }

        flux_canvas_desc cd = {};
        cd.type    = FLUX_TYPE_CANVAS_DESC;
        cd.surface = s->fx_surface;
        if (flux_canvas_create(&cd, &s->fx_canvas) != FLUX_OK) {
            s->fx_canvas = nullptr;
            flux_surface_release(s->fx_surface);
            s->fx_surface = nullptr;
            return false;
        }

        if (flux_arena_init(&s->fx_arena, 256 * 1024, nullptr) != FLUX_OK) {
            flux_canvas_destroy(s->fx_canvas);
            s->fx_canvas = nullptr;
            flux_surface_release(s->fx_surface);
            s->fx_surface = nullptr;
            return false;
        }

        s->surf_w   = bw;
        s->surf_h   = bh;
        s->fx_ready = true;
    } else if (s->viewport) {
        /* Grow-only: rebuild only when content exceeds the current buffer.
         * Shrinks and sub-quantum widenings reuse the existing swapchain and
         * are cropped to size by wp_viewport_set_source at present time. */
        int nw = s->surf_w, nh = s->surf_h;
        if (w > nw) nw = panel_quantize_up(w);
        if (h > nh) nh = panel_quantize_up(h);
        if ((nw != s->surf_w || nh != s->surf_h) &&
            flux_surface_resize(s->fx_surface, (uint32_t)nw, (uint32_t)nh) == FLUX_OK) {
            s->surf_w = nw;
            s->surf_h = nh;
        }
    } else if (s->surf_w != w || s->surf_h != h) {
        /* No viewport: buffer maps 1:1, must match content exactly. */
        if (flux_surface_resize(s->fx_surface, (uint32_t)w, (uint32_t)h) == FLUX_OK) {
            s->surf_w = w;
            s->surf_h = h;
        }
    }

    return s->fx_ready;
}

/* Record + present one frame. Bounded acquire (PANEL_PRESENT_TIMEOUT_NS) so a
 * stalled compositor cannot block the single-threaded IME event loop; after a
 * few consecutive stalls the swapchain is recreated so presentation resumes. */
static PanelPresentResult do_present(TypioPanelSurface *s,
                                     const PanelGeometry *geom, int selected) {
    if (!s->fx_ready || !geom || !geom->palette) return PANEL_PRESENT_FAIL;

    flux_frame_begin_desc bd = {};
    bd.type       = FLUX_TYPE_FRAME_BEGIN_DESC;
    bd.timeout_ns = PANEL_PRESENT_TIMEOUT_NS;

    uint64_t t_begin_us = typio_wl_monotonic_us();

    flux_frame *frame = nullptr;
    flux_result r = flux_surface_begin_frame(s->fx_surface, &bd, &frame);
    if (r == FLUX_ERROR_SURFACE_LOST) {
        (void)flux_surface_resize(s->fx_surface,
                                  (uint32_t)s->surf_w, (uint32_t)s->surf_h);
        s->present_timeout_streak = 0;
        r = flux_surface_begin_frame(s->fx_surface, &bd, &frame);
    }
    uint64_t t_acquire_us = typio_wl_monotonic_us();

    if (r == FLUX_ERROR_TIMEOUT) {
        uint64_t elapsed_us = t_acquire_us - t_begin_us;
        if (++s->present_timeout_streak >= PANEL_PRESENT_RECOVER_STREAK) {
            (void)flux_surface_resize(s->fx_surface,
                                      (uint32_t)s->surf_w, (uint32_t)s->surf_h);
            s->present_timeout_streak = 0;
            typio_log_debug("Panel present RETRY streak: swapchain rebuilt (streak=%d, elapsed=%" PRIu64 "us)",
                            PANEL_PRESENT_RECOVER_STREAK, elapsed_us);
        } else {
            typio_log_debug("Panel present RETRY: acquire timeout (streak=%d/%d, elapsed=%" PRIu64 "us)",
                            s->present_timeout_streak, PANEL_PRESENT_RECOVER_STREAK, elapsed_us);
        }
        return PANEL_PRESENT_RETRY;
    }
    if (r != FLUX_OK) return PANEL_PRESENT_FAIL;

    s->present_timeout_streak = 0;

    flux_color clear = panel_bg_color(geom->palette);
    if (flux_canvas_begin(s->fx_canvas, frame, &clear) != FLUX_OK) return PANEL_PRESENT_FAIL;

    PanelPaintTarget target = { s->fx_canvas, &s->fx_arena };
    panel_record(&target, geom, selected);

    flux_arena_reset(&s->fx_arena);
    flux_canvas_end(s->fx_canvas);

    uint64_t t_record_us = typio_wl_monotonic_us();

    if (flux_frame_submit(frame) != FLUX_OK) return PANEL_PRESENT_FAIL;

    uint64_t t_submit_us = typio_wl_monotonic_us();

    r = flux_frame_present(frame);

    uint64_t t_present_us = typio_wl_monotonic_us();

    if (r == FLUX_ERROR_SURFACE_LOST) {
        (void)flux_surface_resize(s->fx_surface,
                                  (uint32_t)s->surf_w, (uint32_t)s->surf_h);
        return PANEL_PRESENT_RETRY;  /* next update repaints at the new extent */
    }

    if (r == FLUX_OK) {
        uint64_t acquire_us = t_acquire_us - t_begin_us;
        uint64_t record_us  = t_record_us - t_acquire_us;
        uint64_t submit_us  = t_submit_us - t_record_us;
        uint64_t present_us = t_present_us - t_submit_us;
        uint64_t total_us   = t_present_us - t_begin_us;
        /* Log if any phase exceeded 5ms (5000us) — the threshold where
         * navigation starts feeling sluggish. Below this the per-event
         * emit floods DEBUG output during normal typing. */
        if (total_us > 5000) {
            typio_log_debug("Panel present slow: total=%" PRIu64 "us "
                            "(acquire=%" PRIu64 " record=%" PRIu64 " submit=%" PRIu64 " present=%" PRIu64 ") "
                            "selected=%d",
                            total_us, acquire_us, record_us, submit_us, present_us, selected);
        }
    }

    return r == FLUX_OK ? PANEL_PRESENT_OK : PANEL_PRESENT_FAIL;
}

PanelPresentResult panel_surface_present(TypioPanelSurface *s,
                                         const PanelGeometry *geom, int selected) {
    if (!s || !s->surface || !geom) return PANEL_PRESENT_FAIL;

    float sc = geom->scale > 0.0f ? geom->scale : 1.0f;
    int pw = (int)ceilf((float)geom->panel_w * sc);
    int ph = (int)ceilf((float)geom->panel_h * sc);
    if (pw < 1) pw = 1;
    if (ph < 1) ph = 1;
    if (!ensure_fx_surface(s, pw, ph)) {
        typio_log_warning("Panel: flux surface unavailable");
        return PANEL_PRESENT_FAIL;
    }

    /* Tell the compositor how to interpret the buffer. With wp_viewporter +
     * wp_fractional_scale_v1 we publish the buffer at scale=1 and map it to the
     * logical rect via the viewport. Without those globals we fall back to the
     * legacy integer wl_surface buffer_scale path. */
    if (s->viewport) {
        wl_surface_set_buffer_scale(s->surface, 1);
        wp_viewport_set_source(s->viewport,
                               wl_fixed_from_int(0), wl_fixed_from_int(0),
                               wl_fixed_from_int(pw), wl_fixed_from_int(ph));
        wp_viewport_set_destination(s->viewport,
                                    geom->panel_w, geom->panel_h);
    } else {
        int isc = (int)ceilf(sc);
        if (isc < 1) isc = 1;
        wl_surface_set_buffer_scale(s->surface, isc);
    }

    PanelPresentResult pres = do_present(s, geom, selected);
    if (pres == PANEL_PRESENT_OK) {
        /* Advance the retire ring: anything pushed during the previous sweep at
         * (epoch - PANEL_RETIRE_DEPTH + 1) is now safe to free. */
        s->present_epoch++;
        retire_slot_drain(&s->retire[s->present_epoch % PANEL_RETIRE_DEPTH]);
    } else if (pres == PANEL_PRESENT_RETRY) {
        /* Compositor isn't releasing swapchain images yet (display asleep or
         * surface occluded after a lock/suspend). The caller keeps the update
         * pending and re-presents later so the visible highlight catches up. */
    }
    return pres;
}

void panel_surface_hide(TypioPanelSurface *s) {
    if (!s || !s->surface) return;
    /* Unmap by committing a null buffer. The flux swapchain stays alive so a
     * later show only needs a present, not a swapchain rebuild. */
    wl_surface_attach(s->surface, nullptr, 0, 0);
    wl_surface_commit(s->surface);
}

bool panel_surface_is_available(const TypioPanelSurface *s) {
    return s && s->surface && s->popup_surface;
}

bool panel_surface_fx_ready(const TypioPanelSurface *s) {
    return s && s->fx_ready;
}

/* ── Surface / output event handlers ───────────────────────────────── */

static void on_text_input_rectangle(void *data,
                                     [[maybe_unused]] struct zwp_input_popup_surface_v2 *surf,
                                     int32_t x, int32_t y, int32_t w, int32_t h) {
    TypioPanelSurface *s = (TypioPanelSurface *)data;
    s->text_input_x = x;
    s->text_input_y = y;
    s->text_input_w = w;
    s->text_input_h = h;
    typio_wl_panel_coordinator_note_caret_rect(s->frontend);
    typio_wl_panel_coordinator_mark_anchor_ready(s->frontend, "text_input_rectangle");
}

static const struct zwp_input_popup_surface_v2_listener popup_surface_listener = {
    .text_input_rectangle = on_text_input_rectangle,
};

static void on_surface_enter(void *data,
                               [[maybe_unused]] struct wl_surface *surface,
                               struct wl_output *output) {
    track_output((TypioPanelSurface *)data, output);
}

static void on_surface_leave(void *data,
                               [[maybe_unused]] struct wl_surface *surface,
                               struct wl_output *output) {
    untrack_output((TypioPanelSurface *)data, output);
}

/* wl_surface v6: integer scale hint emitted before the first commit. We prefer
 * it over the legacy enter-based output scan. wp_fractional_scale_v1 still wins
 * above this when both are present. */
static void on_surface_preferred_buffer_scale(void *data,
                                              [[maybe_unused]] struct wl_surface *surface,
                                              int32_t factor) {
    TypioPanelSurface *s = (TypioPanelSurface *)data;
    if (!s || factor <= 0) return;
    if (s->preferred_buffer_scale == factor) return;
    s->preferred_buffer_scale = factor;
    typio_panel_refresh(s->owner);
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

/* wp_fractional_scale_v1: 24.8 fixed-point logical-to-physical ratio in 120ths
 * (120 = 1.0×, 150 = 1.25×, 180 = 1.5×). When present we use it as the source
 * of truth, sample the wl_surface buffer at scale=1, and let wp_viewport handle
 * the logical sizing. */
static void on_fractional_preferred_scale(void *data,
                                          [[maybe_unused]] struct wp_fractional_scale_v1 *scale,
                                          uint32_t scale_120) {
    TypioPanelSurface *s = (TypioPanelSurface *)data;
    if (!s || scale_120 == 0) return;
    if (s->fractional_scale_120 == scale_120) return;
    s->fractional_scale_120 = scale_120;
    typio_panel_refresh(s->owner);
}

static const struct wp_fractional_scale_v1_listener fractional_scale_listener = {
    .preferred_scale = on_fractional_preferred_scale,
};

/* ── Lifecycle ──────────────────────────────────────────────────────── */

TypioPanelSurface *panel_surface_create(TypioWlFrontend *frontend, TypioPanel *owner) {
    if (!frontend || !frontend->compositor || !frontend->input_method) return nullptr;
    TypioPanelSurface *s = (TypioPanelSurface *)calloc(1, sizeof(*s));
    if (!s) return nullptr;
    s->frontend   = frontend;
    s->owner      = owner;
    s->vk_surface = VK_NULL_HANDLE;
    s->surface    = wl_compositor_create_surface(frontend->compositor);
    if (!s->surface) { free(s); return nullptr; }
    wl_surface_add_listener(s->surface, &wl_surface_listener, s);
    s->popup_surface = zwp_input_method_v2_get_input_popup_surface(frontend->input_method, s->surface);
    if (!s->popup_surface) { wl_surface_destroy(s->surface); free(s); return nullptr; }
    zwp_input_popup_surface_v2_add_listener(s->popup_surface, &popup_surface_listener, s);

    /* HiDPI helpers — both optional. The fractional-scale event fires before
     * the first commit, eliminating the legacy "render at 1× then reupload at
     * N×" round-trip the old enter-based path required. */
    if (frontend->viewporter) {
        s->viewport = wp_viewporter_get_viewport(frontend->viewporter, s->surface);
    }
    if (frontend->fractional_scale_manager) {
        s->fractional_scale = wp_fractional_scale_manager_v1_get_fractional_scale(
            frontend->fractional_scale_manager, s->surface);
        if (s->fractional_scale) {
            wp_fractional_scale_v1_add_listener(s->fractional_scale,
                                                &fractional_scale_listener, s);
        }
    }
    return s;
}

void panel_surface_destroy(TypioPanelSurface *s) {
    if (!s) return;
    fx_teardown(s);
    /* fx_teardown waited the device idle (or there was never a swapchain), so
     * retire-ring contents are safe to free now. */
    for (size_t i = 0; i < PANEL_RETIRE_DEPTH; ++i) retire_slot_free(&s->retire[i]);
    clear_outputs(s);
    if (s->fractional_scale) wp_fractional_scale_v1_destroy(s->fractional_scale);
    if (s->viewport) wp_viewport_destroy(s->viewport);
    if (s->popup_surface) zwp_input_popup_surface_v2_destroy(s->popup_surface);
    if (s->surface) wl_surface_destroy(s->surface);
    free(s);
}
