/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "../nvrm_off.h"
#ifdef NMX_ACCEL
// NMXAccel: this accelerator built for the AMD and Intel GPUs that NMXGPU drives (see nmx_accel_names.h)
#include "nmx_accel_names.h"
#endif
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <pexpert/pexpert.h>
#include <libkern/OSKextLib.h>
#include <mach/kmod.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IORangeAllocator.h>
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IOMemoryDescriptor.h>
#include "IOAccelerator.h"
#include "IOGraphicsAccelerator2.h"
#include "IOAccelEventMachineFast2.h"
#include "IOAccelLegacyDisplayMachine.h"
#include "IOAccelTask.h"
#include "IOAccelMemoryMap.h"
#include "../nvrm_gpuva_abi.h"
#include "../nvrm_utilization_abi.h"
class NVMemoryMap;
static unsigned long long nvAccelGpuVaAlloc(IOAccelMemoryMap *map,
                                           unsigned long long len, unsigned long long align);
static void               nvAccelGpuVaFree(IOAccelMemoryMap *map);
static bool               nvAccelFlipTo(IOAccelResource2 *res);
static bool               nvAccelCopyToScanout(IOAccelResource2 *res);
void                      nvAccelSwapProbe(IOAccelResource2 *a1, IOAccelResource2 *a2);
bool                      nvAccelResHasContent(IOAccelResource2 *res);
static void               nvAccelWriteStamp(int index, unsigned int value);
#ifdef NMX_ACCEL
#define NV_LOG_TAG "NMXAccel: "
#else
#define NV_LOG_TAG "NVAccel: "
#endif
#define ALOG(fmt, ...) do { kprintf(NV_LOG_TAG fmt "\n", ##__VA_ARGS__); IOLog(NV_LOG_TAG fmt "\n", ##__VA_ARGS__); } while (0)
// Per-allocation lines: WindowServer and every Metal app create resources continuously, and logging each one filled the
// system log (#242). They print only with the -nvaccelverbose boot-arg.
static bool nvaccel_verbose() { static int v = -1; if (v < 0) { int x = 0; v = PE_parse_boot_argn("-nvaccelverbose", &x, sizeof x) ? 1 : 0; } return v == 1; }
#define ALOGV(fmt, ...) do { if (nvaccel_verbose()) ALOG(fmt, ##__VA_ARGS__); } while (0)
#include <libkern/c++/OSSymbol.h>
#include "nvrm_vram_abi.h"
#include <IOKit/IODeviceMemory.h>
#include "tahoe_fwd.h"   // macOS 26: inherited slots Apple no longer exports, forwarded through its vtables
#include "nvaccel_stubs.h"
#include "nvkms-kapi.h"
#include <sys/sysctl.h>
#include <libkern/OSAtomic.h>

// ---- GPU core temperature, published into PerformanceStatistics -------------
// Same register the out-of-tree NvidiaSensor.kext reads. nouveau's gp100_temp_get() reads
// 0x020460 as well, but it insists on the "valid" bit (0x20000000) being set and returns
// -ENODEV otherwise; a GPU whose firmware is GSP-RM never sets it, which is why
// gp100_therm_new() refuses to attach at all under GSP. Here the range is the judge instead:
// a value outside 1..125 C is treated as "no reading" rather than published.
static constexpr UInt32   kNvTsensOffset     = 0x20460;
static constexpr UInt32   kNvTsensValueMask  = 0x0001fff8u;
static constexpr unsigned kNvTempMinC        = 1;
static constexpr unsigned kNvTempMaxC        = 125;
// The family rewrites PerformanceStatistics 0.25-2 s apart, so a slower writer would be
// overwritten before iStat reads it; 250 ms keeps our key present essentially always.
static constexpr unsigned kNvTempIntervalMs  = 250;
// Nothing touches BAR0 until the GPU has settled: bring-up still runs for a while after start().
static constexpr unsigned kNvTempFirstMs     = 5000;
static constexpr unsigned kNvUtilIntervalMs  = 1000;
static constexpr uint64_t kNvUtilMaxAgeNs    = 5000000000ULL;

extern "C" {   // kern/thread_call.h and kern/clock.h clash with the family headers here, so the KPIs are declared by hand
struct thread_call *thread_call_allocate(void (*func)(void *, void *), void *param0);
int thread_call_enter_delayed(struct thread_call *call, uint64_t deadline);
void clock_interval_to_deadline(uint32_t interval, uint32_t scale_factor, uint64_t *result);
uint64_t mach_absolute_time(void);
uint64_t mach_continuous_time(void);
void absolutetime_to_nanoseconds(uint64_t abstime, uint64_t *result);
void proc_selfname(char *buf, int size);
}
static volatile SInt64 gCntVmAlloc, gCntVmAllocBytes, gCntVmDealloc, gCntVmDeallocBytes, gCntVmRefused,
                       gCntResNew, gCntResFree, gCntMapCommit, gCntMapRelease, gCntMapFree;
static void nvaccelInstallSysctl(void);
static int gNvAccelArmed = 0;
// 1 once the headless check restored the plugin name without arming (no NVIDIA display head). The Metal plugin reads it:
// a headless GPU hands frames to another GPU's WindowServer through system memory, so IOSurfaces must be copied out.
static int gNvHeadless = 0;
#ifdef NMX_ACCEL
SYSCTL_INT(_debug, OID_AUTO, nmxaccel_headless, CTLFLAG_RD | CTLFLAG_LOCKED | CTLFLAG_KERN, &gNvHeadless, 0,
           "1 = AMD/Intel Metal device for apps; frames are copied to the framebuffer WindowServer draws");
#else
SYSCTL_INT(_debug, OID_AUTO, nvaccel_headless, CTLFLAG_RD | CTLFLAG_LOCKED | CTLFLAG_KERN, &gNvHeadless, 0,
           "1 = no NVIDIA display: Metal device for apps only, WindowServer on the GPU that owns the display");
#endif
// 4 NVIDIA heads (nvfbheads=4) + 1 for an Intel built-in panel (NMIntelFB, IOFBDependentIndex 4): WindowServer composites
// that panel on the NVIDIA card and the display-pipe copy path below writes each frame into the Intel scan-out surface.
static const unsigned kNvMaxFB = 5;
static unsigned gNvHeadsPublished = 0, gNvHeadsRegistered = 0;
static unsigned gNvHeadsPiped = 0;
// The Intel built-in panel (head 4) joins only when this is 1. Off by default while the path is proven: a write at run
// time (then debug.nvaccelfb=3 and a WindowServer restart) can be undone by a reboot, so a hang never needs hands
// (the first virtual-panel test froze the machine at boot and needed a power cycle).
static int gNvIgpu = 0;
SYSCTL_INT(_debug, OID_AUTO, nvaccel_igpu, CTLFLAG_RW | CTLFLAG_LOCKED | CTLFLAG_KERN, &gNvIgpu, 0,
           "1 = give an Intel built-in panel (NMIntelFB, head 4) a display pipe on this accelerator");

extern "C" kern_return_t _start(kmod_info_t *ki, void *d); extern "C" kern_return_t _stop(kmod_info_t *ki, void *d);
#ifdef NMX_ACCEL
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMXAccel, "0.1", _start, _stop) }
#else
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NVAccel, "0.1", _start, _stop) }
#endif
extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }

class NVEventMachine : public IOAccelEventMachineFast2 {
    OSDeclareDefaultStructors(NVEventMachine)
    NM_TAHOE_FWD(NVEventMachine)
public:
    void writeStamp(int s, vendevtCommandRec *r, unsigned int v) APPLE_KEXT_OVERRIDE {
        (void)r;
        nvAccelWriteStamp(s, v);
    }
    void prepareBarrier(vendevtBarrierRec *b) APPLE_KEXT_OVERRIDE { ALOG("EM prepareBarrier(%p)", b); }
    void completeBarrier(vendevtBarrierRec *b) APPLE_KEXT_OVERRIDE { ALOG("EM completeBarrier(%p)", b); }
    void writeBarrierElement(vendevtBarrierRec *b, int i, unsigned int v) APPLE_KEXT_OVERRIDE { ALOG("EM writeBarrierElement(%p, %d, %u)", b, i, v); }
    bool checkGPUProgress() APPLE_KEXT_OVERRIDE { return true; }
    bool checkChannelProgress(int ch) APPLE_KEXT_OVERRIDE { return true; }
    void *eventTimeout(int stamp) APPLE_KEXT_OVERRIDE { ALOG("EM eventTimeout(%d) — ignored, we do not use family channels", stamp); return nullptr; }
};
OSDefineMetaClassAndStructors(NVEventMachine, IOAccelEventMachineFast2)
static void (*gEnableAccel)(const char *why) = nullptr;
static int gNvScanout = 0;
static int gNvAperture = 0;
class NVDisplayMachine : public IOAccelLegacyDisplayMachine {
    OSDeclareDefaultStructors(NVDisplayMachine)
    NM_TAHOE_FWD(NVDisplayMachine)
public:
    bool displayModeWillChange() APPLE_KEXT_OVERRIDE { ALOG("DM displayModeWillChange"); return true; }
    bool displayModeDidChange() APPLE_KEXT_OVERRIDE {
        ALOG("DM displayModeDidChange -> true");
        if (gEnableAccel) gEnableAccel("displayModeDidChange");
        return true;
    }
};
OSDefineMetaClassAndStructors(NVDisplayMachine, IOAccelLegacyDisplayMachine)
#define NV_PRIVATE_VA_BASE  0x800000000000ull
#define NV_PRIVATE_VA_SIZE  0x000010000000ull
class NVTask : public IOAccelTask {
    OSDeclareDefaultStructors(NVTask)
    NM_TAHOE_FWD(NVTask)
public:
    IORangeAllocator *fVa = nullptr;
    unsigned fAllocLogged = 0, fPrivate = 0;
    unsigned long long allocate(const IOAccelMemoryMap *map) APPLE_KEXT_OVERRIDE {
        const unsigned long long len = map ? ((IOAccelMemoryMap *)map)->getLength() : 0;
        const unsigned long long align = map ? *(const volatile unsigned long long *)((const unsigned char *)map + 0x80) : 0;
        unsigned long long got = nvAccelGpuVaAlloc((IOAccelMemoryMap *)map, len, align);
        if (!got && fAllocLogged < 12)
            ALOG("NVTask::allocate: RM refused %llu bytes (align %llu) -- no GPU address", len, align);
        return got;
    }
    void deallocate(const IOAccelMemoryMap *map, unsigned long long addr) APPLE_KEXT_OVERRIDE {
        (void)addr;
        nvAccelGpuVaFree((IOAccelMemoryMap *)map);
    }
};
OSDefineMetaClassAndStructors(NVTask, IOAccelTask)
class NVMemoryMap : public IOAccelMemoryMap {
    OSDeclareDefaultStructors(NVMemoryMap)
    NM_TAHOE_FWD(NVMemoryMap)
public:
    IOAccelMemory     *fMem       = nullptr;
    unsigned int       fHVirt     = 0;
    unsigned int       fHRmMemory = 0;
    unsigned long long fGpuVa     = 0;
    unsigned long long fVaLen     = 0;
    bool               fMapped    = false;
    unsigned int fCommits = 0;
    bool init(IOGraphicsAccelerator2 *a, IOAccelTask *t, IOAccelMemory *m,
              unsigned int o) APPLE_KEXT_OVERRIDE {
        if (!IOAccelMemoryMap::init(a, t, m, o)) return false;
        fMem = m;
        if (fMem) fMem->retain();
        return true;
    }
    void free() APPLE_KEXT_OVERRIDE {
        if (fMem) { fMem->release(); fMem = nullptr; }
        IOAccelMemoryMap::free();
    }
    bool commitIntoGPUPageTable() APPLE_KEXT_OVERRIDE;
    void releaseFromGPUPageTable() APPLE_KEXT_OVERRIDE;
    bool updateGPUPageTable() APPLE_KEXT_OVERRIDE { return fMapped; }
};
OSDefineMetaClassAndStructors(NVMemoryMap, IOAccelMemoryMap)

#define NV_CARRIERS 32
class IOAccelNVRMCarrier : public IOService {
    OSDeclareDefaultStructors(IOAccelNVRMCarrier)
};
OSDefineMetaClassAndStructors(IOAccelNVRMCarrier, IOService)

