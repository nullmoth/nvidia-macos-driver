/*
 * kobj for the FreeBSD sound drivers on the FreeBSD-on-xnu layer. bsdcompat dispatches device methods by name
 * (device_method_t is { name, func }); the sound core's channel and mixer classes are the same kind of table, looked up
 * by name too. mkif.py's generated headers call these.
 */
#ifndef NMX_KOBJ_H
#define NMX_KOBJ_H
#include <sys/bus.h>

typedef struct kobj_method kobj_method_t;
#define KOBJMETHOD(name, func) { #name, (void *)(func) }
#define KOBJMETHOD_END { NULL, NULL }
struct kobj_class { const char *name; kobj_method_t *methods; size_t size; };
typedef struct kobj_class *kobj_class_t;
struct kobj { kobj_class_t ops; };
typedef struct kobj *kobj_t;
#define KOBJ_FIELDS kobj_class_t ops
#define DEFINE_CLASS(name, methods, size) struct kobj_class name ## _class = { #name, methods, size }

void *nmx_device_method(device_t dev, const char *name);
void *nmx_kobj_method(kobj_class_t cls, const char *name);
kobj_t nmx_kobj_create(kobj_class_t cls);        /* zeroed object of cls->size with ops set */
void nmx_kobj_delete(kobj_t obj);

/* newbus instance variables (FreeBSD's BUS_READ_IVAR/BUS_WRITE_IVAR and __BUS_ACCESSOR), dispatched to the parent's
 * bus_read_ivar / bus_write_ivar device method */
typedef int nmx_bus_rw_ivar_t(device_t, device_t, int, uintptr_t *);
static __inline int BUS_READ_IVAR(device_t bus, device_t child, int which, uintptr_t *result)
{
	nmx_bus_rw_ivar_t *f = (nmx_bus_rw_ivar_t *)nmx_device_method(bus, "bus_read_ivar");
	return f ? f(bus, child, which, result) : ENOENT;
}
typedef int nmx_bus_w_ivar_t(device_t, device_t, int, uintptr_t);
static __inline int BUS_WRITE_IVAR(device_t bus, device_t child, int which, uintptr_t value)
{
	nmx_bus_w_ivar_t *f = (nmx_bus_w_ivar_t *)nmx_device_method(bus, "bus_write_ivar");
	return f ? f(bus, child, which, value) : ENOENT;
}
#ifndef __BUS_ACCESSOR
#define __BUS_ACCESSOR(varp, var, ivarp, ivar, type)					\
static __inline type varp ## _get_ ## var(device_t dev)					\
{											\
	uintptr_t v = 0;								\
	BUS_READ_IVAR(device_get_parent(dev), dev, ivarp ## _IVAR_ ## ivar, &v);	\
	return ((type) v);								\
}											\
static __inline void varp ## _set_ ## var(device_t dev, type t)				\
{											\
	BUS_WRITE_IVAR(device_get_parent(dev), dev, ivarp ## _IVAR_ ## ivar, (uintptr_t)t);	\
}
#endif
#endif
