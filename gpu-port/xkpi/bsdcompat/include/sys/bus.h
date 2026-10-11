/*
 * FreeBSD newbus on xnu, as far as LinuxKPI uses it. A device_t is the port's record for one IOService (the matched
 * IOPCIDevice, or a child the drivers create): it carries the softc, unit, parent and children.
 *
 * Attachment is a small newbus: drivers register with a bus class (devclass_add_driver: LinuxKPI puts DRM drivers
 * on "vgapci"), the kext registers each IOPCIDevice it matched with its class (nmx_bus_add_device), and every
 * driver of the class is offered the device as FreeBSD's vgapci does: a child is added, probed through the driver's
 * device_probe method and attached through device_attach. Methods are found by name in the driver's table.
 */
#ifndef NMXKPI_SYS_BUS_H
#define NMXKPI_SYS_BUS_H
#include <sys/types.h>
#include <sys/queue.h>
#include <machine/bus.h>
typedef struct nmx_device *device_t;
typedef struct devclass *devclass_t;
typedef struct kobj_method { const char *name; void *func; } device_method_t;
#define DEVMETHOD(name, func) { #name, (void *)(func) }
#define DEVMETHOD_END { NULL, NULL }
typedef struct driver { const char *name; device_method_t *methods; size_t size; } driver_t;
#define DEFINE_CLASS_0(name, classvar, methods, size) driver_t classvar = { #name, methods, size }
#define DEFINE_CLASS_1(name, classvar, methods, size, base) DEFINE_CLASS_0(name, classvar, methods, size)
#define DRIVER_MODULE(name, busname, driver, evh, arg)
#define DRIVER_MODULE_ORDERED(name, busname, driver, evh, arg, order)
#define EARLY_DRIVER_MODULE(name, busname, driver, evh, arg, pass)
#define BUS_PASS_DEFAULT 0x7fffffff
#define BUS_PROBE_SPECIFIC 0
#define BUS_PROBE_DEFAULT (-20)
#define BUS_PROBE_GENERIC (-100)
#define BUS_PROBE_NOWILDCARD (-2000000000)
typedef int device_probe_t(device_t dev);
typedef int device_attach_t(device_t dev);
typedef int device_detach_t(device_t dev);
typedef int device_suspend_t(device_t dev);
typedef int device_resume_t(device_t dev);
typedef int device_shutdown_t(device_t dev);
typedef enum device_state { DS_NOTPRESENT = 10, DS_ALIVE = 20, DS_ATTACHING = 25, DS_ATTACHED = 30 } device_state_t;
void *device_get_softc(device_t dev);
void device_set_softc(device_t dev, void *softc);
device_t device_get_parent(device_t dev);
int device_get_unit(device_t dev);
const char *device_get_nameunit(device_t dev);
const char *device_get_name(device_t dev);
const char *device_get_desc(device_t dev);
void device_set_desc(device_t dev, const char *desc);
void device_set_desc_copy(device_t dev, const char *desc);
driver_t *device_get_driver(device_t dev);
device_state_t device_get_state(device_t dev);
int device_is_attached(device_t dev);
int device_printf(device_t dev, const char *fmt, ...) __printflike(2, 3);
int device_print_prettyname(device_t dev);
struct cdev;
extern device_t root_bus;
const char *devtoname(struct cdev *cdev);
int dev2udev(struct cdev *x);
int device_get_children(device_t dev, device_t **listp, int *countp);
device_t device_add_child(device_t dev, const char *name, int unit);
int device_delete_child(device_t dev, device_t child);
int device_probe_and_attach(device_t dev);
int device_detach(device_t dev);
void device_set_ivars(device_t dev, void *ivars);
void *device_get_ivars(device_t dev);
void device_quiet(device_t dev);
int device_attach(device_t dev);
#define DEVICE_UNIT_ANY (-1)
devclass_t devclass_find(const char *classname);
devclass_t devclass_create(const char *classname);
device_t devclass_get_device(devclass_t dc, int unit);
int devclass_get_maxunit(devclass_t dc);
void bus_topo_lock(void);
void bus_topo_unlock(void);
void bus_topo_assert(void);
int bus_generic_attach(device_t dev);
int bus_generic_detach(device_t dev);
void bus_attach_children(device_t dev);
#define BUS_RESCAN(dev) 0
device_t bus_generic_add_child(device_t dev, u_int order, const char *name, int unit);
int devclass_add_driver(devclass_t dc, driver_t *driver, int pass, devclass_t *dcp);
int devclass_delete_driver(devclass_t busclass, driver_t *driver);
/* true while the hardware still answers (config space does not read all ones) */
int bus_child_present(device_t child);
/* x86 has one address space for CPU and bus: translation is the identity */
int bus_translate_resource(device_t dev, int type, rman_res_t start, rman_res_t *newstart);
/* the kext's side of attachment (see the comment at the top) */
/* the record for an IOPCIDevice (made on first use), its IOPCIDevice (NULL for a device with no PCI function), and
 * dropping the record when the IOPCIDevice goes away */
device_t nmx_device_for_pci(void *iopci);
void *nmx_device_xnu(device_t dev);
void nmx_device_forget(device_t dev);
int nmx_bus_add_device(device_t dev, const char *classname);
void nmx_bus_remove_device(device_t dev);
/* runs a device method ("device_suspend", "device_resume", "device_shutdown") on the attached children of dev */
int nmx_bus_call_children(device_t dev, const char *method);
void *nmx_device_method(device_t dev, const char *name);
struct resource;
struct resource_spec { int type; int rid; int flags; };
/* the resources a PCI function offers: one entry per BAR and interrupt line (FreeBSD's resource_list) */
struct resource_list_entry {
    STAILQ_ENTRY(resource_list_entry) link;
    int type;
    int rid;
    int flags;
    struct resource *res;           /* the allocated resource while a driver holds it */
    rman_res_t start;
    rman_res_t end;
    rman_res_t count;
};
STAILQ_HEAD(resource_list, resource_list_entry);
struct resource_list_entry *resource_list_find(struct resource_list *rl, int type, int rid);
struct resource *bus_alloc_resource_any(device_t dev, int type, int *rid, u_int flags);
int bus_release_resource(device_t dev, int type, int rid, struct resource *r);
int bus_setup_intr(device_t dev, struct resource *r, int flags, int (*filter)(void *), void (*handler)(void *), void *arg, void **cookiep);
int bus_teardown_intr(device_t dev, struct resource *r, void *cookie);
int bus_describe_intr(device_t dev, struct resource *irq, void *cookie, const char *fmt, ...) __printflike(4, 5);
#define RF_ACTIVE 0x0002
int bus_bind_intr(device_t dev, struct resource *r, int cpu);
int bus_get_domain(device_t dev, int *domain);
#define RF_SHAREABLE 0x0004
/*
 * Bus methods a driver calls on its parent to carve a new MMIO range (i915's MCHBAR when the firmware left it off).
 * xnu has no allocator for unassigned PCI address space, so no range is found; the driver reports that and goes on.
 * Every machine the port has seen arrives with MCHBAR already enabled by its firmware.
 */
#define BUS_ALLOC_RESOURCE(bus, child, type, rid, start, end, count, flags) ((struct resource *)NULL)
#define BUS_DEACTIVATE_RESOURCE(bus, child, ...) 0
#define BUS_RELEASE_RESOURCE(bus, child, ...) 0
#include <sys/rman.h>
#endif
