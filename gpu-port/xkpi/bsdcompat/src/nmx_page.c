/*
 * FreeBSD's physical-page API (vm_page) on xnu, for LinuxKPI's alloc_pages/page_address/get_page/put_page.
 *
 * xnu gives kexts no page array and no direct map, so every page handed out is a struct vm_page this file creates:
 *   - single pages come from 2 MB chunks of wired kernel memory (nmx_xnu_mem_alloc), carved page by page and
 *     recycled through a free list; a chunk is returned to the system when all its pages are free again;
 *   - multi-page, 32-bit-DMA, or explicitly contiguous requests get one physically contiguous run of their own.
 * Every page is registered by physical page number, so PHYS_TO_VM_PAGE and PHYS_TO_DMAP work for all of them.
 * Pages start wired once (ref_count 1); the last unwire frees them, which is LinuxKPI's put_page/__free_page rule.
 * A physical address the port never allocated (kmalloc memory reached through virt_to_page, GPU BAR pages) gets a
 * "fictitious" record on demand; those carry no backing allocation and are never freed to a pool.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <vm/vm.h>
#include <vm/vm_page.h>
#include <vm/vm_object.h>
#include <vm/vm_pager.h>
#include <vm/pmap.h>

int nmx_malloc_note_contig(void *addr, size_t size, struct malloc_type *type);
void nmx_malloc_forget_contig(void *addr);
#include <vm/vm_extern.h>
#include <machine/cpufunc.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_NMXPAGE, "nmxpage", "page records");

#define CHUNK_PAGES 512                    /* 2 MB of 4 KB pages */
#define REG_BUCKETS 65536                  /* page registry hash: page frame number -> struct vm_page */
#define DMA32_LIMIT 0xffffffffULL

enum backing_kind { BK_CHUNK = 1, BK_RUN = 2, BK_FICTITIOUS = 3 };

struct backing {
    int kind;
    void *handle;                          /* nmx_xnu_mem_alloc handle */
    void *kva;
    size_t npages;
    u_int live;                            /* pages handed out and not yet freed */
    struct backing *next;                  /* chunk list */
    struct vm_page pages[];                /* BK_CHUNK / BK_RUN: one record per page */
};

struct reg_entry {
    vm_paddr_t pfn;
    struct vm_page *m;
    struct reg_entry *next;
};

static struct reg_entry **reg_table;
static lck_spin_t *pg_lock;               /* registry, chunk list and free list */
static struct backing *chunks;
static struct vm_page *free_pages;         /* free single pages, linked through plinks.s.ss */
static u_int free_count;
static uint64_t pages_live;                /* pages currently handed out (all kinds) */

static unsigned reg_bucket(vm_paddr_t pfn)
{
    return (unsigned)((pfn ^ (pfn >> 16)) & (REG_BUCKETS - 1));
}

static void reg_insert_locked(struct vm_page *m, struct reg_entry *e)
{
    vm_paddr_t pfn = m->phys_addr >> PAGE_SHIFT;
    unsigned b = reg_bucket(pfn);
    e->pfn = pfn;
    e->m = m;
    e->next = reg_table[b];
    reg_table[b] = e;
}

static struct reg_entry *reg_remove_locked(vm_paddr_t pfn)
{
    unsigned b = reg_bucket(pfn);
    for (struct reg_entry **pp = &reg_table[b]; *pp; pp = &(*pp)->next) {
        if ((*pp)->pfn == pfn) {
            struct reg_entry *e = *pp;
            *pp = e->next;
            return e;
        }
    }
    return NULL;
}

static struct vm_page *reg_find_locked(vm_paddr_t pfn)
{
    for (struct reg_entry *e = reg_table[reg_bucket(pfn)]; e; e = e->next)
        if (e->pfn == pfn)
            return e->m;
    return NULL;
}

int nmx_page_init(void)
{
    pg_lock = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    reg_table = malloc(sizeof(*reg_table) * REG_BUCKETS, M_NMXPAGE, M_WAITOK | M_ZERO);
    return (pg_lock && reg_table) ? 0 : ENOMEM;
}

