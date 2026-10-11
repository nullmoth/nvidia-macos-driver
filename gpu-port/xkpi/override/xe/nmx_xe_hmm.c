// SPDX-License-Identifier: MIT
/*
 * xe userptr on macOS: the process's pages behind a userptr VMA, pinned and DMA-mapped for the GPU.
 *
 * Intel's xe_hmm.c faults the range in with Linux HMM (hmm_range_fault) and relies on MMU notifiers to learn when
 * the CPU mapping changes, holding no pin. xnu has neither, so this version pins the pages instead
 * (get_user_pages_remote -> the port's IOMemoryDescriptor wiring) and keeps them pinned until the sg table is freed:
 * the GPU can never see pages the process gave back. The port's notifier sequence never advances (see
 * linux/mmu_notifier.h), so a populated range stays valid until xe frees it, as it does when the VMA is destroyed.
 *
 * The sg table holds one page per entry: the port's records for a process's pages are separate objects, so the
 * entries must not be walked with page-pointer arithmetic as merged runs would be.
 */
#include <linux/mm.h>
#include <linux/scatterlist.h>
#include <linux/dma-mapping.h>
#include <linux/slab.h>

#include "xe_hmm.h"
#include "xe_vm.h"
#include "xe_bo.h"

static enum dma_data_direction userptr_dir(struct xe_vma *vma)
{
	return xe_vma_read_only(vma) ? DMA_TO_DEVICE : DMA_BIDIRECTIONAL;
}

void xe_hmm_userptr_unmap(struct xe_userptr_vma *uvma)
{
	struct xe_userptr *userptr = &uvma->userptr;
	struct xe_vma *vma = &uvma->vma;
	struct xe_device *xe = xe_vma_vm(vma)->xe;

	mutex_lock(&userptr->unmap_mutex);
	if (userptr->sg && userptr->mapped)
		dma_unmap_sgtable(xe->drm.dev, userptr->sg, userptr_dir(vma), 0);
	userptr->mapped = false;
	mutex_unlock(&userptr->unmap_mutex);
}

/* drops the pins: one put per page, one page per entry */
static void userptr_unpin(struct sg_table *st)
{
	struct scatterlist *sg;
	unsigned int i;

	for_each_sg(st->sgl, sg, st->orig_nents, i)
		if (sg_page(sg) != NULL)
			put_page(sg_page(sg));
}

void xe_hmm_userptr_free_sg(struct xe_userptr_vma *uvma)
{
	struct xe_userptr *userptr = &uvma->userptr;

	xe_assert(xe_vma_vm(&uvma->vma)->xe, userptr->sg);
	xe_hmm_userptr_unmap(uvma);
	userptr_unpin(userptr->sg);
	sg_free_table(userptr->sg);
	userptr->sg = NULL;
}

int xe_hmm_userptr_populate_range(struct xe_userptr_vma *uvma, bool is_mm_mmap_locked)
{
	struct xe_userptr *userptr = &uvma->userptr;
	struct xe_vma *vma = &uvma->vma;
	struct xe_vm *vm = xe_vma_vm(vma);
	struct mm_struct *mm = userptr->notifier.mm;
	u64 start = xe_vma_userptr(vma);
	unsigned long npages = xe_vma_size(vma) >> PAGE_SHIFT;
	bool write = !xe_vma_read_only(vma);
	unsigned long seq;
	struct page **pages;
	struct scatterlist *sg;
	unsigned int i;
	long pinned;
	int ret;

	if (vma->gpuva.flags & XE_VMA_DESTROYED)
		return 0;

	/* already populated for the current mapping */
	seq = mmu_interval_read_begin(&userptr->notifier);
	if (seq == userptr->notifier_seq)
		return 0;

	if (userptr->sg)
		xe_hmm_userptr_free_sg(uvma);

	pages = kvmalloc_array(npages, sizeof(*pages), GFP_KERNEL);
	if (!pages)
		return -ENOMEM;

	if (!mmget_not_zero(mm)) {
		ret = -EFAULT;
		goto free_pages;
	}
	pinned = get_user_pages_remote(NULL, mm, start, npages, write ? FOLL_WRITE : 0, pages, NULL);
	mmput(mm);
	if (pinned < 0) {
		ret = pinned;
		goto free_pages;
	}
	if ((unsigned long)pinned != npages) {
		release_pages(pages, pinned);
		ret = -EFAULT;
		goto free_pages;
	}

	ret = sg_alloc_table(&userptr->sgt, npages, GFP_KERNEL);
	if (ret)
		goto unpin;
	for_each_sg(userptr->sgt.sgl, sg, npages, i)
		sg_set_page(sg, pages[i], PAGE_SIZE, 0);

	ret = dma_map_sgtable(vm->xe->drm.dev, &userptr->sgt, write ? DMA_BIDIRECTIONAL : DMA_TO_DEVICE,
			      DMA_ATTR_SKIP_CPU_SYNC | DMA_ATTR_NO_KERNEL_MAPPING);
	if (ret)
		goto free_st;

	ret = down_read_interruptible(&vm->userptr.notifier_lock);
	if (ret)
		goto unmap;
	mutex_lock(&userptr->unmap_mutex);
	userptr->mapped = true;
	mutex_unlock(&userptr->unmap_mutex);
	userptr->sg = &userptr->sgt;
	userptr->notifier_seq = seq;
	up_read(&vm->userptr.notifier_lock);
	kvfree(pages);
	return 0;

unmap:
	dma_unmap_sgtable(vm->xe->drm.dev, &userptr->sgt, write ? DMA_BIDIRECTIONAL : DMA_TO_DEVICE, 0);
free_st:
	sg_free_table(&userptr->sgt);
unpin:
	release_pages(pages, npages);
free_pages:
	kvfree(pages);
	return ret;
}
