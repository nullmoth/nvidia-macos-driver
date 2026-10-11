/*
 * FreeBSD newbus on xnu, as far as LinuxKPI uses it. A device_t is the port's record for one IOService (the matched
 * IOPCIDevice, or a child the drivers create): it carries the softc, unit, parent and children. Driver attachment
 * itself is IOKit matching, so DRIVER_MODULE and the method tables only record what a driver offers.
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
struct resource;
struct resource_spec { int type; int rid; int flags; };
struct resource *bus_alloc_resource_any(device_t dev, int type, int *rid, u_int flags);
int bus_release_resource(device_t dev, int type, int rid, struct resource *r);
int bus_setup_intr(device_t dev, struct resource *r, int flags, int (*filter)(void *), void (*handler)(void *), void *arg, void **cookiep);
int bus_teardown_intr(device_t dev, struct resource *r, void *cookie);
int bus_describe_intr(device_t dev, struct resource *irq, void *cookie, const char *fmt, ...) __printflike(4, 5);
#define RF_ACTIVE 0x0002
int bus_bind_intr(device_t dev, struct resource *r, int cpu);
int bus_get_domain(device_t dev, int *domain);
#define RF_SHAREABLE 0x0004
#endif
