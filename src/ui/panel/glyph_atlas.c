/**
 * @file glyph_atlas.c
 * @brief Shared glyph coverage atlas + reclamation (see header).
 */
#include "glyph_atlas.h"
#include "glyph_upload.h"
#include "glyph_pack.h"
#include "font_cache.h"
#include "device.h"

#include <typio/abi/log.h>

#include <stdlib.h>
#include <string.h>

#define GLYPH_ATLAS_PAD   1u      /* transparent gutter to stop bilinear bleed */
#define GLYPH_SLOT_CAP    131072u /* power of two; 4× the atlas image capacity
                                   * so the hash table stays well below the 75%
                                   * reclaim threshold in normal use            */
#define ATLAS_RECLAIM_THRESHOLD_PCT 75

typedef struct {
    flux_image  *image;
    GlyphSlot   *slots;     /* GLYPH_SLOT_CAP entries                          */
    GlyphPacker  packer;    /* skyline shelf cursor (see glyph_pack.h)         */
    uint32_t     live_count;/* occupied entries                                */
    bool         packer_exhausted; /* image ran out of shelf space (resettable) */
} GlyphAtlas;

static GlyphAtlas g_atlas;

/* Cumulative diagnostics counters (session-wide). */
static uint64_t g_atlas_rebuilds;
static uint64_t g_glyphs_rasterized;

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

bool glyph_atlas_reclaim(void)
{
    if (!g_atlas.slots) return false;

    /* Only reclaim when packer is exhausted (texture full). The hash table
     * load factor is a secondary concern — with 131072 slots and ~3000 unique
     * CJK glyphs, the table is well below 75% even after hours of use.
     * Packer exhaustion is the real signal that the texture needs rebuilding.
     *
     * This is called from the panel render path; triggering reclaim here
     * causes a full GPU drain via flux_device_wait_idle. To avoid stalling
     * every frame when the atlas is near capacity, we only trigger once
     * per packer-exhaustion event (the flag is cleared by glyph_atlas_reset). */
    if (!g_atlas.packer_exhausted) return false;

    uint32_t threshold =
        (uint32_t)((uint64_t)GLYPH_SLOT_CAP * ATLAS_RECLAIM_THRESHOLD_PCT / 100);
    typio_log_debug("Glyph atlas reclaim: rebuild (live=%u/%u, reason=%s)",
                    g_atlas.live_count, (unsigned)GLYPH_SLOT_CAP,
                    g_atlas.live_count >= threshold ? "load+packer" : "image-full");
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
}

void glyph_atlas_shutdown(void)
{
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

    /* Miss — rasterise, pack, upload the glyph's sub-rect. The probe stopped at
     * the empty insertion slot `i`; nothing mutates the table before we write
     * it (overflow inserts a non-drawable slot, but never resets the table). */
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
            /* Tightly repack the (pitch-padded) FreeType bitmap. */
            uint8_t *tight = (uint8_t *)malloc((size_t)b->width * b->rows);
            if (tight) {
                for (uint32_t row = 0; row < b->rows; ++row)
                    memcpy(tight + (size_t)row * b->width,
                           b->buffer + (size_t)row * (size_t)b->pitch, b->width);
                if (glyph_upload_region(g_atlas.image, u, v, b->width, b->rows,
                                         tight, (size_t)b->width * b->rows)) {
                    slot.u = (uint16_t)u;        slot.v = (uint16_t)v;
                    slot.w = (uint16_t)b->width; slot.h = (uint16_t)b->rows;
                    slot.drawable = true;
                }
                free(tight);
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
