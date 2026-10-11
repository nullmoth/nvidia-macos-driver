/*
 * FreeBSD newbus, rman resources, interrupts and pci(9) on xnu, over the IOKit glue (nmx_xnu.h).
 *
 * A device_t is a record for one IOService: the matched GPU's IOPCIDevice, another PCI device the driver looks up
 * (an audio function, a bridge), or a child the driver adds. PCI config space goes through IOPCIDevice; BARs are
 * mapped by IOKit and never sized by writing the BAR; interrupts are message-signalled vectors set up by the glue.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/sx.h>
#include <sys/bus.h>
#include <sys/rman.h>
#include <sys/interrupt.h>
#include <machine/resource.h>
#include <machine/vm.h>
#include <dev/pci/pcireg.h>
#include <dev/pci/pcivar.h>
#include "nmx_xnu.h"

MALLOC_DEFINE(M_NMXBUS, "nmxbus", "device records and resources");

struct nmx_device {
    void *xnu;                      /* the IOService (IOPCIDevice for PCI devices) */
    int is_pci;
    struct nmx_device *parent;
    TAILQ_HEAD(, nmx_device) children;
    TAILQ_ENTRY(nmx_device) link;   /* in the parent's children */
    TAILQ_ENTRY(nmx_device) all;    /* in the global list */
    void *softc;
    void *ivars;
    int unit;
    char name[32];
    char nameunit[48];
    char desc[96];
    driver_t *driver;
    device_state_t state;
    struct pci_devinfo dinfo;
    struct pci_map bars[PCIR_MAX_BAR_0 + 1];
    uint32_t saved_cfg[16];         /* configuration header kept by pci_save_state */
    int saved;
};

struct resource {
    device_t dev;
    int type;
    int rid;
    void *va;
    uint64_t phys;
    uint64_t size;
    void *map_handle;
    int vector;                     /* SYS_RES_IRQ: index of the message-signalled vector */
};

struct nmx_intr {
    struct resource *r;
    driver_filter_t *filter;
    driver_intr_t *handler;
    void *arg;
    void *xcookie;
};

static TAILQ_HEAD(, nmx_device) all_devices = TAILQ_HEAD_INITIALIZER(all_devices);
static struct mtx bus_mtx;
MTX_SYSINIT(nmx_bus, &bus_mtx, "nmx bus", MTX_DEF);
static struct sx topo_sx;
SX_SYSINIT(nmx_topo, &topo_sx, "bus topology");

int pci_enable_aspm = 0;

static void fill_pci_info(struct nmx_device *d)
{
    pcicfgregs *c = &d->dinfo.cfg;
    nmx_xnu_pci_location(d->xnu, &c->domain, &c->bus, &c->slot, &c->func);
    c->vendor = (uint16_t)nmx_xnu_pci_read(d->xnu, PCIR_VENDOR, 2);
    c->device = (uint16_t)nmx_xnu_pci_read(d->xnu, PCIR_DEVICE, 2);
    c->revid = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_REVID, 1);
    c->progif = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_PROGIF, 1);
    c->subclass = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_SUBCLASS, 1);
    c->baseclass = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_CLASS, 1);
    c->hdrtype = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_HDRTYPE, 1);
    c->subvendor = (uint16_t)nmx_xnu_pci_read(d->xnu, PCIR_SUBVEND_0, 2);
    c->subdevice = (uint16_t)nmx_xnu_pci_read(d->xnu, PCIR_SUBDEV_0, 2);
    c->intpin = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_INTPIN, 1);
    c->intline = (uint8_t)nmx_xnu_pci_read(d->xnu, PCIR_INTLINE, 1);
    int cap = 0;
    if (pci_find_cap(d, PCIY_EXPRESS, &cap) == 0) {
        c->pcie.pcie_location = cap;
        c->pcie.pcie_flags = (uint16_t)nmx_xnu_pci_read(d->xnu, cap + PCIER_FLAGS, 2);
        c->pcie.pcie_type = c->pcie.pcie_flags & PCIEM_FLAGS_TYPE;
    }
    snprintf(d->name, sizeof(d->name), "pci");
    snprintf(d->nameunit, sizeof(d->nameunit), "pci%u:%u:%u:%u", c->domain, c->bus, c->slot, c->func);
}

