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
#include <machine/bus.h>
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
    int softc_owned;                /* softc was allocated here for the driver (driver->size) */
    int irq_base;                   /* PCI functions: first of this function's interrupt numbers (see below) */
    bus_dma_tag_t dmat;             /* PCI functions: root DMA tag */
    devclass_t devclass;            /* the class this device is a member of (its bus class for PCI functions) */
};

struct resource {
    device_t dev;
    int type;
    int rid;
    void *va;
    uint64_t phys;
    uint64_t size;
    void *map_handle;
    int vector;                     /* SYS_RES_IRQ: message-signalled vector index, or -1 for the legacy line */
    rman_res_t irqno;               /* SYS_RES_IRQ: the interrupt number LinuxKPI sees */
};

struct nmx_intr {
    struct resource *r;
    driver_filter_t *filter;
    driver_intr_t *handler;
    void *arg;
    void *xcookie;
    TAILQ_ENTRY(nmx_intr) link;     /* in 'intrs', for _intr_drain */
};

static TAILQ_HEAD(, nmx_intr) intrs = TAILQ_HEAD_INITIALIZER(intrs);

/*
 * The PCI function a device belongs to: itself, or the nearest PCI ancestor (a DRM device is a child of the GPU's
 * function, as under FreeBSD's vgapci, and its pci(9) calls and resources are the function's). A device with no PCI
 * ancestor resolves to a zeroed record whose config space reads as an absent device.
 */
static struct nmx_device nopci;

bus_dma_tag_t nmx_busdma_root_tag(void *pci);
void nmx_busdma_root_free(bus_dma_tag_t t);
static void free_record(struct nmx_device *d);
static void devclass_leave(device_t dev);

static struct nmx_device *P(device_t d)
{
    for (device_t x = d; x != NULL; x = x->parent)
        if (x->is_pci)
            return x;
    return &nopci;
}

/*
 * Interrupt numbers: FreeBSD hands out global IRQ numbers and LinuxKPI looks a device up by them, so each PCI
 * function gets a block of IRQ_BLOCK numbers: base + 0 is the legacy line (rid 0), base + n is message-signalled
 * vector n - 1 (rid n). Numbers start at IRQ_FIRST, above anything the legacy PIC/IOAPIC range would use.
 */
#define IRQ_FIRST 1024
#define IRQ_BLOCK 64
static int irq_next_block;

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
    if (pci_find_cap(d, PCIY_MSI, &cap) == 0) {
        c->msi.msi_location = (uint8_t)cap;
        c->msi.msi_ctrl = (uint16_t)nmx_xnu_pci_read(d->xnu, cap + PCIR_MSI_CTRL, 2);
    }
    d->irq_base = IRQ_FIRST + IRQ_BLOCK * __atomic_fetch_add(&irq_next_block, 1, __ATOMIC_RELAXED);
}

static void rl_add(struct nmx_device *d, int type, int rid, rman_res_t start, rman_res_t count)
{
    struct resource_list_entry *rle = malloc(sizeof(*rle), M_NMXBUS, M_WAITOK | M_ZERO);
    if (rle == NULL)
        return;
    rle->type = type;
    rle->rid = rid;
    rle->start = start;
    rle->count = count;
    rle->end = start + count - 1;
    STAILQ_INSERT_TAIL(&d->dinfo.resources, rle, link);
}

/* the BARs IOKit assigned (a 64-bit BAR is one entry at its low register) and the legacy interrupt line */
static void fill_resources(struct nmx_device *d)
{
    STAILQ_INIT(&d->dinfo.resources);
    int maxbar = (d->dinfo.cfg.hdrtype & PCIM_HDRTYPE) == PCIM_HDRTYPE_NORMAL ? PCIR_MAX_BAR_0 : 1;
    for (int i = 0; i <= maxbar; i++) {
        uint64_t phys = 0, size = 0;
        int is_io = 0;
        if (!nmx_xnu_pci_bar_info(d->xnu, PCIR_BAR(i), &phys, &size, &is_io) || size == 0)
            continue;
        rl_add(d, is_io ? SYS_RES_IOPORT : SYS_RES_MEMORY, PCIR_BAR(i), phys, size);
    }
    if (d->dinfo.cfg.intpin != 0)
        rl_add(d, SYS_RES_IRQ, 0, (rman_res_t)d->irq_base, 1);
}