static void page_record_init(struct vm_page *m, struct backing *bk, vm_paddr_t pa, void *kva)
{
    bzero(m, sizeof(*m));
    m->phys_addr = pa;
    m->nmx_kva = kva;
    m->nmx_alloc = bk;
    m->oflags = VPO_UNMANAGED;
    m->md.pat_mode = VM_MEMATTR_WRITE_BACK;
}

/* registers every page of a new backing; the registry entries are allocated before taking the spin lock */
static int backing_register(struct backing *bk)
{
    struct reg_entry *ents = malloc(sizeof(*ents) * bk->npages, M_NMXPAGE, M_WAITOK);
    if (ents == NULL)
        return ENOMEM;
    lck_spin_lock(pg_lock);
    for (size_t i = 0; i < bk->npages; i++)
        reg_insert_locked(&bk->pages[i], &ents[i]);
    lck_spin_unlock(pg_lock);
    return 0;
}

/* removes a backing's pages from the registry (lock held); returns the registry entry array for freeing */
static struct reg_entry *backing_unregister_locked(struct backing *bk)
{
    struct reg_entry *first = NULL;
    for (size_t i = 0; i < bk->npages; i++) {
        struct reg_entry *e = reg_remove_locked(bk->pages[i].phys_addr >> PAGE_SHIFT);
        if (i == 0)
            first = e;
    }
    return first;   /* entries were allocated as one array whose first element maps page 0 */
}

static struct backing *backing_alloc(int kind, size_t npages, int contiguous, uint64_t max_phys, size_t align)
{
    struct backing *bk = malloc(sizeof(*bk) + sizeof(struct vm_page) * npages, M_NMXPAGE, M_WAITOK | M_ZERO);
    if (bk == NULL)
        return NULL;
    bk->kind = kind;
    bk->npages = npages;
    bk->kva = nmx_xnu_mem_alloc(npages * PAGE_SIZE, contiguous, max_phys, align, &bk->handle);
    if (bk->kva == NULL) {
        free(bk, M_NMXPAGE);
        return NULL;
    }
    for (size_t i = 0; i < npages; i++) {
        void *kva = (char *)bk->kva + i * PAGE_SIZE;
        page_record_init(&bk->pages[i], bk, nmx_xnu_kva_to_phys(kva), kva);
    }
    if (backing_register(bk) != 0) {
        nmx_xnu_mem_free(bk->handle);
        free(bk, M_NMXPAGE);
        return NULL;
    }
    return bk;
}

static void backing_release(struct backing *bk)
{
    lck_spin_lock(pg_lock);
    struct reg_entry *ents = backing_unregister_locked(bk);
    lck_spin_unlock(pg_lock);
    free(ents, M_NMXPAGE);
    nmx_xnu_mem_free(bk->handle);
    free(bk, M_NMXPAGE);
}

/* ---- allocation --------------------------------------------------------------------------------------------------- */

static struct vm_page *single_alloc(void)
{
    for (;;) {
        lck_spin_lock(pg_lock);
        struct vm_page *m = free_pages;
        if (m) {
            free_pages = (struct vm_page *)(uintptr_t)m->plinks.s.ss.sle_next;
            free_count--;
            ((struct backing *)m->nmx_alloc)->live++;
            pages_live++;
            lck_spin_unlock(pg_lock);
            m->plinks.s.ss.sle_next = NULL;
            return m;
        }
        lck_spin_unlock(pg_lock);

        struct backing *bk = backing_alloc(BK_CHUNK, CHUNK_PAGES, 0, 0, PAGE_SIZE);
        if (bk == NULL)
            return NULL;
        lck_spin_lock(pg_lock);
        bk->next = chunks;
        chunks = bk;
        for (size_t i = 0; i < bk->npages; i++) {
            bk->pages[i].plinks.s.ss.sle_next = free_pages;
            free_pages = &bk->pages[i];
            free_count++;
        }
        lck_spin_unlock(pg_lock);
    }
}

