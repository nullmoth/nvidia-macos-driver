/* SPDX-License-Identifier: MIT */
/*
 * A cursor over the memory behind a TTM resource or a DMA scatter list, in contiguous pieces.
 *
 * Written for this port from the interface xe uses (Linux's own version of this file is GPL-2.0 and is not used).
 * At any point the cursor describes one physically contiguous piece:
 *   start      VRAM/stolen: offset of the piece in the region; scatter list: offset into the current segment;
 *              any other resource: offset in the buffer
 *   size       bytes left in this piece (never more than 'remaining')
 *   remaining  bytes left in the whole walk
 *   mem_type   the TTM placement the memory is in
 * xe_res_dma() gives the address the GPU uses for the current position.
 */
#ifndef _XE_RES_CURSOR_H_
#define _XE_RES_CURSOR_H_

#include <linux/scatterlist.h>
#include <linux/list.h>

#include <drm/drm_buddy.h>
#include <drm/ttm/ttm_placement.h>
#include <drm/ttm/ttm_resource.h>

#include "xe_bo.h"
#include "xe_device.h"
#include "xe_ttm_vram_mgr.h"

struct xe_res_cursor {
	u64 start;
	u64 size;
	u64 remaining;
	u32 mem_type;
	/* VRAM/stolen: the buddy block the piece is in, and the allocator that sized it */
	struct drm_buddy_block *block;
	struct list_head *blocks;
	struct drm_buddy *mm;
	/* scatter list: the DMA segment the piece is in */
	struct scatterlist *sgl;
};

/*
 * VRAM0, VRAM1 and stolen memory are allocated by xe's buddy-based VRAM manager (stolen is one too, built on it);
 * xe_bo.h names them XE_PL_VRAM0, XE_PL_VRAM1 and XE_PL_STOLEN, which are these TTM numbers.
 */
static inline bool xe_res_is_buddy(u32 mem_type)
{
	return mem_type == TTM_PL_VRAM || mem_type == TTM_PL_VRAM + 1 || mem_type == TTM_NUM_MEM_TYPES - 1;
}

static inline void xe_res_linear(u64 start, u64 size, struct xe_res_cursor *cur)
{
	cur->start = start;
	cur->size = size;
	cur->remaining = size;
	cur->block = NULL;
	cur->blocks = NULL;
	cur->mm = NULL;
	cur->sgl = NULL;
}

/* position a buddy cursor 'offset' bytes past the start of 'block' and the blocks after it */
static inline void xe_res_buddy_seek(struct xe_res_cursor *cur, struct drm_buddy_block *block, u64 offset)
{
	for (;;) {
		u64 bsize = drm_buddy_block_size(cur->mm, block);
		if (offset < bsize || list_is_last(&block->link, cur->blocks)) {
			cur->block = block;
			cur->start = drm_buddy_block_offset(block) + offset;
			cur->size = min_t(u64, bsize - min_t(u64, offset, bsize), cur->remaining);
			return;
		}
		offset -= bsize;
		block = list_next_entry(block, link);
	}
}

/* start a walk of 'size' bytes at byte 'start' of the memory behind 'res' (size 0 positions without walking) */
static inline void xe_res_first(struct ttm_resource *res, u64 start, u64 size, struct xe_res_cursor *cur)
{
	if (res == NULL) {
		xe_res_linear(start, size, cur);
		cur->mem_type = TTM_PL_SYSTEM;
		return;
	}
	xe_res_linear(start, size, cur);
	cur->mem_type = res->mem_type;
	if (xe_res_is_buddy(res->mem_type)) {
		struct xe_ttm_vram_mgr_resource *vres = to_xe_ttm_vram_mgr_resource(res);
		struct ttm_resource_manager *man = ttm_manager_type(res->bo->bdev, res->mem_type);

		if (list_empty(&vres->blocks))
			return;
		cur->blocks = &vres->blocks;
		cur->mm = &to_xe_ttm_vram_mgr(man)->mm;
		xe_res_buddy_seek(cur, list_first_entry(&vres->blocks, struct drm_buddy_block, link), start);
	}
}

/* position 'cur' at byte 'offset' of the DMA segments starting at 'sg' */
static inline void xe_res_sg_seek(struct xe_res_cursor *cur, struct scatterlist *sg, u64 offset)
{
	while (sg != NULL && offset >= sg_dma_len(sg)) {
		offset -= sg_dma_len(sg);
		if (sg_is_last(sg))
			break;
		sg = sg_next(sg);
	}
	cur->sgl = sg;
	cur->start = offset;
	cur->size = sg != NULL ? min_t(u64, sg_dma_len(sg) - min_t(u64, offset, sg_dma_len(sg)), cur->remaining) : 0;
}

/* start a walk of 'size' bytes at byte 'start' of a DMA-mapped scatter list */
static inline void xe_res_first_sg(const struct sg_table *sg, u64 start, u64 size, struct xe_res_cursor *cur)
{
	xe_res_linear(start, size, cur);
	cur->mem_type = TTM_PL_TT;
	xe_res_sg_seek(cur, sg->sgl, start);
}

/* advance 'size' bytes, into the next block or segment when this piece ends */
static inline void xe_res_next(struct xe_res_cursor *cur, u64 size)
{
	if (size > cur->remaining)
		size = cur->remaining;
	cur->remaining -= size;
	if (cur->remaining == 0) {
		cur->size = 0;
		return;
	}
	if (size < cur->size) {
		cur->start += size;
		cur->size -= size;
		return;
	}
	if (cur->sgl != NULL) {
		xe_res_sg_seek(cur, cur->sgl, cur->start + size);
	} else if (cur->block != NULL) {
		u64 into = cur->start - drm_buddy_block_offset(cur->block);
		xe_res_buddy_seek(cur, cur->block, into + size);
	} else {
		cur->start += size;
		cur->size = cur->remaining;
	}
}

/* the GPU address of the current position: the segment's DMA address, or the region offset for VRAM/stolen */
static inline u64 xe_res_dma(const struct xe_res_cursor *cur)
{
	if (cur->sgl != NULL)
		return sg_dma_address(cur->sgl) + cur->start;
	return cur->start;
}

#endif
