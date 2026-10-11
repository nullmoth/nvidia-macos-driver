/*
 * bus_dma(9) for the port. A tag records what one device can reach; a map holds every mapping loaded into it (a
 * scatter-gather list loads one entry at a time into the same map) and releases them all on unload.
 *
 * Device-visible addresses come from xnu/nmx_dma.cpp: through IODMACommand on the system IOMapper when an IOMMU
 * is active, otherwise the physical address. Nothing is bounced: memory outside a tag's address limit, or a
 * mapping that would break its alignment or boundary, is refused (EFBIG / EINVAL) and the caller sees a failed map,
 * never a silently wrong address.
 *
 * x86 PCIe DMA is cache-coherent, so a sync is an ordering fence only.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/bus.h>
#include <machine/bus.h>
#include <vm/vm.h>
#include <vm/vm_extern.h>
#include <vm/pmap.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_BUSDMA, "busdma", "bus_dma tags and maps");

struct bus_dma_tag {
    void *pci;                      /* the IOPCIDevice the mappings are made for */
    bus_size_t alignment;
    bus_addr_t boundary;
    bus_addr_t lowaddr;             /* highest device address the device can use */
    bus_size_t maxsize;
    int nsegments;
    bus_size_t maxsegsz;
    int flags;
    int is_root;
};

#define MAP_HANDLES_INLINE 4

struct bus_dmamap {
    int nloaded;                    /* segments loaded since the last unload */
    int cap;
    void **handles;                 /* one nmx_xnu_dma_map handle per segment (NULL for identity mappings) */
    void *inline_handles[MAP_HANDLES_INLINE];
    void *mem;                      /* bus_dmamem_alloc memory owned by this map */
    bus_size_t mem_size;
};

/* the root tag of a PCI device (sys/bus.h bus_get_dma_tag hands it out; nmx_bus.c owns its lifetime) */
bus_dma_tag_t nmx_busdma_root_tag(void *pci)
{
    struct bus_dma_tag *t = malloc(sizeof(*t), M_BUSDMA, M_WAITOK | M_ZERO);
    if (t == NULL)
        return NULL;
    t->pci = pci;
    t->alignment = 1;
    t->lowaddr = BUS_SPACE_MAXADDR;
    t->maxsize = BUS_SPACE_MAXSIZE;
    t->nsegments = INT_MAX;
    t->maxsegsz = BUS_SPACE_MAXSIZE;
    t->is_root = 1;
    return t;
}

void nmx_busdma_root_free(bus_dma_tag_t t)
{
    if (t != NULL && t->is_root)
        free(t, M_BUSDMA);
}

int bus_dma_tag_create(bus_dma_tag_t parent, bus_size_t alignment, bus_addr_t boundary, bus_addr_t lowaddr,
                       bus_addr_t highaddr, bus_dma_filter_t *filter, void *filterarg, bus_size_t maxsize,
                       int nsegments, bus_size_t maxsegsz, int flags, bus_dma_lock_t *lockfunc, void *lockfuncarg,
                       bus_dma_tag_t *dmat)
{
    *dmat = NULL;
    /* alignment and boundary are powers of two (FreeBSD rejects anything else the same way) */
    if (alignment == 0 || (alignment & (alignment - 1)) != 0 || (boundary & (boundary - 1)) != 0 || filter != NULL)
        return EINVAL;
    /* a segment never crosses a boundary, so none is longer than one (FreeBSD clamps the same way) */
    if (boundary != 0 && boundary < maxsegsz)
        maxsegsz = boundary;
    struct bus_dma_tag *t = malloc(sizeof(*t), M_BUSDMA, (flags & BUS_DMA_NOWAIT ? M_NOWAIT : M_WAITOK) | M_ZERO);
    if (t == NULL)
        return ENOMEM;
    t->pci = parent ? parent->pci : NULL;
    t->alignment = alignment;
    t->boundary = boundary;
    t->lowaddr = lowaddr;
    t->maxsize = maxsize;
    t->nsegments = nsegments < 1 ? 1 : nsegments;
    t->maxsegsz = maxsegsz;
    t->flags = flags;
    if (parent != NULL) {
        /* a child never reaches further than its parent */
        if (parent->lowaddr < t->lowaddr)
            t->lowaddr = parent->lowaddr;
        if (parent->alignment > t->alignment)
            t->alignment = parent->alignment;
        if (parent->boundary != 0 && (t->boundary == 0 || parent->boundary < t->boundary))
            t->boundary = parent->boundary;
    }
    *dmat = t;
    return 0;
}