class NVAccel : public IOGraphicsAccelerator2 {
    OSDeclareDefaultStructors(NVAccel)
    NM_TAHOE_FWD(NVAccel)
    IOAccelNVRMCarrier *fCarriers[NV_CARRIERS] = { nullptr };
    unsigned            fCarrierNext = 0;
    unsigned            fCarrierLogged = 0;
public:
    IOBufferMemoryDescriptor *fStamps = nullptr; IORangeAllocator *fRanges[32] = {};
    uint64_t  getStampMemory(unsigned int *p) APPLE_KEXT_OVERRIDE {
        if (!fStamps) { fStamps = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut | kIOMemoryPhysicallyContiguous, 16384, 4096); if (fStamps) { fStamps->prepare(); bzero(fStamps->getBytesNoCopy(), 16384); } }
        if (p) *p = fStamps ? 4096 : 0;
        ALOG("getStampMemory(%p) -> descriptor %p (%u stamps, 16 KB)", p, fStamps, p ? *p : 0); return (uint64_t)fStamps; }
    OSObject *newEventMachine() APPLE_KEXT_OVERRIDE { NVEventMachine *m = OSTypeAlloc(NVEventMachine); ALOG("newEventMachine -> %p (family inits it)", m); return m; }
    OSObject *newGPUTask(const char *who) {
        for (unsigned i = 0; i < 32; i++) if (!fRanges[i]) {
            fRanges[i] = IORangeAllocator::withRange(1ULL << 40, 4096);
            if (fRanges[i]) fRanges[i]->allocateRange(0, 0x100000);
        }
        NVTask *t = OSTypeAlloc(NVTask); bool ok = t && t->init(this, 1, fRanges);
        ALOG("%s -> %p init=%d", who, t, ok); if (t && !ok) { t->release(); t = nullptr; } return t; }
    OSObject *createUserGPUTask() APPLE_KEXT_OVERRIDE { return newGPUTask("createUserGPUTask"); }
    void populateAccelConfig(IOAccelConfig *c) APPLE_KEXT_OVERRIDE {
        uint8_t *p = reinterpret_cast<uint8_t *>(c);
        if (!p) { ALOG("populateAccelConfig(NULL) — nothing to do"); return; }
        if (((uintptr_t)p & 0xFFF) + 0x70 > 0x1000) {
            ALOG("populateAccelConfig(%p) — 0x70 bytes would cross out of this page; REFUSING to touch it", p);
            return;
        }
        ALOG("populateAccelConfig(%p) — what the FAMILY pre-filled, before we write a byte:", p);
        for (unsigned i = 0; i < 0x70; i += 8)
            ALOG("   cfg+0x%02x = 0x%016llx", i, *(const uint64_t *)(p + i));

        *(uint64_t *)(p + 0x00) = (uint64_t)(uintptr_t)"NVIDIAAccelerator";
        *(uint32_t *)(p + 0x08) = 0x30844300u;
        *(uint32_t *)(p + 0x0c) = 15u;
        *(uint32_t *)(p + 0x18) = 8u;
        *(uint32_t *)(p + 0x30) = 0x40004000u;
        *(uint32_t *)(p + 0x34) = 0x00008000u;
        *(uint32_t *)(p + 0x38) = 0x00008000u;
        *(uint8_t  *)(p + 0x44) = 0;
        *(uint8_t  *)(p + 0x46) = 1;
        *(uint8_t  *)(p + 0x47) = 1;
        *(uint64_t *)(p + 0x48) = 0x0000001F0000000FULL;
        *(uint64_t *)(p + 0x50) = 0x7FFFFFFF0000001FULL;
        *(uint64_t *)(p + 0x58) = 0x0000000C00000000ULL;
        *(uint32_t *)(p + 0x64) = 0x4Bu;
        *(uint32_t *)(p + 0x28) = kNvMaxFB;
        ALOG("populateAccelConfig(%p) — filled: AccelCaps=15, flags=0x30844300, maxtex=16384x16384, maxfb=%u written (cfg+0x2c left %u)",
             p, kNvMaxFB, *(const uint32_t *)(p + 0x2c));
    }
    bool      configureDevice(IOPCIDevice *pci) APPLE_KEXT_OVERRIDE { ALOG("configureDevice(%p) %04x:%04x — RM owns the card (NVRM.kext); nothing to do here yet", pci, pci ? pci->configRead16(0) : 0, pci ? pci->configRead16(2) : 0); return true; }
    void      teardownDevice(IOPCIDevice *pci) APPLE_KEXT_OVERRIDE { ALOG("teardownDevice(%p)", pci); }
    OSObject *createKernelGPUTask() APPLE_KEXT_OVERRIDE { return newGPUTask("createKernelGPUTask"); }
    OSObject *newDisplayPipe() APPLE_KEXT_OVERRIDE {
        NVDisplayPipe *p = OSTypeAlloc(NVDisplayPipe);
        if (!p) { ALOG("newDisplayPipe: OSTypeAlloc FAILED - the family will store this NULL unchecked"); return NULL; }
        ALOG("newDisplayPipe -> %p", p); return p; }
    OSObject *newDisplayMachine() APPLE_KEXT_OVERRIDE { NVDisplayMachine *d = OSTypeAlloc(NVDisplayMachine);
        fDM = d;
        ALOG("newDisplayMachine -> %p", d); return d; }
    OSObject *newGLContext() APPLE_KEXT_OVERRIDE { NVGLContext *c = OSTypeAlloc(NVGLContext); ALOG("newGLContext -> %p", c); return c; }
    OSObject *newCLContext() APPLE_KEXT_OVERRIDE { NVCLContext *c = OSTypeAlloc(NVCLContext); ALOG("newCLContext -> %p", c); return c; }
    OSObject *newSurface() APPLE_KEXT_OVERRIDE { NVSurface *o = OSTypeAlloc(NVSurface); ALOG("newSurface -> %p", o); return o; }
    OSObject *new2DContext() APPLE_KEXT_OVERRIDE { NV2DContext *c = OSTypeAlloc(NV2DContext); ALOG("new2DContext -> %p", c); return c; }
    OSObject *newVideoContext() APPLE_KEXT_OVERRIDE { NVVideoContext *c = OSTypeAlloc(NVVideoContext); ALOG("newVideoContext -> %p", c); return c; }
    OSObject *newSysMemory() APPLE_KEXT_OVERRIDE { NVSysMemory *m = OSTypeAlloc(NVSysMemory); ALOGV("newSysMemory -> %p", m); return m; }
    OSObject *newVidMemory() APPLE_KEXT_OVERRIDE { NVVidMemory *m = OSTypeAlloc(NVVidMemory); ALOGV("newVidMemory -> %p", m); return m; }
    OSObject *newResource() APPLE_KEXT_OVERRIDE { NVResource *r = OSTypeAlloc(NVResource); OSAddAtomic64(1, &gCntResNew); ALOGV("newResource -> %p", r); return r; }
    OSObject *newMemoryMap() APPLE_KEXT_OVERRIDE { NVMemoryMap *m = OSTypeAlloc(NVMemoryMap); ALOG("newMemoryMap -> %p", m); return m; }

    NVDisplayMachine *fDM = nullptr;
    IOPCIDevice *fPCI = nullptr;
    // GPU core temperature: BAR0 is mapped lazily on the first tick, never in start().
    IOMemoryMap        *fTempMap    = nullptr;
    volatile UInt32    *fTempRegs   = nullptr;
    IOTimerEventSource *fTempTimer  = nullptr;
    int                 fTempC      = 0;
    UInt32              fTempFails  = 0;
    bool                fTempLogged = false;
    // RM queries can sleep waiting for firmware. Keep them off the graphics work loop;
    // the temperature timer only copies the most recent result into the registry.
    IOWorkLoop         *fUtilLoop = nullptr;
    IOTimerEventSource *fUtilTimer = nullptr;
    IOService          *fUtilProvider = nullptr;
    IOLock             *fUtilLock = nullptr;
    NVRMUtilization     fUtil = {};
    uint64_t            fUtilUpdated = 0;
    bool                fUtilValid = false;
    IONotifier *fFBNote = nullptr;
    IONotifier *fIntelNote = nullptr;
    static NVAccel *gAccel;
    IOService *fFB = nullptr;
    IOService *volatile fFBs[kNvMaxFB] = {};
    unsigned fFBRegistered = 0;
    IOLock *fRegLock = nullptr;
    OSObject *fMetalPlugin = nullptr;
    static bool fbPublished(void *target, void *ref, IOService *newService, IONotifier *note) {
        (void)ref; (void)note;
        NVAccel *me = (NVAccel *)target;
        if (!me || !newService) return true;
        unsigned idx = 0;
        OSNumber *num = OSDynamicCast(OSNumber, newService->getProperty("IOFBDependentIndex"));
        if (num) idx = num->unsigned32BitValue();
        const char *what = "kept";
        if (idx >= kNvMaxFB) what = "OUT OF RANGE -- this head will get no pipe";
        else {
            newService->retain();
            if (OSCompareAndSwapPtr(nullptr, newService, (void *volatile *)&me->fFBs[idx])) OSBitOrAtomic(1u << idx, &gNvHeadsPublished);
            else { newService->release(); what = "DUPLICATE -- the first one keeps the slot"; }
        }
        newService->retain();
        if (idx == 0 && me->fFBs[0] == newService) {
            IOService *old;
            do { old = me->fFB; } while (!OSCompareAndSwapPtr(old, newService, (void *volatile *)&me->fFB));
            if (old == newService) newService->release();
            else if (old) {
                bool slotted = false;
                for (unsigned j = 0; j < kNvMaxFB; j++) if (me->fFBs[j] == old) slotted = true;
                ALOG("fFB: head 0 (%s) displaced %s, which published first (%s)", newService->getName(), old->getName(),
                     slotted ? "its extra reference returned" : "NOT slotted: keeping its reference rather than risk a reader");
                if (slotted) old->release();
            }
        // only head 0 may become the primary framebuffer: an Intel panel (index 4) publishing first became fFB (10-10 log:
        // "head 0 displaced NMIntelFB, which published first"), and on a laptop with nothing on the NVIDIA ports it stays
        } else if (idx != 0 || !OSCompareAndSwapPtr(nullptr, newService, (void *volatile *)&me->fFB)) newService->release();
        unsigned before = me->fDM ? me->fDM->getFramebufferCount() : 0xFFFFFFFFu;
        ALOG("%s published -- display machine framebuffer count = %u (index %u %s)", newService->getName(), before, idx, what);
        if (gNvAccelArmed)
            ALOG("  index %u published AFTER the arm: it gets a pipe only when debug.nvaccelfb=3 is written again, "
                 "and WindowServer composites it only after a WindowServer restart", idx);
        else
            ALOG("  to register it:  sudo sysctl -w debug.nvaccelfb=1   (2 = re-run the family's own sweep)");
        return true;
    }
    // Headless GPU (laptops whose panel and outputs are all on the Intel or AMD GPU: issues #76, #106). NVKMS finds no
    // display, so no NVRMFB head ever publishes and the withheld MetalPluginName was never restored: Metal never offered
    // the card, although NVRM and NVK run it fine (#106: vkCreateInstance enumerates the GTX 1660 Ti). Apple's own
    // two-GPU MacBook Pros work this way: the integrated GPU drives every display and WindowServer, and the discrete GPU
    // is a second Metal device that apps choose and macOS copies their frames from. So when no head has published by the
    // deadline, the plugin name goes back on WITHOUT arming: apps get the NVIDIA device, and WindowServer (allow-listed
    // "!WindowServer" = only when armed) keeps the GPU that owns the display. Nothing here touches a display pipe or a
    // mode change. Boot-arg -nvaccelnoheadless keeps the old behaviour.
    // The check runs every few seconds rather than once at the deadline: on a laptop whose screen is on the other GPU,
    // NVKMS has listed this card's connectors, with none connected, about 7 s after start, and every app opened before
    // the plugin name comes back never sees the NVIDIA device (Ampere and Turing laptops reported "Metal device: no" at
    // first login while the name came back at 60 s). So the name goes back as soon as both are true: NVKMS reports no
    // display on any of this card's connectors, and another GPU's framebuffer already shows the desktop. A desktop whose
    // monitor is slow to wake has no other GPU's framebuffer and still waits for the deadline.
    static constexpr uint32_t kHeadlessDeadlineSec = 60;   // chosen: NVRMFB heads publish within ~20 s of start (#76 log)
    static constexpr uint32_t kHeadlessPollSec = 2;        // chosen: a small fraction of the ~7 s NVKMS takes to enumerate
    struct thread_call *fHeadlessCall = nullptr;
    uint32_t fHeadlessWaited = 0;
    // true when NVKMS enumerated this card's connectors (NVRMFB writes connected-displays on each display nub it
    // enumerates) and found a display on none of them
    static bool headlessNoDisplayHere() {
        bool seen = false, none = true;
        if (OSIterator *it = getMatchingServices(serviceMatching("NVRMDisplay"))) {
            while (OSObject *o = it->getNextObject()) {
                IOService *s = OSDynamicCast(IOService, o);
                OSNumber *cd = s ? OSDynamicCast(OSNumber, s->getProperty("connected-displays")) : nullptr;
                if (!cd) continue;
                seen = true;
                if (cd->unsigned32BitValue() > 0) none = false;
            }
            it->release();
        }
        return seen && none;
    }
    // the first framebuffer that is not one of this driver's heads; *other is set when it sits on another vendor's GPU
    static void headlessDisplayOwner(char *owner, size_t len, bool *other) {
        *other = false;
        OSIterator *it = getMatchingServices(serviceMatching("IOFramebuffer"));
        if (!it) return;
        while (OSObject *o = it->getNextObject()) {
            IOService *s = OSDynamicCast(IOService, o);
            if (!s || s->metaCast("NVRMNVDAFramebuffer")) continue;
            strlcpy(owner, s->getMetaClass()->getClassName(), len);
            // the Intel panel driver publishes as a head of this accelerator, so it is not another GPU's desktop here
            if (s->metaCast("NMIntelFB")) break;
            IORegistryEntry *e = s->getProvider();
            for (int up = 0; e && up < 8 && !e->metaCast("IOPCIDevice"); up++) e = e->getParentEntry(gIOServicePlane);
            OSData *v = e ? OSDynamicCast(OSData, e->getProperty("vendor-id")) : nullptr;
            if (v && v->getLength() >= 2) {
                const uint8_t *b = (const uint8_t *)v->getBytesNoCopy();
                *other = (uint16_t)(b[0] | (b[1] << 8)) != 0x10de;
            }
            break;
        }
        it->release();
    }
    void armHeadlessCheck() {
        int off = 0;
        if (PE_parse_boot_argn("-nvaccelnoheadless", &off, sizeof off)) { ALOG("headless check: off (-nvaccelnoheadless)"); return; }
        fHeadlessCall = thread_call_allocate(&NVAccel::headlessCheck, this);
        if (!fHeadlessCall) { ALOG("headless check: thread_call_allocate FAILED -- a GPU with no display stays hidden from Metal"); return; }
        fHeadlessWaited = 0;
        uint64_t when = 0;
        clock_interval_to_deadline(kHeadlessPollSec, kSecondScale, &when);
        thread_call_enter_delayed(fHeadlessCall, when);
        ALOG("headless check: armed, every %u s for up to %u s", kHeadlessPollSec, kHeadlessDeadlineSec);
    }
    static void headlessCheck(void *p0, void *) {
        NVAccel *me = (NVAccel *)p0;
        if (!me || me != gAccel) return;
        if (me->fRegLock) IOLockLock(me->fRegLock);
        const char *why = me->fFB ? "a display head published" : !me->fMetalPlugin ? "no plugin name to restore"
                        : me->getProperty("MetalPluginName") ? "the plugin name is already back" : nullptr;
        me->fHeadlessWaited += kHeadlessPollSec;
        if (why) {
            ALOG("headless check: not headless (%s) -- nothing to do", why);
        } else {
            // name the GPU that owns the display, so a log says which driver WindowServer is on
            char owner[64] = "none found";
            bool other = false;
            headlessDisplayOwner(owner, sizeof owner, &other);
            const bool early = other && headlessNoDisplayHere();
            if (!early && me->fHeadlessWaited < kHeadlessDeadlineSec) {
                uint64_t when = 0;
                clock_interval_to_deadline(kHeadlessPollSec, kSecondScale, &when);
                thread_call_enter_delayed(me->fHeadlessCall, when);
                if (me->fRegLock) IOLockUnlock(me->fRegLock);
                return;
            }
            // 2.5.3 field logs (desktops whose only GPU is this card): no head by the deadline and no other GPU showing the
            // desktop. With the name back, CoreDisplay asked this GPU's Metal plugin for WindowServer's device, the plugin
            // refuses WindowServer on a headless GPU, and WindowServer aborted at start ("Failed to create MetalDevice")
            // every ten seconds. With no other GPU there is no display to hand frames to, so the name stays withheld:
            // WindowServer runs unaccelerated on the firmware framebuffer and the desktop comes up, as before the check.
            if (!other && strcmp(owner, "NMIntelFB") != 0) {
                ALOG("headless check: no NVIDIA display head after %u s and no other GPU shows the desktop (%s) -- "
                     "MetalPluginName stays withheld so WindowServer starts on the firmware framebuffer",
                     me->fHeadlessWaited, owner);
                if (me->fRegLock) IOLockUnlock(me->fRegLock);
                return;
            }
            me->setProperty("MetalPluginName", me->fMetalPlugin);
            me->setProperty("NVAccelHeadless", kOSBooleanTrue);
            gNvHeadless = 1;
            ALOG("headless check: %s after %u s -- MetalPluginName restored WITHOUT arming: apps get "
                 "the NVIDIA Metal device, WindowServer stays on the display's GPU (%s)",
                 early ? "no display on any NVIDIA connector and another GPU shows the desktop" : "no NVIDIA display head, another GPU shows the desktop",
                 me->fHeadlessWaited, owner);
            // Not a good start for the boot guard: a card whose connectors carry no display clears the guard in the
            // framebuffer's own enumeration (nvrm-fb.cpp), with the connector list as evidence. Reaching this deadline
            // without that means the framebuffer never enumerated at all, which is a failure the guard must keep counting.
        }
        if (me->fRegLock) IOLockUnlock(me->fRegLock);
    }
    void deliverModeChange() {
        if (!fDM) return;
        uint8_t *dm = (uint8_t *)fDM;
        ALOG("  delivering display_mode_will_change/did_change (nest %u, pipes %u)",
             *(const volatile uint32_t *)(dm + 0x10c), *(const volatile uint32_t *)(dm + 0x108));
        fDM->display_mode_will_change(0);
        fDM->display_mode_did_change(0);
        reportPipeGates("after deliverModeChange");
    }
    void reportPipeGates(const char *when) {
        uint8_t accelFlags = *(const volatile uint8_t *)((const uint8_t *)this + 0xc78);
        ALOG("  GATES %s: accel+0xc78 = 0x%02x (enabled bit 0x2 %s)",
             when, accelFlags, (accelFlags & 0x2) ? "SET" : "CLEAR <- kIOReturnNotReady");
        if (!fDM) return;
        uint8_t *dm = (uint8_t *)fDM;
        uint32_t n = *(const volatile uint32_t *)(dm + 0x108);
        if (n > 8) { ALOG("  GATES %s: implausible pipe count %u -- not walking it", when, n); return; }
        for (uint32_t i = 0; i < n; i++) {
            uint8_t *p = *(uint8_t **)(dm + 0x88 + 8 * i);
            if (!p) { ALOG("  GATES %s: pipe %u is NULL", when, i); continue; }
            ALOG("  GATES %s: pipe %u active(+0x298) = %u, wsaaDefer(+0x282) = %u",
                 when, i, *(const volatile uint8_t *)(p + 0x298), *(const volatile uint8_t *)(p + 0x282));
        }
    }
    unsigned registerPendingLocked(const char *why) {
        unsigned added = 0;
        for (unsigned i = 0; i < kNvMaxFB; i++) {
            IOService *fb = fFBs[i];
            if (!fb) {
                // The Intel panel (index 4, NMIntelFB) does not wait for NVIDIA heads that never publish: a machine with one
                // monitor publishes only head 0, and the walk stopped there, so the Intel head got no pipe (measured 10-10,
                // published 0x11 registered 0x1). Pipes are found by their framebuffer pointer, not their number.
                if (gNvIgpu && i < 4 && ((gNvHeadsPublished >> 4) & 1)) { i = 3; continue; }
                if (gNvHeadsPublished >> i)
                    ALOG("  %s: index %u has not published (published 0x%x): stopping, so no head above it gets a pipe "
                         "this walk", why, i, gNvHeadsPublished);
                break;
            }
            if (fFBRegistered & (1u << i)) continue;
            if (i == 4 && !gNvIgpu) break;   // the Intel panel waits for debug.nvaccel_igpu=1
            unsigned b = fDM->getFramebufferCount();
            ALOG("  %s: calling found_framebuffer(%s index %u) -- count before %u", why, fb->getName(), i, b);
            fDM->found_framebuffer((IOFramebuffer *)fb);
            unsigned c = fDM->getFramebufferCount();
            fFBRegistered |= 1u << i;
            gNvHeadsRegistered = fFBRegistered;
            if (c != b + 1) {
                ALOG("  %s: index %u -- count went %u -> %u; the family did not add it (already known, or the cap "
                     "accel+0xcb0 = %u refused it). Stopping here so no later head takes this one's pipe number.",
                     why, i, b, c, *(const volatile uint32_t *)((const uint8_t *)this + 0xcb0));
                break;
            }
            uint8_t *dm = (uint8_t *)fDM;
            void *pipe = (b < 16) ? *(void *const volatile *)(dm + 0x88 + 8 * b) : nullptr;
            if (!pipe) {
                *(volatile uint32_t *)(dm + 0x108) = b;
                ALOG("  %s: index %u -- the family stored a NULL pipe in slot %u (0xe3b5, unchecked) and counted it; "
                     "count put back %u -> %u so no broadcast dereferences it (0xe320). This head gets no pipe this boot.",
                     why, i, b, c, fDM->getFramebufferCount());
                break;
            }
            IOService *pfb = *(IOService *const volatile *)((const uint8_t *)pipe + 0x98);
            if (pfb != fb) {
                ALOG("  %s: index %u -- pipe %u (%p) drives %p, not %s %p; keeping the pipe (it is real), stopping "
                     "the walk so no later head's pipe number is off by this one", why, i, b, pipe, pfb, fb->getName(), fb);
                break;
            }
            ALOG("  %s: index %u -> pipe %u (%p), count %u", why, i, b, pipe, c);
            gNvHeadsPiped |= 1u << i;
            added++;
        }
        return added;
    }
    void onCountGrewLocked() {
        deliverModeChange();
        gNvAccelArmed = 1;
        if (fMetalPlugin && !getProperty("MetalPluginName")) {
            setProperty("MetalPluginName", fMetalPlugin);
            ALOG("  MetalPluginName restored -- a NEW process that asks Metal for this accelerator "
                 "can now load the plugin (already-running ones will not)");
        }
        if (!getProperty("IOGLBundleName")) {
            setProperty("IOGLBundleName", "AppleMetalOpenGLRenderer");
            ALOG("  IOGLBundleName=AppleMetalOpenGLRenderer -- NEW GL processes get a hardware renderer over Metal");
        }
        ALOG("  debug.nvaccelfb now reads 1 -- armed-only Metal clients may have a device THIS BOOT ONLY "
             "(heads published 0x%x, registered 0x%x, piped 0x%x)", gNvHeadsPublished, gNvHeadsRegistered, gNvHeadsPiped);
    }
    void registerFramebuffer(int how) {
        if (fRegLock) IOLockLock(fRegLock);
        registerFramebufferLocked(how);
        if (fRegLock) IOLockUnlock(fRegLock);
    }
    bool registerFramebufferLocked(int how) {
        if (!fDM) { ALOG("registerFramebuffer(%d): no display machine", how); return false; }
        if (!fFB) { ALOG("registerFramebuffer(%d): no framebuffer has published yet", how); return false; }
        unsigned before = fDM->getFramebufferCount();
        ALOG("registerFramebuffer(%d): framebuffer count before = %u", how, before);
        if (how == 2) {
            if (before != 0) ALOG("  already %u -- refusing to re-run the family's sweep", before);
            else { ALOG("  re-running IOAccelDisplayMachine::start(pci) -- the family's own sweep"); fDM->start(fPCI); }
        } else {
            registerPendingLocked("sysctl");
        }
        unsigned after = fDM->getFramebufferCount();
        ALOG("  framebuffer count after = %u", after);
        if (after > before) { onCountGrewLocked(); return true; }
        return false;
    }
    const OSSymbol *fSymAlloc = nullptr, *fSymFree = nullptr;
    IOReturn askFramebufferForVram(struct NVRMVramRequest *r, bool alloc) {
        if (!fFB) return kIOReturnNotAttached;
        if (!fSymAlloc) fSymAlloc = OSSymbol::withCStringNoCopy(NVRM_VRAM_FN_ALLOC);
        if (!fSymFree)  fSymFree  = OSSymbol::withCStringNoCopy(NVRM_VRAM_FN_FREE);
        const OSSymbol *fn = alloc ? fSymAlloc : fSymFree;
        if (!fn) return kIOReturnNoMemory;
        return fFB->callPlatformFunction(fn, false, r, nullptr, nullptr, nullptr);
    }
    IOReturn askFramebufferForScanout(struct NVRMVramRequest *r, IOService *fb = nullptr) {
        if (!fb) fb = fFB;
        if (!fb) return kIOReturnNotAttached;
        if (!fSymScanout) fSymScanout = OSSymbol::withCStringNoCopy(NVRM_VRAM_FN_SCANOUT);
        return fb->callPlatformFunction(fSymScanout, false, r, nullptr, nullptr, nullptr);
    }
    const OSSymbol *fSymScanout = nullptr;
    IOService *pipeFramebuffer(const void *pipe, unsigned *idx) {
        if (!pipe) return nullptr;
        IOService *fb = *(IOService *const volatile *)((const uint8_t *)pipe + 0x98);
        if (!fb) return nullptr;
        for (unsigned i = 0; i < kNvMaxFB; i++)
            if (fFBs[i] == fb) { if (idx) *idx = i; return fb; }
        return nullptr;
    }
    IOService *fNVRM = nullptr;
    IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type,
                           IOUserClient **handler) APPLE_KEXT_OVERRIDE {
        if ((type & 0xFFFF0000u) != 0x4E560000u) {
            IOReturn fr = IOGraphicsAccelerator2::newUserClient(owningTask, securityID, type, handler);
            if (fUcLogged < 40) { fUcLogged++;
                ALOG("newUserClient(type %u / 0x%x) -> family returned 0x%x, client %s",
                     (unsigned)type, (unsigned)type, (unsigned)fr,
                     (handler && *handler) ? (*handler)->getMetaClass()->getClassName() : "(none)"); }
            return fr;
        }
#ifdef NMX_ACCEL
        // NVIDIA RM clients are NVK's; RADV and ANV talk to NMXGPU's DRM device nodes instead
        ALOG("newUserClient(0x%x): an NVIDIA RM client type on an AMD/Intel accelerator -- refused", type);
        return kIOReturnUnsupported;
#endif

        if (!fNVRM) {
            OSDictionary *m = serviceMatching("NVRM");
            if (m) fNVRM = waitForMatchingService(m, 100ULL * 1000 * 1000);
        }
        if (!fNVRM) { ALOG("newUserClient(0x%x): no NVRM service", type); return kIOReturnNotAttached; }

        const OSSymbol *fnSym = OSSymbol::withCStringNoCopy("nvNewAccelClient");
        if (!fnSym) return kIOReturnNoMemory;
        IOUserClient *uc = nullptr;
        IOReturn r = fNVRM->callPlatformFunction(fnSym, false, (void *)owningTask,
                                                 (void *)(uintptr_t)(type & 0xFFFFu), &uc, nullptr);
        fnSym->release();
        if (r != kIOReturnSuccess || !uc) { ALOG("newUserClient(0x%x): NVRM refused (0x%x)", type, r); return r ? r : kIOReturnNoResources; }

        IOService *carrier = nullptr;
        for (unsigned i = 0; i < NV_CARRIERS && !carrier; i++)
            carrier = fCarriers[(fCarrierNext + i) % NV_CARRIERS];
        fCarrierNext++;
        if (!carrier) {
            ALOG("newUserClient(0x%x): NO CARRIER available -- refusing rather than attaching to the "
                 "accelerator, which sleeps at ~1019 children", type);
            uc->release(); return kIOReturnNoResources;
        }
        if (!uc->attach(carrier))  { ALOG("newUserClient(0x%x): attach failed", type); uc->release(); return kIOReturnInternalError; }
        if (!uc->start(carrier))   { ALOG("newUserClient(0x%x): start failed", type); uc->detach(carrier); uc->release(); return kIOReturnInternalError; }
        ALOG("newUserClient(0x%x) -> %s on the ACCELERATOR (rm type %u)", type, uc->getMetaClass()->getClassName(), type & 0xFFFFu);
        *handler = uc;
        return kIOReturnSuccess;
    }
    static void triggerFromSysctl(int how) {
        if (!gAccel) { ALOG("sysctl debug.nvaccelfb=%d but no NVAccel is running", how); return; }
        if (how >= 4) gNvAperture = 1;
        if (how >= 3) {
            gNvScanout = 1;
            ALOG("  level %d: registering any published head that has no pipe yet", how);
            if (gAccel->fRegLock) IOLockLock(gAccel->fRegLock);
            if (gAccel->registerFramebufferLocked(1))
                ALOG("sysctl debug.nvaccelfb=%d: scanout-backed display pipe ENABLED; the registration just delivered the mode "
                     "change -- not delivering it twice", how);
            else {
                ALOG("sysctl debug.nvaccelfb=%d: scanout-backed display pipe ENABLED, re-delivering the mode change", how);
                gAccel->deliverModeChange();
            }
            if (gAccel->fRegLock) IOLockUnlock(gAccel->fRegLock);
            return;
        }
        gAccel->registerFramebuffer(how);
    }
    static void enableFromHook(const char *why) {
        if (!gAccel) return;
        gAccel->enableAccelerator();
        gAccel->fEnableCount++;
        ALOG("enableAccelerator() from %s (call #%u)", why, gAccel->fEnableCount);
        gAccel->setProperty("NVAccelEnabledFrom", why);
        gAccel->setProperty("NVAccelEnableCount", gAccel->fEnableCount, 32);
    }
    unsigned fEnableCount = 0;
    unsigned fUcLogged = 0;
    virtual IOService *probe(IOService *provider, SInt32 *score) APPLE_KEXT_OVERRIDE {
        ALOG("probe(%s) score %d", provider ? provider->getName() : "?", score ? (int)*score : 0);
        if (IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider)) {
            const unsigned cls = pci->configRead8(0x0B), sub = pci->configRead8(0x0A);
            const unsigned ven = pci->configRead16(0x00), dev = pci->configRead16(0x02);
#ifdef NMX_ACCEL
            static const char *const kind = "AMD/Intel";
            if (cls != 0x03 || (ven != 0x1002 && ven != 0x8086)) {
#else
            static const char *const kind = "NVIDIA";
            if (cls != 0x03 || (sub != 0x00 && sub != 0x02) || ven != 0x10de || dev < 0x0020) {
#endif
                ALOG("%04x:%04x class %02x.%02x is not an %s display device we support - not claiming",
                     ven, dev, cls, sub, kind);
                return NULL;
            }
            ALOG("%s display device %04x:%04x class %02x.%02x - claiming", kind, ven, dev, cls, sub);
        }
        return IOGraphicsAccelerator2::probe(provider, score);
    }
    virtual bool start(IOService *provider) APPLE_KEXT_OVERRIDE {
        { if (nvrm_driver_off()) return false; }
#ifdef NMX_ACCEL
        // NMXGPU decides which AMD/Intel GPUs are driven (nmxamd=1 / nmxintel=1 / nullmoth-gpu); this accelerator only
        // starts on a device it has claimed, so a GPU macOS drives itself never gets a second accelerator
        // No waiting here: NMXGPU attaches from a thread once the disks with its firmware are mounted, and mounting waits
        // for matching to settle - a start() that blocked matching for the claim would hold the boot at the logo. NMXGPU
        // re-runs matching on the device (registerService) after it sets nmxgpu-claimed, which offers it here again.
        if (IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider)) {
            if (pci->getProperty("nmxgpu-claimed") != kOSBooleanTrue) {
                ALOG("NMXGPU has not claimed %04x:%04x yet - not starting now; offered again when it does",
                     pci->configRead16(0x00), pci->configRead16(0x02));
                return false;
            }
            ALOG("NMXGPU drives %04x:%04x - starting", pci->configRead16(0x00), pci->configRead16(0x02));
        }