static void page_prepare(struct vm_page *m, int req)
{
    m->ref_count = 1;               /* every page starts wired once; LinuxKPI always asks for wired pages */
    m->flags = 0;
    m->md.pat_mode = VM_MEMATTR_WRITE_BACK;
    if (req & VM_ALLOC_ZERO)
        bzero(m->nmx_kva, PAGE_SIZE);
}

vm_page_t vm_page_alloc_noobj(int req)
{
    struct vm_page *m = single_alloc();
    if (m)
        page_prepare(m, req);
    return m;
}

vm_page_t vm_page_alloc_noobj_contig(int req, u_long npages, vm_paddr_t low, vm_paddr_t high, u_long alignment,
                                     vm_paddr_t boundary, vm_memattr_t memattr)
{
    /* a single page with no address limit can come from the chunk pool; any limit needs its own placed run */
    if (npages == 1 && high >= BUS_SPACE_MAXADDR && alignment <= PAGE_SIZE)
        return vm_page_alloc_noobj(req);
    struct backing *bk = backing_alloc(BK_RUN, npages, 1, high, alignment > PAGE_SIZE ? alignment : PAGE_SIZE);
    if (bk == NULL)
        return NULL;
    lck_spin_lock(pg_lock);
    bk->live = (u_int)npages;
    pages_live += npages;
    lck_spin_unlock(pg_lock);
    for (u_long i = 0; i < npages; i++) {
        page_prepare(&bk->pages[i], req);
        bk->pages[i].md.pat_mode = memattr;
    }
    return &bk->pages[0];
}

/* returns one page to its backing; the backing is released when its last page comes back */
void vm_page_free(vm_page_t m)
{
    struct backing *bk = m->nmx_alloc;
    if (bk == NULL || bk->kind == BK_FICTITIOUS)
        return;
    lck_spin_lock(pg_lock);
    pages_live--;
    bk->live--;
    if (bk->kind == BK_CHUNK) {
        m->plinks.s.ss.sle_next = free_pages;
        free_pages = m;
        free_count++;
        lck_spin_unlock(pg_lock);
        return;
    }
    int last = bk->live == 0;
    lck_spin_unlock(pg_lock);
    if (last)
        backing_release(bk);
}

void vm_page_free_contig(vm_page_t m, u_long npages)
{
    for (u_long i = 0; i < npages; i++)
        vm_page_free(m + i);
}

void vm_page_wire(vm_page_t m)
{
    __atomic_fetch_add(&m->ref_count, 1, __ATOMIC_SEQ_CST);
}

/*
 * drops one wiring; true when it was the last and the page belongs to no object, so the caller frees it. A page in
 * an object stays with the object at zero wirings, as in FreeBSD; the object frees it.
 */
/*
 * ---- a process's pages pinned for a device (Linux get_user_pages) -------------------------------------------------------
 * xnu wires a user range through an IOMemoryDescriptor on the process's task (nmx_xnu_user_wire), one descriptor per
 * request. Each page of the request gets the port's page record (a fictitious one: the memory is the process's) and
 * one extra wiring, plus an entry on the record's pin list naming the request. Linux drops a pin with put_page; here
 * the matching unwire pops one entry from that page's list and, when every page of a request has been put, the
 * descriptor is released and xnu unwires the range. A page pinned by several requests carries one entry per pin,
 * and any of them may be popped: each request is only released after as many puts as it has pages, and a page's
 * entries are only consumed by puts of that page, so no page is unwired while one of its pins is still held.
 * The release can block (xnu unwires the range), so put_page on a pinned user page must come from thread context,
 * which is where Linux drivers release userptr pages.
 */
struct user_pin {
    void *handle;                          /* nmx_xnu_user_wire descriptor */
    volatile u_int remaining;              /* page puts still to come */
};
struct pin_ref {
    struct user_pin *pin;
    struct pin_ref *next;
};

