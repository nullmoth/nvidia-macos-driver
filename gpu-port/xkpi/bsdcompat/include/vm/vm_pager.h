/* FreeBSD <vm/vm_pager.h>: pager return codes LinuxKPI's fault path uses. */
#pragma once
#define VM_PAGER_OK 0
#define VM_PAGER_BAD 1
#define VM_PAGER_FAIL 2
#define VM_PAGER_PEND 3
#define VM_PAGER_ERROR 4
#define VM_PAGER_AGAIN 5
#include <vm/vm.h>
struct ucred;
/* an anonymous object of 'size' bytes (the only kind LinuxKPI asks for: shmem files, OBJT_SWAP) */
vm_object_t vm_pager_allocate(objtype_t type, void *handle, vm_ooffset_t size, vm_prot_t prot, vm_ooffset_t off,
                              struct ucred *cred);
