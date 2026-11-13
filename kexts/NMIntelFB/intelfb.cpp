// NMIntelFB: framebuffer for the built-in panel of laptops whose panel is wired to an Intel GPU macOS has no driver for
// (display versions 12-20: Tiger Lake, Rocket Lake, Alder Lake, Raptor Lake, Meteor Lake, Arrow Lake, Lunar Lake).
//
// Phase 1 takes over the mode the firmware already lit: it finds the pipe whose primary plane is scanning out, checks
// that the plane is linear 32-bit XRGB, and publishes that surface (through the graphics aperture, BAR2 + the plane's GGTT
// offset) as an IOFramebuffer with the panel's real size and refresh. Nothing is written to the display engine, so a
// laptop it does not understand keeps exactly the screen it had: start() fails, every reason is logged, and the firmware
// framebuffer (IONDRVFramebuffer) takes the device as before.
//
// Register layout: Linux i915/xe display headers (MIT). Plane 1 of pipe P: PLANE_CTL 0x70180 + P*0x1000, STRIDE +0x08,
// SIZE +0x10, SURF +0x1C. Transcoder T timings at 0x60000 + T*0x1000, TRANSCONF at 0x70008 + P*0x1000, PIPESRC 0x6001C.

#include <IOKit/IOLib.h>
// IOPCIDevice (with its setBusMasterEnable) is marked deprecated in favour of PCIDriverKit, which runs in user space and
// cannot replace a kernel extension's PCI access; that deprecation is ignored from here on in this kext's sources.
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/pci/IOPCIDevice.h>
#include <IOKit/graphics/IOFramebuffer.h>
#include <IOKit/graphics/IODisplay.h>
#include <IOKit/ndrvsupport/IOMacOSVideo.h>
#include <libkern/OSAtomic.h>
#include "nvrm_vram_abi.h"
#include "nm_hybrid.h"
#include "nm_hybrid_abi.h"
#include "nm_hybrid_resume.h"
#include "nm_hybrid_resume14.h"
#include "nm_hybrid_tc.h"
#include "nm_hybrid_extmode.h"
#include "nm_hybrid_heads.h"
#include "nm_hybrid_vbt.h"
#include "nm_hybrid_dmc.h"
#include "nvrm_off.h"
#include <IOKit/IODeviceTreeSupport.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOTimerEventSource.h>
#include <IOKit/IOWorkLoop.h>
#include <kern/thread_call.h>
#include <sys/sysctl.h>
#include <libkern/version.h>

#define IFBLOG(fmt, ...) IOLog("NMIntelFB: " fmt "\n", ##__VA_ARGS__)

// kmutil places a kext in a kernel collection only with a _kmod_info symbol (Xcode generates it; this build does not)
#include <mach/kmod.h>
extern "C" kern_return_t _start(kmod_info_t *ki, void *d); extern "C" kern_return_t _stop(kmod_info_t *ki, void *d);
#ifdef NMINTELFB_VIRTUAL
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMIntelFBVirtual, "0.1", _start, _stop) }
#else
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NMIntelFB, "0.1", _start, _stop) }
#endif
extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }

// last step reached (see ifbStep in the namespace below)
char gIfbStepStr[48] = "none";
SYSCTL_STRING(_debug, OID_AUTO, nmintelfb_step, CTLFLAG_RD | CTLFLAG_LOCKED, gIfbStepStr, sizeof gIfbStepStr,
              "last step the Intel panel driver reached (nmhybrid)");

// display firmware hand-over: a root helper writes the verified file here (package/nm_dmc_push); debug.nmintelfb_dmc_want names the
// file this GPU needs
class NMIntelFB;
static NMIntelFB *gDmcTarget = nullptr;
// Serialises the firmware upload (sysctl, any thread), the reload after a resume (thread call) and stop: all three touch
// gDmcTarget and the target's fDmcBlob. Allocated once on first start and kept for the life of the kext, so no path can
// race its free.
static IOLock *gDmcLock = nullptr;
static int nmintelfb_dmc_write SYSCTL_HANDLER_ARGS;
SYSCTL_PROC(_debug, OID_AUTO, nmintelfb_dmc, CTLTYPE_OPAQUE | CTLFLAG_WR | CTLFLAG_LOCKED, nullptr, 0, nmintelfb_dmc_write, "-",
            "Intel display firmware for the panel driver (root only; parsed and checked before use)");
static int nmintelfb_dmc_want_read SYSCTL_HANDLER_ARGS;
SYSCTL_PROC(_debug, OID_AUTO, nmintelfb_dmc_want, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, nullptr, 0, nmintelfb_dmc_want_read, "A",
            "name of the Intel display firmware file this GPU needs, or none");

