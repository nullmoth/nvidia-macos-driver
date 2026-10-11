/* FreeBSD eventhandler(9) on xnu: named event lists; LinuxKPI registers for shutdown and low-memory events. */
#pragma once
#include <sys/types.h>
typedef void *eventhandler_tag;
#define EVENTHANDLER_PRI_FIRST 0
#define EVENTHANDLER_PRI_ANY 10000
#define EVENTHANDLER_PRI_LAST 20000
#define SHUTDOWN_PRI_FIRST EVENTHANDLER_PRI_FIRST
#define SHUTDOWN_PRI_DEFAULT EVENTHANDLER_PRI_ANY
#define SHUTDOWN_PRI_LAST EVENTHANDLER_PRI_LAST
eventhandler_tag nmx_eventhandler_register(const char *name, void *func, void *arg, int priority);
void nmx_eventhandler_deregister(const char *name, eventhandler_tag tag);
void nmx_eventhandler_invoke(const char *name, int how);
#define EVENTHANDLER_DECLARE(name, type) struct eventhandler_list_##name
#define EVENTHANDLER_DEFINE(name, func, arg, priority)
#define EVENTHANDLER_REGISTER(name, func, arg, priority) nmx_eventhandler_register(#name, (void *)(func), (arg), (priority))
#define EVENTHANDLER_DEREGISTER(name, tag) nmx_eventhandler_deregister(#name, (tag))
/*
 * Every handler of an event the port invokes takes (void *arg, int-sized value): shutdown_final (howto), the power
 * events (enum power_stype), vm_lowmem (flags). The value is the one optional argument, 0 when there is none.
 * (This was a no-op once, and every suspend/resume notification silently went nowhere.)
 */
#define NMX_EH_ARG(z, a, ...) ((int)(a))
#define EVENTHANDLER_INVOKE(name, ...) nmx_eventhandler_invoke(#name, NMX_EH_ARG(0, ##__VA_ARGS__, 0))
typedef void (*vm_lowmem_handler_t)(void *, int);
#define VM_LOW_KMEM 0x01
#define VM_LOW_PAGES 0x02
