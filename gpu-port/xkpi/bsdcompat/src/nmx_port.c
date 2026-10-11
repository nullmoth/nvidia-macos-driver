/*
 * Start and stop of the whole port, and the kext's view of it (xnu/nmx_kext.cpp calls only these).
 *
 * Order matters: the lock group and CPU facts first, then the allocators, sleep queues, the softclock thread and
 * the task queue thread, then the device-node glue, and only then the SYSINITs (static locks, LinuxKPI, DRM, TTM
 * and the AMD driver's module init, which registers it on the "vgapci" bus class). Stop is the exact reverse.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/kernel.h>
#include <sys/bus.h>
#include <sys/power.h>
#include <sys/eventhandler.h>
#include "nmx_xnu.h"
#include "nmx_port.h"

int nmx_kernel_init(void);
void nmx_kernel_fini(void);
int nmx_malloc_init(void);
void nmx_malloc_fini(void);
int nmx_page_init(void);
void nmx_page_fini(void);
int nmx_sleepq_init(void);
void nmx_sleepq_fini(void);
int nmx_callout_init(void);
void nmx_callout_fini(void);
int nmx_taskqueue_init(void);
void nmx_taskqueue_fini(void);

static int stage;       /* how far start got, so a failed start unwinds exactly what it did */

int nmx_port_start(void)
{
    int err = 0;
#define STEP(call) do { if ((err = (call)) != 0) { printf("NullMoth xkpi: start failed at %s (%d)\n", #call, err); \
                                                   nmx_port_stop(); return err; } stage++; } while (0)
    stage = 0;
    STEP(nmx_kernel_init());
    STEP(nmx_malloc_init());
    STEP(nmx_page_init());
    STEP(nmx_sleepq_init());
    STEP(nmx_callout_init());
    STEP(nmx_taskqueue_init());
    STEP(nmx_xnu_devfs_init());
    STEP(nmx_sysinit_run());
#undef STEP
    return 0;
}

void nmx_port_stop(void)
{
    if (stage >= 8)
        nmx_sysuninit_run();
    if (stage >= 7)
        nmx_xnu_devfs_fini();
    if (stage >= 6)
        nmx_taskqueue_fini();
    if (stage >= 5)
        nmx_callout_fini();
    if (stage >= 4)
        nmx_sleepq_fini();
    if (stage >= 3)
        nmx_page_fini();
    if (stage >= 2)
        nmx_malloc_fini();
    if (stage >= 1)
        nmx_kernel_fini();
    stage = 0;
}

int nmx_port_busy(void)
{
    return nmx_xnu_devfs_busy();
}

static void *attach_on(void *iopci, const char *busclass, int display);

/* offers the IOPCIDevice to the registered drivers; the device record when one attached, else NULL */
void *nmx_port_attach(void *iopci)
{
    return attach_on(iopci, "vgapci", 1);
}

/* the same for a device that is not a display (an NPU): its drivers register on the plain "pci" bus class */
void *nmx_port_attach_pci(void *iopci)
{
    return attach_on(iopci, "pci", 0);
}

static void *attach_on(void *iopci, const char *busclass, int display)
{
    device_t dev = nmx_device_for_pci(iopci);
    if (dev == NULL)
        return NULL;
    if (display)
        nmx_fb_before_attach();
    int err = nmx_bus_add_device(dev, busclass);
    device_t *kids = NULL;
    int n = 0, attached = 0;
    if (err == 0 && device_get_children(dev, &kids, &n) == 0) {
        for (int i = 0; i < n; i++)
            if (device_is_attached(kids[i]))
                attached++;
        free(kids, M_TEMP);
    }
    if (attached == 0) {
        printf("NullMoth xkpi: %s: no driver attached (%d)\n", device_get_nameunit(dev), err);
        nmx_bus_remove_device(dev);
        nmx_device_forget(dev);
        return NULL;
    }
    return dev;
}

void nmx_port_detach(void *dev)
{
    if (dev == NULL)
        return;
    nmx_bus_remove_device(dev);
    nmx_device_forget(dev);
}

/* IOKit power changes: the FreeBSD notifications first (LinuxKPI tracks the sleep type), then the driver */
int nmx_port_power(void *dev, int on)
{
    if (on) {
        int err = nmx_bus_call_children(dev, "device_resume");
        EVENTHANDLER_INVOKE(power_resume, POWER_STYPE_AWAKE);
        return err;
    }
    EVENTHANDLER_INVOKE(power_suspend_early, POWER_STYPE_FW_SUSPEND);
    EVENTHANDLER_INVOKE(power_suspend, POWER_STYPE_FW_SUSPEND);
    return nmx_bus_call_children(dev, "device_suspend");
}

int nmx_port_shutdown(void *dev)
{
    return nmx_bus_call_children(dev, "device_shutdown");
}