namespace {
// display version by PCI device ID, from Linux include/drm/intel/pciids.h (MIT); 0 = not ours
struct Id { uint16_t id; uint8_t ver; };
const Id kIds[] = {
#include "display_ids.inc"
};

uint8_t displayVersion(uint16_t dev)
{
    for (const Id &e : kIds) if (e.id == dev) return e.ver;
    return 0;
}

constexpr uint32_t PLANE_CTL(int p)    { return 0x70180u + (uint32_t)p * 0x1000u; }
constexpr uint32_t PLANE_STRIDE(int p) { return 0x70188u + (uint32_t)p * 0x1000u; }
constexpr uint32_t PLANE_SIZE(int p)   { return 0x70190u + (uint32_t)p * 0x1000u; }
constexpr uint32_t PLANE_SURF(int p)   { return 0x7019Cu + (uint32_t)p * 0x1000u; }
constexpr uint32_t TRANSCONF(int p)    { return 0x70008u + (uint32_t)p * 0x1000u; }
constexpr uint32_t HTOTAL(int t)       { return 0x60000u + (uint32_t)t * 0x1000u; }
constexpr uint32_t VTOTAL(int t)       { return 0x6000Cu + (uint32_t)t * 0x1000u; }
constexpr uint32_t HBLANK(int t)       { return 0x60004u + (uint32_t)t * 0x1000u; }
constexpr uint32_t HSYNC(int t)        { return 0x60008u + (uint32_t)t * 0x1000u; }
constexpr uint32_t VBLANK(int t)       { return 0x60010u + (uint32_t)t * 0x1000u; }
constexpr uint32_t VSYNC(int t)        { return 0x60014u + (uint32_t)t * 0x1000u; }
// one count per vblank (Linux PIPE_FRMCOUNT_G4X, 0x70040 + pipe): counting it is how the real refresh is known without
// reading the PLL and M/N values back
constexpr uint32_t FRMCOUNT(int p)     { return 0x70040u + (uint32_t)p * 0x1000u; }
// the surface the plane is scanning out right now (Linux PLANE_SURFLIVE, display 9+); PLANE_SURF is the one being armed
constexpr uint32_t PLANE_SURFLIVE(int p) { return 0x701ACu + (uint32_t)p * 0x1000u; }
// graphics translation table invalidate after entries change (Linux GFX_FLSH_CNTL_GEN6, enable bit 0)
[[maybe_unused]] constexpr uint32_t kGfxFlshCntl = 0x101008u;
uint64_t ggttRead(void *ctx, uint64_t idx) { return ((volatile uint64_t *)ctx)[idx]; }

// Where the panel driver got to, for a boot that froze: a sysctl and a registry property, and the NVRAM variable
// nullmoth-ifb-step where the platform lets a kext write it at that point (read it with: nvram nullmoth-ifb-step).
int gIfbStep = NMH_STEP_NONE;
bool gIfbStepRegistered = false;
void ifbStep(int step)
{
    gIfbStep = step;
    snprintf(gIfbStepStr, sizeof gIfbStepStr, "%d %s", step, nmh_step_name(step));
    if (!gIfbStepRegistered) { gIfbStepRegistered = true; sysctl_register_oid(&sysctl__debug_nmintelfb_step); sysctl_register_oid(&sysctl__debug_nmintelfb_dmc); sysctl_register_oid(&sysctl__debug_nmintelfb_dmc_want); }
    IOLog("NMIntelFB: hybrid: step %s\n", gIfbStepStr);
    if (IORegistryEntry *opt = IORegistryEntry::fromPath("/options", gIODTPlane)) {
        if (OSData *d = OSData::withBytes(gIfbStepStr, (unsigned)strlen(gIfbStepStr))) { opt->setProperty("nullmoth-ifb-step", d); d->release(); }
        opt->release();
    }
}
// the boot guard's verdict, read the way NVRM does (nvrm_off.h)
int ifbStandDownWhy()
{
    char arg[8];
    const int nvoff = PE_parse_boot_argn("-nvoff", arg, sizeof arg) ? 1 : 0;
    int remove = 0; unsigned starts = 0;
    if (IORegistryEntry *opt = IORegistryEntry::fromPath("/options", gIODTPlane)) {
        if (OSObject *f = opt->copyProperty("nullmoth-remove")) { remove = 1; f->release(); }
        starts = nvrm_guard_starts(opt);
        opt->release();
    }
    return nmh_stand_down_why(nvoff, remove, starts, NVRM_GUARD_LIMIT);
}

// PLANE_CTL (display 11+): enable bit 31, format bits 27:23 (8 = XRGB 8:8:8:8, Linux PLANE_CTL_FORMAT_XRGB_8888 on
// ICL+), tiling bits 12:10 (0 = linear)
constexpr uint32_t kEnable = 1u << 31;
// the two power states NVRMFB registers: off, and on + usable
IOPMPowerState kPowerStates[2] = {
    { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
    { kIOPMPowerStateVersion1, kIOPMDeviceUsable, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
};
constexpr uint32_t fmtOf(uint32_t ctl)  { return (ctl >> 23) & 0x1F; }
constexpr uint32_t tileOf(uint32_t ctl) { return (ctl >> 10) & 0x7; }
}

// Panel brightness through the south display PWM (PCH display versions 12-20: CNP, ICP, TGP, ADP, MTP).
// Registers from Linux intel_backlight_regs.h (MIT): BXT_BLC_PWM_CTL 0xC8250 (bit 31 enable, bit 29 polarity),
// BXT_BLC_PWM_FREQ 0xC8254 (period in raw-clock ticks), BXT_BLC_PWM_DUTY 0xC8258. The method (keep the period and
// polarity the firmware programmed, scale only the duty, 64-bit product so long periods do not overflow, read the
// duty back) follows WhateverXe's XELP::PanelPWM, BSD-3-Clause (WhateverXe by b00t0x, derived from WhateverGreen, Copyright (c) 2018 vit9696); see NOTICE. Only a PWM the
// firmware already enabled is used: the period is never invented, so a panel lit another way keeps its brightness
// and gets no slider.
static const uint32_t kBlcPwmCtl = 0xC8250, kBlcPwmFreq = 0xC8254, kBlcPwmDuty = 0xC8258;
static const uint32_t kBlcPwmEnable = 1u << 31;
static const SInt32 kIntelMinBacklight = 5;   // never 0: some panels switch the backlight off completely
class NMIntelFB;
class NMIntelBacklight : public IODisplayParameterHandler {
    OSDeclareDefaultStructors(NMIntelBacklight)
    NMIntelFB *fFB = nullptr;
    thread_call_t fApply = nullptr;
    volatile SInt32 fPending = -1;
    SInt32 fLevel = 100;
    static void applyFire(thread_call_param_t me, thread_call_param_t);
public:
    static NMIntelBacklight *withFB(NMIntelFB *fb, SInt32 level);
    bool setDisplay(IODisplay *display) override;
    bool doIntegerSet(OSDictionary *params, const OSSymbol *paramName, UInt32 value) override;
    bool doDataSet(const OSSymbol *, OSData *) override { return false; }
    bool doUpdate() override { return true; }
    void free() override;
};

class NMIntelFB : public IOFramebuffer {
    OSDeclareDefaultStructors(NMIntelFB)
    IOPCIDevice *fPCI = nullptr;
    IOMemoryMap *fMMIO = nullptr;
    IOMemoryMap *fSurfMap = nullptr;   // write-combined CPU view of the scan-out surface (NVIDIA frames land here)
    // WindowServer paces an accelerated display on its VBL interrupt: the NVIDIA display pipe completes each frame on it.
    // Without one the first frame never completed and WindowServer stalled every display (in testing, the
    // virtual panel froze the Mac). A 60 Hz timer is the VBL, as NVRMFB's fallback is.
    struct Irq { IOFBInterruptProc proc = nullptr; OSObject *target = nullptr; void *ref = nullptr; };
    Irq fVbl, fConnect;
    thread_call_t fVblTimer = nullptr;
    uint64_t fVblNext = 0, fVblCalls = 0;
    volatile bool fStopping = false;
    static void vblFire(thread_call_param_t p0, thread_call_param_t) {
        NMIntelFB *me = (NMIntelFB *)p0;
        if (me->fStopping) return;
        if (me->fVbl.proc) { me->fVbl.proc(me->fVbl.target, me->fVbl.ref); me->fVblCalls++; }
        uint64_t period; nanoseconds_to_absolutetime(me->fFrameNs, &period);
        uint64_t now = mach_absolute_time();
        me->fVblNext = (me->fVblNext && me->fVblNext + period > now) ? me->fVblNext + period : now + period;
        thread_call_enter_delayed(me->fVblTimer, me->fVblNext);
    }
    void armVbl() {
        fVblTimer = thread_call_allocate(vblFire, this);
        if (fVblTimer) { uint64_t d; clock_interval_to_deadline((uint32_t)(fFrameNs / 1000), kMicrosecondScale, &d); fVblNext = d; thread_call_enter_delayed(fVblTimer, d); }
        IFBLOG("vbl timer %s", fVblTimer ? "armed (60 Hz)" : "ALLOCATE FAILED");
    }
#ifdef NMINTELFB_VIRTUAL
    IOBufferMemoryDescriptor *fVBuf = nullptr;
    IOTimerEventSource *fTimer = nullptr;
    unsigned fSamples = 0;
    // 64 pixels across the buffer: how many are non-black and how many differ from the first, published in the registry
public:
    // 64 pixels across the buffer, read on demand through sysctl debug.nmintelfb_sample (the registry property the
    // timer set never showed up)
    void sampleInto(char *b, size_t n) {
        if (!fSurfMap) { snprintf(b, n, "no surface"); return; }
        const volatile uint32_t *px = (const volatile uint32_t *)fSurfMap->getVirtualAddress();
        unsigned nz = 0, varied = 0; uint32_t first = px[0] & 0xFFFFFF;
        for (unsigned i = 0; i < 8; i++) for (unsigned j = 0; j < 8; j++) {
            uint32_t v = px[(fHeight * (2 * i + 1) / 16) * (fRowBytes / 4) + fWidth * (2 * j + 1) / 16] & 0xFFFFFF;
            if (v) nz++; if (v != first) varied++; }
        snprintf(b, n, "sample %u: nonzero %u/64 varied %u/64 first 0x%06x", ++fSamples, nz, varied, first);
    }
    void sample() { char b[96]; sampleInto(b, sizeof b); setProperty("NMIntelFBSample", b); }
private:
#endif   // write-combined CPU view of the scan-out surface (NVIDIA frames land here)
    volatile uint8_t *fRegs = nullptr;
    IOPhysicalAddress fApertureBase = 0;
    IOPhysicalLength fApertureLen = 0;
    uint32_t fWidth = 0, fHeight = 0, fRowBytes = 0, fSurf = 0, fRefresh16 = 60 << 16;
    uint64_t fFrameNs = 16666667ull;   // vblank period; measured from the pipe's frame counter on real hardware
    // transcoder timing (active, total, sync start/end) for getTimingInfoForDisplayMode
    uint32_t fHT = 0, fVT = 0, fHSS = 0, fHSE = 0, fVSS = 0, fVSE = 0;
    uint64_t fPclk = 0;
    int fPipe = -1;
    uint8_t fVer = 0;
    uint32_t fStrideReg = 0;                     // PLANE_STRIDE as the firmware left it, restored when going back to the boot surface

    // hybrid path, level 2 (nm_hybrid.h): the composited surface's pages are mapped into unused graphics-table entries and
    // the plane is flipped to them. Level 1 needs nothing here: NVAccel copies into fSurfMap.
    int fHyLevel = 0;
    bool fHyReady = false, fHyBad = false;
    volatile uint64_t *fGsm = nullptr;
    uint64_t fGsmEntries = 0;
    struct nmh_ring fRing;
    uint32_t fHyPitch = 0, fHyHeight = 0;
    [[maybe_unused]] uint32_t fHyFlips = 0;
    bool hybridSetup(uint32_t pitch, uint32_t height);
    bool hybridWaitLive(int slot);
    void hybridWriteSurf(uint32_t surf, uint32_t strideReg);
    void hybridHome();
    IOReturn hybridImport(NMHybridReq *q);
    // level 3: buffers this driver owns (nm_hybrid.h, struct nmh_own)
    struct nmh_own fOwn;
    IOBufferMemoryDescriptor *fOwnMd[NMH_OWN_BUFS] = {};
    unsigned long long *fOwnPages[NMH_OWN_BUFS] = {};    // bus address of each page, kept for the copy engine (level 4)
    uint32_t fOwnPageCount = 0;
    bool fOwnReady = false;
    bool ownSetup();
    void ownRelease();
    // sleep and wake (nm_hybrid_resume.h): boot-arg nmhybridresume=1
    bool fResumeOn = false, fSlept = false;
    struct nmr_snap *fSnap = nullptr;
    uint64_t *fFwPte = nullptr; uint64_t fFwPteFirst = 0, fFwPteCount = 0;
    thread_call_t fResumeCall = nullptr;
    bool ggttMap();
    bool readVbt(struct nmr_edp *out, uint8_t **keep, uint32_t *keepLen);
    // Intel display firmware (nm_hybrid_dmc.h), handed over by a root helper through debug.nmintelfb_dmc; boot-arg nmhybriddmc=1
    // loads it (with every event handler left disabled). The kext cannot read a file, and never trusts the bytes: they are parsed
    // and range-checked here before anything is kept or written.
    uint8_t *fDmcBlob = nullptr; uint32_t fDmcLen = 0; bool fDmcOn = false;
    struct nmd_step dmcStep() const;
public:
    int dmcAccept(const uint8_t *buf, uint32_t len);
    char fDmcWant[40] = "none";
private:
    void dmcApply();          // takes gDmcLock
    void dmcApplyLocked();    // caller holds gDmcLock
    void resumeCapture();
    void resumeRun();
    static void resumeFire(thread_call_param_t p0, thread_call_param_t) { ((NMIntelFB *)p0)->resumeRun(); }
    // Type-C ports (nm_hybrid_tc.h), observe only: boot-arg nmhybridtc=1 logs the live status of the four Type-C ports as cables come and
    // go (display versions 12 and 13). It never takes a PHY, so a dock keeps its USB lanes.
    thread_call_t fTcTimer = nullptr; struct nmt_port fTcPort[4]; uint32_t fTcLast[4] = {0, 0, 0, 0}; unsigned fTcN = 0;
    void tcStart();
    static void tcFire(thread_call_param_t p0, thread_call_param_t);
    // External test heads (boot-arg nmhybridext=1, display versions 12 and 13): when a cable arrives on a Type-C port the whole bring-up of
    // nm_hybrid_extmode.h runs on a second pipe with a colour-bar buffer mapped into the graphics table, and unplug takes it down. The
    // framebuffer for the window system is not published (heads.h has the lifecycle; the accelerator has no pipe for it yet).
    struct ExtObj;
    bool fExtOn = false; struct nmhd fHeads; struct nmhd_ops fHeadOps; ExtObj *fExtObj[NMHD_MAX] = {}; uint64_t fExtScratch = 0; uint32_t fExtPipes = 0;
    static int extDevOpen(void *c); static void extDevClose(void *c);
    static void *extCreate(void *c, unsigned slot, unsigned port); static int extBringUp(void *c, void *o, unsigned port);
    static int extPublish(void *c, void *o, unsigned slot); static void extUnpublish(void *c, void *o);
    static void extTakeDown(void *c, void *o, unsigned port); static void extRelease(void *c, void *o);

    uint32_t rd(uint32_t off) const { return *(volatile uint32_t *)(fRegs + off); }
    void wr(uint32_t off, uint32_t v) const { *(volatile uint32_t *)(fRegs + off) = v; }
    bool adoptFirmwareMode();
    uint32_t fPwmPeriod = 0;                     // firmware's PWM period; 0 = no brightness control
    NMIntelBacklight *fBacklight = nullptr;
    void publishBacklight();
public:
    bool setPanelPercent(uint32_t percent);      // from NMIntelBacklight's thread call; false if the write did not stick
private:

public:
    IOService *probe(IOService *provider, SInt32 *score) override;
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    void startUnwind();   // undo everything start() published; shared by a failed start and stop()

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
    // WindowServer showed the panel at 0.00 Hz without this (virtual panel)
    IOReturn getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info) override {
        if (mode != 1 || !info) return kIOReturnUnsupportedMode;
        bzero(info, sizeof(*info));
        info->appleTimingID = kIOTimingIDInvalid;
        if (!fPclk || fHT <= fWidth || fVT <= fHeight) return kIOReturnSuccess;
        IODetailedTimingInformationV2 &d = info->detailedInfo.v2;
        d.pixelClock = d.minPixelClock = d.maxPixelClock = fPclk;
        d.horizontalActive = fWidth; d.horizontalBlanking = fHT - fWidth;
        d.horizontalSyncOffset = fHSS > fWidth ? fHSS - fWidth : 0; d.horizontalSyncPulseWidth = fHSE > fHSS ? fHSE - fHSS : 0;
        d.verticalActive = fHeight; d.verticalBlanking = fVT - fHeight;
        d.verticalSyncOffset = fVSS > fHeight ? fVSS - fHeight : 0; d.verticalSyncPulseWidth = fVSE > fVSS ? fVSE - fVSS : 0;
        info->flags = kIODetailedTimingValid;
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
    // Power and connection handling copied from NVRMFB, which shares this controller (same IOFBDependentID). Left to
    // IOFramebuffer's base class, a power change queues on the shared controller thread and waits for an acknowledge
    // (IOFramebuffer::setPowerState returns a 45 s timeout, checkPowerWork -> setAttribute(kIOPowerStateAttribute));
    // NVRMFB acknowledges at once and answers the power attributes itself. With NMIntelFB under NVRM the
    // first WindowServer restart froze every display.
    IOReturn setPowerState(unsigned long ordinal, IOService *) override {
        handleEvent(ordinal ? kIOFBNotifyDidPowerOn : kIOFBNotifyWillPowerOff);
        if (fResumeOn) {
            if (!ordinal) { fSlept = true; ifbStep(NMH_STEP_SUSPEND); }
            else if (fSlept) { fSlept = false; thread_call_enter(fResumeCall); }   // off this thread: the replay waits on hardware
        }
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

OSDefineMetaClassAndStructors(NMIntelFB, IOFramebuffer)
static int nmintelfb_dmc_write SYSCTL_HANDLER_ARGS
{
    if (!req->newptr) return 0;
    if (req->newlen < 1024 || req->newlen > 160 * 1024) return EINVAL;
    uint8_t *buf = (uint8_t *)IOMalloc(req->newlen);
    if (!buf) return ENOMEM;
    int err = SYSCTL_IN(req, buf, req->newlen);
    if (!err) {
        if (!gDmcLock) err = ENXIO;
        else {
            IOLockLock(gDmcLock);
            err = gDmcTarget ? gDmcTarget->dmcAccept(buf, (uint32_t)req->newlen) : ENXIO;
            IOLockUnlock(gDmcLock);
        }
    }
    IOFree(buf, req->newlen);
    return err;
}
static int nmintelfb_dmc_want_read SYSCTL_HANDLER_ARGS
{
    char b[40] = "none";
    if (gDmcLock) { IOLockLock(gDmcLock); if (gDmcTarget) snprintf(b, sizeof b, "%s", gDmcTarget->fDmcWant); IOLockUnlock(gDmcLock); }
    return sysctl_handle_string(oidp, b, sizeof b, req);
}
#ifdef NMINTELFB_VIRTUAL
static NMIntelFB *gVirt;
static int nmintelfb_sample_sysctl SYSCTL_HANDLER_ARGS
{
    char b[128] = "no panel";
    if (gVirt) gVirt->sampleInto(b, sizeof b);
    return sysctl_handle_string(oidp, b, sizeof b, req);
}
SYSCTL_PROC(_debug, OID_AUTO, nmintelfb_sample, CTLTYPE_STRING | CTLFLAG_RD | CTLFLAG_LOCKED, nullptr, 0,
            nmintelfb_sample_sysctl, "A", "64 pixels of the virtual panel: non-black and varied counts");
#endif

IOService *NMIntelFB::probe(IOService *provider, SInt32 *score)
{
    if (version_major != 24 && version_major != 25) return nullptr;   /* the 2.x line supports macOS 15 (Darwin 24) and macOS 26 (Darwin 25) only */
#ifdef NMINTELFB_VIRTUAL
    if (!provider->getProperty("nm-intel-panel")) return nullptr;   // only the nub the loader published
    // test build: a virtual 1920x1080 "panel" in system memory, so the NVIDIA -> Intel frame path can be proven on a
    // machine with no Intel GPU (frames composited on the NVIDIA card must arrive in this buffer)
    fVer = 0xFF; return IOFramebuffer::probe(provider, score);
#endif
    IOPCIDevice *pci = OSDynamicCast(IOPCIDevice, provider);
    if (!pci) return nullptr;
    if (PE_parse_boot_argn("-nmintelfboff", nullptr, 0)) { IFBLOG("off by boot-arg"); return nullptr; }
    // One switch, default off: this framebuffer takes a laptop's panel from the firmware only when nmhybrid is 1 or 2
    { int lv = 0; const int present = PE_parse_boot_argn("nmhybrid", &lv, sizeof lv);
      fHyLevel = nmh_level_parse(present, lv);
      if (!fHyLevel) return nullptr; }
    ifbStep(NMH_STEP_PROBE);
    // The NVIDIA driver is off (removed from the boot picker, or the boot guard tripped): nothing will composite for this
    // panel, so the firmware framebuffer keeps it.
    { const int why = ifbStandDownWhy();
      if (why) { ifbStep(NMH_STEP_STOOD_DOWN); IFBLOG("hybrid: standing down, the firmware framebuffer keeps the panel: %s", nmh_stay_name(why)); return nullptr; } }
    uint16_t dev = pci->configRead16(kIOPCIConfigDeviceID);
    fVer = displayVersion(dev);
    if (!fVer) return nullptr;   // an Intel GPU this driver does not know (older ones have Apple's own driver)
    IFBLOG("8086:%04x display version %u", dev, fVer);
    return IOFramebuffer::probe(provider, score);
}

bool NMIntelFB::adoptFirmwareMode()
{
    // the panel is on whichever pipe the firmware used; take the first pipe whose transcoder and plane 1 are both on
    for (int p = 0; p < 4; p++) {
        uint32_t conf = rd(TRANSCONF(p)), ctl = rd(PLANE_CTL(p));
        if (!(conf & kEnable) || !(ctl & kEnable)) continue;
        if (fmtOf(ctl) != 8) { IFBLOG("pipe %c: plane format %u is not XRGB8888 - leaving the firmware screen", 'A' + p, fmtOf(ctl)); return false; }
        if (tileOf(ctl) != 0) { IFBLOG("pipe %c: plane is tiled (%u) - leaving the firmware screen", 'A' + p, tileOf(ctl)); return false; }
        uint32_t size = rd(PLANE_SIZE(p)), stride = rd(PLANE_STRIDE(p)) & 0x7FF;
        fStrideReg = rd(PLANE_STRIDE(p));
        fWidth = (size & 0xFFFF) + 1; fHeight = ((size >> 16) & 0xFFFF) + 1;
        fRowBytes = stride * 64;                     // linear planes count the stride in 64-byte units
        fSurf = rd(PLANE_SURF(p)) & ~0xFFFu;         // GGTT offset of the surface, 4 KiB aligned
        if (fWidth < 320 || fHeight < 200 || fRowBytes < fWidth * 4) {
            IFBLOG("pipe %c: plane %ux%u stride %u is not a usable surface", 'A' + p, fWidth, fHeight, fRowBytes); return false;
        }
        if ((uint64_t)fSurf + (uint64_t)fRowBytes * fHeight > fApertureLen) {
            IFBLOG("pipe %c: surface at 0x%x is past the %llu MiB aperture", 'A' + p, fSurf, (unsigned long long)(fApertureLen >> 20)); return false;
        }
        // the transcoder that feeds pipe p is p on these display versions (eDP on transcoder A in every recording)
        fHT = (rd(HTOTAL(p)) >> 16) + 1; fVT = (rd(VTOTAL(p)) >> 16) + 1;
        fHSS = (rd(HSYNC(p)) & 0xFFFF) + 1; fHSE = (rd(HSYNC(p)) >> 16) + 1;
        fVSS = (rd(VSYNC(p)) & 0xFFFF) + 1; fVSE = (rd(VSYNC(p)) >> 16) + 1;
        // refresh: count vblanks for 250 ms (a 30-240 Hz panel gives 7-60 counts); fall back to 60 Hz if it does not move
        uint32_t f0 = rd(FRMCOUNT(p)); uint64_t t0 = mach_absolute_time();
        IOSleep(250);
        uint32_t f1 = rd(FRMCOUNT(p)); uint64_t t1 = mach_absolute_time(), ns = 0;
        absolutetime_to_nanoseconds(t1 - t0, &ns);
        uint32_t frames = f1 - f0;
        if (frames >= 5 && frames <= 100 && ns) {
            fFrameNs = ns / frames;
            uint64_t mhz = (uint64_t)frames * 1000000000000ull / ns;   // refresh in milli-Hz
            fRefresh16 = (uint32_t)((mhz << 16) / 1000);
            fPclk = (uint64_t)fHT * fVT * mhz / 1000;
        } else fPclk = (uint64_t)fHT * fVT * 60;
        fPipe = p;
        IFBLOG("pipe %c: %ux%u stride %u surface 0x%x totals %ux%u, %u vblanks in %llu ms -> %u.%02u Hz, pclk %llu", 'A' + p,
               fWidth, fHeight, fRowBytes, fSurf, fHT, fVT, frames, (unsigned long long)(ns / 1000000),
               fRefresh16 >> 16, ((fRefresh16 & 0xFFFF) * 100) >> 16, (unsigned long long)fPclk);
        return true;
    }
    IFBLOG("no pipe is scanning out - the firmware left the panel off; leaving it to the firmware framebuffer");
    return false;
}

bool NMIntelFB::start(IOService *provider)
{
#ifdef NMINTELFB_VIRTUAL
    fWidth = 1920; fHeight = 1080; fRowBytes = 7680; fSurf = 0; fPipe = 0;
    fHT = 2200; fVT = 1125; fHSS = 2008; fHSE = 2052; fVSS = 1084; fVSE = 1089; fPclk = 148500000ull;   // CEA 1080p60
    fVBuf = IOBufferMemoryDescriptor::inTaskWithPhysicalMask(kernel_task, kIODirectionInOut | kIOMemoryPhysicallyContiguous,
                                                             (mach_vm_size_t)fRowBytes * fHeight, 0xFFFFFFFFFFFFF000ull);
    if (!fVBuf || fVBuf->prepare() != kIOReturnSuccess) { IFBLOG("virtual: no contiguous buffer"); OSSafeReleaseNULL(fVBuf); return false; }
    bzero(fVBuf->getBytesNoCopy(), fVBuf->getLength());
    fApertureBase = fVBuf->getPhysicalSegment(0, nullptr, kIOMemoryMapperNone); fApertureLen = fVBuf->getLength();
    fSurfMap = fVBuf->map();
    // Before IOFramebuffer::start, so start() attaches the shared controller (see the panel path below).
    setProperty("IOFBDependentID", 0x4E56524D00000001ull, 64);
    setProperty("IOFBDependentIndex", 4ull, 32);
    if (!IOFramebuffer::start(provider)) { OSSafeReleaseNULL(fSurfMap); fVBuf->complete(); OSSafeReleaseNULL(fVBuf); return false; }
    if (IOWorkLoop *wl = getWorkLoop()) {
        fTimer = IOTimerEventSource::timerEventSource(this, [](OSObject *o, IOTimerEventSource *t) {
            ((NMIntelFB *)o)->sample(); t->setTimeoutMS(2000); });
        if (fTimer && wl->addEventSource(fTimer) == kIOReturnSuccess) fTimer->setTimeoutMS(2000);
    }
    armVbl();
    registerPowerDriver(this, kPowerStates, 2);
    gVirt = this; sysctl_register_oid(&sysctl__debug_nmintelfb_sample);
    IFBLOG("virtual panel published (1920x1080 at phys 0x%llx)", (unsigned long long)fApertureBase);
    return true;
#endif
    fPCI = OSDynamicCast(IOPCIDevice, provider);
    if (!fPCI) return false;
    // nmhybriddelay=<s>: wait before taking the panel (some machines froze without it). Bounded at 120 s; the guard's
    // verdict is read again afterwards, because a long wait is exactly when it can change.
    { long long dv = 0; int dl = 0; const int present = PE_parse_boot_argn("nmhybriddelay", &dl, sizeof dl); dv = dl;
      const unsigned secs = nmh_delay_parse(present, dv);
      if (secs) {
          ifbStep(NMH_STEP_DELAY);
          IFBLOG("hybrid: waiting %u s before taking the panel (boot-arg nmhybriddelay)", secs);
          for (unsigned i = 0; i < secs; i++) IOSleep(1000);
          const int why = ifbStandDownWhy();
          if (why) { ifbStep(NMH_STEP_STOOD_DOWN); IFBLOG("hybrid: standing down after the wait: %s", nmh_stay_name(why)); return false; }
      } }
    fPCI->setMemoryEnable(true);
    fMMIO = fPCI->mapDeviceMemoryWithRegister(kIOPCIConfigBaseAddress0);
    IODeviceMemory *ap = fPCI->getDeviceMemoryWithRegister(kIOPCIConfigBaseAddress2);
    if (!fMMIO || !ap) { IFBLOG("BAR0 or BAR2 is not mapped - leaving the firmware screen"); OSSafeReleaseNULL(fMMIO); return false; }
    fRegs = (volatile uint8_t *)fMMIO->getVirtualAddress();
    fApertureBase = ap->getPhysicalAddress(); fApertureLen = ap->getLength();
    ifbStep(NMH_STEP_MMIO);
    if (!adoptFirmwareMode()) { OSSafeReleaseNULL(fMMIO); fRegs = nullptr; return false; }
    ifbStep(NMH_STEP_ADOPT);
    // Head 4 of the NVIDIA accelerator (NVAccel): WindowServer composites this panel on the NVIDIA card and the
    // display pipe copies each frame into fSurfMap.
    // The dependent ID and index must exist before IOFramebuffer::start. start() attaches the framebuffer to its
    // shared controller only when it finds a dependent ID; set later, the controller stays unset until open(), and the
    // power change queued by registerPowerDriver() below takes the controller's lock through a null pointer (page fault
    // in IOFramebuffer::powerStateWillChangeTo at boot). NVRMFB sets them before start for the same reason.
    // The mapping is made here, before resumeCapture, tcStart and the DMC target publish anything another thread can
    // reach, so a failure to map needs nothing undone but the register mapping.
    if (IODeviceMemory *sm = IODeviceMemory::withRange(fApertureBase + fSurf, (IOPhysicalLength)fRowBytes * fHeight)) {
        fSurfMap = sm->map(kIOMapWriteCombineCache); sm->release();
    }
    if (!fSurfMap) {
        // Without the mapping there is no head 4 and no dependent ID, so no controller either: starting would hit the
        // same null controller on the first power change. Leave the panel to the firmware framebuffer.
        IFBLOG("the surface would not map write-combined: leaving the panel to the firmware framebuffer");
        OSSafeReleaseNULL(fMMIO); fRegs = nullptr; return false;
    }
    setProperty("IOFBDependentID", 0x4E56524D00000001ull, 64);
    setProperty("IOFBDependentIndex", 4ull, 32);
    resumeCapture();
    tcStart();
    { int a = 0; fDmcOn = PE_parse_boot_argn("nmhybriddmc", &a, sizeof a) && a == 1;
      const char *want = nmd_file_for(fVer, fPCI->configRead16(kIOPCIConfigDeviceID));
      snprintf(fDmcWant, sizeof fDmcWant, "%s", want ? want : "none");
      if (!gDmcLock) { IOLock *l = IOLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void * volatile *)&gDmcLock)) IOLockFree(l); }
      if (gDmcLock) { IOLockLock(gDmcLock); gDmcTarget = this; IOLockUnlock(gDmcLock); } }
    publishBacklight();   // before IOFramebuffer::start: IOFramebuffer::open looks for a service named "backlight"
    if (!IOFramebuffer::start(provider)) {
        // resumeCapture, tcStart and the DMC target already published state other threads reach: undo all of it.
        startUnwind();
        return false;
    }
    ifbStep(NMH_STEP_FB_START);
    ifbStep(NMH_STEP_PUBLISHED);
    IFBLOG("hybrid: provider NMIntelFB %ux%u pitch %u display version %u level %d (CPU-mapped scanout, no VRAM)",
           fWidth, fHeight, fRowBytes, fVer, fHyLevel);
    setProperty("NMIntelFBHybridLevel", (unsigned long long)fHyLevel, 32);
    setProperty("NMIntelFBPipe", (unsigned long long)fPipe, 32);
    setProperty("NMIntelFBDisplayVersion", fVer, 32);
    setProperty("built-in", kOSBooleanTrue);   // the panel is the laptop's own (kConnectionFlags says so too)
    armVbl();
    registerPowerDriver(this, kPowerStates, 2);
    IFBLOG("built-in panel published (%ux%u)", fWidth, fHeight);
    return true;
}

// Undoes what start() published before IOFramebuffer::start: the DMC target and its blob, the Type-C timer and any
// external heads, the resume capture, the backlight service, and the mappings. Used by a failed start and by stop(),
// so the two cannot drift apart.
void NMIntelFB::startUnwind()
{
    fStopping = true;
    fResumeOn = false;
    if (gDmcLock) IOLockLock(gDmcLock);
    if (gDmcTarget == this) gDmcTarget = nullptr;
    if (fDmcBlob) { IOFree(fDmcBlob, fDmcLen); fDmcBlob = nullptr; }
    if (gDmcLock) IOLockUnlock(gDmcLock);
    if (fTcTimer) { thread_call_cancel(fTcTimer); while (!thread_call_free(fTcTimer)) { thread_call_cancel(fTcTimer); IOSleep(5); } fTcTimer = nullptr; }
    if (fExtOn) { nmhd_stop(&fHeads, &fHeadOps); fExtOn = false; }
    if (fResumeCall) { thread_call_cancel(fResumeCall); while (!thread_call_free(fResumeCall)) { thread_call_cancel(fResumeCall); IOSleep(5); } fResumeCall = nullptr; }
    if (fSnap) { IOFree(fSnap, sizeof *fSnap); fSnap = nullptr; }
    if (fFwPte) { IOFree(fFwPte, (size_t)(fFwPteCount * sizeof(uint64_t))); fFwPte = nullptr; }
#ifdef NMINTELFB_VIRTUAL
    if (gVirt == this) { sysctl_unregister_oid(&sysctl__debug_nmintelfb_sample); gVirt = nullptr; }
#endif
    // thread_call_cancel_wait is not exported to kexts: cancel and retry the free until the callback is out (as NVRMFB)
    if (fVblTimer) { thread_call_cancel(fVblTimer); while (!thread_call_free(fVblTimer)) { thread_call_cancel(fVblTimer); IOSleep(5); } fVblTimer = nullptr; }
    if (fBacklight) { fBacklight->terminate(kIOServiceSynchronous); fBacklight->detach(this); OSSafeReleaseNULL(fBacklight); }
    fPwmPeriod = 0;
    OSSafeReleaseNULL(fSurfMap); OSSafeReleaseNULL(fMMIO); fRegs = nullptr;
}

void NMIntelFB::stop(IOService *provider)
{
    fStopping = true;
    if (fHyReady && fRing.live != -1) hybridHome();
    if (fOwnReady && nmh_own_may_free(&fOwn)) ownRelease();
    startUnwind();
    IOFramebuffer::stop(provider);
}

IODeviceMemory *NMIntelFB::getApertureRange(IOPixelAperture aperture)
{
    if (aperture != kIOFBSystemAperture) return nullptr;
    return IODeviceMemory::withRange(fApertureBase + fSurf, (IOPhysicalLength)fRowBytes * fHeight);
}

IOReturn NMIntelFB::getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info)
{
    if (mode != 1 || !info) return kIOReturnBadArgument;
    bzero(info, sizeof(*info));
    info->maxDepthIndex = 0; info->nominalWidth = fWidth; info->nominalHeight = fHeight;
    info->refreshRate = fRefresh16; info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
    return kIOReturnSuccess;
}