int bus_dma_tag_destroy(bus_dma_tag_t dmat)
{
    if (dmat != NULL && !dmat->is_root)
        free(dmat, M_BUSDMA);
    return 0;
}

/* true when the device address of [buf, buf+len) is the physical address and the tag's limits all hold */
bool bus_dma_id_mapped(bus_dma_tag_t dmat, vm_paddr_t buf, bus_size_t buflen)
{
    if (nmx_xnu_iommu_active() || buflen == 0)
        return false;
    if (buf + buflen - 1 > dmat->lowaddr)
        return false;
    if ((buf & (dmat->alignment - 1)) != 0)
        return false;
    if (dmat->boundary != 0 && (buf / dmat->boundary) != ((buf + buflen - 1) / dmat->boundary))
        return false;
    return true;
}

int bus_dmamap_create(bus_dma_tag_t dmat, int flags, bus_dmamap_t *mapp)
{
    struct bus_dmamap *m = malloc(sizeof(*m), M_BUSDMA, (flags & BUS_DMA_NOWAIT ? M_NOWAIT : M_WAITOK) | M_ZERO);
    if (m == NULL)
        return ENOMEM;
    m->handles = m->inline_handles;
    m->cap = MAP_HANDLES_INLINE;
    *mapp = m;
    return 0;
}

int bus_dmamap_destroy(bus_dma_tag_t dmat, bus_dmamap_t map)
{
    if (map == NULL)
        return 0;
    if (map->nloaded != 0)
        return EBUSY;   /* FreeBSD's answer for a map that is still loaded */
    if (map->handles != map->inline_handles)
        free(map->handles, M_BUSDMA);
    free(map, M_BUSDMA);
    return 0;
}

static int map_reserve(bus_dmamap_t map, int flags)
{
    if (map->nloaded < map->cap)
        return 0;
    int ncap = map->cap * 2;
    void **nh = malloc(sizeof(void *) * (size_t)ncap, M_BUSDMA, flags & BUS_DMA_NOWAIT ? M_NOWAIT : M_WAITOK);
    if (nh == NULL)
        return ENOMEM;
    memcpy(nh, map->handles, sizeof(void *) * (size_t)map->nloaded);
    if (map->handles != map->inline_handles)
        free(map->handles, M_BUSDMA);
    map->handles = nh;
    map->cap = ncap;
    return 0;
}

/* maps one physically contiguous range as one segment */
static int load_one(bus_dma_tag_t dmat, bus_dmamap_t map, vm_paddr_t pa, bus_size_t len, int flags, bus_addr_t *out)
{
    if (len == 0 || len > dmat->maxsegsz || len > dmat->maxsize)
        return EFBIG;
    int err = map_reserve(map, flags);
    if (err)
        return err;
    uint64_t addr = 0;
    void *h = NULL;
    err = nmx_xnu_dma_map(dmat->pci, pa, (size_t)len, dmat->lowaddr, &addr, &h);
    if (err)
        return err;
    if ((addr & (dmat->alignment - 1)) != 0 ||
        (dmat->boundary != 0 && (addr / dmat->boundary) != ((addr + len - 1) / dmat->boundary)) ||
        addr + len - 1 > dmat->lowaddr) {
        nmx_xnu_dma_unmap(h);
        return EFBIG;
    }
    map->handles[map->nloaded++] = h;
    *out = addr;
    return 0;
}