/* the record for an IOPCIDevice, created on first use */
device_t nmx_device_for_pci(void *iopci)
{
    if (iopci == NULL)
        return NULL;
    mtx_lock(&bus_mtx);
    struct nmx_device *d;
    TAILQ_FOREACH(d, &all_devices, all)
        if (d->xnu == iopci) { mtx_unlock(&bus_mtx); return d; }
    mtx_unlock(&bus_mtx);

    struct nmx_device *n = malloc(sizeof(*n), M_NMXBUS, M_WAITOK | M_ZERO);
    if (n == NULL)
        return NULL;
    n->xnu = iopci;
    n->is_pci = 1;
    n->state = DS_ATTACHED;
    TAILQ_INIT(&n->children);
    fill_pci_info(n);

    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(d, &all_devices, all)
        if (d->xnu == iopci) { mtx_unlock(&bus_mtx); free(n, M_NMXBUS); return d; }
    TAILQ_INSERT_TAIL(&all_devices, n, all);
    mtx_unlock(&bus_mtx);
    return n;
}

void *nmx_device_xnu(device_t dev)
{
    return dev ? dev->xnu : NULL;
}

void nmx_device_forget(device_t dev)
{
    if (dev == NULL)
        return;
    mtx_lock(&bus_mtx);
    TAILQ_REMOVE(&all_devices, dev, all);
    mtx_unlock(&bus_mtx);
    free(dev, M_NMXBUS);
}

/* ---- newbus ------------------------------------------------------------------------------------------------------- */

void *device_get_softc(device_t dev) { return dev->softc; }
void device_set_softc(device_t dev, void *softc) { dev->softc = softc; }
device_t device_get_parent(device_t dev) { return dev->parent; }
int device_get_unit(device_t dev) { return dev->unit; }
const char *device_get_nameunit(device_t dev) { return dev->nameunit; }
const char *device_get_name(device_t dev) { return dev->name; }
const char *device_get_desc(device_t dev) { return dev->desc; }
void device_set_desc(device_t dev, const char *desc) { strlcpy(dev->desc, desc ? desc : "", sizeof(dev->desc)); }
void device_set_desc_copy(device_t dev, const char *desc) { device_set_desc(dev, desc); }
driver_t *device_get_driver(device_t dev) { return dev->driver; }
device_state_t device_get_state(device_t dev) { return dev->state; }
int device_is_attached(device_t dev) { return dev->state >= DS_ATTACHED; }
void device_set_ivars(device_t dev, void *ivars) { dev->ivars = ivars; }
void *device_get_ivars(device_t dev) { return dev->ivars; }
void device_quiet(device_t dev) { }

int device_printf(device_t dev, const char *fmt, ...)
{
    char buf[256];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    printf("%s: %s", dev ? dev->nameunit : "?", buf);
    return 0;
}

int device_print_prettyname(device_t dev)
{
    printf("%s: ", dev ? dev->nameunit : "?");
    return 0;
}

device_t device_add_child(device_t dev, const char *name, int unit)
{
    struct nmx_device *c = malloc(sizeof(*c), M_NMXBUS, M_WAITOK | M_ZERO);
    if (c == NULL)
        return NULL;
    c->parent = dev;
    c->unit = unit < 0 ? 0 : unit;
    c->state = DS_NOTPRESENT;
    TAILQ_INIT(&c->children);
    strlcpy(c->name, name ? name : "child", sizeof(c->name));
    snprintf(c->nameunit, sizeof(c->nameunit), "%s%d", c->name, c->unit);
    mtx_lock(&bus_mtx);
    if (dev)
        TAILQ_INSERT_TAIL(&dev->children, c, link);
    TAILQ_INSERT_TAIL(&all_devices, c, all);
    mtx_unlock(&bus_mtx);
    return c;
}

int device_delete_child(device_t dev, device_t child)
{
    device_t g;
    while ((g = TAILQ_FIRST(&child->children)) != NULL)
        device_delete_child(child, g);
    mtx_lock(&bus_mtx);
    if (dev)
        TAILQ_REMOVE(&dev->children, child, link);
    TAILQ_REMOVE(&all_devices, child, all);
    mtx_unlock(&bus_mtx);
    free(child, M_NMXBUS);
    return 0;
}

int device_get_children(device_t dev, device_t **listp, int *countp)
{
    int n = 0;
    device_t c;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(c, &dev->children, link)
        n++;
    device_t *list = n ? malloc(sizeof(device_t) * (size_t)n, M_TEMP, M_NOWAIT | M_ZERO) : NULL;
    int i = 0;
    if (list)
        TAILQ_FOREACH(c, &dev->children, link)
            list[i++] = c;
    mtx_unlock(&bus_mtx);
    if (n && list == NULL)
        return ENOMEM;
    *listp = list;
    *countp = i;
    return 0;
}

