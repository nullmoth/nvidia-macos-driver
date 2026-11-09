// NMGopFB: panel provider for laptops whose built-in screen is wired to an integrated AMD GPU macOS has no driver for
// (Rembrandt, Phoenix, Hawk Point, Strix and the like), so that the NVIDIA card can draw the desktop and show it there.
//
// It writes nothing to the display engine. The firmware (GOP) lit the panel and left the GPU scanning out a plain 32-bit
// framebuffer; this driver publishes that framebuffer as head 4 of the NVIDIA accelerator (the same slot NMIntelFB uses),
// maps it write-combined, and NVAccel copies each composited frame into it (nm_hybrid.h: only the pixels that changed).
// If anything about the framebuffer is not what it expects, start() fails with the reason logged and the firmware
// framebuffer keeps the device exactly as before.
//
// Level 1 (copy) only. Import (level 2) needs the display engine's translation table, which this driver does not touch;
// the accelerator is told so and copies.
//
// The idea, and the way the firmware framebuffer is found, checked against the GPU's BAR and mapped write-combined, come
// from PrimeBridge (https://github.com/myvoow/PrimeBridge, BootFramebuffer.cpp), MIT License, Copyright (c) 2026 myvoow.
// Permission is hereby granted, free of charge, to any person obtaining a copy of that software and associated
// documentation files, to deal in it without restriction, subject to the following condition: the above copyright
// notice and this permission notice shall be included in all copies or substantial portions of the Software. THE SOFTWARE
// IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND; the full MIT text is in NOTICE.
//
// One switch, default off: boot-arg nmhybrid=1 (nm_hybrid.h). With it absent, 0 or any other value this driver does not
// start, so a machine without the switch behaves exactly as before. -nmgopfboff turns it off even with the switch on.

#include <IOKit/IOLib.h>
// IOPCIDevice (with its setBusMasterEnable) is marked deprecated in favour of PCIDriverKit, which runs in user space and
// cannot replace a kernel extension's PCI access; that deprecation is ignored from here on in this kext's sources.
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/graphics/IOFramebuffer.h>
#include <IOKit/ndrvsupport/IOMacOSVideo.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/IODeviceMemory.h>
#include <pexpert/pexpert.h>
#include <kern/thread_call.h>
#include <libkern/version.h>
#include "nvrm_vram_abi.h"
#include "nm_hybrid.h"
#include "nm_hybrid_abi.h"
#include "nm_gopfb.h"

#define GFBLOG(fmt, ...) IOLog("NMGopFB: " fmt "\n", ##__VA_ARGS__)

#include <mach/kmod.h>
extern "C" kern_return_t _start(kmod_info_t *ki, void *d); extern "C" kern_return_t _stop(kmod_info_t *ki, void *d);
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMGopFB, "0.1", _start, _stop) }
extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }

