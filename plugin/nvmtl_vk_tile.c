/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * The Vulkan side of tile shading. Included into the translation unit after nvmtl_vk.c, so it sees that file's statics.
 *
 * There is no tile memory on this hardware. A tile function runs as a compute dispatch between two parts of a render
 * pass: the pass is ended, the colour attachments are bound as storage images (nvmtl_vk_tile_bind_plane), one workgroup
 * runs per tile, and the pass is resumed with every attachment loaded. nvk_cmdbuf bindings that the tile function
 * overwrites belong to the draws around it, so they are saved before and restored after (nvmtl_vk_tile_table_*).
 */

/* The binding table's vertex-stage slots, copied. NULL when the command buffer has no table yet. */
void *nvmtl_vk_tile_table_save(nvk_cmdbuf *c)
{
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    nvmtl_slot *copy = (nvmtl_slot *)calloc(NVMTL_NBIND, sizeof *copy);
    if (!copy) return NULL;
    if (t) memcpy(copy, t->s[0], NVMTL_NBIND * sizeof *copy);
    return copy;
}

/* Put the slots back and mark every one that differs as changed, so the next descriptor set rewrites it. Frees `saved`. */
void nvmtl_vk_tile_table_restore(nvk_cmdbuf *c, void *saved)
{
    nvmtl_slot *old = (nvmtl_slot *)saved;
    nvmtl_bindtable *t = (nvmtl_bindtable *)c->table;
    if (!old || !t) { free(old); return; }
    int changed = 0;
    for (uint32_t i = 0; i < NVMTL_NBIND; i++) {
        nvmtl_slot *cur = &t->s[0][i];
        if (cur->kind == old[i].kind && cur->h == old[i].h && cur->off == old[i].off && cur->size == old[i].size &&
            cur->address == old[i].address) continue;
        *cur = old[i];
        t->dirty[0] = 1; t->chg[0][i >> 6] |= 1ull << (i & 63); changed = 1;
    }
    if (changed) t->texgen[0]++;
    free(old);
}

/* Bind `view` as storage image `index` of the vertex set (the set a compute pipeline reads). */
int nvmtl_vk_tile_bind_plane(nvk_cmdbuf *c, uint32_t index, void *view)
{
    return nvmtl_vk_bind_storage_view(c, 0, index, view);
}

/* An image rests in TRANSFER_SRC_OPTIMAL between render passes (the layout every pass begins and ends in). */
int nvmtl_vk_tile_image_rest(nvk_cmdbuf *c, nvk_image *img)
{
    if (!c || !img || c->in_rp) return -1;
    img_to(c, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
           VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT);
    return 0;
}

/* The tile block after the dispatch region: tile width, tile height, tiles across, tile memory words per tile. */
void nvmtl_vk_tile_push(nvk_cmdbuf *c, nvk_pipeline *p, uint32_t tile_w, uint32_t tile_h, uint32_t tiles_across, uint32_t tile_words)
{
    const uint32_t block[4] = { tile_w, tile_h, tiles_across, tile_words };
    pvkCmdPushConstants(c->cb, (VkPipelineLayout)p->layout, VK_SHADER_STAGE_COMPUTE_BIT, NVMTL_DISPATCH_PC_BYTES, sizeof block, block);
}

/* Build a compute pipeline whose layout carries the tile block. The scope also keeps address lowering SAFE. */
int nvmtl_vk_tile_pipeline_create(const void *cs, size_t n, nvk_pipeline *out)
{
    g_tile_build++;
    int rc = nvmtl_vk_compute_pipeline_create(cs, n, out);
    g_tile_build--;
    return rc;
}
void nvmtl_vk_tile_scope(int on) { if (on) g_tile_build++; else if (g_tile_build) g_tile_build--; }

/* A zeroed image to keep tile planes in: the render pass's own format when storage-capable, else a stand-in. */
int nvmtl_vk_tile_plane_create(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, nvk_image *out)
{
    int rc = nvmtl_vk_image_create_typed(w, h, vkfmt, bpp, 0, 1, 1, 2, 1, out);
    if (rc) return rc;
    if (!out->storage) { nvmtl_vk_image_destroy(out); memset(out, 0, sizeof *out); return -2; }
    return 0;
}
int nvmtl_vk_tile_format_is_storage(uint32_t vkfmt) { return nvmtl_vk_format_is_storage(vkfmt); }