int device_probe_and_attach(device_t dev) { dev->state = DS_ATTACHED; return 0; }
int device_attach(device_t dev) { dev->state = DS_ATTACHED; return 0; }
int device_detach(device_t dev) { dev->state = DS_NOTPRESENT; return 0; }
int bus_generic_attach(device_t dev) { return 0; }
int bus_generic_detach(device_t dev) { return 0; }
void bus_attach_children(device_t dev) { }

void bus_topo_lock(void) { sx_xlock(&topo_sx); }
void bus_topo_unlock(void) { sx_xunlock(&topo_sx); }
void bus_topo_assert(void) { }

devclass_t devclass_find(const char *classname) { return NULL; }
devclass_t devclass_create(const char *classname) { return NULL; }
device_t devclass_get_device(devclass_t dc, int unit) { return NULL; }
int devclass_get_maxunit(devclass_t dc) { return 0; }

const char *devtoname(struct cdev *cdev) { return "dri"; }
int dev2udev(struct cdev *x) { return 0; }

int bus_bind_intr(device_t dev, struct resource *r, int cpu) { return 0; }
int bus_get_domain(device_t dev, int *domain) { return ENOENT; }

/* ---- resources ---------------------------------------------------------------------------------------------------- */

struct resource *bus_alloc_resource_any(device_t dev, int type, int *rid, u_int flags)
{
    if (dev == NULL || !dev->is_pci)
        return NULL;
    struct resource *r = malloc(sizeof(*r), M_NMXBUS, M_WAITOK | M_ZERO);
    if (r == NULL)
        return NULL;
    r->dev = dev;
    r->type = type;
    r->rid = *rid;
    if (type == SYS_RES_MEMORY || type == SYS_RES_IOPORT) {
        r->va = nmx_xnu_pci_map_bar(dev->xnu, *rid, NMX_CACHE_UC, &r->phys, &r->size, &r->map_handle);
        if (r->va == NULL) {
            free(r, M_NMXBUS);
            return NULL;
        }
    } else if (type == SYS_RES_IRQ) {
        /* FreeBSD numbers message-signalled vectors from rid 1; rid 0 is the legacy line */
        r->vector = *rid > 0 ? *rid - 1 : 0;
    } else {
        free(r, M_NMXBUS);
        return NULL;
    }
    return r;
}

int bus_release_resource(device_t dev, int type, int rid, struct resource *r)
{
    if (r == NULL)
        return EINVAL;
    if (r->map_handle)
        nmx_xnu_unmap(r->map_handle);
    free(r, M_NMXBUS);
    return 0;
}

rman_res_t rman_get_start(struct resource *r) { return r->type == SYS_RES_IRQ ? (rman_res_t)r->vector : r->phys; }
rman_res_t rman_get_end(struct resource *r) { return r->type == SYS_RES_IRQ ? (rman_res_t)r->vector : r->phys + r->size - 1; }
rman_res_t rman_get_size(struct resource *r) { return r->type == SYS_RES_IRQ ? 1 : r->size; }
void *rman_get_virtual(struct resource *r) { return r->va; }
bus_space_tag_t rman_get_bustag(struct resource *r) { return X86_BUS_SPACE_MEM; }
bus_space_handle_t rman_get_bushandle(struct resource *r) { return (bus_space_handle_t)(uintptr_t)r->va; }
int rman_get_rid(struct resource *r) { return r->rid; }
u_int rman_get_flags(struct resource *r) { return RF_ACTIVE; }

static void intr_trampoline(void *p)
{
    struct nmx_intr *in = p;
    int rv = FILTER_SCHEDULE_THREAD;
    if (in->filter)
        rv = in->filter(in->arg);
    if (in->handler && (rv & FILTER_SCHEDULE_THREAD))
        in->handler(in->arg);
}

int bus_setup_intr(device_t dev, struct resource *r, int flags, driver_filter_t *filter, driver_intr_t *handler,
                   void *arg, void **cookiep)
{
    if (r == NULL || r->type != SYS_RES_IRQ)
        return EINVAL;
    struct nmx_intr *in = malloc(sizeof(*in), M_NMXBUS, M_WAITOK | M_ZERO);
    if (in == NULL)
        return ENOMEM;
    in->r = r;
    in->filter = filter;
    in->handler = handler;
    in->arg = arg;
    int err = nmx_xnu_pci_irq_setup(dev->xnu, r->vector, intr_trampoline, in, &in->xcookie);
    if (err) {
        free(in, M_NMXBUS);
        return ENXIO;
    }
    *cookiep = in;
    return 0;
}