IOReturn NMIntelFB::getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi)
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

OSDefineMetaClassAndStructors(NMIntelBacklight, IODisplayParameterHandler)

NMIntelBacklight *NMIntelBacklight::withFB(NMIntelFB *fb, SInt32 level)
{
    NMIntelBacklight *h = OSTypeAlloc(NMIntelBacklight);
    if (!h || !h->init()) { OSSafeReleaseNULL(h); return nullptr; }
    h->fFB = fb;   // not retained: the framebuffer owns this handler and terminates it in stop()
    h->fApply = thread_call_allocate(applyFire, h);
    h->fLevel = level < 0 ? 0 : level > 100 ? 100 : level;
    if (!h->fApply) { h->release(); return nullptr; }
    return h;
}

void NMIntelBacklight::free()
{
    if (fApply) { thread_call_cancel(fApply); while (!thread_call_free(fApply)) { thread_call_cancel(fApply); IOSleep(1); } fApply = nullptr; }
    IODisplayParameterHandler::free();
}

// The display may have no IODisplayParameters yet; IODisplay::setProperties() only accepts parameters listed there.
bool NMIntelBacklight::setDisplay(IODisplay *display)
{
    if (!display) return false;
    OSObject *old = display->copyProperty(gIODisplayParametersKey);
    OSDictionary *cur = OSDynamicCast(OSDictionary, old);
    OSDictionary *params = cur ? OSDictionary::withDictionary(cur) : OSDictionary::withCapacity(2);
    OSSafeReleaseNULL(old);
    if (!params) return false;
    IODisplay::addParameter(params, gIODisplayBrightnessKey, 0, 100);
    IODisplay::setParameter(params, gIODisplayBrightnessKey, fLevel);
    IODisplay::addParameter(params, gIODisplayLinearBrightnessKey, 0, 100);
    IODisplay::setParameter(params, gIODisplayLinearBrightnessKey, fLevel);
    display->setProperty(gIODisplayParametersKey, params);
    params->release();
    IFBLOG("backlight: brightness parameters on %s, panel at %d%%", display->getName(), (int)fLevel);
    return true;
}