static void rl_remove(struct nmx_device *d, int type, int rid)
{
    struct resource_list_entry *rle = resource_list_find(&d->dinfo.resources, type, rid);
    if (rle == NULL)
        return;
    STAILQ_REMOVE(&d->dinfo.resources, rle, resource_list_entry, link);
    free(rle, M_NMXBUS);
}

static void free_record(struct nmx_device *d)
{
    struct resource_list_entry *rle;
    while ((rle = STAILQ_FIRST(&d->dinfo.resources)) != NULL) {
        STAILQ_REMOVE_HEAD(&d->dinfo.resources, link);
        free(rle, M_NMXBUS);
    }
    nmx_busdma_root_free(d->dmat);
    free(d, M_NMXBUS);
}

struct resource_list_entry *resource_list_find(struct resource_list *rl, int type, int rid)
{
    struct resource_list_entry *rle;
    STAILQ_FOREACH(rle, rl, link)
        if (rle->type == type && rle->rid == rid)
            return rle;
    return NULL;
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
    n->ivars = &n->dinfo;           /* FreeBSD's PCI ivars are the device info; a DRM child shares them */
    TAILQ_INIT(&n->children);
    fill_pci_info(n);
    fill_resources(n);

    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(d, &all_devices, all)
        if (d->xnu == iopci) { mtx_unlock(&bus_mtx); free_record(n); return d; }
    TAILQ_INSERT_TAIL(&all_devices, n, all);
    mtx_unlock(&bus_mtx);
    return n;
}

void *nmx_device_xnu(device_t dev)
{
    return dev ? P(dev)->xnu : NULL;   /* NULL for a device with no PCI function */
}