int bus_teardown_intr(device_t dev, struct resource *r, void *cookie)
{
    struct nmx_intr *in = cookie;
    if (in == NULL)
        return EINVAL;
    nmx_xnu_pci_irq_teardown(in->xcookie);
    free(in, M_NMXBUS);
    return 0;
}

int bus_describe_intr(device_t dev, struct resource *irq, void *cookie, const char *fmt, ...)
{
    return 0;
}

/* ---- pci(9) -------------------------------------------------------------------------------------------------------- */

uint32_t pci_read_config(device_t dev, int reg, int width) { return nmx_xnu_pci_read(dev->xnu, reg, width); }
void pci_write_config(device_t dev, int reg, uint32_t val, int width) { nmx_xnu_pci_write(dev->xnu, reg, val, width); }
uint16_t pci_get_vendor(device_t dev) { return dev->dinfo.cfg.vendor; }
uint16_t pci_get_device(device_t dev) { return dev->dinfo.cfg.device; }
uint16_t pci_get_subvendor(device_t dev) { return dev->dinfo.cfg.subvendor; }
uint16_t pci_get_subdevice(device_t dev) { return dev->dinfo.cfg.subdevice; }
uint8_t pci_get_revid(device_t dev) { return dev->dinfo.cfg.revid; }
uint8_t pci_get_class(device_t dev) { return dev->dinfo.cfg.baseclass; }
uint8_t pci_get_subclass(device_t dev) { return dev->dinfo.cfg.subclass; }
uint8_t pci_get_progif(device_t dev) { return dev->dinfo.cfg.progif; }
uint32_t pci_get_domain(device_t dev) { return dev->dinfo.cfg.domain; }
uint8_t pci_get_bus(device_t dev) { return dev->dinfo.cfg.bus; }
uint8_t pci_get_slot(device_t dev) { return dev->dinfo.cfg.slot; }
uint8_t pci_get_function(device_t dev) { return dev->dinfo.cfg.func; }
uint8_t pci_get_irq(device_t dev) { return dev->dinfo.cfg.intline; }
uint32_t pci_get_devid(device_t dev) { return ((uint32_t)dev->dinfo.cfg.device << 16) | dev->dinfo.cfg.vendor; }
struct pci_devinfo *nmx_pci_devinfo(device_t dev) { return &dev->dinfo; }
bool is_pci_device(device_t dev) { return dev && dev->is_pci; }

int pci_enable_busmaster(device_t dev)
{
    pci_write_config(dev, PCIR_COMMAND, pci_read_config(dev, PCIR_COMMAND, 2) | PCIM_CMD_BUSMASTEREN, 2);
    return 0;
}

int pci_disable_busmaster(device_t dev)
{
    pci_write_config(dev, PCIR_COMMAND, pci_read_config(dev, PCIR_COMMAND, 2) & ~PCIM_CMD_BUSMASTEREN, 2);
    return 0;
}

int pci_enable_io(device_t dev, int space)
{
    uint16_t bit = space == SYS_RES_IOPORT ? PCIM_CMD_PORTEN : PCIM_CMD_MEMEN;
    pci_write_config(dev, PCIR_COMMAND, pci_read_config(dev, PCIR_COMMAND, 2) | bit, 2);
    return 0;
}

int pci_disable_io(device_t dev, int space)
{
    uint16_t bit = space == SYS_RES_IOPORT ? PCIM_CMD_PORTEN : PCIM_CMD_MEMEN;
    pci_write_config(dev, PCIR_COMMAND, pci_read_config(dev, PCIR_COMMAND, 2) & ~bit, 2);
    return 0;
}

/* walks the classic capability list; at most 48 entries so a malformed list cannot loop forever */
int pci_find_next_cap(device_t dev, int capability, int start, int *capreg)
{
    if (!(pci_read_config(dev, PCIR_STATUS, 2) & PCIM_STATUS_CAPPRESENT))
        return ENXIO;
    int ptr = start ? (int)pci_read_config(dev, start + PCICAP_NEXTPTR, 1) : (int)pci_read_config(dev, PCIR_CAP_PTR, 1);
    for (int guard = 0; ptr >= 0x40 && guard < 48; guard++) {
        ptr &= ~3;
        if ((int)pci_read_config(dev, ptr + PCICAP_ID, 1) == capability) {
            if (capreg)
                *capreg = ptr;
            return 0;
        }
        ptr = (int)pci_read_config(dev, ptr + PCICAP_NEXTPTR, 1);
    }
    return ENOENT;
}