int _bus_dmamap_load_phys(bus_dma_tag_t dmat, bus_dmamap_t map, vm_paddr_t buf, bus_size_t buflen, int flags,
                          bus_dma_segment_t *segs, int *segp)
{
    if (*segp + 1 >= dmat->nsegments)
        return EFBIG;
    bus_addr_t addr;
    int err = load_one(dmat, map, buf, buflen, flags, &addr);
    if (err)
        return err;
    (*segp)++;
    if (segs != NULL) {
        segs[*segp].ds_addr = addr;
        segs[*segp].ds_len = buflen;
    }
    return 0;
}

/*
 * A kernel virtual buffer: split into physically contiguous runs (page by page), each one a segment, and the
 * callback runs before return (BUS_DMA_NOWAIT semantics; nothing is ever deferred).
 */
int bus_dmamap_load(bus_dma_tag_t dmat, bus_dmamap_t map, void *buf, bus_size_t buflen,
                    bus_dmamap_callback_t *callback, void *callback_arg, int flags)
{
    int nmax = dmat->nsegments > 64 ? 64 : dmat->nsegments;
    bus_dma_segment_t *segs = malloc(sizeof(*segs) * (size_t)nmax, M_BUSDMA, flags & BUS_DMA_NOWAIT ? M_NOWAIT : M_WAITOK);
    if (segs == NULL)
        return ENOMEM;
    int nseg = -1, err = 0;
    vm_offset_t va = (vm_offset_t)buf;
    bus_size_t left = buflen;
    while (left > 0 && err == 0) {
        vm_paddr_t pa = vtophys(va);
        if (pa == 0) {
            err = EFAULT;
            break;
        }
        bus_size_t run = PAGE_SIZE - (va & PAGE_MASK);
        if (run > left)
            run = left;
        /* extend the run while the next page follows physically and the segment limit allows */
        while (run < left && vtophys(va + run) == pa + run && run + PAGE_SIZE <= dmat->maxsegsz)
            run += (left - run) < PAGE_SIZE ? (left - run) : PAGE_SIZE;
        if (nseg + 1 >= nmax) {
            err = EFBIG;
            break;
        }
        err = _bus_dmamap_load_phys(dmat, map, pa, run, flags, segs, &nseg);
        va += run;
        left -= run;
    }
    if (err)
        bus_dmamap_unload(dmat, map);
    if (callback)
        callback(callback_arg, segs, err ? 0 : nseg + 1, err);
    free(segs, M_BUSDMA);
    return err;
}

void bus_dmamap_unload(bus_dma_tag_t dmat, bus_dmamap_t map)
{
    if (map == NULL)
        return;
    for (int i = 0; i < map->nloaded; i++)
        if (map->handles[i])
            nmx_xnu_dma_unmap(map->handles[i]);
    map->nloaded = 0;
}

void bus_dmamap_sync(bus_dma_tag_t dmat, bus_dmamap_t map, bus_dmasync_op_t op)
{
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
}

int bus_dmamem_alloc(bus_dma_tag_t dmat, void **vaddr, int flags, bus_dmamap_t *mapp)
{
    *vaddr = NULL;
    int err = bus_dmamap_create(dmat, flags, mapp);
    if (err)
        return err;
    bus_size_t size = round_page(dmat->maxsize);
    u_long align = dmat->alignment > PAGE_SIZE ? (u_long)dmat->alignment : PAGE_SIZE;
    void *mem = kmem_alloc_contig(size, (flags & BUS_DMA_NOWAIT ? M_NOWAIT : M_WAITOK) | (flags & BUS_DMA_ZERO ? M_ZERO : 0),
                                  0, dmat->lowaddr, align, dmat->boundary, VM_MEMATTR_DEFAULT);
    if (mem == NULL) {
        bus_dmamap_destroy(dmat, *mapp);
        *mapp = NULL;
        return ENOMEM;
    }
    (*mapp)->mem = mem;
    (*mapp)->mem_size = size;
    *vaddr = mem;
    return 0;
}

void bus_dmamem_free(bus_dma_tag_t dmat, void *vaddr, bus_dmamap_t map)
{
    if (map == NULL)
        return;
    bus_dmamap_unload(dmat, map);
    if (map->mem != NULL)
        kmem_free(map->mem, map->mem_size);
    map->mem = NULL;
    bus_dmamap_destroy(dmat, map);
}
