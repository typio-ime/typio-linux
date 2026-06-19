/**
 * @file glyph_atlas.c
 * @brief Shared glyph coverage atlas + reclamation (see header).
 */
#include "glyph_atlas.h"
#include "glyph_upload.h"
#include "glyph_pack.h"
#include "font_cache.h"
#include "device.h"

#include <flux/flux.h>

#include <typio/abi/log.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include <stdlib.h>
#include <string.h>

#define GLYPH_ATLAS_PAD   1u      /* transparent gutter to stop bilinear bleed */
#define GLYPH_SLOT_CAP    131072u /* power of two; 4× the atlas image capacity
                                    * so the hash table stays well below the 75%
                                    * reclaim threshold in normal use            */
#define ATLAS_RECLAIM_THRESHOLD_PCT 75

/* Per-frame upload queue. glyph_atlas_get() pushes one entry per miss;
 * glyph_atlas_flush() submits them all in a single vkQueueSubmit. The cap is
 * generous: a 16-row candidate panel with up to ~32 glyphs per row plus
 * preedit + mode label is ~600 entries; the cap covers a worst-case page with
 * headroom while staying bounded. On overflow (unreachable in practice) we
 * flush inline, which degrades to the legacy per-frame behaviour.
 * GLYPH_PENDING_CAP is defined in the header so tests can verify overflow. */

typedef struct {
    uint8_t *data;         /* tightly packed @w×@h R8 coverage (owned)        */
    uint32_t slot_index;   /* index into g_atlas.slots to clear on failure    */
    uint32_t x, y, w, h;
    size_t   bytes;
} PendingUpload;

typedef struct {
    flux_image  *image;
    GlyphSlot   *slots;     /* GLYPH_SLOT_CAP entries                          */
    GlyphPacker  packer;    /* skyline shelf cursor (see glyph_pack.h)         */
    uint32_t     live_count;/* occupied entries                                */
    bool         packer_exhausted; /* image ran out of shelf space (resettable) */
} GlyphAtlas;

static GlyphAtlas g_atlas;

/* Per-frame upload queue. Entries own their @data; flush frees them. The cap
 * is exposed in the header (GLYPH_PENDING_CAP) for tests that verify overflow. */
static PendingUpload g_pending[GLYPH_PENDING_CAP];
static uint32_t      g_pending_count;

/* Upload backend. Defaults to the Vulkan batched upload; tests override via
 * glyph_atlas_set_upload_fn to verify the queue/batching contract without
 * a GPU. */
static bool default_upload_fn(const GlyphUploadRegion *regions, size_t count,
                               void *user)
{
    (void)user;
    flux_image *img = glyph_atlas_image();
    if (!img) return false;
    return glyph_upload_regions(img, regions, count);
}

static GlyphAtlasUploadFn g_upload_fn = default_upload_fn;
static void              *g_upload_user;

/* Cumulative diagnostics counters (session-wide). */
static uint64_t g_atlas_rebuilds;
static uint64_t g_glyphs_rasterized;
static uint64_t g_flush_count;          /* cumulative glyph_atlas_flush() calls that did work */
static uint32_t g_flush_peak_batch;     /* largest single batch observed        */
static uint64_t g_flush_total_regions;  /* cumulative regions across all batches */

static bool glyph_atlas_ensure(void)
{
    if (g_atlas.image) return true;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    /* Clear so the gutters between glyphs sample as zero coverage. */
    uint8_t *zero = (uint8_t *)calloc((size_t)GLYPH_ATLAS_DIM * GLYPH_ATLAS_DIM, 1);
    if (!zero) return false;

    flux_image_desc id = {};
    id.type         = FLUX_TYPE_IMAGE_DESC;
    id.width        = GLYPH_ATLAS_DIM;
    id.height       = GLYPH_ATLAS_DIM;
    id.format       = FLUX_FORMAT_R8_UNORM;
    id.initial_data = zero;

    flux_image *img = NULL;
    flux_result r = flux_image_create(dev, &id, &img);
    free(zero);
    if (r != FLUX_OK || !img) return false;

    g_atlas.slots = (GlyphSlot *)calloc(GLYPH_SLOT_CAP, sizeof(GlyphSlot));
    if (!g_atlas.slots) { flux_image_release(img); return false; }

    g_atlas.image  = img;
    g_atlas.packer = (GlyphPacker){0};
    return true;
}

flux_image *glyph_atlas_image(void)
{
    if (!g_atlas.image) glyph_atlas_ensure();
    return g_atlas.image;
}

uint32_t glyph_atlas_entry_count(void)
{
    return g_atlas.slots ? g_atlas.live_count : 0;
}