void nmx_device_forget(device_t dev)
{
    if (dev == NULL)
        return;
    mtx_lock(&bus_mtx);
    TAILQ_REMOVE(&all_devices, dev, all);
    mtx_unlock(&bus_mtx);
    free_record(dev);
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
    c->unit = unit < 0 ? 0 : unit;      /* a class member gets its real unit from devclass_join */
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

/* FreeBSD device_delete_child: detaches the subtree bottom-up first; a detach that fails stops the deletion */
int device_delete_child(device_t dev, device_t child)
{
    device_t g;
    while ((g = TAILQ_FIRST(&child->children)) != NULL) {
        int err = device_delete_child(child, g);
        if (err)
            return err;
    }
    int err = device_detach(child);
    if (err)
        return err;
    devclass_leave(child);
    mtx_lock(&bus_mtx);
    if (dev)
        TAILQ_REMOVE(&dev->children, child, link);
    TAILQ_REMOVE(&all_devices, child, all);
    mtx_unlock(&bus_mtx);
    if (child->softc_owned)
        free(child->softc, M_NMXBUS);
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


void bus_topo_lock(void) { sx_xlock(&topo_sx); }
void bus_topo_unlock(void) { sx_xunlock(&topo_sx); }
void bus_topo_assert(void) { }

/* ---- driver classes, probe and attach ------------------------------------------------------------------------------ */

#define DC_UNITS 64

struct driverlink {
    TAILQ_ENTRY(driverlink) link;
    driver_t *driver;
    int pass;
};

struct devclass {
    TAILQ_ENTRY(devclass) link;
    char name[32];
    TAILQ_HEAD(, driverlink) drivers;   /* drivers offered to this class's devices */
    device_t units[DC_UNITS];          /* members, by unit number */
};

static TAILQ_HEAD(, devclass) devclasses = TAILQ_HEAD_INITIALIZER(devclasses);

devclass_t devclass_find(const char *classname)
{
    struct devclass *dc;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(dc, &devclasses, link)
        if (strcmp(dc->name, classname) == 0)
            break;
    mtx_unlock(&bus_mtx);
    return dc;
}

devclass_t devclass_create(const char *classname)
{
    if (classname == NULL)
        return NULL;
    devclass_t dc = devclass_find(classname);
    if (dc != NULL)
        return dc;
    struct devclass *n = malloc(sizeof(*n), M_NMXBUS, M_WAITOK | M_ZERO);
    if (n == NULL)
        return NULL;
    strlcpy(n->name, classname, sizeof(n->name));
    TAILQ_INIT(&n->drivers);
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(dc, &devclasses, link)
        if (strcmp(dc->name, classname) == 0)
            break;
    if (dc == NULL) {
        TAILQ_INSERT_TAIL(&devclasses, n, link);
        dc = n;
        n = NULL;
    }
    mtx_unlock(&bus_mtx);
    if (n != NULL)
        free(n, M_NMXBUS);
    return dc;
}

device_t devclass_get_device(devclass_t dc, int unit)
{
    if (dc == NULL || unit < 0 || unit >= DC_UNITS)
        return NULL;
    mtx_lock(&bus_mtx);
    device_t d = dc->units[unit];
    mtx_unlock(&bus_mtx);
    return d;
}

int devclass_get_maxunit(devclass_t dc)
{
    int max = 0;
    if (dc == NULL)
        return 0;
    mtx_lock(&bus_mtx);
    for (int u = 0; u < DC_UNITS; u++)
        if (dc->units[u])
            max = u + 1;
    mtx_unlock(&bus_mtx);
    return max;
}

/* makes dev a member of dc with its own unit number (the first free one when 'want' is -1 or taken) */
static int devclass_join(devclass_t dc, device_t dev, int want)
{
    int unit = -1;
    mtx_lock(&bus_mtx);
    if (want >= 0 && want < DC_UNITS && dc->units[want] == NULL)
        unit = want;
    for (int u = 0; unit < 0 && u < DC_UNITS; u++)
        if (dc->units[u] == NULL)
            unit = u;
    if (unit >= 0) {
        dc->units[unit] = dev;
        dev->devclass = dc;
        dev->unit = unit;
    }
    mtx_unlock(&bus_mtx);
    if (unit < 0)
        return ENOSPC;
    snprintf(dev->nameunit, sizeof(dev->nameunit), "%s%d", dc->name, unit);
    return 0;
}

static void devclass_leave(device_t dev)
{
    devclass_t dc = dev->devclass;
    if (dc == NULL)
        return;
    mtx_lock(&bus_mtx);
    if (dev->unit >= 0 && dev->unit < DC_UNITS && dc->units[dev->unit] == dev)
        dc->units[dev->unit] = NULL;
    dev->devclass = NULL;
    mtx_unlock(&bus_mtx);
}

void *nmx_device_method(device_t dev, const char *name)
{
    if (dev == NULL || dev->driver == NULL || dev->driver->methods == NULL)
        return NULL;
    for (device_method_t *m = dev->driver->methods; m->name != NULL; m++)
        if (strcmp(m->name, name) == 0)
            return m->func;
    return NULL;
}

/* FreeBSD device_set_driver: the softc is the driver's, sized by it, unless someone already set one */
static int set_driver(device_t dev, driver_t *driver)
{
    if (dev->softc_owned) {
        free(dev->softc, M_NMXBUS);
        dev->softc = NULL;
        dev->softc_owned = 0;
    }
    dev->driver = driver;
    if (driver != NULL && dev->softc == NULL && driver->size > 0) {
        dev->softc = malloc(driver->size, M_NMXBUS, M_WAITOK | M_ZERO);
        if (dev->softc == NULL) {
            dev->driver = NULL;
            return ENOMEM;
        }
        dev->softc_owned = 1;
    }
    return 0;
}

/* probe with one driver: 0 or a negative priority accepts (FreeBSD's convention), a positive errno declines */
static int probe_with(device_t dev, driver_t *driver)
{
    int err = set_driver(dev, driver);
    if (err)
        return err;
    device_probe_t *probe = (device_probe_t *)nmx_device_method(dev, "device_probe");
    int r = probe ? probe(dev) : 0;
    if (r > 0) {
        set_driver(dev, NULL);
        return r;
    }
    dev->state = DS_ALIVE;
    return 0;
}

int device_attach(device_t dev)
{
    if (dev->state < DS_ALIVE)
        return ENXIO;
    device_attach_t *attach = (device_attach_t *)nmx_device_method(dev, "device_attach");
    dev->state = DS_ATTACHING;
    int err = attach ? attach(dev) : 0;
    if (err) {
        printf("NullMoth xkpi: %s: attach failed (%d)\n", dev->nameunit, err);
        dev->state = DS_NOTPRESENT;
        set_driver(dev, NULL);
        return err;
    }
    dev->state = DS_ATTACHED;
    return 0;
}

/*
 * FreeBSD device_probe_and_attach: a device with a driver is probed by it; one without is offered the drivers of its
 * parent's class in turn. With no driver that accepts, the device stays as it is and ENXIO is returned.
 */
int device_probe_and_attach(device_t dev)
{
    if (dev->state == DS_ATTACHED)
        return 0;
    int err = ENXIO;
    if (dev->driver != NULL) {
        err = probe_with(dev, dev->driver);
    } else if (dev->parent != NULL && dev->parent->devclass != NULL) {
        struct driverlink *dl;
        TAILQ_FOREACH(dl, &dev->parent->devclass->drivers, link)
            if ((err = probe_with(dev, dl->driver)) == 0)
                break;
    }
    if (err)
        return err;
    if (dev->devclass == NULL)
        devclass_join(devclass_create(dev->driver->name), dev, -1);
    return device_attach(dev);
}

int device_detach(device_t dev)
{
    if (dev->state < DS_ATTACHED)
        return 0;
    device_detach_t *detach = (device_detach_t *)nmx_device_method(dev, "device_detach");
    int err = detach ? detach(dev) : 0;
    if (err)
        return err;
    dev->state = DS_NOTPRESENT;
    return 0;
}

int bus_generic_attach(device_t dev)
{
    device_t c;
    TAILQ_FOREACH(c, &dev->children, link)
        (void)device_probe_and_attach(c);
    return 0;
}

int bus_generic_detach(device_t dev)
{
    device_t c;
    TAILQ_FOREACH(c, &dev->children, link) {
        int err = device_detach(c);
        if (err)
            return err;
    }
    return 0;
}

void bus_attach_children(device_t dev)
{
    bus_generic_attach(dev);
}

device_t bus_generic_add_child(device_t dev, u_int order, const char *name, int unit)
{
    return device_add_child(dev, name, unit);
}

/* offers a member device of a bus class to one driver: a child named after the driver, probed and attached */
static void offer(device_t bus, driver_t *driver)
{
    device_t c;
    TAILQ_FOREACH(c, &bus->children, link)
        if (c->driver == driver)
            return;             /* already attached by this driver */
    c = device_add_child(bus, driver->name, -1);
    if (c == NULL)
        return;
    c->ivars = bus->ivars;
    if (probe_with(c, driver) != 0) {
        device_delete_child(bus, c);
        return;
    }
    if (devclass_join(devclass_create(driver->name), c, -1) != 0 || device_attach(c) != 0) {
        devclass_leave(c);
        device_delete_child(bus, c);
    }
}

int devclass_add_driver(devclass_t dc, driver_t *driver, int pass, devclass_t *dcp)
{
    if (dc == NULL || driver == NULL)
        return EINVAL;
    struct driverlink *dl = malloc(sizeof(*dl), M_NMXBUS, M_WAITOK | M_ZERO);
    if (dl == NULL)
        return ENOMEM;
    dl->driver = driver;
    dl->pass = pass;
    mtx_lock(&bus_mtx);
    TAILQ_INSERT_TAIL(&dc->drivers, dl, link);
    mtx_unlock(&bus_mtx);
    if (dcp != NULL)
        *dcp = devclass_create(driver->name);
    /* devices already in the class meet the new driver now (FreeBSD's BUS_DRIVER_ADDED) */
    for (int u = 0; u < DC_UNITS; u++) {
        device_t bus = devclass_get_device(dc, u);
        if (bus != NULL)
            offer(bus, driver);
    }
    return 0;
}

int devclass_delete_driver(devclass_t busclass, driver_t *driver)
{
    if (busclass == NULL || driver == NULL)
        return EINVAL;
    for (int u = 0; u < DC_UNITS; u++) {
        device_t bus = devclass_get_device(busclass, u);
        if (bus == NULL)
            continue;
        device_t c, tmp;
        TAILQ_FOREACH_SAFE(c, &bus->children, link, tmp) {
            if (c->driver != driver)
                continue;
            int err = device_detach(c);
            if (err)
                return err;     /* the driver refused (busy); it stays registered and attached */
            set_driver(c, NULL);
            devclass_leave(c);
            device_delete_child(bus, c);
        }
    }
    struct driverlink *dl;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(dl, &busclass->drivers, link)
        if (dl->driver == driver)
            break;
    if (dl != NULL)
        TAILQ_REMOVE(&busclass->drivers, dl, link);
    mtx_unlock(&bus_mtx);
    if (dl != NULL)
        free(dl, M_NMXBUS);
    return 0;
}

int nmx_bus_add_device(device_t dev, const char *classname)
{
    devclass_t dc = devclass_create(classname);
    if (dc == NULL)
        return ENOMEM;
    bus_topo_lock();
    int err = dev->devclass == dc ? 0 : devclass_join(dc, dev, -1);
    if (err == 0) {
        struct driverlink *dl;
        TAILQ_FOREACH(dl, &dc->drivers, link)
            offer(dev, dl->driver);
    }
    bus_topo_unlock();
    return err;
}

void nmx_bus_remove_device(device_t dev)
{
    bus_topo_lock();
    device_t c, tmp;
    TAILQ_FOREACH_SAFE(c, &dev->children, link, tmp) {
        if (device_detach(c) != 0)
            printf("NullMoth xkpi: %s: detach refused at removal\n", c->nameunit);
        set_driver(c, NULL);
        devclass_leave(c);
        device_delete_child(dev, c);
    }
    devclass_leave(dev);
    bus_topo_unlock();
}

int nmx_bus_call_children(device_t dev, const char *method)
{
    int first = 0;
    device_t c;
    TAILQ_FOREACH(c, &dev->children, link) {
        if (c->state != DS_ATTACHED)
            continue;
        int (*fn)(device_t) = (int (*)(device_t))nmx_device_method(c, method);
        int err = fn ? fn(c) : 0;
        if (err && first == 0)
            first = err;
    }
    return first;
}

/*
 * synchronize_irq(): returns once no handler for interrupt number 'irq' is running. Handlers run on their interrupt
 * source's work loop, so a pass through that work loop's gate waits for one in flight.
 */
void _intr_drain(int irq)
{
    void *cookies[8];
    int n = 0;
    struct nmx_intr *in;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(in, &intrs, link)
        if ((int)in->r->irqno == irq && n < 8)
            cookies[n++] = in->xcookie;
    mtx_unlock(&bus_mtx);
    for (int i = 0; i < n; i++)
        nmx_xnu_pci_irq_drain(cookies[i]);
}

int bus_child_present(device_t child)
{
    struct nmx_device *f = P(child);
    return f != &nopci && nmx_xnu_pci_read(f->xnu, PCIR_VENDOR, 2) != 0xffff;
}

int bus_translate_resource(device_t dev, int type, rman_res_t start, rman_res_t *newstart)
{
    *newstart = start;
    return 0;
}

bus_dma_tag_t bus_get_dma_tag(device_t dev)
{
    struct nmx_device *f = P(dev);
    if (f == &nopci)
        return NULL;
    mtx_lock(&bus_mtx);
    bus_dma_tag_t t = f->dmat;
    mtx_unlock(&bus_mtx);
    if (t != NULL)
        return t;
    t = nmx_busdma_root_tag(f->xnu);
    mtx_lock(&bus_mtx);
    if (f->dmat == NULL) {
        f->dmat = t;
        t = NULL;
    }
    mtx_unlock(&bus_mtx);
    if (t != NULL)
        nmx_busdma_root_free(t);
    return f->dmat;
}

/* devtoname and dev2udev live with the device nodes, in nmx_linux_cdev.c */

/* the root of the device tree, the parent LinuxKPI gives its root device ("/sys/devices") */
static struct nmx_device nmx_root_device = { .name = "root", .nameunit = "root0" };
device_t root_bus = &nmx_root_device;

int bus_bind_intr(device_t dev, struct resource *r, int cpu) { return 0; }
int bus_get_domain(device_t dev, int *domain) { return ENOENT; }

/* ---- resources ---------------------------------------------------------------------------------------------------- */

struct resource *bus_alloc_resource_any(device_t dev, int type, int *rid, u_int flags)
{
    if (dev == NULL || P(dev) == &nopci)
        return NULL;
    struct resource *r = malloc(sizeof(*r), M_NMXBUS, M_WAITOK | M_ZERO);
    if (r == NULL)
        return NULL;
    r->dev = dev;
    r->type = type;
    r->rid = *rid;
    if (type == SYS_RES_MEMORY || type == SYS_RES_IOPORT) {
        r->va = nmx_xnu_pci_map_bar(P(dev)->xnu, *rid, NMX_CACHE_UC, &r->phys, &r->size, &r->map_handle);
        if (r->va == NULL) {
            free(r, M_NMXBUS);
            return NULL;
        }
    } else if (type == SYS_RES_IRQ) {
        /* FreeBSD numbers message-signalled vectors from rid 1; rid 0 is the legacy line */
        if (*rid < 0 || *rid >= IRQ_BLOCK || (*rid > 0 && *rid > P(dev)->dinfo.cfg.msi.msi_alloc)) {
            free(r, M_NMXBUS);
            return NULL;
        }
        r->vector = *rid > 0 ? *rid - 1 : -1;
        r->irqno = (rman_res_t)(P(dev)->irq_base + *rid);
    } else {
        free(r, M_NMXBUS);
        return NULL;
    }
    struct resource_list_entry *rle = resource_list_find(&P(dev)->dinfo.resources, type, *rid);
    if (rle != NULL)
        rle->res = r;
    return r;
}

int bus_release_resource(device_t dev, int type, int rid, struct resource *r)
{
    if (r == NULL)
        return EINVAL;
    struct resource_list_entry *rle = resource_list_find(&P(r->dev)->dinfo.resources, r->type, r->rid);
    if (rle != NULL && rle->res == r)
        rle->res = NULL;
    if (r->map_handle)
        nmx_xnu_unmap(r->map_handle);
    free(r, M_NMXBUS);
    return 0;
}

rman_res_t rman_get_start(struct resource *r) { return r->type == SYS_RES_IRQ ? r->irqno : r->phys; }
rman_res_t rman_get_end(struct resource *r) { return r->type == SYS_RES_IRQ ? r->irqno : r->phys + r->size - 1; }
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
    int err = nmx_xnu_pci_irq_setup(P(dev)->xnu, r->vector, intr_trampoline, in, &in->xcookie);
    if (err == 0) {
        mtx_lock(&bus_mtx);
        TAILQ_INSERT_TAIL(&intrs, in, link);
        mtx_unlock(&bus_mtx);
    }
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
    mtx_lock(&bus_mtx);
    TAILQ_REMOVE(&intrs, in, link);
    mtx_unlock(&bus_mtx);
    nmx_xnu_pci_irq_teardown(in->xcookie);
    free(in, M_NMXBUS);
    return 0;
}

int bus_describe_intr(device_t dev, struct resource *irq, void *cookie, const char *fmt, ...)
{
    return 0;
}

/* ---- pci(9) -------------------------------------------------------------------------------------------------------- */

uint32_t pci_read_config(device_t dev, int reg, int width) { return nmx_xnu_pci_read(P(dev)->xnu, reg, width); }
void pci_write_config(device_t dev, int reg, uint32_t val, int width) { nmx_xnu_pci_write(P(dev)->xnu, reg, val, width); }
uint16_t pci_get_vendor(device_t dev) { return P(dev)->dinfo.cfg.vendor; }
uint16_t pci_get_device(device_t dev) { return P(dev)->dinfo.cfg.device; }
uint16_t pci_get_subvendor(device_t dev) { return P(dev)->dinfo.cfg.subvendor; }
uint16_t pci_get_subdevice(device_t dev) { return P(dev)->dinfo.cfg.subdevice; }
uint8_t pci_get_revid(device_t dev) { return P(dev)->dinfo.cfg.revid; }
uint8_t pci_get_class(device_t dev) { return P(dev)->dinfo.cfg.baseclass; }
uint8_t pci_get_subclass(device_t dev) { return P(dev)->dinfo.cfg.subclass; }
uint8_t pci_get_progif(device_t dev) { return P(dev)->dinfo.cfg.progif; }
uint32_t pci_get_domain(device_t dev) { return P(dev)->dinfo.cfg.domain; }
uint8_t pci_get_bus(device_t dev) { return P(dev)->dinfo.cfg.bus; }
uint8_t pci_get_slot(device_t dev) { return P(dev)->dinfo.cfg.slot; }
uint8_t pci_get_function(device_t dev) { return P(dev)->dinfo.cfg.func; }
uint8_t pci_get_irq(device_t dev) { return P(dev)->dinfo.cfg.intline; }
uint32_t pci_get_devid(device_t dev) { return ((uint32_t)P(dev)->dinfo.cfg.device << 16) | P(dev)->dinfo.cfg.vendor; }
struct pci_devinfo *nmx_pci_devinfo(device_t dev) { return &P(dev)->dinfo; }
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

int pci_msi_count(device_t dev) { return nmx_xnu_pci_msi_count(P(dev)->xnu); }
int pci_msix_count(device_t dev) { return 0; }

int pci_alloc_msi(device_t dev, int *count)
{
    struct nmx_device *f = P(dev);
    if (f->dinfo.cfg.msi.msi_alloc != 0)
        return ENXIO;           /* already allocated (FreeBSD answers the same) */
    int avail = nmx_xnu_pci_msi_count(f->xnu);
    if (avail <= 0 || *count < 1)
        return ENXIO;
    if (*count > avail)
        *count = avail;
    if (*count > IRQ_BLOCK - 1)
        *count = IRQ_BLOCK - 1;
    for (int i = 1; i <= *count; i++)
        rl_add(f, SYS_RES_IRQ, i, (rman_res_t)(f->irq_base + i), 1);
    f->dinfo.cfg.msi.msi_alloc = *count;
    return 0;
}

int pci_alloc_msix(device_t dev, int *count) { return ENXIO; }
int pci_release_msi(device_t dev)
{
    struct nmx_device *f = P(dev);
    if (f->dinfo.cfg.msi.msi_alloc == 0)
        return ENODEV;
    for (int i = 1; i <= f->dinfo.cfg.msi.msi_alloc; i++) {
        struct resource_list_entry *rle = resource_list_find(&f->dinfo.resources, SYS_RES_IRQ, i);
        if (rle != NULL && rle->res != NULL)
            return EBUSY;       /* a vector is still set up */
    }
    for (int i = 1; i <= f->dinfo.cfg.msi.msi_alloc; i++)
        rl_remove(f, SYS_RES_IRQ, i);
    f->dinfo.cfg.msi.msi_alloc = 0;
    return 0;
}

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
    int cap = P(dev)->dinfo.cfg.pcie.pcie_location;
    if (cap == 0)
        return 0;
    uint16_t ctl = (uint16_t)pci_read_config(dev, cap + PCIER_DEVICE_CTL, 2);
    return 128 << ((ctl & PCIEM_CTL_MAX_PAYLOAD) >> 5);
}