namespace {
struct IntelId { uint16_t id; uint8_t ver; };
const IntelId kIntelIds[] = {
#include "../NMIntelFB/display_ids.inc"
};
unsigned intelDisplayVersion(uint16_t dev)
{
    for (const IntelId &e : kIntelIds) if (e.id == dev) return e.ver;
    return 0;
}
IOPMPowerState kPowerStates[2] = {
    { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
    { kIOPMPowerStateVersion1, kIOPMDeviceUsable, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
};
}

class NMGopFB : public IOFramebuffer {
    OSDeclareDefaultStructors(NMGopFB)
    IOMemoryMap *fSurfMap = nullptr;   // write-combined CPU view of the firmware framebuffer (NVIDIA frames land here)
    struct Irq { IOFBInterruptProc proc = nullptr; OSObject *target = nullptr; void *ref = nullptr; };
    Irq fVbl, fConnect;
    thread_call_t fVblTimer = nullptr;
    uint64_t fVblNext = 0;
    volatile bool fStopping = false;
    uint64_t fPhys = 0;
    uint32_t fWidth = 0, fHeight = 0, fRowBytes = 0;
    int fHyLevel = 0;
    // The firmware does not tell the panel's refresh rate and this driver does not read the display engine, so the
    // pacing timer runs at 60 Hz, as NVRMFB's fallback does.
    static constexpr uint64_t kFrameNs = 16666667ull;

    static void vblFire(thread_call_param_t p0, thread_call_param_t) {
        NMGopFB *me = (NMGopFB *)p0;
        if (me->fStopping) return;
        if (me->fVbl.proc) me->fVbl.proc(me->fVbl.target, me->fVbl.ref);
        uint64_t period; nanoseconds_to_absolutetime(kFrameNs, &period);
        uint64_t now = mach_absolute_time();
        me->fVblNext = (me->fVblNext && me->fVblNext + period > now) ? me->fVblNext + period : now + period;
        thread_call_enter_delayed(me->fVblTimer, me->fVblNext);
    }
    void armVbl() {
        fVblTimer = thread_call_allocate(vblFire, this);
        if (fVblTimer) { uint64_t d; clock_interval_to_deadline((uint32_t)(kFrameNs / 1000), kMicrosecondScale, &d); fVblNext = d; thread_call_enter_delayed(fVblTimer, d); }
        GFBLOG("vbl timer %s", fVblTimer ? "armed (60 Hz)" : "ALLOCATE FAILED");
    }
    bool adoptFirmwareFramebuffer(IOPCIDevice *pci);

public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;

    IOReturn enableController() override { return kIOReturnSuccess; }
    bool isConsoleDevice() override { return true; }
    IODeviceMemory *getApertureRange(IOPixelAperture aperture) override;
    const char *getPixelFormats() override { return IO32BitDirectPixels "\0"; }
    IOItemCount getDisplayModeCount() override { return 1; }
    IOReturn getDisplayModes(IODisplayModeID *modes) override { modes[0] = 1; return kIOReturnSuccess; }
    IOReturn getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info) override;
    UInt64 getPixelFormatsForDisplayMode(IODisplayModeID, IOIndex) override { return 0; }
    IOReturn getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi) override;
    IOReturn getCurrentDisplayMode(IODisplayModeID *mode, IOIndex *depth) override { *mode = 1; *depth = 0; return kIOReturnSuccess; }
    IOReturn getStartupDisplayMode(IODisplayModeID *mode, IOIndex *depth) override { return getCurrentDisplayMode(mode, depth); }
    IOReturn getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info) override {
        if (mode != 1 || !info) return kIOReturnUnsupportedMode;
        bzero(info, sizeof(*info));
        info->appleTimingID = kIOTimingIDInvalid;      // no timing is known: the firmware's, which this driver never reads
        return kIOReturnSuccess;
    }
    IOReturn setDisplayMode(IODisplayModeID mode, IOIndex depth) override { return (mode == 1 && depth == 0) ? kIOReturnSuccess : kIOReturnUnsupported; }
    IOItemCount getConnectionCount() override { return 1; }
    IOReturn callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4) override;
    IOReturn registerForInterruptType(IOSelect type, IOFBInterruptProc proc, OSObject *target, void *ref, void **iref) override {
        if (type == kIOFBVBLInterruptType) { fVbl = { proc, target, ref }; if (iref) *iref = &fVbl; return kIOReturnSuccess; }
        if (type == kIOFBConnectInterruptType) { fConnect = { proc, target, ref }; if (iref) *iref = &fConnect; return kIOReturnSuccess; }
        return kIOReturnUnsupported;
    }
    IOReturn unregisterInterrupt(void *) override { return kIOReturnSuccess; }
    IOReturn setInterruptState(void *, UInt32) override { return kIOReturnSuccess; }
    IOReturn setGammaTable(UInt32, UInt32, UInt32, void *) override { return kIOReturnSuccess; }
    IOReturn setGammaTable(UInt32, UInt32, UInt32, void *, bool) override { return kIOReturnSuccess; }
    IOReturn setCLUTWithEntries(IOColorEntry *, UInt32, UInt32, IOOptionBits) override { return kIOReturnSuccess; }
    IOReturn setApertureEnable(IOPixelAperture, IOOptionBits) override { return kIOReturnSuccess; }
    IOReturn setStartupDisplayMode(IODisplayModeID, IOIndex) override { return kIOReturnSuccess; }
    // Same power handling as NMIntelFB and NVRMFB, which share the controller (IOFBDependentID): acknowledge at once.
    IOReturn setPowerState(unsigned long ordinal, IOService *) override {
        handleEvent(ordinal ? kIOFBNotifyDidPowerOn : kIOFBNotifyWillPowerOff);
        return kIOPMAckImplied;
    }
    IOReturn setAttribute(IOSelect attribute, uintptr_t value) override {
        if (attribute == kIOWindowServerActiveAttribute) return kIOReturnSuccess;
        if (attribute == kIOPowerAttribute) {
            if (value) { IOReturn r = IOFramebuffer::setAttribute(attribute, value); handleEvent(kIOFBNotifyDidPowerOn);
                         return r == kIOReturnUnsupported ? kIOReturnSuccess : r; }
            handleEvent(kIOFBNotifyWillPowerOff);
            IOReturn r = IOFramebuffer::setAttribute(attribute, value);
            return r == kIOReturnUnsupported ? kIOReturnSuccess : r;
        }
        return IOFramebuffer::setAttribute(attribute, value);
    }
    IOReturn setAttributeForConnection(IOIndex idx, IOSelect attribute, uintptr_t value) override {
        switch (attribute) {
        case kConnectionPower: case kConnectionColorModesSupported: case kConnectionColorDepthsSupported:
        case kConnectionControllerColorDepth: case kConnectionControllerDepthsSupported: case kConnectionColorMode:
        case kConnectionDisplayFlags: case kConnectionFlags:
            return kIOReturnSuccess;
        default: return IOFramebuffer::setAttributeForConnection(idx, attribute, value);
        }
    }
    IOReturn connectFlags(IOIndex, IODisplayModeID, IOOptionBits *flags) override {
        if (flags) *flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
        return kIOReturnSuccess;
    }
    IODeviceMemory *getVRAMRange() override { return getApertureRange(kIOFBSystemAperture); }
    IOReturn getAttribute(IOSelect attribute, uintptr_t *value) override {
        if (attribute == kIOWindowServerActiveAttribute) { if (value) *value = 1; return kIOReturnSuccess; }
        return IOFramebuffer::getAttribute(attribute, value);
    }
    IOReturn getAttributeForConnection(IOIndex idx, IOSelect attr, uintptr_t *value) override;
    IOReturn getAppleSense(IOIndex connectIndex, UInt32 *senseType, UInt32 *primary, UInt32 *extended, UInt32 *displayType) override;
};

