/*-
 * Copyright (c) 2010 Isilon Systems, Inc.
 * Copyright (c) 2016 Matthew Macy (mmacy@mattmacy.io)
 * Copyright (c) 2017 Mellanox Technologies, Ltd.
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice unmodified, this list of conditions, and the following
 *    disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/kernel.h>
#include <sys/sysctl.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/rwlock.h>
#include <sys/proc.h>
#include <sys/sched.h>
#include <sys/memrange.h>

#include <machine/bus.h>

#include <vm/vm.h>
#include <vm/pmap.h>
#include <vm/vm_param.h>
#include <vm/vm_kern.h>
#include <vm/vm_object.h>
#include <vm/vm_map.h>
#include <vm/vm_page.h>
#include <vm/vm_pageout.h>
#include <vm/vm_pager.h>
#include <vm/vm_extern.h>
#include "nmx_xnu.h"
#include "nmx_vma.h"

#include <vm/uma.h>

#include <linux/gfp.h>
#include <linux/mm.h>
#include <linux/preempt.h>
#include <linux/fs.h>
#include <linux/shmem_fs.h>
#include <linux/kernel.h>
#include <linux/idr.h>
#include <linux/io.h>
#include <linux/io-mapping.h>
#include <linux/device.h>

#ifdef __i386__
DEFINE_IDR(mtrr_idr);
static MALLOC_DEFINE(M_LKMTRR, "idr", "Linux MTRR compat");
extern int pat_works;
#endif

void
si_meminfo(struct sysinfo *si)
{
	si->totalram = physmem;
	si->freeram = vm_free_count();
	si->totalhigh = 0;
	si->freehigh = 0;
	si->mem_unit = PAGE_SIZE;
}

/* macOS port: every page record carries the kernel address it is mapped at (bsdcompat/src/nmx_page.c) */
void *
linux_page_address(const struct page *page)
{
	return (page->nmx_kva);
}

/*
 * macOS port: pages come from the port's allocator (bsdcompat/src/nmx_page.c). Single pages without an address
 * limit come from its chunk pool; larger orders and 32-bit-DMA requests get a physically contiguous run. There is
 * no NUMA domain choice and no reclaim loop: xnu's own allocator decides placement and pressure.
 */
struct page *
linux_alloc_pages(gfp_t flags, unsigned int order)
{
	unsigned long npages = 1UL << order;
	int req = VM_ALLOC_WIRED;

	if ((flags & M_ZERO) != 0)
		req |= VM_ALLOC_ZERO;
	if (order == 0 && (flags & GFP_DMA32) == 0)
		return (vm_page_alloc_noobj(req));
	return (vm_page_alloc_noobj_contig(req, npages, 0,
	    (flags & GFP_DMA32) ? BUS_SPACE_MAXADDR_32BIT : BUS_SPACE_MAXADDR,
	    PAGE_SIZE, 0, VM_MEMATTR_DEFAULT));
}

static void
_linux_free_kmem(vm_offset_t addr, unsigned int order)
{
	size_t size = ((size_t)PAGE_SIZE) << order;

	kmem_free((void *)addr, size);
}

void
linux_free_pages(struct page *page, unsigned int order)
{
	if (PMAP_HAS_DMAP) {
		unsigned long npages = 1UL << order;
		unsigned long x;

		for (x = 0; x != npages; x++) {
			vm_page_t pgo = page + x;

			/*
			 * The "free page" function is used in several
			 * contexts.
			 *
			 * Some pages are allocated by `linux_alloc_pages()`
			 * above, but not all of them are. For instance in the
			 * DRM drivers, some pages come from
			 * `shmem_read_mapping_page_gfp()`.
			 *
			 * That's why we need to check if the page is managed
			 * or not here.
			 */
			if ((pgo->oflags & VPO_UNMANAGED) == 0) {
				vm_page_unwire(pgo, PQ_ACTIVE);
			} else {
				if (vm_page_unwire_noq(pgo))
					vm_page_free(pgo);
			}
		}
	} else {
		vm_offset_t vaddr;

		vaddr = (vm_offset_t)page_address(page);

		_linux_free_kmem(vaddr, order);
	}
}

void
linux_release_pages(release_pages_arg arg, int nr)
{
	int i;

	CTASSERT(offsetof(struct folio, page) == 0);

	for (i = 0; i < nr; i++)
		__free_page(arg.pages[i]);
}

