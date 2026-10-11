/*
 * FreeBSD <vm/vm.h> on xnu.
 *
 * Memory model of this port: xnu gives a kext no direct map of physical memory and no global page array, so every
 * page LinuxKPI hands out is a struct vm_page created by the port (src/nmx_page.c). It records the page's physical
 * address and the kernel address it is mapped at, and is registered by physical address so PHYS_TO_VM_PAGE and
 * PHYS_TO_DMAP work for every page the GPU drivers allocate. Pages are wired: macOS never pages them out.
 */
#ifndef NMXKPI_VM_VM_H
#define NMXKPI_VM_VM_H
#include <sys/types.h>
#include <mach/vm_prot.h>
#include <machine/vm.h>
typedef struct vm_object *vm_object_t;
typedef struct vm_page *vm_page_t;
/* vm_map_t and vm_inherit_t are xnu's (mach/vm_types.h) */
typedef u_char objtype_t;
#define OBJT_DEFAULT 0
#define OBJT_SWAP 1
/* byte offset <-> page index within an object */
#define OFF_TO_IDX(off) ((vm_pindex_t)(((vm_ooffset_t)(off)) >> PAGE_SHIFT))
#define IDX_TO_OFF(idx) (((vm_ooffset_t)(idx)) << PAGE_SHIFT)
#define OBJT_VNODE 2
#define OBJT_DEVICE 3
#define OBJT_PHYS 4
#define OBJT_DEAD 5
#define OBJT_SG 6
#define OBJT_MGTDEVICE 7
#define VM_PROT_RW (VM_PROT_READ | VM_PROT_WRITE)
#define VM_PROT_RWX (VM_PROT_READ | VM_PROT_WRITE | VM_PROT_EXECUTE)
#define VM_PROT_COPY 0x08
#endif