OSDefineMetaClassAndStructors(NMGopFB, IOFramebuffer)

IOService *NMGopFB::probe(IOService *provider, SInt32 *score)
{
    if (version_major != 24 && version_major != 25) return nullptr;   // macOS 15 (Darwin 24) and macOS 26 (Darwin 25) only
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci) return nullptr;
    if (PE_parse_boot_argn("-nmgopfboff", nullptr, 0)) { GFBLOG("off by boot-arg"); return nullptr; }
    // One switch, default off, the same one NMIntelFB and the accelerator read.
    int lv = 0; const int present = PE_parse_boot_argn("nmhybrid", &lv, sizeof lv);
    fHyLevel = nmh_level_parse(present, lv);
    if (!fHyLevel) return nullptr;
    const uint16_t ven = pci->configRead16(kIOPCIConfigVendorID), dev = pci->configRead16(kIOPCIConfigDeviceID);
    // the AMD / Intel test driver (NMXGPU) drives the whole GPU, display included, when it is installed
    int nmx = 0;
    if (OSDictionary *m = IOService::serviceMatching("NMXGPU")) {
        if (OSIterator *it = IOService::getMatchingServices(m)) { nmx = it->getNextObject() != nullptr; it->release(); }
        m->release();
    }
    uint32_t ovr = 0;
    PE_parse_boot_argn("nmgopid", &ovr, sizeof ovr);
    const int sel = nmgop_select(ven, dev, ven == NMGOP_VENDOR_INTEL ? intelDisplayVersion(dev) : 0, nmx, ovr);
    if (sel != NMGOP_USE_GOPFB) {
        if (sel == NMGOP_USE_INTELFB) GFBLOG("%04x:%04x is an Intel panel NMIntelFB drives - leaving it to that driver", ven, dev);
        else if (nmx) GFBLOG("%04x:%04x is driven by NMXGPU - leaving it", ven, dev);
        return nullptr;
    }
    GFBLOG("%04x:%04x selected (hybrid level %d)", ven, dev, fHyLevel);
    return IOFramebuffer::probe(provider, score);
}

