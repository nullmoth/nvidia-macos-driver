# fork_linux_page.py <override/src/linux_page.c>: replaces the FreeBSD-VM-specific functions of LinuxKPI's
# linux_page.c with their macOS forms. Each replacement is anchored on the function's signature line and must match
# exactly once; everything else in the file stays LinuxKPI's.
import pathlib, re, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()

def replace_func(sig_line, new_body):
    global s
    anchored = "\n" + sig_line                 # the signature must begin a line (not match a longer name)
    assert s.count(anchored) == 1, (sig_line, s.count(anchored))
    i = s.index(anchored) + 1
    start = s.rfind("\n\n", 0, i) + 2          # start of the return-type line(s)
    end = s.index("\n}\n", i) + 3
    s = s[:start] + new_body.strip("\n") + "\n" + s[end:]

def replace_once(old, new):
    global s
    assert s.count(old) == 1, (old[:60], s.count(old))
    s = s.replace(old, new)

# FreeBSD VM internals (radix tree, reservations, UMA internals) do not exist on macOS
for line in ["#include <vm/vm_radix.h>\n", "#include <vm/vm_reserv.h>\n", "#include <vm/uma_int.h>\n"]:
    replace_once(line, "")
replace_once("#include <vm/vm_extern.h>\n", '#include <vm/vm_extern.h>\n#include "nmx_xnu.h"\n#include "nmx_vma.h"\n')

replace_func("linux_alloc_pages(gfp_t flags, unsigned int order)\n", r'''
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
''')

replace_func("linux_free_kmem(vm_offset_t addr, unsigned int order)\n", r'''
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
''')

replace_func("linux_get_user_pages_internal(vm_map_t map, unsigned long start, int nr_pages,\n", r'''
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
''')

replace_func("get_user_pages_remote(struct task_struct *task, struct mm_struct *mm,\n", r'''
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
''')

replace_func("lkpi_get_user_pages(unsigned long start, unsigned long nr_pages,\n", r'''
long
lkpi_get_user_pages(unsigned long start, unsigned long nr_pages,
    unsigned int gup_flags, struct page **pages)
{
	if (pages == NULL)
		return (-EOPNOTSUPP);
	return (nmx_user_pages_pin(NULL, start, (int)nr_pages, (gup_flags & FOLL_WRITE) != 0, pages));
}
''')

replace_once("struct vmmap {\n\tLIST_ENTRY(vmmap)\tvm_next;\n\tvoid\t\t\t*vm_addr;\n\tunsigned long\t\tvm_size;\n};",
             "struct vmmap {\n\tLIST_ENTRY(vmmap)\tvm_next;\n\tvoid\t\t\t*vm_addr;\n\tunsigned long\t\tvm_size;\n"
             "\tvoid\t\t\t*vm_handle;\t/* macOS port: the IOKit mapping to release */\n};")

replace_func("is_vmalloc_addr(const void *addr)\n", r'''
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
''')

replace_func("vmmap_add(void *addr, unsigned long size)\n", r'''
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
''')

# _ioremap_attr: keep LinuxKPI's body, record no IOKit handle (pmap_unmapdev releases by address)
replace_once("\tvmmap_add(addr, size);\n\n\treturn (addr);\n}\n#endif\n", "\tvmmap_add(addr, size, NULL);\n\n\treturn (addr);\n}\n#endif\n")

replace_func("vmap(struct page **pages, unsigned int count, unsigned long flags, int prot)\n", r'''
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
''')

replace_func("linuxkpi_vmap_pfn(unsigned long *pfns, unsigned int count, int prot)\n", r'''
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
''')

replace_func("vunmap(void *addr)\n", r'''
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
''')

replace_func("lkpi_vmf_insert_pfn_prot_locked(struct vm_area_struct *vma, unsigned long addr,\n", r'''
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
''')

replace_func("lkpi_remap_pfn_range(struct vm_area_struct *vma, unsigned long start_addr,\n", r'''
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
''')

replace_func("lkpi_unmap_mapping_range(void *obj, loff_t const holebegin __unused,\n", r'''
/*
 * macOS port: a buffer stays resident while it is mapped into a process (the device-node glue pins it for the
 * life of the mapping), so there is never a stale user mapping to tear down when TTM moves memory.
 */
void
lkpi_unmap_mapping_range(void *obj, loff_t const holebegin __unused,
    loff_t const holelen __unused, int even_cows __unused)
{
}
''')

replace_func("lkpi_arch_phys_wc_add(unsigned long base, unsigned long size)\n", r'''
/* macOS port: write-combining comes from the mapping's PAT attribute; no MTRR is programmed */
int
lkpi_arch_phys_wc_add(unsigned long base, unsigned long size)
{
	return (0);
}
''')

replace_func("lkpi_arch_phys_wc_del(int reg)\n", r'''
void
lkpi_arch_phys_wc_del(int reg)
{
}
''')

replace_func("lkpi_set_pages_attr(struct page *page, int numpages, vm_memattr_t ma)\n", r'''
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
''')

replace_func("linux_page_address(const struct page *page)\n", r"""
/* macOS port: every page record carries the kernel address it is mapped at (bsdcompat/src/nmx_page.c) */
void *
linux_page_address(const struct page *page)
{
	return (page->nmx_kva);
}
""")

replace_func("__get_user_pages_fast(unsigned long start, int nr_pages, int write,\n", r"""
/* macOS port: pins no user pages (see linux_get_user_pages_internal); callers fall back to the slow path */
int
__get_user_pages_fast(unsigned long start, int nr_pages, int write,
    struct page **pages)
{
	return (0);
}
""")

p.write_text(s)
print("linux_page.c fork: FreeBSD-VM functions replaced")
