/*
 * FreeBSD <dev/agp/agp_i810.h>: the GTT of Intel graphics before Gen6, reached through agp(4). The port drives Gen9
 * and later, whose GTT i915 runs itself, so these entry points exist only to link and report that no such GTT is
 * present (src/nmx_agp.c). Declarations as FreeBSD's.
 */
#ifndef NMXKPI_DEV_AGP_AGP_I810_H
#define NMXKPI_DEV_AGP_AGP_I810_H
#include <sys/param.h>
#include <sys/bus.h>
#include <vm/vm.h>
#include <vm/vm_page.h>
#include <dev/agp/agpvar.h>
struct sglist;
#define AGP_DCACHE_MEMORY 1
#define AGP_PHYS_MEMORY 2
#define AGP_USER_CACHED_MEMORY_LLC_MLC (AGP_USER_TYPES + 2)
#define AGP_USER_UNCACHED_MEMORY (AGP_USER_TYPES + 4)
#define AGP_USER_CACHED_MEMORY_GFDT (1 << 3)
struct intel_gtt {
    unsigned int stolen_size;
    unsigned int gtt_total_entries;
    unsigned int gtt_mappable_entries;
    unsigned int needs_dmar : 1;
    unsigned int do_idle_maps : 1;
    vm_paddr_t scratch_page_dma;
    vm_page_t scratch_page;
    uint32_t *gtt;
    bus_addr_t gma_bus_addr;
};
struct intel_gtt *intel_gtt_get(void);
int intel_gtt_chipset_flush(void);
void intel_gtt_unmap_memory(struct sglist *sg_list);
void intel_gtt_clear_range(u_int first_entry, u_int num_entries);
void intel_gtt_install_pte(u_int index, vm_paddr_t addr, u_int flags);
int intel_gtt_map_memory(vm_page_t *pages, u_int num_entries, struct sglist **sg_list);
void intel_gtt_insert_sg_entries(struct sglist *sg_list, u_int pg_start, u_int flags);
void intel_gtt_insert_pages(u_int first_entry, u_int num_entries, vm_page_t *pages, u_int flags);
vm_paddr_t intel_gtt_read_pte_paddr(u_int entry);
u_int32_t intel_gtt_read_pte(u_int entry);
device_t intel_gtt_get_bridge_device(void);
void intel_gtt_write(u_int entry, uint32_t val);
void *intel_gtt_get_registers(void);
void _intel_gtt_get(size_t *gtt_total, size_t *stolen_size, unsigned long *mappable_end);
#endif