// Finds the firmware framebuffer, checks it against this GPU's BARs and maps it write-combined.
bool NMGopFB::adoptFirmwareFramebuffer(IOPCIDevice *pci)
{
    PE_Video v {};
    if (IOService::getPlatform()->getConsoleInfo(&v) != kIOReturnSuccess) { GFBLOG("no console info - leaving the firmware screen"); return false; }
    GFBLOG("console base 0x%llx %lux%lu depth %lu rowBytes %lu", (unsigned long long)v.v_baseAddr, v.v_width, v.v_height, v.v_depth, v.v_rowBytes);
    nmgop_bar bars[6]; unsigned n = 0;
    static const int kRegs[6] = { kIOPCIConfigBaseAddress0, kIOPCIConfigBaseAddress1, kIOPCIConfigBaseAddress2,
                                  kIOPCIConfigBaseAddress3, kIOPCIConfigBaseAddress4, kIOPCIConfigBaseAddress5 };
    for (int r : kRegs) if (IODeviceMemory *m = pci->getDeviceMemoryWithRegister((UInt8)r)) { bars[n].phys = m->getPhysicalAddress(); bars[n].len = m->getLength(); n++; }
    nmgop_geom g;
    const int why = nmgop_geometry((uint64_t)v.v_baseAddr, v.v_width, v.v_height, v.v_rowBytes, v.v_depth, bars, n, &g);
    if (why != NMGOP_OK) { GFBLOG("%s - leaving the firmware screen", nmgop_why_name(why)); return false; }
    if (g.flags) GFBLOG("base address carries flag bits 0x%llx (removed)", (unsigned long long)g.flags);
    if (g.phys & 0xFFFull) GFBLOG("note: base 0x%llx is not 4 KiB aligned", (unsigned long long)g.phys);
    // XNU fixes a physical range's cache mode when its descriptor is created and defaults unmanaged memory to uncached;
    // asking for write-combining only at map time left such a mapping at a hundredth of the speed. kIOMemoryBufferCacheShift
    // is private to XNU (28).
    const IOOptionBits kCacheShift = 28;
    IOMemoryDescriptor *md = IOMemoryDescriptor::withAddressRange(g.phys, g.bytes,
        kIODirectionInOut | kIOMemoryMapperNone | (kIOWriteCombineCache << kCacheShift), TASK_NULL);
    fSurfMap = md ? md->createMappingInTask(kernel_task, 0, kIOMapAnywhere | kIOMapWriteCombineCache) : nullptr;
    if (md) md->release();
    if (!fSurfMap) { GFBLOG("the framebuffer would not map write-combined - leaving the firmware screen"); return false; }
    fPhys = g.phys; fWidth = g.width; fHeight = g.height; fRowBytes = g.pitch;
    GFBLOG("firmware framebuffer %ux%u pitch %u at 0x%llx (BAR%u +0x%llx)", fWidth, fHeight, fRowBytes,
           (unsigned long long)fPhys, g.bar, (unsigned long long)g.bar_offset);
    return true;
}

bool NMGopFB::start(IOService *provider)
{
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci) return false;
    pci->setMemoryEnable(true);
    if (!adoptFirmwareFramebuffer(pci)) return false;
    if (!IOFramebuffer::start(provider)) { OSSafeReleaseNULL(fSurfMap); return false; }
    // Head 4 of the NVIDIA accelerator: WindowServer composites this panel on the NVIDIA card and the display pipe copies
    // each frame into the mapping.
    setProperty("IOFBDependentID", 0x4E56524D00000001ull, 64);
    setProperty("IOFBDependentIndex", 4ull, 32);
    GFBLOG("hybrid: provider NMGopFB %ux%u pitch %u level %d (CPU-mapped scanout, no VRAM)", fWidth, fHeight, fRowBytes, fHyLevel);
    setProperty("NMGopFBHybridLevel", (unsigned long long)fHyLevel, 32);
    setProperty("built-in", kOSBooleanTrue);
    armVbl();
    registerPowerDriver(this, kPowerStates, 2);
    GFBLOG("built-in panel published (%ux%u)", fWidth, fHeight);
    return true;
}