static void pin_put(struct user_pin *pin)
{
    if (__atomic_sub_fetch(&pin->remaining, 1, __ATOMIC_SEQ_CST) == 0) {
        nmx_xnu_user_unwire(pin->handle);
        free(pin, M_NMXPAGE);
    }
}

/* takes one pin entry off a page, if it has any; the request it named, or NULL */
static struct user_pin *page_pop_pin(vm_page_t m)
{
    struct pin_ref *r;
    lck_spin_lock(pg_lock);
    r = m->nmx_pins;
    if (r != NULL)
        m->nmx_pins = r->next;
    lck_spin_unlock(pg_lock);
    if (r == NULL)
        return NULL;
    struct user_pin *pin = r->pin;
    free(r, M_NMXPAGE);
    return pin;
}

int nmx_user_pages_pin(void *mm, uintptr_t start, int npages, int write, vm_page_t *pages)
{
    if (npages <= 0)
        return 0;
    uint64_t *phys = malloc(sizeof(uint64_t) * (size_t)npages, M_NMXPAGE, M_WAITOK);
    struct user_pin *pin = malloc(sizeof(*pin), M_NMXPAGE, M_WAITOK | M_ZERO);
    struct pin_ref **refs = malloc(sizeof(*refs) * (size_t)npages, M_NMXPAGE, M_WAITOK | M_ZERO);
    int err = ENOMEM, i;
    if (phys == NULL || pin == NULL || refs == NULL)
        goto fail;
    for (i = 0; i < npages; i++)
        if ((refs[i] = malloc(sizeof(struct pin_ref), M_NMXPAGE, M_WAITOK)) == NULL)
            goto fail;
    err = nmx_xnu_user_wire(mm, start, (size_t)npages, write, phys, &pin->handle);
    if (err != 0)
        goto fail;
    for (i = 0; i < npages; i++) {
        pages[i] = nmx_phys_to_page(phys[i]);
        if (pages[i] == NULL) {
            /* no record could be made: give back the pins already placed, then the wiring */
            pin->remaining = (u_int)i + 1;
            for (int k = 0; k < i; k++) {
                __atomic_sub_fetch(&pages[k]->ref_count, 1, __ATOMIC_SEQ_CST);
                pin_put(page_pop_pin(pages[k]));
            }
            pin_put(pin);
            pin = NULL;
            for (int k = i; k < npages; k++)
                free(refs[k], M_NMXPAGE);
            free(refs, M_NMXPAGE);
            free(phys, M_NMXPAGE);
            return -ENOMEM;
        }
    }
    pin->remaining = (u_int)npages;
    for (i = 0; i < npages; i++) {
        refs[i]->pin = pin;
        lck_spin_lock(pg_lock);
        refs[i]->next = pages[i]->nmx_pins;
        pages[i]->nmx_pins = refs[i];
        lck_spin_unlock(pg_lock);
        vm_page_wire(pages[i]);
    }
    free(refs, M_NMXPAGE);
    free(phys, M_NMXPAGE);
    return npages;
fail:
    if (refs != NULL)
        for (i = 0; i < npages; i++)
            free(refs[i], M_NMXPAGE);
    free(refs, M_NMXPAGE);
    free(pin, M_NMXPAGE);
    free(phys, M_NMXPAGE);
    return -err;
}

bool vm_page_unwire_noq(vm_page_t m)
{
    /* a pinned user page: this put releases one pin (see nmx_user_pages_pin) */
    if ((m->flags & PG_FICTITIOUS) != 0 && m->nmx_pins != NULL) {
        struct user_pin *pin = page_pop_pin(m);
        if (pin != NULL)
            pin_put(pin);
    }
    return __atomic_sub_fetch(&m->ref_count, 1, __ATOMIC_SEQ_CST) == 0 && m->object == NULL;
}

void vm_page_unwire(vm_page_t m, uint8_t queue)
{
    if (vm_page_unwire_noq(m))
        vm_page_free(m);
}