#else
        uint32_t gate = 0;
        if (!PE_parse_boot_argn("nvaccel", &gate, sizeof gate) || !gate) { ALOG("boot-arg nvaccel=1 absent: NOT starting (boot-loop guard)"); return false; }
        if (IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider)) {
            unsigned waited = 0;
            while (pci->getProperty("nvrm-claimed") != kOSBooleanTrue && waited < 300) { IOSleep(100); waited++; }
            if (pci->getProperty("nvrm-claimed") != kOSBooleanTrue) {
                ALOG("NVRM never claimed %04x:%04x in 30 s - NOT starting the accelerator on a GPU with no RM",
                     pci->configRead16(0x00), pci->configRead16(0x02));
                return false;
            }
            ALOG("NVRM owns %04x:%04x (waited %u ms) - starting", pci->configRead16(0x00), pci->configRead16(0x02), waited * 100);
        }
#endif
        ALOG("start(%s): calling IOGraphicsAccelerator2::start — the family's own bring-up", provider ? provider->getName() : "?");
        bool r = IOGraphicsAccelerator2::start(provider);
        ALOG("IOGraphicsAccelerator2::start -> %d", r);
        if (r) {
            // Tells the Metal plugin that NVResource::pageoff waits for a surface WindowServer's GPU is still writing
            // (nvSurfWaitReady). Only then does the plugin mark WindowServer's surfaces newest in VRAM at commit; with an
            // accelerator that does not wait, that mark would let a CPU lock or a copied present read a surface before
            // its pixels landed, so the plugin keeps marking them after the GPU finishes.
            setProperty("NVAccelSurfaceWait", 1ULL, 32);
            registerService(); ALOG("registerService() called — Metal can now find MetalPluginName=%s", "NVMTLDriver");
            gAccel = this; gEnableAccel = &NVAccel::enableFromHook;
            for (unsigned i = 0; i < NV_CARRIERS; i++) {
                IOAccelNVRMCarrier *c = OSTypeAlloc(IOAccelNVRMCarrier);
                if (!c) break;
                if (!c->init()) { c->release(); break; }
                if (!c->attach(this)) { c->release(); break; }
                if (!c->start(this)) { c->detach(this); c->release(); break; }
                fCarriers[i] = c;
            }
            { unsigned n = 0; for (unsigned i = 0; i < NV_CARRIERS; i++) if (fCarriers[i]) n++;
              ALOG("user-client carriers: %u of %u ready (each holds ~1019 clients before "
                   "IOService::attach starts sleeping)", n, (unsigned)NV_CARRIERS); }
            enableFromHook("start");
            fPCI = OSDynamicCast(IOPCIDevice, provider);
            startTemperature();
            startUtilization();
            ALOG("display machine framebuffer count after the family sweep = %u", fDM ? fDM->getFramebufferCount() : 0xFFFFFFFFu);
            fRegLock = IOLockAlloc();
            ALOG("multi-head: up to %u heads, registration lock %p%s", kNvMaxFB, fRegLock,
                 fRegLock ? "" : " -- NULL: two sysctl writers could interleave a registration");
#ifdef NMX_ACCEL
            // AMD/Intel: the Metal device is for apps; WindowServer draws into NMXGPU's framebuffer, and the plugin copies
            // finished frames into the IOSurfaces it reads (the headless path NVIDIA laptops use)
            nvaccelInstallSysctl();
            gNvHeadless = 1;
            setProperty("NMXAccelHeadless", kOSBooleanTrue);
            ALOG("AMD/Intel accelerator up: Metal device for apps, frames copied to the framebuffer WindowServer draws");
            OSDictionary *fbm = nullptr;
#else
            OSDictionary *fbm = serviceMatching("NVRMNVDAFramebuffer");
#endif
            if (fbm) {
                fFBNote = addMatchingNotification(gIOFirstPublishNotification, fbm, &NVAccel::fbPublished, this, nullptr, 0);
                fbm->release();
                ALOG("watching for NVRMFramebuffer to publish -> notifier %p", fFBNote);
                // a laptop whose built-in panel is on the Intel GPU: NMIntelFB publishes as head 4 of this accelerator
                if (OSDictionary *im = serviceMatching("NMIntelFB")) {
                    fIntelNote = addMatchingNotification(gIOFirstPublishNotification, im, &NVAccel::fbPublished, this, nullptr, 0);
                    im->release();
                    ALOG("watching for an Intel built-in panel (NMIntelFB) -> notifier %p", fIntelNote);
                }
                nvaccelInstallSysctl();
                if (getProperty("MetalPluginName")) {
                    OSObject *mp = getProperty("MetalPluginName"); if (mp) mp->retain();
                    fMetalPlugin = mp;
                    removeProperty("MetalPluginName");
                    ALOG("MetalPluginName withheld -- nothing will load the Metal plugin until "
                         "`sysctl -w debug.nvaccelfb=1` puts it back, and a reboot takes it away again");
                    armHeadlessCheck();
                }
            } else ALOG("serviceMatching(NVRMFramebuffer) returned NULL -- cannot watch for the framebuffer");
        }
        return r;
    }
    // ---- GPU core temperature ------------------------------------------------
    // Published into our own PerformanceStatistics as "Temperature(C)", the key iStat Menus
    // shows on the GPU panel. NVAccel already owns the IOPCIDevice, so no second kext and no
    // helper process are involved: that is the whole point of doing it here.
    void startTemperature() {
        if (fTempTimer || !fPCI) return;
        IOWorkLoop *wl = getWorkLoop();
        if (!wl) { ALOG("temperature: no work loop -- no temperature will be published"); return; }
        fTempTimer = IOTimerEventSource::timerEventSource(this, &NVAccel::tempTimerFired);
        if (!fTempTimer) { ALOG("temperature: timerEventSource FAILED"); return; }
        if (wl->addEventSource(fTempTimer) != kIOReturnSuccess) {
            ALOG("temperature: addEventSource FAILED");
            fTempTimer->release(); fTempTimer = nullptr; return;
        }
        fTempTimer->setTimeoutMS(kNvTempFirstMs);
        ALOG("temperature: will read TSENS 0x%05x every %u ms", kNvTsensOffset, kNvTempIntervalMs);
    }

    void stopTemperature() {
        if (fTempTimer) {
            fTempTimer->cancelTimeout();
            if (IOWorkLoop *wl = getWorkLoop()) wl->removeEventSource(fTempTimer);
            fTempTimer->release(); fTempTimer = nullptr;
        }
        if (fTempMap) { fTempMap->release(); fTempMap = nullptr; }
        fTempRegs = nullptr;
    }

    static void tempTimerFired(OSObject *owner, IOTimerEventSource *sender) {
        (void)sender;
        // Registered with `this` as the owner, so the cast cannot fail; OSDynamicCast is not
        // used because it needs the complete class, and this body sits inside the class.
        NVAccel *me = (NVAccel *)owner;
        if (!me) return;
        me->sampleTemperature();
        me->publishStatistics();
        if (me->fTempTimer) me->fTempTimer->setTimeoutMS(kNvTempIntervalMs);
    }

    void sampleTemperature() {
        if (!fTempRegs) {
            fTempMap = fPCI->mapDeviceMemoryWithRegister(kIOPCIConfigBaseAddress0, 0);
            if (!fTempMap) { ALOG("temperature: BAR0 map FAILED -- giving up"); return; }
            fTempRegs = (volatile UInt32 *)fTempMap->getVirtualAddress();
            if (!fTempRegs) { ALOG("temperature: BAR0 has no virtual address -- giving up"); return; }
        }
        UInt32 raw = fTempRegs[kNvTsensOffset / 4];
        unsigned val = (raw & kNvTsensValueMask) >> 8;
        // A GPU in a low power state can answer 0xffffffff, and 0 means the sensor has not been
        // clocked yet: both fall outside the range and are left unpublished rather than guessed.
        if (raw == 0xffffffffu || val < kNvTempMinC || val > kNvTempMaxC) {
            if (fTempFails++ == 0)
                ALOG("temperature: TSENS raw=0x%08x outside %u..%u C -- not publishing",
                     raw, kNvTempMinC, kNvTempMaxC);
            return;
        }
        fTempFails = 0;
        if (!fTempLogged) { fTempLogged = true; ALOG("temperature: first reading raw=0x%08x -> %u C", raw, val); }
        fTempC = (int)val;
    }

    void startUtilization() {
#ifndef NMX_ACCEL
        if (!fPCI || fUtilLoop) return;
        fUtilLock = IOLockAlloc();
        fUtilLoop = IOWorkLoop::workLoop();
        fUtilTimer = IOTimerEventSource::timerEventSource(kIOTimerEventSourceOptionsPriorityWorkLoop,
                                                        this, &NVAccel::utilTimerFired);
        if (!fUtilLock || !fUtilLoop || !fUtilTimer ||
            fUtilLoop->addEventSource(fUtilTimer) != kIOReturnSuccess) {
            ALOG("utilization: cannot create sampling work loop");
            stopUtilization();
            return;
        }
        fUtilTimer->setTimeoutMS(kNvTempFirstMs);
#endif
    }

    void stopUtilization() {
        if (fUtilTimer) {
            fUtilTimer->cancelTimeout();
            // Removing the source takes the loop gate, so an in-flight RM query finishes
            // before its provider reference and the cache lock can be released.
            if (fUtilLoop && fUtilTimer->getWorkLoop()) fUtilLoop->removeEventSource(fUtilTimer);
            // The callback may have rearmed itself while removal waited for the gate.
            fUtilTimer->cancelTimeout();
            fUtilTimer->release(); fUtilTimer = nullptr;
        }
        if (fUtilLoop) { fUtilLoop->release(); fUtilLoop = nullptr; }
        if (fUtilProvider) { fUtilProvider->release(); fUtilProvider = nullptr; }
        if (fUtilLock) { IOLockFree(fUtilLock); fUtilLock = nullptr; }
        fUtilValid = false;
    }

    static void utilTimerFired(OSObject *owner, IOTimerEventSource *sender) {
        NVAccel *me = (NVAccel *)owner;
        me->sampleUtilization();
        sender->setTimeoutMS(kNvUtilIntervalMs);
    }

    void sampleUtilization() {
        if (fUtilProvider && fUtilProvider->isInactive()) {
            fUtilProvider->release(); fUtilProvider = nullptr;
        }
        if (!fUtilProvider) {
            // Match the RM service on this PCI device, rather than a global GPU 0.
            OSIterator *clients = fPCI->getClientIterator();
            if (clients) {
                while (OSObject *o = clients->getNextObject()) {
                    IOService *service = OSDynamicCast(IOService, o);
                    if (service && service->metaCast("NVRM") && !service->isInactive()) {
                        service->retain(); fUtilProvider = service; break;
                    }
                }
                clients->release();
            }
        }
        NVRMUtilization sample = {};
        sample.version = NVRM_UTILIZATION_ABI_VERSION;
        IOReturn status = kIOReturnNotAttached;
        if (fUtilProvider && !fUtilProvider->isInactive()) {
            const OSSymbol *fn = OSSymbol::withCStringNoCopy(NVRM_UTILIZATION_FN);
            if (fn) {
                status = fUtilProvider->callPlatformFunction(fn, false, &sample, nullptr, nullptr, nullptr);
                fn->release();
            }
        }
        IOLockLock(fUtilLock);
        if (status == kIOReturnSuccess && sample.version == NVRM_UTILIZATION_ABI_VERSION &&
            sample.timestamp && sample.gr <= 10000 && sample.fb <= 10000 &&
            sample.nvenc <= 10000 && sample.nvdec <= 10000) {
            // A successful query may return the same firmware sample. That must not
            // indefinitely refresh a cached value after the GPU stops sampling.
            if (!fUtilUpdated || sample.timestamp != fUtil.timestamp) {
                fUtil = sample;
                fUtilUpdated = mach_continuous_time();
            }
            fUtilValid = true;
        } else {
            fUtilValid = false;
        }
        IOLockUnlock(fUtilLock);
    }

    // Read-copy-modify-write: the family keeps 70-odd keys in this dictionary and replaces it
    // wholesale every 0.25-2 s, so the copy is taken from whatever is in it at this moment.
    void publishStatistics() {
        OSObject *property = copyProperty("PerformanceStatistics");
        OSDictionary *cur = OSDynamicCast(OSDictionary, property);
        OSDictionary *d = cur ? OSDynamicCast(OSDictionary, cur->copyCollection()) : nullptr;
        if (property) property->release();
        if (!d) { d = OSDictionary::withCapacity(8); if (!d) return; }
        if (fTempC) {
            OSNumber *n = OSNumber::withNumber((unsigned long long)fTempC, 32);
            if (n) { d->setObject("Temperature(C)", n); n->release(); }
        }
#ifndef NMX_ACCEL
        NVRMUtilization sample = {};
        bool valid = false;
        if (fUtilLock) {
            IOLockLock(fUtilLock);
            uint64_t age = 0;
            if (fUtilValid) absolutetime_to_nanoseconds(mach_continuous_time() - fUtilUpdated, &age);
            valid = fUtilValid && age <= kNvUtilMaxAgeNs;
            sample = fUtil;
            IOLockUnlock(fUtilLock);
        }
        const char *keys[] = { "Device Utilization %", "GPU Core Utilization %",
                               "GPU Memory Utilization %", "GPU Video Engine Utilization %",
                               "GPU Video Decode Utilization %" };
        const unsigned values[] = { sample.gr, sample.gr, sample.fb, sample.nvenc, sample.nvdec };
        for (unsigned i = 0; i < sizeof values / sizeof values[0]; i++) {
            if (valid) {
                OSNumber *n = OSNumber::withNumber((unsigned long long)((values[i] + 50) / 100), 32);
                if (n) { d->setObject(keys[i], n); n->release(); }
            } else {
                // Missing telemetry is not an idle GPU; remove stale values instead of
                // turning errors, sleep or an unsupported firmware command into 0%.
                d->removeObject(keys[i]);
            }
        }
#endif
        setProperty("PerformanceStatistics", d);
        d->release();
    }

    virtual void stop(IOService *provider) APPLE_KEXT_OVERRIDE {
        ALOG("stop");
        stopTemperature();
        stopUtilization();
        IOGraphicsAccelerator2::stop(provider);
    }
    virtual IOReturn callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4) APPLE_KEXT_OVERRIDE;
};
OSDefineMetaClassAndStructors(NVAccel, IOGraphicsAccelerator2)
NVAccel *NVAccel::gAccel = nullptr;

IOAccelMemory *NVDisplayPipe::initFramebufferResource(unsigned int index, IOAccelResource2 *res)
{
    if (!gNvScanout) {
        ALOG("NVDisplayPipe::initFramebufferResource(index %u): scanout gate CLOSED -- returning NULL, "
             "which is the family's own default (sysctl debug.nvaccelfb=3 opens it)", index);
        return nullptr;
    }
    NVAccel *a = NVAccel::gAccel;
    if (!a || !res) { ALOG("NVDisplayPipe::initFramebufferResource: no accelerator or no resource"); return nullptr; }
    unsigned head = 0;
    IOService *fb = a->pipeFramebuffer(this, &head);
    if (!fb) {
        ALOG("NVDisplayPipe::initFramebufferResource(index %u): pipe %p drives framebuffer %p, which is none of ours -- "
             "refusing (never head 0's pages: that IS the bug)", index, this,
             *(void *const volatile *)((const uint8_t *)this + 0x98));
        return nullptr;
    }

    struct NVRMVramRequest r;
    for (unsigned i = 0; i < sizeof r; i++) ((volatile unsigned char *)&r)[i] = 0;
    r.version = NVRM_VRAM_ABI_VERSION;
    IOReturn ret = a->askFramebufferForScanout(&r, fb);
    if (ret == kIOReturnSuccess && r.kva && !r.phys) {
        // Another GPU's panel (NMIntelFB on a laptop without a MUX): its scanout is Intel memory with a CPU mapping and
        // no NVIDIA VRAM address. No vidmem may stand for it (addToAperture/getApertureMemoryDescriptor would hand
        // BAR1 offset 0 to the GPU), and none may be allocated in its place (fresh VRAM mapped here is the 10-10
        // os_map_kernel_space hang shape). NULL is the family's own default: frames reach this panel only through
        // performTransaction's copy into kva (debug.nvaccel_iop_ok_h4 counts them).
        ALOG("NVDisplayPipe::initFramebufferResource(head %u, index %u): CPU-only scanout %u x %u pitch %u (another GPU's "
             "panel) -- no framebuffer resource, frames are copied", head, index, r.width, r.height, r.pitch);
        return nullptr;
    }
    if (ret != kIOReturnSuccess || !r.phys || !r.actualSize) {
        ALOG("NVDisplayPipe::initFramebufferResource: framebuffer would not describe its scanout (0x%x)", (unsigned)ret);
        return nullptr;
    }

    OSObject *o = a->createVidMemory(nullptr, res, r.actualSize, nullptr);
    NVVidMemory *m = OSDynamicCast(NVVidMemory, o);
    if (!m) {
        ALOG("NVDisplayPipe::initFramebufferResource: createVidMemory gave %p, not an NVVidMemory", o);
        if (o) o->release();
        return nullptr;
    }
    m->fScanout = true;
    m->fPhys    = r.phys;
    m->fBytes   = r.actualSize;
    *(volatile unsigned long long *)((unsigned char *)m + 0xc8) = r.phys;
    ALOG("NVDisplayPipe::initFramebufferResource(head %u, index %u, res %p) -> vidmem %p over the LIVE SCANOUT "
         "phys 0x%llx %u x %u pitch %u (%llu bytes)",
         head, index, res, m, (unsigned long long)r.phys, r.width, r.height, r.pitch,
         (unsigned long long)r.actualSize);
    return (IOAccelMemory *)m;
}

static NVVidMemory *nvres_vidmem(IOAccelResource2 *r)
{
    if (!r) return nullptr;
    OSObject *o = *(OSObject **)((unsigned char *)r + 0x88);
    return OSDynamicCast(NVVidMemory, o);
}

bool NVResource::addToAperture()
{
    NVVidMemory *m = nvres_vidmem(this);
    if (fApLogged < 4) { fApLogged++;
        ALOG("NVResource::addToAperture -> true (vidmem %p phys 0x%llx %llu bytes, already in BAR1)",
             m, m ? m->fPhys : 0ull, m ? m->fBytes : 0ull); }
    return m && m->fPhys && m->fBytes;
}