vm_offset_t
linux_alloc_kmem(gfp_t flags, unsigned int order)
{
	size_t size = ((size_t)PAGE_SIZE) << order;
	void *addr;

	addr = kmem_alloc_contig(size, flags & GFP_NATIVE_MASK, 0,
	    ((flags & GFP_DMA32) == 0) ? -1UL : BUS_SPACE_MAXADDR_32BIT,
	    PAGE_SIZE, 0, VM_MEMATTR_DEFAULT);

	return ((vm_offset_t)addr);
}

/* macOS port: every kmem block is a run of registered pages, so it is freed through its page records */
void
linux_free_kmem(vm_offset_t addr, unsigned int order)
{
	vm_page_t page;

	KASSERT((addr & ~PAGE_MASK) == 0,
	    ("%s: addr %p is not page aligned", __func__, (void *)addr));
	page = PHYS_TO_VM_PAGE(vtophys(addr));
	if (page != NULL)
		linux_free_pages(page, order);
}

/*
 * macOS port: pinning a process's pages for the GPU (userptr buffers, imported host memory) goes through the
 * port's pin facility (nmx_user_pages_pin: an IOMemoryDescriptor wiring on the task, released by the last put_page).
 * 'map' is unused: the calling process is the one whose pages are pinned.
 */
static int
linux_get_user_pages_internal(vm_map_t map, unsigned long start, int nr_pages,
    int write, struct page **pages)
{
	return (nmx_user_pages_pin(NULL, start, nr_pages, write, pages));
}

/* macOS port: pins no user pages (see linux_get_user_pages_internal); callers fall back to the slow path */
int
__get_user_pages_fast(unsigned long start, int nr_pages, int write,
    struct page **pages)
{
	return (0);
}

long
get_user_pages_remote(struct task_struct *task, struct mm_struct *mm,
    unsigned long start, unsigned long nr_pages, unsigned int gup_flags,
    struct page **pages, struct vm_area_struct **vmas)
{
	/* macOS port: another process's pages, found through its mm (nmx_mm.cpp) */
	if (pages == NULL)
		return (-EOPNOTSUPP);
	return (nmx_user_pages_pin(mm, start, (int)nr_pages, (gup_flags & FOLL_WRITE) != 0, pages));
}

long
lkpi_get_user_pages(unsigned long start, unsigned long nr_pages,
    unsigned int gup_flags, struct page **pages)
{
	if (pages == NULL)
		return (-EOPNOTSUPP);
	return (nmx_user_pages_pin(NULL, start, (int)nr_pages, (gup_flags & FOLL_WRITE) != 0, pages));
}

/*
 * Hash of vmmap addresses.  This is infrequently accessed and does not
 * need to be particularly large.  This is done because we must store the
 * caller's idea of the map size to properly unmap.
 */
struct vmmap {
	LIST_ENTRY(vmmap)	vm_next;
	void			*vm_addr;
	unsigned long		vm_size;
	void			*vm_handle;	/* macOS port: the IOKit mapping to release */
};

struct vmmaphd {
	struct vmmap *lh_first;
};
#define VMMAP_HASH_SIZE 64
#define VMMAP_HASH_MASK (VMMAP_HASH_SIZE - 1)
#define VM_HASH(addr)   ((uintptr_t)(addr) >> PAGE_SHIFT) & VMMAP_HASH_MASK
static struct vmmaphd vmmaphead[VMMAP_HASH_SIZE];
static struct mtx vmmaplock;

/* macOS port: vmalloc'd memory is exactly what vmap mapped (kmalloc memory has no UMA slab to look up here) */
int
is_vmalloc_addr(const void *addr)
{
	struct vmmap *vmmap;

	mtx_lock(&vmmaplock);
	LIST_FOREACH(vmmap, &vmmaphead[VM_HASH(addr)], vm_next)
		if (addr == vmmap->vm_addr)
			break;
	mtx_unlock(&vmmaplock);
	return (vmmap != NULL);
}

static void
vmmap_add(void *addr, unsigned long size, void *handle)
{
	struct vmmap *vmmap;

	vmmap = kmalloc(sizeof(*vmmap), GFP_KERNEL);
	mtx_lock(&vmmaplock);
	vmmap->vm_size = size;
	vmmap->vm_addr = addr;
	vmmap->vm_handle = handle;
	LIST_INSERT_HEAD(&vmmaphead[VM_HASH(addr)], vmmap, vm_next);
	mtx_unlock(&vmmaplock);
}