/*
 * ---- VM objects: anonymous page containers (LinuxKPI shmem files, which TTM swaps buffers out to) ----------------------
 * A page array indexed by page number, filled on demand with zeroed pages. There is no pager behind it: the pages
 * stay in memory for the object's life. The object lock (VM_OBJECT_WLOCK) guards the array.
 */
vm_object_t vm_object_allocate(objtype_t type, vm_pindex_t size)
{
    struct vm_object *o = malloc(sizeof(*o), M_NMXPAGE, M_WAITOK | M_ZERO);
    if (o == NULL)
        return NULL;
    if (size > 0) {
        o->nmx_pages = malloc(sizeof(vm_page_t) * (size_t)size, M_NMXPAGE, M_WAITOK | M_ZERO);
        if (o->nmx_pages == NULL) {
            free(o, M_NMXPAGE);
            return NULL;
        }
    }
    mtx_init(&o->lock, "vm object", NULL, MTX_DEF);
    o->type = type;
    o->size = size;
    o->ref_count = 1;
    return o;
}

vm_object_t vm_pager_allocate(objtype_t type, void *handle, vm_ooffset_t size, vm_prot_t prot, vm_ooffset_t off,
                              struct ucred *cred)
{
    return vm_object_allocate(type, atop(round_page(size)));
}

void vm_object_reference(vm_object_t o)
{
    if (o != NULL)
        __atomic_fetch_add(&o->ref_count, 1, __ATOMIC_SEQ_CST);
}

/* detaches the page at i; frees it unless someone still holds a wiring (they free it at their last unwire) */
static void object_drop_page_locked(vm_object_t o, vm_pindex_t i)
{
    vm_page_t *pages = o->nmx_pages;
    vm_page_t m = pages[i];
    if (m == NULL)
        return;
    pages[i] = NULL;
    o->resident_page_count--;
    m->object = NULL;
    if (__atomic_load_n(&m->ref_count, __ATOMIC_SEQ_CST) == 0)
        vm_page_free(m);
}

void vm_object_deallocate(vm_object_t o)
{
    if (o == NULL || __atomic_sub_fetch(&o->ref_count, 1, __ATOMIC_SEQ_CST) != 0)
        return;
    VM_OBJECT_WLOCK(o);
    for (vm_pindex_t i = 0; i < o->size; i++)
        object_drop_page_locked(o, i);
    VM_OBJECT_WUNLOCK(o);
    mtx_destroy(&o->lock);
    free(o->nmx_pages, M_NMXPAGE);
    free(o, M_NMXPAGE);
}

vm_page_t vm_page_lookup(vm_object_t o, vm_pindex_t pindex)
{
    if (o == NULL || pindex >= o->size)
        return NULL;
    return ((vm_page_t *)o->nmx_pages)[pindex];
}

vm_page_t vm_page_grab(vm_object_t o, vm_pindex_t pindex, int allocflags)
{
    if (o == NULL || pindex >= o->size)
        return NULL;
    vm_page_t *pages = o->nmx_pages;
    vm_page_t m = pages[pindex];
    if (m == NULL) {
        if (allocflags & VM_ALLOC_NOCREAT)
            return NULL;
        m = vm_page_alloc_noobj(VM_ALLOC_ZERO);         /* comes back with one wiring */
        if (m == NULL)
            return NULL;
        m->object = o;
        m->pindex = pindex;
        pages[pindex] = m;
        o->resident_page_count++;
        if (!(allocflags & VM_ALLOC_WIRED))
            __atomic_fetch_sub(&m->ref_count, 1, __ATOMIC_SEQ_CST);  /* the object owns it unwired */
        return m;
    }
    if (allocflags & VM_ALLOC_WIRED)
        vm_page_wire(m);
    return m;
}

int vm_page_grab_valid(vm_page_t *mp, vm_object_t o, vm_pindex_t pindex, int allocflags)
{
    *mp = vm_page_grab(o, pindex, allocflags & ~VM_ALLOC_NOCREAT);
    return *mp ? VM_PAGER_OK : VM_PAGER_FAIL;
}