void NVResource::removeFromAperture() { }

void *NVResource::getApertureMemoryDescriptor(unsigned long long *a0)
{
    if (a0) *a0 = 0;
    if (!gNvAperture) {
        if (fApLogged < 4) { fApLogged++;
            ALOG("NVResource::getApertureMemoryDescriptor: aperture gate CLOSED -- returning nullptr "
                 "(sysctl debug.nvaccelfb=4 opens it)"); }
        return nullptr;
    }
    if (fApDesc) return fApDesc;
    NVVidMemory *m = nvres_vidmem(this);
    if (!m || !m->fPhys || !m->fBytes) {
        if (fApLogged < 8) { fApLogged++;
            ALOG("NVResource::getApertureMemoryDescriptor: no BAR1-backed vidmem at +0x88 (mem %p)", m); }
        return nullptr;
    }
    IODeviceMemory *d = IODeviceMemory::withRange((IOPhysicalAddress)m->fPhys, (IOPhysicalLength)m->fBytes);
    if (!d) { ALOG("NVResource::getApertureMemoryDescriptor: IODeviceMemory::withRange(0x%llx, %llu) FAILED",
                   m->fPhys, m->fBytes); return nullptr; }
    fApDesc = d;
    if (fApLogged < 8) { fApLogged++;
        ALOG("NVResource::getApertureMemoryDescriptor -> %p over BAR1 phys 0x%llx (%llu bytes), offset 0",
             d, m->fPhys, m->fBytes); }
    return fApDesc;
}

static void nvSVForgetResource(NVResource *resource);

void NVResource::free()
{
    nvSVForgetResource(this);
    if (fApDesc) { fApDesc->release(); fApDesc = nullptr; }
    OSAddAtomic64(1, &gCntResFree);
    IOAccelResource2::free();
}

// 1 = a surface keeps its VRAM across the family's deallocPhysical/allocPhysical (see deallocPhysical).
static int gVmSticky = 1;
static SInt64 gCntVmParked, gCntVmUnparked, gCntVmParkReleased;

bool NVVidMemory::allocPhysical()
{
    if (fScanout) {
        if (!fPhys || !fBytes) { ALOG("NVVidMemory::allocPhysical(%p): scanout with no address", this); return false; }
        *(volatile unsigned long long *)((unsigned char *)this + 0xc8) = fPhys;
        ALOG("NVVidMemory::allocPhysical(%p): SCANOUT phys 0x%llx (%llu bytes) -- already backed, nothing allocated",
             this, fPhys, fBytes);
        return true;
    }
    if (fRmHandle) {
        ALOG("NVVidMemory::allocPhysical(%p): already backed by 0x%llx (%llu bytes) -- false, as AMD's does",
             this, fPhys, fBytes);
        return false;
    }
    const unsigned long long want = *(const volatile unsigned long long *)((const unsigned char *)this + 0x40);
    if (!want) { ALOG("NVVidMemory::allocPhysical(%p): length at +0x40 is 0 -- nothing to back", this); return false; }
    if (!NVAccel::gAccel) { ALOG("NVVidMemory::allocPhysical(%p): no accelerator", this); return false; }
    if (fParkHandle) {
        if (fParkBytes >= want) {
            fRmHandle = fParkHandle; fKva = fParkKva; fPhys = fParkPhys; fBytes = fParkBytes; fAllocationCookie = fParkCookie;
            fParkHandle = nullptr; fParkKva = nullptr; fParkPhys = fParkBytes = fParkCookie = 0;
            *(volatile unsigned long long *)((unsigned char *)this + 0xc8) = fPhys;
            OSAddAtomic64(1, &gCntVmUnparked);
            return true;
        }
        releaseParked();   // the resource grew: its old VRAM cannot hold it
    }

    struct NVRMVramRequest r;
    for (unsigned i = 0; i < sizeof r; i++) ((volatile unsigned char *)&r)[i] = 0;
    r.version = NVRM_VRAM_ABI_VERSION;
    r.size    = want;
    IOReturn ret = NVAccel::gAccel->askFramebufferForVram(&r, true);
    if (ret != kIOReturnSuccess || !r.handle || !r.phys) {
        // A full BAR1 window refuses every frame (~100/s on a 256 MB BAR1, a third of everything the machine logs:
        // #387). Same throttle as the framebuffer's nvAllocVram: the first eight, then every 256th with the count;
        // debug.nvaccel_vm_refused keeps the exact total.
        const SInt64 nRefused = OSAddAtomic64(1, &gCntVmRefused) + 1;
        if (nRefused <= 8 || (nRefused & 255) == 0)
            ALOG("NVVidMemory::allocPhysical(%p): %llu bytes REFUSED by the framebuffer (0x%x, #%lld)",
                 this, want, (unsigned)ret, (long long)nRefused);
        // once per boot: how much of the window this side holds as sticky parks (kept across the family's dealloc)
        static volatile SInt32 said = 0;
        if (OSCompareAndSwap(0, 1, &said))
            ALOG("VRAM refused: %lld allocs (%lld bytes), %lld deallocs (%lld bytes), %lld parked now",
                 (long long)gCntVmAlloc, (long long)gCntVmAllocBytes, (long long)gCntVmDealloc, (long long)gCntVmDeallocBytes,
                 (long long)(gCntVmParked - gCntVmUnparked - gCntVmParkReleased));
        return false;
    }
    fRmHandle = r.handle; fKva = r.kva; fPhys = r.phys; fBytes = r.actualSize; fAllocationCookie = r.allocationCookie;
    OSAddAtomic64(1, &gCntVmAlloc); OSAddAtomic64((SInt64)fBytes, &gCntVmAllocBytes);
    *(volatile unsigned long long *)((unsigned char *)this + 0xc8) = fPhys;
    ALOGV("NVVidMemory::allocPhysical(%p): %llu bytes of REAL VRAM -- phys 0x%llx kva %p [BAR1 %llu]",
         this, fBytes, fPhys, fKva, r.mappedTotal);
    return true;
}

void NVVidMemory::deallocPhysical()
{
    if (fScanout) {
        ALOG("NVVidMemory::deallocPhysical(%p): SCANOUT -- not ours to free, only forgetting it", this);
        fPhys = 0; fBytes = 0; fScanout = false;
        *(volatile unsigned long long *)((unsigned char *)this + 0xc8) = 0;
        return;
    }
    if (!fRmHandle) return;
    OSAddAtomic64(1, &gCntVmDealloc); OSAddAtomic64((SInt64)fBytes, &gCntVmDeallocBytes);
    // The family frees a surface's VRAM after copying it back for the CPU, and backs the surface with new VRAM when the
    // GPU needs it again. The Metal plugin imported the old VRAM once and keeps drawing into it, so from then on the
    // display and every copy-back read new, never-written VRAM: black frames, and windows black until a drag made
    // WindowServer import the surface again. Parked instead, this VRAM is what the next allocPhysical hands back, so the
    // surface keeps the memory the plugin draws into. It is released when the resource goes away or outgrows it.
    if (gVmSticky) {
        releaseParked();
        fParkHandle = fRmHandle; fParkKva = fKva; fParkPhys = fPhys; fParkBytes = fBytes; fParkCookie = fAllocationCookie;
        OSAddAtomic64(1, &gCntVmParked);
    } else {
        releaseVram(fRmHandle, fKva, fBytes, fAllocationCookie);
    }
    fRmHandle = nullptr; fKva = nullptr; fPhys = 0; fBytes = 0; fAllocationCookie = 0;
    *(volatile unsigned long long *)((unsigned char *)this + 0xc8) = 0;
}

void NVVidMemory::releaseVram(void *handle, void *kva, unsigned long long bytes, unsigned long long cookie)
{
    struct NVRMVramRequest r;
    for (unsigned i = 0; i < sizeof r; i++) ((volatile unsigned char *)&r)[i] = 0;
    r.version = NVRM_VRAM_ABI_VERSION;
    r.handle = handle; r.kva = kva; r.actualSize = bytes; r.allocationCookie = cookie;
    if (NVAccel::gAccel) NVAccel::gAccel->askFramebufferForVram(&r, false);
}

void NVVidMemory::releaseParked()
{
    if (!fParkHandle) return;
    releaseVram(fParkHandle, fParkKva, fParkBytes, fParkCookie);
    fParkHandle = nullptr; fParkKva = nullptr; fParkPhys = fParkBytes = fParkCookie = 0;
    OSAddAtomic64(1, &gCntVmParkReleased);
}

void NVVidMemory::free()
{
    if (fRmHandle && !fScanout) {
        releaseVram(fRmHandle, fKva, fBytes, fAllocationCookie);
        fRmHandle = nullptr; fKva = nullptr; fPhys = 0; fBytes = 0; fAllocationCookie = 0;
    }
    releaseParked();
    IOAccelVidMemory::free();
}

class IOSurface {
public:
    IOMemoryDescriptor *getMemoryDescriptor() const;
    unsigned long long  getWidth() const;
    unsigned long long  getHeight() const;
    unsigned long long  getBytesPerRow() const;
    unsigned int        getPixelFormat() const;
    unsigned int        getSurfaceID() const;
};
class IOAccelDisplayPipeTransaction2 {
public:
    IOSurface *getPlaneIOSurface(unsigned int, unsigned int) const;
    OSObject  *getPlaneResource(unsigned int, unsigned int) const;
};

static int    gNvIop = 0;
static SInt64 gCntIopOk, gCntIopFail, gCntIopEmpty, gCntIopSrcRes, gCntIopSrcSurf, gCntIopBlank;
static SInt64 gCntIopOkHead[kNvMaxFB];
#define NVIOP_LOG_MAX 40
static unsigned gIopLogs = 0;
#define NVIOPLOG(fmt, ...) do { if (gIopLogs < NVIOP_LOG_MAX) { gIopLogs++; ALOG("iop: " fmt, ##__VA_ARGS__); } } while (0)
#define NVIOP_FAIL(rc, fmt, ...) do { OSAddAtomic64(1, &gCntIopFail); NVIOPLOG(fmt, ##__VA_ARGS__); return (rc); } while (0)

void NVDisplayPipe::iopDropSources()
{
    for (unsigned i = 0; i < kIopSrcSlots; i++) {
        NVIopSrc *e = &fIopSrc[i];
        if (e->map) { e->map->release(); e->map = nullptr; }
        if (e->md)  { e->md->complete(); e->md->release(); e->md = nullptr; }
        e->id = 0;
    }
}

void NVDisplayPipe::free()
{
    iopDropSources();
    IOAccelLegacyDisplayPipe::free();
}

const void *NVDisplayPipe::iopMapSurface(unsigned id, IOMemoryDescriptor *md, unsigned long long need)
{
    for (unsigned i = 0; i < kIopSrcSlots; i++)
        if (fIopSrc[i].md == md && fIopSrc[i].id == id && fIopSrc[i].map)
            return (fIopSrc[i].map->getLength() >= need) ? (const void *)fIopSrc[i].map->getVirtualAddress() : nullptr;
    if (md->getLength() < need) return nullptr;
    NVIopSrc *e = &fIopSrc[fIopNext % kIopSrcSlots]; fIopNext++;
    if (e->map) { e->map->release(); e->map = nullptr; }
    if (e->md)  { e->md->complete(); e->md->release(); e->md = nullptr; }
    e->id = 0;
    if (md->prepare() != kIOReturnSuccess) return nullptr;
    IOMemoryMap *m = md->map(kIOMapReadOnly);
    if (!m) { md->complete(); return nullptr; }
    md->retain(); e->md = md; e->map = m; e->id = id;
    NVIOPLOG("mapped source surface 0x%x (%llu bytes) into slot %u", id, (unsigned long long)m->getLength(), (fIopNext - 1) % kIopSrcSlots);
    return (const void *)m->getVirtualAddress();
}

IOReturn NVDisplayPipe::validateTransaction(IOAccelDisplayPipeTransaction2 *)
{
    return kIOReturnSuccess;
}

static bool nvAccelIopAsyncBusy(const void *pipe);
bool NVDisplayPipe::isTransactionComplete(IOAccelDisplayPipeTransaction2 *)
{
    return gNvIop != 0 && !nvAccelIopAsyncBusy(this);
}

static void nvAccelSurfScanoutSync(IOSurface *s);
static bool nvAccelSurfCopyReady(IOSurface *s, unsigned head);
static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe);
static bool nvSurfWaitReady(unsigned int id);
static void nvAccelIopHome(IOService *fb, unsigned head);
IOReturn NVDisplayPipe::performTransaction(IOAccelDisplayPipeTransaction2 *txn)
{
    if (!gNvIop) return (IOReturn)0xe00002c7;
    if (!txn) NVIOP_FAIL(kIOReturnBadArgument, "performTransaction(NULL)");
    NVAccel *a = NVAccel::gAccel;
    unsigned head = 0;
    IOService *fb = a ? a->pipeFramebuffer(this, &head) : nullptr;
    if (!fb) NVIOP_FAIL(kIOReturnNotAttached, "no accelerator, or pipe %p drives a framebuffer that is none of ours", this);

    IOSurface *s  = txn->getPlaneIOSurface(0, 0);
    OSObject  *ro = txn->getPlaneResource(0, 0);
    if (!s && !ro) { OSAddAtomic64(1, &gCntIopEmpty); return kIOReturnSuccess; }

    const volatile unsigned int *src = nullptr; unsigned w = 0, h = 0, pitch = 0; bool fromRes = false;
    NVResource *res = OSDynamicCast(NVResource, ro);
    NVVidMemory *vm = res ? nvres_vidmem(res) : nullptr;
    static SInt32 gIopK6Logs;
    if (gIopK6Logs < 6) {
        OSIncrementAtomic(&gIopK6Logs);
        const unsigned char *kr = (const unsigned char *)res;
        ALOG("iop: K6 plane 0: surface %s id 0x%x %ux%u row %u fmt 0x%08x | resource %s w %u h %u +0xb8 %u +0xc0 %u vidmem %llu bytes kva %d scanout %d",
             s ? "yes" : "NO", s ? (unsigned)s->getSurfaceID() : 0u, s ? (unsigned)s->getWidth() : 0u, s ? (unsigned)s->getHeight() : 0u,
             s ? (unsigned)s->getBytesPerRow() : 0u, s ? (unsigned)s->getPixelFormat() : 0u,
             ro ? ro->getMetaClass()->getClassName() : "(none)",
             res ? (unsigned)*(const volatile unsigned short *)(kr + 0xb0) : 0u, res ? (unsigned)*(const volatile unsigned short *)(kr + 0xb2) : 0u,
             res ? *(const volatile unsigned int *)(kr + 0xb8) : 0u, res ? *(const volatile unsigned int *)(kr + 0xc0) : 0u,
             vm ? (unsigned long long)vm->fBytes : 0ull, (vm && vm->fKva) ? 1 : 0, vm ? (int)vm->fScanout : -1);
    }
    if (!s && vm && vm->fKva) {
        const unsigned char *rr = (const unsigned char *)res;
        w     = *(const volatile unsigned short *)(rr + 0xb0);
        h     = *(const volatile unsigned short *)(rr + 0xb2);
        pitch = *(const volatile unsigned int   *)(rr + 0xb8);
        if ((unsigned long long)pitch < 4ull * w) NVIOP_FAIL(kIOReturnBadArgument, "resource %ux%u: +0xb8 reads %u, which is not a row pitch (need >= %u) — not a surface buffer", w, h, pitch, 4u * w);
        if ((unsigned long long)h * pitch > vm->fBytes) NVIOP_FAIL(kIOReturnOverrun, "resource %ux%u pitch %u exceeds its %llu bytes", w, h, pitch, (unsigned long long)vm->fBytes);
        src = (const volatile unsigned int *)vm->fKva; fromRes = true;
    } else if (s && nvAccelIopFlip(fb, head, s, this)) {
        OSAddAtomic64(1, &gCntIopSrcSurf); OSAddAtomic64(1, &gCntIopOk); OSAddAtomic64(1, &gCntIopOkHead[head]);
        return kIOReturnSuccess;
    } else if (s) {
        // A copied frame must come from the surface's newest pixels. Copying while they were still only in VRAM put a
        // stale, one-colour buffer on the panel every other frame: full-screen flicker, 5 of 40 flicker recordings
        // (1.13-1.14.1), nvaccel_iop_blank about half of every copied frame, never on machines whose frames all flip.
        // Such a frame now keeps the previous picture on the panel instead.
        nvSurfWaitReady(s->getSurfaceID());   // a copied frame is read here, so its GPU work must be done first
        if (!nvAccelSurfCopyReady(s, head)) return kIOReturnSuccess;
        const unsigned fmt = s->getPixelFormat();
        if (fmt != 0x42475241u) NVIOP_FAIL(kIOReturnUnsupported, "surface 0x%x format 0x%08x is not BGRA", s->getSurfaceID(), fmt);
        const unsigned long long W = s->getWidth(), H = s->getHeight(), P = s->getBytesPerRow();
        if (!W || !H || P < 4 || W > 16384 || H > 16384 || P > 16384ull * 8) NVIOP_FAIL(kIOReturnBadArgument, "surface geometry %llux%llu pitch %llu", W, H, P);
        if (P < 4 * W || (P & 3)) NVIOP_FAIL(kIOReturnBadArgument, "surface %llux%llu: row pitch %llu is under 4*W or not word-aligned", W, H, P);
        IOMemoryDescriptor *md = s->getMemoryDescriptor();
        if (!md) NVIOP_FAIL(kIOReturnNoMemory, "surface 0x%x has no memory descriptor", s->getSurfaceID());
        src = (const volatile unsigned int *)iopMapSurface(s->getSurfaceID(), md, H * P);
        if (!src) NVIOP_FAIL(kIOReturnVMError, "surface 0x%x would not prepare/map %llu bytes", s->getSurfaceID(), H * P);
        w = (unsigned)W; h = (unsigned)H; pitch = (unsigned)P;
    } else {
        NVIOP_FAIL(kIOReturnUnsupported, "plane resource %p is not an NVResource with a CPU mapping, and there is no IOSurface", ro);
    }
    if (!w || !h || pitch < 4) NVIOP_FAIL(kIOReturnBadArgument, "source geometry %ux%u pitch %u", w, h, pitch);

    struct NVRMVramRequest q;
    for (unsigned i = 0; i < sizeof q; i++) ((volatile unsigned char *)&q)[i] = 0;
    q.version = NVRM_VRAM_ABI_VERSION;
    IOReturn ret = a->askFramebufferForScanout(&q, fb);
    if (ret != kIOReturnSuccess || !q.kva || q.pitch < 4 || !q.height) NVIOP_FAIL(kIOReturnNotReady, "framebuffer would not describe its scanout (0x%x kva %p pitch %u)", (unsigned)ret, q.kva, q.pitch);
    if (w != q.width || h != q.height) NVIOP_FAIL(kIOReturnBadArgument, "source %ux%u is not the scanout's %ux%u", w, h, q.width, q.height);

    nvAccelIopHome(fb, head);
    const unsigned words = (((pitch < q.pitch) ? pitch : q.pitch) & ~3u) / 4;
    const unsigned spr = pitch / 4, dpr = q.pitch / 4;
    volatile unsigned int *d = (volatile unsigned int *)q.kva;
    for (unsigned y = 0; y < h; y++) {
        const volatile unsigned int *sp = src + (unsigned long)y * spr;
        volatile unsigned int *dp = d + (unsigned long)y * dpr;
        for (unsigned x = 0; x < words; x++) dp[x] = sp[x];
    }
    OSAddAtomic64(1, fromRes ? &gCntIopSrcRes : &gCntIopSrcSurf);
    const SInt64 n = OSAddAtomic64(1, &gCntIopOk);
    const SInt64 nh = OSAddAtomic64(1, &gCntIopOkHead[head]);
    unsigned nz = 0, varied = 0; const unsigned first = src[0] & 0xffffffu;
    for (unsigned i = 0; i < 8; i++) for (unsigned j = 0; j < 8; j++) {
        unsigned sx = (w * (2 * j + 1)) / 16; if (sx >= spr) sx = spr - 1;
        const unsigned v = src[(unsigned long)((h * (2 * i + 1)) / 16) * spr + sx] & 0xffffffu;
        if (v) nz++;
        if (v != first) varied++;
    }
    if (!varied) OSAddAtomic64(1, &gCntIopBlank);
    if ((n <= 3 || n == 600 || nh == 0) && gIopK6Logs < 12) {
        OSIncrementAtomic(&gIopK6Logs);
        ALOG("iop: K6 transaction #%lld (head %u #%lld) on the panel: %ux%u pitch %u from %s — 64 samples: nonzero %u varied %u", (long long)n, head, (long long)nh, w, h, pitch,
             fromRes ? "our VRAM resource" : "the IOSurface's memory", nz, varied);
    }
    return kIOReturnSuccess;
}