void NMGopFB::stop(IOService *provider)
{
    fStopping = true;
    if (fVblTimer) { thread_call_cancel(fVblTimer); while (!thread_call_free(fVblTimer)) { thread_call_cancel(fVblTimer); IOSleep(5); } fVblTimer = nullptr; }
    OSSafeReleaseNULL(fSurfMap);
    IOFramebuffer::stop(provider);
}

IODeviceMemory *NMGopFB::getApertureRange(IOPixelAperture aperture)
{
    if (aperture != kIOFBSystemAperture) return nullptr;
    return IODeviceMemory::withRange(fPhys, (IOPhysicalLength)fRowBytes * fHeight);
}

IOReturn NMGopFB::getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info)
{
    if (mode != 1 || !info) return kIOReturnBadArgument;
    bzero(info, sizeof(*info));
    info->maxDepthIndex = 0; info->nominalWidth = fWidth; info->nominalHeight = fHeight;
    info->refreshRate = 60 << 16; info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
    return kIOReturnSuccess;
}

IOReturn NMGopFB::getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi)
{
    if (mode != 1 || depth != 0 || aperture != kIOFBSystemAperture || !pi) return kIOReturnUnsupportedMode;
    bzero(pi, sizeof(*pi));
    pi->bytesPerRow = fRowBytes; pi->bytesPerPlane = 0; pi->bitsPerPixel = 32; pi->pixelType = kIORGBDirectPixels;
    pi->componentCount = 3; pi->bitsPerComponent = 8;
    pi->componentMasks[0] = 0x00FF0000; pi->componentMasks[1] = 0x0000FF00; pi->componentMasks[2] = 0x000000FF;
    strlcpy(pi->pixelFormat, IO32BitDirectPixels, sizeof(pi->pixelFormat));
    pi->activeWidth = fWidth; pi->activeHeight = fHeight;
    return kIOReturnSuccess;
}

// NVAccel's display pipe asks every head for its scan-out. This head answers with a CPU mapping and no VRAM address (phys 0
// keeps NVAccel from treating AMD memory as NVIDIA VRAM): frames reach the panel only through the copy into kva. Import is
// refused with a reason, so the accelerator copies.
IOReturn NMGopFB::callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4)
{
    if (fn && fn->isEqualTo(NMH_FN_IMPORT)) {
        NMHybridReq *q = (NMHybridReq *)p1;
        if (!q || q->version != NMH_ABI_VERSION) return kIOReturnBadArgument;
        if (q->op == NMH_OP_HOME) { q->result = NMH_OK; return kIOReturnSuccess; }   // the plane was never moved
        q->result = NMH_E_PROVIDER;
        return kIOReturnUnsupported;
    }
    if (fn && fn->isEqualTo(NVRM_VRAM_FN_SCANOUT)) {
        NVRMVramRequest *r = (NVRMVramRequest *)p1;
        if (!r || r->version != NVRM_VRAM_ABI_VERSION) return kIOReturnBadArgument;
        if (!fSurfMap) return kIOReturnNotReady;
        r->kva = (void *)fSurfMap->getVirtualAddress();
        r->phys = 0; r->actualSize = 0;
        r->width = fWidth; r->height = fHeight; r->pitch = fRowBytes;
        return kIOReturnSuccess;
    }
    return IOFramebuffer::callPlatformFunction(fn, wait, p1, p2, p3, p4);
}

// No backlight handler: the display engine is not touched, so the panel is a plain display without a brightness slider.
IOReturn NMGopFB::getAppleSense(IOIndex, UInt32 *, UInt32 *, UInt32 *, UInt32 *)
{
    return kIOReturnUnsupported;
}

IOReturn NMGopFB::getAttributeForConnection(IOIndex idx, IOSelect attr, uintptr_t *value)
{
    if (idx != 0 || !value) return kIOReturnBadArgument;
    switch (attr) {
    case kConnectionEnable: *value = 1; return kIOReturnSuccess;
    case kConnectionCheckEnable: *value = 1; return kIOReturnSuccess;
    case kConnectionFlags: *value = kIOConnectionBuiltIn; return kIOReturnSuccess;
    case kConnectionColorModesSupported: case kConnectionColorMode: *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionColorDepthsSupported: case kConnectionControllerColorDepth: case kConnectionControllerDepthsSupported:
        *value = 0x00000002; return kIOReturnSuccess;
    default: return IOFramebuffer::getAttributeForConnection(idx, attr, value);
    }
}
