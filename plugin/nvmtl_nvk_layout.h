/* nvmtl_nvk_layout.h - the installed NVK's private object layout, for the device-loss report only.

   The texture header (TIC) a storage write goes through lives in NVK's device image table; the heap descriptor set
   element names it by index, and the image view keeps its own copy and index. None of that is reachable through
   Vulkan. These offsets were measured with offsetof() on the exact tree the installed ICD was built from (the one
   whose LC_UUID is below), and the reader refuses to run against any other ICD. Pure decode of the texture header
   bit fields (Maxwell+ TEXHEAD_V2_BL: address 31:9 and 56:32, gob heights 64-72, max mip 95:92, components 118:112,
   version 127:124, width 144:128, type 153:150, height 176:160, depth 191:177) is in nvmtl_tic_decode, which
   tools/test_slots.c proves against hand-built words. */
#ifndef NVMTL_NVK_LAYOUT_H
#define NVMTL_NVK_LAYOUT_H
#include <stdint.h>
#include <stdio.h>

#define NVMTL_NVK_LAYOUT_UUID "8E640FDA-5389-3EFD-8177-EE5A14956359"   /* libvulkan_nouveau.dylib built 2026-11-01 16:23 from the speed267 tree */
enum {
    NVK_L_VIEW_PLANE_COUNT = 161, NVK_L_VIEW_SAMPLED_DESC = 168, NVK_L_VIEW_SAMPLED_INDEX = 200, NVK_L_VIEW_STORAGE_DESC = 204, NVK_L_VIEW_STORAGE_INDEX = 236, NVK_L_VIEW_SIZE = 392,
    NVK_L_DEV_IMAGES = 28640, NVK_L_TABLE_DESC_SIZE = 392, NVK_L_TABLE_ALLOC = 396,
    NVK_L_ARENA_CONTIG_VA = 96, NVK_L_ARENA_MEM_COUNT = 108, NVK_L_ARENA_MEM = 120, NVK_L_ARENA_MEM_STRIDE = 16, NVK_L_ARENA_MIN_LOG2 = 16,
    NVK_L_NVKMD_MEM_MAP = 160, NVK_L_NVKMD_VA_ADDR = 24,
    NVK_L_SET_POOL = 64, NVK_L_SET_LAYOUT = 88, NVK_L_SET_MAP = 96, NVK_L_SET_MEM_OFFSET = 104, NVK_L_POOL_MEM = 88, NVK_L_NVKMD_MEM_VA = 56, NVK_L_LAYOUT_BINDING = 152, NVK_L_BINDING_SIZE = 32, NVK_L_BINDING_OFFSET = 12, NVK_L_BINDING_STRIDE = 16, NVK_L_BINDING_ARRAY_SIZE = 8,
    NVK_L_TIC_BYTES = 32,
    NVK_L_QUEUE_IMAGES_ALLOC = 624, NVK_L_QUEUE_SAMPLERS_ALLOC = 628,   /* nvk_queue.state.images/samplers.alloc_count: what the queue last programmed as the pools' sizes */
    NVK_L_NVKMD_MEM_SIZE = 48, NVK_L_SET_SIZE = 112
};

typedef struct { uint64_t addr; uint32_t width, height, depth, type, max_mip, gob_w, gob_h, gob_d, components, version; } nvmtl_tic;
static inline void nvmtl_tic_decode(const uint32_t w[8], nvmtl_tic *t)
{
    t->addr = ((uint64_t)(w[1] & 0x1FFFFFFu) << 32) | (w[0] & 0xFFFFFE00u);
    t->gob_w = w[2] & 7u; t->gob_h = (w[2] >> 3) & 7u; t->gob_d = (w[2] >> 6) & 7u; t->max_mip = (w[2] >> 28) & 0xFu;
    t->components = (w[3] >> 16) & 0x7Fu; t->version = (w[3] >> 28) & 0xFu;
    t->width = (w[4] & 0x1FFFFu) + 1; t->type = (w[4] >> 22) & 0xFu;
    t->height = (w[5] & 0x1FFFFu) + 1; t->depth = ((w[5] >> 17) & 0x7FFFu) + 1;
}
static inline const char *nvmtl_tic_type_name(uint32_t t)
{
    static const char *const n[16] = { "1D", "2D", "3D", "CUBE", "1D_ARRAY", "2D_ARRAY", "1D_BUFFER", "2D_NO_MIP", "CUBE_ARRAY", "?9", "HTEX_2D", "HTEX_3D", "?12", "?13", "HTEX_2D_ARRAY", "?15" };
    return n[t & 15];
}
/* the byte offset inside the contiguous arena's memory list for a table offset: which memory, and where in it */
static inline uint32_t nvmtl_nvk_arena_mem_index(uint64_t arena_offset)
{
    const uint64_t q = arena_offset >> (NVK_L_ARENA_MIN_LOG2 - 1);
    uint32_t i = 0; while ((q >> (i + 1)) != 0) i++;   /* floor(log2(q)); q == 0 -> 0 */
    return q ? i : 0;
}
static inline uint64_t nvmtl_nvk_arena_mem_offset(uint32_t mem_idx) { return mem_idx == 0 ? 0 : (uint64_t)(1u << (NVK_L_ARENA_MIN_LOG2 - 1)) << mem_idx; }
#endif