// IODisplay calls this with the framebuffer lock held, so the register write runs from a thread call.
bool NMIntelBacklight::doIntegerSet(OSDictionary *, const OSSymbol *paramName, UInt32 value)
{
    if (paramName != gIODisplayBrightnessKey && paramName != gIODisplayLinearBrightnessKey) return false;
    fPending = value > 100 ? 100 : (SInt32)value;
    thread_call_enter(fApply);
    return true;
}

void NMIntelBacklight::applyFire(thread_call_param_t p, thread_call_param_t)
{
    NMIntelBacklight *me = (NMIntelBacklight *)p;
    SInt32 v;
    do { v = me->fPending; } while (!OSCompareAndSwap((UInt32)v, (UInt32)-1, (volatile UInt32 *)&me->fPending));
    if (v < 0) return;
    uint32_t percent = (uint32_t)(v < kIntelMinBacklight ? kIntelMinBacklight : v);
    if (me->fFB->setPanelPercent(percent)) me->fLevel = v;
    else IFBLOG("backlight: setting %u%% did not stick", percent);
}

bool NMIntelFB::setPanelPercent(uint32_t percent)
{
    if (!fRegs || !fPwmPeriod || fStopping) return false;
    uint32_t duty = (uint32_t)((uint64_t)fPwmPeriod * percent / 100);
    wr(kBlcPwmDuty, duty);
    return rd(kBlcPwmDuty) == duty;
}

