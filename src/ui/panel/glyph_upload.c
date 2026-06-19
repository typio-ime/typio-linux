/**
 * @file glyph_upload.c
 * @brief Persistent Vulkan staging context for glyph uploads (see header).
 */
#define VK_USE_PLATFORM_WAYLAND_KHR
#include "glyph_upload.h"
#include "device.h"

#include <flux/vulkan.h>

#include <stdlib.h>
#include <string.h>

#define UPLOAD_STAGING_INITIAL  (16u * 1024u)  /* 16 KiB — covers most CJK glyphs */

typedef struct {
    VkCommandPool   pool;
    VkCommandBuffer cmd;
    VkFence         fence;
    VkBuffer        staging;
    void           *staging_mapped;
    VkDeviceSize    staging_size;
    VkDeviceMemory  staging_memory;
    bool            initialized;
} GlyphUploadCtx;

static GlyphUploadCtx g_upload_ctx;

void glyph_upload_shutdown(void)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev || !ctx->initialized) {
        ctx->initialized = false;
        return;
    }
    VkDevice vkd = flux_device_vk_device(dev);
    if (ctx->fence)          vkDestroyFence(vkd, ctx->fence, nullptr);
    if (ctx->cmd)            { /* freed with pool */ }
    if (ctx->pool)           vkDestroyCommandPool(vkd, ctx->pool, nullptr);
    if (ctx->staging)        vkDestroyBuffer(vkd, ctx->staging, nullptr);
    if (ctx->staging_memory) vkFreeMemory(vkd, ctx->staging_memory, nullptr);
    *ctx = (GlyphUploadCtx){0};
}

static uint32_t find_host_visible_mem_type(flux_device *dev, uint32_t type_bits)
{
    VkPhysicalDevice phys = flux_device_vk_physical_device(dev);
    VkPhysicalDeviceMemoryProperties mp;
    vkGetPhysicalDeviceMemoryProperties(phys, &mp);

    uint32_t wanted = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                      VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (uint32_t i = 0; i < mp.memoryTypeCount; ++i) {
        if ((type_bits & (1u << i)) &&
            (mp.memoryTypes[i].propertyFlags & wanted) == wanted) {
            return i;
        }
    }
    return UINT32_MAX;
}

/* Allocate + map a host-visible staging buffer of @size into the context. */
static bool staging_alloc(GlyphUploadCtx *ctx, flux_device *dev, VkDeviceSize size)
{
    VkDevice vkd = flux_device_vk_device(dev);

    ctx->staging_size = size;
    VkBufferCreateInfo bci = {
        .sType       = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
        .size        = size,
        .usage       = VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
        .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    };
    if (vkCreateBuffer(vkd, &bci, nullptr, &ctx->staging) != VK_SUCCESS)
        return false;

    VkMemoryRequirements mr;
    vkGetBufferMemoryRequirements(vkd, ctx->staging, &mr);

    uint32_t mem_type = find_host_visible_mem_type(dev, mr.memoryTypeBits);
    if (mem_type == UINT32_MAX) return false;

    VkMemoryAllocateInfo mai = {
        .sType           = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize  = mr.size,
        .memoryTypeIndex = mem_type,
    };
    if (vkAllocateMemory(vkd, &mai, nullptr, &ctx->staging_memory) != VK_SUCCESS)
        return false;
    if (vkBindBufferMemory(vkd, ctx->staging, ctx->staging_memory, 0) != VK_SUCCESS)
        return false;
    if (vkMapMemory(vkd, ctx->staging_memory, 0, VK_WHOLE_SIZE, 0,
                    &ctx->staging_mapped) != VK_SUCCESS)
        return false;
    return true;
}

