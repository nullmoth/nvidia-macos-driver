/* SPDX-License-Identifier: GPL-2.0 */
/* Linux interfaces the NPU drivers use that LinuxKPI does not provide; force-included by cc-npu.sh. */
#ifndef _NMX_NPU_COMPAT_H_
#define _NMX_NPU_COMPAT_H_
#include <linux/sizes.h>
#include <linux/kfifo.h>
#ifndef SZ_128G
#define SZ_128G (128ULL << 30)
#endif
#ifndef SZ_256G
#define SZ_256G (256ULL << 30)
#endif
#ifndef EBADR
#define EBADR 53	/* Linux's "invalid request descriptor" */
#endif
/* empty the fifo (LinuxKPI's kfifo keeps count/first/last) */
#ifndef kfifo_reset
#define kfifo_reset(_kf) do { (_kf)->count = 0; (_kf)->first = 0; (_kf)->last = 0; } while (0)
#endif
#ifndef EBADFD
#define EBADFD 77	/* Linux's "file descriptor in bad state" */
#endif
#ifndef iscntrl
#define iscntrl(c) ((unsigned char)(c) < 0x20 || (c) == 0x7f)
#endif
#ifndef IOSYS_MAP_INIT_VADDR
#define IOSYS_MAP_INIT_VADDR(v) ((struct iosys_map){ .vaddr = (v), .is_iomem = false })
#endif
#ifndef lockdep_assert_irqs_disabled
#define lockdep_assert_irqs_disabled() do { } while (0)
#endif
/* xarray: LinuxKPI's xa_alloc takes its own lock, which is safe from any context the driver calls it in */
#ifndef xa_alloc_irq
#define xa_alloc_irq(xa, id, entry, limit, gfp) xa_alloc((xa), (id), (entry), (limit), (gfp))
#endif
/* the port requests interrupts enabled; the NPU masks its own interrupt sources until the driver arms them */
#ifndef IRQF_NO_AUTOEN
#define IRQF_NO_AUTOEN 0
#endif
/* runtime PM: macOS keeps the device powered while the driver runs, so it is never runtime-suspended */
static inline bool pm_runtime_status_suspended(struct device *dev) { return false; }
static inline int pm_runtime_set_suspended(struct device *dev) { return 0; }
#ifndef vmf_insert_pfn
/* fault handlers run with the VM object locked (the port's mmap path, as FreeBSD's pager): the locked insert */
#define vmf_insert_pfn(vma, addr, pfn) lkpi_vmf_insert_pfn_prot_locked((vma), (addr), (pfn), (vma)->vm_page_prot)
#endif

/* ---- newer-kernel interfaces AMD's XDNA (Linux 7.3) uses on the 6.14 layer ---- */
#ifndef COMPLETION_INITIALIZER_ONSTACK
#define COMPLETION_INITIALIZER_ONSTACK(work) (*({ init_completion(&(work)); &(work); }))
#endif
#ifndef BIT_U64
#define BIT_U64(n) (1ULL << (n))
#endif
/* PCI functions have 6 standard BARs here (LinuxKPI keeps no ROM/IOV/bridge resources) */
#ifndef PCI_NUM_RESOURCES
#define PCI_NUM_RESOURCES 6
#endif
#ifndef CAP_IPC_LOCK
#define CAP_IPC_LOCK 14
#endif
#ifndef FOLL_LONGTERM
#define FOLL_LONGTERM 0x100
#endif
/* no memlock accounting on macOS: the limit never constrains a driver */
#ifndef RLIMIT_MEMLOCK
#define RLIMIT_MEMLOCK 6
#endif
#define rlimit(r) (~0UL)
#define mmput_async(mm) mmput(mm)
/* 7.x typed allocation helpers; an optional last argument is the gfp mask (GFP_KERNEL when absent) */
#define NMX_GFP_(g, ...) g
#define NMX_GFP(...) NMX_GFP_(__VA_ARGS__ __VA_OPT__(,) GFP_KERNEL)
#define kzalloc_obj(v, ...) kzalloc(sizeof(v), NMX_GFP(__VA_ARGS__))
#define kmalloc_obj(v, ...) kmalloc(sizeof(v), NMX_GFP(__VA_ARGS__))
#define kzalloc_objs(v, n, ...) kcalloc((n), sizeof(v), NMX_GFP(__VA_ARGS__))
#define kmalloc_objs(v, n, ...) kmalloc_array((n), sizeof(v), NMX_GFP(__VA_ARGS__))
#define kvzalloc_objs(v, n, ...) kvcalloc((n), sizeof(v), NMX_GFP(__VA_ARGS__))
#define kvmalloc_objs(v, n, ...) kvmalloc_array((n), sizeof(v), NMX_GFP(__VA_ARGS__))
/* xarray range walks over LinuxKPI's xa_for_each (entries outside the range are skipped; break leaves the walk) */
#define xa_for_each_range(xa, index, entry, start, last) \
	xa_for_each((xa), (index), (entry)) if ((index) < (start) || (index) > (last)) {} else
