/* FreeBSD <vm/vm_page.h> on xnu: the port's page record (see vm/vm.h). */
#ifndef NMXKPI_VM_VM_PAGE_H
#define NMXKPI_VM_VM_PAGE_H
#include <vm/vm.h>
#include <sys/queue.h>
struct vm_page {
    vm_paddr_t phys_addr;           /* physical address */
    void *nmx_kva;                  /* kernel virtual address the page is mapped at */
    void *nmx_alloc;                /* allocation this page belongs to (head page holds it), for freeing */
    u_int nmx_npages;               /* pages in that allocation (head page only) */
    volatile u_int ref_count;       /* wire count, as FreeBSD's */
    vm_object_t object;
    vm_pindex_t pindex;
    uint16_t flags;
    uint8_t oflags;
    uint8_t a_flags;
    uint8_t order;
    uint8_t pool;
    struct { vm_memattr_t pat_mode; } md;   /* cache attribute of the page's mappings */
    TAILQ_ENTRY(vm_page) listq;
    union {                         /* FreeBSD's queue links; drm-kmod's TTM pool keeps free pages on plinks.q */
        TAILQ_ENTRY(vm_page) q;
        struct { SLIST_ENTRY(vm_page) ss; } s;
    } plinks;
    void *nmx_lkpi;                 /* LinuxKPI per-page data (struct page private) */
    void *nmx_pins;                 /* user-page pins this page is held by (see nmx_user_pages_pin) */
};
TAILQ_HEAD(pglist, vm_page);
#define PG_FICTITIOUS 0x0004
#define PG_ZERO 0x0008
#define PG_MARKER 0x0010
#define VPO_UNMANAGED 0x04
#define VM_ALLOC_NORMAL 0
#define VM_ALLOC_INTERRUPT 1
#define VM_ALLOC_SYSTEM 2
#define VM_ALLOC_CLASS_MASK 3
#define VM_ALLOC_WAITOK 0x0008
#define VM_ALLOC_WAITFAIL 0x0010
#define VM_ALLOC_WIRED 0x0020
#define VM_ALLOC_ZERO 0x0040
#define VM_ALLOC_NORECLAIM 0x0080
#define VM_ALLOC_NOFREE 0x0100
#define VM_ALLOC_NOBUSY 0x0200
#define VM_ALLOC_NOCREAT 0x0400
#define VM_ALLOC_IGN_SBUSY 0x1000
#define VM_ALLOC_NODUMP 0x2000
#define VM_ALLOC_SBUSY 0x4000
#define VM_ALLOC_NOWAIT 0x8000
#define VM_PAGE_TO_PHYS(m) ((m)->phys_addr)
vm_page_t nmx_phys_to_page(vm_paddr_t pa);
#define PHYS_TO_VM_PAGE(pa) nmx_phys_to_page(pa)
void *nmx_phys_to_kva(vm_paddr_t pa);
bool nmx_phys_in_dmap(vm_paddr_t pa);
/* FreeBSD 15: PHYS_TO_DMAP yields a pointer; defined for every page the port registered (see vm/vm.h) */
#define PMAP_HAS_DMAP 1
#define PHYS_IN_DMAP(pa) nmx_phys_in_dmap(pa)
#define PHYS_TO_DMAP_ADDR(pa) ((vm_offset_t)nmx_phys_to_kva(pa))
#define PHYS_TO_DMAP(pa) nmx_phys_to_kva(pa)
vm_paddr_t nmx_kva_to_phys(const void *va);
#define DMAP_TO_PHYS(va) nmx_kva_to_phys((const void *)(va))
vm_page_t vm_page_alloc_noobj(int req);
vm_page_t vm_page_alloc_noobj_contig(int req, u_long npages, vm_paddr_t low, vm_paddr_t high, u_long alignment, vm_paddr_t boundary, vm_memattr_t memattr);
void vm_page_free(vm_page_t m);
void vm_page_free_contig(vm_page_t m, u_long npages);
void vm_page_wire(vm_page_t m);
bool vm_page_unwire_noq(vm_page_t m);
/* pins npages of a process's memory at 'start' (mm NULL = the calling process); returns npages or -errno */
int nmx_user_pages_pin(void *mm, uintptr_t start, int npages, int write, vm_page_t *pages);
void vm_page_unwire(vm_page_t m, uint8_t queue);
#define vm_page_xbusy(m) do { } while (0)
#define vm_page_xunbusy(m) do { } while (0)
#define vm_page_sunbusy(m) do { } while (0)
#define vm_page_tryxbusy(m) 1
#define vm_page_busy_acquire(m, f) 1
#define vm_page_reference(m) do { } while (0)
#define vm_page_dirty(m) do { } while (0)
#define vm_page_activate(m) do { } while (0)
#define vm_page_lock(m) do { } while (0)
#define vm_page_unlock(m) do { } while (0)
#define vm_page_wired(m) ((m)->ref_count > 0)
#define vm_page_insert(m, o, i) 0
#define vm_page_remove(m) do { } while (0)
#define vm_page_assert_xbusied(m) do { } while (0)
#define vm_page_aflag_set(m, f) do { } while (0)
#define PQ_NONE 255
#define PQ_ACTIVE 1
#define PQ_INACTIVE 0
#define VM_PAGE_BITS_ALL 0xffu
static inline vm_memattr_t pmap_page_get_memattr(vm_page_t m) { return m->md.pat_mode; }
#endif