int pci_find_cap(device_t dev, int capability, int *capreg)
{
    return pci_find_next_cap(dev, capability, 0, capreg);
}

/* walks the PCIe extended capability list at 0x100 */
int pci_find_next_extcap(device_t dev, int capability, int start, int *capreg)
{
    int ptr = start ? (int)PCI_EXTCAP_NEXTPTR(pci_read_config(dev, start, 4)) : 0x100;
    for (int guard = 0; ptr >= 0x100 && ptr < 0x1000 && guard < 960; guard++) {
        uint32_t hdr = pci_read_config(dev, ptr, 4);
        if (hdr == 0 || hdr == 0xffffffff)
            return ENOENT;
        if ((int)PCI_EXTCAP_ID(hdr) == capability) {
            if (capreg)
                *capreg = ptr;
            return 0;
        }
        ptr = (int)PCI_EXTCAP_NEXTPTR(hdr);
    }
    return ENOENT;
}

int pci_find_extcap(device_t dev, int capability, int *capreg)
{
    return pci_find_next_extcap(dev, capability, 0, capreg);
}

int pci_msi_count(device_t dev) { return nmx_xnu_pci_msi_count(dev->xnu); }
int pci_msix_count(device_t dev) { return 0; }

int pci_alloc_msi(device_t dev, int *count)
{
    int avail = nmx_xnu_pci_msi_count(dev->xnu);
    if (avail <= 0)
        return ENXIO;
    if (*count > avail)
        *count = avail;
    return 0;
}

int pci_alloc_msix(device_t dev, int *count) { return ENXIO; }
int pci_release_msi(device_t dev) { return 0; }

int pci_set_powerstate(device_t dev, int state)
{
    int cap;
    if (pci_find_cap(dev, PCIY_PMG, &cap) != 0)
        return EOPNOTSUPP;
    uint16_t st = (uint16_t)pci_read_config(dev, cap + PCIR_POWER_STATUS, 2);
    st = (uint16_t)((st & ~PCIM_PSTAT_DMASK) | (state & PCIM_PSTAT_DMASK));
    pci_write_config(dev, cap + PCIR_POWER_STATUS, st, 2);
    DELAY(state == PCI_POWERSTATE_D0 ? 10000 : 200);   /* PCI PM spec settle times (D3hot->D0 10 ms) */
    return 0;
}

int pci_get_powerstate(device_t dev)
{
    int cap;
    if (pci_find_cap(dev, PCIY_PMG, &cap) != 0)
        return PCI_POWERSTATE_D0;
    return (int)(pci_read_config(dev, cap + PCIR_POWER_STATUS, 2) & PCIM_PSTAT_DMASK);
}

bool pci_has_pme(device_t dev, int state)
{
    int cap;
    if (pci_find_cap(dev, PCIY_PMG, &cap) != 0)
        return false;
    uint16_t caps = (uint16_t)pci_read_config(dev, cap + PCIR_POWER_CAP, 2);
    return (caps & (PCIM_PCAP_D0PME << state)) != 0;
}

int pci_get_max_payload(device_t dev)
{
    int cap = dev->dinfo.cfg.pcie.pcie_location;
    if (cap == 0)
        return 0;
    uint16_t ctl = (uint16_t)pci_read_config(dev, cap + PCIER_DEVICE_CTL, 2);
    return 128 << ((ctl & PCIEM_CTL_MAX_PAYLOAD) >> 5);
}

int pci_get_max_read_req(device_t dev)
{
    int cap = dev->dinfo.cfg.pcie.pcie_location;
    if (cap == 0)
        return 0;
    uint16_t ctl = (uint16_t)pci_read_config(dev, cap + PCIER_DEVICE_CTL, 2);
    return 128 << ((ctl & PCIEM_CTL_MAX_READ_REQUEST) >> 12);
}

int pci_set_max_read_req(device_t dev, int size)
{
    int cap = dev->dinfo.cfg.pcie.pcie_location;
    if (cap == 0 || size < 128 || size > 4096)
        return 0;
    int enc = fls(size) - 8;
    uint16_t ctl = (uint16_t)pci_read_config(dev, cap + PCIER_DEVICE_CTL, 2);
    ctl = (uint16_t)((ctl & ~PCIEM_CTL_MAX_READ_REQUEST) | (enc << 12));
    pci_write_config(dev, cap + PCIER_DEVICE_CTL, ctl, 2);
    return size;
}

