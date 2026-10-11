/* FreeBSD pmap(9) on xnu, for the calls LinuxKPI makes: address translation, cache attributes and cache flushes. */
#ifndef NMXKPI_VM_PMAP_H
#define NMXKPI_VM_PMAP_H
#include <vm/vm.h>
#include <vm/vm_page.h>
vm_paddr_t pmap_kextract(vm_offset_t va);
#define vtophys(va) pmap_kextract((vm_offset_t)(va))
void *pmap_mapdev_attr(vm_paddr_t pa, vm_size_t size, int mode);
#define pmap_mapdev(pa, size) pmap_mapdev_attr((pa), (size), VM_MEMATTR_UNCACHEABLE)
void pmap_unmapdev(void *va, vm_size_t size);
int pmap_change_attr(void *va, vm_size_t size, int mode);
void pmap_page_set_memattr(vm_page_t m, vm_memattr_t ma);
void pmap_page_set_memattr_noflush(vm_page_t m, vm_memattr_t ma);
void pmap_invalidate_cache(void);
void pmap_invalidate_cache_pages(vm_page_t *pages, int count);
void pmap_invalidate_cache_range(vm_offset_t sva, vm_offset_t eva);
void pmap_force_invalidate_cache_range(vm_offset_t sva, vm_offset_t eva);
void pmap_copy_page(vm_page_t src, vm_page_t dst);
void pmap_zero_page(vm_page_t m);
void pmap_qenter(vm_offset_t va, vm_page_t *m, int count);
void pmap_qremove(vm_offset_t va, int count);
#define pmap_kenter(va, pa) do { } while (0)
#endif