int pci_get_max_read_req(device_t dev)
{
    int cap = P(dev)->dinfo.cfg.pcie.pcie_location;
    if (cap == 0)
        return 0;
    uint16_t ctl = (uint16_t)pci_read_config(dev, cap + PCIER_DEVICE_CTL, 2);
    return 128 << ((ctl & PCIEM_CTL_MAX_READ_REQUEST) >> 12);
}

int pci_set_max_read_req(device_t dev, int size)
{
    int cap = P(dev)->dinfo.cfg.pcie.pcie_location;
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
    int cap = P(dev)->dinfo.cfg.pcie.pcie_location;
    return cap ? pci_read_config(dev, cap + reg, width) : (width == 4 ? 0xffffffff : width == 2 ? 0xffff : 0xff);
}

void pcie_write_config(device_t dev, int reg, uint32_t value, int width)
{
    int cap = P(dev)->dinfo.cfg.pcie.pcie_location;
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
        P(dev)->saved_cfg[i] = pci_read_config(dev, i * 4, 4);
    P(dev)->saved = 1;
}

/* restores the header from offset 0x04 on (vendor/device are read-only) with the command register written last */
void pci_restore_state(device_t dev)
{
    if (!P(dev)->saved)
        return;
    for (int i = 15; i >= 2; i--)
        pci_write_config(dev, i * 4, P(dev)->saved_cfg[i], 4);
    pci_write_config(dev, PCIR_COMMAND, P(dev)->saved_cfg[1] & 0xffff, 2);
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

device_t pci_find_base_class_from(uint8_t cls, device_t from)
{
    device_t d, found = NULL;
    int past = from == NULL;
    mtx_lock(&bus_mtx);
    TAILQ_FOREACH(d, &all_devices, all) {
        if (!past) { past = d == from; continue; }
        if (d->is_pci && d->dinfo.cfg.baseclass == cls) { found = d; break; }
    }
    mtx_unlock(&bus_mtx);
    return found;
}

struct resource *pci_reserve_map(device_t dev, device_t child, int type, int rid, rman_res_t start, rman_res_t end,
                                 rman_res_t count, u_int num, u_int flags)
{
    return NULL;
}

/* the largest option ROM PCI allows a device to decode (the ROM BAR size field tops out at 16 MB) */
#define ROM_MAX (16u << 20)

void *vga_pci_map_bios(device_t dev, size_t *size)
{
    *size = 0;
    void *tmp = malloc(ROM_MAX, M_NMXBUS, M_WAITOK);
    if (tmp == NULL)
        return NULL;
    size_t len = nmx_xnu_pci_rom(P(dev)->xnu, tmp, ROM_MAX);
    void *rom = len ? malloc(len, M_NMXBUS, M_WAITOK) : NULL;
    if (rom != NULL) {
        memcpy(rom, tmp, len);
        *size = len;
    }
    free(tmp, M_NMXBUS);
    return rom;
}

void vga_pci_unmap_bios(device_t dev, void *bios)
{
    free(bios, M_NMXBUS);
}

int pci_get_id(device_t dev, int type, uintptr_t *id)
{
    *id = ((uintptr_t)P(dev)->dinfo.cfg.bus << 8) | ((uintptr_t)P(dev)->dinfo.cfg.slot << 3) | P(dev)->dinfo.cfg.func;
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
    if (!nmx_xnu_pci_bar_info(P(dev)->xnu, reg, &phys, &size, &is_io))
        return NULL;
    struct pci_map *pm = &P(dev)->bars[i];
    pm->pm_reg = (uint16_t)reg;
    pm->pm_value = phys | (is_io ? PCIM_BAR_IO_SPACE : 0) | (pci_read_config(dev, reg, 4) & 0xf & (is_io ? 0x3 : 0xf));
    pm->pm_size = size;
    return pm;
}
