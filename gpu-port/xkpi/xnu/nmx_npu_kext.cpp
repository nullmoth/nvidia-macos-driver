// SPDX-License-Identifier: GPL-2.0
/*
 * NMXNPU: the kext for neural processing units - Intel's NPU (Meteor Lake, Arrow Lake, Lunar Lake, Panther Lake) through
 * Linux's ivpu driver, and AMD's Ryzen AI NPU (XDNA) through Linux's amdxdna driver running on carveout memory, both on
 * the port's layer and DRM accel core. GPL-2.0 like the drivers it carries, so it is a separate kext from NMXGPU with
 * its own copy of the layer; the two never share code or symbols.
 *
 * Opt-in only: probe() declines unless the boot argument nmxnpu=1 or the device property "nullmoth-npu" is set, and
 * the boot picker's -nvoff entry parks it like every NullMoth driver. A matched NPU is offered to the drivers registered
 * on the plain "pci" bus class; the driver publishes /dev/accel/accel0 for userland.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/IONVRAM.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/c++/OSBoolean.h>
#include <libkern/c++/OSData.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>
#include <kern/thread.h>
#include "nmx_port.h"
#include "nmx_xnu.h"

class NMXNPU : public IOService {
    OSDeclareDefaultStructors(NMXNPU)
    void *dev_;
    IOPCIDevice *pci_;
    volatile int attach_;   /* 0 = attach pending on its thread, 1 = attached, -1 = not attached */
    bool pm_;
    static void attachThread(void *arg, wait_result_t);
    void attachNow();
public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn setPowerState(unsigned long state, IOService *device) override;
    void systemWillShutdown(IOOptionBits spec) override;
};
OSDefineMetaClassAndStructors(NMXNPU, IOService)

static IOLock *g_lock;
static bool g_started;
static int g_devices;

static bool opted_in(IOPCIDevice *pci)
{
    int v = 0;
    if (PE_parse_boot_argn("-nvoff", &v, sizeof(v)))
        return false;
    if (PE_parse_boot_argn("nmxnpu", &v, sizeof(v)) && v != 0)
        return true;
    OSObject *p = pci->getProperty("nullmoth-npu");
    if (OSBoolean *b = OSDynamicCast(OSBoolean, p))
        return b->isTrue();
    if (OSData *d = OSDynamicCast(OSData, p))
        return d->getLength() > 0 && ((const uint8_t *)d->getBytesNoCopy())[0] != 0;
    return false;
}

IOService *NMXNPU::probe(IOService *provider, SInt32 *score)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || !opted_in(pci))
        return nullptr;
    return IOService::probe(provider, score);
}

bool NMXNPU::start(IOService *provider)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (pci == nullptr || g_lock == nullptr || !IOService::start(provider))
        return false;
    IOLockLock(g_lock);
    if (!g_started) {
        int err = nmx_port_start();
        if (err) {
            IOLockUnlock(g_lock);
            IOLog("NullMoth NPU: the driver layer did not start (%d)\n", err);
            IOService::stop(provider);
            return false;
        }
        g_started = true;
    }
    g_devices++;
    IOLockUnlock(g_lock);

    /*
     * The driver loads the NPU's firmware while it attaches. During boot matching that read would come before the root
     * and Data volumes are mounted - and mounting waits for matching to settle, so the Mac stopped at the logo (first
     * boot with ivpu attaching, Arrow Lake, 10-11). The attach runs on its own thread once the firmware folder exists.
     */
    pci_ = pci;
    dev_ = nullptr;
    pm_ = false;
    attach_ = 0;
    retain();
    thread_t t = THREAD_NULL;
    if (kernel_thread_start(&NMXNPU::attachThread, this, &t) != KERN_SUCCESS) {
        IOLog("NullMoth NPU: could not start the attach thread\n");
        attach_ = -1;
        release();
        return true;
    }
    thread_deallocate(t);
    return true;
}

void NMXNPU::attachThread(void *arg, wait_result_t)
{
    NMXNPU *self = (NMXNPU *)arg;
    self->attachNow();
    self->release();
}

/*
 * Boot guard. The first attach on a real Arrow Lake NPU panicked 7 s into boot, and with nmxnpu=1 in the boot
 * arguments every start panicked again until the arguments were removed from Linux (10-11). Before attaching, the
 * driver sets an NVRAM marker and writes it out; it clears it once the attach returns. A start that finds the marker
 * still set follows an attach that never returned (a panic or a hang), so it leaves the NPU alone and says so. Turning
 * the test driver off and on again in the app clears the marker (setup deletes it).
 */
static const char *const kAttachMarker = "nullmoth-npu-attaching";

static IODTNVRAM *nmx_nvram()
{
    IORegistryEntry *e = IORegistryEntry::fromPath("/options", gIODTPlane);
    IODTNVRAM *n = OSDynamicCast(IODTNVRAM, e);
    if (n == nullptr && e != nullptr)
        e->release();
    return n;
}

static bool nmx_marker_set()
{
    IODTNVRAM *n = nmx_nvram();
    if (n == nullptr)
        return false;
    OSObject *v = n->copyProperty(kAttachMarker);
    bool set = v != nullptr;
    if (v)
        v->release();
    n->release();
    return set;
}

static void nmx_marker_write(bool on)
{
    IODTNVRAM *n = nmx_nvram();
    if (n == nullptr) {
        IOLog("NullMoth NPU: no NVRAM; the boot guard is off\n");
        return;
    }
    const OSSymbol *k = OSSymbol::withCString(kAttachMarker);
    if (k != nullptr) {
        if (on) {
            OSData *d = OSData::withBytes("1", 1);
            if (d == nullptr || !n->setProperty(k, d))
                IOLog("NullMoth NPU: could not set the boot guard\n");
            if (d)
                d->release();
        } else {
            n->removeProperty(k);
        }
        k->release();
    }
    n->sync();
    n->release();
}

