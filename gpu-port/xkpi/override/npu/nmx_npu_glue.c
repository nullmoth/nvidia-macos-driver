// SPDX-License-Identifier: GPL-2.0
/*
 * NMXNPU's own pieces of glue: what the NPU kext needs that NMXGPU's layer objects get from GPU-only files it does not
 * link (the display bridge, xe's helpers), and the GEM page helpers drm-kmod leaves to Linux.
 */
#include <linux/mm.h>
#include <linux/mutex.h>
#include <linux/shmem_fs.h>
#include <linux/slab.h>
#include <linux/swap.h>
#include <drm/drm_gem.h>
#include <drm/drm_managed.h>

/* the display bridge's pre-attach hook (nmx_port.c): an NPU has no display */
void nmx_fb_before_attach(void)
{
}

/* drmm_mutex_init's release (nmx_xe_compat.h) */
void nmx_drmm_mutex_release(struct drm_device *dev, void *lock)
{
	mutex_destroy(lock);
}

/*
 * The pages behind a shmem-backed GEM object, wired, one per page of the object (Linux's drm_gem_get_pages; drm-kmod
 * compiles it for Linux only). On LinuxKPI the object's backing store is the shmem vm_object of its file.
 */
struct page **drm_gem_get_pages(struct drm_gem_object *obj)
{
	unsigned long i, npages = obj->size >> PAGE_SHIFT;
	struct page **pages;

	if (obj->filp == NULL)
		return ERR_PTR(-EINVAL);
	pages = kvmalloc_array(npages, sizeof(*pages), GFP_KERNEL);
	if (pages == NULL)
		return ERR_PTR(-ENOMEM);
	for (i = 0; i < npages; i++) {
		struct page *p = shmem_read_mapping_page(obj->filp->f_shmem, i);
		if (IS_ERR(p)) {
			while (i > 0)
				put_page(pages[--i]);
			kvfree(pages);
			return ERR_CAST(p);
		}
		pages[i] = p;
	}
	return pages;
}

void drm_gem_put_pages(struct drm_gem_object *obj, struct page **pages, bool dirty, bool accessed)
{
	unsigned long i, npages = obj->size >> PAGE_SHIFT;

	for (i = 0; i < npages; i++) {
		if (pages[i] == NULL)
			continue;
		if (dirty)
			set_page_dirty(pages[i]);
		if (accessed)
			mark_page_accessed(pages[i]);
		put_page(pages[i]);
	}
	kvfree(pages);
}