static int nvacceliop_sysctl SYSCTL_HANDLER_ARGS
{
    int v = gNvIop;
    int err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || req->newptr == 0) return err;
    NVAccel *a = NVAccel::gAccel;
    if (!a) return ENXIO;
    if (v) {
        OSDictionary *caps = OSDictionary::withCapacity(2);
        if (!caps) return ENOMEM;
        caps->setObject("DisplayPipeSupported", kOSBooleanTrue);
        caps->setObject("TransactionsSupported", kOSBooleanTrue);
        a->setProperty("IOAccelDisplayPipeCapabilities", caps);
        caps->release();
        gNvIop = 1;
        ALOG("iop: debug.nvaccel_iop=1 -- capabilities published, transaction hooks OPEN");
    } else {
        gNvIop = 0;
        a->removeProperty("IOAccelDisplayPipeCapabilities");
        ALOG("iop: debug.nvaccel_iop=0 -- capabilities removed, transaction hooks answer as the base class");
    }
    return 0;
}
SYSCTL_PROC(_debug, OID_AUTO, nvaccel_iop, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED | CTLFLAG_KERN,
            NULL, 0, nvacceliop_sysctl, "I",
            "K3: 1 = publish IOAccelDisplayPipeCapabilities and perform display-pipe transactions, 0 = behave as the base class (default)");

static int nvaccelfb_sysctl SYSCTL_HANDLER_ARGS
{
    int v = gNvAccelArmed;
    int err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || req->newptr == 0) return err;
    NVAccel::triggerFromSysctl(v);
    return 0;
}
SYSCTL_PROC(_debug, OID_AUTO, nvaccelfb, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED | CTLFLAG_KERN,
            NULL, 0, nvaccelfb_sysctl, "I",
            "register the NVIDIA framebuffer with the accelerator display machine (1 = found_framebuffer, 2 = family sweep)");
#define NVACCEL_CNT(nm, var, desc) SYSCTL_QUAD(_debug, OID_AUTO, nm, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&var, desc)
NVACCEL_CNT(nvaccel_vm_alloc,         gCntVmAlloc,        "NVVidMemory::allocPhysical successes");
NVACCEL_CNT(nvaccel_vm_alloc_bytes,   gCntVmAllocBytes,   "bytes of VRAM granted to NVVidMemory");
NVACCEL_CNT(nvaccel_vm_dealloc,       gCntVmDealloc,      "NVVidMemory::deallocPhysical calls with a handle");
NVACCEL_CNT(nvaccel_vm_dealloc_bytes, gCntVmDeallocBytes, "bytes of VRAM given back by NVVidMemory");
NVACCEL_CNT(nvaccel_vm_refused,       gCntVmRefused,      "allocPhysical refused by the framebuffer");
SYSCTL_INT(_debug, OID_AUTO, nvaccel_vm_sticky, CTLFLAG_RW | CTLFLAG_LOCKED, &gVmSticky, 0, "1 = a surface keeps its VRAM across the family's dealloc/alloc (the memory the plugin draws into)");
NVACCEL_CNT(nvaccel_vm_parked,        gCntVmParked,       "VRAM kept when the family freed a surface's VRAM");
NVACCEL_CNT(nvaccel_vm_unparked,      gCntVmUnparked,     "allocations answered with the surface's kept VRAM");
NVACCEL_CNT(nvaccel_vm_park_released, gCntVmParkReleased, "kept VRAM released (resource freed or grown)");
NVACCEL_CNT(nvaccel_res_new,          gCntResNew,         "NVResource objects created");
NVACCEL_CNT(nvaccel_res_free,         gCntResFree,        "NVResource objects freed");
NVACCEL_CNT(nvaccel_map_commit,       gCntMapCommit,      "GPU page-table commits (MAP_MEMORY_DMA ok)");
NVACCEL_CNT(nvaccel_map_release,      gCntMapRelease,     "GPU page-table releases (UNMAP_ONLY)");
NVACCEL_CNT(nvaccel_map_free,         gCntMapFree,        "GPU VA ranges freed (nvAccelGpuVaFree)");
NVACCEL_CNT(nvaccel_iop_ok,           gCntIopOk,          "K3: display-pipe transactions copied to the panel");
NVACCEL_CNT(nvaccel_iop_ok_h0,        gCntIopOkHead[0],   "frames copied to head 0's panel");
NVACCEL_CNT(nvaccel_iop_ok_h1,        gCntIopOkHead[1],   "frames copied to head 1's panel");
NVACCEL_CNT(nvaccel_iop_ok_h2,        gCntIopOkHead[2],   "frames copied to head 2's panel");
NVACCEL_CNT(nvaccel_iop_ok_h3,        gCntIopOkHead[3],   "frames copied to head 3's panel");
NVACCEL_CNT(nvaccel_iop_ok_h4,        gCntIopOkHead[4],   "frames copied to head 4 (an Intel built-in panel, NMIntelFB)");
SYSCTL_UINT(_debug, OID_AUTO, nvaccel_heads_published, CTLFLAG_RD | CTLFLAG_LOCKED | CTLFLAG_KERN, &gNvHeadsPublished, 0,
            "bit i = head i's framebuffer has published to the accelerator");
SYSCTL_UINT(_debug, OID_AUTO, nvaccel_heads_registered, CTLFLAG_RD | CTLFLAG_LOCKED, &gNvHeadsRegistered, 0,
            "bit i = head i's framebuffer was handed to the display machine (found_framebuffer)");
SYSCTL_UINT(_debug, OID_AUTO, nvaccel_heads_piped, CTLFLAG_RD | CTLFLAG_LOCKED, &gNvHeadsPiped, 0,
            "bit i = head i has a display pipe that drives ITS OWN framebuffer (pipe+0x98 read back)");
static_assert(kNvMaxFB == 5, "kNvMaxFB changed: add debug.nvaccel_iop_ok_h<i> for every new head");
NVACCEL_CNT(nvaccel_iop_fail,         gCntIopFail,        "K3: transactions refused (see the capped iop: log lines)");
NVACCEL_CNT(nvaccel_iop_empty,        gCntIopEmpty,       "K3: transactions with no plane (gamma/options only)");
NVACCEL_CNT(nvaccel_iop_src_res,      gCntIopSrcRes,      "K3: frames sourced from our VRAM resource");
NVACCEL_CNT(nvaccel_iop_src_surf,     gCntIopSrcSurf,     "K3: frames sourced from the IOSurface memory");
NVACCEL_CNT(nvaccel_iop_blank,        gCntIopBlank,       "K6: frames copied whose 64 samples were all ONE colour (a blank frame)");
static SInt64 gCntIopCopySkip;
NVACCEL_CNT(nvaccel_iop_copy_skip,    gCntIopCopySkip,    "frames kept on the panel because the surface's newest pixels were not yet in system memory");
static int    gIopFlip = 0;
static SInt64 gCntIopFlip, gCntIopFlipHome, gCntIopFlipNo[8];
SYSCTL_INT(_debug, OID_AUTO, nvaccel_iop_flip, CTLFLAG_RW | CTLFLAG_LOCKED | CTLFLAG_KERN, &gIopFlip, 0, "1 = scan the composited IOSurface out of its own VRAM (no copy)");
NVACCEL_CNT(nvaccel_iop_flips,        gCntIopFlip,        "frames scanned out of the surface's own VRAM (zero copy)");
NVACCEL_CNT(nvaccel_iop_flip_home,    gCntIopFlipHome,    "flips back to the boot scanout before a copied frame");
NVACCEL_CNT(nvaccel_iop_flip_lock,    gCntIopFlipNo[3],   "copied because the accelerator lock was busy");
NVACCEL_CNT(nvaccel_iop_flip_novram,  gCntIopFlipNo[4],   "copied because the surface has no RM VRAM");
NVACCEL_CNT(nvaccel_iop_flip_stale,   gCntIopFlipNo[5],   "copied because system memory holds the newest copy");
NVACCEL_CNT(nvaccel_iop_flip_refused, gCntIopFlipNo[7],   "copied because the framebuffer refused the flip");
static SInt64 gCntIopUnwritten, gCntIopCopyUnwritten;
NVACCEL_CNT(nvaccel_iop_flip_unwritten, gCntIopUnwritten,     "copied because the family marked VRAM newest on a copy the plugin never wrote");
NVACCEL_CNT(nvaccel_iop_copy_unwritten, gCntIopCopyUnwritten, "copied frames that skipped the page-off because the plugin never wrote the VRAM copy");
// The two decisions the display pipe makes from the family's validity bits, kept as pure functions so they can be
// exercised outside the kernel (tools/test_iop_flip_gate.py). vramNewest is the family's bit (valid[6] & plane);
// sysmemCurrent is valid[0] & plane; pluginWrote is nvAccelPluginWrote(): the resource's recorded write cookie equals the
// allocation cookie of the VRAM it holds now.
// A zero-copy flip scans VRAM out directly, so it needs a VRAM copy that holds a frame: the family's bit alone is not
// enough, the plugin must have written it (its dirty escape sets both).
static inline bool nvAccelIopVramFlippable(bool vramNewest, bool pluginWrote) { return vramNewest && pluginWrote; }
// A copied frame is read from system memory. When the family says that copy is stale, the resource is paged off VRAM
// first; a VRAM copy the plugin never wrote holds nothing worth paging, and paging it would overwrite the pixels the
// plugin copied out. Such a surface is read as it is.
static inline bool nvAccelIopCopyNeedsPageoff(bool sysmemCurrent, bool pluginWrote) { return !sysmemCurrent && pluginWrote; }
static inline bool nvAccelPluginWrote(unsigned long long wroteCookie, unsigned long long allocCookie) { return wroteCookie != 0 && wroteCookie == allocCookie; }
static bool nvAccelResPluginWrote(NVResource *res) { NVVidMemory *vm = res ? nvres_vidmem(res) : nullptr; return vm && nvAccelPluginWrote(res->fPluginWroteCookie, vm->fAllocationCookie); }
static SInt64 gCntIopFlipHit, gCntIopFlipMiss;
NVACCEL_CNT(nvaccel_iop_flip_hit,     gCntIopFlipHit,     "swaps resolved from the cache (no accelerator lock)");
NVACCEL_CNT(nvaccel_iop_flip_miss,    gCntIopFlipMiss,    "swaps that resolved under the accelerator lock");
static int    gIopAsync = 1;

/*
 * Frame completion off WindowServer's thread. NVIDIA's display path never makes the compositor wait for the GPU: a flip
 * completes on the display engine's flip event, and a flip ordered behind a fence waits for it in a worker
 * (nvidia-drm-modeset.c:636/692). Here WindowServer waited for every composite to finish on the GPU before swapping
 * (issue #196: ~1.3 ms of each 6.9 ms frame at 144 Hz, ~95-105 presents/s). Now the plugin marks the surfaces a
 * WindowServer command buffer renders as pending when it commits and ready when the GPU has finished them, and the swap
 * waits for that here: on the async flip thread for a zero-copy flip, before reading for a copied frame. A mark answers
 * 0 while this is on; the plugin keeps WindowServer's own wait when it answers anything else.
 */
static int gSurfGpuOn = 1;
static SInt64 gCntSurfMarks, gCntSurfWaits, gCntSurfTimeouts;
static int gSurfWaitMaxUs;
static const unsigned kSurfGpuWaitMs = 100;   // a GPU that has not finished a composite in 100 ms is hung; flip anyway
struct NVSurfPending { unsigned int id; unsigned int seq; };
static NVSurfPending gSurfPend[32];
static unsigned int gSurfPendSeq;
static IOLock *gSurfPendLock;
static IOLock *nvSurfLock()
{
    if (!gSurfPendLock) { IOLock *l = IOLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gSurfPendLock)) IOLockFree(l); }
    return gSurfPendLock;
}
static void nvSurfMark(unsigned int id, bool pending)
{
    IOLock *l = nvSurfLock();
    if (!l || !id) return;
    IOLockLock(l);
    unsigned slot = 32, oldest = 0;
    for (unsigned i = 0; i < 32; i++) {
        if (gSurfPend[i].id == id) { slot = i; break; }
        if (slot == 32 && !gSurfPend[i].id) slot = i;
        if (gSurfPend[i].seq < gSurfPend[oldest].seq) oldest = i;
    }
    if (pending) {
        if (slot == 32) slot = oldest;   // 32 surfaces in flight at once is not a compositor; the oldest mark goes
        gSurfPend[slot].id = id; gSurfPend[slot].seq = ++gSurfPendSeq;
    } else if (slot < 32 && gSurfPend[slot].id == id) {
        gSurfPend[slot].id = 0;
        IOLockWakeup(l, gSurfPend, false);
    }
    IOLockUnlock(l);
    OSAddAtomic64(1, &gCntSurfMarks);
}
static bool nvSurfWaitReady(unsigned int id)
{
    IOLock *l = gSurfPendLock;
    if (!l || !id || !gSurfGpuOn) return true;   // switched off: marks left from before never hold a swap
    uint64_t start = mach_absolute_time(), deadline = 0;
    clock_interval_to_deadline(kSurfGpuWaitMs, kMillisecondScale, &deadline);
    bool waited = false, ready = true;
    IOLockLock(l);
    for (;;) {
        unsigned i = 0;
        while (i < 32 && gSurfPend[i].id != id) i++;
        if (i == 32) break;
        if (mach_absolute_time() >= deadline) { gSurfPend[i].id = 0; ready = false; break; }
        waited = true;
        IOLockSleepDeadline(l, gSurfPend, deadline, THREAD_UNINT);
    }
    IOLockUnlock(l);
    if (waited) {
        uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - start, &ns);
        if ((int)(ns / 1000) > gSurfWaitMaxUs) gSurfWaitMaxUs = (int)(ns / 1000);
        OSAddAtomic64(1, ready ? &gCntSurfWaits : &gCntSurfTimeouts);
    }
    return ready;
}
SYSCTL_INT(_debug, OID_AUTO, nvaccel_surface_gpu, CTLFLAG_RD | CTLFLAG_LOCKED, &gSurfGpuOn, 0, "1 = WindowServer swaps wait for GPU completion here (same value as nvaccel_surface_gpu_on)");
SYSCTL_INT(_debug, OID_AUTO, nvaccel_surface_gpu_on, CTLFLAG_RW | CTLFLAG_LOCKED, &gSurfGpuOn, 0, "0 = WindowServer waits for its own composites again (the plugin re-asks every second)");
NVACCEL_CNT(nvaccel_surface_gpu_marks,    gCntSurfMarks,    "pending/ready marks from WindowServer");
NVACCEL_CNT(nvaccel_surface_gpu_waits,    gCntSurfWaits,    "swaps that waited here for their composite to finish");
NVACCEL_CNT(nvaccel_surface_gpu_timeouts, gCntSurfTimeouts, "swaps whose composite did not finish within 100 ms");
SYSCTL_INT(_debug, OID_AUTO, nvaccel_surface_gpu_wait_max_us, CTLFLAG_RD | CTLFLAG_LOCKED, &gSurfWaitMaxUs, 0, "longest swap wait for a composite, us");
static SInt64 gCntIopAsync, gCntIopAsyncRefused, gCntIopAsyncDrop;
SYSCTL_INT(_debug, OID_AUTO, nvaccel_iop_async, CTLFLAG_RW | CTLFLAG_LOCKED, &gIopAsync, 0, "1 = the swap runs on a thread call and completes through signalTransactionInterrupt (Apple's shape); 0 = the earlier synchronous flip");
NVACCEL_CNT(nvaccel_iop_async_flips,   gCntIopAsync,        "swaps completed asynchronously");
NVACCEL_CNT(nvaccel_iop_async_refused, gCntIopAsyncRefused, "async swaps NVKMS refused (that frame stayed off the panel)");
NVACCEL_CNT(nvaccel_iop_async_drop,    gCntIopAsyncDrop,    "frames dropped because a flip was still in flight past a refresh period");
static SInt64 gCntIopLate;
NVACCEL_CNT(nvaccel_iop_late,          gCntIopLate,         "frames kept on the panel because the previous flip had not reached it yet (framebuffer answered busy)");
static int gCrcHead = 0;
SYSCTL_INT(_debug, OID_AUTO, nvaccel_crc_head, CTLFLAG_RW | CTLFLAG_LOCKED, &gCrcHead, 0, "b85k-crc: which head debug.nvaccel_crc reads");
static int nvaccel_crc_sysctl SYSCTL_HANDLER_ARGS;
SYSCTL_PROC(_debug, OID_AUTO, nvaccel_crc, CTLTYPE_INT | CTLFLAG_RD | CTLFLAG_LOCKED, 0, 0, nvaccel_crc_sysctl, "I",
            "b85k-crc: display CRC32 of what head nvaccel_crc_head is showing (0 = unavailable)");
static bool gSysctlInstalled = false;
static void nvaccelInstallSysctl(void)
{
    if (gSysctlInstalled) return;
#ifdef NMX_ACCEL
    // only this one: the NVIDIA accelerator may be loaded too, and owns the debug.nvaccel* names
    sysctl_register_oid(&sysctl__debug_nmxaccel_headless);
    gSysctlInstalled = true;
#else
    sysctl_register_oid(&sysctl__debug_nvaccelfb);
    sysctl_register_oid(&sysctl__debug_nvaccel_headless);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_alloc);       sysctl_register_oid(&sysctl__debug_nvaccel_vm_alloc_bytes);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_dealloc);     sysctl_register_oid(&sysctl__debug_nvaccel_vm_dealloc_bytes);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_refused);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_sticky);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_parked);      sysctl_register_oid(&sysctl__debug_nvaccel_vm_unparked);
    sysctl_register_oid(&sysctl__debug_nvaccel_vm_park_released);
    sysctl_register_oid(&sysctl__debug_nvaccel_res_new);        sysctl_register_oid(&sysctl__debug_nvaccel_res_free);
    sysctl_register_oid(&sysctl__debug_nvaccel_map_commit);     sysctl_register_oid(&sysctl__debug_nvaccel_map_release);
    sysctl_register_oid(&sysctl__debug_nvaccel_map_free);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok);         sysctl_register_oid(&sysctl__debug_nvaccel_iop_fail);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_empty);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_src_res);    sysctl_register_oid(&sysctl__debug_nvaccel_iop_src_surf);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_blank);      sysctl_register_oid(&sysctl__debug_nvaccel_iop_copy_skip);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip);       sysctl_register_oid(&sysctl__debug_nvaccel_iop_flips);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_home);  sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_lock);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_novram); sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_stale);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_unwritten); sysctl_register_oid(&sysctl__debug_nvaccel_iop_copy_unwritten);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_refused);
    sysctl_register_oid(&sysctl__debug_nvaccel_crc_head);       sysctl_register_oid(&sysctl__debug_nvaccel_crc);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_hit);   sysctl_register_oid(&sysctl__debug_nvaccel_iop_flip_miss);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_async);      sysctl_register_oid(&sysctl__debug_nvaccel_iop_async_flips);
    { extern struct sysctl_oid sysctl__debug_nvaccel_svdiag; sysctl_register_oid(&sysctl__debug_nvaccel_svdiag); }
    sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu);    sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu_on);
    sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu_marks); sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu_waits);
    sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu_timeouts); sysctl_register_oid(&sysctl__debug_nvaccel_surface_gpu_wait_max_us);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_async_refused); sysctl_register_oid(&sysctl__debug_nvaccel_iop_async_drop);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_late);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok_h0);      sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok_h1);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok_h2);      sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok_h3);
    sysctl_register_oid(&sysctl__debug_nvaccel_iop_ok_h4);      sysctl_register_oid(&sysctl__debug_nvaccel_igpu);
    sysctl_register_oid(&sysctl__debug_nvaccel_heads_published); sysctl_register_oid(&sysctl__debug_nvaccel_heads_registered);
    sysctl_register_oid(&sysctl__debug_nvaccel_heads_piped);
    gSysctlInstalled = true;
    ALOG("sysctl debug.nvaccelfb installed -- `sudo sysctl -w debug.nvaccelfb=1` registers the framebuffer");
#endif
}

static inline void nvAccelZero(void *p, unsigned long n)
{ volatile unsigned char *b = (volatile unsigned char *)p; while (n--) *b++ = 0; }

static struct NVRMRmHandles gRmHandles;
static bool                 gRmHandlesValid;

static unsigned nvRmHandlesLogged;

static bool nvAccelRmHandles(struct NVRMRmHandles *out)
{
    if (gRmHandlesValid) { *out = gRmHandles; return true; }
    NVAccel *a = NVAccel::gAccel;
    if (!a) {
        if (nvRmHandlesLogged < 8) { nvRmHandlesLogged++;
            ALOG("nvAccelRmHandles: no NVAccel instance yet -- too early"); }
        return false;
    }
    if (!a->fFB) {
        if (nvRmHandlesLogged < 8) { nvRmHandlesLogged++;
            ALOG("nvAccelRmHandles: no framebuffer has published yet -- will retry"); }
        return false;
    }
    const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVRM_GPUVA_FN_HANDLES);
    if (!sym) {
        if (nvRmHandlesLogged < 8) { nvRmHandlesLogged++;
            ALOG("nvAccelRmHandles: OSSymbol allocation failed"); }
        return false;
    }
    struct NVRMRmHandles h;
    nvAccelZero(&h, sizeof h);
    h.version = NVRM_GPUVA_ABI_VERSION;
    IOReturn r = a->fFB->callPlatformFunction(sym, false, &h, nullptr, nullptr, nullptr);
    sym->release();
    if (r != kIOReturnSuccess || !h.kapiDevice) {
        if (nvRmHandlesLogged < 8) { nvRmHandlesLogged++;
            ALOG("nvAccelRmHandles: framebuffer gave no kapi device (ret 0x%x, kapiDevice %p)",
                 (unsigned)r, h.kapiDevice); }
        return false;
    }
    gRmHandles = h; gRmHandlesValid = true;
    ALOG("RM handles: kapiDevice %p (hClient/hDevice resolved inside NVRM by name) "
         "*** the accelerator can now build GPU page tables ***", h.kapiDevice);
    *out = h;
    return true;
}

