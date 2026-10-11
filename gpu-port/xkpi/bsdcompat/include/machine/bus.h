/* FreeBSD <machine/bus.h> (x86) on xnu: bus-space handles are kernel virtual addresses, accesses are plain volatile. */
#ifndef NMXKPI_MACHINE_BUS_H
#define NMXKPI_MACHINE_BUS_H
#include <sys/types.h>
typedef uint64_t bus_space_tag_t;
typedef uint64_t bus_space_handle_t;
#define X86_BUS_SPACE_IO 0
#define X86_BUS_SPACE_MEM 1
#define BUS_SPACE_MAXADDR 0xFFFFFFFFFFFFFFFFULL
#define BUS_SPACE_MAXADDR_32BIT 0xFFFFFFFFULL
#define BUS_SPACE_MAXADDR_24BIT 0x00FFFFFFULL
#define BUS_SPACE_MAXSIZE 0xFFFFFFFFFFFFFFFFULL
#define BUS_SPACE_MAXSIZE_32BIT 0xFFFFFFFFULL
#define BUS_SPACE_UNRESTRICTED (~0)
#define BUS_SPACE_BARRIER_READ 0x01
#define BUS_SPACE_BARRIER_WRITE 0x02
static inline void bus_space_barrier(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o, bus_size_t l, int f) { __asm__ __volatile__("" ::: "memory"); }
#define NMX_BS_RW(w, type) \
static inline type bus_space_read_##w(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o) { return *(volatile type *)(h + o); } \
static inline void bus_space_write_##w(bus_space_tag_t t, bus_space_handle_t h, bus_size_t o, type v) { *(volatile type *)(h + o) = v; }
NMX_BS_RW(1, uint8_t)
NMX_BS_RW(2, uint16_t)
NMX_BS_RW(4, uint32_t)
NMX_BS_RW(8, uint64_t)
#undef NMX_BS_RW
/*
 * bus_dma(9): a tag describes what a device can reach (address limit, alignment, boundary, segments); a map holds the
 * device-visible mappings of the buffers loaded into it, made through IODMACommand when the IOMMU is active and
 * identical to physical addresses when it is not (src/nmx_busdma.c over xnu/nmx_dma.cpp). Buffers the device
 * cannot reach are refused with EFBIG, never bounced: no GPU path loads memory outside its own DMA mask.
 */
typedef struct bus_dma_tag *bus_dma_tag_t;
typedef struct bus_dmamap *bus_dmamap_t;
typedef struct bus_dma_segment { bus_addr_t ds_addr; bus_size_t ds_len; } bus_dma_segment_t;
typedef int bus_dmasync_op_t;
#define BUS_DMASYNC_PREREAD 1
#define BUS_DMASYNC_POSTREAD 2
#define BUS_DMASYNC_PREWRITE 4
#define BUS_DMASYNC_POSTWRITE 8
#define BUS_DMA_WAITOK 0x00
#define BUS_DMA_NOWAIT 0x01
#define BUS_DMA_ALLOCNOW 0x02
#define BUS_DMA_COHERENT 0x04
#define BUS_DMA_ZERO 0x08
#define BUS_DMA_NOCACHE 0x100
typedef void bus_dmamap_callback_t(void *, bus_dma_segment_t *, int, int);
typedef int bus_dma_filter_t(void *, bus_addr_t);
typedef enum { BUS_DMA_LOCK, BUS_DMA_UNLOCK } bus_dma_lock_op_t;
typedef void bus_dma_lock_t(void *, bus_dma_lock_op_t);
struct nmx_device;
bus_dma_tag_t bus_get_dma_tag(struct nmx_device *dev);
int bus_dma_tag_create(bus_dma_tag_t parent, bus_size_t alignment, bus_addr_t boundary, bus_addr_t lowaddr,
                       bus_addr_t highaddr, bus_dma_filter_t *filter, void *filterarg, bus_size_t maxsize,
                       int nsegments, bus_size_t maxsegsz, int flags, bus_dma_lock_t *lockfunc, void *lockfuncarg,
                       bus_dma_tag_t *dmat);
int bus_dma_tag_destroy(bus_dma_tag_t dmat);
bool bus_dma_id_mapped(bus_dma_tag_t dmat, vm_paddr_t buf, bus_size_t buflen);
int bus_dmamap_create(bus_dma_tag_t dmat, int flags, bus_dmamap_t *mapp);
int bus_dmamap_destroy(bus_dma_tag_t dmat, bus_dmamap_t map);
int _bus_dmamap_load_phys(bus_dma_tag_t dmat, bus_dmamap_t map, vm_paddr_t buf, bus_size_t buflen, int flags,
                          bus_dma_segment_t *segs, int *segp);
int bus_dmamap_load(bus_dma_tag_t dmat, bus_dmamap_t map, void *buf, bus_size_t buflen,
                    bus_dmamap_callback_t *callback, void *callback_arg, int flags);
void bus_dmamap_unload(bus_dma_tag_t dmat, bus_dmamap_t map);
void bus_dmamap_sync(bus_dma_tag_t dmat, bus_dmamap_t map, bus_dmasync_op_t op);
int bus_dmamem_alloc(bus_dma_tag_t dmat, void **vaddr, int flags, bus_dmamap_t *mapp);
void bus_dmamem_free(bus_dma_tag_t dmat, void *vaddr, bus_dmamap_t map);
#endif