/* Tear the atlas down to nothing; glyph_atlas_ensure rebuilds a fresh zeroed
 * image (and clean gutters) on the next lookup. The persistent upload context
 * is separate and deliberately left intact. */
static void glyph_atlas_reset(void)
{
    if (!g_atlas.image && !g_atlas.slots) return;

    flux_device *dev = typio_render_device_get();
    if (dev) flux_device_wait_idle(dev);

    if (g_atlas.image) flux_image_release(g_atlas.image);
    free(g_atlas.slots);
    g_atlas = (GlyphAtlas){0};
    g_atlas_rebuilds++;
}

bool glyph_atlas_should_reclaim(uint32_t live_count, bool packer_exhausted,
                                 uint32_t slot_capacity, uint32_t threshold_pct)
{
    /* Both triggers documented in the header must fire:
     *   - packer exhaustion: texture shelf space ran out
     *   - load factor: hash table crossed threshold_pct occupancy
     * Either condition forces a wholesale rebuild. */
    if (packer_exhausted) return true;
    if (slot_capacity == 0) return false;   /* avoid divide-by-zero in bad calls */
    uint32_t threshold =
        (uint32_t)((uint64_t)slot_capacity * threshold_pct / 100);
    return live_count >= threshold;
}

bool glyph_atlas_reclaim(void)
{
    if (!g_atlas.slots) return false;

    /* Delegate the trigger decision to the pure predicate so the contract is
     * unit-testable without a GPU. See glyph_atlas_should_reclaim for the
     * rationale (the historical bug: only packer-exhaustion was honoured
     * even though the header documented both triggers). */
    if (!glyph_atlas_should_reclaim(g_atlas.live_count,
                                     g_atlas.packer_exhausted,
                                     GLYPH_SLOT_CAP,
                                     ATLAS_RECLAIM_THRESHOLD_PCT))
        return false;

    typio_log_debug("Glyph atlas reclaim: rebuild (live=%u/%u, reason=%s)",
                    g_atlas.live_count, (unsigned)GLYPH_SLOT_CAP,
                    g_atlas.packer_exhausted
                        ? (glyph_atlas_should_reclaim(g_atlas.live_count, false,
                                                       GLYPH_SLOT_CAP,
                                                       ATLAS_RECLAIM_THRESHOLD_PCT)
                              ? "load+packer" : "image-full")
                        : "load-factor");
    glyph_atlas_reset();
    return true;
}

void glyph_atlas_get_diag(GlyphAtlasDiag *out)
{
    if (!out) return;
    out->live          = g_atlas.slots ? g_atlas.live_count : 0;
    out->slot_capacity = GLYPH_SLOT_CAP;
    out->shelf_y       = g_atlas.packer.shelf_y;
    out->dim           = GLYPH_ATLAS_DIM;
    out->packer_full   = g_atlas.packer_exhausted;
    out->rebuilds      = g_atlas_rebuilds;
    out->rasterized    = g_glyphs_rasterized;
    out->flushes       = g_flush_count;
    out->flush_peak_batch   = g_flush_peak_batch;
    out->flush_total_regions = g_flush_total_regions;
}

void glyph_atlas_shutdown(void)
{
    /* Drop any queued uploads that never got flushed (engine teardown while a
     * frame was in progress). The slots they reference are about to be freed
     * along with the rest of the atlas, so we don't need to mark them. */
    for (uint32_t i = 0; i < g_pending_count; ++i) free(g_pending[i].data);
    g_pending_count = 0;

    if (g_atlas.image) {
        /* The atlas may still be referenced by an in-flight panel frame. */
        flux_device *dev = typio_render_device_get();
        if (dev) flux_device_wait_idle(dev);
        flux_image_release(g_atlas.image);
    }
    glyph_upload_shutdown();
    free(g_atlas.slots);
    g_atlas = (GlyphAtlas){0};
}

/* Push a pending upload. Steals @data (caller must not free it on success).
 * Returns false (and frees @data itself) if the queue is full; on overflow we
 * flush inline and retry, so a false return here propagates as "slot not
 * drawable" rather than as a crash.
 *
 * Exposed as glyph_atlas_test_push_pending for tests; production callers go
 * through this same body via the static alias below. */