uint32_t pcie_read_config(device_t dev, int reg, int width)
{
    int cap = dev->dinfo.cfg.pcie.pcie_location;
    return cap ? pci_read_config(dev, cap + reg, width) : (width == 4 ? 0xffffffff : width == 2 ? 0xffff : 0xff);
}

void pcie_write_config(device_t dev, int reg, uint32_t value, int width)
{
    int cap = dev->dinfo.cfg.pcie.pcie_location;
    if (cap)
        pci_write_config(dev, cap + reg, value, width);
}

uint32_t pcie_adjust_config(device_t dev, int reg, uint32_t mask, uint32_t value, int width)
{
    uint32_t old = pcie_read_config(dev, reg, width);
    pcie_write_config(dev, reg, (old & ~mask) | value, width);
    return old;
}

int pcie_get_max_completion_timeout(device_t dev) { return 0; }
bool pcie_flr(device_t dev, u_int max_delay, bool force) { return false; }
int pci_iov_attach(device_t dev, void *pf_schema, void *vf_schema) { return ENXIO; }
int pci_iov_detach(device_t dev) { return 0; }

void pci_save_state(device_t dev)
{
    for (int i = 0; i < 16; i++)
        dev->saved_cfg[i] = pci_read_config(dev, i * 4, 4);
    dev->saved = 1;
}

/* restores the header from offset 0x04 on (vendor/device are read-only) with the command register written last */
void pci_restore_state(device_t dev)
{
    if (!dev->saved)
        return;
    for (int i = 15; i >= 2; i--)
        pci_write_config(dev, i * 4, dev->saved_cfg[i], 4);
    pci_write_config(dev, PCIR_COMMAND, dev->saved_cfg[1] & 0xffff, 2);
}

device_t pci_find_dbsf(uint32_t domain, uint8_t bus, uint8_t slot, uint8_t func)
{
    return nmx_device_for_pci(nmx_xnu_pci_find(domain, bus, slot, func));
}

device_t pci_find_bsf(uint8_t bus, uint8_t slot, uint8_t func)
{
    return pci_find_dbsf(0, bus, slot, func);
}

device_t pci_find_device(uint16_t vendor, uint16_t device)
{
    device_t d, found = NULL;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(d, &all_devices, all)
        if (d->is_pci && d->dinfo.cfg.vendor == vendor && d->dinfo.cfg.device == device) { found = d; break; }
    mtx_unlock(&bus_mtx);
    return found;
}

device_t pci_find_class(uint8_t cls, uint8_t subcls)
{
    return pci_find_class_from(cls, subcls, NULL);
}

device_t pci_find_class_from(uint8_t cls, uint8_t subcls, device_t from)
{
    device_t d, found = NULL;
    int past = from == NULL;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(d, &all_devices, all) {
        if (!past) { past = d == from; continue; }
        if (d->is_pci && d->dinfo.cfg.baseclass == cls && d->dinfo.cfg.subclass == subcls) { found = d; break; }
    }
    mtx_unlock(&bus_mtx);
    return found;
}

int pci_get_id(device_t dev, int type, uintptr_t *id)
{
    *id = ((uintptr_t)dev->dinfo.cfg.bus << 8) | ((uintptr_t)dev->dinfo.cfg.slot << 3) | dev->dinfo.cfg.func;
    return 0;
}

device_t pci_find_pcie_root_port(device_t dev)
{
    return NULL;
}

struct pci_map *pci_find_bar(device_t dev, int reg)
{
    int i = (reg - PCIR_BAR(0)) / 4;
    if (i < 0 || i > PCIR_MAX_BAR_0)
        return NULL;
    uint64_t phys = 0, size = 0;
    int is_io = 0;
    if (!nmx_xnu_pci_bar_info(dev->xnu, reg, &phys, &size, &is_io))
        return NULL;
    struct pci_map *pm = &dev->bars[i];
    pm->pm_reg = (uint16_t)reg;
    pm->pm_value = phys | (is_io ? PCIM_BAR_IO_SPACE : 0) | (pci_read_config(dev, reg, 4) & 0xf & (is_io ? 0x3 : 0xf));
    pm->pm_size = size;
    return pm;
}
