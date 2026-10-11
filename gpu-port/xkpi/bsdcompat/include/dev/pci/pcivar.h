/*
 * FreeBSD PCI bus API (pci(9)) on xnu. A PCI device_t is the port's record for an IOPCIDevice
 * (src/nmx_pci.cpp): config space goes through IOPCIDevice::configRead/Write, capabilities are found by walking
 * the capability lists, BARs and MSI vectors are IOKit resources.
 */
#ifndef NMXKPI_DEV_PCI_PCIVAR_H
#define NMXKPI_DEV_PCI_PCIVAR_H
#include <sys/types.h>
#include <sys/bus.h>
#include <dev/pci/pcireg.h>
uint32_t pci_read_config(device_t dev, int reg, int width);
void pci_write_config(device_t dev, int reg, uint32_t val, int width);
uint16_t pci_get_vendor(device_t dev);
uint16_t pci_get_device(device_t dev);
uint16_t pci_get_subvendor(device_t dev);
uint16_t pci_get_subdevice(device_t dev);
uint8_t pci_get_revid(device_t dev);
uint8_t pci_get_class(device_t dev);
uint8_t pci_get_subclass(device_t dev);
uint8_t pci_get_progif(device_t dev);
uint32_t pci_get_domain(device_t dev);
uint8_t pci_get_bus(device_t dev);
uint8_t pci_get_slot(device_t dev);
uint8_t pci_get_function(device_t dev);
uint8_t pci_get_irq(device_t dev);
uint32_t pci_get_devid(device_t dev);
#define pci_get_baseclass(dev) pci_get_class(dev)
int pci_enable_busmaster(device_t dev);
int pci_disable_busmaster(device_t dev);
int pci_enable_io(device_t dev, int space);
int pci_disable_io(device_t dev, int space);
int pci_find_cap(device_t dev, int capability, int *capreg);
int pci_find_next_cap(device_t dev, int capability, int start, int *capreg);
int pci_find_extcap(device_t dev, int capability, int *capreg);
int pci_find_next_extcap(device_t dev, int capability, int start, int *capreg);
int pci_msi_count(device_t dev);
int pci_msix_count(device_t dev);
int pci_alloc_msi(device_t dev, int *count);
int pci_alloc_msix(device_t dev, int *count);
int pci_release_msi(device_t dev);
int pci_set_powerstate(device_t dev, int state);
int pci_get_powerstate(device_t dev);
int pci_get_max_payload(device_t dev);
int pci_get_max_read_req(device_t dev);
int pci_set_max_read_req(device_t dev, int size);
void pci_save_state(device_t dev);
void pci_restore_state(device_t dev);
device_t pci_find_dbsf(uint32_t domain, uint8_t bus, uint8_t slot, uint8_t func);
device_t pci_find_bsf(uint8_t bus, uint8_t slot, uint8_t func);
device_t pci_find_device(uint16_t vendor, uint16_t device);
device_t pci_find_class(uint8_t cls, uint8_t subcls);
device_t pci_find_class_from(uint8_t cls, uint8_t subcls, device_t from);
int pci_get_id(device_t dev, int type, uintptr_t *id);
#define PCI_GET_ID(bus, child, type, id) pci_get_id((child), (type), (id))
device_t pci_find_base_class_from(uint8_t cls, device_t from);
/* a BAR the device does not have has no resource list entry; nothing can be reserved for it */
struct resource *pci_reserve_map(device_t dev, device_t child, int type, int rid, rman_res_t start, rman_res_t end,
                                 rman_res_t count, u_int num, u_int flags);
/* the expansion ROM (VBIOS) of a display device, as a kernel copy */
void *vga_pci_map_bios(device_t dev, size_t *size);
void vga_pci_unmap_bios(device_t dev, void *bios);
int pcie_get_max_completion_timeout(device_t dev);
uint32_t pcie_read_config(device_t dev, int reg, int width);
void pcie_write_config(device_t dev, int reg, uint32_t value, int width);
uint32_t pcie_adjust_config(device_t dev, int reg, uint32_t mask, uint32_t value, int width);
bool pcie_flr(device_t dev, u_int max_delay, bool force);
int pci_iov_attach(device_t dev, void *pf_schema, void *vf_schema);
int pci_iov_detach(device_t dev);
#define PCI_ID_RID 0
#define PCI_POWERSTATE_D0 0
#define PCI_POWERSTATE_D1 1
#define PCI_POWERSTATE_D2 2
#define PCI_POWERSTATE_D3_HOT 3
#define PCI_POWERSTATE_D3_COLD 4
#define PCI_POWERSTATE_D3 PCI_POWERSTATE_D3_COLD
#define PCI_POWERSTATE_UNKNOWN -1
#define PCI_RID2BUS(rid) (((rid) >> 8) & 0xff)
#define PCI_RID2SLOT(rid) (((rid) >> 3) & 0x1f)
#define PCI_RID2FUNC(rid) ((rid) & 0x7)
#define PCI_RID(bus, slot, func) ((((bus) & 0xff) << 8) | (((slot) & 0x1f) << 3) | ((func) & 0x7))
#define PCIB_BCR_SECBUS_RESET 0x0040
/* the configuration header fields LinuxKPI reads from FreeBSD's per-device info */
typedef struct pcicfg {
    uint16_t subvendor, subdevice, vendor, device, cmdreg, statreg;
    uint8_t baseclass, subclass, progif, revid, hdrtype, intpin, intline;
    uint32_t domain;
    uint8_t bus, slot, func;
    struct { int pcie_location; uint8_t pcie_type; uint16_t pcie_flags; } pcie;
    struct pcicfg_msi {
        uint16_t msi_ctrl;           /* message control */
        uint8_t msi_location;        /* offset of the MSI capability */
        int msi_alloc;               /* messages allocated */
        uint64_t msi_addr;           /* address register as programmed */
        uint16_t msi_data;           /* data register as programmed */
        u_int msi_handlers;
    } msi;
} pcicfgregs;
struct pci_devinfo {
    pcicfgregs cfg;
    struct resource_list resources;
};
struct pci_devinfo *nmx_pci_devinfo(device_t dev);
struct pci_map { pci_addr_t pm_value; pci_addr_t pm_size; uint16_t pm_reg; STAILQ_ENTRY(pci_map) pm_link; };
struct pci_map *pci_find_bar(device_t dev, int reg);
#define PCI_BAR_IO(x) (((x) & PCIM_BAR_SPACE) == PCIM_BAR_IO_SPACE)
#define PCI_BAR_MEM(x) (((x) & PCIM_BAR_SPACE) == PCIM_BAR_MEM_SPACE)
bool is_pci_device(device_t dev);
bool pci_has_pme(device_t dev, int state);
device_t pci_find_pcie_root_port(device_t dev);
/* ASPM changes stay with the firmware's settings on macOS */
extern int pci_enable_aspm;
#endif
