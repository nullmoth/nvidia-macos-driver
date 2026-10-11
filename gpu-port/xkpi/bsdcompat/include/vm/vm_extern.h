/* FreeBSD <vm/vm_extern.h> on xnu: kernel address space allocation (kva_*) and wired kernel memory (kmem_*). */
#ifndef NMXKPI_VM_VM_EXTERN_H
#define NMXKPI_VM_VM_EXTERN_H
#include <vm/vm.h>
vm_offset_t kva_alloc(vm_size_t size);
void kva_free(vm_offset_t addr, vm_size_t size);
void *kmem_malloc(vm_size_t size, int flags);
void kmem_free(void *addr, vm_size_t size);
void *kmem_alloc_contig(vm_size_t size, int flags, vm_paddr_t low, vm_paddr_t high, u_long alignment, vm_paddr_t boundary, vm_memattr_t memattr);
int vm_fault_disable_pagefaults(void);
void vm_fault_enable_pagefaults(int save);
void *kmem_alloc_attr(vm_size_t size, int flags, vm_paddr_t low, vm_paddr_t high, vm_memattr_t memattr);
#endif
