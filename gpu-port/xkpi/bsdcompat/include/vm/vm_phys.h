/* FreeBSD <vm/vm_phys.h>: fictitious (device memory) page ranges; the port gives BAR pages records on demand. */
#pragma once
#include <vm/vm.h>
int vm_phys_fictitious_reg_range(vm_paddr_t start, vm_paddr_t end, vm_memattr_t memattr);
void vm_phys_fictitious_unreg_range(vm_paddr_t start, vm_paddr_t end);