static IOService *nvAccelNVRM()
{
    NVAccel *a = NVAccel::gAccel;
    if (!a) return nullptr;
    if (!a->fNVRM) {
        OSDictionary *m = IOService::serviceMatching("NVRM");
        if (m) a->fNVRM = IOService::waitForMatchingService(m, 100ULL * 1000 * 1000);
    }
    return a->fNVRM;
}

static IOReturn nvAccelGpuVaCall(const char *fn, struct NVRMGpuVaRequest *g)
{
    IOService *nvrm = nvAccelNVRM();
    if (!nvrm) return kIOReturnNotAttached;
    const OSSymbol *sym = OSSymbol::withCStringNoCopy(fn);
    if (!sym) return kIOReturnNoMemory;
    IOReturn r = nvrm->callPlatformFunction(sym, false, g, nullptr, nullptr, nullptr);
    sym->release();
    return r;
}

#define NVRM_SS_KERNEL_ABI 1
#include "../nvrm_surfshare_abi.h"
extern "C" {
void *_ZN13IOSurfaceRoot13lookupSurfaceEjP4task(void *root, unsigned int id, task_t task);
void *_ZN22IOGraphicsAccelerator223deviceCacheForIOSurfaceEP9IOSurfacej(void *accel, void *surface, unsigned int plane);
void  _ZN16IOAccelVidMemory4wireEv(void *vidmem);
void  _ZN20IOSurfaceDeviceCache10setCurrentEv(void *cache);
void *_ZNK9IOSurface19getMemoryDescriptorEv(void *surface);
unsigned int _ZNK9IOSurface12getSurfaceIDEv(void *surface);
void  _ZN22IOGraphicsAccelerator29lock_busyEv(void *accel);
void  _ZN22IOGraphicsAccelerator211unlock_busyEv(void *accel);
}
static bool nvFamLock(NVAccel *a, bool tryOnly)
{
    unsigned char *b = (unsigned char *)a;
    IOLock *lk = *(IOLock *volatile *)(b + 0x88);
    if (!lk) return false;
    if (tryOnly) { if (!IOLockTryLock(lk)) return false; }
    else {
        OSIncrementAtomic((volatile SInt32 *)(b + 0x90));
        IOLockLock(lk);
        OSDecrementAtomic((volatile SInt32 *)(b + 0x90));
    }
    _ZN22IOGraphicsAccelerator29lock_busyEv(a);
    a->acceleratorDidLock("", 0);
    return true;
}
static void nvFamUnlock(NVAccel *a)
{
    a->acceleratorWillUnlock("", 0);
    _ZN22IOGraphicsAccelerator211unlock_busyEv(a);
    IOLockUnlock(*(IOLock *volatile *)((unsigned char *)a + 0x88));
}
static unsigned gNVSVLockMiss;
enum { kNVSVPend = 512 };
static unsigned gNVSVPendId[kNVSVPend];
static NVResource *gNVSVPendResource[kNVSVPend];
static IOSimpleLock *gNVSVPendLock;
static bool nvSVPend(unsigned id, int op, NVResource *resource = nullptr)
{
    if (!id) return op == 1 ? false : false;
    if (!gNVSVPendLock) return op == 1 ? false : false;
    bool r = false; int freeSlot = -1;
    IOSimpleLockLock(gNVSVPendLock);
    for (int i = 0; i < kNVSVPend; i++) {
        if (gNVSVPendId[i] == id) {
            r = true;
            if (op == 0) { gNVSVPendId[i] = 0; gNVSVPendResource[i] = nullptr; }
            else if (op == 1) gNVSVPendResource[i] = resource;
            break;
        }
        if (!gNVSVPendId[i] && freeSlot < 0) freeSlot = i;
    }
    if (op == 1 && !r) { if (freeSlot >= 0) { gNVSVPendId[freeSlot] = id; gNVSVPendResource[freeSlot] = resource; r = true; } }
    else if (op == 1) r = true;
    IOSimpleLockUnlock(gNVSVPendLock);
    return r;
}
// A pending mark belongs to the resource that last dirtied the surface. Never
// dereference a device cache during destruction, or clear a newer owner's mark.
static void nvSVForgetResource(NVResource *resource)
{
    if (!resource || !gNVSVPendLock) return;
    IOSimpleLockLock(gNVSVPendLock);
    for (unsigned i = 0; i < kNVSVPend; i++) if (gNVSVPendResource[i] == resource) {
        gNVSVPendId[i] = 0;
        gNVSVPendResource[i] = nullptr;
    }
    IOSimpleLockUnlock(gNVSVPendLock);
}
static void    *gNVSVRoot;
static unsigned gNVSVOk, gNVSVRefused;
static inline void nvSVZero(void *p, unsigned long n) { volatile unsigned char *b = (volatile unsigned char *)p; while (n--) *b++ = 0; }

static unsigned gNVSVDirty, gNVSVDirtyRefused, gNVSVPageoff, gNVSVPageoffFail;
// sysctl debug.nvaccel_svdiag=N logs the next N surface dirty marks and page-offs (surface, result, path); 0 = off
static int gSVDiag;
SYSCTL_INT(_debug, OID_AUTO, nvaccel_svdiag, CTLFLAG_RW | CTLFLAG_LOCKED, &gSVDiag, 0, "log the next N surface dirty marks and page-offs");
static bool nvSVDiag() { return gSVDiag > 0 && OSDecrementAtomic(&gSVDiag) > 0; }
static unsigned nvAccelSurfDirty(void *cache, NVResource *res, unsigned plane)
{
    volatile unsigned char *r = (volatile unsigned char *)res;
    unsigned char type = r[0x14];
    unsigned int fl = *(volatile unsigned int *)(r + 0xc);
    volatile unsigned short *valid = *(unsigned short *volatile *)(r + 0x30);
    if (type != 0xc0 || (fl & 0x8000) || !valid || plane > 15) return NVRM_SS_NO_CACHE;
    if (!gNVSVPendLock) gNVSVPendLock = IOSimpleLockAlloc();
    if (!nvSVPend(_ZNK9IOSurface12getSurfaceIDEv(*(void *volatile *)((unsigned char *)cache + 0x10)), 1, res)) return NVRM_SS_NO_CACHE;
    _ZN20IOSurfaceDeviceCache10setCurrentEv(cache);
    valid[6] |= (unsigned short)(1u << plane);
    valid[0] &= (unsigned short)~(1u << plane);
    { NVVidMemory *vm = nvres_vidmem(res); res->fPluginWroteCookie = vm ? vm->fAllocationCookie : 0; }
    return 0;
}
// The caller holds the accelerator family lock. The plugin can still write a
// retained allocation after the family unwires it; make that same backing active
// before marking the GPU copy current. Never allocate fresh memory for this case.
static bool nvAccelRestoreParkedForDirty(NVResource *res, NVVidMemory *vm, unsigned plane)
{
    if (!res || !vm || vm->fRmHandle || !vm->fParkHandle) return true;
    const volatile unsigned char *r = (const volatile unsigned char *)res;
    const unsigned flags = *(const volatile unsigned *)(r + 0xc);
    const volatile unsigned short *valid = *(unsigned short *volatile const *)(r + 0x30);
    const unsigned long long want = *(const volatile unsigned long long *)((const unsigned char *)vm + 0x40);
    if (vm->fScanout || r[0x14] != 0xc0 || (flags & 0x8000) || !valid || plane > 15 ||
        !want || vm->fParkBytes < want || !vm->fParkPhys || !vm->fParkKva)
        return true;  // Preserve the original refusal and CPU-copy fallback.
    void *const handle = vm->fParkHandle;
    void *const mapping = vm->fParkKva;
    const unsigned long long bytes = vm->fParkBytes, phys = vm->fParkPhys, cookie = vm->fParkCookie;
    // IOAccelVidMemory::wire calls allocPhysical and updates family accounting;
    // it does not page CPU pixels in. The size check above selects allocPhysical's
    // retained-allocation branch, which moves the existing backing without copying.
    _ZN16IOAccelVidMemory4wireEv(vm);
    return vm->fRmHandle == handle && vm->fKva == mapping && vm->fPhys == phys &&
        vm->fBytes == bytes && vm->fAllocationCookie == cookie && !vm->fParkHandle;
}
// The marks arrive through NVK's surface escape (NVRM_ESC_SURF_DIRTY with plane NVRM_SS_PLANE_GPU_MARK | pending): the
// WindowServer sandbox denies sysctl writes. Only WindowServer's own marks are taken, so no other process can hold a swap.
static unsigned nvSurfGpuEscape(const struct NVRMSurfShareEsc *p)
{
    if (!gSurfGpuOn) return NVRM_SS_OFF;
    char nm[32] = { 0 };
    proc_selfname(nm, sizeof nm);
    if (strcmp(nm, "WindowServer")) return NVRM_SS_OFF;
    if (p->surfaceID) nvSurfMark(p->surfaceID, (p->plane & 1u) != 0);
    return 0;
}
static IOReturn nvAccelSurfVram(struct NVRMSurfShareEsc *p, task_t task, unsigned cmd)
{
    if (p->version == NVRM_SURFSHARE_VERSION && cmd == NVRM_ESC_SURF_DIRTY && (p->plane & NVRM_SS_PLANE_GPU_MARK)) {
        p->hSrcClient = p->hSrcMemory = 0;
        p->status = nvSurfGpuEscape(p);
        return kIOReturnSuccess;
    }
    p->hSrcClient = p->hSrcMemory = 0;
    if (p->version != NVRM_SURFSHARE_VERSION) { p->status = NVRM_SS_BAD_VERSION; return kIOReturnSuccess; }
    // The plugin re-marks a surface after its last GPU write on the thread that releases the pending marks. A page-off
    // waiting for one of those marks holds the accelerator lock, so that re-mark must not wait for the lock: it is
    // dropped instead, and the mark taken at commit stands.
    const bool tryOnly = cmd == NVRM_ESC_SURF_DIRTY && (p->plane & NVRM_SS_PLANE_TRYLOCK);
    p->plane &= ~(NVRM_SS_PLANE_TRYLOCK | NVRM_SS_PLANE_GPU_MARK);
    // plane is a full uint32 from the caller and reaches IOGraphicsAccelerator2::deviceCacheForIOSurface
    // below. The plane > 15 refusals further down run after that call, and only on the dirty path.
    if (p->plane > 15) { p->status = NVRM_SS_NO_CACHE; return kIOReturnSuccess; }
    NVAccel *a = NVAccel::gAccel;
    struct NVRMRmHandles h; nvSVZero(&h, sizeof h);
    if (!task || !a || !nvAccelRmHandles(&h)) { p->status = NVRM_SS_OFF; return kIOReturnSuccess; }
    if (!gNVSVRoot) {
        OSDictionary *m = IOService::serviceMatching("IOSurfaceRoot");
        if (m) { gNVSVRoot = IOService::waitForMatchingService(m, 5ULL * 1000 * 1000 * 1000); m->release(); }
        if (!gNVSVRoot) { p->status = NVRM_SS_OFF; return kIOReturnSuccess; }
    }
    void *surf = _ZN13IOSurfaceRoot13lookupSurfaceEjP4task(gNVSVRoot, p->surfaceID, task);
    if (!surf) { p->status = NVRM_SS_NO_SURFACE; gNVSVRefused++; return kIOReturnSuccess; }
    if (!nvFamLock(a, tryOnly)) { ((OSObject *)surf)->release(); p->status = tryOnly ? NVRM_SS_BUSY : NVRM_SS_OFF; return kIOReturnSuccess; }
    void *cache = _ZN22IOGraphicsAccelerator223deviceCacheForIOSurfaceEP9IOSurfacej((void *)a, surf, p->plane);
    OSObject *ctx = cache ? *(OSObject *volatile *)((unsigned char *)cache + 0x20) : nullptr;
    NVResource *res = OSDynamicCast(NVResource, ctx);
    NVVidMemory *vm = res ? nvres_vidmem(res) : nullptr;
    if (cmd == NVRM_ESC_SURF_DIRTY) {
        const bool sameBacking = nvAccelRestoreParkedForDirty(res, vm, p->plane);
        p->status = (sameBacking && cache && res && vm && vm->fRmHandle)
            ? nvAccelSurfDirty(cache, res, p->plane) : NVRM_SS_NO_CACHE;
        if (p->status == 0) gNVSVDirty++; else gNVSVDirtyRefused++;
        if (nvSVDiag()) ALOG("SVDIAG dirty surface %u plane %u -> %#x (cache %d res %d vm %d)", p->surfaceID, p->plane, p->status,
                             cache ? 1 : 0, res ? 1 : 0, (vm && vm->fRmHandle) ? 1 : 0);
        if (gNVSVDirty + gNVSVDirtyRefused <= 6 || ((gNVSVDirty + gNVSVDirtyRefused) & 4095) == 0)
            ALOG("surfvram dirty: surface %u plane %u type 0x%x -> %#x (dirty %u, refused %u)", p->surfaceID, p->plane,
                 res ? (unsigned)((volatile unsigned char *)res)[0x14] : 0u, p->status, gNVSVDirty, gNVSVDirtyRefused);
        if (cache) ((OSObject *)cache)->release();
        nvFamUnlock(a);
        ((OSObject *)surf)->release();
        return kIOReturnSuccess;
    }
    if (vm && !vm->fRmHandle && !vm->fScanout) _ZN16IOAccelVidMemory4wireEv(vm);
    if (!cache || !res) p->status = NVRM_SS_NO_CACHE;
    else if (!vm || !vm->fRmHandle || !vm->fBytes) p->status = NVRM_SS_NO_VRAM;
    else if (p->size > vm->fBytes) p->status = NVRM_SS_TOO_SMALL;
    else {
        struct NVRMSurfShareRm g; nvSVZero(&g, sizeof g);
        g.version = NVRM_SS_RM_VERSION; g.op = NVRM_SS_RM_SHARE_KAPI;
        g.kapiDevice = h.kapiDevice; g.kapiMemory = vm->fRmHandle; g.target = p->hClient; g.status = NVRM_SS_OFF;
        IOService *nvrm = nvAccelNVRM();
        const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVRM_SS_FN_RM);
        if (nvrm && sym && nvrm->callPlatformFunction(sym, false, &g, nullptr, nullptr, nullptr) == kIOReturnSuccess) {}
        if (sym) sym->release();
        p->status = g.status; p->hSrcClient = g.hClient; p->hSrcMemory = g.hObject; p->size = vm->fBytes;
    }
    if (p->status == 0) gNVSVOk++; else gNVSVRefused++;
    if (gNVSVOk + gNVSVRefused <= 8 || ((gNVSVOk + gNVSVRefused) & 1023) == 0)
        ALOG("surfvram: surface %u plane %u -> cache %p res %p vidmem %p (%llu B) -> client %#x mem %#x status %#x (ok %u, refused %u)",
             p->surfaceID, p->plane, cache, res, vm, vm ? vm->fBytes : 0ull, p->hSrcClient, p->hSrcMemory, p->status, gNVSVOk, gNVSVRefused);
    if (cache) ((OSObject *)cache)->release();
    nvFamUnlock(a);
    ((OSObject *)surf)->release();
    return kIOReturnSuccess;
}

#include <mach/mach_time.h>
static IOLock *volatile gNVSVStageLock;
static IOBufferMemoryDescriptor *gNVSVStageBuf;
static unsigned gNVSVStaged, gNVSVStageNoBuf, gNVSVStageBackoff;
static IOLock *nvSVStageLock(void)
{
    if (!gNVSVStageLock) {
        IOLock *l = IOLockAlloc();
        if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gNVSVStageLock)) IOLockFree(l);
    }
    return gNVSVStageLock;
}
static IOBufferMemoryDescriptor *nvSVStage(unsigned long long len)
{
    if (gNVSVStageBuf && gNVSVStageBuf->getLength() >= len) return gNVSVStageBuf;
    if (len > (64ull << 20) || gNVSVStageBackoff) { if (gNVSVStageBackoff) gNVSVStageBackoff--; return nullptr; }
    const unsigned long long want = (len + 0xFFFFFull) & ~0xFFFFFull;
    IOBufferMemoryDescriptor *b = IOBufferMemoryDescriptor::withOptions(kIODirectionInOut | kIOMemoryPhysicallyContiguous,
                                                                      (vm_size_t)want, PAGE_SIZE);
    if (b && b->prepare() != kIOReturnSuccess) { b->release(); b = nullptr; }
    if (!b) {
        gNVSVStageNoBuf++; gNVSVStageBackoff = 1024;
        ALOG("surfvram pageoff: no %llu B physically contiguous staging buffer (miss %u) -- per-page copies for the next 1024", want, gNVSVStageNoBuf);
        return nullptr;
    }
    if (gNVSVStageBuf) { gNVSVStageBuf->complete(); gNVSVStageBuf->release(); }
    gNVSVStageBuf = b;
    ALOG("surfvram pageoff: staging buffer %llu B, physically contiguous", want);
    return b;
}

void NVResource::pageoff(IOAccelEvent *ev, bool a1, bool *out, unsigned long long a3)
{
    volatile unsigned char *r = (volatile unsigned char *)this;
    void *cache = *(void *volatile *)(r + 0xe0);
    NVVidMemory *vm = nvres_vidmem(this);
    struct NVRMRmHandles h; nvSVZero(&h, sizeof h);
    void *surf = (r[0x14] == 0xc0 && cache) ? *(void *volatile *)((unsigned char *)cache + 0x10) : nullptr;
    if (nvSVDiag()) {
        volatile unsigned short *vd = *(unsigned short *volatile *)(r + 0x30);
        ALOG("SVDIAG pageoff surface %u type 0x%x vm %d rm %d scanout %d valid0 %#x valid6 %#x", surf ? _ZNK9IOSurface12getSurfaceIDEv(surf) : 0u,
             (unsigned)r[0x14], vm ? 1 : 0, (vm && vm->fRmHandle) ? 1 : 0, vm ? (int)vm->fScanout : -1, vd ? vd[0] : 0xffff, vd ? vd[6] : 0xffff);
    }
    if (!surf || !vm || !vm->fRmHandle || vm->fScanout || !vm->fBytes || !nvAccelRmHandles(&h)) {
        IOAccelResource2::pageoff(ev, a1, out, a3);
        return;
    }
    // a CPU lock that arrives while WindowServer's GPU is still writing this surface waits for that write first, as a
    // normal IOSurfaceLock does on Apple's drivers (the commit marked it pending; see nvmtl_ws_mark_pending)
    nvSurfWaitReady(_ZNK9IOSurface12getSurfaceIDEv(surf));
    IOMemoryDescriptor *md = (IOMemoryDescriptor *)_ZNK9IOSurface19getMemoryDescriptorEv(surf);
    if (!md || md->prepare(kIODirectionNone) != kIOReturnSuccess) {
        gNVSVPageoffFail++;
        ALOG("surfvram pageoff: res %p surface pages could not be wired (md %p) -- family pageoff instead", this, md);
        IOAccelResource2::pageoff(ev, a1, out, a3);
        return;
    }
    bool mayUnwire = true;
    unsigned long long len = md->getLength();
    if (len > vm->fBytes) len = vm->fBytes;
    unsigned long long pages = (len + 4095) >> 12;
    bool ok = false; unsigned st = NVRM_SS_OFF; unsigned long long usec = 0, cpuUs = 0;
    bool staged = false;
    if (IOLock *lk = nvSVStageLock()) {
        IOLockLock(lk);
        IOBufferMemoryDescriptor *sb = nvSVStage(len);
        IOByteCount sl = 0;
        addr64_t sp = sb ? sb->getPhysicalSegment(0, &sl, kIOMemoryMapperNone) : 0;
        unsigned long long *spa = (sp && sl >= len && pages) ? (unsigned long long *)IOMalloc(pages * sizeof *spa) : nullptr;
        if (spa) {
            for (unsigned long long k = 0; k < pages; k++) spa[k] = sp + k * 4096;
            struct NVRMSurfPageoff q; nvSVZero(&q, sizeof q);
            q.version = NVRM_SS_RM_VERSION; q.status = NVRM_SS_OFF; q.kapiDevice = h.kapiDevice; q.kapiMemory = vm->fRmHandle;
            q.pages = spa; q.pageCount = pages; q.length = len;
            IOService *nvrm = nvAccelNVRM();
            const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVRM_SS_FN_PAGEOFF);
            if (nvrm && sym) nvrm->callPlatformFunction(sym, false, &q, nullptr, nullptr, nullptr);
            if (sym) sym->release();
            st = q.status; usec = q.usec;
            if (st == 0) {
                const uint64_t t0 = mach_absolute_time();
                const IOByteCount w = md->writeBytes(0, sb->getBytesNoCopy(), (IOByteCount)len);
                cpuUs = (mach_absolute_time() - t0) / 1000;
                ok = (w == (IOByteCount)len);
                if (!ok) st = NVRM_SS_TOO_SMALL;
                else gNVSVStaged++;
            } else if (st == 0x65u) {
                ALOG("surfvram pageoff: CE copy TIMED OUT into the staging buffer -- abandoning it (%llu B)", (unsigned long long)sb->getLength());
                gNVSVStageBuf = nullptr;
            }
            staged = true;
            IOFree(spa, pages * sizeof *spa);
        }
        IOLockUnlock(lk);
    }
    unsigned long long *pa = (!staged && pages) ? (unsigned long long *)IOMalloc(pages * sizeof *pa) : nullptr;
    if (pa) {
        unsigned long long i = 0;
        for (IOByteCount off = 0; i < pages; ) {
            IOByteCount seglen = 0;
            addr64_t phys = md->getPhysicalSegment(off, &seglen, kIOMemoryMapperNone);
            if (!phys || !seglen) break;
            for (IOByteCount k = 0; k < seglen && i < pages; k += 4096, off += 4096) pa[i++] = phys + k;
        }
        if (i == pages) {
            struct NVRMSurfPageoff q; nvSVZero(&q, sizeof q);
            q.version = NVRM_SS_RM_VERSION; q.status = NVRM_SS_OFF; q.kapiDevice = h.kapiDevice; q.kapiMemory = vm->fRmHandle;
            q.pages = pa; q.pageCount = pages; q.length = len;
            IOService *nvrm = nvAccelNVRM();
            const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVRM_SS_FN_PAGEOFF);
            if (nvrm && sym) nvrm->callPlatformFunction(sym, false, &q, nullptr, nullptr, nullptr);
            if (sym) sym->release();
            st = q.status; usec = q.usec; ok = st == 0;
            if (st == 0x65u) mayUnwire = false;
        }
        IOFree(pa, pages * sizeof *pa);
    }
    if (mayUnwire) md->complete(kIODirectionNone);
    else ALOG("surfvram pageoff: res %p CE copy TIMED OUT -- surface pages stay wired (%llu B)", this, len);
    if (ok) {
        nvSVPend(_ZNK9IOSurface12getSurfaceIDEv(surf), 0);
        volatile unsigned short *valid = *(unsigned short *volatile *)(r + 0x30);
        if (valid) valid[0] |= 1;
        gNVSVPageoff++;
    } else gNVSVPageoffFail++;
    if (out) *out = ok;
    if (gNVSVPageoff + gNVSVPageoffFail <= 8 || ((gNVSVPageoff + gNVSVPageoffFail) & 1023) == 0)
        ALOG("surfvram pageoff: res %p %llu B on the copy engine -> %#x in %llu us%s + cpu %llu us (ok %u, failed %u, staged %u)",
             this, len, st, usec, staged ? " (staged)" : " (per page)", cpuUs, gNVSVPageoff, gNVSVPageoffFail, gNVSVStaged);
}