/*
 * Function Level Reset before the driver touches the NPU, as Linux's reset path does (its reset_method for the Arrow
 * Lake NPU is "flr"). Under macOS the NPU's always-on registers were not at their reset values when ivpu started
 * (VPU idle generation off, a power island already on), so power-up failed its NOC handshake or the firmware never
 * answered; Linux on the same machine sees reset values and boots it in 67 ms (10-11). Config space is saved and
 * restored around the reset, which clears BARs and the command register.
 */
static void nmx_npu_flr(IOPCIDevice *pci)
{
    UInt8 cap = 0;
    if (pci->findPCICapability(kIOPCIPCIExpressCapability, &cap) == 0 || cap == 0) {
        IOLog("NullMoth NPU: no PCI Express capability; no reset before attach\n");
        return;
    }
    if (!(pci->configRead32((UInt8)(cap + 0x04)) & (1u << 28))) {   /* Device Capabilities: Function Level Reset */
        IOLog("NullMoth NPU: the device has no Function Level Reset; attaching as found\n");
        return;
    }
    /* wait (up to 100 ms) for outstanding transactions, as the PCIe spec asks before an FLR */
    for (int i = 0; i < 10 && (pci->configRead16((UInt8)(cap + 0x0a)) & (1u << 5)); i++)
        IOSleep(10);
    pci->saveDeviceState();
    pci->configWrite16((UInt8)(cap + 0x08), (UInt16)(pci->configRead16((UInt8)(cap + 0x08)) | (1u << 15)));
    IOSleep(100);
    pci->restoreDeviceState();
    IOLog("NullMoth NPU: function level reset done (vendor %04x)\n", pci->configRead16(kIOPCIConfigVendorID));
}

void NMXNPU::attachNow()
{
    int err = nmx_xnu_wait_firmware_root(300);
    if (err) {
        IOLog("NullMoth NPU: the firmware folder never appeared (%d); the NPU stays off\n", err);
        attach_ = -1;
        return;
    }
    if (nmx_marker_set()) {
        IOLog("NullMoth NPU: the last attach of %04x:%04x never returned (the Mac stopped or restarted); the NPU stays "
              "off. Send logs, then turn the test driver off and on to try again\n",
              pci_->configRead16(kIOPCIConfigVendorID), pci_->configRead16(kIOPCIConfigDeviceID));
        setProperty("nullmoth-npu-state", "parked: the last attach never returned");
        attach_ = -1;
        return;
    }
    nmx_marker_write(true);
    nmx_npu_flr(pci_);
    dev_ = nmx_port_attach_pci(pci_);
    nmx_marker_write(false);
    if (dev_ == nullptr) {
        IOLog("NullMoth NPU: no driver took %04x:%04x\n", pci_->configRead16(kIOPCIConfigVendorID),
              pci_->configRead16(kIOPCIConfigDeviceID));
        attach_ = -1;
        return;
    }
    static IOPMPowerState states[2] = {
        { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
        { kIOPMPowerStateVersion1, kIOPMPowerOn, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
    };
    PMinit();
    pci_->joinPMtree(this);
    registerPowerDriver(this, states, 2);
    pm_ = true;
    registerService();
    IOLog("NullMoth NPU: driving %04x:%04x\n", pci_->configRead16(kIOPCIConfigVendorID),
          pci_->configRead16(kIOPCIConfigDeviceID));
    OSSynchronizeIO();
    attach_ = 1;
}

void NMXNPU::stop(IOService *provider)
{
    /* an attach still running on its thread finishes first (it holds a reference on this object meanwhile) */
    for (int i = 0; attach_ == 0 && i < 6000; i++)
        IOSleep(100);
    if (pm_)
        PMstop();
    pm_ = false;
    nmx_port_detach(dev_);
    dev_ = nullptr;
    IOLockLock(g_lock);
    g_devices--;
    IOLockUnlock(g_lock);
    IOService::stop(provider);
}

IOReturn NMXNPU::setPowerState(unsigned long state, IOService *device)
{
    if (dev_ != nullptr) {
        int err = nmx_port_power(dev_, state != 0);
        if (err)
            IOLog("NullMoth NPU: %s returned %d\n", state ? "resume" : "suspend", err);
    }
    return kIOPMAckImplied;
}

void NMXNPU::systemWillShutdown(IOOptionBits spec)
{
    if (dev_ != nullptr)
        nmx_port_shutdown(dev_);
    IOService::systemWillShutdown(spec);
}

extern "C" {
kern_return_t nmx_npu_kmod_start(kmod_info_t *ki, void *data);
kern_return_t nmx_npu_kmod_stop(kmod_info_t *ki, void *data);
}

kern_return_t nmx_npu_kmod_start(kmod_info_t *ki, void *data)
{
    g_lock = IOLockAlloc();
    return g_lock ? KERN_SUCCESS : KERN_FAILURE;
}

kern_return_t nmx_npu_kmod_stop(kmod_info_t *ki, void *data)
{
    IOLockLock(g_lock);
    if (g_started) {
        if (g_devices > 0 || nmx_port_busy() > 0) {
            IOLockUnlock(g_lock);
            IOLog("NullMoth NPU: still in use; not unloading\n");
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

extern "C" {
kern_return_t _start(kmod_info_t *ki, void *data);
kern_return_t _stop(kmod_info_t *ki, void *data);
KMOD_EXPLICIT_DECL(com.nullmoth.NMXNPU, "1.0.0", _start, _stop)
__private_extern__ kmod_start_func_t *_realmain = nmx_npu_kmod_start;
__private_extern__ kmod_stop_func_t *_antimain = nmx_npu_kmod_stop;
}