#define xa_for_each_start(xa, index, entry, start) \
	xa_for_each((xa), (index), (entry)) if ((index) < (start)) {} else
#ifndef SYSTEM_SLEEP_PM_OPS
#define SYSTEM_SLEEP_PM_OPS(s, r) SET_SYSTEM_SLEEP_PM_OPS(s, r)
#endif
static inline int pci_vfs_assigned(struct pci_dev *pdev) { return 0; }
#ifndef virt_to_phys
#define virt_to_phys(p) ((phys_addr_t)DMAP_TO_PHYS(p))
#endif
#ifndef GENMASK_U64
#define GENMASK_U64(h, l) GENMASK_ULL(h, l)
#endif
#define can_do_mlock() true
#ifndef RUNTIME_PM_OPS
#define RUNTIME_PM_OPS(s, r, i) SET_RUNTIME_PM_OPS(s, r, i)
#endif
#define MODULE_IMPORT_NS(ns)
/* drm-kmod builds these (drm_gem.c) without declaring them */
struct drm_gem_object;
struct iosys_map;
int drm_gem_vmap(struct drm_gem_object *obj, struct iosys_map *map);
void drm_gem_vunmap(struct drm_gem_object *obj, struct iosys_map *map);
/*
 * Pre-faulting and batch page insertion for user mappings: the port's mmap ioctl already fills every page of a mapping
 * through the object's fault handler before returning it, so these have nothing left to do.
 */
#define handle_mm_fault(vma, addr, flags, regs) ((vm_fault_t)0)
static inline int vm_insert_pages(struct vm_area_struct *vma, unsigned long addr, struct page **pages, unsigned long *num) { *num = 0; return 0; }
/* fdinfo memory lines ("drm-<stat>-<region>: <size>") */
#define drm_fdinfo_print_size(p, prefix, stat, region, sz) \
	drm_printf((p), "%s-%s-%s:\t%llu\n", (prefix), (stat), (region), (unsigned long long)(sz))
/* an ordered workqueue destroyed with the DRM device */
static inline void nmx_drmm_wq_release(struct drm_device *dev, void *wq) { destroy_workqueue(wq); }
#define drmm_alloc_ordered_workqueue(dev, fmt, flags, ...) ({					\
	struct workqueue_struct *_wq = alloc_ordered_workqueue(fmt, flags, ##__VA_ARGS__);	\
	if (_wq == NULL) _wq = ERR_PTR(-ENOMEM);						\
	else if (drmm_add_action_or_reset((dev), nmx_drmm_wq_release, _wq)) _wq = ERR_PTR(-ENOMEM); \
	_wq; })
struct amdxdna_dev;
void nmx_xdna_carveout_setup(struct amdxdna_dev *xdna);	/* nmx_xdna_glue.c */

/* ---- newer-kernel interfaces Intel's ivpu (Linux 7.3) uses on the 6.14 layer ---- */
/* runtime PM is never enabled here (see pm_runtime_status_suspended above) */
#define pm_runtime_enabled(dev) false
#ifndef in_range
#define in_range(val, start, len) ((val) >= (start) && (val) - (start) < (len))
#endif
/* 6.14's drm_gem_shmem_vmap/vunmap already require the reservation lock: they are the later _locked forms */
#define drm_gem_shmem_vmap_locked(shmem, map) drm_gem_shmem_vmap((shmem), (map))
#define drm_gem_shmem_vunmap_locked(shmem, map) drm_gem_shmem_vunmap((shmem), (map))
#define drm_gem_is_imported(obj) ((obj)->import_attach != NULL)
/* LinuxKPI has no disabled-work state: cancel and wait is the nearest form (the callers never queue it again) */
#define disable_work_sync(w) cancel_work_sync(w)
#define system_dfl_wq system_unbound_wq
/* the ECC capability MSR is read only for 50xx NPUs; an unreadable MSR means "no ECC", which the driver handles */
#ifndef MSR_INTEGRITY_CAPS
#define MSR_INTEGRITY_CAPS 0x2d9
#endif
#define rdmsrq_safe(msr, p) (-EIO)
/* the opener's uid, logged with each client; the port's files carry no Linux credentials */
#define current_uid() ((struct { uid_t val; }){ .val = 0 })
#endif