bool glyph_atlas_test_push_pending(uint32_t slot_index, uint32_t x, uint32_t y,
                                    uint32_t w, uint32_t h,
                                    uint8_t *data, size_t bytes)
{
    if (g_pending_count >= GLYPH_PENDING_CAP) {
        /* Pathological: more than GLYPH_PENDING_CAP cache misses in a single
         * frame. Flush what we have so far to make room; the caller's slot is
         * still recorded in the hash table, just without pixels until the
         * next frame's draw re-requests it via a hit (which is impossible,
         * since the slot is already occupied) — so we explicitly leave the
         * slot non-drawable by returning false. */
        typio_log_debug("glyph_atlas: pending queue overflow (%u); flushing early",
                        g_pending_count);
        glyph_atlas_flush();
    }
    if (g_pending_count >= GLYPH_PENDING_CAP) {
        free(data);
        return false;
    }
    PendingUpload *p = &g_pending[g_pending_count++];
    p->data       = data;
    p->slot_index = slot_index;
    p->x = x; p->y = y; p->w = w; p->h = h;
    p->bytes      = bytes;
    return true;
}

uint32_t glyph_atlas_pending_count(void)
{
    return g_pending_count;
}

void glyph_atlas_set_upload_fn(GlyphAtlasUploadFn fn, void *user)
{
    g_upload_fn   = fn ? fn : default_upload_fn;
    g_upload_user = fn ? user : NULL;
}

void glyph_atlas_test_init_slots(void)
{
    if (g_atlas.slots) return;
    /* Allocate the hash-slot array only; no atlas image, no GPU. Matches the
     * slot-only fields a CPU test exercises (the .drawable flag and the
     * .key/.occupied bookkeeping). */
    g_atlas.slots = (GlyphSlot *)calloc(GLYPH_SLOT_CAP, sizeof(GlyphSlot));
}

void glyph_atlas_test_reset(void)
{
    for (uint32_t k = 0; k < g_pending_count; ++k) free(g_pending[k].data);
    g_pending_count = 0;
    free(g_atlas.slots);
    /* Keep the image pointer intact if there is one — we don't own the device
     * here. Tests that initialised via test_init_slots never set g_atlas.image,
     * so this leaves g_atlas in the same state test_init_slots started from. */
    g_atlas.slots = NULL;
    g_atlas.live_count = 0;
    g_atlas.packer_exhausted = false;
    g_atlas.packer = (GlyphPacker){0};
    g_upload_fn = default_upload_fn;
    g_upload_user = NULL;
}

/* Mark the slot at @idx non-drawable, NULL-safe: tests can run without ever
 * allocating g_atlas.slots (queue-only tests), in which case there is nothing
 * to mark. */
static void mark_slot_non_drawable(uint32_t idx)
{
    if (g_atlas.slots && idx < GLYPH_SLOT_CAP) {
        g_atlas.slots[idx].drawable = false;
    }
}

void glyph_atlas_test_set_drawable(uint32_t idx, bool drawable)
{
    if (g_atlas.slots && idx < GLYPH_SLOT_CAP) {
        g_atlas.slots[idx].drawable = drawable;
    }
}

bool glyph_atlas_test_get_drawable(uint32_t idx)
{
    if (g_atlas.slots && idx < GLYPH_SLOT_CAP) {
        return g_atlas.slots[idx].drawable;
    }
    return false;
}