static IOSurface *gIopFlipFront[kNvMaxFB], *gIopFlipPrev[kNvMaxFB];
static bool gIopFlipped[kNvMaxFB];
static unsigned gIopFlipLogs;
static bool nvAccelIopFlipNo(int why) { OSAddAtomic64(1, &gCntIopFlipNo[why & 7]); return false; }
struct NVIopFlipCache { IOSurface *s; NVResource *res; NVVidMemory *vm; void *kmem; unsigned long long bytes; unsigned long long use; };
static NVIopFlipCache gIopFlipCache[8];
static IOSimpleLock *gIopFlipCacheLock;
static unsigned long long gIopFlipCacheTick;
static bool nvAccelIopCacheGet(IOSurface *s, NVResource **res, NVVidMemory **vm, void **kmem, unsigned long long *bytes)
{
    if (!gIopFlipCacheLock) return false;
    bool hit = false;
    IOSimpleLockLock(gIopFlipCacheLock);
    for (unsigned i = 0; i < 8; i++) if (gIopFlipCache[i].s == s) {
        NVVidMemory *v = gIopFlipCache[i].vm;
        if (v && v->fRmHandle == gIopFlipCache[i].kmem && v->fBytes == gIopFlipCache[i].bytes) {
            *res = gIopFlipCache[i].res; *vm = v; *kmem = gIopFlipCache[i].kmem; *bytes = gIopFlipCache[i].bytes;
            gIopFlipCache[i].use = ++gIopFlipCacheTick; hit = true;
        }
        break;
    }
    IOSimpleLockUnlock(gIopFlipCacheLock);
    return hit;
}
static void nvAccelIopCachePut(IOSurface *s, NVResource *res, NVVidMemory *vm, void *kmem, unsigned long long bytes)
{
    if (!gIopFlipCacheLock) { IOSimpleLock *l = IOSimpleLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gIopFlipCacheLock)) IOSimpleLockFree(l); }
    if (!gIopFlipCacheLock) return;
    IOSurface *drop = nullptr;
    ((OSObject *)s)->retain();
    IOSimpleLockLock(gIopFlipCacheLock);
    unsigned k = 0;
    for (unsigned i = 0; i < 8; i++) {
        if (gIopFlipCache[i].s == s) { k = i; break; }
        if (!gIopFlipCache[i].s) { k = i; break; }
        if (gIopFlipCache[i].use < gIopFlipCache[k].use) k = i;
    }
    drop = gIopFlipCache[k].s;
    gIopFlipCache[k].s = s; gIopFlipCache[k].res = res; gIopFlipCache[k].vm = vm; gIopFlipCache[k].kmem = kmem;
    gIopFlipCache[k].bytes = bytes; gIopFlipCache[k].use = ++gIopFlipCacheTick;
    IOSimpleLockUnlock(gIopFlipCacheLock);
    if (drop) ((OSObject *)drop)->release();
}
extern "C" {
void _ZN18IOAccelDisplayPipe26signalTransactionInterruptEPv(void *pipe, void *arg);
struct thread_call *thread_call_allocate(void (*func)(void *, void *), void *param0);
int thread_call_enter(struct thread_call *call);
}
// next/nextF: one frame WindowServer presented while the previous flip was still in flight past the bound, taken by
// the worker as soon as that flip lands (guarded by gIopAsLock, like busy's clearing).
struct NVIopAsync { void *pipe; IOService *fb; IOSurface *s; struct NVRMFlipRequest f; volatile SInt32 busy; struct thread_call *call;
                    IOSurface *next; struct NVRMFlipRequest nextF; };
static NVIopAsync gIopAs[kNvMaxFB];
static SInt32 gIopAsyncRefuseRun;
// WindowServer's next present waits here while the previous asynchronous flip is still on its way to the panel. It
// waited in IOSleep(1) steps, each overshooting by a millisecond or more - a large slice of a 6.9 ms frame at 144 Hz,
// where the desktop then ran near half the panel rate. The flip's worker wakes the waiter when it finishes; the busy
// flag is still re-read at least every 250 us. The bound is one refresh period and a quarter, published by the
// framebuffer from its mode (NVRMFlipWaitUs): a flip in flight lands at the next vblank, so a wait past that only
// holds WindowServer's thread; the frame is dropped instead and the next one follows the flip.
static IOLock *gIopAsLock;
static uint64_t nvAccelFlipWaitUs(IOService *fb)
{
    // A retained copy: a mode switch republishes the property, which releases the number getProperty would return.
    OSObject *o = fb ? fb->copyProperty("NVRMFlipWaitUs") : nullptr;
    OSNumber *n = OSDynamicCast(OSNumber, o);
    const uint64_t us = n ? n->unsigned64BitValue() : 0;
    if (o) o->release();
    return us ? us : 50000;   // before the framebuffer has a mode, the old 50 ms
}
// What the framebuffer's answer to a flip means for the frame: on the panel, kept (busy: the flip before it is still
// landing, the next present follows it), or refused (copied, and three in a row turn the asynchronous swap off).
enum { kNvFlipShown = 0, kNvFlipKept = 1, kNvFlipRefused = 2 };
static int nvAccelFlipOutcome(IOReturn rr)
{
    return rr == kIOReturnSuccess ? kNvFlipShown : rr == kIOReturnBusy ? kNvFlipKept : kNvFlipRefused;
}
static bool nvAccelIopAsyncBusy(const void *pipe)
{
    for (unsigned h = 0; h < kNvMaxFB; h++)
        if (gIopAs[h].pipe == pipe && __atomic_load_n(&gIopAs[h].busy, __ATOMIC_ACQUIRE)) return true;
    return false;
}
static void nvAccelIopAsyncRun(void *p0, void *)
{
    NVIopAsync *e = (NVIopAsync *)p0;
    const unsigned head = (unsigned)(e - gIopAs);
    IOSurface *s = e->s; e->s = nullptr;
  again:
    nvSurfWaitReady(s->getSurfaceID());   // WindowServer did not wait for this composite; the flip does, off its thread
    const OSSymbol *sym = OSSymbol::withCStringNoCopy("nvFlipToSurfacePure");
    IOReturn rr = sym ? e->fb->callPlatformFunction(sym, false, &e->f, nullptr, nullptr, nullptr) : kIOReturnNoMemory;
    if (sym) sym->release();
    const int outcome = nvAccelFlipOutcome(rr);
    if (outcome == kNvFlipShown) {
        gIopAsyncRefuseRun = 0;
        if (gIopFlipPrev[head]) ((OSObject *)gIopFlipPrev[head])->release();
        gIopFlipPrev[head] = gIopFlipFront[head]; gIopFlipFront[head] = s; gIopFlipped[head] = true;
        const SInt64 n = OSAddAtomic64(1, &gCntIopFlip); OSAddAtomic64(1, &gCntIopAsync);
        if (n < 4 || n == 600 || (n & 0xffff) == 0)
            ALOG("iop: ASYNC swap #%lld head %u: surface 0x%x %ux%u pitch %u scanned out of its own VRAM (flipResult %d)",
                 (long long)n + 1, head, s->getSurfaceID(), e->f.width, e->f.height, e->f.pitch, e->f.flipResult);
    } else if (outcome == kNvFlipKept) {
        OSAddAtomic64(1, &gCntIopLate);
        ((OSObject *)s)->release();
    } else {
        OSAddAtomic64(1, &gCntIopAsyncRefused); OSAddAtomic64(1, &gCntIopFlipNo[7]);
        if (gIopFlipLogs < 8) { gIopFlipLogs++; ALOG("iop: async flip REFUSED (0x%x, flipResult %d) head %u -- that frame is not on the panel",
                                                     (unsigned)rr, e->f.flipResult, head); }
        if (++gIopAsyncRefuseRun >= 3 && gIopAsync) {
            gIopAsync = 0;
            ALOG("iop: b89k 3 async refusals in a row -- debug.nvaccel_iop_async=0 (synchronous flip, which copies on refusal)");
        }
        ((OSObject *)s)->release();
    }
    // A frame queued behind this flip follows it at once; otherwise the worker is free and the waiter is woken.
    IOSurface *queued = nullptr;
    if (gIopAsLock) {
        IOLockLock(gIopAsLock);
        queued = e->next; e->next = nullptr;
        if (queued) { for (unsigned i = 0; i < sizeof e->f; i++) ((volatile unsigned char *)&e->f)[i] = ((const volatile unsigned char *)&e->nextF)[i]; }
        else { __atomic_store_n(&e->busy, 0, __ATOMIC_RELEASE); IOLockWakeup(gIopAsLock, (event_t)&e->busy, false); }
        IOLockUnlock(gIopAsLock);
    } else {
        __atomic_store_n(&e->busy, 0, __ATOMIC_RELEASE);
    }
    _ZN18IOAccelDisplayPipe26signalTransactionInterruptEPv(e->pipe, nullptr);
    if (queued) { s = queued; goto again; }
}
static bool nvAccelIopAsyncStart(void *pipe, IOService *fb, unsigned head, IOSurface *s, const struct NVRMFlipRequest *f)
{
    NVIopAsync *e = &gIopAs[head];
    if (!gIopAsLock) { IOLock *l = IOLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gIopAsLock)) IOLockFree(l); }
    if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE)) {
        uint64_t end = 0; clock_interval_to_deadline((uint32_t)nvAccelFlipWaitUs(fb), kMicrosecondScale, &end);
        while (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE) && mach_absolute_time() < end) {
            if (!gIopAsLock) { IOSleep(1); continue; }
            uint64_t next = 0; clock_interval_to_deadline(250, kMicrosecondScale, &next);
            if (next > end) next = end;
            IOLockLock(gIopAsLock);
            if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE))
                IOLockSleepDeadline(gIopAsLock, (event_t)&e->busy, next, THREAD_UNINT);
            IOLockUnlock(gIopAsLock);
        }
    }
    if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE) && gIopAsLock) {
        // Still in flight past the bound: this frame is queued for the worker instead of holding WindowServer any
        // longer, replacing an older queued frame (that one is the dropped frame the counter names).
        IOLockLock(gIopAsLock);
        if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE) && e->pipe == pipe && e->fb == fb) {
            IOSurface *old = e->next;
            ((OSObject *)s)->retain();
            e->next = s;
            for (unsigned i = 0; i < sizeof *f; i++) ((volatile unsigned char *)&e->nextF)[i] = ((const unsigned char *)f)[i];
            IOLockUnlock(gIopAsLock);
            if (old) { OSAddAtomic64(1, &gCntIopAsyncDrop); ((OSObject *)old)->release(); }
            return true;
        }
        IOLockUnlock(gIopAsLock);
    }
    if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE)) {
        OSAddAtomic64(1, &gCntIopAsyncDrop);
        return true;
    }
    if (!e->call) e->call = thread_call_allocate(nvAccelIopAsyncRun, e);
    if (!e->call) return false;
    ((OSObject *)s)->retain();
    e->pipe = pipe; e->fb = fb; e->s = s;
    for (unsigned i = 0; i < sizeof *f; i++) ((volatile unsigned char *)&e->f)[i] = ((const unsigned char *)f)[i];
    __atomic_store_n(&e->busy, 1, __ATOMIC_RELEASE);
    thread_call_enter(e->call);
    return true;
}
static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe)
{
    if (!gIopFlip || !fb || !s || head >= kNvMaxFB) return false;
    // zero-copy scan-out is an NVIDIA display-engine flip; another vendor's panel (NMIntelFB) always takes the copy
    if (!fb->metaCast("NVRMNVDAFramebuffer")) return false;
    NVAccel *a = NVAccel::gAccel;
    if (!a) return nvAccelIopFlipNo(1);
    if (s->getPixelFormat() != 0x42475241u) return nvAccelIopFlipNo(2);
    NVResource *res = nullptr; NVVidMemory *vm = nullptr; void *kmem = nullptr; unsigned long long bytes = 0;
    if (nvAccelIopCacheGet(s, &res, &vm, &kmem, &bytes)) {
        OSAddAtomic64(1, &gCntIopFlipHit);
    } else {
        if (!nvFamLock(a, true)) return nvAccelIopFlipNo(3);
        void *cache = _ZN22IOGraphicsAccelerator223deviceCacheForIOSurfaceEP9IOSurfacej((void *)a, (void *)s, 0);
        OSObject *ctx = cache ? *(OSObject *volatile *)((unsigned char *)cache + 0x20) : nullptr;
        res = OSDynamicCast(NVResource, ctx);
        vm = res ? nvres_vidmem(res) : nullptr;
        kmem = (vm && vm->fRmHandle && vm->fBytes && !vm->fScanout) ? vm->fRmHandle : nullptr;
        bytes = vm ? vm->fBytes : 0;
        if (cache) ((OSObject *)cache)->release();
        nvFamUnlock(a);
        OSAddAtomic64(1, &gCntIopFlipMiss);
        if (kmem) nvAccelIopCachePut(s, res, vm, kmem, bytes);
    }
    volatile unsigned short *valid = res ? *(unsigned short *volatile *)((volatile unsigned char *)res + 0x30) : nullptr;
    const bool vramNewest = valid && (valid[6] & 1u);
    if (!kmem) return nvAccelIopFlipNo(4);
    if (!vramNewest) return nvAccelIopFlipNo(5);
    // Checked against the allocation this flip submits (vm, resolved above and cached), not re-resolved from the
    // resource: this runs outside the family lock, and the record must describe the VRAM that is about to be scanned.
    if (!nvAccelIopVramFlippable(vramNewest, nvAccelPluginWrote(res->fPluginWroteCookie, vm->fAllocationCookie))) {
        const SInt64 n = OSAddAtomic64(1, &gCntIopUnwritten);
        if (n < 4 || (n & 0xfff) == 0)
            ALOG("iop: surface 0x%x: VRAM marked newest but the plugin never wrote it -- copying its system memory instead of flipping (%lld so far)",
                 s->getSurfaceID(), (long long)n + 1);
        return false;
    }
    const unsigned long long W = s->getWidth(), H = s->getHeight(), P = s->getBytesPerRow();
    if (!W || !H || P < 4 * W || H * P > bytes) return nvAccelIopFlipNo(6);
    struct NVRMFlipRequest f; nvSVZero(&f, sizeof f);
    f.version = NVRM_FLIP_ABI_VERSION; f.kapiMemory = kmem; f.allocationCookie = vm->fAllocationCookie; f.width = (unsigned)W; f.height = (unsigned)H; f.pitch = (unsigned)P;
    if (gIopAsync && pipe) return nvAccelIopAsyncStart(pipe, fb, head, s, &f);
    nvSurfWaitReady(s->getSurfaceID());
    const OSSymbol *sym = OSSymbol::withCStringNoCopy("nvFlipToSurfacePure");
    IOReturn rr = sym ? fb->callPlatformFunction(sym, false, &f, nullptr, nullptr, nullptr) : kIOReturnNoMemory;
    if (sym) sym->release();
    if (nvAccelFlipOutcome(rr) == kNvFlipKept) { OSAddAtomic64(1, &gCntIopLate); return true; }   // the frame on the panel stays
    if (rr != kIOReturnSuccess) {
        if (gIopFlipLogs < 8) { gIopFlipLogs++; ALOG("iop: flip REFUSED (0x%x, flipResult %d) head %u %llux%llu pitch %llu -- copying",
                                                     (unsigned)rr, f.flipResult, head, W, H, P); }
        return nvAccelIopFlipNo(7);
    }
    ((OSObject *)s)->retain();
    if (gIopFlipPrev[head]) ((OSObject *)gIopFlipPrev[head])->release();
    gIopFlipPrev[head] = gIopFlipFront[head]; gIopFlipFront[head] = s; gIopFlipped[head] = true;
    const SInt64 n = OSAddAtomic64(1, &gCntIopFlip);
    if (n < 4 || n == 600 || (n & 0xffff) == 0)
        ALOG("iop: zero-copy swap #%lld head %u: surface 0x%x %llux%llu pitch %llu scanned out of its own VRAM (flipResult %d)",
             (long long)n + 1, head, s->getSurfaceID(), W, H, P, f.flipResult);
    return true;
}
static int nvaccel_crc_sysctl SYSCTL_HANDLER_ARGS
{
    unsigned v = 0;
    NVAccel *a = NVAccel::gAccel;
    IOService *fb = a ? a->fFB : nullptr;
    IOService *nub = fb ? fb->getProvider() : nullptr;
    OSNumber *n = nub ? OSDynamicCast(OSNumber, nub->getProperty("nvkms-kapi")) : nullptr;
    struct NvKmsKapiFunctionsTable *k = n ? (struct NvKmsKapiFunctionsTable *)(uintptr_t)n->unsigned64BitValue() : nullptr;
    struct NVRMRmHandles h; nvSVZero(&h, sizeof h);
    if (k && k->getCRC32 && gCrcHead >= 0 && gCrcHead < 8 && nvAccelRmHandles(&h) && h.kapiDevice) {
        struct NvKmsKapiCrcs c; nvSVZero(&c, sizeof c);
        if (k->getCRC32((struct NvKmsKapiDevice *)h.kapiDevice, (NvU32)gCrcHead, &c))
            v = c.compositorCrc32.supported ? c.compositorCrc32.value : (c.outputCrc32.supported ? c.outputCrc32.value : 0);
    }
    int out = (int)v;
    return sysctl_handle_int(oidp, &out, 0, req);
}
static void nvAccelIopHome(IOService *fb, unsigned head)
{
    if (!fb || head >= kNvMaxFB || !gIopFlipped[head] || !fb->metaCast("NVRMNVDAFramebuffer")) return;
    struct NVRMFlipRequest f; nvSVZero(&f, sizeof f);
    f.version = NVRM_FLIP_ABI_VERSION; f.flags = 1u;
    const OSSymbol *sym = OSSymbol::withCStringNoCopy("nvFlipToSurfacePure");
    IOReturn rr = sym ? fb->callPlatformFunction(sym, false, &f, nullptr, nullptr, nullptr) : kIOReturnNoMemory;
    if (sym) sym->release();
    OSAddAtomic64(1, &gCntIopFlipHome);
    // Busy: the flip before HOME is still landing and HOME was not submitted, so the panel is not home yet and the
    // next copied frame tries again. Clearing the flag here left every later copy offscreen behind that flip.
    if (rr == kIOReturnBusy) return;
    if (rr != kIOReturnSuccess && gIopFlipLogs < 8) { gIopFlipLogs++; ALOG("iop: HOME flip refused (0x%x) head %u", (unsigned)rr, head); }
    gIopFlipped[head] = false;
}
static void nvAccelSurfScanoutSync(IOSurface *s)
{
    if (!s || !NVAccel::gAccel || !nvSVPend(_ZNK9IOSurface12getSurfaceIDEv((void *)s), 2)) return;
    NVAccel *a = NVAccel::gAccel;
    if (!nvFamLock(a, true)) {
        if (++gNVSVLockMiss <= 8 || (gNVSVLockMiss & 1023) == 0)
            ALOG("surfvram scanout: accelerator lock busy, surface %u stays pending (misses %u)",
                 _ZNK9IOSurface12getSurfaceIDEv((void *)s), gNVSVLockMiss);
        return;
    }
    void *cache = _ZN22IOGraphicsAccelerator223deviceCacheForIOSurfaceEP9IOSurfacej((void *)a, (void *)s, 0);
    OSObject *ctx = cache ? *(OSObject *volatile *)((unsigned char *)cache + 0x20) : nullptr;
    NVResource *res = OSDynamicCast(NVResource, ctx);
    bool out = false;
    if (res) res->pageoff(nullptr, true, &out, 0);
    if (!out) nvSVPend(_ZNK9IOSurface12getSurfaceIDEv((void *)s), 0);
    if (cache) ((OSObject *)cache)->release();
    nvFamUnlock(a);
}
// True when the surface's system memory holds its newest pixels, after moving them there if the GPU drew them into VRAM.
// Unlike nvAccelSurfScanoutSync it waits a few ms for the accelerator lock and does not clear the pending mark when the
// page-off fails. At most 4 frames in a row are held back per head: a surface that never syncs is copied as before
// rather than freezing the panel.
static unsigned gIopCopyHeld[kNvMaxFB];
static bool nvAccelSurfCopyReady(IOSurface *s, unsigned head)
{
    NVAccel *a = NVAccel::gAccel;
    if (!s || !a) return true;
    const unsigned id = _ZNK9IOSurface12getSurfaceIDEv((void *)s);
    bool ready = !nvSVPend(id, 2);
    if (!ready) {
        bool locked = false;
        for (unsigned i = 0; i < 8 && !(locked = nvFamLock(a, true)); i++) IOSleep(1);
        if (locked) {
            void *cache = _ZN22IOGraphicsAccelerator223deviceCacheForIOSurfaceEP9IOSurfacej((void *)a, (void *)s, 0);
            OSObject *ctx = cache ? *(OSObject *volatile *)((unsigned char *)cache + 0x20) : nullptr;
            NVResource *res = OSDynamicCast(NVResource, ctx);
            volatile unsigned short *valid = res ? *(unsigned short *volatile *)((volatile unsigned char *)res + 0x30) : nullptr;
            if (!res || !valid || (valid[0] & 1u)) ready = true;
            else if (!nvAccelIopCopyNeedsPageoff(false, nvAccelResPluginWrote(res))) { ready = true; OSAddAtomic64(1, &gCntIopCopyUnwritten); }
            else { bool out = false; res->pageoff(nullptr, true, &out, 0); ready = out || (valid[0] & 1u); }
            if (ready) nvSVPend(id, 0);
            if (cache) ((OSObject *)cache)->release();
            nvFamUnlock(a);
        }
    }
    if (head >= kNvMaxFB) return true;
    if (ready) { gIopCopyHeld[head] = 0; return true; }
    if (++gIopCopyHeld[head] > 4) { gIopCopyHeld[head] = 0; return true; }
    OSAddAtomic64(1, &gCntIopCopySkip);
    return false;
}

