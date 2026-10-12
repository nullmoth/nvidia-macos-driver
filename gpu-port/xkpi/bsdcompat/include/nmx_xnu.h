/*
 * The boundary between the FreeBSD/LinuxKPI side of the port (C, compiled with the compatibility headers) and the
 * IOKit side (C++, compiled with Apple's Kernel.framework headers only). Only plain types cross it, so neither side
 * ever sees the other's definition of a shared name (struct proc, struct vnode, malloc, ...).
 */
#ifndef NMXKPI_NMX_XNU_H
#define NMXKPI_NMX_XNU_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* cache modes for mappings (match FreeBSD's VM_MEMATTR_* values in machine/vm.h) */
#define NMX_CACHE_UC 0x00
#define NMX_CACHE_WC 0x01
#define NMX_CACHE_WT 0x04
#define NMX_CACHE_WB 0x06

/* ---- wired memory (nmx_vm.cpp) ---------------------------------------------------------------------------------- */

/*
 * Allocates 'size' bytes of wired kernel memory. contiguous != 0 asks for one physically contiguous range below
 * 'max_phys' aligned to 'align'. Returns the kernel address and an opaque handle for nmx_xnu_mem_free, or NULL.
 */
void *nmx_xnu_mem_alloc(size_t size, int contiguous, uint64_t max_phys, size_t align, void **handle);
void nmx_xnu_mem_free(void *handle);
/* physical address of a kernel virtual address (0 if unmapped) */
uint64_t nmx_xnu_kva_to_phys(const void *va);

/* maps physical memory into the kernel with a cache mode; returns the address and an unmap handle */
void *nmx_xnu_map_phys(uint64_t pa, size_t size, int cache, void **handle);
/* maps an array of single physical pages contiguously into the kernel (vmap) */
void *nmx_xnu_map_pages(const uint64_t *pages, size_t npages, int cache, void **handle);
void nmx_xnu_unmap(void *handle);

/* ---- another process's memory (nmx_mm.cpp) -------------------------------------------------------------------------- */

/*
 * The port keeps one Linux mm_struct per process. These record which Mach task an mm belongs to (called from the
 * process's own thread when its mm is created, and when the mm is freed), so a driver worker thread can later read
 * or write that process's memory (Linux: kthread_use_mm + copy_to_user). The caller must hold an mm_users reference
 * (mmget_not_zero), which keeps a thread of the process, and so its task, alive for the copy.
 */
void nmx_xnu_mm_note_task(void *mm);
void nmx_xnu_mm_forget(void *mm);
/* copies len bytes between kbuf and uaddr in mm's process; to_user != 0 writes it. Returns 0 or an errno. */
int nmx_xnu_mm_copy(void *mm, uint64_t uaddr, void *kbuf, size_t len, int to_user);

/*
 * Wires npages of user memory at uaddr in mm's process (mm NULL: the calling process) and returns each page's
 * physical address, plus a handle that keeps the range wired until nmx_xnu_user_unwire. Returns 0 or an errno.
 */
int nmx_xnu_user_wire(void *mm, uint64_t uaddr, size_t npages, int write, uint64_t *phys, void **handle);
void nmx_xnu_user_unwire(void *handle);

/* ---- DMA (nmx_dma.cpp) -------------------------------------------------------------------------------------------- */

/*
 * device-visible address for a physical range of 'pci' (an IOPCIDevice *); handle releases the mapping (NULL when no
 * IOMMU is active and the address is the physical one). Returns 0, ENOMEM, or EFBIG (beyond dma_mask / split).
 */
int nmx_xnu_dma_map(void *pci, uint64_t pa, size_t size, uint64_t dma_mask, uint64_t *dma_addr, void **handle);
void nmx_xnu_dma_unmap(void *handle);
int nmx_xnu_iommu_active(void);

/* ---- PCI (nmx_pci.cpp) -------------------------------------------------------------------------------------------- */

uint32_t nmx_xnu_pci_read(void *pci, int reg, int width);
void nmx_xnu_pci_write(void *pci, int reg, uint32_t val, int width);
void nmx_xnu_pci_location(void *pci, uint32_t *domain, uint8_t *bus, uint8_t *dev, uint8_t *func);
/* maps BAR at config register 'bar_reg' (0x10..0x24); returns the kernel address, its physical base and size */
void *nmx_xnu_pci_map_bar(void *pci, int bar_reg, int cache, uint64_t *phys, uint64_t *size, void **handle);
/* BAR at config register 'bar_reg' as IOKit assigned it (never sized by writing the BAR); 0 = no such BAR */
int nmx_xnu_pci_bar_info(void *pci, int bar_reg, uint64_t *phys, uint64_t *size, int *is_io);
/* message-signalled interrupts: number available, and handler registration (handler runs on a work loop thread) */
int nmx_xnu_pci_msi_count(void *pci);
int nmx_xnu_pci_irq_setup(void *pci, int vector, void (*handler)(void *), void *arg, void **cookie);
void nmx_xnu_pci_irq_teardown(void *cookie);
/* returns once a handler running on this source has finished */
void nmx_xnu_pci_irq_drain(void *cookie);
void nmx_xnu_pci_irq_enable(void *cookie, int on);
/* the IOPCIDevice at domain:bus:dev.func, or NULL */
void *nmx_xnu_pci_find(uint32_t domain, uint8_t bus, uint8_t dev, uint8_t func);
/* the device's expansion ROM image (VBIOS) as the firmware left it, copied into 'buf'; returns its length or 0 */
size_t nmx_xnu_pci_rom(void *pci, void *buf, size_t max);