static struct vmmap *
vmmap_remove(void *addr)
{
	struct vmmap *vmmap;

	mtx_lock(&vmmaplock);
	LIST_FOREACH(vmmap, &vmmaphead[VM_HASH(addr)], vm_next)
		if (vmmap->vm_addr == addr)
			break;
	if (vmmap)
		LIST_REMOVE(vmmap, vm_next);
	mtx_unlock(&vmmaplock);

	return (vmmap);
}

#if defined(__i386__) || defined(__amd64__) || defined(__powerpc__) || defined(__aarch64__) || defined(__riscv)
void *
_ioremap_attr(vm_paddr_t phys_addr, unsigned long size, int attr)
{
	void *addr;

	addr = pmap_mapdev_attr(phys_addr, size, attr);
	if (addr == NULL)
		return (NULL);
	vmmap_add(addr, size, NULL);

	return (addr);
}
#endif

void
iounmap(void *addr)
{
	struct vmmap *vmmap;

	vmmap = vmmap_remove(addr);
	if (vmmap == NULL)
		return;
#if defined(__i386__) || defined(__amd64__) || defined(__powerpc__) || defined(__aarch64__) || defined(__riscv)
	pmap_unmapdev(addr, vmmap->vm_size);
#endif
	kfree(vmmap);
}

static void
lkpi_devm_memremap_unmap(struct device *dev, void *p)
{
	void **dr = p;

	memunmap(*dr);
}

void *
linuxkpi_devm_memremap(struct device *dev, resource_size_t offset, size_t size,
    unsigned long flags)
{
	void **dr, *addr;

	dr = devres_alloc(lkpi_devm_memremap_unmap, sizeof(*dr), GFP_KERNEL);
	if (dr == NULL)
		return (ERR_PTR(-ENOMEM));
	addr = memremap(offset, size, flags);
	if (addr != NULL) {
		*dr = addr;
		devres_add(dev, dr);
	} else {
		addr = ERR_PTR(-ENXIO);
		devres_free(dr);
	}

	return (addr);
}

/*
 * macOS port: the pages are mapped contiguously into the kernel through IOKit in one step (xnu has no separate
 * "reserve address range, then enter pages"). The cache mode is the one the caller asked for in 'prot', or else
 * the one recorded on the pages (set_memory_wc / pmap_page_set_memattr).
 */
void *
vmap(struct page **pages, unsigned int count, unsigned long flags, int prot)
{
	uint64_t *phys;
	void *addr, *handle;
	int cache;
	unsigned int i;

	if (count == 0)
		return (NULL);
	phys = kmalloc(sizeof(*phys) * count, GFP_KERNEL);
	if (phys == NULL)
		return (NULL);
	for (i = 0; i < count; i++)
		phys[i] = VM_PAGE_TO_PHYS(pages[i]);
	cache = pgprot2cachemode(prot);
	if (cache == VM_MEMATTR_DEFAULT)
		cache = pages[0]->md.pat_mode;
	addr = nmx_xnu_map_pages(phys, count, cache, &handle);
	kfree(phys);
	if (addr == NULL)
		return (NULL);
	vmmap_add(addr, (unsigned long)count * PAGE_SIZE, handle);
	return (addr);
}

#define	VMAP_MAX_CHUNK_SIZE (65536U / sizeof(struct vm_page)) /* KMEM_ZMAX */

/* macOS port: as vmap, from page frame numbers (device memory included) */
void *
linuxkpi_vmap_pfn(unsigned long *pfns, unsigned int count, int prot)
{
	uint64_t *phys;
	void *addr, *handle;
	unsigned int i;

	if (count == 0)
		return (NULL);
	phys = kmalloc(sizeof(*phys) * count, GFP_KERNEL);
	if (phys == NULL)
		return (NULL);
	for (i = 0; i < count; i++)
		phys[i] = ptoa((vm_paddr_t)pfns[i]);
	addr = nmx_xnu_map_pages(phys, count, pgprot2cachemode(prot), &handle);
	kfree(phys);
	if (addr == NULL)
		return (NULL);
	vmmap_add(addr, (unsigned long)count * PAGE_SIZE, handle);
	return (addr);
}

void
vunmap(void *addr)
{
	struct vmmap *vmmap;

	vmmap = vmmap_remove(addr);
	if (vmmap == NULL)
		return;
	nmx_xnu_unmap(vmmap->vm_handle);
	kfree(vmmap);
}