// Brightness only when the firmware enabled the PWM with a usable period; otherwise the panel keeps the level it has.
void NMIntelFB::publishBacklight()
{
    uint32_t ctl = rd(kBlcPwmCtl), period = rd(kBlcPwmFreq), duty = rd(kBlcPwmDuty);
    if (ctl == ~0u || period == ~0u || !(ctl & kBlcPwmEnable) || !period || duty > period) {
        IFBLOG("backlight: PWM ctl 0x%08x period %u duty %u - not enabled by the firmware, no brightness control", ctl, period, duty);
        return;
    }
    fPwmPeriod = period;
    SInt32 level = (SInt32)((uint64_t)duty * 100 / period);
    fBacklight = NMIntelBacklight::withFB(this, level);
    if (!fBacklight) { fPwmPeriod = 0; IFBLOG("backlight: handler allocation failed"); return; }
    fBacklight->setName("backlight");
    if (!fBacklight->attach(this)) { OSSafeReleaseNULL(fBacklight); fPwmPeriod = 0; IFBLOG("backlight: handler attach failed"); return; }
    fBacklight->registerService(kIOServiceSynchronous);
    setProperty("NMIntelFBPwmPeriod", period, 32);
    IFBLOG("backlight: PWM period %u, panel at %d%% - brightness keys and slider through AppleBacklightDisplay", period, (int)level);
}

// AppleBacklightDisplay::probe takes only an LCD connection; without a backlight handler the panel stays a plain display.
IOReturn NMIntelFB::getAppleSense(IOIndex connectIndex, UInt32 *senseType, UInt32 *primary, UInt32 *extended, UInt32 *displayType)
{
    if (connectIndex != 0 || !fBacklight) return kIOReturnUnsupported;
    if (senseType) *senseType = 0;
    if (primary) *primary = 0;
    if (extended) *extended = 0;
    if (displayType) *displayType = kGenericLCD;
    return kIOReturnSuccess;
}

IOReturn NMIntelFB::getAttributeForConnection(IOIndex idx, IOSelect attr, uintptr_t *value)
{
    if (idx != 0 || !value) return kIOReturnBadArgument;
    switch (attr) {
    case kConnectionEnable: *value = 1; return kIOReturnSuccess;
    case kConnectionCheckEnable: *value = 1; return kIOReturnSuccess;
    case kConnectionFlags: *value = kIOConnectionBuiltIn; return kIOReturnSuccess;
    // the colour answers NVRMFB gives (RGB, 8 bits per component), which WindowServer asks before compositing
    case kConnectionColorModesSupported: case kConnectionColorMode: *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionColorDepthsSupported: case kConnectionControllerColorDepth: case kConnectionControllerDepthsSupported:
        *value = 0x00000002; return kIOReturnSuccess;
    default: return IOFramebuffer::getAttributeForConnection(idx, attr, value);
    }
}


// ---- hybrid level 2: scan out the composited surface in place ----------------------------------------------------------
// Layout and registers from the Linux i915 driver (MIT): display versions 12 and 13 keep the graphics table in the upper
// half of BAR0 (one 64-bit entry per 4 KiB page, bit 0 present, bits 45:12 the page address), its size in GMCH_CTRL
// (config 0x50, bits 7:6, in MiB as a power of two). Entries are written only inside a range that already reads as one
// repeated value (the unused scratch or zero entries); the firmware's own surface and every other mapping are left alone.
// the graphics translation table: upper half of BAR0, size from GMCH_CTRL (display versions 12 and 13)
bool NMIntelFB::ggttMap()
{
    if (fGsm) return true;
    if (!nmh_gen_ok(fVer)) return false;
    const uint64_t len = fMMIO ? fMMIO->getLength() : 0;
    if (len < (16ull << 20)) { IFBLOG("hybrid: BAR0 is %llu MiB, the table is expected in the upper half of 16 MiB", (unsigned long long)(len >> 20)); return false; }
    const unsigned ggms = (fPCI->configRead16(0x50) >> 6) & 3;
    const uint64_t gsmBytes = ggms ? ((uint64_t)1 << ggms) << 20 : 0;
    if (!gsmBytes || gsmBytes > len / 2) { IFBLOG("hybrid: graphics table size field %u does not fit BAR0", ggms); return false; }
    fGsm = (volatile uint64_t *)(fRegs + len / 2);
    fGsmEntries = gsmBytes / 8;
    return true;
}

bool NMIntelFB::hybridSetup(uint32_t pitch, uint32_t height)
{
    if (fHyReady) return true;
    if (fHyBad) return false;
#define HYFAIL(...) do { fHyBad = true; IFBLOG("hybrid: import unavailable: " __VA_ARGS__); return false; } while (0)
    if (!nmh_gen_ok(fVer)) HYFAIL("display version %u", fVer);
    if (!ggttMap()) HYFAIL("the graphics table is not reachable");
    const uint64_t slotPages = nmh_pages_for(pitch, height), need = slotPages * (NMH_SLOTS + (fHyLevel >= NMH_OWNED ? NMH_OWN_BUFS : 0));
    const uint64_t top = fGsmEntries > 512 ? fGsmEntries - 512 : 0;      // keep clear of the top 2 MiB
    const uint64_t fwLo = fSurf >> 12, fwHi = (((uint64_t)fSurf + (uint64_t)fRowBytes * fHeight) - 1) >> 12;
    const int64_t firstS = nmh_find_free_run(ggttRead, (void *)fGsm, top, 65536, need, fwLo, fwHi);
    if (firstS < 0) HYFAIL("no run of %llu unused entries in the top 256 MiB of %llu entries", (unsigned long long)need, (unsigned long long)fGsmEntries);
    const uint64_t first = (uint64_t)firstS;
    if ((first + need) * NMH_PAGE > (1ull << 32)) HYFAIL("range ends past the 32-bit plane address");
    nmh_ring_init(&fRing, first * NMH_PAGE, slotPages * NMH_PAGE);
    ifbStep(NMH_STEP_RING);
    fHyPitch = pitch; fHyHeight = height; fHyReady = true;
    IFBLOG("hybrid: import ring %d slots x %llu KiB at table offset 0x%llx (entries %lld..%lld of %llu)", NMH_SLOTS,
           (unsigned long long)(slotPages * 4), (unsigned long long)fRing.base, (long long)first, (long long)(first + need - 1),
           (unsigned long long)fGsmEntries);
    return true;
#undef HYFAIL
}

void NMIntelFB::hybridWriteSurf(uint32_t surf, uint32_t strideReg)
{
    wr(PLANE_STRIDE(fPipe), strideReg);
    wr(PLANE_SURF(fPipe), surf);        // writing the surface register arms the update for the next vertical blank
    (void)rd(PLANE_SURF(fPipe));
}

// polls the live register for up to 40 ms (about two frames at 60 Hz); the flip lands at the next vertical blank
bool NMIntelFB::hybridWaitLive(int slot)
{
    for (int i = 0; i < 160; i++) {
        const int rc = nmh_flip_poll(&fRing, rd(PLANE_SURFLIVE(fPipe)), fSurf, 160);
        if (rc == 1) return true;
        if (rc < 0) break;
        IODelay(250);
    }
    IFBLOG("hybrid: flip to %s did not reach the screen in 40 ms", slot < 0 ? "the boot surface" : "an import slot");
    nmh_flip_abandon(&fRing);
    return false;
}

void NMIntelFB::hybridHome()
{
    if (!nmh_need_home(&fRing)) return;
    if (fRing.pending != NMH_NONE) nmh_flip_abandon(&fRing);
    fRing.pending = NMH_NONE;
    if (nmh_flip_begin(&fRing, -1) != NMH_OK) return;
    hybridWriteSurf(fSurf, fStrideReg);
    if (hybridWaitLive(-1)) IFBLOG("hybrid: plane back on the boot surface");
    if (fOwnReady) nmh_own_home(&fOwn);
}

// Three buffers of ordinary cached memory, mapped into the graphics table after the import slots, same size as the
// firmware's surface so the plane's stride never changes. The CPU writes them (fast, no write-combined stores) and the
// panel driver flushes a buffer from the CPU caches before the plane is flipped to it.
bool NMIntelFB::ownSetup()
{
    if (fOwnReady) return true;
    if (fHyLevel < NMH_OWNED || !hybridSetup(fRowBytes, fHeight)) return false;
    if (nmh_pages_for(fRowBytes, fHeight) * NMH_PAGE != fRing.slot_bytes) { IFBLOG("hybrid: owned buffers unavailable: ring slots were sized for another pitch"); return false; }
    const uint64_t pages = fRing.slot_bytes / NMH_PAGE;
    for (int i = 0; i < NMH_OWN_BUFS; i++) {
        IOBufferMemoryDescriptor *b = IOBufferMemoryDescriptor::inTaskWithPhysicalMask(kernel_task, kIODirectionInOut, (mach_vm_size_t)fRing.slot_bytes, 0xFFFFFFFFFFFFF000ull);
        if (!b || b->prepare() != kIOReturnSuccess) { OSSafeReleaseNULL(b); ownRelease(); IFBLOG("hybrid: owned buffers unavailable: allocation failed"); return false; }
        bzero(b->getBytesNoCopy(), b->getLength());
        fOwnMd[i] = b;
        fOwnPages[i] = (unsigned long long *)IOMalloc((size_t)(pages * sizeof(unsigned long long)));
        if (!fOwnPages[i]) { ownRelease(); IFBLOG("hybrid: owned buffers unavailable: no memory for the page list"); return false; }
        fOwnPageCount = (uint32_t)pages;
        const uint64_t at = fRing.base / NMH_PAGE + (uint64_t)(NMH_SLOTS + i) * pages;
        for (uint64_t p = 0; p < pages; p++) {
            addr64_t a = b->getPhysicalSegment((IOByteCount)(p * NMH_PAGE), nullptr, kIOMemoryMapperNone);
            if (!nmh_ggtt_pte(a)) { ownRelease(); IFBLOG("hybrid: owned buffers unavailable: a page is out of the table's reach"); return false; }
            fGsm[at + p] = nmh_ggtt_pte(a);
            fOwnPages[i][p] = a;
        }
    }
    (void)fGsm[fRing.base / NMH_PAGE];
    wr(kGfxFlshCntl, 1);
    nmh_own_init(&fOwn);
    fOwnReady = true;
    IFBLOG("hybrid: %d owned buffers x %llu KiB mapped after the import slots", NMH_OWN_BUFS, (unsigned long long)(fRing.slot_bytes / 1024));
    return true;
}
void NMIntelFB::ownRelease()
{
    for (int i = 0; i < NMH_OWN_BUFS; i++) {
        if (fOwnMd[i]) { fOwnMd[i]->complete(); fOwnMd[i]->release(); fOwnMd[i] = nullptr; }
        if (fOwnPages[i]) { IOFree(fOwnPages[i], (size_t)(fOwnPageCount * sizeof(unsigned long long))); fOwnPages[i] = nullptr; }
    }
    fOwnReady = false;
}

// ---- sleep and wake ----------------------------------------------------------------------------------------------------

