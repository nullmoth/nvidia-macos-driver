/*
 * The kext: one IOService per AMD GPU it is allowed to drive. Compiled with Apple's headers only; everything else is
 * reached through nmx_port.h.
 *
 * Opt-in only. The service matches every AMD display-class device but declines in probe() unless the user asked for
 * it, with the boot argument nmxamd=1 or the device property "nullmoth-amdgpu" (set through the bootloader's
 * DeviceProperties). Without that, the device keeps whatever driver macOS gives it. Taking a GPU that is lighting
 * the boot display resets its display engine, so the opt-in is per user, never a default.
 *
 * The port (the FreeBSD/Linux layer and the AMD driver) starts with the first device and stops when the kext
 * unloads; unloading is refused while any process still has a GPU device node open.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSData.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>
#include "nmx_port.h"

#define AMD_VENDOR 0x1002

/* the display bridge (nmx_fb.cpp) */
IOService *nmx_fb_publish(IOService *gpu, void *dev);
void nmx_fb_unpublish(IOService *fb, IOService *gpu, void *dev);

class NMXAMDGPU : public IOService {
    OSDeclareDefaultStructors(NMXAMDGPU)
    void *dev_;
    IOService *fb_;
public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn setPowerState(unsigned long state, IOService *device) override;
    void systemWillShutdown(IOOptionBits spec) override;
};
OSDefineMetaClassAndStructors(NMXAMDGPU, IOService)

static IOLock *g_lock;
static bool g_started;
static int g_devices;

static bool opted_in(IOPCIDevice *pci)
{
    int v = 0;
    if (PE_parse_boot_argn("nmxamd", &v, sizeof(v)) && v != 0)
        return true;
    OSObject *p = pci->getProperty("nullmoth-amdgpu");
    if (OSBoolean *b = OSDynamicCast(OSBoolean, p))
        return b->isTrue();
    if (OSData *d = OSDynamicCast(OSData, p))
        return d->getLength() > 0 && ((const uint8_t *)d->getBytesNoCopy())[0] != 0;
    return false;
}

IOService *NMXAMDGPU::probe(IOService *provider, SInt32 *score)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || pci->configRead16(kIOPCIConfigVendorID) != AMD_VENDOR)
        return nullptr;
    if (!opted_in(pci))
        return nullptr;     /* not asked for: leave the device to macOS */
    return IOService::probe(provider, score);
}

bool NMXAMDGPU::start(IOService *provider)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || g_lock == nullptr || !IOService::start(provider))
        return false;
    IOLockLock(g_lock);
    if (!g_started) {
        int err = nmx_port_start();
        if (err) {
            IOLockUnlock(g_lock);
            IOLog("NullMoth AMD: the driver layer did not start (%d)\n", err);
            IOService::stop(provider);
            return false;
        }
        g_started = true;
    }
    g_devices++;
    IOLockUnlock(g_lock);

    dev_ = nmx_port_attach(pci);
    if (dev_ == nullptr) {
        IOLog("NullMoth AMD: no driver took %04x:%04x\n", pci->configRead16(kIOPCIConfigVendorID),
              pci->configRead16(kIOPCIConfigDeviceID));
        IOLockLock(g_lock);
        g_devices--;
        IOLockUnlock(g_lock);
        IOService::stop(provider);
        return false;
    }

    static IOPMPowerState states[2] = {
        { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
        { kIOPMPowerStateVersion1, kIOPMPowerOn, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
    };
    PMinit();
    provider->joinPMtree(this);
    registerPowerDriver(this, states, 2);
    registerService();
    /* light the outputs and hand them to macOS; without it the device still renders, with no screen of its own */
    fb_ = nmx_fb_publish(this, dev_);
    IOLog("NullMoth AMD: driving %04x:%04x\n", pci->configRead16(kIOPCIConfigVendorID),
          pci->configRead16(kIOPCIConfigDeviceID));
    return true;
}

void NMXAMDGPU::stop(IOService *provider)
{
    nmx_fb_unpublish(fb_, this, dev_);
    fb_ = nullptr;
    PMstop();
    nmx_port_detach(dev_);
    dev_ = nullptr;
    IOLockLock(g_lock);
    g_devices--;
    IOLockUnlock(g_lock);
    IOService::stop(provider);
}

/* state 0 = sleep, 1 = on */
IOReturn NMXAMDGPU::setPowerState(unsigned long state, IOService *device)
{
    if (dev_ != nullptr) {
        int err = nmx_port_power(dev_, state != 0);
        if (err)
            IOLog("NullMoth AMD: %s returned %d\n", state ? "resume" : "suspend", err);
    }
    return kIOPMAckImplied;
}

void NMXAMDGPU::systemWillShutdown(IOOptionBits spec)
{
    if (dev_ != nullptr)
        nmx_port_shutdown(dev_);
    IOService::systemWillShutdown(spec);
}

/* ---- module start/stop: the stop refuses while device nodes are open ------------------------------------------------- */

extern "C" {
kern_return_t nmx_kmod_start(kmod_info_t *ki, void *data);
kern_return_t nmx_kmod_stop(kmod_info_t *ki, void *data);
}

kern_return_t nmx_kmod_start(kmod_info_t *ki, void *data)
{
    g_lock = IOLockAlloc();
    return g_lock ? KERN_SUCCESS : KERN_FAILURE;
}

kern_return_t nmx_kmod_stop(kmod_info_t *ki, void *data)
{
    IOLockLock(g_lock);
    if (g_started) {
        if (g_devices > 0 || nmx_port_busy() > 0) {
            IOLockUnlock(g_lock);
            IOLog("NullMoth AMD: still in use; not unloading\n");
            return KERN_FAILURE;
        }
        nmx_port_stop();
        g_started = false;
    }
    IOLockUnlock(g_lock);
    IOLockFree(g_lock);
    g_lock = nullptr;
    return KERN_SUCCESS;
}

/* libkmod's entry points; they call _realmain/_antimain below */
extern "C" {
kern_return_t _start(kmod_info_t *ki, void *data);
kern_return_t _stop(kmod_info_t *ki, void *data);
KMOD_EXPLICIT_DECL(com.nullmoth.NMXAMD, "1.0.0", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = nmx_kmod_start;
__private_extern__ kmod_stop_func_t *_antimain = nmx_kmod_stop;
}
