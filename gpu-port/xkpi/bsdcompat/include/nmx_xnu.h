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

/* ---- DMA (nmx_dma.cpp) -------------------------------------------------------------------------------------------- */

/* device-visible address for a physical range of 'pci' (an IOPCIDevice *); handle releases the mapping */
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
void nmx_xnu_pci_irq_enable(void *cookie, int on);
/* the IOPCIDevice at domain:bus:dev.func, or NULL */
void *nmx_xnu_pci_find(uint32_t domain, uint8_t bus, uint8_t dev, uint8_t func);
/* the device's expansion ROM image (VBIOS) as the firmware left it, copied into 'buf'; returns its length or 0 */
size_t nmx_xnu_pci_rom(void *pci, void *buf, size_t max);

/* ---- firmware files (nmx_firmware.cpp) ---------------------------------------------------------------------------- */

/* loads a GPU firmware file by its Linux name (e.g. "amdgpu/psp_13_0_4_toc.bin"); caller frees with nmx_xnu_firmware_free */
int nmx_xnu_firmware_load(const char *name, void **data, size_t *size);
void nmx_xnu_firmware_free(void *data, size_t size);

/* ---- ACPI (nmx_acpi.cpp) ------------------------------------------------------------------------------------------ */

/* copies the ACPI table with this 4-character signature (instance 1..n); returns its length or 0 */
size_t nmx_xnu_acpi_table(const char *sig, int instance, void *buf, size_t max);
/* evaluates 'method' on the ACPI node of 'pci' (or an absolute path); integer and buffer results are copied out */
int nmx_xnu_acpi_eval(void *pci, const char *method, const uint64_t *int_args, int nargs, const void *buf_arg,
                      size_t buf_len, uint64_t *int_result, void *buf_result, size_t *buf_result_len);

/* ---- SMBIOS / DMI (nmx_dmi.cpp) ----------------------------------------------------------------------------------- */

/* one SMBIOS string by Linux DMI field number (DMI_SYS_VENDOR, ...); returns its length or 0 */
size_t nmx_xnu_dmi_field(int field, char *buf, size_t max);

#ifdef __cplusplus
}
#endif
#endif