/* ---- firmware files (nmx_firmware.cpp) ---------------------------------------------------------------------------- */

/* loads a GPU firmware file by its Linux name (e.g. "amdgpu/psp_13_0_4_toc.bin"); caller frees with nmx_xnu_firmware_free */
int nmx_xnu_firmware_load(const char *name, void **data, size_t *size);
void nmx_xnu_firmware_free(void *data, size_t size);
/*
 * Waits, up to max_secs, until the firmware folder can be looked up. A driver that attaches during boot matching
 * runs before the root and Data volumes are mounted, and mounting them waits for matching to settle: a firmware
 * read at that point never returns and the Mac stops at the boot logo (first NMXNPU boot on an Arrow Lake NPU,
 * 10-11). Attach from a thread after this returns 0; ETIMEDOUT when the folder never appears.
 */
int nmx_xnu_wait_firmware_root(int max_secs);

/* ---- ACPI (nmx_acpi.cpp) ------------------------------------------------------------------------------------------ */

/* copies the ACPI table with this 4-character signature (instance 1..n); returns its length or 0 */
size_t nmx_xnu_acpi_table(const char *sig, int instance, void *buf, size_t max);
/*
 * ACPI namespace nodes (device objects) are retained IOACPIPlatformDevice references: every function returning
 * one hands the caller a reference to drop with nmx_xnu_acpi_node_put. Methods and data objects are named
 * through the node that contains them.
 */
void *nmx_xnu_acpi_node_root(void);
void *nmx_xnu_acpi_node_for_pci(void *pci);
void *nmx_xnu_acpi_node_parent(void *node);
void *nmx_xnu_acpi_node_child(void *parent, const char *seg);
/* the child device after 'prev' (NULL = the first) */
void *nmx_xnu_acpi_node_next_child(void *parent, void *prev);
int nmx_xnu_acpi_node_name(void *node, char out[5]);
void nmx_xnu_acpi_node_get(void *node);
void nmx_xnu_acpi_node_put(void *node);
/* 1 when the node contains an object (method or data) of this name */
int nmx_xnu_acpi_has(void *node, const char *name);

/* ACPI values across the boundary; the type numbers are ACPICA's ACPI_TYPE_* */
#define NMX_ACPI_ANY 0
#define NMX_ACPI_INTEGER 1
#define NMX_ACPI_STRING 2
#define NMX_ACPI_BUFFER 3
#define NMX_ACPI_PACKAGE 4
struct nmx_acpi_obj {
    uint32_t type;
    uint32_t count;             /* package: elements */
    uint64_t integer;
    const void *ptr;            /* string (NUL-terminated) or buffer bytes; package: struct nmx_acpi_obj[count] */
    size_t len;                 /* string length without the NUL, or buffer length */
};
/*
 * Evaluates 'name' in 'node' with up to 7 arguments. A returned value comes back as one allocation holding the
 * whole tree (*result, *result_size), released with nmx_xnu_acpi_free. Returns 0, ENOENT (no such object), EINVAL,
 * ENOMEM or EIO (the method failed).
 */
int nmx_xnu_acpi_evaluate(void *node, const char *name, const struct nmx_acpi_obj *args, int nargs,
                          struct nmx_acpi_obj **result, size_t *result_size);
void nmx_xnu_acpi_free(void *p, size_t size);

/* ---- SMBIOS / DMI (nmx_dmi.cpp) ----------------------------------------------------------------------------------- */

/* one SMBIOS string by Linux DMI field number (DMI_SYS_VENDOR, ...); returns its length or 0 */
size_t nmx_xnu_dmi_field(int field, char *buf, size_t max);

/* ---- device nodes (nmx_devfs.cpp) --------------------------------------------------------------------------------- */

int nmx_xnu_devfs_init(void);
void nmx_xnu_devfs_fini(void);
/* opens still outstanding (unload must wait for zero) */
int nmx_xnu_devfs_busy(void);
/* a clone node at /dev/<path>; 'cookie' is handed back to nmx_cdev_open for every open of it */
void *nmx_xnu_devfs_make(const char *path, uint32_t uid, uint32_t gid, int mode, void *cookie);
void nmx_xnu_devfs_remove(void *node);
/*
 * maps physical pages into the calling process (the GPU mmap ioctl); 'uaddr' receives the user address. fixed != 0
 * places the mapping at that address, replacing whatever the process had there (Linux MAP_FIXED); unmapping such a
 * mapping leaves a hole, as munmap(2) does.
 */
int nmx_xnu_map_user(const uint64_t *pages, size_t npages, int cache, int writable, uint64_t fixed, uint64_t *uaddr,
                     void **handle);
void nmx_xnu_unmap_user(void *handle);

/* implemented on the LinuxKPI side (override/src/nmx_linux_cdev.c), called by the device-node glue */
int nmx_cdev_open(void *cookie, int open_id, int fflags);
int nmx_cdev_close(int open_id, int fflags);
int nmx_cdev_ioctl(int open_id, unsigned long cmd, void *data, int fflag);
/* reads into the caller's own buffer at 'uaddr' (the driver copies out to it) */
int nmx_cdev_read(int open_id, uint64_t uaddr, size_t len, size_t *done, int nonblock);
int nmx_cdev_poll(int open_id, int events);

#ifdef __cplusplus
}
#endif
#endif