int vm_page_grab_valid_unlocked(vm_page_t *mp, vm_object_t o, vm_pindex_t pindex, int allocflags)
{
    VM_OBJECT_WLOCK(o);
    int r = vm_page_grab_valid(mp, o, pindex, allocflags);
    VM_OBJECT_WUNLOCK(o);
    return r;
}

/*
 * Removes pages [start, end) (end 0 = to the end). OBJPR_CLEANONLY asks for clean pages only; with no record of
 * which pages were written, every page is treated as dirty and kept, so no data is ever dropped.
 */
void vm_object_page_remove(vm_object_t o, vm_pindex_t start, vm_pindex_t end, int options)
{
    if (o == NULL || (options & OBJPR_CLEANONLY))
        return;
    if (end == 0 || end > o->size)
        end = o->size;
    for (vm_pindex_t i = start; i < end; i++)
        object_drop_page_locked(o, i);
}

/* ---- physical <-> page <-> kernel address ------------------------------------------------------------------------- */

vm_page_t nmx_phys_to_page(vm_paddr_t pa)
{
    vm_paddr_t pfn = pa >> PAGE_SHIFT;
    lck_spin_lock(pg_lock);
    struct vm_page *m = reg_find_locked(pfn);
    lck_spin_unlock(pg_lock);
    if (m)
        return m;

    /* not one of ours: a fictitious record (GPU BAR page, or memory reached through virt_to_page) */
    struct backing *bk = malloc(sizeof(*bk) + sizeof(struct vm_page), M_NMXPAGE, M_WAITOK | M_ZERO);
    struct reg_entry *e = malloc(sizeof(*e), M_NMXPAGE, M_WAITOK);
    if (bk == NULL || e == NULL) {
        free(bk, M_NMXPAGE);
        free(e, M_NMXPAGE);
        return NULL;
    }
    bk->kind = BK_FICTITIOUS;
    bk->npages = 1;
    page_record_init(&bk->pages[0], bk, pfn << PAGE_SHIFT, NULL);
    bk->pages[0].flags = PG_FICTITIOUS;
    bk->pages[0].ref_count = 1;
    lck_spin_lock(pg_lock);
    m = reg_find_locked(pfn);
    if (m == NULL) {
        reg_insert_locked(&bk->pages[0], e);
        m = &bk->pages[0];
        bk = NULL;
        e = NULL;
    }
    lck_spin_unlock(pg_lock);
    free(bk, M_NMXPAGE);
    free(e, M_NMXPAGE);
    return m;
}

/* the page record for a kernel address, remembering that address so page_address() returns it */
vm_page_t nmx_kva_to_page(const void *va)
{
    vm_paddr_t pa = nmx_xnu_kva_to_phys(va);
    if (pa == 0)
        return NULL;
    vm_page_t m = nmx_phys_to_page(pa);
    if (m && m->nmx_kva == NULL)
        m->nmx_kva = (void *)trunc_page((uintptr_t)va);
    return m;
}

bool nmx_phys_in_dmap(vm_paddr_t pa)
{
    lck_spin_lock(pg_lock);
    struct vm_page *m = reg_find_locked(pa >> PAGE_SHIFT);
    bool in = m && m->nmx_kva;
    lck_spin_unlock(pg_lock);
    return in;
}

void *nmx_phys_to_kva(vm_paddr_t pa)
{
    lck_spin_lock(pg_lock);
    struct vm_page *m = reg_find_locked(pa >> PAGE_SHIFT);
    void *kva = (m && m->nmx_kva) ? (char *)m->nmx_kva + (pa & PAGE_MASK) : NULL;
    lck_spin_unlock(pg_lock);
    return kva;
}

vm_paddr_t nmx_kva_to_phys(const void *va)
{
    return nmx_xnu_kva_to_phys(va);
}

vm_paddr_t pmap_kextract(vm_offset_t va)
{
    return nmx_xnu_kva_to_phys((const void *)va);
}

/* ---- kernel memory (kmem) and device mappings (pmap) -------------------------------------------------------------- */