// The video BIOS tables through the OpRegion (PCI config 0xFC holds its physical address): the panel type, and from it whether
// this panel uses the low-swing or reduced-power drive table. Nothing is guessed: any failure leaves the DisplayPort tables.
bool NMIntelFB::readVbt(struct nmr_edp *out, uint8_t **keep, uint32_t *keepLen)
{
    *keep = nullptr; *keepLen = 0;
    const uint32_t asls = fPCI->configRead32(0xFC);
    if (!asls) return false;
    IOMemoryDescriptor *od = IOMemoryDescriptor::withPhysicalAddress(asls, NMV_OPREGION_SIZE, kIODirectionIn);
    IOMemoryMap *om = od ? od->map() : nullptr;
    bool ok = false;
    if (om) {
        const uint8_t *op = (const uint8_t *)om->getVirtualAddress();
        uint32_t io = 0, is = 0; struct nmv_loc loc; struct nmv_edp e;
        if (nmv_locate(op, asls, &io, &is, &loc)) {
            if (loc.kind == 1) {
                ok = nmv_edp(op + io, is, nullptr, &e);
                if (!ok && is <= 65536 && (*keep = (uint8_t *)IOMalloc(is))) { memcpy(*keep, op + io, is); *keepLen = is; }
            }
            else if (loc.kind == 2) {
                IOMemoryDescriptor *vd = IOMemoryDescriptor::withPhysicalAddress((IOPhysicalAddress)loc.phys, loc.size, kIODirectionIn);
                IOMemoryMap *vm = vd ? vd->map() : nullptr;
                if (vm) {
                    const uint8_t *vb = (const uint8_t *)vm->getVirtualAddress();
                    ok = nmv_edp(vb, loc.size, nullptr, &e);
                    if (!ok && loc.size <= 65536 && (*keep = (uint8_t *)IOMalloc(loc.size))) { memcpy(*keep, vb, loc.size); *keepLen = loc.size; }
                }
                if (vm) vm->release();
                if (vd) vd->release();
            }
        }
        if (ok) { out->known = 1; out->low_vswing = (uint8_t)e.low_vswing; out->hobl = (uint8_t)e.hobl;
                  IFBLOG("hybrid: video BIOS: panel type %u, low-swing table %s, reduced-power table %s (VBT %u, BDB %u)", e.panel_type,
                         e.low_vswing ? "yes" : "no", e.hobl ? "yes" : "no", e.vbt_version, e.bdb_version); }
        else IFBLOG("hybrid: video BIOS: no panel type named; trying the panel's identifier");
        om->release();
    }
    if (od) od->release();
    return ok;
}

// ---- display firmware
int NMIntelFB::dmcAccept(const uint8_t *buf, uint32_t len)
{
    struct nmd_fw fw;
    if (!fRegs || fVer < 12 || fVer > 14) return ENXIO;
    const struct nmd_step stp = dmcStep();
    const int rc = nmd_parse(buf, len, stp.st, stp.sub, nmd_max_words(fVer), &fw);
    if (rc != NMD_OK) { IFBLOG("hybrid: display firmware refused: %s", nmd_err(rc)); return EINVAL; }
    uint8_t *copy = (uint8_t *)IOMalloc(len);
    if (!copy) return ENOMEM;
    memcpy(copy, buf, len);
    if (fDmcBlob) IOFree(fDmcBlob, fDmcLen);
    fDmcBlob = copy; fDmcLen = len;
    IFBLOG("hybrid: display firmware accepted for stepping %c%c: %u words for the main program, version %u.%u%s", stp.st, stp.sub, fw.part[0].words, fw.version >> 16, fw.version & 0xFFFF,
           fDmcOn ? "" : " (not loaded: boot-arg nmhybriddmc is not 1)");
    if (fDmcOn) dmcApplyLocked();
    return 0;
}
// the display stepping that picks the package entry (Linux intel_step.c): the PCI revision id through the sub-platform's table,
// or the display step register on version 14
struct nmd_step NMIntelFB::dmcStep() const
{
    const uint32_t gmd = fVer == 14 ? rd(0x510A0u) : 0;
    return nmd_stepping(fVer, fPCI->configRead16(kIOPCIConfigDeviceID), fPCI->configRead8(kIOPCIConfigRevisionID), gmd);
}
void NMIntelFB::dmcApply()
{
    if (!gDmcLock) return;
    IOLockLock(gDmcLock);
    if (!fStopping) dmcApplyLocked();
    IOLockUnlock(gDmcLock);
}
void NMIntelFB::dmcApplyLocked()
{
    struct nmd_fw fw;
    const struct nmd_step stp = dmcStep();
    if (!fDmcBlob || nmd_parse(fDmcBlob, fDmcLen, stp.st, stp.sub, nmd_max_words(fVer), &fw) != NMD_OK) return;
    const uint32_t cap = 1u << 15;
    struct nmd_write *w = (struct nmd_write *)IOMalloc(cap * sizeof *w);
    if (!w) return;
    const uint32_t n = nmd_load_plan(&fw, fVer, w, cap);
    if (n) { for (uint32_t i = 0; i < n; i++) wr(w[i].reg, w[i].val); IFBLOG("hybrid: display firmware loaded: %u register writes, every event handler left disabled", n); }
    else IFBLOG("hybrid: display firmware not loaded: the load plan did not fit");
    IOFree(w, cap * sizeof *w);
}

void NMIntelFB::resumeCapture()
{
    long long v = 0; int a = 0; const int present = PE_parse_boot_argn("nmhybridresume", &a, sizeof a); v = a;
    if (!nmr_enabled(present, v)) return;
    // Lunar Lake and Panther Lake (display 20 and 30, nm_hybrid_resume14.h, the C20 PHY): ported from Linux's xe display code, never run on
    // hardware, so they stay behind their own boot-arg.
    const bool xe2 = fVer == 20 || fVer >= 30;
    if (xe2) {
        int a20 = 0;
        if (!PE_parse_boot_argn("nmhybrid20", &a20, sizeof a20) || a20 != 1) { IFBLOG("hybrid: resume unavailable: display version %u needs the boot-arg nmhybrid20=1 (not yet run on hardware)", fVer); return; }
    }
    if (!((fVer >= 12 && fVer <= 14) || xe2) || fPipe < 0 || fPipe > 3) { IFBLOG("hybrid: resume unavailable: display version %u pipe %d (versions 12 to 14, 20 and 30, pipes A to D)", fVer, fPipe); return; }
    if (fVer <= 13 && !ggttMap()) return;
    fSnap = (struct nmr_snap *)IOMalloc(sizeof *fSnap);
    if (!fSnap) return;
    bzero(fSnap, sizeof *fSnap);
    struct nmr_ops ops = { this,
        [](void *c, uint32_t o) { return ((NMIntelFB *)c)->rd(o); },
        [](void *c, uint32_t o, uint32_t x) { ((NMIntelFB *)c)->wr(o, x); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *st) { IFBLOG("hybrid: resume step: %s", st); },
        nullptr };
    struct nmr_edp edp = {};
    uint8_t *vbtKeep = nullptr; uint32_t vbtKeepLen = 0;
    readVbt(&edp, &vbtKeep, &vbtKeepLen);
    const int rc = fVer >= 14 ? nmr20_snapshot(&ops, fSnap, (unsigned)fPipe, &edp, fVer) : nmr_snapshot(&ops, fSnap, (unsigned)fPipe, fVer, &edp);
    if (rc != NMR_OK) { IFBLOG("hybrid: resume unavailable: %s", nmr_result_name(rc)); IOFree(fSnap, sizeof *fSnap); fSnap = nullptr; if (vbtKeep) IOFree(vbtKeep, vbtKeepLen); return; }
    if (vbtKeep) {
        // the video BIOS named no panel type: the panel's own identifier picks the entry (Linux pnpid_get_panel_type)
        uint8_t id[10]; struct nmv_edp e; struct nmr_ops o2 = ops; bool got;
        if (fVer >= 14) { o2.aux_ctl_or = NMX_AUX_POWER_REQ; nmx_aux_power(&ops, fSnap->topo.port, 1); }
        got = nmr_edid_id(&o2, fSnap->topo.port, id) == 0;
        if (fVer >= 14) nmx_aux_power(&ops, fSnap->topo.port, 0);
        if (got && nmv_edp(vbtKeep, vbtKeepLen, id, &e)) {
            fSnap->edp.known = 1; fSnap->edp.low_vswing = (uint8_t)e.low_vswing; fSnap->edp.hobl = (uint8_t)e.hobl;
            IFBLOG("hybrid: video BIOS: panel type %u by the panel's identifier, low-swing table %s, reduced-power table %s", e.panel_type, e.low_vswing ? "yes" : "no", e.hobl ? "yes" : "no");
        } else IFBLOG("hybrid: video BIOS: the panel's identifier matched no entry; the DisplayPort drive tables are used");
        IOFree(vbtKeep, vbtKeepLen);
    }
    // the firmware surface's entries in the graphics table: restored first on wake (versions 12 and 13; on version 14 the table
    // sits in system memory and is not touched)
    fFwPteFirst = fSurf >> 12; fFwPteCount = (((uint64_t)fRowBytes * fHeight) + 4095) / 4096;
    if (fVer <= 13 && fFwPteFirst + fFwPteCount <= fGsmEntries) {
        fFwPte = (uint64_t *)IOMalloc((size_t)(fFwPteCount * sizeof(uint64_t)));
        if (fFwPte) for (uint64_t i = 0; i < fFwPteCount; i++) fFwPte[i] = fGsm[fFwPteFirst + i];
    }
    fResumeCall = thread_call_allocate(resumeFire, this);
    fResumeOn = fResumeCall != nullptr;
    IFBLOG("hybrid: resume armed: panel state captured (pipe %c, DDI %c, sink link 0x%02x x%u, %s)", 'A' + fSnap->topo.pipe, 'A' + fSnap->topo.port, fSnap->dpcd[0], fSnap->dpcd[1] & 0x1F,
           fSnap->no_handshake ? "no AUX handshake" : "AUX link training");
}