const GlyphSlot *glyph_atlas_get(uint32_t font_id, FT_Face face, uint32_t glyph_id,
                                  float size_px, int32_t weight)
{
    if (!glyph_atlas_ensure()) return NULL;

    uint64_t key  = ((uint64_t)font_id << 32) | glyph_id;
    uint32_t mask = GLYPH_SLOT_CAP - 1u;
    uint32_t i    = (uint32_t)(key * 1099511628211ULL) & mask;

    /* Bounded linear probe: stop at the first empty slot (insertion point) or a
     * key match (hit). The bound makes a (pathologically) full table return
     * "skip this glyph" instead of spinning forever. */
    uint32_t probes = 0;
    while (g_atlas.slots[i].occupied) {
        if (g_atlas.slots[i].key == key) return &g_atlas.slots[i];
        if (++probes >= GLYPH_SLOT_CAP) return NULL;   /* table full, key absent */
        i = (i + 1u) & mask;
    }

    /* Miss — rasterise, pack, queue the upload. The probe stopped at the empty
     * insertion slot `i`; nothing mutates the table before we write it. */
    GlyphSlot slot = { .key = key, .occupied = true, .drawable = false };

    font_cache_apply(face, size_px, weight);
    if (FT_Load_Glyph(face, glyph_id, FT_LOAD_RENDER | FT_LOAD_TARGET_NORMAL) == 0) {
        FT_GlyphSlot s = face->glyph;
        FT_Bitmap   *b = &s->bitmap;
        slot.left = (int16_t)s->bitmap_left;
        slot.top  = (int16_t)s->bitmap_top;

        uint32_t u, v;
        if (b->width > 0 && b->rows > 0 &&
            glyph_packer_place(&g_atlas.packer, GLYPH_ATLAS_DIM, GLYPH_ATLAS_PAD,
                               b->width, b->rows, &u, &v)) {
            /* Tighten the pitch-padded FreeType bitmap so the staging copy is
             * contiguous, then queue the upload (the actual vkQueueSubmit is
             * deferred to glyph_atlas_flush). The slot is marked drawable now
             * because placement has succeeded; if the flush later fails, the
             * flush path will mark it non-drawable to skip the draw. */
            uint8_t *tight = (uint8_t *)malloc((size_t)b->width * b->rows);
            if (tight) {
                for (uint32_t row = 0; row < b->rows; ++row)
                    memcpy(tight + (size_t)row * b->width,
                           b->buffer + (size_t)row * (size_t)b->pitch, b->width);
                if (glyph_atlas_test_push_pending(i, u, v, b->width, b->rows, tight,
                                   (size_t)b->width * b->rows)) {
                    slot.u = (uint16_t)u;        slot.v = (uint16_t)v;
                    slot.w = (uint16_t)b->width; slot.h = (uint16_t)b->rows;
                    slot.drawable = true;
                }
            }
        } else if (b->width > 0 && b->rows > 0 &&
                   b->width + GLYPH_ATLAS_PAD <= GLYPH_ATLAS_DIM &&
                   b->rows  + GLYPH_ATLAS_PAD <= GLYPH_ATLAS_DIM) {
            /* The glyph fits an empty atlas but not the current one: the shelf
             * packer is exhausted. Flag it so the next reclaim checkpoint
             * rebuilds the atlas and reclaims the space (the never-fits case —
             * a glyph larger than the atlas — is excluded so it cannot thrash
             * the rebuild). */
            if (!g_atlas.packer_exhausted) {
                typio_log_debug("Glyph atlas full: %ux%u glyph did not fit "
                                "(live=%u); flagged for reclaim",
                                b->width, b->rows, g_atlas.live_count);
            }
            g_atlas.packer_exhausted = true;
        }
    }

    g_atlas.slots[i] = slot;
    g_atlas.live_count++;
    g_glyphs_rasterized++;
    return &g_atlas.slots[i];
}

bool glyph_atlas_flush(void)
{
    if (g_pending_count == 0) return true;

    /* Build the batched region array from the pending queue. The regions
     * reference each PendingUpload's @data buffer; that is safe because the
     * queue is not mutated during the upload call. */
    GlyphUploadRegion *regions =
        (GlyphUploadRegion *)calloc(g_pending_count, sizeof(GlyphUploadRegion));
    bool ok = false;
    if (regions) {
        for (uint32_t k = 0; k < g_pending_count; ++k) {
            regions[k] = (GlyphUploadRegion){
                .x = g_pending[k].x, .y = g_pending[k].y,
                .w = g_pending[k].w, .h = g_pending[k].h,
                .data = g_pending[k].data, .bytes = g_pending[k].bytes,
            };
        }
        /* Default upload fn calls glyph_upload_regions(glyph_atlas_image(), ...);
         * a test injects a counting / failure-simulating stub via
         * glyph_atlas_set_upload_fn. The image acquisition is the fn's
         * responsibility so a CPU-only test never needs a GPU. */
        ok = g_upload_fn(regions, g_pending_count, g_upload_user);
        free(regions);
    }

    if (!ok) {
        /* Flush failed (fence timeout, OOM, no device, or a test stub
         * simulating failure). Mark every affected slot non-drawable so the
         * in-progress render pass skips them instead of sampling whatever
         * bytes happened to be in the staging buffer. The slot is now
         * permanently recorded as occupied+non-drawable in the hash table,
         * so future lookups return the cached non-drawable slot and skip
         * re-rasterisation. That is the correct degradation: the panel shows
         * a hole (rather than retrying every frame and re-triggering the
         * failed upload), and the next atlas reclaim re-builds clean. */
        typio_log_warning("glyph_atlas: flush failed for %u regions; marking "
                          "non-drawable", g_pending_count);
        for (uint32_t k = 0; k < g_pending_count; ++k) {
            mark_slot_non_drawable(g_pending[k].slot_index);
        }
    }

    for (uint32_t k = 0; k < g_pending_count; ++k) free(g_pending[k].data);
    /* Track batch-size stats so the slow-render log can distinguish a steady
     * warm-atlas flush (0 regions) from a post-reclaim re-warm (dozens). */
    if (g_pending_count > 0) {
        g_flush_count++;
        g_flush_total_regions += g_pending_count;
        if (g_pending_count > g_flush_peak_batch) g_flush_peak_batch = g_pending_count;
    }
    g_pending_count = 0;
    return ok;
}