static bool glyph_upload_ctx_ensure(void)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    if (ctx->initialized) return true;

    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);
    uint32_t gfx_family = flux_device_vk_graphics_family(dev);

    VkCommandPoolCreateInfo pci = {
        .sType            = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .queueFamilyIndex = gfx_family,
        .flags            = VK_COMMAND_POOL_CREATE_TRANSIENT_BIT |
                            VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    };
    if (vkCreateCommandPool(vkd, &pci, nullptr, &ctx->pool) != VK_SUCCESS)
        goto fail;

    VkCommandBufferAllocateInfo cbai = {
        .sType              = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool        = ctx->pool,
        .level              = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        .commandBufferCount = 1,
    };
    if (vkAllocateCommandBuffers(vkd, &cbai, &ctx->cmd) != VK_SUCCESS)
        goto fail;

    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    if (vkCreateFence(vkd, &fci, nullptr, &ctx->fence) != VK_SUCCESS)
        goto fail;

    if (!staging_alloc(ctx, dev, UPLOAD_STAGING_INITIAL))
        goto fail;

    ctx->initialized = true;
    return true;

fail:
    glyph_upload_shutdown();
    return false;
}

static bool glyph_upload_ctx_grow_staging(VkDeviceSize needed)
{
    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);

    if (ctx->staging_mapped) vkUnmapMemory(vkd, ctx->staging_memory);
    vkDestroyBuffer(vkd, ctx->staging, nullptr);
    vkFreeMemory(vkd, ctx->staging_memory, nullptr);
    ctx->staging        = VK_NULL_HANDLE;
    ctx->staging_memory = VK_NULL_HANDLE;
    ctx->staging_mapped = nullptr;

    VkDeviceSize new_size = ctx->staging_size ? ctx->staging_size : UPLOAD_STAGING_INITIAL;
    while (new_size < needed) new_size *= 2;

    return staging_alloc(ctx, dev, new_size);
}

/* Record one copy + the surrounding layout transitions into @cmd for the
 * destination image @dst. The first call between Begin/End must precede the
 * barrier pair with the to_dst barrier; the last must follow it with to_shader.
 * For the batched path we record one to_dst barrier up front, then all copies,
 * then one to_shader barrier at the end — minimising pipeline barriers per
 * upload to two regardless of region count. */
static void record_to_dst_barrier(VkCommandBuffer cmd, VkImage dst)
{
    VkImageMemoryBarrier2 b = {
        .sType         = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        .srcStageMask  = VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        .srcAccessMask = VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        .dstStageMask  = VK_PIPELINE_STAGE_2_COPY_BIT,
        .dstAccessMask = VK_ACCESS_2_TRANSFER_WRITE_BIT,
        .oldLayout     = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        .newLayout     = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .image         = dst,
        .subresourceRange = {
            .aspectMask = VK_IMAGE_ASPECT_COLOR_BIT,
            .levelCount = 1, .layerCount = 1,
        },
    };
    VkDependencyInfo di = {
        .sType = VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        .imageMemoryBarrierCount = 1,
        .pImageMemoryBarriers = &b,
    };
    vkCmdPipelineBarrier2(cmd, &di);
}

static void record_to_shader_barrier(VkCommandBuffer cmd, VkImage dst)
{
    VkImageMemoryBarrier2 b = {
        .sType         = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2,
        .srcStageMask  = VK_PIPELINE_STAGE_2_COPY_BIT,
        .srcAccessMask = VK_ACCESS_2_TRANSFER_WRITE_BIT,
        .dstStageMask  = VK_PIPELINE_STAGE_2_FRAGMENT_SHADER_BIT,
        .dstAccessMask = VK_ACCESS_2_SHADER_SAMPLED_READ_BIT,
        .oldLayout     = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        .newLayout     = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        .image         = dst,
        .subresourceRange = {
            .aspectMask = VK_IMAGE_ASPECT_COLOR_BIT,
            .levelCount = 1, .layerCount = 1,
        },
    };
    VkDependencyInfo di = {
        .sType = VK_STRUCTURE_TYPE_DEPENDENCY_INFO,
        .imageMemoryBarrierCount = 1,
        .pImageMemoryBarriers = &b,
    };
    vkCmdPipelineBarrier2(cmd, &di);
}

/* The internal entry point shared by the single-region and batched APIs.
 * Copies @count regions from @ctx->staging (at successive offsets) into @img
 * in ONE command buffer, ONE submit, ONE fence wait. */
