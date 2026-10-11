/* The pre-Gen6 Intel GTT entry points (dev/agp/agp_i810.h): no such GTT exists on a machine the port drives. */
#include <sys/param.h>
#include <sys/systm.h>
#include <dev/agp/agp_i810.h>

struct intel_gtt *intel_gtt_get(void) { return NULL; }
int intel_gtt_chipset_flush(void) { return 0; }
void intel_gtt_unmap_memory(struct sglist *sg_list) { }
void intel_gtt_clear_range(u_int first_entry, u_int num_entries) { }
void intel_gtt_install_pte(u_int index, vm_paddr_t addr, u_int flags) { }
int intel_gtt_map_memory(vm_page_t *pages, u_int num_entries, struct sglist **sg_list) { return ENODEV; }
void intel_gtt_insert_sg_entries(struct sglist *sg_list, u_int pg_start, u_int flags) { }
void intel_gtt_insert_pages(u_int first_entry, u_int num_entries, vm_page_t *pages, u_int flags) { }
vm_paddr_t intel_gtt_read_pte_paddr(u_int entry) { return 0; }
u_int32_t intel_gtt_read_pte(u_int entry) { return 0; }
device_t intel_gtt_get_bridge_device(void) { return NULL; }
void intel_gtt_write(u_int entry, uint32_t val) { }
void *intel_gtt_get_registers(void) { return NULL; }
void _intel_gtt_get(size_t *gtt_total, size_t *stolen_size, unsigned long *mappable_end)
{
    *gtt_total = 0;
    *stolen_size = 0;
    *mappable_end = 0;
}
