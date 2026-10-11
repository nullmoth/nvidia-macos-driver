/* FreeBSD <vm/vm_object.h> on xnu: LinuxKPI's shmem and mmap code keys pages by (object, index). */
#ifndef NMXKPI_VM_VM_OBJECT_H
#define NMXKPI_VM_VM_OBJECT_H
#include <vm/vm.h>
#include <sys/_mutex.h>
struct vm_object {
    struct mtx lock;
    objtype_t type;
    vm_pindex_t size;
    int ref_count;
    void *handle;
    void *nmx_pages;                /* index -> vm_page array of 'size' entries, owned by src/nmx_page.c */
    int resident_page_count;        /* pages present */
    struct { void *devp_pglist; } un_pager;
};
#define VM_OBJECT_WLOCK(o) mtx_lock(&(o)->lock)
#define VM_OBJECT_WUNLOCK(o) mtx_unlock(&(o)->lock)
#define VM_OBJECT_RLOCK(o) mtx_lock(&(o)->lock)
#define VM_OBJECT_RUNLOCK(o) mtx_unlock(&(o)->lock)
#define VM_OBJECT_ASSERT_WLOCKED(o) do { } while (0)
#define VM_OBJECT_ASSERT_LOCKED(o) do { } while (0)
vm_object_t vm_object_allocate(objtype_t type, vm_pindex_t size);
void vm_object_deallocate(vm_object_t object);
void vm_object_reference(vm_object_t object);
vm_page_t vm_page_lookup(vm_object_t object, vm_pindex_t pindex);
vm_page_t vm_page_grab(vm_object_t object, vm_pindex_t pindex, int allocflags);
int vm_page_grab_valid_unlocked(vm_page_t *mp, vm_object_t object, vm_pindex_t pindex, int allocflags);
/* object locked: the page at pindex, made (zero-filled) when absent; VM_PAGER_OK or VM_PAGER_FAIL */
int vm_page_grab_valid(vm_page_t *mp, vm_object_t object, vm_pindex_t pindex, int allocflags);
void vm_object_page_remove(vm_object_t object, vm_pindex_t start, vm_pindex_t end, int options);
#define OBJPR_CLEANONLY 0x1
#define OBJPR_NOTMAPPED 0x2
#endif