static unsigned nvVaAllocLogged, nvVaMapLogged;

static unsigned long long nvAccelGpuVaAlloc(IOAccelMemoryMap *map,
                                            unsigned long long len, unsigned long long align)
{
    NVMemoryMap *mm = OSDynamicCast(NVMemoryMap, map);
    if (!mm || !len) return 0;
    if (mm->fGpuVa) return mm->fGpuVa;

    struct NVRMRmHandles h;
    if (!nvAccelRmHandles(&h)) return 0;

    struct NVRMGpuVaRequest g;
    nvAccelZero(&g, sizeof g);
    g.version = NVRM_GPUVA_ABI_VERSION;
    g.kapiDevice = h.kapiDevice;
    g.size = len; g.align = align;
    if (nvAccelGpuVaCall(NVRM_GPUVA_FN_ALLOC, &g) != kIOReturnSuccess || !g.gpuva) {
        if (nvVaAllocLogged < 8) { nvVaAllocLogged++;
            ALOG("gpuVaAlloc: RM refused %llu bytes (align %llu) -> status 0x%x", len, align, g.status); }
        return 0;
    }
    mm->fHVirt = g.hVirt; mm->fGpuVa = g.gpuva; mm->fVaLen = len;
    if (nvVaAllocLogged < 8) { nvVaAllocLogged++;
        ALOG("gpuVaAlloc: %llu bytes -> gpuVA 0x%llx (hVirt 0x%x)", len, g.gpuva, g.hVirt); }
    return g.gpuva;
}

static void nvAccelGpuVaFree(IOAccelMemoryMap *map)
{
    NVMemoryMap *mm = OSDynamicCast(NVMemoryMap, map);
    if (!mm || !mm->fHVirt) return;
    struct NVRMRmHandles h;
    if (!nvAccelRmHandles(&h)) return;
    struct NVRMGpuVaRequest g;
    nvAccelZero(&g, sizeof g);
    g.version = NVRM_GPUVA_ABI_VERSION;
    g.kapiDevice = h.kapiDevice;
    g.hVirt = mm->fHVirt; g.hMemory = mm->fMapped ? mm->fHRmMemory : 0; g.size = mm->fVaLen;
    g.gpuva = mm->fGpuVa;
    nvAccelGpuVaCall(NVRM_GPUVA_FN_FREE, &g);
    OSAddAtomic64(1, &gCntMapFree);
    mm->fHVirt = 0; mm->fGpuVa = 0; mm->fVaLen = 0; mm->fMapped = false;
}

bool NVMemoryMap::commitIntoGPUPageTable()
{
    if (fMapped) return true;
    NVVidMemory *vm = OSDynamicCast(NVVidMemory, fMem);
    void *kapiMem = vm ? vm->fRmHandle : nullptr;
    unsigned long long len = fVaLen ? fVaLen : getLength();
    if (!fHVirt || !kapiMem || !len) {
        if (fCommits < 8) { fCommits++;
            ALOG("MM commit: NOT MAPPED -- hVirt 0x%x kapiMem %p len %llu (mem %p%s)",
                 fHVirt, kapiMem, len, fMem, vm ? "" : ", not an NVVidMemory"); }
        return false;
    }
    struct NVRMRmHandles h;
    if (!nvAccelRmHandles(&h)) return false;
    struct NVRMGpuVaRequest g;
    nvAccelZero(&g, sizeof g);
    g.version = NVRM_GPUVA_ABI_VERSION;
    g.kapiDevice = h.kapiDevice; g.kapiMemory = kapiMem;
    g.hVirt = fHVirt; g.size = len;
    g.isVidmem = 1;
    if (nvAccelGpuVaCall(NVRM_GPUVA_FN_MAP, &g) != kIOReturnSuccess) {
        if (fCommits < 8) { fCommits++;
            ALOG("MM commit: MAP_MEMORY_DMA refused gpuVA 0x%llx len %llu -> status 0x%x",
                 fGpuVa, len, g.status); }
        return false;
    }
    fHRmMemory = g.hMemory; fMapped = true;
    OSAddAtomic64(1, &gCntMapCommit);
    if (g.gpuva) fGpuVa = g.gpuva;
    if (fCommits < 8) { fCommits++;
        ALOG("MM commit: MAPPED gpuVA 0x%llx len %llu <- hMem 0x%x  *** the GPU can reach these bytes ***",
             fGpuVa, len, g.hMemory); }
    return true;
}

void NVMemoryMap::releaseFromGPUPageTable()
{
    if (!fMapped || !fHVirt) return;
    struct NVRMRmHandles h;
    if (!nvAccelRmHandles(&h)) return;
    struct NVRMGpuVaRequest g;
    nvAccelZero(&g, sizeof g);
    g.version = NVRM_GPUVA_ABI_VERSION;
    g.kapiDevice = h.kapiDevice;
    g.hVirt = fHVirt; g.hMemory = fHRmMemory; g.size = fVaLen;
    g.gpuva = fGpuVa;
    g.flags = NVRM_GPUVA_FLAG_UNMAP_ONLY;
    nvAccelGpuVaCall(NVRM_GPUVA_FN_FREE, &g);
    fMapped = false;
    OSAddAtomic64(1, &gCntMapRelease);
}

static void nvAccelWriteStamp(int index, unsigned int value)
{
    NVAccel *a = NVAccel::gAccel;
    if (!a || !a->fStamps || index < 0 || index >= 4096) return;
    volatile unsigned int *base = (volatile unsigned int *)a->fStamps->getBytesNoCopy();
    if (!base) return;
    base[index] = value;
}

static unsigned nvFrontSamples, nvFrontFlips;
static unsigned nvFrontLastSum;
static void nvAccelSampleFront(NVVidMemory *vm, unsigned w, unsigned h, unsigned pitch)
{
    if (!vm || !vm->fKva || !pitch || !w || !h) return;
#define NVFRONT_EVERY 10u
    if (++nvFrontFlips % NVFRONT_EVERY) return;
    if (nvFrontSamples >= 40) return;
    nvFrontSamples++;
    const volatile unsigned int *px = (const volatile unsigned int *)vm->fKva;
    const unsigned ppr = pitch / 4;
    unsigned n = 0, nonzero = 0, sum = 0;
    for (unsigned y = 0; y < h; y += 8)
        for (unsigned x = 0; x < w; x += 8) {
            unsigned v = px[y * ppr + x];
            sum = sum * 33u + v; if (v) nonzero++; n++;
        }
    const unsigned p1 = (w > 100  && h > 100)  ? px[100  * ppr + 100]  : 0u;
    const unsigned p2 = (w > 960  && h > 540)  ? px[540  * ppr + 960]  : 0u;
    const unsigned p3 = (w > 1800 && h > 1000) ? px[1000 * ppr + 1800] : 0u;
    const bool moved = (nvFrontSamples > 1) && (sum != nvFrontLastSum);
    ALOG("FRONT SURFACE sample %u: sum 0x%08x %s nonzero %u/%u "
         "px(100,100)=0x%08x px(960,540)=0x%08x px(1800,1000)=0x%08x %ux%u pitch %u",
         nvFrontSamples, sum,
         moved ? "*** MOVED -- A LIVE WRITER IS PAINTING THE SCANNED-OUT SURFACE ***"
               : (nvFrontSamples > 1 ? "(UNCHANGED since last sample -- frozen, not proof of a picture)"
                                     : "(first sample -- motion is the proof, wait for the next)"),
         nonzero, n, p1, p2, p3, w, h, pitch);
    nvFrontLastSum = sum;
}

static unsigned nvSwapProbes;
static void nvAccelResStats(IOAccelResource2 *res, const char *tag)
{
    if (!res) { ALOG("  swap %s: (null)", tag); return; }
    NVVidMemory *vm = nvres_vidmem(res);
    const unsigned char *r = (const unsigned char *)res;
    const unsigned w     = *(const volatile unsigned short *)(r + 0xb0);
    const unsigned h     = *(const volatile unsigned short *)(r + 0xb2);
    const unsigned pitch = *(const volatile unsigned int   *)(r + 0xb8);
    if (!vm || !vm->fKva || !pitch || !w || !h) {
        ALOG("  swap %s: %ux%u pitch %u vm %p kva %p scanout %d -- NOT SAMPLEABLE",
             tag, w, h, pitch, vm, vm ? vm->fKva : nullptr, vm ? (int)vm->fScanout : -1);
        return;
    }
    const volatile unsigned int *px = (const volatile unsigned int *)vm->fKva;
    const unsigned ppr = pitch / 4;
    unsigned n = 0, nonzero = 0, sum = 0;
    for (unsigned y = 0; y < h; y += 8)
        for (unsigned x = 0; x < w; x += 8) {
            unsigned v = px[y * ppr + x];
            sum = sum * 33u + v; if (v) nonzero++; n++;
        }
    ALOG("  swap %s: %ux%u pitch %u scanout %d  sum 0x%08x  nonzero %u/%u  %s",
         tag, w, h, pitch, (int)vm->fScanout, sum, nonzero, n,
         nonzero ? "*** HAS CONTENT -- THIS IS THE SOURCE ***" : "(empty)");
}
bool nvAccelResHasContent(IOAccelResource2 *res)
{
    if (!res) return false;
    NVVidMemory *vm = nvres_vidmem(res);
    if (!vm || !vm->fKva) return false;
    const unsigned char *r = (const unsigned char *)res;
    const unsigned w     = *(const volatile unsigned short *)(r + 0xb0);
    const unsigned h     = *(const volatile unsigned short *)(r + 0xb2);
    const unsigned pitch = *(const volatile unsigned int   *)(r + 0xb8);
    if (!w || !h || pitch < 4) return false;
    const volatile unsigned int *px = (const volatile unsigned int *)vm->fKva;
    const unsigned ppr = pitch / 4;
    for (unsigned iy = 1; iy <= 4; iy++)
        for (unsigned ix = 1; ix <= 4; ix++) {
            const unsigned x = (w * ix) / 5, y = (h * iy) / 5;
            if (x < w && y < h && px[y * ppr + x]) return true;
        }
    return false;
}

void nvAccelSwapProbe(IOAccelResource2 *a1, IOAccelResource2 *a2)
{
    if (nvSwapProbes >= 6) return;
    nvSwapProbes++;
    ALOG("SWAP PROBE %u -- which argument holds the composited frame?", nvSwapProbes);
    nvAccelResStats(a1, "a1");
    nvAccelResStats(a2, "a2");
}

static unsigned nvFlipWhy = 0;
#define NVFLIPWHY(...) do { if (nvFlipWhy < 20) { nvFlipWhy++; ALOG(__VA_ARGS__); } } while (0)

#define NVSCANOUT_COPY_MAX 120
static unsigned nvScanoutCopies = 0;
static bool nvAccelCopyToScanout(IOAccelResource2 *res)
{
    if (nvScanoutCopies >= NVSCANOUT_COPY_MAX) return false;
    if (!res) return false;
    NVAccel *a = NVAccel::gAccel;
    if (!a || !a->fFB) { NVFLIPWHY("copyToScanout: no accelerator/framebuffer"); return false; }
    if (gNvHeadsPiped & (gNvHeadsPiped - 1)) { NVFLIPWHY("copyToScanout: refused -- heads piped 0x%x, no way to tell whose frame this is", gNvHeadsPiped); return false; }
    NVVidMemory *vm = nvres_vidmem(res);
    if (!vm || !vm->fKva) { NVFLIPWHY("copyToScanout: source has no kva"); return false; }

    const unsigned char *rr = (const unsigned char *)res;
    const unsigned w     = *(const volatile unsigned short *)(rr + 0xb0);
    const unsigned h     = *(const volatile unsigned short *)(rr + 0xb2);
    const unsigned pitch = *(const volatile unsigned int   *)(rr + 0xb8);
    if (!w || !h || pitch < 4) { NVFLIPWHY("copyToScanout: source geometry %ux%u pitch %u", w, h, pitch); return false; }

    struct NVRMVramRequest q;
    for (unsigned i = 0; i < sizeof q; i++) ((volatile unsigned char *)&q)[i] = 0;
    q.version = NVRM_VRAM_ABI_VERSION;
    IOReturn ret = a->askFramebufferForScanout(&q);
    if (ret != kIOReturnSuccess || !q.kva || !q.pitch) {
        NVFLIPWHY("copyToScanout: framebuffer would not describe its scanout (0x%x kva %p pitch %u)",
                  (unsigned)ret, q.kva, q.pitch);
        return false;
    }

    const unsigned rows = (h < q.height) ? h : q.height;
    const unsigned rowb = ((pitch < q.pitch) ? pitch : q.pitch) & ~3u;
    const unsigned words = rowb / 4;
    const unsigned spr = pitch / 4, dpr = q.pitch / 4;
    const volatile unsigned int *s = (const volatile unsigned int *)vm->fKva;
    volatile unsigned int *d = (volatile unsigned int *)q.kva;
    for (unsigned y = 0; y < rows; y++) {
        const volatile unsigned int *sp = s + (unsigned long)y * spr;
        volatile unsigned int *dp = d + (unsigned long)y * dpr;
        for (unsigned x = 0; x < words; x++) dp[x] = sp[x];
    }
    nvScanoutCopies++;
    if (nvScanoutCopies <= 3 || nvScanoutCopies == NVSCANOUT_COPY_MAX)
        ALOG("*** COPIED THE COMPOSITED FRAME ONTO THE PANEL *** #%u: %ux%u, %u rows x %u bytes, "
             "src kva %p pitch %u -> scanout kva %p phys 0x%llx pitch %u%s",
             nvScanoutCopies, w, h, rows, rowb, vm->fKva, pitch, q.kva,
             (unsigned long long)q.phys, q.pitch,
             nvScanoutCopies == NVSCANOUT_COPY_MAX ? "  [CAP REACHED -- stopping, panel keeps this frame]" : "");
    return true;
}

static bool nvAccelFlipTo(IOAccelResource2 *res)
{
    if (!res) { NVFLIPWHY("nvAccelFlipTo: no resource"); return false; }
    NVAccel *a = NVAccel::gAccel;
    if (!a || !a->fFB) { NVFLIPWHY("nvAccelFlipTo: accel %p fFB %p", a, a ? a->fFB : nullptr); return false; }
    if (gNvHeadsPiped & (gNvHeadsPiped - 1)) { NVFLIPWHY("nvAccelFlipTo: refused -- heads piped 0x%x, no way to tell whose surface this is", gNvHeadsPiped); return false; }
    NVVidMemory *vm = nvres_vidmem(res);
    if (!vm || !vm->fRmHandle) {
        NVFLIPWHY("nvAccelFlipTo: vm %p fRmHandle %p -- the composited surface has no RM memory to flip to",
                  vm, vm ? vm->fRmHandle : nullptr);
        return false;
    }
    if (vm->fScanout) { NVFLIPWHY("nvAccelFlipTo: already the scanout"); return false; }

    const unsigned char *r = (const unsigned char *)res;
    const unsigned w     = *(const volatile unsigned short *)(r + 0xb0);
    const unsigned h     = *(const volatile unsigned short *)(r + 0xb2);
    const unsigned pitch = *(const volatile unsigned int   *)(r + 0xb8);
    if (!w || !h || !pitch) { NVFLIPWHY("nvAccelFlipTo: geometry %ux%u pitch %u", w, h, pitch); return false; }

    struct NVRMFlipRequest f;
    volatile unsigned char *z = (volatile unsigned char *)&f;
    for (unsigned i = 0; i < sizeof f; i++) z[i] = 0;
    f.version = NVRM_FLIP_ABI_VERSION;
    f.kapiMemory = vm->fRmHandle; f.allocationCookie = vm->fAllocationCookie;
    f.width = w; f.height = h; f.pitch = pitch;

    const OSSymbol *sym = OSSymbol::withCStringNoCopy(NVRM_FLIP_FN);
    if (!sym) { NVFLIPWHY("nvAccelFlipTo: OSSymbol allocation failed"); return false; }
    IOReturn rr = a->fFB->callPlatformFunction(sym, false, &f, nullptr, nullptr, nullptr);
    sym->release();
    if (rr != kIOReturnSuccess) {
        NVFLIPWHY("nvAccelFlipTo: framebuffer refused the flip (0x%x, flipResult %d) for %ux%u pitch %u kapiMemory %p",
                  (unsigned)rr, f.flipResult, w, h, pitch, vm->fRmHandle);
        return false;
    }
    NVFLIPWHY("nvAccelFlipTo: *** FLIPPED *** to %ux%u pitch %u kapiMemory %p (flipResult %d)",
              w, h, pitch, vm->fRmHandle, f.flipResult);
    nvAccelSampleFront(vm, w, h, pitch);
    return true;
}

IOReturn NVAccel::callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4)
{
    if (fn && p1 && fn->isEqualTo(NVACCEL_SS_FN))
        return nvAccelSurfVram((struct NVRMSurfShareEsc *)p1, (task_t)p2, (unsigned)(uintptr_t)p3);
    return IOService::callPlatformFunction(fn, wait, p1, p2, p3, p4);
}

NM_TAHOE_FWD_DEFS