static bool upload_batch(flux_image *img, const GlyphUploadRegion *regions, size_t count)
{
    if (count == 0) return true;

    GlyphUploadCtx *ctx = &g_upload_ctx;
    flux_device *dev = typio_render_device_get();
    if (!dev) return false;

    VkDevice vkd = flux_device_vk_device(dev);
    VkQueue  gfx_queue = flux_device_vk_graphics_queue(dev);

    /* Sum the byte total and ensure the staging buffer can hold every region
     * back-to-back. Growing is rare: the initial 16 KiB staging already covers
     * ~30 average CJK glyphs, and after the first growth the buffer is sized
     * for the worst-case frame forever. */
    VkDeviceSize total_bytes = 0;
    for (size_t i = 0; i < count; ++i) {
        total_bytes += (VkDeviceSize)regions[i].bytes;
    }
    if (total_bytes > ctx->staging_size) {
        if (!glyph_upload_ctx_grow_staging(total_bytes))
            return false;
    }

    /* Copy each region's source into the staging buffer at successive offsets.
     * The copies are byte-aligned because the atlas is R8 (1 byte/texel). */
    uint8_t *staging_base = (uint8_t *)ctx->staging_mapped;
    VkDeviceSize offset = 0;
    VkBufferImageCopy *copies =
        (VkBufferImageCopy *)calloc(count, sizeof(VkBufferImageCopy));
    if (!copies) return false;
    for (size_t i = 0; i < count; ++i) {
        memcpy(staging_base + offset, regions[i].data, regions[i].bytes);
        copies[i] = (VkBufferImageCopy){
            .bufferOffset = offset,
            .imageSubresource = {
                .aspectMask     = VK_IMAGE_ASPECT_COLOR_BIT,
                .mipLevel       = 0,
                .baseArrayLayer = 0, .layerCount = 1,
            },
            .imageOffset = { (int32_t)regions[i].x, (int32_t)regions[i].y, 0 },
            .imageExtent = { regions[i].w, regions[i].h, 1 },
        };
        offset += (VkDeviceSize)regions[i].bytes;
    }

    vkResetCommandPool(vkd, ctx->pool, 0);

    VkCommandBufferBeginInfo cbbi = {
        .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
    };
    vkBeginCommandBuffer(ctx->cmd, &cbbi);

    VkImage dst = flux_image_vk_image(img);
    record_to_dst_barrier(ctx->cmd, dst);
    vkCmdCopyBufferToImage(ctx->cmd, ctx->staging, dst,
                            VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                            (uint32_t)count, copies);
    record_to_shader_barrier(ctx->cmd, dst);

    vkEndCommandBuffer(ctx->cmd);
    free(copies);

    vkResetFences(vkd, 1, &ctx->fence);

    VkSubmitInfo si = {
        .sType              = VK_STRUCTURE_TYPE_SUBMIT_INFO,
        .commandBufferCount = 1,
        .pCommandBuffers    = &ctx->cmd,
    };
    if (vkQueueSubmit(gfx_queue, 1, &si, ctx->fence) != VK_SUCCESS)
        return false;

    /* Finite timeout: if the GPU is stalled (driver hang, memory pressure),
     * skip this batch rather than freezing the event loop indefinitely.
     * 100ms is generous for a small atlas upload even with hundreds of
     * regions. */
    VkResult wr = vkWaitForFences(vkd, 1, &ctx->fence, VK_TRUE,
                                   100ull * 1000ull * 1000ull);
    if (wr != VK_SUCCESS) return false;
    return true;
}

bool glyph_upload_regions(flux_image *img,
                          const GlyphUploadRegion *regions, size_t count)
{
    if (!img || !regions || count == 0) return true;
    if (!glyph_upload_ctx_ensure()) return false;
    return upload_batch(img, regions, count);
}

bool glyph_upload_region(flux_image *img,
                         uint32_t x, uint32_t y, uint32_t w, uint32_t h,
                         const void *data, size_t bytes)
{
    if (!glyph_upload_ctx_ensure()) return false;

    GlyphUploadRegion region = {
        .x = x, .y = y, .w = w, .h = h,
        .data = data, .bytes = bytes,
    };
    return upload_batch(img, &region, 1);
}
