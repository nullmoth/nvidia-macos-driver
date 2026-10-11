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
/* bus_dma: a DMA tag/map describe a buffer the device reaches through IODMACommand (src/nmx_busdma.cpp) */
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
#endif