struct NMIntelFB::ExtObj { IOBufferMemoryDescriptor *md = nullptr; uint64_t first = 0, count = 0; struct nmo m; unsigned pipe = 0; };
int NMIntelFB::extDevOpen(void *c) { NMIntelFB *me = (NMIntelFB *)c; return me->fRegs && me->fGsm ? 0 : -1; }
void NMIntelFB::extDevClose(void *) {}
void *NMIntelFB::extCreate(void *c, unsigned slot, unsigned)
{
    NMIntelFB *me = (NMIntelFB *)c;
    if (slot >= NMHD_MAX) return nullptr;
    ExtObj *x = (ExtObj *)IOMalloc(sizeof(ExtObj));
    if (!x) return nullptr;
    bzero(x, sizeof *x);
    const size_t bytes = (size_t)1920 * 1080 * 4;
    x->md = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task, kIODirectionInOut, bytes, 4096);
    if (!x->md || x->md->prepare() != kIOReturnSuccess) { OSSafeReleaseNULL(x->md); IOFree(x, sizeof *x); return nullptr; }
    bzero(x->md->getBytesNoCopy(), bytes);
    me->fExtObj[slot] = x;
    return x;
}
int NMIntelFB::extBringUp(void *c, void *o, unsigned port)
{
    NMIntelFB *me = (NMIntelFB *)c; ExtObj *x = (ExtObj *)o;
    const size_t bytes = (size_t)x->md->getLength();
    const uint64_t pages = (bytes + 4095) / 4096;
    // a pipe other than the panel's and the other external heads'
    unsigned pipe = 4;
    for (unsigned p = 0; p < 4; p++) if ((int)p != me->fPipe && !(me->fExtPipes & (1u << p))) { pipe = p; break; }
    if (pipe == 4) return -1;
    // a run of unused graphics-table entries below the import ring, clear of the firmware surface
    const uint64_t top0 = me->fGsmEntries > 512 ? me->fGsmEntries - 512 : 0;
    const uint64_t top = (me->fHyReady && me->fRing.base / NMH_PAGE < top0) ? me->fRing.base / NMH_PAGE : top0;
    const uint64_t fwLo = me->fSurf >> 12, fwHi = (((uint64_t)me->fSurf + (uint64_t)me->fRowBytes * me->fHeight) - 1) >> 12;
    const int64_t first = nmh_find_free_run(ggttRead, (void *)me->fGsm, top, 65536, pages, fwLo, fwHi);
    if (first < 0 || ((uint64_t)first + pages) * NMH_PAGE > (1ull << 32)) { IFBLOG("hybrid: external head: no free run of %llu graphics-table entries", (unsigned long long)pages); return -1; }
    for (uint64_t i = 0; i < pages; i++) {
        IOByteCount seg = 0;
        const addr64_t a = x->md->getPhysicalSegment((IOByteCount)i * 4096, &seg, kIOMemoryMapperNone);
        const uint64_t pte = (a && seg >= 4096) ? nmh_ggtt_pte(a) : 0;
        if (!pte) { for (uint64_t k = 0; k < i; k++) me->fGsm[(uint64_t)first + k] = me->fExtScratch; return -1; }
        me->fGsm[(uint64_t)first + i] = pte;
    }
    (void)me->fGsm[(uint64_t)first + pages - 1];
    me->wr(kGfxFlshCntl, 1);
    x->first = (uint64_t)first; x->count = pages; x->pipe = pipe;
    struct nmr_ops ops = { me,
        [](void *cc, uint32_t off) { return ((NMIntelFB *)cc)->rd(off); },
        [](void *cc, uint32_t off, uint32_t v) { ((NMIntelFB *)cc)->wr(off, v); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *st) { IFBLOG("hybrid: %s", st); },
        nullptr };
    const uint32_t ref = (me->rd(NMR_DSSM) >> 29) & 7u;
    const unsigned refclk = ref == 0 ? 24000u : (ref == 1 ? 19200u : 38400u);
    unsigned ds = 0, de = 0;
    { const uint32_t pc = me->rd(0x7027Cu + 0x1000u * (unsigned)me->fPipe); ds = pc & 0xFFFu; de = ((pc >> 16) & 0xFFFu) + 1u; }
    unsigned fs, fe; nmw_free_run(nmw_dbuf_total(me->fVer), ds, de, &fs, &fe);
    if (nmo_init(&ops, &x->m, me->fVer, port, pipe, refclk)) return -1;
    const int rc = nmo_enable(&ops, &x->m, (uint32_t)(x->first * NMH_PAGE), (uint32_t)bytes, fs, fe);
    if (rc) { IFBLOG("hybrid: external head on Type-C port %u failed: %s", port + 1, nmo_err_name(rc)); for (uint64_t k = 0; k < pages; k++) me->fGsm[(uint64_t)first + k] = me->fExtScratch; me->wr(kGfxFlshCntl, 1); return -1; }
    // colour bars in the buffer, then out of the CPU caches for the display engine
    { uint32_t *px = (uint32_t *)x->md->getBytesNoCopy(); static const uint32_t bars[8] = { 0xFFFFFF, 0xFFFF00, 0x00FFFF, 0x00FF00, 0xFF00FF, 0xFF0000, 0x0000FF, 0x000000 };
      const unsigned w = x->m.t.hactive, h = x->m.t.vactive, pitch = x->m.pitch / 4;
      for (unsigned yy = 0; yy < h; yy++) for (unsigned xx = 0; xx < w; xx++) px[(size_t)yy * pitch + xx] = bars[xx * 8 / w];
      x->md->performOperation(kIOMemoryIncoherentIOFlush, 0, (IOByteCount)bytes); }
    me->fExtPipes |= 1u << pipe;
    IFBLOG("hybrid: external head on Type-C port %u is up: %ux%u, %u lanes at %u Gb/s per lane x100, pipe %c", port + 1, x->m.t.hactive, x->m.t.vactive, x->m.lanes, x->m.rate / 10000u, 'A' + pipe);
    return 0;
}
int NMIntelFB::extPublish(void *, void *, unsigned slot) { IFBLOG("hybrid: external head %u shows colour bars (it is not published to the window system)", slot); return 0; }
void NMIntelFB::extUnpublish(void *, void *) {}
void NMIntelFB::extTakeDown(void *c, void *o, unsigned)
{
    NMIntelFB *me = (NMIntelFB *)c; ExtObj *x = (ExtObj *)o;
    struct nmr_ops ops = { me,
        [](void *cc, uint32_t off) { return ((NMIntelFB *)cc)->rd(off); },
        [](void *cc, uint32_t off, uint32_t v) { ((NMIntelFB *)cc)->wr(off, v); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *st) { IFBLOG("hybrid: %s", st); },
        nullptr };
    nmo_teardown(&ops, &x->m);
    if (x->count) { for (uint64_t k = 0; k < x->count; k++) me->fGsm[x->first + k] = me->fExtScratch; me->wr(kGfxFlshCntl, 1); x->count = 0; }
    me->fExtPipes &= ~(1u << x->pipe);
}
void NMIntelFB::extRelease(void *c, void *o)
{
    NMIntelFB *me = (NMIntelFB *)c; ExtObj *x = (ExtObj *)o;
    for (unsigned i = 0; i < NMHD_MAX; i++) if (me->fExtObj[i] == x) me->fExtObj[i] = nullptr;
    if (x->md) { x->md->complete(); OSSafeReleaseNULL(x->md); }
    IOFree(x, sizeof *x);
}

static const char *tcModeName(unsigned m) { return m == NMT_DPALT ? "DisplayPort-alt" : m == NMT_TBT ? "Thunderbolt-alt" : m == NMT_LEGACY ? "legacy" : "disconnected"; }
void NMIntelFB::tcFire(thread_call_param_t p0, thread_call_param_t)
{
    NMIntelFB *me = (NMIntelFB *)p0;
    if (me->fStopping) return;
    struct nmr_ops ops = { me,
        [](void *c, uint32_t o) { return ((NMIntelFB *)c)->rd(o); },
        [](void *c, uint32_t o, uint32_t x) { ((NMIntelFB *)c)->wr(o, x); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *) {},
        nullptr };
    for (unsigned i = 0; i < me->fTcN; i++) {
        const int ev = nmt_poll(&ops, &me->fTcPort[i], &me->fTcLast[i]);
        if (ev) IFBLOG("hybrid: Type-C port %u: %s, live mode %s", i + 1, ev == 1 ? "cable plugged" : ev == 2 ? "cable unplugged" : "cable changed",
                       tcModeName(nmt_mask_to_mode(me->fTcLast[i])));
        if (me->fExtOn && !me->fStopping) {
            if (ev == 1) { const int r = nmhd_plug(&me->fHeads, &me->fHeadOps, i); IFBLOG("hybrid: external head on Type-C port %u: %s", i + 1, r == 0 ? "up" : r == 1 ? "waiting for a teardown" : "not brought up"); }
            else if (ev == 2) nmhd_unplug(&me->fHeads, &me->fHeadOps, i);
            else if (ev == 3) { nmhd_unplug(&me->fHeads, &me->fHeadOps, i); if (me->fTcLast[i]) nmhd_plug(&me->fHeads, &me->fHeadOps, i); }
        }
    }
    uint64_t d; clock_interval_to_deadline(2000, kMillisecondScale, &d);
    if (!me->fStopping) thread_call_enter_delayed(me->fTcTimer, d);
}
void NMIntelFB::tcStart()
{
    int a = 0;
    int e = 0;
    const bool ext = PE_parse_boot_argn("nmhybridext", &e, sizeof e) && e == 1;
    if (!ext && (!PE_parse_boot_argn("nmhybridtc", &a, sizeof a) || a != 1)) return;
    if (fVer != 12 && fVer != 13) { IFBLOG("hybrid: Type-C monitor: display version %u is not supported (versions 12 and 13)", fVer); return; }
    struct nmr_ops ops = { this,
        [](void *c, uint32_t o) { return ((NMIntelFB *)c)->rd(o); },
        [](void *c, uint32_t o, uint32_t x) { ((NMIntelFB *)c)->wr(o, x); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *) {},
        nullptr };
    for (unsigned i = 0; i < 4; i++) {
        if (nmt_init(&ops, &fTcPort[fTcN], fVer, i, 0) == 0) { fTcLast[fTcN] = 0; fTcN++; }
        else IFBLOG("hybrid: Type-C monitor: port %u unavailable (PHY in TC-cold or the platform controller did not answer)", i + 1);
    }
    if (!fTcN) return;
    if (ext) {
        if (fPipe < 0 || fPipe > 3 || !fRegs || !ggttMap()) { IFBLOG("hybrid: external heads unavailable (pipe %d, graphics table %s)", fPipe, fGsm ? "ok" : "not reachable"); }
        else {
            nmhd_init(&fHeads);
            fHeadOps = (struct nmhd_ops){ this, extDevOpen, extDevClose, extCreate, extBringUp, extPublish, extUnpublish, extTakeDown, extRelease };
            fExtScratch = fGsm[fGsmEntries - 1];
            if (nmhd_panel_up(&fHeads, &fHeadOps) == 0) fExtOn = true;
        }
    }
    fTcTimer = thread_call_allocate(tcFire, this);
    if (!fTcTimer) { fExtOn = false; return; }
    IFBLOG("hybrid: Type-C monitor armed for %u ports (%s)", fTcN, fExtOn ? "external heads on: colour bars on a second pipe, not published to the window system" : "observe only");
    thread_call_enter(fTcTimer);
}

void NMIntelFB::resumeRun()
{
    if (!fResumeOn || !fSnap || fStopping) return;
    ifbStep(NMH_STEP_RESUME_BEGIN);
    if (fDmcOn) dmcApply();     // first, as Linux does: before any power well or display clock is brought back
    if (fFwPte) { for (uint64_t i = 0; i < fFwPteCount; i++) fGsm[fFwPteFirst + i] = fFwPte[i]; wr(kGfxFlshCntl, 1); }
    struct nmr_ops ops = { this,
        [](void *c, uint32_t o) { return ((NMIntelFB *)c)->rd(o); },
        [](void *c, uint32_t o, uint32_t x) { ((NMIntelFB *)c)->wr(o, x); },
        [](void *, unsigned us) { IODelay(us); },
        [](void *, const char *st) { IFBLOG("hybrid: resume step: %s", st); },
        nullptr };      // swing and pre-emphasis levels are programmed by nmr_drive_apply (nm_hybrid_resume.h)
    const int rc = fVer >= 14 ? nmr20_resume(&ops, fSnap) : nmr_resume(&ops, fSnap, fVer);
    IFBLOG("hybrid: resume: %s", nmr_result_name(rc));
    ifbStep(rc == NMR_OK || rc == NMR_ALREADY_LIT ? NMH_STEP_RESUME_DONE : NMH_STEP_RESUME_FAILED);
    if (rc == NMR_OK || rc == NMR_ALREADY_LIT) {
        // the plane is on the firmware surface again; import slots and owned buffers must be mapped anew
        if (fHyReady) { nmh_ring_init(&fRing, fRing.base, fRing.slot_bytes); }
        if (fOwnReady) { nmh_own_home(&fOwn); }
    }
}