/*
 * macOS port: a fault handler's insert records the page in the user-mapping record that owns the vma
 * (nmx_linux_cdev.c), which maps the whole range into the process once every page is known.
 */
vm_fault_t
lkpi_vmf_insert_pfn_prot_locked(struct vm_area_struct *vma, unsigned long addr,
    unsigned long pfn, pgprot_t prot)
{
	return (nmx_umap_insert(vma, addr, pfn, prot));
}

/* macOS port: a driver's direct mapping of a physical range is recorded page by page, as a fault would insert it */
int
lkpi_remap_pfn_range(struct vm_area_struct *vma, unsigned long start_addr,
    unsigned long start_pfn, unsigned long size, pgprot_t prot)
{
	unsigned long off;

	for (off = 0; off < size; off += PAGE_SIZE) {
		if (nmx_umap_insert(vma, start_addr + off, start_pfn + (off >> PAGE_SHIFT),
		    prot) != VM_FAULT_NOPAGE)
			return (-EINVAL);
	}
	return (0);
}

int
lkpi_io_mapping_map_user(struct io_mapping *iomap,
    struct vm_area_struct *vma, unsigned long addr,
    unsigned long pfn, unsigned long size)
{
	pgprot_t prot;
	int ret;

	prot = cachemode2protval(iomap->attr);
	ret = lkpi_remap_pfn_range(vma, addr, pfn, size, prot);

	return (ret);
}

/*
 * macOS port: a buffer stays resident while it is mapped into a process (the device-node glue pins it for the
 * life of the mapping), so there is never a stale user mapping to tear down when TTM moves memory.
 */
void
lkpi_unmap_mapping_range(void *obj, loff_t const holebegin __unused,
    loff_t const holelen __unused, int even_cows __unused)
{
}

/* macOS port: write-combining comes from the mapping's PAT attribute; no MTRR is programmed */
int
lkpi_arch_phys_wc_add(unsigned long base, unsigned long size)
{
	return (0);
}

void
lkpi_arch_phys_wc_del(int reg)
{
}

/* macOS port: the attribute is recorded on each page and applied when the page is mapped */
int
lkpi_set_pages_attr(struct page *page, int numpages, vm_memattr_t ma)
{
	while (numpages-- > 0) {
		pmap_page_set_memattr(page, ma);
		page++;
	}
	return (0);
}

/*
 * This is a highly simplified version of the Linux page_frag_cache.
 * We only support up-to 1 single page as fragment size and we will
 * always return a full page.  This may be wasteful on small objects
 * but the only known consumer (mt76) is either asking for a half-page
 * or a full page.  If this was to become a problem we can implement
 * a more elaborate version.
 */
void *
linuxkpi_page_frag_alloc(struct page_frag_cache *pfc,
    size_t fragsz, gfp_t gfp)
{
	struct page *pages;

	if (fragsz == 0)
		return (NULL);

	KASSERT(fragsz <= PAGE_SIZE, ("%s: fragsz %zu > PAGE_SIZE not yet "
	    "supported", __func__, fragsz));

	pages = alloc_pages(gfp, flsl(howmany(fragsz, PAGE_SIZE) - 1));
	if (pages == NULL)
		return (NULL);
	pfc->va = linux_page_address(pages);

	/* Passed in as "count" to __page_frag_cache_drain(). Unused by us. */
	pfc->pagecnt_bias = 0;

	return (pfc->va);
}

void
linuxkpi_page_frag_free(void *addr)
{
	struct page *page;

	page = virt_to_page(addr);
	linux_free_pages(page, 0);
}

void
linuxkpi__page_frag_cache_drain(struct page *page, size_t count __unused)
{

	linux_free_pages(page, 0);
}

static void
lkpi_page_init(void *arg)
{
	int i;

	mtx_init(&vmmaplock, "IO Map lock", NULL, MTX_DEF);
	for (i = 0; i < VMMAP_HASH_SIZE; i++)
		LIST_INIT(&vmmaphead[i]);
}
SYSINIT(lkpi_page, SI_SUB_DRIVERS, SI_ORDER_SECOND, lkpi_page_init, NULL);

static void
lkpi_page_uninit(void *arg)
{
	mtx_destroy(&vmmaplock);
}
SYSUNINIT(lkpi_page, SI_SUB_DRIVERS, SI_ORDER_SECOND, lkpi_page_uninit, NULL);