void *kmem_alloc_contig(vm_size_t size, int flags, vm_paddr_t low, vm_paddr_t high, u_long alignment,
                        vm_paddr_t boundary, vm_memattr_t memattr)
{
    vm_page_t m = vm_page_alloc_noobj_contig(VM_ALLOC_WIRED | ((flags & M_ZERO) ? VM_ALLOC_ZERO : 0),
                                             atop(round_page(size)), low, high, alignment, boundary, memattr);
    return m ? m->nmx_kva : NULL;
}

void *kmem_alloc_attr(vm_size_t size, int flags, vm_paddr_t low, vm_paddr_t high, vm_memattr_t memattr)
{
    return kmem_alloc_contig(size, flags, low, high, PAGE_SIZE, 0, memattr);
}

void *kmem_malloc(vm_size_t size, int flags)
{
    return kmem_alloc_contig(size, flags, 0, ~(vm_paddr_t)0, PAGE_SIZE, 0, VM_MEMATTR_DEFAULT);
}

void kmem_free(void *addr, vm_size_t size)
{
    if (addr == NULL)
        return;
    vm_page_t m = nmx_phys_to_page(nmx_xnu_kva_to_phys(addr));
    if (m)
        vm_page_free_contig(m, atop(round_page(size)));
}

void *contigmalloc(unsigned long size, struct malloc_type *type, int flags, vm_paddr_t low, vm_paddr_t high,
                   unsigned long alignment, vm_paddr_t boundary)
{
    void *p = kmem_alloc_contig(size, flags, low, high, alignment, boundary, VM_MEMATTR_DEFAULT);
    /* recorded so free() takes it back as FreeBSD's does (LinuxKPI kfree of a kmalloc above a page) */
    if (p != NULL && nmx_malloc_note_contig(p, size, type) != 0) {
        kmem_free(p, size);
        return NULL;
    }
    return p;
}

void contigfree(void *addr, unsigned long size, struct malloc_type *type)
{
    if (addr == NULL)
        return;
    nmx_malloc_forget_contig(addr);
    kmem_free(addr, size);
}

/* device mappings, tracked by address so pmap_unmapdev can release them */
struct devmap { void *va; void *handle; struct devmap *next; };
static struct devmap *devmaps;
static struct mtx devmap_mtx;
MTX_SYSINIT(nmx_devmap, &devmap_mtx, "device mappings", MTX_DEF);

void *pmap_mapdev_attr(vm_paddr_t pa, vm_size_t size, int mode)
{
    struct devmap *d = malloc(sizeof(*d), M_NMXPAGE, M_WAITOK);
    if (d == NULL)
        return NULL;
    d->va = nmx_xnu_map_phys(pa, size, mode, &d->handle);
    if (d->va == NULL) {
        free(d, M_NMXPAGE);
        return NULL;
    }
    mtx_lock(&devmap_mtx);
    d->next = devmaps;
    devmaps = d;
    mtx_unlock(&devmap_mtx);
    return d->va;
}

void pmap_unmapdev(void *va, vm_size_t size)
{
    mtx_lock(&devmap_mtx);
    for (struct devmap **pp = &devmaps; *pp; pp = &(*pp)->next) {
        if ((*pp)->va == va) {
            struct devmap *d = *pp;
            *pp = d->next;
            mtx_unlock(&devmap_mtx);
            nmx_xnu_unmap(d->handle);
            free(d, M_NMXPAGE);
            return;
        }
    }
    mtx_unlock(&devmap_mtx);
}

/*
 * Cache attributes: a kext cannot change the cache type of an existing kernel mapping on macOS. The attribute is
 * recorded on the page and applied when the page is mapped (vmap, user mappings); system memory stays cached in
 * the kernel's own view, which is coherent for a discrete GPU because PCIe accesses to system RAM snoop the CPU
 * caches (an integrated NPU's need not: see pmap_page_set_memattr).
 */
int pmap_change_attr(void *va, vm_size_t size, int mode)
{
    return 0;
}