IOReturn NMIntelFB::hybridImport(NMHybridReq *q)
{
    q->result = NMH_OK;
#ifdef NMINTELFB_VIRTUAL
    q->result = NMH_E_PROVIDER; return kIOReturnUnsupported;
#else
    if (q->op == NMH_OP_HOME) {       // always honoured: after a stuck flip the plane must still be brought back
        if (fHyReady && fRegs) hybridHome();
        return kIOReturnSuccess;
    }
    if (fHyLevel < NMH_IMPORT || !fRegs || fPipe < 0) { q->result = fHyLevel < NMH_IMPORT ? NMH_E_LEVEL : NMH_E_PROVIDER; return kIOReturnUnsupported; }
    if (q->op == NMH_OP_QUERY) {
        q->result = nmh_import_check(fHyLevel, fVer, q->format, q->width, q->height, q->pitch, fWidth, fHeight);
        if (q->result != NMH_OK) return kIOReturnUnsupported;
        if (!(fHyLevel >= NMH_OWNED ? hybridSetup(fRowBytes, fHeight) : hybridSetup(q->pitch, q->height))) { q->result = NMH_E_NOSPACE; return kIOReturnUnsupported; }
        if (nmh_pages_for(q->pitch, q->height) * NMH_PAGE != fRing.slot_bytes) { q->result = NMH_E_SIZE; return kIOReturnUnsupported; }
        int need = 0; uint32_t ev = 0;
        q->slot = nmh_ring_acquire(&fRing, q->surfaceId, &need, &ev);
        if (q->slot < 0) { q->result = NMH_E_BUSY; return kIOReturnBusy; }
        q->needMap = (unsigned)need; q->evictedId = ev;
        return kIOReturnSuccess;
    }
    if (q->op == NMH_OP_ACQUIRE || q->op == NMH_OP_PRESENT || q->op == NMH_OP_ABORT || q->op == NMH_OP_QUARANTINE) {
        if (fHyLevel < NMH_OWNED || !ownSetup()) { q->result = NMH_E_NOSPACE; return kIOReturnUnsupported; }
        if (q->op == NMH_OP_ACQUIRE) {
            const int b = nmh_own_acquire(&fOwn);
            if (b < 0) { q->result = NMH_E_BUSY; return kIOReturnBusy; }
            q->slot = b; q->kva = fOwnMd[b]->getBytesNoCopy(); q->pitch = fRowBytes;
            if (q->flags & NMH_REQ_DEVICE_WRITES) {
                // the copy engine will write these pages: no dirty CPU line may be left to land over its data
                fOwnMd[b]->performOperation(kIOMemoryIncoherentIOFlush, 0, (IOByteCount)fRing.slot_bytes);
                q->pages = fOwnPages[b]; q->npages = fOwnPageCount;
            }
            return kIOReturnSuccess;
        }
        if (q->slot < 0 || q->slot >= NMH_OWN_BUFS) { q->result = NMH_E_BUSY; return kIOReturnBadArgument; }
        if (q->op == NMH_OP_ABORT) { nmh_own_abort(&fOwn, q->slot); return kIOReturnSuccess; }
        if (q->op == NMH_OP_QUARANTINE) { nmh_own_quarantine(&fOwn, q->slot); IFBLOG("hybrid: owned buffer %d quarantined (a copy timed out; it is not used again)", q->slot); return kIOReturnSuccess; }
        // PRESENT: flush, queue, flip, wait for the live register
        fOwnMd[q->slot]->performOperation(kIOMemoryIncoherentIOFlush, 0, (IOByteCount)fRing.slot_bytes);
        if (nmh_own_present(&fOwn, q->slot) != NMH_OK) { q->result = NMH_E_BUSY; return kIOReturnBusy; }
        const int rs = NMH_SLOTS + q->slot;
        if (nmh_flip_begin(&fRing, rs) != NMH_OK) { nmh_own_flip_failed(&fOwn); q->result = NMH_E_BUSY; return kIOReturnBusy; }
        if (fRing.pending == NMH_NONE) { nmh_own_landed(&fOwn); return kIOReturnSuccess; }
        hybridWriteSurf((uint32_t)nmh_slot_surf(&fRing, rs), fStrideReg);
        if (!hybridWaitLive(rs)) {
            fHyLevel = NMH_COPY;
            hybridWriteSurf(fSurf, fStrideReg);
            fRing.live = -3;
            nmh_own_flip_failed(&fOwn);
            q->result = NMH_E_STUCK;
            IFBLOG("hybrid: fallback to copy: %s", nmh_why_name(NMH_E_STUCK));
            return kIOReturnTimeout;
        }
        nmh_own_landed(&fOwn);
        return kIOReturnSuccess;
    }
    if (!fHyReady || q->slot < 0 || q->slot >= NMH_SLOTS) { q->result = NMH_E_BUSY; return kIOReturnBadArgument; }
    if (q->op == NMH_OP_MAP) {
        const uint64_t want = fRing.slot_bytes / NMH_PAGE;
        if (!q->pages || q->npages != want) { q->result = NMH_E_PAGES; nmh_ring_forget(&fRing, q->slot); return kIOReturnBadArgument; }
        const uint64_t at = fRing.base / NMH_PAGE + (uint64_t)q->slot * want;
        for (uint32_t i = 0; i < q->npages; i++)
            if (!nmh_ggtt_pte(q->pages[i])) { q->result = NMH_E_RANGE; nmh_ring_forget(&fRing, q->slot); return kIOReturnBadArgument; }
        for (uint32_t i = 0; i < q->npages; i++) fGsm[at + i] = nmh_ggtt_pte(q->pages[i]);
        (void)fGsm[at + q->npages - 1];                  // the last write has landed before the table is invalidated
        wr(kGfxFlshCntl, 1);
        nmh_ring_mapped(&fRing, q->slot);
        return kIOReturnSuccess;
    }
    if (q->op == NMH_OP_FLIP) {
        if (!fRing.s[q->slot].mapped) { q->result = NMH_E_PAGES; return kIOReturnNotReady; }
        if (nmh_flip_begin(&fRing, q->slot) != NMH_OK) { q->result = NMH_E_BUSY; return kIOReturnBusy; }
        if (fRing.pending == NMH_NONE) return kIOReturnSuccess;             // already on screen
        hybridWriteSurf((uint32_t)nmh_slot_surf(&fRing, q->slot), (fStrideReg & ~0x7FFu) | (q->pitch / 64));
        if (!hybridWaitLive(q->slot)) {
            // import is over for this boot: the copy path takes the panel back
            fHyLevel = NMH_COPY;
            hybridWriteSurf(fSurf, fStrideReg);
            fRing.live = -3;
            q->result = NMH_E_STUCK;
            IFBLOG("hybrid: fallback to copy: %s", nmh_why_name(NMH_E_STUCK));
            return kIOReturnTimeout;
        }
        if (fHyFlips == 0) ifbStep(NMH_STEP_FLIP);
        if (++fHyFlips == 1) IFBLOG("hybrid: zero-copy flip #1 surface 0x%x slot %d live", q->surfaceId, q->slot);
        return kIOReturnSuccess;
    }
    q->result = NMH_E_PROVIDER;
    return kIOReturnBadArgument;
#endif
}

// NVAccel's display pipe asks every head for its scan-out (NVRM_VRAM_FN_SCANOUT). This head answers with a CPU mapping and
// no VRAM address: phys 0 keeps NVAccel from treating Intel memory as NVIDIA VRAM (it is not in the NVIDIA card's BAR1),
// so frames reach the panel only through the copy into kva.
IOReturn NMIntelFB::callPlatformFunction(const OSSymbol *fn, bool wait, void *p1, void *p2, void *p3, void *p4)
{
    if (fn && fn->isEqualTo(NMH_FN_IMPORT)) {
        NMHybridReq *q = (NMHybridReq *)p1;
        if (!q || q->version != NMH_ABI_VERSION) return kIOReturnBadArgument;
        return hybridImport(q);
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

#ifdef NMINTELFB_VIRTUAL
// Test loader: attaches a virtual panel under the NVIDIA PCI device on demand, the way the real driver will place the
// Intel panel so WindowServer composites it on the NVIDIA accelerator.
class NMIntelFBLoader : public IOService {
    OSDeclareDefaultStructors(NMIntelFBLoader)
public:
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
};
OSDefineMetaClassAndStructors(NMIntelFBLoader, IOService)
static int gAttach = 0;
static int nmintelfb_attach_sysctl SYSCTL_HANDLER_ARGS
{
    int v = gAttach, err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || !req->newptr || v != 1 || gAttach == 1) return err;
    // A spare NVRMDisplay nub under NVRM (NVRM's own class), marked as the Intel panel: NMIntelFB claims it ahead of
    // NVRMFB, so the panel sits where NVRMFB does (GPU -> NVRM -> NVRMDisplay -> framebuffer). Starting a framebuffer
    // directly on the NVIDIA PCI device hung the kernel.
    IOService *nub0 = nullptr, *nvrm = nullptr;
    if (OSDictionary *m = IOService::serviceMatching("NVRMDisplay")) {
        if (OSIterator *it = IOService::getMatchingServices(m)) {
            while (OSObject *o = it->getNextObject()) {
                IOService *n = OSDynamicCast(IOService, o);
                OSNumber *fi = n ? OSDynamicCast(OSNumber, n->getProperty("fb-index")) : nullptr;
                if (fi && fi->unsigned32BitValue() == 0) { nub0 = n; nub0->retain(); break; }
            }
            it->release();
        }
        m->release();
    }
    if (nub0) { nvrm = nub0->getProvider(); if (nvrm) nvrm->retain(); }
    if (!nub0 || !nvrm) { OSSafeReleaseNULL(nub0); OSSafeReleaseNULL(nvrm); IFBLOG("attach: no NVRMDisplay 0 / NVRM"); return ENODEV; }
    IOService *nub = OSDynamicCast(IOService, OSMetaClass::allocClassWithName("NVRMDisplay"));
    if (!nub || !nub->init()) { OSSafeReleaseNULL(nub); nub0->release(); nvrm->release(); return ENOMEM; }
    static const char *const kCopy[] = {"gpu-id"};
    for (const char *k : kCopy) if (OSObject *pv = nub0->getProperty(k)) nub->setProperty(k, pv);
    nub->setProperty("fb-index", 4ull, 32);
    nub->setProperty("nm-intel-panel", kOSBooleanTrue);
    nub0->release();
    if (!nub->attach(nvrm)) { nub->release(); nvrm->release(); return EIO; }
    nvrm->release();
    nub->registerService();
    nub->release();
    gAttach = 1;
    IFBLOG("attach: Intel-panel NVRMDisplay nub published under NVRM");
    return 0;
}
SYSCTL_PROC(_debug, OID_AUTO, nmintelfb_attach, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED | CTLFLAG_KERN, nullptr, 0,
            nmintelfb_attach_sysctl, "I", "1 = attach the virtual panel under the NVIDIA display device (test build)");
bool NMIntelFBLoader::start(IOService *provider)
{
    if (version_major != 24 && version_major != 25) return false;   /* the 2.x line supports macOS 15 (Darwin 24) and macOS 26 (Darwin 25) only */
    if (!IOService::start(provider)) return false;
    sysctl_register_oid(&sysctl__debug_nmintelfb_attach);
    IFBLOG("loader idle until debug.nmintelfb_attach=1");
    return true;
}
void NMIntelFBLoader::stop(IOService *provider)
{
    sysctl_unregister_oid(&sysctl__debug_nmintelfb_attach);
    IOService::stop(provider);
}
#endif