/*
 * A page leaving write-back is written out of the CPU caches first, as FreeBSD's pmap and Linux's set_pages_array_wc
 * do. "PCIe snoops" above holds for a discrete GPU, not for the NPU: an integrated NPU reads write-combined and
 * uncached buffers without snooping, so a line still in the cache (the page was zeroed through the cached view) is
 * invisible to it and can later be written back over what was written through the WC view. The first Arrow Lake NPU
 * firmware boot never answered, its firmware and log buffers being exactly such pages.
 */
void pmap_invalidate_cache_range(vm_offset_t sva, vm_offset_t eva);
void pmap_page_set_memattr(vm_page_t m, vm_memattr_t ma)
{
    if (ma != VM_MEMATTR_WRITE_BACK && m->md.pat_mode != ma && m->nmx_kva)
        pmap_invalidate_cache_range((vm_offset_t)m->nmx_kva, (vm_offset_t)m->nmx_kva + PAGE_SIZE);
    m->md.pat_mode = ma;
}

void pmap_page_set_memattr_noflush(vm_page_t m, vm_memattr_t ma)
{
    m->md.pat_mode = ma;
}

static void wbinvd_action(void *arg)
{
    wbinvd();
}

void pmap_invalidate_cache(void)
{
    extern void smp_rendezvous(void (*)(void *), void (*)(void *), void (*)(void *), void *);
    smp_rendezvous(NULL, wbinvd_action, NULL, NULL);
}

void pmap_invalidate_cache_range(vm_offset_t sva, vm_offset_t eva)
{
    for (vm_offset_t va = sva & ~(vm_offset_t)63; va < eva; va += 64)
        clflush(va);
    mfence();
}

void pmap_force_invalidate_cache_range(vm_offset_t sva, vm_offset_t eva)
{
    pmap_invalidate_cache_range(sva, eva);
}

void pmap_invalidate_cache_pages(vm_page_t *pages, int count)
{
    for (int i = 0; i < count; i++)
        if (pages[i]->nmx_kva)
            pmap_invalidate_cache_range((vm_offset_t)pages[i]->nmx_kva, (vm_offset_t)pages[i]->nmx_kva + PAGE_SIZE);
}

void pmap_copy_page(vm_page_t src, vm_page_t dst)
{
    memcpy(dst->nmx_kva, src->nmx_kva, PAGE_SIZE);
}

void pmap_zero_page(vm_page_t m)
{
    bzero(m->nmx_kva, PAGE_SIZE);
}

/* ---- accounting --------------------------------------------------------------------------------------------------- */

uint64_t nmx_physmem_pages(void)
{
    extern int sysctlbyname(const char *, void *, size_t *, void *, size_t);
    uint64_t bytes = 0;
    size_t len = sizeof(bytes);
    if (sysctlbyname("hw.memsize", &bytes, &len, NULL, 0) != 0)
        return 0;
    return bytes >> PAGE_SHIFT;
}

uint64_t nmx_free_pages(void)
{
    extern int sysctlbyname(const char *, void *, size_t *, void *, size_t);
    uint32_t n = 0;
    size_t len = sizeof(n);
    if (sysctlbyname("vm.page_free_count", &n, &len, NULL, 0) != 0)
        return 0;
    return n;
}

uint64_t nmx_pages_live(void)
{
    return pages_live;
}

void nmx_page_fini(void)
{
    if (pg_lock == NULL)
        return;
    if (pages_live)
        printf("NullMoth xkpi: %llu page(s) still held at unload\n", (unsigned long long)pages_live);
    /* fully free chunks go back; a chunk with pages still held is leaked deliberately rather than freed under them */
    struct backing *bk = chunks;
    chunks = NULL;
    while (bk) {
        struct backing *n = bk->next;
        if (bk->live == 0)
            backing_release(bk);
        bk = n;
    }
    free(reg_table, M_NMXPAGE);
    reg_table = NULL;
    lck_spin_free(pg_lock, nmx_lck_grp);
    pg_lock = NULL;
}
