/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "../nvrm_off.h"
#include <IOKit/graphics/IOGraphicsTypes.h>
#ifndef detailedTimingModeID
#define detailedTimingModeID __reservedA[0]
#endif
#include <IOKit/IOService.h>
#include <IOKit/IOCommandGate.h>
#include <IOKit/IOLib.h>
#include <sys/sysctl.h>
#include <kern/thread_call.h>
#include <mach/mach_time.h>
#include <IOKit/IODeviceMemory.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/IOSubMemoryDescriptor.h>
#include <IOKit/graphics/IOFramebuffer.h>
#include <IOKit/graphics/IOGraphicsTypes.h>
#include <IOKit/graphics/IODisplay.h>
#include <IOKit/ndrvsupport/IOMacOSVideo.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSData.h>
#include <libkern/OSAtomic.h>
#include "../nvrm_eld.h"
#include "nvrm-hud.h"
#include "nvrm-fb-safety.h"
#include "nvrm-allocation-table.h"
static IORecursiveLock *startLock();
class NVRMCallReference {
    OSObject *owner;
public:
    explicit NVRMCallReference(OSObject *object) : owner(object) {}
    ~NVRMCallReference() { owner->release(); }
};
class NVRMDisplayGate {
    IORecursiveLock *lock;
public:
    NVRMDisplayGate() : lock(startLock()) { if (lock) IORecursiveLockLock(lock); }
    ~NVRMDisplayGate() { if (lock) IORecursiveLockUnlock(lock); }
    bool valid() const { return lock != nullptr; }
};
static NVRMAllocationTable<struct NvKmsKapiMemory, 4096> gAllocations;
extern "C" {
#include "nvkms.h"
#include "nvrm_gpuva_abi.h"
}
#include <libkern/OSAtomic.h>
#include "nvrm_vram_abi.h"

// Surfaces may use half of a resizable BAR1 (>= 4 GB) and three quarters of a smaller one. WAS: a fixed 192 MB below
// 4 GB, which is 3/4 of the 256 MB firmware window but left a 512 MB or 1 GB BAR1 no better off (issue #27).
// A BAR1 under 256 MB (64 MB on some RTX 2060 boards) gets three quarters of itself too: the 192 MB floor promised
// three times the window, so allocations failed inside mapMemory instead of being budgeted. Unknown (0) keeps 192 MB.
static inline uint64_t nvrmVramBudget(uint64_t barLen)
{
    if (barLen >= (4ull << 30)) return barLen / 2;
    uint64_t b = barLen / 4 * 3;
    if (barLen && barLen < (256ull << 20)) return b;
    return b > NVRM_VRAM_BAR1_BUDGET ? b : NVRM_VRAM_BAR1_BUDGET;
}

// The framebuffer's own scanout and cursor sit in BAR1 (useVideoMemory), yet the accel budget above never counted them,
// so the blind 3/4 fraction both wasted the BAR1 the display was not using on a small display and over-promised it on a
// large one. An RTX 2070 Super (256 MB BAR1, no larger size offered, 3840x2560) spent its 192 MB budget while ~40 MB of
// scanout went uncounted: WindowServer's surfaces were refused and the desktop stayed black with a cursor (user logs:
// "BAR1 budget spent (200015872 of 201326592 bytes)" with NVAccel allocPhysical REFUSED). The budget is now
// the BAR1 the display is not holding, less a margin for RM's own mappings (which are not in gVramMappedBytes); a large
// display lowers it instead of overflowing the aperture. gVramDisplayBytes is the measured scanout+cursor; 0 means the
// scanout is not mapped yet, so the conservative policy stands until it is.
static volatile SInt64 gVramDisplayBytes = 0;
static int gVramDisplayAware = 1;
static inline uint64_t nvrmSurfaceBudget(uint64_t barLen, SInt64 displayBytes)
{
    if (barLen >= (4ull << 30)) return barLen / 2;        // resizable BAR: half, the display is a rounding error
    if (!gVramDisplayAware || displayBytes <= 0)
        return nvrmVramBudget(barLen);                     // display not measured yet -- keep the conservative policy
    const uint64_t margin = 16ull << 20;                   // RM's own BAR1 mappings are not tracked in gVramMappedBytes
    const uint64_t reserve = (uint64_t)displayBytes + margin;
    if (reserve >= barLen) return nvrmVramBudget(barLen);  // pathological (display fills the window) -- fall back
    return barLen - reserve;
}

// EDID base block bytes 21 and 22 hold the panel's physical size in centimetres. macOS wants it in
// millimetres, per mode, through IODisplayModeInformation.imageWidth/imageHeight -- IOGraphicsTypes.h
// calls them "physical width/height of active image if known, in millimeters, otherwise zero", and
// IOFramebuffer passes the two fields straight through (they appear nowhere in IOFramebuffer.cpp).
// Left at zero, Core Graphics has no size for the display: CGDisplayScreenSize() returns the bounds
// at 25.4/72 mm per pixel, every mode carries kDisplayModeTelevisionFlag, and a 21.5" 1920x1080
// panel is reported as a 30-inch TV in About This Mac. Issue #126.
static void nvrmEdidPanelSizeMm(const NvU8 *edid, NvU16 size, NvU16 *mmW, NvU16 *mmH)
{
    *mmW = 0; *mmH = 0;
    if (size < 128) return;
    if (!edid[21] || !edid[22]) return;   // unknown size, or the EDID 1.4 aspect-ratio encoding
    *mmW = (NvU16)(edid[21] * 10);
    *mmH = (NvU16)(edid[22] * 10);
}

typedef NvU64 (*nvrm_phys_for_va_t)(const void *va);
#include <mach/kmod.h>
extern "C" kern_return_t _start(kmod_info_t *ki, void *d); extern "C" kern_return_t _stop(kmod_info_t *ki, void *d);
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NVRMFB, "0.1", _start, _stop) }
extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }
static inline void nvu_zero(void *p, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(p), "+c"(n) : "a"(0) : "memory"); }
static inline void nvu_copy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }
// Clears freshly granted VRAM through its BAR1 mapping. Byte stores are the slow case there: on an RTX 3070 Laptop a byte-wise clear of an
// 8 MB grant took ~180 ms with WindowServer waiting on it; 8-byte stores do the same job with 8x fewer PCIe writes (~20 ms).
static inline void nvu_zero_wide(void *p, size_t n) { size_t q = n >> 3; __asm__ __volatile__("rep stosq" : "+D"(p), "+c"(q) : "a"(0ull) : "memory"); nvu_zero(p, n & 7); }
#define FBTRACE(buf) do { trace(buf); } while (0)
#define FBSEL(a) char _s[5] = { (char)((a) >> 24), (char)((a) >> 16), (char)((a) >> 8), (char)(a), 0 }
#define FBLOG(fmt, ...) do { IOLog("NVRM-fb: " fmt "\n", ##__VA_ARGS__); kprintf("NVRM-fb: " fmt "\n", ##__VA_ARGS__); \
    { char _tb[220]; snprintf(_tb, sizeof _tb, fmt, ##__VA_ARGS__); FBTRACE(_tb); } } while (0)
static const char kPixelFormats[] = IO32BitDirectPixels "\0";
static const IODisplayModeID kModeId = 1;
static const unsigned kTraceMax = 900;

// Laptop panel brightness. macOS gives the brightness keys and the brightness slider to its built-in
// display, an AppleBacklightDisplay. IOGraphics creates one (AppleBacklightDisplay::probe) for a connection
// that reports kIOConnectionBuiltIn and an LCD from getAppleSense(), provided a service named "backlight"
// existed when the first framebuffer was opened (gIOFBHaveBacklight, IOFramebuffer::open). On Macs that is
// the ACPI PNLF device; here it is this handler, published by the panel's framebuffer before super::start().
// IODisplay finds it below the framebuffer (searchParameterHandlers) and offers "brightness" and
// "linear-brightness" through it; both are percent of the panel's PWM. The level goes to NVRM
// ("NVRMBacklight": nvKmsSetBacklight on the backlight NVKMS registered), from a thread call, because
// IODisplay calls doIntegerSet() with the framebuffer lock held.
static const SInt32 kNVRMMinBacklight = 5;   // never 0: some panels switch the backlight off completely
#define BLLOG(fmt, ...) do { IOLog("NVRM-fb: " fmt "\n", ##__VA_ARGS__); kprintf("NVRM-fb: " fmt "\n", ##__VA_ARGS__); } while (0)

class NVRMBacklightHandler : public IODisplayParameterHandler {
    OSDeclareDefaultStructors(NVRMBacklightHandler)
    IOService *fRM = nullptr;
    const OSSymbol *fFn = nullptr;
    thread_call_t fApply = nullptr;
    volatile SInt32 fPending = -1;
    SInt32 fLevel = 100;
    static void applyFire(thread_call_param_t me, thread_call_param_t);
public:
    static NVRMBacklightHandler *withRM(IOService *rm, NvU32 level);
    bool setDisplay(IODisplay *display) override;
    bool doIntegerSet(OSDictionary *params, const OSSymbol *paramName, UInt32 value) override;
    bool doDataSet(const OSSymbol *, OSData *) override { return false; }
    bool doUpdate() override { return true; }
    void free() override;
};
OSDefineMetaClassAndStructors(NVRMBacklightHandler, IODisplayParameterHandler)

NVRMBacklightHandler *NVRMBacklightHandler::withRM(IOService *rm, NvU32 level)
{
    NVRMBacklightHandler *h = OSTypeAlloc(NVRMBacklightHandler);
    if (!h || !h->init()) { OSSafeReleaseNULL(h); return nullptr; }
    h->fRM = rm; rm->retain();
    h->fFn = OSSymbol::withCStringNoCopy("NVRMBacklight");
    h->fApply = thread_call_allocate(applyFire, h);
    h->fLevel = level <= 100 ? (SInt32)level : 100;
    if (!h->fFn || !h->fApply) { h->release(); return nullptr; }
    return h;
}

void NVRMBacklightHandler::free()
{
    if (fApply) { thread_call_cancel(fApply); while (!thread_call_free(fApply)) { thread_call_cancel(fApply); IOSleep(1); } fApply = nullptr; }
    OSSafeReleaseNULL(fFn);
    OSSafeReleaseNULL(fRM);
    IODisplayParameterHandler::free();
}

// NVRMFB reports no framebuffer display parameters, so the display may have no IODisplayParameters yet;
// IODisplay::setProperties() only accepts parameters listed there.
bool NVRMBacklightHandler::setDisplay(IODisplay *display)
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
    BLLOG("backlight: brightness parameters on %s, panel at %d%%", display->getName(), (int)fLevel);
    return true;
}

// User changes arrive with the parameter's own { min, max, value } (IODisplay::setForKey stores the value);
// AppleBacklightDisplay's power states (dim, display sleep, lid) pass the whole IODisplayParameters.
bool NVRMBacklightHandler::doIntegerSet(OSDictionary *, const OSSymbol *paramName, UInt32 value)
{
    if (paramName != gIODisplayBrightnessKey && paramName != gIODisplayLinearBrightnessKey) return false;
    fPending = value > 100 ? 100 : (SInt32)value;
    thread_call_enter(fApply);
    return true;
}

void NVRMBacklightHandler::applyFire(thread_call_param_t p, thread_call_param_t)
{
    NVRMBacklightHandler *me = (NVRMBacklightHandler *)p;
    SInt32 v;
    do { v = me->fPending; } while (!OSCompareAndSwap((UInt32)v, (UInt32)-1, (volatile UInt32 *)&me->fPending));
    if (v < 0) return;
    NvU32 percent = (NvU32)(v < kNVRMMinBacklight ? kNVRMMinBacklight : v);
    IOReturn r = me->fRM->callPlatformFunction(me->fFn, false, &percent, (void *)1, NULL, NULL);
    if (r == kIOReturnSuccess) me->fLevel = v;
    else BLLOG("backlight: setting %u%% failed (0x%x)", (unsigned)percent, r);
}

// 1 once this boot's guard clear happened (a head lit, or nothing to light): NVRM reads it (debug.nvrmfb_guard_cleared)
// before putting its start count back after a settle wait, so a display brought up by hand during the wait is not
// counted as a failed start (nvrm_off.h).
static int gGuardCleared;
static void fbGuardCleared(const char *why);
class NVRMNVDAFramebuffer : public IOFramebuffer {
    OSDeclareDefaultStructors(NVRMNVDAFramebuffer)
    struct NvKmsKapiFunctionsTable *fKms = nullptr; NvU32 fGpuId = 0; nvrm_phys_for_va_t fPhysForVa = nullptr;
    struct NvKmsKapiDevice *fDev = nullptr;
    NvKmsKapiDisplay fDisplay = 0; NvKmsKapiConnector fConnector = 0; NvU32 fHead = 0;
    unsigned fIndex = 0; bool fOwnsHead = false;
    bool fBootConsole = false;
    bool fInternal = false;     // NVKMS static display info: laptop panel (eDP/LVDS)
    NVRMBacklightHandler *fBacklight = nullptr;   // the panel's brightness control, see NVRMBacklightHandler
    void publishBacklight();
    struct NvKmsKapiDisplayMode fMode = {};
    // the mode NVKMS validated for this display, kept when the boot pair takes the firmware's timing instead (applyMode)
    struct NvKmsKapiDisplayMode fKmsMode = {};
    bool fHaveKmsMode = false;
    struct NvKmsKapiMemory *fMem = nullptr; struct NvKmsKapiSurface *fSurf = nullptr;
    void *fKva = nullptr; NvU64 fPhys = 0, fSize = 0; NvU32 fPitch = 0, fW = 0, fH = 0;
    NvU64 fScanLogPhys = 0; NvU32 fScanLogW = 0, fScanLogH = 0, fScanLogPitch = 0;  // last nvScanoutInfo answer logged
    IODeviceMemory *fConsMem = nullptr; IOMemoryMap *fConsMap = nullptr;
    void *fConsKva = nullptr; NvU64 fConsPhys = 0, fConsSize = 0; NvU32 fConsPitch = 0;
    NvU64 fVtFbBase = 0, fVtFbSize = 0;
    void *fUserPhys = nullptr;
    IOService *fPciNub = nullptr;
    IOService *pciNub();
    IOMemoryDescriptor *fBarMem = nullptr; IOMemoryMap *fBarMap = nullptr;
    void *fBarKva = nullptr; NvU64 fBarLen = 0;
    bool fSpareIndex = false;   // kapiInit stopped because this index has no display to drive (expected, not a failure)
    void surveyBar1();
    bool fConsoleAperture = false;
    NvU64 fBar1Base = 0;
    NvU8 fEdid[NVKMS_KAPI_EDID_BUFFER_SIZE]; NvU16 fEdidSize = 0;
    // The panel's physical size in millimetres, from the EDID's base block. Reported per mode as
    // IODisplayModeInformation.imageWidth/imageHeight; see nvrmEdidPanelSizeMm().
    NvU16 fEdidMmW = 0, fEdidMmH = 0;
    bool fModeSet = false;
    unsigned fHomePure = 0;
    IODetailedTimingInformationV2 fDT[128]; unsigned fNDT = 0;
    IODisplayModeID fCurId = 1;
    struct NvKmsKapiDisplayMode fBootMode = {};
    NvU32 fBootW = 0, fBootH = 0, fBootPitch = 0;
    NvU32 fRasterW = 0, fRasterH = 0;
    // The gamma table WindowServer last set (Night Shift, True Tone, ColorSync calibration, fades), in NVKMS's output-LUT
    // form. Every modeset carries it so a mode change does not drop it; setGammaTable commits it on its own.
    struct NvKmsLutRamps *fGamma = nullptr;
    bool fGammaOn = false;
    unsigned fGammaCommits = 0, fGammaFails = 0;
    IOReturn nvrmSetGamma(UInt32 channels, UInt32 count, UInt32 width, const void *data);
    NvU32 fPitchAlign = 256;
    NvU32 fScalerMaxW = 0, fScalerMaxH = 0;
    // Widest raster offered when it fits the scanout memory. Separate from the scaler limit: a 5120x1440 monitor whose
    // firmware started at 2560x1440 has a 3840-wide scaler limit, yet 5120x1440 needs less memory than 3840x2160.
    NvU32 fRasterMaxW = 0;
    NvU64 fAllocSize = 0, fMemBytes = 0;
    // The scanout and cursor bytes this head mapped into BAR1 (video memory only), counted once into gVramDisplayBytes.
    NvU64 fScanoutBarBytes = 0, fCursorBarBytes = 0;
    unsigned fModeSwitches = 0, fValidateCalls = 0, fValidateOk = 0;
    bool fBootInDT = false;
    bool fKmsModesLogged = false;   // the per-mode NVKMS listing is printed once, on the first enumeration
    int dtIndex(IODisplayModeID id) const;
    bool edidModeFor(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *m);
    unsigned addKmsRefreshModes(unsigned n);
    void computeScanoutCaps(NvU32 pitchAlign);
    unsigned fEdidHits = 0;
    static void dtToKapi(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *m);
    IOReturn switchMode(IODisplayModeID id, const struct NvKmsKapiDisplayMode &m, NvU32 w, NvU32 h);
    void publishTimingCaps();
    unsigned fLastOk = 0; int fFlipResult = -99;
    unsigned fParkedCount = 0;
    struct NvKmsKapiMemory *fCurMem = nullptr; struct NvKmsKapiSurface *fCurSurf = nullptr;
    void *fCurKva = nullptr; NvU32 fCurDim = 0;
    NvU32 fCurMaxPx = 0, fCurCompModes = 0;
    SInt32 fCurX = 0, fCurY = 0; bool fCurVisible = false; bool fCurHaveImage = false;
    unsigned fCurApplies = 0; int fCurRc = 0;
    // What the head's cursor shows since the last cursorApply that named its surface (fCurSurf or none). Unknown after a
    // failed apply and after a mode set, whose request names no cursor; the next apply then names it again.
    struct NvKmsKapiSurface *fCurShown = nullptr; bool fCurShownKnown = false;
    // setCursorState only records the position and visibility (under fCurStateLock) and has fCursorCall apply them.
    // WindowServer calls it for every mouse event from its cursor queue while holding CoreDisplay's cursor mutex,
    // which its main thread takes for every event too, so an NVKMS cursor flip (about 0.3 ms, under the display gate)
    // done there stalls the compositor while the mouse moves. A move that arrives while one is queued is applied with
    // it. cursorApply and setCursorImage take turns on the display gate alone: a lock of the cursor's own taken before
    // the gate would deadlock with any thread that holds the gate and reaches setCursorImage.
    thread_call_t fCursorCall = nullptr;
    IOSimpleLock *fCurStateLock = nullptr;
    static void cursorFire(thread_call_param_t p0, thread_call_param_t);
    bool fCursorEnabled = false;
    bool cursorInit();
    bool cursorApply(bool imageChanged = false);
    IOReturn setCursorImageLocked(void *cursorImage);
    uint64_t fVblNext = 0;
    unsigned fHudFrames = 0;
    NvU32 fNumHeads = 0, fNumConnectors = 0;
    struct { IOFBInterruptProc proc; OSObject *target; void *ref; } fVbl = {}, fConnect = {};
    // This instance exists because a display appeared after boot (see gNDpyBoot), and its connect handler has already
    // been fired once -- see connectNotifyFire and enableController.
    bool fHotplugBorn = false, fConnectFired = false;
    // A different monitor was plugged into this framebuffer's connector (its EDID changed): see adoptSwappedDisplay.
    bool fEdidSwapped = false;
    void adoptSwappedDisplay();
    static void connectNotifyFire(thread_call_param_t p0, thread_call_param_t);
    volatile SInt32 fStopping = 0;
    void scheduleCall(thread_call_t call, uint64_t deadline);
    thread_call_t fSampler = nullptr; unsigned fSamples = 0;
    static void trace(const char *s);
    thread_call_t fVblTimer = nullptr;
    struct NvKmsKapiVblankIntrCallback *fHwVblCb = nullptr;
    thread_call_t fHwVblCall = nullptr;
    volatile uint64_t fHwVblLast = 0;
    volatile unsigned long long fHwVblCalls = 0;
    unsigned fModeSetTicks = 0;
    bool fHwVblWanted = false;
    unsigned fHwVblTries = 0, fHwVblTickAtTry = 0, fVblTicks = 0;
    static void hwVblIntr(NvU64 param, NvU64 timestamp);
    static void hwVblDeliver(thread_call_param_t p0, thread_call_param_t);
    uint64_t vblPeriodAbs();
    thread_call_t fBars = nullptr;
    struct NVFlipEntry { struct NvKmsKapiMemory *mem; struct NvKmsKapiSurface *surf;
                         unsigned w, h, pitch; UInt64 epoch, cookie; struct NvKmsKapiMemory *pin; };
    NVFlipEntry fFlipCache[16] = {};
    IOSimpleLock *fFlipCacheLock = nullptr;
    unsigned    fFlipCacheN = 0, fFlipCount = 0, fFlipRejects = 0, fFlipEvict = 0;
    struct NvKmsKapiSurface *fFrontSurf = nullptr;
    static inline volatile SInt32 sFlipLatched[8] = {};
    bool flipToMemory(struct NvKmsKapiMemory *mem, unsigned w, unsigned h, unsigned pitch, int *rcOut, bool pure = false, UInt64 cookie = 0);
    bool fSubmissionSeen = false, fSubmissionFaulted = false;
    // When the tracked flip was committed, so a flip still pending can be told late (another period will do) from
    // stuck (kFlipStuckUs since its submit, the head is faulted).
    uint64_t fFlipSubmitAbs = 0;
    unsigned fLatchLateLogs = 0;
    enum { kPriorDone = 0, kPriorLate = 1, kPriorStuck = 2 };
    uint64_t flipWaitUs();
    int priorFlipState();
    void publishFlipWait();
    unsigned fRejected = 0;     // commits NVKMS refused on this head, for NVRMRejectedCommits
    NvBool submitConfig(struct NvKmsKapiRequestedModeSetConfig *cfg, struct NvKmsKapiModeSetReplyConfig *rep, NvBool commit,
                        bool bootstrap = false, bool cursorOnly = false, bool frame = false);
    thread_call_t fReflip = nullptr;
    unsigned fReflips = 0; int fReflipRc = -99;
    unsigned fVblReg = 0;
    unsigned long long fVblCalls = 0;
    unsigned fApertureLogged = 0;
    unsigned fBarPaints = 0;
    thread_call_t fHeadProbe = nullptr; unsigned fProbeStep = 0;
    static void headProbeFire(thread_call_param_t p0, thread_call_param_t);
    bool setHeadActive(bool active);
    static void sampleFire(thread_call_param_t p0, thread_call_param_t);
    static void vblFire(thread_call_param_t p0, thread_call_param_t);
    static void barsFire(thread_call_param_t p0, thread_call_param_t);
    static void reflipFire(thread_call_param_t p0, thread_call_param_t);
    bool reflip();
    bool headOff();
    bool fHeadSlept = false;
    // System sleep: NVRM puts the GPU in standby (NVKMS turns every head off) and resumes it on wake, telling each
    // framebuffer through callPlatformFunction("NVRMGpuPower"). The head is lit again only once the GPU is back.
    bool fPowerOn = true;                 // this framebuffer's own power state (display sleep / system sleep)
    bool fSysSlept = false;               // NVRM put the GPU in standby since this framebuffer last powered on

    thread_call_t fGpuWake = nullptr;
    static void gpuWakeFire(thread_call_param_t p0, thread_call_param_t);
    bool rmGpuReady();
    void familyDidWake();
    IOReturn familyDidWakeGated(void *, void *, void *, void *);
    IOCommandGate *fWakeGate = nullptr;
    // WillSleep(false) deliveries since the last full wake. IOGraphics sends one per system sleep, also when the sleep
    // starts from a dark wake, and IOGraphicsAccelerator2 counts each as a display mode change in progress; the
    // accelerator is enabled again only when as many DidWake(false) have arrived.
    IONotifier *fSleepTap = nullptr;
    volatile SInt32 fWillSleepOff = 0;
    static IOReturn sleepTap(OSObject *self, void *, IOFramebuffer *, IOIndex event, void *info);
    bool fDpyLost = false;   // hot-plug saw this framebuffer's display disconnected; light it again when it returns
    static void eventCallback(const struct NvKmsKapiEvent *event) {
        // only display changes are logged: the routine events arrive every minute for as long as the Mac is up, and
        // in sent logs they pushed every boot line past the collector's 512 KB limit
        if (event->type == NVKMS_EVENT_TYPE_DPY_CHANGED || event->type == NVKMS_EVENT_TYPE_DYNAMIC_DPY_CONNECTED)
            FBLOG("kapi event type %d (display change)", (int)event->type);
        if (event->type == NVKMS_EVENT_TYPE_FLIP_OCCURRED && event->u.flipOccurred.head < 8
            && event->u.flipOccurred.layer == NVKMS_KAPI_LAYER_PRIMARY_IDX) {
            OSIncrementAtomic(&sFlipLatched[event->u.flipOccurred.head]);
            flipWake(event->u.flipOccurred.head);
        }
        // A display connected (or a connector changed) after boot: look for it off this context, after a short
        // settle (EDID reads follow the hot-plug interrupt).
        if (event->type == NVKMS_EVENT_TYPE_DPY_CHANGED || event->type == NVKMS_EVENT_TYPE_DYNAMIC_DPY_CONNECTED)
            hotplugKick();
    }
    static void hotplugKick();
    static void hotplugFire(thread_call_param_t p0, thread_call_param_t);
    static void flipWake(unsigned head);
    bool waitFlipDone(uint64_t limitUs);
    void flipFaultLog(const char *why);
    bool kapiInit();
    bool applyMode();
    void hudDraw();
public:
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    void free() override;
    IOReturn enableController() override;
    const char *getPixelFormats() override { return kPixelFormats; }
    // macOS builds its mode list from getDisplayModes BEFORE it calls setDetailedTimings, and never re-reads it: a
    // mode added only in setDetailedTimings never reached WindowServer (36 added, 0 shown). So the
    // display's native-raster modes at every refresh rate NVKMS validated are offered from the first query on.
    IOItemCount getDisplayModeCount() override { if (!fNDT) fNDT = addKmsRefreshModes(0); return (fBootInDT ? 0 : 1) + fNDT; }
    IOReturn getDisplayModes(IODisplayModeID *allDisplayModes) override {
        unsigned k = 0;
        if (!fBootInDT) allDisplayModes[k++] = kModeId;
        for (unsigned i = 0; i < fNDT; i++) allDisplayModes[k++] = (IODisplayModeID)fDT[i].detailedTimingModeID;
        return kIOReturnSuccess; }
    IOReturn validateDetailedTiming(void *description, IOByteCount descripSize) override;
    IOReturn setDetailedTimings(OSArray *array) override;
    IOReturn getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info) override;
    IOReturn getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info) override;
    UInt64 getPixelFormatsForDisplayMode(IODisplayModeID, IOIndex) override { return 0; }
    IOReturn getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi) override;
    IOReturn getCurrentDisplayMode(IODisplayModeID *mode, IOIndex *depth) override { if (mode) *mode = fCurId; if (depth) *depth = 0; return kIOReturnSuccess; }
    IOReturn setDisplayMode(IODisplayModeID mode, IOIndex depth) override;
    IODeviceMemory *apertureFor(NvU64 target, IOByteCount bytes);
    bool verifyAperture(NvU64 phys, const char *label);
    IODeviceMemory *getApertureRange(IOPixelAperture aperture) override;
    uint32_t fVramLogged = 0;
    IODeviceMemory *getVRAMRange() override;

    IOReturn callPlatformFunction(const OSSymbol *functionName, bool waitForFunction,
                                  void *param1, void *param2, void *param3, void *param4) APPLE_KEXT_OVERRIDE;
    bool     vramGrant(struct NVRMVramRequest *r);
    bool     vramRelease(struct NVRMVramRequest *r);
    uint32_t fVramGrantLogged = 0;
    uint32_t fVramGrants = 0;
    IOReturn getAttribute(IOSelect attribute, uintptr_t *value) override;
    IOReturn getAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t *value) override;
    IOReturn getAppleSense(IOIndex connectIndex, UInt32 *senseType, UInt32 *primary, UInt32 *extended, UInt32 *displayType) override;
    IOReturn setAttribute(IOSelect attribute, uintptr_t value) override;
    bool setHeadPower(bool awake);
    IOReturn setPowerState(unsigned long ordinal, IOService *device) override;
    IOReturn setCursorImage(void *cursorImage) override;
    IOReturn setCursorState(SInt32 x, SInt32 y, bool visible) override;
    bool isConsoleDevice(void) override { return fBootConsole; }
    // WAS: both returned success and did nothing, so Night Shift, True Tone and calibration changed nothing on screen.
    IOReturn setGammaTable(UInt32 ch, UInt32 n, UInt32 w, void *d) override { return nvrmSetGamma(ch, n, w, d); }
    IOReturn setGammaTable(UInt32 ch, UInt32 n, UInt32 w, void *d, bool) override { return nvrmSetGamma(ch, n, w, d); }
    IOReturn setCLUTWithEntries(IOColorEntry *, UInt32, UInt32, IOOptionBits) override { return kIOReturnSuccess; }
    IOReturn setApertureEnable(IOPixelAperture, IOOptionBits) override { return kIOReturnSuccess; }
    IOReturn setStartupDisplayMode(IODisplayModeID, IOIndex) override { return kIOReturnSuccess; }
    IOReturn getStartupDisplayMode(IODisplayModeID *mode, IOIndex *depth) override
        { if (mode) *mode = kModeId; if (depth) *depth = 0; return kIOReturnSuccess; }
    IOReturn connectFlags(IOIndex, IODisplayModeID, IOOptionBits *flags) override
        { if (flags) *flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
          return kIOReturnSuccess; }
    IOReturn setAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t value) override;
    IOItemCount getConnectionCount() override { return 1; }
    bool hasDDCConnect(IOIndex) override { return fEdidSize >= 128; }
    IOReturn getDDCBlock(IOIndex connectIndex, UInt32 blockNumber, IOSelect blockType, IOOptionBits options, UInt8 *data, IOByteCount *length) override;
    IOReturn registerForInterruptType(IOSelect type, IOFBInterruptProc proc, OSObject *target, void *ref, void **interruptRef) override;
    IOReturn unregisterInterrupt(void *interruptRef) override { return kIOReturnSuccess; }
    IOReturn setInterruptState(void *interruptRef, UInt32 state) override { return kIOReturnSuccess; }
};
OSDefineMetaClassAndStructors(NVRMNVDAFramebuffer, IOFramebuffer)
struct nvrm_dpy_rec {
    NvKmsKapiDisplay handle; NvKmsKapiConnector connector; NvU32 headMask; NvBool internal;
    NvU32 edidSize; NvU8 edid[NVKMS_KAPI_EDID_BUFFER_SIZE];
};
static struct NvKmsKapiDevice *gDev;
static struct nvrm_dpy_rec gDpy[NVKMS_KAPI_MAX_CONNECTORS * 2];
static int gNDpy = -1;
// How many displays the boot enumeration found. A framebuffer whose display index is >= this exists because a display
// appeared after boot (hotplugFire adds it to gDpy and re-registers the idle nub), and that instance is the one that
// has to tell the framework its connection is new -- see connectNotifyFire/enableController.
static int gNDpyBoot = -1;
static int gBootHead = -2;
static NvKmsKapiDisplay gBootDpy = 0;
static struct NvKmsKapiDisplayMode gBootMode;
static NvU32 gHeadsTaken;
static NvU32 gNumHeads;
// Flip-cache allocation epoch. The cache is keyed by the NvKmsKapiMemory pointer, and a buffer allocated after a free can
// land at the freed one's address; a cache hit then scanned out the old surface over freed memory, and after a refresh or
// resolution change on either monitor the other one strobed black every other frame. Every grant and release advances
// this epoch, and an entry only matches at the epoch it was made in, so a reused address always gets a new surface.
// No framebuffer touches another one's cache: a framebuffer that fails start (a card has more display nubs than
// connected monitors) is freed, and walking a list of framebuffers to purge their caches faulted on the freed one.
static volatile SInt64 gFlipEpoch = 1;
static const SInt64 kFlipEpochLimit = 0x7fffffffffffffffLL;
static UInt64 readFlipEpoch() { return (UInt64)OSAddAtomic64(0, &gFlipEpoch); }
static void advanceFlipEpoch()
{
    // Saturates instead of wrapping, so an old entry can never become current again; at the limit caching stops.
    for (;;) {
        const SInt64 old = OSAddAtomic64(0, &gFlipEpoch);
        if (old == kFlipEpochLimit) return;
        if (OSCompareAndSwap64((UInt64)old, (UInt64)(old + 1), (volatile UInt64 *)&gFlipEpoch)) return;
    }
}

class NVRMFBClaim : public IOService
{
    OSDeclareDefaultStructors(NVRMFBClaim)
public:
    bool start(IOService *provider) override
    {
        // With the driver off, leave the slot to IONDRVFramebuffer so the firmware's display stays lit while the
        // removal runs. WAS: the claim held it anyway and the screen stayed dark until the second restart.
        if (nvrm_driver_off()) return false;
        if (!IOService::start(provider)) return false;
        IOLog("NVRMFB: NVRMFBClaim holds the IOFramebuffer match category on %s - IONDRVFramebuffer blocked\n",
              provider ? provider->getName() : "?");
        return true;
    }
};
OSDefineMetaClassAndStructors(NVRMFBClaim, IOService)

// With the driver off ("Start once without NVIDIA driver", "Remove NVIDIA driver") the claim above steps aside, but the
// install excludes IONDRVSupport in OpenCore so Apple's firmware framebuffer never takes this display: nothing lit the
// screen and both driver-off starts came up black (1.14.0/1.14.1 reports). This is what IONDRVFramebuffer does for a card
// without an NDRV: one fixed mode on the firmware's (GOP) framebuffer, drawn by the CPU. It only matches with the driver
// off, and only when the firmware's framebuffer lies inside this card's BARs (a panel on the iGPU is not ours).
class NVRMBootFB : public IOFramebuffer
{
    OSDeclareDefaultStructors(NVRMBootFB)
    PE_Video fV;
    IODeviceMemory *fAp;
public:
    IOService *probe(IOService *provider, SInt32 *score) override
    {
        if (!nvrm_driver_off() || !provider) return NULL;
        PE_Video v; nvu_zero(&v, sizeof v);
        if (!getPlatform() || getPlatform()->getConsoleInfo(&v) != kIOReturnSuccess) return NULL;
        if (!v.v_baseAddr || !v.v_rowBytes || !v.v_width || !v.v_height || v.v_depth != 32) return NULL;
        const NvU64 base = (NvU64)v.v_baseAddr & ~3ull, bytes = (NvU64)v.v_rowBytes * v.v_height;
        for (IOItemCount i = 0; i < provider->getDeviceMemoryCount(); i++) {
            IOMemoryDescriptor *m = provider->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            const NvU64 pa = (NvU64)m->getPhysicalSegment(0, 0, kIOMemoryMapperNone), len = m->getLength();
            if (base < pa || bytes > len || base - pa > len - bytes) continue;
            IOMemoryDescriptor *sub = IOSubMemoryDescriptor::withSubRange(m, (IOByteCount)(base - pa), (IOByteCount)bytes,
                                                                          kIODirectionNone);
            if (!sub) return NULL;
            OSSafeReleaseNULL(fAp);
            fAp = (IODeviceMemory *)sub; fV = v;
            IOLog("NVRMFB: driver off - firmware framebuffer %lux%lu rowbytes %lu in BAR%u lights the screen\n",
                  (unsigned long)v.v_width, (unsigned long)v.v_height, (unsigned long)v.v_rowBytes, (unsigned)i);
            return this;
        }
        // the addresses, so a screen that stays dark with the driver off can be traced: macOS can move the card's VRAM
        // window after the firmware drew into it (10-17: the firmware's window was gone on a Resizable BAR card)
        IOLog("NVRMFB: driver off - the firmware framebuffer 0x%llx (+%llu) is not in this card's windows, not starting\n", base, bytes);
        for (IOItemCount i = 0; i < provider->getDeviceMemoryCount(); i++)
            if (IOMemoryDescriptor *m = provider->getDeviceMemoryWithIndex(i))
                IOLog("NVRMFB:   window %u at 0x%llx (%llu B)\n", (unsigned)i,
                      (unsigned long long)m->getPhysicalSegment(0, 0, kIOMemoryMapperNone), (unsigned long long)m->getLength());
        return NULL;
    }
    void free() override { OSSafeReleaseNULL(fAp); IOFramebuffer::free(); }
    IOReturn enableController() override { return kIOReturnSuccess; }
    bool isConsoleDevice() override { return true; }
    const char *getPixelFormats() override { return IO32BitDirectPixels "\0"; }
    IOItemCount getDisplayModeCount() override { return 1; }
    IOReturn getDisplayModes(IODisplayModeID *m) override { if (m) *m = 1; return kIOReturnSuccess; }
    IOReturn getCurrentDisplayMode(IODisplayModeID *m, IOIndex *d) override { if (m) *m = 1; if (d) *d = 0; return kIOReturnSuccess; }
    IOReturn getStartupDisplayMode(IODisplayModeID *m, IOIndex *d) override { return getCurrentDisplayMode(m, d); }
    UInt64 getPixelFormatsForDisplayMode(IODisplayModeID, IOIndex) override { return 0; }
    IOItemCount getConnectionCount() override { return 1; }
    IODeviceMemory *getApertureRange(IOPixelAperture a) override
    {
        if (a != kIOFBSystemAperture || !fAp) return NULL;
        fAp->retain(); return fAp;
    }
    IOReturn getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info) override
    {
        if (!info || mode != 1) return kIOReturnBadArgument;
        nvu_zero(info, sizeof *info);
        info->nominalWidth = (UInt32)fV.v_width; info->nominalHeight = (UInt32)fV.v_height;
        info->refreshRate = 60u << 16; info->maxDepthIndex = 0;
        info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
        return kIOReturnSuccess;
    }
    IOReturn getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture ap, IOPixelInformation *pi) override
    {
        if (!pi || mode != 1 || depth != 0 || ap != kIOFBSystemAperture) return kIOReturnBadArgument;
        nvu_zero(pi, sizeof *pi);
        pi->bytesPerRow = (UInt32)fV.v_rowBytes; pi->activeWidth = (UInt32)fV.v_width; pi->activeHeight = (UInt32)fV.v_height;
        pi->bitsPerPixel = 32; pi->pixelType = kIORGBDirectPixels; pi->componentCount = 3; pi->bitsPerComponent = 8;
        pi->componentMasks[0] = 0x00ff0000; pi->componentMasks[1] = 0x0000ff00; pi->componentMasks[2] = 0x000000ff;
        strlcpy(pi->pixelFormat, IO32BitDirectPixels, sizeof pi->pixelFormat);
        return kIOReturnSuccess;
    }
    IOReturn getAttributeForConnection(IOIndex ci, IOSelect attribute, uintptr_t *value) override
    {
        switch (attribute) {
        case kConnectionEnable: case kConnectionCheckEnable: if (value) *value = 1; return kIOReturnSuccess;
        case kConnectionFlags: if (value) *value = 0; return kIOReturnSuccess;
        default: return IOFramebuffer::getAttributeForConnection(ci, attribute, value);
        }
    }
};
OSDefineMetaClassAndStructors(NVRMBootFB, IOFramebuffer)

#define super IOFramebuffer

// The firmware can leave a display lit on a head we do not use (seen: HDMI-0 on head 2, while this driver drives it
// from head 1). A commit that names only our head leaves that head as the firmware set it, and NVKMS then shuts it down
// inside every one of our modesets ("TAKEOVER head 2 SHUTDOWN ... HDMI-0"): two heads on one connector, and the panel
// strobed or went black after a refresh change. NVIDIA's own driver commits the state of every head; so does this:
// each modeset also switches off the heads no framebuffer of ours owns.
static void nvrmUnownedHeadsOff(struct NvKmsKapiRequestedModeSetConfig *cfg)
{
    const NvU32 n = gNumHeads ? gNumHeads : 4;
    for (NvU32 h = 0; h < n && h < sizeof cfg->headRequestedConfig / sizeof cfg->headRequestedConfig[0]; h++) {
        if (gHeadsTaken & (1u << h)) continue;
        struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[h];
        cfg->headsMask |= 1u << h;
        hr->modeSetConfig.bActive = NV_FALSE;
        hr->modeSetConfig.numDisplays = 0;
        hr->flags.activeChanged = hr->flags.displaysChanged = NV_TRUE;
    }
}
static void nvrmHeadConfigBaseline(struct NvKmsKapiHeadRequestedConfig *hr, struct NvKmsLutRamps *gamma = NULL)
{
    hr->modeSetConfig.olutFpNormScale = NVKMS_OLUT_FP_NORM_SCALE_DEFAULT;
    hr->flags.olutFpNormScaleChanged = NV_TRUE;
    // the output LUT the way nvidia-drm sets a CRTC's gamma (legacy ramps, flags.legacyOlutChanged)
    if (gamma) {
        hr->modeSetConfig.lut.output.enabled = NV_TRUE;
        hr->modeSetConfig.lut.output.pRamps = gamma;
        hr->flags.legacyOlutChanged = NV_TRUE;
    }
    for (unsigned i = 0; i < NVKMS_KAPI_LAYER_MAX; i++) {
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[i];
        nvu_zero(&lr->config.csc, sizeof lr->config.csc);
        lr->config.csc.m[0][0] = 0x10000;
        lr->config.csc.m[1][1] = 0x10000;
        lr->config.csc.m[2][2] = 0x10000;
        lr->flags.cscChanged = NV_TRUE;
    }
}
void NVRMNVDAFramebuffer::surveyBar1()
{
    IOService *nub = pciNub();
    if (!nub) { FBLOG("BAR survey: no IOPCIDevice ancestor"); return; }
    IOMemoryDescriptor *bar = NULL; addr64_t barPa = 0;
    IOItemCount n = nub->getDeviceMemoryCount();
    for (IOItemCount i = 0; i < n; i++) {
        IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
        if (!m) continue;
        if (m->getLength() >= 0x10000000ull) { bar = m; barPa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone); break; }
    }
    if (!bar) { FBLOG("BAR survey: no BAR >= 256 MB"); return; }
    fBarLen = bar->getLength(); fBar1Base = (NvU64)barPa;
    FBLOG("BAR survey: mapping ALL of BAR1 pa 0x%llx len %llu MB", fBar1Base, fBarLen >> 20);
    fBarMem = IOSubMemoryDescriptor::withSubRange(bar, 0, (IOByteCount)fBarLen, kIODirectionNone);
    if (!fBarMem) { FBLOG("BAR survey: withSubRange failed"); return; }
    fBarMap = fBarMem->map(kIOMapInhibitCache);
    if (!fBarMap) { FBLOG("BAR survey: map failed"); return; }
    fBarKva = (void *)fBarMap->getVirtualAddress();
    const NvU64 kRegion = 1ull << 20, kStep = 4096;
    NvU64 regions = fBarLen / kRegion;
    unsigned shown = 0;
    for (NvU64 r = 0; r < regions; r++) {
        volatile uint32_t *base = (volatile uint32_t *)((uint8_t *)fBarKva + r * kRegion);
        uint32_t sum = 0, first = base[0], distinct = 0, prev = first;
        for (NvU64 o = 0; o < kRegion; o += kStep) {
            uint32_t v = base[o / 4];
            sum = sum * 33 + v;
            if (v != prev) { distinct++; prev = v; }
        }
        if (distinct >= 8 && shown < 12) {
            shown++;
            FBLOG("  BAR1+0x%llx (%llu MB): sum 0x%08x distinct %u first 0x%08x <-- STRUCTURED",
                  r * kRegion, r, sum, distinct, first);
        }
    }
    if (!shown) FBLOG("  BAR survey: NO structured region found in %llu MB", fBarLen >> 20);
    else        FBLOG("  BAR survey: %u structured region(s) listed above", shown);
}
bool NVRMNVDAFramebuffer::kapiInit()
{
    struct NvKmsKapiAllocateDeviceParams ap = {}; ap.gpuId = fGpuId; ap.privateData = this; ap.eventCallback = eventCallback;
    if (gDev) {
        fDev = gDev;
        FBLOG("index %u: reusing the shared NvKmsKapiDevice", fIndex);
    } else {
        fDev = fKms->allocateDevice(&ap);
        if (!fDev) { FBLOG("allocateDevice(gpu 0x%x) failed", fGpuId); return false; }
        gDev = fDev;
    }
    if (!fKms->grabOwnership(fDev)) { FBLOG("grabOwnership failed"); return false; }
    struct NvKmsKapiDeviceResourcesInfo *ri = (struct NvKmsKapiDeviceResourcesInfo *)IOMalloc(sizeof *ri);
    if (!ri) return false;
    nvu_zero(ri, sizeof *ri);
    if (!fKms->getDeviceResourcesInfo(fDev, ri)) { FBLOG("getDeviceResourcesInfo failed"); IOFree(ri, sizeof *ri); return false; }
    if (ri->numHeads && ri->numHeads <= 8) gNumHeads = ri->numHeads;
    FBLOG("device: heads %u connectors %u pitchAlignment %u hasVideoMemory %u max %ux%u", ri->numHeads, ri->numConnectors, ri->caps.pitchAlignment, ri->caps.hasVideoMemory, ri->caps.maxWidthInPixels, ri->caps.maxHeightInPixels);
    fNumHeads = ri->numHeads; fNumConnectors = ri->numConnectors;
    NvU32 pitchAlign = ri->caps.pitchAlignment ? ri->caps.pitchAlignment : 256;
    fVtFbBase = ri->vtFbBaseAddress; fVtFbSize = ri->vtFbSize;
    FBLOG("vtFb (NVKMS's view of the console fb): base 0x%llx size %llu", fVtFbBase, fVtFbSize);
    {
        fCurMaxPx     = ri->caps.maxCursorSizeInPixels;
        fCurCompModes = ri->caps.validCursorCompositionModes;
        NvU64 fmts = ri->supportedSurfaceMemoryFormats[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        NvU64 want = ((NvU64)1) << (NvU64)NvKmsSurfaceMemoryFormatX8R8G8B8;
        FBLOG("primary layer: formats 0x%llx X8R8G8B8 %s | compModes 0x%x OPAQUE %s | contiguousPhysMappings %u",
              fmts, (fmts & want) ? "SUPPORTED" : "*** NOT SUPPORTED ***",
              ri->caps.layer[NVKMS_KAPI_LAYER_PRIMARY_IDX].validCompositionModes,
              (ri->caps.layer[NVKMS_KAPI_LAYER_PRIMARY_IDX].validCompositionModes
                 & (1u << NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE)) ? "ok" : "*** NOT VALID ***",
              (unsigned)ri->caps.contiguousPhysicalMappings);
    }
    IOFree(ri, sizeof *ri);
    if (!fKms->declareEventInterest(fDev,
            (1u << NVKMS_EVENT_TYPE_DPY_CHANGED) |
            (1u << NVKMS_EVENT_TYPE_DYNAMIC_DPY_CONNECTED) |
            (1u << NVKMS_EVENT_TYPE_FLIP_OCCURRED)))
        FBLOG("declareEventInterest FAILED");
    else
        FBLOG("declareEventInterest ok (dpy-changed | dynamic-dpy-connected | flip-occurred)");
    if (gNDpy < 0) {
        NvU32 n = 0; NvKmsKapiDisplay handles[NVKMS_KAPI_MAX_CONNECTORS * 2];
        if (!fKms->getDisplays(fDev, &n, NULL) || n == 0) { FBLOG("getDisplays: none"); return false; }
        if (n > sizeof handles / sizeof handles[0]) n = sizeof handles / sizeof handles[0];
        if (!fKms->getDisplays(fDev, &n, handles)) return false;
        FBLOG("getDisplays: %u handle(s); this GPU has %u heads and %u connectors",
              n, fNumHeads, fNumConnectors);
        struct NvKmsKapiDynamicDisplayParams *dd = (struct NvKmsKapiDynamicDisplayParams *)IOMalloc(sizeof *dd);
        if (!dd) return false;
        gNDpy = 0;
        for (NvU32 i = 0; i < n; i++) {
            nvu_zero(dd, sizeof *dd); dd->handle = handles[i];
            if (!fKms->getDynamicDisplayInfo(fDev, dd)) {
                FBLOG("dpy %u/%u 0x%x: getDynamicDisplayInfo FAILED", i, n, handles[i]);
                continue;
            }
            struct NvKmsKapiStaticDisplayInfo si = {};
            NvBool gs = dd->connected ? fKms->getStaticDisplayInfo(fDev, handles[i], &si) : NV_FALSE;
            FBLOG("dpy %u/%u 0x%x: connected %u edid %u B connector 0x%x headMask 0x%x internal %u%s",
                  i, n, handles[i], dd->connected, dd->edid.bufferSize,
                  gs ? (unsigned)si.connectorHandle : 0u, gs ? (unsigned)si.headMask : 0u, gs ? (unsigned)si.internal : 0u,
                  (dd->connected && !gs) ? " STATIC INFO FAILED" : "");
            if (!dd->connected || !gs) continue;
            if (gNDpy >= (int)(sizeof gDpy / sizeof gDpy[0])) { FBLOG("dpy table full at %d", gNDpy); break; }
            struct nvrm_dpy_rec *r = &gDpy[gNDpy++];
            r->handle = handles[i]; r->connector = si.connectorHandle; r->headMask = si.headMask; r->internal = si.internal;
            r->edidSize = dd->edid.bufferSize > sizeof r->edid ? (NvU32)sizeof r->edid : dd->edid.bufferSize;
            nvu_copy(r->edid, dd->edid.buffer, r->edidSize);
        }
        IOFree(dd, sizeof *dd);
        if (getProvider()) getProvider()->setProperty("connected-displays", (unsigned long long)gNDpy, 32);
        FBLOG("ENUMERATED: %d connected display(s) of %u handle(s), %u heads available",
              gNDpy, n, fNumHeads);
        // NVKMS answered and nothing is plugged into this card (its displays are on another GPU): the driver is up as
        // far as it will go, so this start counts as good for the boot guard (nvrm_off.h)
        if (n > 0 && gNDpy == 0) fbGuardCleared("NVKMS lists this card's connectors and none has a display");
    }

    if (gBootHead == -2) {
        gBootHead = -1;
        IOService *rm = getProvider();
        while (rm && strcmp(rm->getMetaClass()->getClassName(), "NVRM") != 0) rm = rm->getProvider();
        const OSSymbol *q = OSSymbol::withCString("NVRMBootRaster");
        uint32_t br[4] = { 0, 0, 0, 0 };
        IOReturn rr = (rm && q) ? rm->callPlatformFunction(q, false, br, NULL, NULL, NULL) : kIOReturnNotFound;
        if (q) q->release();
        int matches = 0; NvKmsKapiDisplay hit = 0;
        for (int pass = 0; pass < 2 && matches == 0 && rr == kIOReturnSuccess; pass++) {
            for (int k = 0; k < gNDpy; k++) {
                bool m1 = false;
                for (NvU32 i = 0; i < 256 && !m1; i++) {
                    struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
                    int r = fKms->getDisplayMode(fDev, gDpy[k].handle, i, &m, &valid, &pref);
                    if (r < 0) break;
                    if (r == 0 || !valid || (pass == 0 && !pref)) continue;
                    const NvU64 pc = m.timings.pixelClockHz, want = br[1];
                    const NvU64 diff = pc > want ? pc - want : want - pc;
                    if (m.timings.hTotal == br[2] && m.timings.vTotal == br[3] && diff * 500 <= want) m1 = true;
                }
                if (m1) { matches++; hit = gDpy[k].handle; if (!(gDpy[k].headMask & (1u << br[0]))) matches = 99; }
            }
            if (matches) FBLOG("boot pair: %s-mode pass found %d display(s)", pass == 0 ? "preferred" : "any", matches);
        }
        if (matches == 1) { gBootDpy = hit; gBootHead = (int)br[0]; }
        if (matches == 1) {
            for (int k = 1; k < gNDpy; k++) if (gDpy[k].handle == hit) {
                struct nvrm_dpy_rec t = gDpy[0]; gDpy[0] = gDpy[k]; gDpy[k] = t;
                FBLOG("boot order: display 0x%x moved to framebuffer index 0 (was %d)", (unsigned)hit, k);
                break;
            }
        }
        else {
            gBootDpy = 0;
        }
        if (matches == 1) {
            for (NvU32 i = 0; i < 256; i++) {
                struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
                int r = fKms->getDisplayMode(fDev, gBootDpy, i, &m, &valid, &pref);
                if (r < 0) break;
                if (r == 0 || !valid) continue;
                const NvU64 pc = m.timings.pixelClockHz, want = br[1]; const NvU64 diff = pc > want ? pc - want : want - pc;
                if (m.timings.hTotal == br[2] && m.timings.vTotal == br[3] && diff * 500 <= want) { gBootMode = m; break; }
            }
        }
        FBLOG("boot pair: NVRM says head %u %ux%u pclk %u (0x%x); %d display(s) match%s -> %s 0x%x head %d", br[0], br[2], br[3],
              br[1], rr, matches, matches == 99 ? " (head not in its mask)" : "",
              gBootHead >= 0 ? "RESERVED for display" : "no reservation, display", (unsigned)gBootDpy, gBootHead);
    }
    // The boot enumeration is done: remember its size. Any framebuffer that starts later (a display plugged in after
    // boot) has fIndex >= gNDpyBoot, which is how it knows it is the new one.
    if (gNDpyBoot < 0) gNDpyBoot = gNDpy;
    if (gNDpy <= 0 || fIndex >= (unsigned)gNDpy) {
        FBLOG("index %u: only %d display(s) connected -- nothing to drive, not starting", fIndex, gNDpy);
        fSpareIndex = gNDpy > 0;
        return false;
    }
    {
        struct nvrm_dpy_rec *r = &gDpy[fIndex];
        fDisplay = r->handle; fConnector = r->connector; fInternal = r->internal;
        if (fInternal) FBLOG("fb%u: display 0x%x is internal -- reporting kIOConnectionBuiltIn", fIndex, fDisplay);
        fEdidSize = (NvU16)(r->edidSize > sizeof fEdid ? sizeof fEdid : r->edidSize);
        nvu_copy(fEdid, r->edid, fEdidSize);
        nvrmEdidPanelSizeMm(fEdid, fEdidSize, &fEdidMmW, &fEdidMmH);
        FBLOG("fb%u: EDID %u B, panel %ux%u mm (imageWidth/imageHeight)", fIndex, (unsigned)fEdidSize,
              (unsigned)fEdidMmW, (unsigned)fEdidMmH);
        // HDMI/DisplayPort audio: build the sink's ELD from its EDID and publish it with the hda-gfx pairing, the same
        // two properties AMD and Intel framebuffers expose, so the card's audio function (NMHDA + AppleHDAController)
        // has the Short Audio Descriptors a codec needs. External connectors only -- an internal eDP panel has no
        // audio. The connector-type guess is HDMI (the common case and what the reports describe); an HDMI VSDB in the
        // EDID confirms it, and NVKMS must still enable the audio stream on the head for a codec to actually appear.
        if (!fInternal && fEdidSize >= 128) {
            uint8_t eld[NVRM_ELD_MAX];
            size_t eldn = nvrm_build_eld(fEdid, fEdidSize, NVRM_ELD_CONN_HDMI, eld);
            if (eldn) {
                if (OSData *d = OSData::withBytes(eld, (unsigned)eldn)) { setProperty("nvda-eld", d); d->release(); }
                setProperty("hda-gfx", "onboard-1");
                FBLOG("fb%u: published %lu-byte ELD and hda-gfx=onboard-1 for display audio",
                      fIndex, (unsigned long)eldn);
            } else
                FBLOG("fb%u: EDID %u B yielded no ELD; no display-audio properties published",
                      fIndex, (unsigned)fEdidSize);
        }
        NvU32 avail = r->headMask & ~gHeadsTaken;
        if (gBootHead >= 0) {
            if (fDisplay == gBootDpy && (avail & (1u << gBootHead))) avail = 1u << gBootHead;
            else if (fDisplay != gBootDpy && (avail & ~(1u << gBootHead))) avail &= ~(1u << gBootHead);
        }
        if (!avail) {
            FBLOG("index %u: display 0x%x headMask 0x%x, every head already taken (0x%x)",
                  fIndex, fDisplay, (unsigned)r->headMask, (unsigned)gHeadsTaken);
            return false;
        }
        fHead = 0; while (fHead < 32 && !(avail & (1u << fHead))) fHead++;
        gHeadsTaken |= (1u << fHead); fOwnsHead = true;
        FBLOG("index %u -> display 0x%x connector 0x%x headMask 0x%x -> head %u (taken now 0x%x)",
              fIndex, fDisplay, (unsigned)fConnector, (unsigned)r->headMask, fHead,
              (unsigned)gHeadsTaken);
    }
    // This mode becomes the head's boot raster (fBootW/fBootH below), which decides the scaler and
    // raster ceilings and which refresh rates addKmsRefreshModes() can publish. A NVKMS-preferred mode
    // is the EDID native timing and wins outright. When no mode is flagged preferred -- common on a
    // second monitor, whose head never runs the firmware's console detection -- the largest validated
    // raster is the panel's native resolution. Taking the first validated mode instead made the boot
    // raster depend on NVKMS enumeration order: a small raster landing first capped fRasterMaxW below
    // the native resolution (so its high-refresh modes were never offered) and let the intermediate
    // rasters flood the mode table, which is why a second high-refresh display came up at 60/75 Hz or
    // black on some boots and correctly on others. Ties keep the higher pixel clock so the boot mode is
    // the native raster's top rate.
    bool have = false; NvU64 bestArea = 0; NvU32 bestClock = 0;
    for (NvU32 i = 0; i < 256; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, preferred = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &preferred);
        if (r < 0) break;
        if (r == 0 || !valid) continue;
        if (preferred) { fMode = m; have = true; break; }
        const NvU64 area = (NvU64)m.timings.hVisible * m.timings.vVisible;
        if (!have || area > bestArea || (area == bestArea && m.timings.pixelClockHz > bestClock)) {
            fMode = m; bestArea = area; bestClock = m.timings.pixelClockHz; have = true;
        }
    }
    if (!have) { FBLOG("no valid mode"); return false; }
    if (gBootHead < 0 && fIndex == 0) fBootConsole = true;
    fKmsMode = fMode; fHaveKmsMode = true;
    if (gBootHead >= 0 && fDisplay == gBootDpy && (int)fHead == gBootHead) {
        // the fallback for a refused firmware timing must be the same raster (it scans out the same surface): the
        // validated mode at the boot size whose refresh is closest to the firmware's
        bool same = false; NvU32 bestGap = 0;
        for (NvU32 i = 0; i < 256; i++) {
            struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, preferred = NV_FALSE;
            int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &preferred);
            if (r < 0) break;
            if (r == 0 || !valid || m.timings.hVisible != gBootMode.timings.hVisible ||
                m.timings.vVisible != gBootMode.timings.vVisible) continue;
            const NvU32 gap = m.timings.refreshRate > gBootMode.timings.refreshRate ?
                m.timings.refreshRate - gBootMode.timings.refreshRate : gBootMode.timings.refreshRate - m.timings.refreshRate;
            if (!same || gap < bestGap) { fKmsMode = m; bestGap = gap; same = true; }
        }
        fHaveKmsMode = same;
        fMode = gBootMode;
        fBootConsole = true;
        FBLOG("boot pair: display 0x%x keeps the firmware's mode %ux%u @%u on head %u", fDisplay,
              fMode.timings.hVisible, fMode.timings.vVisible, fMode.timings.refreshRate, fHead);
    }
    fW = fMode.timings.hVisible; fH = fMode.timings.vVisible;
    {
        const struct NvKmsKapiDisplayModeTimings &k5t = fMode.timings;
        UInt32 k5[13] = { 0x4b350001u, k5t.pixelClockHz, k5t.hVisible, k5t.hSyncStart, k5t.hSyncEnd, k5t.hTotal, k5t.vVisible, k5t.vSyncStart,
                          k5t.vSyncEnd, k5t.vTotal, k5t.refreshRate, k5t.flags.hSyncNeg ? 0u : 1u, k5t.flags.vSyncNeg ? 0u : 1u };
        OSData *k5d = OSData::withBytes(k5, sizeof k5);
        if (k5d) { setProperty("NVRMTiming", k5d); k5d->release(); }
        else FBLOG("K5: NVRMTiming NOT published (no memory) — AGDC link queries will answer NotFound");
    }
    fPitch = (fW * 4 + pitchAlign - 1) & ~(pitchAlign - 1);
    fSize = ((NvU64)fPitch * fH + 0xffff) & ~0xffffull;
    {
        fBootMode = fMode; fBootW = fW; fBootH = fH; fBootPitch = fPitch; fRasterW = fW; fRasterH = fH; fPitchAlign = pitchAlign;
        computeScanoutCaps(pitchAlign);
        FBLOG("fbmodes: boot raster %ux%u pitch %u; scanout allocation sized for %ux%u (%llu KB, boot needs %llu KB)",
              fW, fH, fPitch, fScalerMaxW, fScalerMaxH, fAllocSize / 1024, fSize / 1024);
    }
    FBLOG("display 0x%x head %u connector 0x%x mode %ux%u @%u pitch %u size %llu KB", fDisplay, fHead, fConnector, fW, fH, fMode.timings.refreshRate, fPitch, fSize / 1024);
    NvU64 consBase = 0, consEnd = 0;
    {
        PE_Video pv; nvu_zero(&pv, sizeof pv);
        if (getPlatform() && getPlatform()->getConsoleInfo(&pv) == kIOReturnSuccess
            && pv.v_baseAddr && pv.v_rowBytes && pv.v_height) {
            consBase = (NvU64)pv.v_baseAddr & ~3ull;
            consEnd  = consBase + (NvU64)pv.v_rowBytes * (NvU64)pv.v_height;
            FBLOG("console range BEFORE allocating: 0x%llx .. 0x%llx (%llu B, rowBytes %lu)",
                  consBase, consEnd, consEnd - consBase, (unsigned long)pv.v_rowBytes);
        } else {
            FBLOG("no console range from the platform -- can only silence it, not avoid it");
        }
    }
    bool consoleSilenced = false;
    if (fIndex == 0 && getPlatform()) {
        getPlatform()->setConsoleInfo(0, kPEDisableScreen);
        consoleSilenced = true;
        FBLOG("kernel console DISABLED (kPEDisableScreen) before allocating");
    }
    static const NvBool kTryOrder[2] = { NV_TRUE, NV_FALSE };
    int apertureRounds = 0;
    bool haveAperture = false;
    struct { struct NvKmsKapiMemory *m; void *k; void *u; } parked[8]; int nParked = 0;
    for (int round = 0; round < 10 && !haveAperture; round++) {
     apertureRounds = round;
     if (round) {
        FBLOG("aperture retry round %d: BAR1 not CPU-writable yet -- waiting 5 s, then rolling again", round);
        IOSleep(5000);
     }
     for (int t = 0; t < 2 && !haveAperture; t++) {
      for (int attempt = 0; attempt < 4 && !haveAperture; attempt++) {
        const char *what = kTryOrder[t] ? "VIDMEM" : "SYSMEM";
        struct NvKmsKapiAllocateMemoryParams mp = {}; NvU8 compressible = 0;
        mp.layout = NvKmsSurfaceMemoryLayoutPitch;
        mp.type   = NVKMS_KAPI_ALLOCATION_TYPE_SCANOUT;
        mp.size   = fAllocSize > fSize ? fAllocSize : fSize;
        mp.useVideoMemory = kTryOrder[t];
        mp.compressible   = &compressible;
        fMem = fKms->allocateMemory(fDev, &mp);
        if (!fMem && mp.size != fSize) {
            FBLOG("%s: allocateMemory(%llu) for scaled modes failed -- falling back to the boot size %llu (no mode above %ux%u)",
                  what, (unsigned long long)mp.size, fSize, fW, fH);
            fAllocSize = fSize; fScalerMaxW = fW; fScalerMaxH = fH; fRasterMaxW = fW; mp.size = fSize;
            fMem = fKms->allocateMemory(fDev, &mp);
        }
        if (!fMem) { FBLOG("%s: allocateMemory(%llu) failed", what, fSize); continue; }
        fMemBytes = mp.size;
        fKva = nullptr; fUserPhys = nullptr;
        if (!fKms->mapMemory(fDev, fMem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &fKva) || !fKva) {
            FBLOG("%s: mapMemory(KERNEL) failed", what);
            fKms->freeMemory(fDev, fMem); fMem = nullptr; continue;
        }
        NvBool contig = fKms->isContiguous ? fKms->isContiguous(fMem) : NV_FALSE;
        NvU64 kernelPhys = fPhysForVa ? fPhysForVa(fKva) : 0;
        NvU64 userPhys   = 0;
        if (fKms->mapMemory(fDev, fMem, NVKMS_KAPI_MAPPING_TYPE_USER, &fUserPhys) && fUserPhys)
            userPhys = (NvU64)(uintptr_t)fUserPhys;
        FBLOG("%s: kva %p contiguous %u kernelPhys 0x%llx userPhys 0x%llx %s",
              what, fKva, (unsigned)contig, kernelPhys, userPhys,
              (kernelPhys == userPhys) ? "(same)" : "(different)");
        bool onConsole = (consEnd > consBase) && kernelPhys
                         && (kernelPhys < consEnd) && ((kernelPhys + fMemBytes) > consBase);
        if (onConsole && nParked < 8) {
            FBLOG("%s attempt %d: 0x%llx+%llu OVERLAPS the console 0x%llx..0x%llx -- parking it "
                  "and re-rolling", what, attempt, kernelPhys, fSize, consBase, consEnd);
            parked[nParked].m = fMem; parked[nParked].k = fKva; parked[nParked].u = fUserPhys;
            nParked++;
            fMem = nullptr; fKva = nullptr; fUserPhys = nullptr;
            continue;
        }
        if (onConsole)
            FBLOG("%s: STILL on the console with %d parked -- taking it anyway; the console is "
                  "silenced, so nothing else writes there", what, nParked);
        if (!contig) {
            FBLOG("%s: NOT CONTIGUOUS -- refusing to publish it as one flat range", what);
        } else if (verifyAperture(kernelPhys, "kernelPhys")) {
            fPhys = kernelPhys; haveAperture = true;
        } else if (userPhys && userPhys != kernelPhys && verifyAperture(userPhys, "userPhys")) {
            fPhys = userPhys; haveAperture = true;
        }
        if (haveAperture) {
            // Count a video-memory scanout against the BAR1 the accel budget may hand out. A system-memory scanout
            // does not sit in BAR1, so it is not counted.
            if (kTryOrder[t] && !fScanoutBarBytes) {
                fScanoutBarBytes = fMemBytes;
                OSAddAtomic64((SInt64)fScanoutBarBytes, &gVramDisplayBytes);
            }
            FBLOG("%s ACCEPTED -- aperture 0x%llx", what, fPhys); break;
        }
        FBLOG("%s rejected -- no CPU-writable aperture; freeing and trying the other pool", what);
        if (fKva)      fKms->unmapMemory(fDev, fMem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, fKva);
        if (fUserPhys) fKms->unmapMemory(fDev, fMem, NVKMS_KAPI_MAPPING_TYPE_USER, fUserPhys);
        fKms->freeMemory(fDev, fMem);
        fMem = nullptr; fKva = nullptr; fUserPhys = nullptr;
      }
     }
    }
    if (haveAperture && apertureRounds)
        FBLOG("aperture won on retry round %d (~%d s of waiting) -- firing early is now survivable",
              apertureRounds, apertureRounds * 5);
    for (int i = 0; i < nParked; i++) {
        if (parked[i].k) fKms->unmapMemory(fDev, parked[i].m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, parked[i].k);
        if (parked[i].u) fKms->unmapMemory(fDev, parked[i].m, NVKMS_KAPI_MAPPING_TYPE_USER, parked[i].u);
        fKms->freeMemory(fDev, parked[i].m);
    }
    fParkedCount = (unsigned)nParked;
    cursorInit();
    if (nParked) FBLOG("released %d parked console-overlapping allocation(s)", nParked);
    if (!haveAperture) {
        FBLOG("NEITHER vidmem NOR sysmem gave a CPU-writable scanout aperture -- refusing to "
              "light a head at memory WindowServer cannot reach");
        if (consoleSilenced && getPlatform()) getPlatform()->setConsoleInfo(0, kPEEnableScreen);
        return false;
    }
    publishTimingCaps();
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = fPitch;
    sp.width = fW; sp.height = fH; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
    fSurf = fKms->createSurface(fDev, &sp);
    if (!fSurf) {
        FBLOG("createSurface failed");
        if (fScanoutBarBytes) { OSAddAtomic64(-(SInt64)fScanoutBarBytes, &gVramDisplayBytes); fScanoutBarBytes = 0; }
        if (consoleSilenced && getPlatform()) getPlatform()->setConsoleInfo(0, kPEEnableScreen);
        return false;
    }
    {
        PE_Video cons; nvu_zero(&cons, sizeof cons);
        if (getPlatform() && getPlatform()->getConsoleInfo(&cons) == kIOReturnSuccess
            && cons.v_baseAddr && cons.v_rowBytes && cons.v_width && cons.v_height) {
            FBLOG("console info: base 0x%lx rowBytes %lu %lux%lu depth %lu",
                  (unsigned long)cons.v_baseAddr, (unsigned long)cons.v_rowBytes,
                  (unsigned long)cons.v_width, (unsigned long)cons.v_height,
                  (unsigned long)cons.v_depth);
            fConsPhys  = (NvU64)cons.v_baseAddr & ~3ull;
            fConsPitch = (NvU32)cons.v_rowBytes;
            fConsSize  = (NvU64)cons.v_rowBytes * (NvU64)cons.v_height;
        } else {
            FBLOG("no console info from the platform; falling back to the BAR1 mask");
            fConsPhys  = fPhys & ~0x0FFFFFFFull;
            fConsPitch = 8192; fConsSize = (NvU64)fConsPitch * fH;
        }
    }
    { char sv[8] = { 0 };
      if (PE_parse_boot_argn("-nvfbsurvey", sv, sizeof(sv))) surveyBar1(); }
    if (IOService *nub = pciNub()) {
        IOItemCount nbar = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < nbar; i++) {
            IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
            FBLOG("BAR%u: pa 0x%llx len %llu", (unsigned)i, (unsigned long long)pa,
                  (unsigned long long)m->getLength());
            if (m->getLength() >= 0x10000000ull && !fBar1Base) fBar1Base = (NvU64)pa;
        }
    }
    if (fConsoleAperture && fBar1Base) {
        fConsPhys = fBar1Base; fConsPitch = 8192; fConsSize = (NvU64)fConsPitch * fH;
        fPitch = fConsPitch;
        FBLOG("CONSOLE APERTURE MODE: publishing BAR1+0 (0x%llx) pitch %u %ux%u -- no modeset",
              fConsPhys, fPitch, fW, fH);
    }
    if (fVtFbBase && fVtFbSize) {
        FBLOG("console probe: using NVKMS vtFb 0x%llx (%llu B) instead of the platform's 0x%llx",
              fVtFbBase, fVtFbSize, fConsPhys);
        fConsPhys = fVtFbBase; fConsSize = fVtFbSize;
        if (!fConsPitch) fConsPitch = 8192;
    }
    if (fConsSize) {
        IOMemoryDescriptor *cm = NULL;
        if (IOService *nub = pciNub()) {
            IOItemCount nb = nub->getDeviceMemoryCount();
            for (IOItemCount i = 0; i < nb && !cm; i++) {
                IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
                if (!m) continue;
                addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
                if (fConsPhys < (NvU64)pa || (fConsPhys + fConsSize) > ((NvU64)pa + m->getLength())) continue;
                cm = IOSubMemoryDescriptor::withSubRange(m, (IOByteCount)(fConsPhys - (NvU64)pa),
                                                        (IOByteCount)fConsSize, kIODirectionNone);
            }
        }
        // A console outside this card's windows belongs to another GPU (the Intel GPU on a laptop): it is not mapped,
        // so nothing here reads or writes that GPU's memory.
        if (!cm) FBLOG("console probe: 0x%llx (+%llu) is not in this card's windows (another GPU's console) - not mapped",
                       fConsPhys, fConsSize);
        fConsMem = (IODeviceMemory *)cm;
        if (fConsMem) { fConsMap = fConsMem->map(kIOMapInhibitCache); if (fConsMap) fConsKva = (void *)fConsMap->getVirtualAddress(); }
    }
    FBLOG("console probe phys 0x%llx pitch %u size %llu kva %p | SURFACE phys 0x%llx pitch %u size %llu",
          fConsPhys, fConsPitch, fConsSize, fConsKva, fPhys, fPitch, fSize);
    volatile uint32_t *px = (volatile uint32_t *)fKva;
    bool painted = false;
    if (gBootHead >= 0 ? (fDisplay == gBootDpy && (int)fHead == gBootHead) : (fIndex == 0)) {
        IOService *rm = getProvider();
        while (rm && strcmp(rm->getMetaClass()->getClassName(), "NVRM") != 0) rm = rm->getProvider();
        const OSSymbol *take = OSSymbol::withCString("NVRMBootScreen"), *done = OSSymbol::withCString("NVRMBootScreenDone");
        void *img = NULL; uint32_t dims[3] = { 0, 0, 0 };
        IOReturn r = (rm && take) ? rm->callPlatformFunction(take, false, &img, dims, NULL, NULL) : kIOReturnNotFound;
        // The copy read back as one flat value (0xFFFFFFFF everywhere: the console aperture
        // answered with all ones), and painting it turned the screen white until the login window. A real boot screen is
        // never one value at the corner, the centre and the quarter points, so such a copy is not painted (black instead).
        bool flat = false;
        if (r == kIOReturnSuccess && img && dims[0] && dims[1] && dims[2] >= dims[0] * 4) {
            const uint32_t *im = (const uint32_t *)img; const uint32_t pp = dims[2] / 4, w = dims[0], h = dims[1];
            const uint32_t v0 = im[0];
            flat = im[(size_t)(h / 2) * pp + w / 2] == v0 && im[(size_t)(h / 4) * pp + w / 4] == v0
                && im[(size_t)(3 * h / 4) * pp + 3 * w / 4] == v0 && im[(size_t)(h - 1) * pp + w - 1] == v0;
            if (flat) FBLOG("boot screen: NVRM's copy is one flat value 0x%08x (unreadable aperture) - not painted, black takeover", v0);
        }
        if (!flat && r == kIOReturnSuccess && img && dims[0] && dims[1] && dims[2] >= dims[0] * 4) {
            const uint32_t bg = ((const uint32_t *)img)[0];
            const uint32_t cw = dims[0] < fW ? dims[0] : fW, ch = dims[1] < fH ? dims[1] : fH;
            const uint32_t dx = (fW - cw) / 2, dy = (fH - ch) / 2, sx = (dims[0] - cw) / 2, sy = (dims[1] - ch) / 2;
            const uint32_t pitchPx = fPitch / 4;
            for (uint32_t y = 0; y < fH && (NvU64)(y + 1) * fPitch <= fSize; y++) {
                volatile uint32_t *row = px + (NvU64)y * pitchPx;
                if (y < dy || y >= dy + ch) { for (uint32_t x = 0; x < pitchPx; x++) row[x] = bg; continue; }
                const uint32_t *srow = (const uint32_t *)((const uint8_t *)img + (NvU64)(sy + y - dy) * dims[2]) + sx;
                for (uint32_t x = 0; x < dx; x++) row[x] = bg;
                for (uint32_t x = 0; x < cw; x++) row[dx + x] = srow[x];
                for (uint32_t x = dx + cw; x < pitchPx; x++) row[x] = bg;
            }
            painted = true;
            FBLOG("boot screen: head 0 painted the %ux%u boot image (pitch %u) centred on %ux%u", dims[0], dims[1], dims[2], fW, fH);
        } else {
            FBLOG("boot screen: NVRM had none (0x%x, provider NVRM %s) — black takeover", r, rm ? "found" : "NOT found");
        }
        if (rm && done) rm->callPlatformFunction(done, false, NULL, NULL, NULL, NULL);
        if (take) take->release(); if (done) done->release();
    }
    if (!painted) for (NvU64 i = 0; i < fSize / 4; i++) px[i] = 0x00000000;
    return true;
}


static NVRMNVDAFramebuffer *gFB = nullptr;
// Every started framebuffer, so hot-plug can find the one driving a display that went away and came back.
static NVRMNVDAFramebuffer *gFBs[16];
// Displays connected after boot. At boot every display NVKMS reports connected gets an index; the nubs past that count
// had nothing to drive and their framebuffers did not start, so a monitor switched on or plugged in later stayed dark
// until a restart (the event arrived and was only logged). Apple's drivers bring such a display up as a new
// framebuffer; this does the same: the display is added to the table and the idle nubs are matched again, so the next
// framebuffer starts exactly as it would have at boot with the monitor already on.
static thread_call_t gHotplug = nullptr;
// A framebuffer whose display appeared after boot has to tell the framework its connection is new, through the handler
// the framework registered for kIOFBConnectInterruptType (fConnect). iOService::registerForInterruptType does keep that
// handler, but nothing ever invoked it: CoreDisplay's connect-change path (IOFBInterestCallback ->
// IOFBProcessConnectChange) then never runs, CoreGraphics keeps listing the displays it saw at startup, and the new
// display gets no display pipe. The mode set has to be done first (WindowServer re-probes a framebuffer that has a
// mode), so this is a delayed call.
static thread_call_t gConnectNotify = nullptr;
void NVRMNVDAFramebuffer::connectNotifyFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    if (!me || OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fConnect.proc) { FBLOG("fb%u connect notify: nothing registered yet", me->fIndex); return; }
    me->fConnectFired = true;
    FBLOG("fb%u -> connect interrupt (the framework's registered handler)", me->fIndex);
    me->fConnect.proc(me->fConnect.target, me->fConnect.ref);
}
// A different monitor on this framebuffer's connector: take its EDID (what getDDCBlock hands macOS), its physical size,
// its display-audio ELD, and the scaler/raster ceilings its own mode list allows. The scanout allocation made at start
// is kept: it backs the surface WindowServer is drawing into, and it cannot be replaced under it. A new monitor whose
// largest raster needs more than that allocation is offered what fits (the mode check refuses the rest as larger than
// the scanout allocation) and gets its full size at the next start; the log says so.
void NVRMNVDAFramebuffer::adoptSwappedDisplay()
{
    if (fIndex >= (unsigned)gNDpy) return;
    const struct nvrm_dpy_rec *r = &gDpy[fIndex];
    fEdidSize = (NvU16)(r->edidSize > sizeof fEdid ? sizeof fEdid : r->edidSize);
    nvu_copy(fEdid, r->edid, fEdidSize);
    nvrmEdidPanelSizeMm(fEdid, fEdidSize, &fEdidMmW, &fEdidMmH);
    if (!fInternal && fEdidSize >= 128) {
        uint8_t eld[NVRM_ELD_MAX];
        size_t eldn = nvrm_build_eld(fEdid, fEdidSize, NVRM_ELD_CONN_HDMI, eld);
        if (eldn) { if (OSData *d = OSData::withBytes(eld, (unsigned)eldn)) { setProperty("nvda-eld", d); d->release(); } }
        else removeProperty("nvda-eld");
    }
    const NvU64 keepAlloc = fAllocSize;
    const NvU32 oldW = fScalerMaxW, oldH = fScalerMaxH;
    computeScanoutCaps(fPitchAlign);
    const bool short_ = fAllocSize > keepAlloc;
    fAllocSize = keepAlloc;
    // ceilings past the allocation would let a desktop scan out beyond the end of its memory: keep the ones that fit
    if (short_) { fScalerMaxW = oldW; fScalerMaxH = oldH; fRasterMaxW = oldW; }
    fEdidHits = 0;
    FBLOG("fb%u: adopted the new monitor's EDID (%u B, panel %ux%u mm); ceilings %ux%u -> %ux%u%s", fIndex,
          (unsigned)fEdidSize, (unsigned)fEdidMmW, (unsigned)fEdidMmH, (unsigned)oldW, (unsigned)oldH,
          (unsigned)fScalerMaxW, (unsigned)fScalerMaxH,
          short_ ? "; its largest modes need more scanout memory than this start reserved - full size after a restart" : "");
}
void NVRMNVDAFramebuffer::hotplugKick()
{
    if (!gHotplug) return;
    uint64_t d; clock_interval_to_deadline(1500, kMillisecondScale, &d);
    thread_call_enter_delayed(gHotplug, d);
}
void NVRMNVDAFramebuffer::hotplugFire(thread_call_param_t, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = gFB;
    if (!me || !me->fKms || !me->fDev || gNDpy < 0) return;
    NvU32 n = 0; NvKmsKapiDisplay handles[NVKMS_KAPI_MAX_CONNECTORS * 2];
    if (!me->fKms->getDisplays(me->fDev, &n, NULL) || n == 0) return;
    if (n > sizeof handles / sizeof handles[0]) n = sizeof handles / sizeof handles[0];
    if (!me->fKms->getDisplays(me->fDev, &n, handles)) return;
    struct NvKmsKapiDynamicDisplayParams *dd = (struct NvKmsKapiDynamicDisplayParams *)IOMalloc(sizeof *dd);
    if (!dd) return;
    int added = 0;
    NVRMNVDAFramebuffer *relight[16]; unsigned nrelight = 0;
    IORecursiveLock *sl = startLock();
    if (sl) IORecursiveLockLock(sl);
    for (NvU32 i = 0; i < n; i++) {
        bool known = false; int kdpy = -1;
        for (int k = 0; k < gNDpy; k++) if (gDpy[k].handle == handles[i]) { known = true; kdpy = k; break; }
        nvu_zero(dd, sizeof *dd); dd->handle = handles[i];
        bool dynOk = me->fKms->getDynamicDisplayInfo(me->fDev, dd);
        if (known) {
            // 1401/driver issue #110: switching the monitor to another input and back left it with no picture. The link
            // drops, the head stops scanning out, and this path skipped every display it already knew. A known display
            // that was seen disconnected and is connected again gets its head lit with the same full mode set the
            // display-wake path uses (reflip). Only on that transition, so other display events cause no modeset.
            if (!dynOk) continue;
            // NVKMS names a display by its connector, so a DIFFERENT monitor plugged into the same port comes back as
            // the same handle. Its EDID is what tells them apart: the old EDID stayed published, and macOS kept the
            // previous monitor's name, size and modes (a new monitor still listed as the one it replaced).
            bool swapped = false;
            if (dd->connected && kdpy >= 0 && dd->edid.bufferSize >= 128) {
                const NvU32 sz = dd->edid.bufferSize > sizeof gDpy[kdpy].edid ? (NvU32)sizeof gDpy[kdpy].edid : dd->edid.bufferSize;
                if (sz != gDpy[kdpy].edidSize || memcmp(gDpy[kdpy].edid, dd->edid.buffer, sz) != 0) {
                    nvu_copy(gDpy[kdpy].edid, dd->edid.buffer, sz); gDpy[kdpy].edidSize = sz;
                    swapped = true;
                    FBLOG("hot-plug: display 0x%x has a different monitor now (EDID %u B, maker %02x%02x product %02x%02x)",
                          handles[i], (unsigned)sz, dd->edid.buffer[8], dd->edid.buffer[9], dd->edid.buffer[11], dd->edid.buffer[10]);
                }
            }
            for (unsigned f = 0; f < sizeof gFBs / sizeof gFBs[0]; f++) {
                NVRMNVDAFramebuffer *fb = gFBs[f];
                if (!fb || fb->fDisplay != handles[i]) continue;
                if (!dd->connected) { if (!fb->fDpyLost) FBLOG("hot-plug: display 0x%x went away (fb %u)", handles[i], fb->fIndex); fb->fDpyLost = true; }
                else if ((fb->fDpyLost || swapped) && nrelight < sizeof relight / sizeof relight[0]) {
                    fb->fDpyLost = false; if (swapped) fb->fEdidSwapped = true; fb->retain(); relight[nrelight++] = fb;
                }
            }
            continue;
        }
        if (!dynOk || !dd->connected) continue;
        struct NvKmsKapiStaticDisplayInfo si = {};
        if (!me->fKms->getStaticDisplayInfo(me->fDev, handles[i], &si)) continue;
        if (gNDpy >= (int)(sizeof gDpy / sizeof gDpy[0])) break;
        struct nvrm_dpy_rec *r = &gDpy[gNDpy];
        r->handle = handles[i]; r->connector = si.connectorHandle; r->headMask = si.headMask;
        r->edidSize = dd->edid.bufferSize > sizeof r->edid ? (NvU32)sizeof r->edid : dd->edid.bufferSize;
        nvu_copy(r->edid, dd->edid.buffer, r->edidSize);
        FBLOG("hot-plug: display 0x%x connector 0x%x headMask 0x%x connected after boot -> index %d",
              handles[i], (unsigned)si.connectorHandle, (unsigned)si.headMask, gNDpy);
        gNDpy++; added++;
    }
    if (sl) IORecursiveLockUnlock(sl);
    IOFree(dd, sizeof *dd);
    for (unsigned f = 0; f < nrelight; f++) {
        NVRMNVDAFramebuffer *fb = relight[f];
        bool ok = !OSAddAtomic(0, &fb->fStopping) && !fb->fHeadSlept && fb->reflip();
        FBLOG("hot-plug: display 0x%x is back (fb %u): head %u lit again -> ok %u rc %d",
              fb->fDisplay, fb->fIndex, fb->fHead, (unsigned)ok, fb->fReflipRc);
        if (fb->fEdidSwapped && !OSAddAtomic(0, &fb->fStopping)) {
            fb->fEdidSwapped = false;
            fb->adoptSwappedDisplay();
            // the framework's connect-change path re-reads the EDID (getDDCBlock), the modes and the display's name,
            // as it does when a monitor is plugged into a real Mac; the head is lit first so it re-probes a live display
            if (!gConnectNotify) gConnectNotify = thread_call_allocate(connectNotifyFire, nullptr);
            uint64_t d; clock_interval_to_deadline(500, kMillisecondScale, &d);
            if (gConnectNotify) thread_call_enter1_delayed(gConnectNotify, fb, d);
        }
        fb->release();
    }
    if (!added) return;
    IOService *nub0 = me->getProvider(), *gpu = nub0 ? nub0->getProvider() : nullptr;
    OSIterator *it = gpu ? gpu->getChildIterator(gIOServicePlane) : nullptr;
    unsigned matched = 0;
    if (it) {
        while (IOService *nub = OSDynamicCast(IOService, it->getNextObject())) {
            OSNumber *ix = OSDynamicCast(OSNumber, nub->getProperty("fb-index"));
            if (!ix || nub->getClient() || ix->unsigned32BitValue() >= (unsigned)gNDpy) continue;
            nub->registerService(kIOServiceAsynchronous);   // matches NVRMFB again: the framebuffer starts on this index
            matched++;
        }
        it->release();
    }
    FBLOG("hot-plug: %d display(s) added, %u idle nub(s) matched again", added, matched);
}
static char gTrace[32768]; static volatile SInt32 gTraceLen = 0, gTraceN = 0, gTraceFull = 0;
void NVRMNVDAFramebuffer::trace(const char *s)
{
    if (gTraceFull || gTraceN >= (SInt32)kTraceMax) return;
    size_t n = strlen(s);
    SInt32 off = OSAddAtomic((SInt32)(n + 1), &gTraceLen);
    if (off < 0 || (size_t)off + n + 2 >= sizeof gTrace) { gTraceFull = 1; return; }
    memcpy(gTrace + off, s, n); gTrace[off + n] = '\n';
    OSIncrementAtomic(&gTraceN);
    NVRMNVDAFramebuffer *me = gFB;
    if (!me) return;
    SInt32 len = OSAddAtomic(0, &gTraceLen);
    if (len < 0) return;
    if ((size_t)len >= sizeof gTrace) len = (SInt32)sizeof gTrace - 1;
    const size_t bytes = (size_t)len + 1;
    char *snapshot = (char *)IOMalloc(bytes);
    if (!snapshot) return;
    memcpy(snapshot, gTrace, (size_t)len);
    snapshot[len] = 0;
    OSString *value = OSString::withCString(snapshot);
    IOFree(snapshot, bytes);
    if (value) { me->setProperty("NVRMTrace", value); value->release(); }
    me->setProperty("NVRMTraceCount", (unsigned long long)gTraceN, 32);
}
static IORecursiveLock *startLock()
{
    static IORecursiveLock *volatile gL = nullptr;
    if (!gL) {
        IORecursiveLock *l = IORecursiveLockAlloc();
        if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gL)) IORecursiveLockFree(l);
    }
    return gL;
}
// Waiting for a flip to complete. Polling NVKMS's pending flag with IOSleep(1) overslept each check by a millisecond or
// more, and a frame went through two such waits: harmless in a 16.7 ms frame, but at 144 Hz (6.9 ms) the flip landed a
// vblank late and the desktop ran near half the panel rate while the hardware cursor stayed smooth (1401 "Record
// flicker" on a 143 Hz panel: 56 flips/s median; 540 Hz: 192/s). The flip-occurred event wakes the waiter, and the
// pending flag (still the truth) is re-read at least every 250 us. The bound is the caller's, in microseconds: the
// frame path passes one refresh period and a quarter (flipWaitUs), the old 100 ms is kept only as the stuck limit.
static IOLock *sFlipWaitLock;
static volatile SInt32 sFlipWakeEv[8];
void NVRMNVDAFramebuffer::flipWake(unsigned head)
{
    if (head < 8 && sFlipWaitLock) IOLockWakeup(sFlipWaitLock, (event_t)&sFlipWakeEv[head], false);
}
bool NVRMNVDAFramebuffer::waitFlipDone(uint64_t limitUs)
{
    if (!sFlipWaitLock) { IOLock *l = IOLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&sFlipWaitLock)) IOLockFree(l); }
    uint64_t end = 0; clock_interval_to_deadline(limitUs > 0xffffffffull ? 0xffffffffu : (uint32_t)limitUs, kMicrosecondScale, &end);
    for (;;) {
        NvBool pending = NV_TRUE;
        if (!fKms->getFlipPendingStatus(fDev, fHead, NVKMS_KAPI_LAYER_PRIMARY_IDX, &pending)) return false;
        if (!pending) return true;
        uint64_t now = mach_absolute_time();
        if (now >= end) return false;
        uint64_t next = 0; clock_interval_to_deadline(250, kMicrosecondScale, &next);
        if (next > end) next = end;
        if (!sFlipWaitLock) { IOSleep(1); continue; }
        IOLockLock(sFlipWaitLock);
        IOLockSleepDeadline(sFlipWaitLock, (event_t)&sFlipWakeEv[fHead < 8 ? fHead : 0], next, THREAD_UNINT);
        IOLockUnlock(sFlipWaitLock);
    }
}
// Flip completion-wait controls, surfaced through the debug.nvrmfb_flip_latch* sysctls registered
// below. gFlipLatch gates the wait (default 1; 0 is a debug bypass that lets a surface retire before
// its flip latches, i.e. reintroduces the strobe). The counters report the real wait the compositor
// path takes, which is what an on-machine flicker check reads.
static int gFlipLatch = 1;
static int gFlipLatchWaits = 0, gFlipLatchTimeouts = 0, gFlipLatchMaxUs = 0, gFlipLatchLate = 0;
// A flip still pending this long after its submit is a display that is not flipping (the head is faulted, as before).
// Inside it, a pending flip is only late: the frame path skips one present and the mode-set path waits on.
static const uint64_t kFlipStuckUs = 100000;
// What the compositor's presents did over a window of activity, one line per head, so a "windows stutter, cursor smooth"
// report is decided by the log alone: flips per second against the panel rate, how many missed their vblank, how many
// presents were skipped for a flip still on its way, and the longest completion wait. A window opens at the first
// frame-path present after the previous one closed and closes after 10 s; quiet windows (an idle desktop) print
// nothing, and the number of lines is capped so a long session cannot flood the log. A pause of more than two seconds
// between presents starts the window over: five seconds of smooth dragging, a minute of nothing, then one present would
// otherwise close as "13 flips/s" and read as starvation.
struct NVPresentWindow { uint64_t t0Abs, lastAbs; uint32_t flips, over, late, maxUs; };
static NVPresentWindow gPresentWin[8];
static uint32_t gPresentLines[8];
static const uint64_t kPresentWindowNs = 10000000000ull;
static const uint64_t kPresentIdleNs = 2000000000ull;
static char gPresentMsg[240];
// True when the window closed with a line to print, in gPresentMsg. The caller prints it with IOLog and kprintf only: FBLOG's
// trace allocates, and this runs on the present path under the display gate.
#define PRESENT_STATS_LOG() do { IOLog("NVRM-fb: %s\n", gPresentMsg); kprintf("NVRM-fb: %s\n", gPresentMsg); } while (0)
static bool presentStatsNote(unsigned head, uint64_t boundUs, bool flipped, bool over, bool late, uint32_t waitUs)
{
    if (head >= 8) return false;
    NVPresentWindow &w = gPresentWin[head];
    const uint64_t now = mach_absolute_time();
    if (w.lastAbs) {
        uint64_t idle = 0; absolutetime_to_nanoseconds(now - w.lastAbs, &idle);
        if (idle > kPresentIdleNs) w = NVPresentWindow();
    }
    w.lastAbs = now;
    if (!w.t0Abs) w.t0Abs = now;
    if (flipped) w.flips++;
    if (over) w.over++;
    if (late) w.late++;
    if (waitUs > w.maxUs) w.maxUs = waitUs;
    uint64_t ns = 0; absolutetime_to_nanoseconds(now - w.t0Abs, &ns);
    if (ns < kPresentWindowNs) return false;
    const uint32_t perSec = (uint32_t)(((uint64_t)w.flips * 1000000000ull + ns / 2) / ns);
    // flipWaitUs() is one refresh period and a quarter, so the period is four fifths of the bound
    const uint64_t periodUs = boundUs * 4 / 5;
    const uint32_t panelHz = periodUs ? (uint32_t)((1000000ull + periodUs / 2) / periodUs) : 0;
    // 60 flips in a window is an animating desktop, whatever its rate; a few per window is the clock or a cursor blink
    const bool busy = w.flips >= 60 || w.over || w.late;
    const bool print = busy && (gPresentLines[head] < 40 || (gPresentLines[head] % 10) == 0);
    if (print)
        snprintf(gPresentMsg, sizeof gPresentMsg, "present stats head %u: %u flips in %llu ms (%u/s, panel %u Hz), %u not on the "
                 "panel within a refresh period and a quarter, %u presents skipped for a flip still pending, longest completion wait %u us",
                 head, w.flips, (unsigned long long)(ns / 1000000ull), perSec, panelHz, w.over, w.late, w.maxUs);
    if (busy) gPresentLines[head]++;
    w = NVPresentWindow();
    return print;
}
// How long a committed flip can take to reach the panel: it latches at the next vblank, so one refresh period from the
// mode's raster and pixel clock, plus a quarter period for the completion write and the 250 us re-read. A constant
// 100 ms here held WindowServer's frame for six 60 Hz periods, or sixteen at 165 Hz, whenever a flip ran late.
// The period comes from the raster directly: vblPeriodAbs floors it at 2 ms for the fallback timer, which would make a
// 540 Hz panel's bound eleven periods. Under 1 ms (over 1000 Hz) or over 50 ms the timings are not trusted.
uint64_t NVRMNVDAFramebuffer::flipWaitUs()
{
    const struct NvKmsKapiDisplayModeTimings &t = fMode.timings;
    uint64_t ns = 0;
    if (t.pixelClockHz && t.hTotal && t.vTotal) ns = (uint64_t)t.hTotal * t.vTotal * 1000000000ull / t.pixelClockHz;
    if (ns < 1000000ull || ns > 50000000ull) absolutetime_to_nanoseconds(vblPeriodAbs(), &ns);
    return (ns + ns / 4) / 1000;
}
// The flip before this one has to be on the panel before the surface it replaced may go away. One period covers a
// flip in flight; past kFlipStuckUs since its submit the display is stuck.
int NVRMNVDAFramebuffer::priorFlipState()
{
    if (!fSubmissionSeen) return kPriorDone;
    if (waitFlipDone(flipWaitUs())) return kPriorDone;
    uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - fFlipSubmitAbs, &ns);
    if (ns / 1000 >= kFlipStuckUs) return kPriorStuck;
    return kPriorLate;
}
// The accelerator bounds its own wait for a flip in flight by the same period (NVAccel reads this when it has to wait).
void NVRMNVDAFramebuffer::publishFlipWait()
{
    setProperty("NVRMFlipWaitUs", (unsigned long long)flipWaitUs(), 64);
}
// The line a head fault leaves in the kernel log: how long the flip had been pending and what the latch counters read,
// so a log that ends in a display failure says whether the head faulted and after how long.
void NVRMNVDAFramebuffer::flipFaultLog(const char *why)
{
    uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - fFlipSubmitAbs, &ns);
    FBLOG("presentation blocked: %s on head %u (%llu us since its submit; latch waits %d timeouts %d late %d max %d us)",
          why, fHead, (unsigned long long)(ns / 1000), gFlipLatchWaits, gFlipLatchTimeouts, gFlipLatchLate, gFlipLatchMaxUs);
}
NvBool NVRMNVDAFramebuffer::submitConfig(struct NvKmsKapiRequestedModeSetConfig *cfg,
                                            struct NvKmsKapiModeSetReplyConfig *rep, NvBool commit, bool bootstrap,
                                            bool cursorOnly, bool frame)
{
    if (!fKms || !fDev || !cfg || !rep || fHead >= 8 || fSubmissionFaulted) return NV_FALSE;
    if (!commit) return fKms->applyModeSetConfig(fDev, cfg, rep, NV_FALSE);
    // A cursor-only request leaves the primary layer alone: KAPI does not advance the primary layer's completion slot
    // for it, so there is no primary flip to wait for and the primary tracking below stays as the last present left it.
    if (cursorOnly) {
        const NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
        return ok && rep->flipResult == NV_KMS_FLIP_RESULT_SUCCESS;
    }
    if (fSubmissionSeen) {
        // Initial console acceptance is asynchronous. A still-pending prior flip gets the same one-period wait as a
        // newly committed flip. On the frame path a flip that is merely late costs this present (IN_PROGRESS: the
        // caller keeps the frame on the panel and the next one follows); a mode set waits on until the stuck limit.
        int prior = priorFlipState();
        while (prior == kPriorLate && !frame) prior = priorFlipState();
        if (prior == kPriorStuck) {
            flipFaultLog("prior completion unavailable");
            fSubmissionFaulted = true; return NV_FALSE;
        }
        if (prior == kPriorLate) {
            OSIncrementAtomic((volatile SInt32 *)&gFlipLatchLate);
            if (frame && presentStatsNote(fHead, flipWaitUs(), false, false, true, 0)) PRESENT_STATS_LOG();
            rep->flipResult = NV_KMS_FLIP_RESULT_IN_PROGRESS; return NV_FALSE;
        }
    }
    const bool tracked = cfg->headRequestedConfig[fHead].modeSetConfig.bActive &&
        cfg->headRequestedConfig[fHead].layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].config.surface;
    const uint64_t submit0 = mach_absolute_time();
    const NvBool accepted = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    if (!accepted || rep->flipResult != NV_KMS_FLIP_RESULT_SUCCESS) {
        // A rejected request queued nothing (INVALID_PARAMS fails validation, IN_PROGRESS refuses before queuing), so
        // no surface is in flight and this call alone fails. Latching here turned one refused 8x8 cursor image at
        // login into no zero-copy flips and no mode or refresh changes on every head until reboot.
        // KAPI resets and advances the layer's completion-notifier slot before it submits (its own comment: "What if
        // commit fail?"), so after a rejection the current slot is one NVKMS will never write and reads NOT_BEGUN
        // forever. The previous flip was already proven complete above (or nothing was tracked), so forget it here;
        // the next accepted flip gets a fresh slot that NVKMS does write. A cursor-only request never advanced it.
        if (!cursorOnly) fSubmissionSeen = false;
        // Per head, first 8 then every 64th: the old global cap of 8 was used up by the login cursor rejections,
        // so later rejections (e.g. after a refresh change on the other head) left no trace.
        if (++fRejected <= 8 || (fRejected % 64) == 0) {
            const struct NvKmsKapiHeadRequestedConfig *h = &cfg->headRequestedConfig[fHead];
            const struct NvKmsKapiLayerRequestedConfig *p = &h->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
            FBLOG("commit rejected on head %u result %d accepted %u (#%u) | heads 0x%x active %u mode/act/dpy chg %u%u%u "
                  "primary surf %u chg %u src %ux%u dst %ux%u | cursor surf %u chg %u",
                  fHead, (int)rep->flipResult, (unsigned)accepted, fRejected, cfg->headsMask,
                  (unsigned)h->modeSetConfig.bActive, (unsigned)h->flags.modeChanged, (unsigned)h->flags.activeChanged,
                  (unsigned)h->flags.displaysChanged, p->config.surface ? 1u : 0u, (unsigned)p->flags.surfaceChanged,
                  (unsigned)p->config.srcWidth, (unsigned)p->config.srcHeight, (unsigned)p->config.dstWidth,
                  (unsigned)p->config.dstHeight, h->cursorRequestedConfig.surface ? 1u : 0u,
                  (unsigned)h->cursorRequestedConfig.flags.surfaceChanged);
        }
        setProperty("NVRMRejectedCommits", (unsigned long long)fRejected, 32);
        return NV_FALSE;
    }
    if (cursorOnly) return NV_TRUE;
    if (!tracked) { fSubmissionSeen = false; return NV_TRUE; }
    fSubmissionSeen = true;
    fFlipSubmitAbs = submit0;
    // Initial acceptance permits console handoff, never surface retirement. The owned boot surface remains pinned.
    if (bootstrap || !gFlipLatch) return NV_TRUE;
    OSIncrementAtomic((volatile SInt32 *)&gFlipLatchWaits);
    const uint64_t latch0 = mach_absolute_time();
    if (waitFlipDone(flipWaitUs())) {
        uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - latch0, &ns);
        const int us = (int)(ns / 1000);
        if (us > gFlipLatchMaxUs) gFlipLatchMaxUs = us;
        if (frame && presentStatsNote(fHead, flipWaitUs(), true, false, false, (uint32_t)us)) PRESENT_STATS_LOG();
        return NV_TRUE;
    }
    OSIncrementAtomic((volatile SInt32 *)&gFlipLatchTimeouts);
    // Past one period the flip is late, not lost, and the wait goes on: until it lands the panel may still scan the
    // surface it replaces, and the caller completes the transaction (WindowServer then reuses that surface) as soon as
    // this returns. On the frame path the caller is the accelerator's swap worker, which WindowServer does not wait on
    // past its own one-period bound. Past the stuck limit both the old front and the submitted surface stay alive until
    // a proven recovery or reboot.
    if (frame && (++fLatchLateLogs <= 8 || (fLatchLateLogs % 64) == 0))
        FBLOG("flip on head %u not on the panel within %llu us (#%u) - waiting for it to land",
              fHead, (unsigned long long)flipWaitUs(), fLatchLateLogs);
    for (;;) {
        if (waitFlipDone(flipWaitUs())) {
            // a late flip is reported with the whole wait it took, not the period it first missed
            uint64_t late = 0; absolutetime_to_nanoseconds(mach_absolute_time() - latch0, &late);
            if (frame && presentStatsNote(fHead, flipWaitUs(), true, true, false, (uint32_t)(late / 1000))) PRESENT_STATS_LOG();
            return NV_TRUE;
        }
        uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - submit0, &ns);
        if (ns / 1000 >= kFlipStuckUs) break;
    }
    flipFaultLog("submitted completion unavailable");
    fSubmissionFaulted = true; return NV_FALSE;
}

void NVRMNVDAFramebuffer::sampleFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fKva) return;
    // Every read stays inside the mapping it uses: the console window is only as large as the firmware's screen, which
    // can be smaller than the current mode, and the fixed sample points can lie past a small mode.
    const bool onCons = me->fConsoleAperture && me->fConsKva;
    volatile uint32_t *px = (volatile uint32_t *)(onCons ? me->fConsKva : me->fKva);
    unsigned ppr = me->fPitch / 4;
    if (!ppr) return;
    const unsigned rows = (unsigned)((onCons ? me->fConsSize : me->fSize) / me->fPitch);
    unsigned w = me->fW < ppr ? me->fW : ppr, h = me->fH < rows ? me->fH : rows;
    auto at = [](volatile uint32_t *b, unsigned pp, unsigned bw, unsigned bh, unsigned x, unsigned y) -> uint32_t {
        return (x < bw && y < bh) ? b[(size_t)y * pp + x] : 0; };
    uint32_t sum = 0; unsigned notGrey = 0, n = 0;
    for (unsigned y = 0; y < h; y += 8) for (unsigned x = 0; x < w; x += 8) { uint32_t v = px[(size_t)y * ppr + x] & 0xffffff; sum = sum * 33 + v; if (v != 0x303030) notGrey++; n++; }
    FBLOG("sample %u: sum 0x%08x not-grey %u/%u px(100,100)=0x%08x px(960,540)=0x%08x px(1800,1000)=0x%08x modeset %u | phys 0x%llx size %llu pitch %u %ux%u ok %u flip %d | disp 0x%x head %u conn 0x%x heads %u conns %u reflips %u rc %d",
          me->fSamples, sum, notGrey, n,
          at(px, ppr, w, h, 100, 100), at(px, ppr, w, h, 960, 540), at(px, ppr, w, h, 1800, 1000), me->fModeSet,
          me->fPhys, me->fSize, me->fPitch, me->fW, me->fH, me->fLastOk, me->fFlipResult,
          me->fDisplay, me->fHead, me->fConnector, me->fNumHeads, me->fNumConnectors,
          me->fReflips, me->fReflipRc);
    FBLOG("  vblreg %u vblcalls %llu", me->fVblReg, me->fVblCalls);
    if (me->fConsKva && me->fConsPitch >= 4) {
        volatile uint32_t *cp = (volatile uint32_t *)me->fConsKva;
        const unsigned cppr = me->fConsPitch / 4, crows = (unsigned)(me->fConsSize / me->fConsPitch);
        const unsigned cw = me->fW < cppr ? me->fW : cppr, ch = me->fH < crows ? me->fH : crows;
        uint32_t csum = 0;
        for (unsigned y = 0; y < ch; y += 8) for (unsigned x = 0; x < cw; x += 8) csum = csum * 33 + (cp[(size_t)y * cppr + x] & 0xffffff);
        FBLOG("  cons@0x%llx: sum 0x%08x px(100,100)=0x%06x px(960,540)=0x%06x px(1800,1000)=0x%06x",
              me->fConsPhys, csum, at(cp, cppr, cw, ch, 100, 100) & 0xffffff,
              at(cp, cppr, cw, ch, 960, 540) & 0xffffff, at(cp, cppr, cw, ch, 1800, 1000) & 0xffffff);
    }
    me->fSamples++;
    uint64_t deadline; clock_interval_to_deadline(10, kSecondScale, &deadline); me->scheduleCall(me->fSampler, deadline);
}
bool NVRMNVDAFramebuffer::reflip()
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    if (!fKms || !fDev || !fSurf) return false;
    struct NvKmsKapiSurface *front = fFrontSurf ? fFrontSurf : fSurf;
    struct NvKmsKapiRequestedModeSetConfig *cfg =
        (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep =
        (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr, fGammaOn ? fGamma : NULL);
    hr->modeSetConfig.bActive = NV_TRUE;
    hr->modeSetConfig.numDisplays = 1;
    hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = front;
    lr->config.srcWidth = fW; lr->config.srcHeight = fH;
    lr->config.dstWidth = fRasterW; lr->config.dstHeight = fRasterH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
    lr->config.minPresentInterval = 0;
    lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged =
        lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    NvBool ok = submitConfig(cfg, rep, NV_TRUE);
    fReflipRc = ok ? (int)rep->flipResult : -1;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok ? true : false;
}
// setGammaTable: WindowServer's CLUT (1 or 3 channels, planar, `count` entries of `width` bits; 8-bit entries are bytes,
// wider ones 16-bit words, as IOGraphics passes them) resampled to NVKMS's 1024-entry 16-bit ramps and committed as the
// head's output LUT on its own: no mode change and no flip wait, so Night Shift's gradual changes cannot blank or stall
// the screen. A failed commit keeps the previous LUT; -nvrmnogamma keeps the old behaviour (accepted, not applied).
IOReturn NVRMNVDAFramebuffer::nvrmSetGamma(UInt32 channels, UInt32 count, UInt32 width, const void *data)
{
    char nb[8] = { 0 };
    if (PE_parse_boot_argn("-nvrmnogamma", nb, sizeof nb)) return kIOReturnSuccess;
    if (!data || (channels != 1 && channels != 3) || count < 2 || count > 4096 || width < 1 || width > 16) {
        FBLOG("gamma: refused a table (%u channels, %u entries, %u bits)", channels, count, width);
        return kIOReturnBadArgument;
    }
    if (!fGamma) {
        fGamma = (struct NvKmsLutRamps *)IOMalloc(sizeof *fGamma);
        if (!fGamma) return kIOReturnNoMemory;
    }
    const UInt32 maxIn = (1u << width) - 1, N = NVKMS_LUT_ARRAY_SIZE;
    NvU16 *out[3] = { fGamma->red, fGamma->green, fGamma->blue };
    for (unsigned c = 0; c < 3; c++) {
        const unsigned src = channels == 3 ? c : 0;
        for (UInt32 i = 0; i < N; i++) {
            const uint64_t pos = (uint64_t)i * (count - 1) * 65536 / (N - 1);   // 16.16 position in the source table
            const UInt32 k = (UInt32)(pos >> 16), f = (UInt32)(pos & 0xffff);
            const UInt32 k1 = k + 1 < count ? k + 1 : k;
            const UInt32 a = width <= 8 ? ((const UInt8 *)data)[src * count + k] : ((const UInt16 *)data)[src * count + k];
            const UInt32 b = width <= 8 ? ((const UInt8 *)data)[src * count + k1] : ((const UInt16 *)data)[src * count + k1];
            const uint64_t v = ((uint64_t)a * (65536 - f) + (uint64_t)b * f) >> 16;   // in the source's range
            out[c][i] = (NvU16)((v > maxIn ? maxIn : v) * 65535 / maxIn);
        }
    }
    fGammaOn = true;
    // Not lit yet (boot, display sleep, GPU in standby, console aperture): the next modeset carries the table.
    if (!fModeSet || !fSurf || fHeadSlept || fConsoleAperture || !rmGpuReady()) return kIOReturnSuccess;
    NVRMDisplayGate gate; if (!gate.valid() || !fKms || !fDev) return kIOReturnSuccess;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return kIOReturnSuccess; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    // the head's current state with only the LUT flagged as changed (unflagged fields are not applied)
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->modeSetConfig.olutFpNormScale = NVKMS_OLUT_FP_NORM_SCALE_DEFAULT;
    hr->modeSetConfig.lut.output.enabled = NV_TRUE;
    hr->modeSetConfig.lut.output.pRamps = fGamma;
    hr->flags.legacyOlutChanged = NV_TRUE;
    const NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    if (ok) fGammaCommits++; else fGammaFails++;
    if (fGammaCommits + fGammaFails <= 3 || !ok)
        FBLOG("gamma: %u-channel %u x %u-bit table on head %u -> %s (flipResult %d; %u applied, %u failed)", channels, count,
              width, fHead, ok ? "applied" : "NOT applied, previous LUT kept", (int)rep->flipResult, fGammaCommits, fGammaFails);
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::reflipFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fReflip) return;
    bool rok = me->reflip(); me->fReflips++;
    FBLOG("reflip %u -> ok %u rc %d (head %u display 0x%x surface %p)",
          me->fReflips, (unsigned)rok, me->fReflipRc, me->fHead, me->fDisplay, me->fSurf);
    static const unsigned kGap[8] = { 12, 12, 12, 12, 12, 15, 15, 30 };
    if (me->fReflips >= 8) { return; }
    uint64_t deadline; clock_interval_to_deadline(kGap[me->fReflips], kSecondScale, &deadline);
    me->scheduleCall(me->fReflip, deadline);
}
void NVRMNVDAFramebuffer::hudDraw()
{
    if (!fKva || !fPitch || fW < 1100 || fH < 400) return;
    volatile unsigned int *px = (volatile unsigned int *)fKva;
    unsigned ppr = fPitch / 4;
    const int S = 4, LH = 7 * S + 8, X0 = 28, Y0 = 24, BW = 1040, BH = 8 * LH + 24;
    hudFill(px, ppr, X0 - 10, Y0 - 10, BW, BH, 0xff000000);
    hudFill(px, ppr, X0 - 10, Y0 - 10, BW, 3, 0xff00ff00);
    hudFill(px, ppr, X0 - 10, Y0 - 10 + BH - 3, BW, 3, 0xff00ff00);
    char b[96]; int y = Y0;
    snprintf(b, sizeof b, "NVRM-FB LIVE  FRAME %u", fHudFrames);
    hudText(px, ppr, X0, y, S, 0xff00ff00, b); y += LH;
    snprintf(b, sizeof b, "SURF 0x%llX PITCH %u %uX%u", (unsigned long long)fPhys, fPitch, fW, fH);
    hudText(px, ppr, X0, y, S, 0xffffffff, b); y += LH;
    snprintf(b, sizeof b, "VTFB 0x%llX SZ %llu", (unsigned long long)fVtFbBase, (unsigned long long)fVtFbSize);
    hudText(px, ppr, X0, y, S, fVtFbSize ? 0xff00ff00 : 0xffff0000, b); y += LH;
    snprintf(b, sizeof b, "CONS 0x%llX PITCH %u SZ %llu",
             (unsigned long long)fConsPhys, fConsPitch, (unsigned long long)fConsSize);
    hudText(px, ppr, X0, y, S, 0xffffffff, b); y += LH;
    snprintf(b, sizeof b, "PARKED %u  MODESET %u OK %u FLIP %d",
             fParkedCount, (unsigned)fModeSet, fLastOk, fFlipResult);
    hudText(px, ppr, X0, y, S, fParkedCount ? 0xffff0000 : 0xff00ff00, b); y += LH;
    snprintf(b, sizeof b, "REFLIP %u RC %d  FMT X8R8G8B8 OPAQUE", fReflips, fReflipRc);
    hudText(px, ppr, X0, y, S, 0xffffffff, b); y += LH;
    snprintf(b, sizeof b, "HWCURSOR %s %uX%u APPLY %u RC %d",
             (fCurSurf && fCurKva) ? "ON" : "OFF", fCurDim, fCurDim, fCurApplies, fCurRc);
    hudText(px, ppr, X0, y, S, (fCurSurf && fCurKva) ? 0xff00ff00 : 0xffff0000, b); y += LH;
    snprintf(b, sizeof b, "VBL %llu CSC IDENT ILUT OFF OLUT OFF", (unsigned long long)fVblCalls);
    hudText(px, ppr, X0, y, S, 0xffffffff, b); y += LH;
    snprintf(b, sizeof b, "PX 0x%08X 0x%08X 0x%08X",
             px[100 * ppr + 100], px[540 * ppr + 960], px[1000 * ppr + 1800]);
    hudText(px, ppr, X0, y, S, 0xffffff00, b);
    fHudFrames++;
}
bool NVRMNVDAFramebuffer::cursorInit()
{
    if (!fKms || !fDev) return false;
    uint32_t gate = 1;
    PE_parse_boot_argn("nvcursor", &gate, sizeof gate);
    fCursorEnabled = gate != 0;
    if (!fCursorEnabled) { FBLOG("hardware cursor disabled by nvcursor=0"); return false; }
    if (fCurMaxPx < 32 || !(fCurCompModes & (1u << NVKMS_COMPOSITION_BLENDING_MODE_PREMULT_ALPHA))) {
        FBLOG("cursor unsupported caps: max %u composition 0x%x", fCurMaxPx, fCurCompModes);
        return false;
    }
    if (!fCurStateLock) fCurStateLock = IOSimpleLockAlloc();
    if (!fCursorCall) fCursorCall = thread_call_allocate(cursorFire, this);
    if (!fCurStateLock || !fCursorCall) { FBLOG("cursor: lock or thread call allocation FAILED"); return false; }
    NvU32 dim = fCurMaxPx;
    if (dim > 256) dim = 256;
    NvU64 sz = (NvU64)dim * (NvU64)dim * 4ull;
    struct NvKmsKapiAllocateMemoryParams mp; nvu_zero(&mp, sizeof mp);
    NvU8 compressible = 0;
    mp.layout = NvKmsSurfaceMemoryLayoutPitch;
    mp.type   = NVKMS_KAPI_ALLOCATION_TYPE_SCANOUT;
    mp.size   = sz;
    mp.useVideoMemory = NV_TRUE;
    mp.compressible   = &compressible;
    fCurMem = fKms->allocateMemory(fDev, &mp);
    if (!fCurMem) { FBLOG("cursor: allocateMemory(%llu) FAILED", sz); return false; }
    if (!fKms->mapMemory(fDev, fCurMem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &fCurKva) || !fCurKva) {
        FBLOG("cursor: mapMemory(KERNEL) FAILED");
        fKms->freeMemory(fDev, fCurMem); fCurMem = nullptr; return false;
    }
    nvu_zero(fCurKva, (size_t)sz);
    struct NvKmsKapiCreateSurfaceParams sp; nvu_zero(&sp, sizeof sp);
    sp.planes[0].memory = fCurMem; sp.planes[0].offset = 0; sp.planes[0].pitch = dim * 4;
    sp.width = dim; sp.height = dim;
    sp.format = NvKmsSurfaceMemoryFormatA8R8G8B8;
    fCurSurf = fKms->createSurface(fDev, &sp);
    if (!fCurSurf) {
        FBLOG("cursor: createSurface %ux%u A8R8G8B8 FAILED", dim, dim);
        fKms->unmapMemory(fDev, fCurMem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, fCurKva);
        fKms->freeMemory(fDev, fCurMem); fCurMem = nullptr; fCurKva = nullptr; return false;
    }
    fCurDim = dim;
    // The cursor plane is video memory too, so it counts against the BAR1 the accel budget may use.
    if (!fCursorBarBytes) {
        fCursorBarBytes = sz;
        OSAddAtomic64((SInt64)fCursorBarBytes, &gVramDisplayBytes);
    }
    FBLOG("cursor plane READY: %ux%u A8R8G8B8 pitch %u (device maxCursorSizeInPixels %u, validCompModes 0x%x)",
          dim, dim, dim * 4, fCurMaxPx, fCurCompModes);
    return true;
}
bool NVRMNVDAFramebuffer::cursorApply(bool imageChanged)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    if (!fKms || !fDev || !fCurSurf || !fModeSet) return false;
    struct NvKmsKapiRequestedModeSetConfig *cfg =
        (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep =
        (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    cfg->headRequestedConfig[fHead].modeSetConfig.bActive = NV_TRUE;
    cfg->headRequestedConfig[fHead].modeSetConfig.numDisplays = 1;
    cfg->headRequestedConfig[fHead].modeSetConfig.displays[0] = fDisplay;
    // Cursor only. This used to name the primary surface too (fFrontSurf, read here without the zero-copy flip path's
    // ordering), so every cursor move was also a primary flip: paced to vsync, and when a zero-copy flip latched a newer
    // frame between that read and this commit, the older frame went back on the panel until the next flip - the flash
    // users saw on mouse movement once zero-copy scan-out starts, gone with -nvrmnoflip. (Same finding as PR #40.)
    // The surface only when a new image arrived or whether one shows changes; a move sends the position alone. NVKMS
    // moves the cursor at once through its immediate channel (nvEvoMoveCursorInternal), while naming the surface
    // reprograms it through the core channel, an update that latches at the next vblank and holds back the compositor's
    // flips while the mouse moves.
    struct NvKmsKapiCursorRequestedConfig *cr = &cfg->headRequestedConfig[fHead].cursorRequestedConfig;
    SInt32 x, y; bool visible;
    {
        IOInterruptState state = IOSimpleLockLockDisableInterrupt(fCurStateLock);
        x = fCurX; y = fCurY; visible = fCurVisible;
        IOSimpleLockUnlockEnableInterrupt(fCurStateLock, state);
    }
    struct NvKmsKapiSurface *want = (visible && fCurHaveImage) ? fCurSurf : nullptr;
    const bool nameSurface = imageChanged || !fCurShownKnown || want != fCurShown;
    if (nameSurface) {
        cr->surface = want;
        cr->compParams.compMode    = NVKMS_COMPOSITION_BLENDING_MODE_PREMULT_ALPHA;
        cr->compParams.surfaceAlpha = 0;
        cr->flags.surfaceChanged = NV_TRUE;
    }
    cr->dstX = (NvS16)x; cr->dstY = (NvS16)y;
    cr->flags.dstXYChanged = NV_TRUE;
    NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    fCurRc = ok ? (int)rep->flipResult : -1;
    if (nameSurface) {
        fCurShownKnown = ok && rep->flipResult == NV_KMS_FLIP_RESULT_SUCCESS;
        fCurShown = want;
    }
    if (fCurApplies < 3 || (fCurApplies % 1000) == 0)
        FBLOG("cursor apply %u: ok %u rc %d at %d,%d visible %u",
              fCurApplies, (unsigned)ok, fCurRc, (int)x, (int)y, (unsigned)visible);
    fCurApplies++;
    bool success = ok && rep->flipResult == NV_KMS_FLIP_RESULT_SUCCESS;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return success;
}
static bool nvrmPackCursor(UInt32 *dst, unsigned dim, const UInt32 *src,
                           unsigned width, unsigned height)
{
    if (!dst || !src || !width || !height || width > dim || height > dim || dim > 256)
        return false;
    nvu_zero(dst, (size_t)dim * dim * sizeof(*dst));
    for (unsigned y = 0; y < height; ++y) {
        for (unsigned x = 0; x < width; ++x) {
            UInt32 p = src[y * width + x], a = p >> 24;
            UInt32 r = (((p >> 16) & 255) * a + 127) / 255;
            UInt32 g = (((p >> 8) & 255) * a + 127) / 255;
            UInt32 b = ((p & 255) * a + 127) / 255;
            dst[y * dim + x] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    return true;
}
IOReturn NVRMNVDAFramebuffer::setCursorImage(void *cursorImage)
{
    if (!fCurKva || !fCurSurf) return kIOReturnUnsupported;
    NVRMDisplayGate gate; if (!gate.valid()) return kIOReturnUnsupported;
    return setCursorImageLocked(cursorImage);
}
IOReturn NVRMNVDAFramebuffer::setCursorImageLocked(void *cursorImage)
{
    size_t bytes = (size_t)fCurDim * fCurDim * sizeof(UInt32);
    UInt32 *staging = (UInt32 *)IOMalloc(bytes);
    if (!staging) return kIOReturnNoMemory;
    nvu_zero(staging, bytes);
    IOHardwareCursorDescriptor d; nvu_zero(&d, sizeof d);
    d.majorVersion = kHardwareCursorDescriptorMajorVersion;
    d.minorVersion = kHardwareCursorDescriptorMinorVersion;
    d.width = fCurDim; d.height = fCurDim;
    d.bitDepth = 32;
    d.supportedSpecialEncodings = kTransparentEncodedPixel;
    IOHardwareCursorInfo ci; nvu_zero(&ci, sizeof ci);
    ci.majorVersion = kHardwareCursorInfoMajorVersion;
    ci.minorVersion = kHardwareCursorInfoMinorVersion;
    ci.hardwareCursorData = (UInt8 *)staging;
    bool converted = convertCursorImage(cursorImage, &d, &ci);
    bool packed = converted && nvrmPackCursor((UInt32 *)fCurKva, fCurDim, staging,
                                               ci.cursorWidth, ci.cursorHeight);
    IOFree(staging, bytes);
    if (!packed) {
        bool hadImage = fCurHaveImage;
        fCurHaveImage = false;
        if (hadImage) cursorApply();
        static unsigned refused;
        if (++refused <= 4) FBLOG("cursor conversion declined %u (discovery or unsupported image)", refused);
        return kIOReturnUnsupported;
    }
    __asm__ __volatile__("sfence" ::: "memory");
    fCurHaveImage = true;
    static unsigned images;
    if (++images <= 4) FBLOG("K8 cursor image %u: %ux%u hot %u,%u plane %u",
        images, (unsigned)ci.cursorWidth, (unsigned)ci.cursorHeight,
        (unsigned)ci.cursorHotSpotX, (unsigned)ci.cursorHotSpotY, fCurDim);
    if (!cursorApply(true)) { fCurHaveImage = false; cursorApply(); return kIOReturnUnsupported; }
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::setCursorState(SInt32 x, SInt32 y, bool visible)
{
    if (!fCurSurf || !fCursorCall || !fCurStateLock) return kIOReturnUnsupported;
    IOInterruptState state = IOSimpleLockLockDisableInterrupt(fCurStateLock);
    const bool same = x == fCurX && y == fCurY && visible == fCurVisible && fCurRc == NV_KMS_FLIP_RESULT_SUCCESS;
    fCurX = x; fCurY = y; fCurVisible = visible;
    IOSimpleLockUnlockEnableInterrupt(fCurStateLock, state);
    if (!same) scheduleCall(fCursorCall, 0);
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::cursorFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    me->cursorApply();   // takes the display gate
}
void NVRMNVDAFramebuffer::barsFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fBars || !me->fKva) return;
    if (me->fBarPaints < 3000) {
    static const uint32_t kCell[12] = {
        0xff0000, 0x00ff00, 0x0000ff, 0xffffff,
        0x808080, 0x272727, 0x000000, 0xffff00,
        0x00ffff, 0xff00ff, 0x804000, 0x0080ff,
    };
    volatile uint32_t *px = (volatile uint32_t *)me->fKva;
    unsigned ppr = me->fPitch / 4, w = me->fW, h = me->fH;
    unsigned cw = w / 4, ch = h / 3;
    for (unsigned y = 0; y < h; y++) {
        volatile uint32_t *row = px + (size_t)y * ppr;
        unsigned r = y / ch; if (r > 2) r = 2;
        unsigned yin = y - r * ch;
        NvBool bottom = (yin >= ch / 2);
        for (unsigned x = 0; x < w; x++) {
            unsigned c = x / cw; if (c > 3) c = 3;
            unsigned xin = x - c * cw;
            if (xin < 8 || yin < 8) { row[x] = 0xff000000; continue; }
            row[x] = kCell[r * 4 + c] | (bottom ? 0xff000000u : 0x00000000u);
        }
    }
    if (me->fBarPaints == 0)
        FBLOG("PROBE GRID: 12 known colours 4x3, top half X=0x00 / bottom half X=0xff, 10 Hz for 5 min");
    }
    me->fBarPaints++;
    me->hudDraw();
    if (me->fBarPaints == 3000)
        FBLOG("PROBE GRID done (%u paints); HUD keeps running, surface back to WindowServer", me->fBarPaints);
    unsigned gap = (me->fBarPaints < 3000) ? 100 : 200;
    uint64_t deadline; clock_interval_to_deadline(gap, kMillisecondScale, &deadline);
    me->scheduleCall(me->fBars, deadline);
}
uint64_t NVRMNVDAFramebuffer::vblPeriodAbs()
{
    const struct NvKmsKapiDisplayModeTimings &t = fMode.timings;
    uint64_t ns = 16666667ull;
    if (t.pixelClockHz && t.hTotal && t.vTotal) {
        const uint64_t q = (uint64_t)t.hTotal * t.vTotal * 1000000000ull / t.pixelClockHz;
        // 2 ms..50 ms is 500 Hz..20 Hz. When NVKMS refuses the hardware vblank this period paces the
        // software fallback that drives WindowServer, so the window has to reach the real panel rate:
        // a 4 ms floor (250 Hz) dropped 280/300/360 Hz panels back to the 60 Hz default and the frame
        // delivery beat against the scan-out. A sub-2 ms period only comes from an implausible pixel
        // clock, so it still falls back to 60 Hz rather than spinning the timer.
        if (q >= 2000000ull && q <= 50000000ull) ns = q;
    }
    uint64_t abs = 0; nanoseconds_to_absolutetime(ns, &abs); return abs;
}
// The hardware vblank registration is retried every kHwVblRetryTicks ticks of the fallback timer (5 s at 60 Hz, 2 s at
// 144 Hz), a dozen times per mode set; a refusal that holds that long is taken as final and the timer stays the VBL.
static const unsigned kHwVblRetryTicks = 300, kHwVblMaxTries = 12;
static bool hwVblRetryDue(unsigned tries, unsigned ticksSinceTry)
{
    return tries < kHwVblMaxTries && ticksSinceTry >= kHwVblRetryTicks;
}
void NVRMNVDAFramebuffer::hwVblIntr(NvU64 param, NvU64)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)(uintptr_t)param;
    uint64_t now = 0; clock_get_uptime(&now); me->fHwVblLast = now;
    me->scheduleCall(me->fHwVblCall, 0);
}
void NVRMNVDAFramebuffer::hwVblDeliver(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fHwVblCb) return;
    if (me->fVbl.proc) { me->fVbl.proc(me->fVbl.target, me->fVbl.ref); me->fVblCalls++; }
    if (++me->fHwVblCalls == 1) FBLOG("hw vblank: first VBL from head %u's real vblank (period %llu abs)", me->fHead, me->vblPeriodAbs());
}
void NVRMNVDAFramebuffer::vblFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fVblTimer) return;
    // With two monitors (75 Hz + 60 Hz), head 1 never latched a pure flip, so its hw vblank was never
    // registered and its VBL stayed this free-running timer - not locked to the panel's scan-out, so the second screen
    // jittered while head 0 (real vblank) was smooth. The latch was only a proxy for "this head has a mode": registering
    // with no mode divided by zero in SetupVBlankRgSemaphoreForApiHead (pixelClock 0). A committed modeset on this head
    // with a non-zero pixel clock, held for ~1 s of ticks, is that condition stated directly.
    if (me->fModeSet && !me->fConsoleAperture && me->fMode.timings.pixelClockHz) { if (me->fModeSetTicks < 1000) me->fModeSetTicks++; }
    else me->fModeSetTicks = 0;
    // A refused registration is tried again (hwVblRetryDue): one try at 60 mode ticks left a display whose head was
    // still settling on the free-running timer for the whole session, its frames beating against the real scan-out.
    // A new mode set starts the tries over.
    me->fVblTicks++;
    if (me->fModeSetTicks == 0) me->fHwVblTries = 0;
    if (me->fHwVblWanted && !me->fHwVblCb && (sFlipLatched[me->fHead] > 0 || me->fModeSetTicks >= 60) &&
        (me->fHwVblTries == 0 || hwVblRetryDue(me->fHwVblTries, me->fVblTicks - me->fHwVblTickAtTry))) {
        me->fHwVblTries++; me->fHwVblTickAtTry = me->fVblTicks;
        me->fHwVblCb = me->fKms->registerVblankIntrCallback(me->fDev, me->fHead, hwVblIntr, (NvU64)(uintptr_t)me);
        if (me->fHwVblCb) me->fHwVblWanted = false;
        FBLOG("hw vblank on head %u: %s after %d latched flips, %u mode ticks (try %u)", me->fHead,
              me->fHwVblCb ? "REGISTERED" : hwVblRetryDue(me->fHwVblTries, kHwVblRetryTicks) ? "REFUSED by NVKMS - timer stays, trying again" : "REFUSED by NVKMS - timer stays",
              (int)sFlipLatched[me->fHead], me->fModeSetTicks, me->fHwVblTries);
    }
    {   uint64_t nowv = 0; clock_get_uptime(&nowv); const uint64_t per = me->vblPeriodAbs();
        const bool hwLive = me->fHwVblLast && nowv - me->fHwVblLast < 2 * per;
        if (!hwLive && me->fVbl.proc) { me->fVbl.proc(me->fVbl.target, me->fVbl.ref); me->fVblCalls++; } }
    uint64_t period = 0, now = 0;
    period = me->vblPeriodAbs();
    clock_get_uptime(&now);
    if (me->fVblNext == 0) me->fVblNext = now;
    me->fVblNext += period;
    if (me->fVblNext <= now) me->fVblNext = now + period;
    me->scheduleCall(me->fVblTimer, me->fVblNext);
}
bool NVRMNVDAFramebuffer::setHeadActive(bool active)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    if (!fKms || !fDev) return false;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr, active && fGammaOn ? fGamma : NULL);
    hr->modeSetConfig.bActive = active ? NV_TRUE : NV_FALSE;
    hr->flags.activeChanged = NV_TRUE;
    if (active) {
        hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = fMode;
        hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = fSurf;
        lr->config.srcWidth = fW; lr->config.srcHeight = fH;
        lr->config.dstWidth = fRasterW; lr->config.dstHeight = fRasterH;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
        lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged =
            lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    }
    NvBool ok = submitConfig(cfg, rep, NV_TRUE);
    int rc = (int)rep->flipResult;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    FBLOG("HEAD PROBE: setHeadActive(%u) on head %u -> ok %u rc %d", (unsigned)active, fHead, (unsigned)ok, rc);
    return ok ? true : false;
}
void NVRMNVDAFramebuffer::headProbeFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fHeadProbe) return;
    unsigned gap = 12;
    switch (me->fProbeStep) {
        case 0: FBLOG("HEAD PROBE step 0: BLANKING the head -- panel should go dark NOW");
                me->setHeadActive(false); break;
        case 1: FBLOG("HEAD PROBE step 1: RESTORING the head -- panel should come back NOW");
                me->setHeadActive(true); break;
        default: FBLOG("HEAD PROBE done; head left ACTIVE"); return;
    }
    me->fProbeStep++;
    uint64_t d; clock_interval_to_deadline(gap, kSecondScale, &d);
    me->scheduleCall(me->fHeadProbe, d);
}
bool NVRMNVDAFramebuffer::applyMode()
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    if (fConsoleAperture) {
        FBLOG("applyMode: skipped (console aperture mode -- the GOP drives this panel)");
        fModeSet = true; return true;
    }
    if (fModeSet) return true;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr, fGammaOn ? fGamma : NULL);
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay; hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = fSurf; lr->config.srcWidth = fW; lr->config.srcHeight = fH; lr->config.dstWidth = fRasterW; lr->config.dstHeight = fRasterH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
    lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    nvrmUnownedHeadsOff(cfg);
    NvBool ok = submitConfig(cfg, rep, NV_TRUE, true);
    // A refused first mode set leaves WindowServer asserting in CGXMappedDisplayStart and crash-looping (an RTX 3060 at
    // 1920x1080, 2.1.0 and 2.2.0). First retry with the mode NVKMS validated for the same raster, when it differs from the
    // firmware's timing (it scans out the same surface).
    if (!ok && fHaveKmsMode && memcmp(&fMode, &fKmsMode, sizeof fMode) != 0 &&
        fKmsMode.timings.hVisible == fRasterW && fKmsMode.timings.vVisible == fRasterH) {
        FBLOG("applyMode: firmware timing pclk %u %ux%u refused; retrying with NVKMS's validated mode pclk %u %ux%u @%u",
              fMode.timings.pixelClockHz, fMode.timings.hTotal, fMode.timings.vTotal, fKmsMode.timings.pixelClockHz,
              fKmsMode.timings.hTotal, fKmsMode.timings.vTotal, fKmsMode.timings.refreshRate);
        hr->modeSetConfig.mode = fKmsMode;
        nvu_zero(rep, sizeof *rep);
        ok = submitConfig(cfg, rep, NV_TRUE, true);
        if (ok) fMode = fKmsMode;
    }
    // Then keep the mode the firmware is already scanning out and only put our surface on the head: a flip, no mode set.
    // KAPI takes the flip path only when no head in the request changes mode, activity or displays, so the request holds
    // this head alone. The 2.2.0 log of that RTX 3060 shows the validated mode equals the firmware's, so the refusal is
    // not the timing; NVKMS now logs its reason codes ("[kapi] set mode refused").
    if (!ok) {
        nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
        cfg->headsMask = 1u << fHead;
        nvrmHeadConfigBaseline(hr, fGammaOn ? fGamma : NULL);
        hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = fMode;
        lr->config.surface = fSurf; lr->config.srcWidth = fW; lr->config.srcHeight = fH;
        lr->config.dstWidth = fRasterW; lr->config.dstHeight = fRasterH;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged =
            lr->flags.dstWHChanged = NV_TRUE;
        ok = submitConfig(cfg, rep, NV_TRUE, true);
        FBLOG("applyMode: mode set refused; flip onto the firmware's running %ux%u mode on head %u -> %u (flipResult %d)",
              fRasterW, fRasterH, fHead, (unsigned)ok, (int)rep->flipResult);
    }
    fLastOk = (unsigned)ok; fFlipResult = (int)rep->flipResult;
    FBLOG("applyModeSetConfig -> %u (flipResult %d)", ok, (int)rep->flipResult);
    if (ok && fKms->framebufferConsoleDisabled) {
        fKms->framebufferConsoleDisabled(fDev);
        FBLOG("framebufferConsoleDisabled() sent -- NVKMS releases the old GOP console");
    }
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    fModeSet = ok;
    fCurShownKnown = false;   // the mode set named no cursor
    publishFlipWait();
    fHwVblTries = 0;
    return ok;
}

static NvU64 propU64(IOService *s, const char *key)
{
    OSNumber *n = OSDynamicCast(OSNumber, s->getProperty(key));
    return n ? n->unsigned64BitValue() : 0;
}
static IOPMPowerState kFBPowerStates[2] = {
    { kIOPMPowerStateVersion1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
    { kIOPMPowerStateVersion1, kIOPMDeviceUsable, kIOPMPowerOn, kIOPMPowerOn, 0, 0, 0, 0, 0, 0, 0, 0 },
};
static void nvrmDiscoverBar1(IOService *provider, NvU64 *length, NvU64 *base)
{
    *length = 0; *base = 0;
    for (IOService *nub = provider; nub; nub = nub->getProvider()) {
        if (!nub->metaCast("IOPCIDevice")) continue;
        // BAR1 is the largest range after BAR0 (the registers). WAS: the first range of at least 256 MB, so a 64 or
        // 128 MB BAR1 read as 0 and got the budget and park ceiling of an unknown window.
        const IOItemCount count = nub->getDeviceMemoryCount();
        IOMemoryDescriptor *best = nullptr;
        for (IOItemCount i = 1; i < count; i++) {
            IOMemoryDescriptor *memory = nub->getDeviceMemoryWithIndex(i);
            if (memory && memory->getLength() >= (32ull << 20) && (!best || memory->getLength() > best->getLength())) best = memory;
        }
        if (best) {
            *length = best->getLength();
            *base = best->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
        }
        return;
    }
}
bool NVRMNVDAFramebuffer::start(IOService *provider)
{
    { if (nvrm_driver_off()) return false; }
    {
        int gate = 0;
        if (!PE_parse_boot_argn("nvfb", &gate, sizeof gate) || !gate) {
            IOLog("NVRMFB: boot-arg nvfb=1 absent: NOT starting (boot-loop guard)\n");
            return false;
        }
    }
    fFlipCacheLock = IOSimpleLockAlloc();
    if (!fFlipCacheLock) return false;
    fKms = (struct NvKmsKapiFunctionsTable *)(uintptr_t)propU64(provider, "nvkms-kapi");
    fGpuId = (NvU32)propU64(provider, "gpu-id");
    fIndex = (unsigned)propU64(provider, "fb-index");
    nvrmDiscoverBar1(provider, &fBarLen, &fBar1Base);
    FBLOG("BAR1 metadata: %llu MB at 0x%llx; normal surface budget %llu MB",
          (unsigned long long)(fBarLen >> 20), (unsigned long long)fBar1Base,
          (unsigned long long)(nvrmVramBudget(fBarLen) >> 20));
    if (fIndex == 0 && gFB != this) { retain(); gFB = this; }
    if (fIndex == 0 && !gHotplug) gHotplug = thread_call_allocate(hotplugFire, nullptr);
    // WhateverGreen (with Lilu) replaces IOFramebuffer::initFB. Unless boot-arg gfxrst=4 is set, the first initFB of any
    // framebuffer copies the boot console from the address WhateverGreen saved at boot. macOS frees that mapping when
    // the console moves (to a framebuffer, or to a new PCI window), and the copy then faults: a kernel panic in
    // WindowServer's IOFramebuffer::open (postOpen) as this display opens. This driver cannot repair the saved address;
    // gfxrst=4 turns the copy off, and this driver draws its own takeover screen.
    // Its class is looked up rather than its service: a Lilu plugin's service is started but never registered, so a
    // service match does not find it.
    if (fIndex == 0) {
        const OSSymbol *name = OSSymbol::withCStringNoCopy("WhateverGreen");
        const bool weg = name && OSMetaClass::getMetaClassWithName(name);
        OSSafeReleaseNULL(name);
        int gfxrst = -1;
        if (weg && !(PE_parse_boot_argn("gfxrst", &gfxrst, sizeof gfxrst) && gfxrst == 4))
            FBLOG("WhateverGreen is loaded without boot-arg gfxrst=4: its boot screen copy can read freed memory when "
                  "this display opens (kernel panic in IOFramebuffer::postOpen). Add gfxrst=4 to boot-args.");
    }
    if (fIndex < sizeof gFBs / sizeof gFBs[0]) {
        IORecursiveLock *sl = startLock();
        if (sl) IORecursiveLockLock(sl);
        if (gFBs[fIndex] != this) { retain(); if (gFBs[fIndex]) gFBs[fIndex]->release(); gFBs[fIndex] = this; }
        if (sl) IORecursiveLockUnlock(sl);
    }
    // A display index at or past the boot enumeration means this display appeared after boot (hotplugFire added it to
    // gDpy and re-registered the idle nub). Such an instance has to fire the framework's connect handler once its head
    // has a mode -- see connectNotifyFire and enableController.
    fHotplugBorn = (gNDpyBoot >= 0 && (int)fIndex >= gNDpyBoot);
    if (fHotplugBorn && !gConnectNotify) gConnectNotify = thread_call_allocate(connectNotifyFire, nullptr);
    if (fHotplugBorn) FBLOG("fb%u: a display appeared after boot -- the connect interrupt will be fired once the head has a mode", fIndex);
    fPhysForVa = (nvrm_phys_for_va_t)(uintptr_t)propU64(provider, "phys-for-va");
    if (!fKms || !fPhysForVa || !fKms->getFlipPendingStatus || !fKms->dupMemory) { FBLOG("nub lacks nvkms-kapi/phys-for-va properties"); return false; }
    setProperty("IOFBDependentID", (unsigned long long)0x4E56524D00000001ull, 64);
    setProperty("IOFBDependentIndex", (unsigned long long)fIndex, 32);
    setProperty("IOFBMemorySize", (unsigned long long)(8ull << 30), 64);
    IORecursiveLock *sl = startLock();
    if (!sl) { FBLOG("start lock alloc failed -- refusing to start"); return false; }
    IORecursiveLockLock(sl); bool kapiOk = kapiInit();
    if (!kapiOk && fOwnsHead) { gHeadsTaken &= ~(1u << fHead); fOwnsHead = false; }
    IORecursiveLockUnlock(sl);
    if (!kapiOk) {
        // nvfbheads=4 starts four framebuffer instances; the ones past the connected displays have nothing to drive and
        // stop here on every healthy boot. Say so, so a log with one lit head is not read as a refused framebuffer.
        if (fSpareIndex)
            FBLOG("index %u: spare framebuffer instance (a display-less index) -- not started, expected", fIndex);
        else
            FBLOG("kapiInit failed -- refusing to start. super::start() was NOT called, so there is "
                  "nothing half-registered to unwind and the machine stays up without a display.");
        return false;
    }
    setName("NVRMFramebuffer");
    if (fInternal) publishBacklight();   // before super::start(): IOFramebuffer::open looks for "backlight"
    if (!super::start(provider)) {
        NVRMDisplayGate gate; if (gate.valid() && fOwnsHead) { gHeadsTaken &= ~(1u << fHead); fOwnsHead = false; }
        FBLOG("super::start failed after kapiInit succeeded"); return false;
    }
    fSleepTap = addFramebufferNotification(&NVRMNVDAFramebuffer::sleepTap, this, nullptr);
    if (!fSleepTap) FBLOG("sleep/wake notification tap not registered - a wake after dark wakes may leave the accelerator off");
    fVblTimer = thread_call_allocate(vblFire, this);
    if (fVblTimer) { uint64_t d; clock_interval_to_deadline(16667, kMicrosecondScale, &d); scheduleCall(fVblTimer, d); }
    FBLOG("vbl timer %s", fVblTimer ? "armed (fallback)" : "ALLOCATE FAILED");
    fHwVblCall = thread_call_allocate(hwVblDeliver, this);
    UInt32 hwvblArg = 1; PE_parse_boot_argn("nvhwvbl", &hwvblArg, sizeof hwvblArg);
    fHwVblWanted = hwvblArg && fHwVblCall && fKms && fDev && fKms->registerVblankIntrCallback && fHead < 8;
    FBLOG("hw vblank on head %u: %s", fHead, !hwvblArg ? "OFF (boot-arg nvhwvbl=0) - the timer is the VBL"
                                       : fHwVblWanted ? "deferred until NVKMS latches a flip on this head" : "not available - the timer stays the VBL");
    fReflip = thread_call_allocate(reflipFire, this);
    fGpuWake = thread_call_allocate(gpuWakeFire, this);
    FBLOG("reflip keep-alive NOT armed: enableController's modeset owns the head, no boot-time re-syncs");
    fBars = thread_call_allocate(barsFire, this);
    uint32_t hudGate = 0;
    if (fBars && PE_parse_boot_argn("nvhud", &hudGate, sizeof hudGate) && hudGate) {
        uint64_t d; clock_interval_to_deadline(45, kSecondScale, &d);
        scheduleCall(fBars, d);
        FBLOG("PROBE GRID + HUD ARMED (boot-arg nvhud=1): fires at t+45 s");
    } else {
        FBLOG("probe grid + HUD OFF -- macOS only (add nvhud=1 to boot-args to show them)");
    }
    fHeadProbe = thread_call_allocate(headProbeFire, this);
    FBLOG("head probe retired (answered: we own head %u)", fHead);
    uint32_t sampleGate = 0;
    if (PE_parse_boot_argn("nvfbsample", &sampleGate, sizeof sampleGate) && sampleGate) {
        fSampler = thread_call_allocate(sampleFire, this);
        if (fSampler) { uint64_t deadline; clock_interval_to_deadline(10, kSecondScale, &deadline); scheduleCall(fSampler, deadline); }
        FBLOG("pixel sampler ARMED (boot-arg nvfbsample=1): every 10 s");
    } else {
        FBLOG("pixel sampler OFF -- add nvfbsample=1 to boot-args to arm it");
    }
    registerPowerDriver(this, kFBPowerStates, 2);
    FBLOG("registerPowerDriver(2 states) done");
    FBLOG("started on %s", provider->getName());
    fbGuardCleared("the framebuffer has its scanout memory and a display mode");
    return true;
}
void NVRMNVDAFramebuffer::scheduleCall(thread_call_t call, uint64_t deadline)
{
    if (!call || !fFlipCacheLock) return;
    IOInterruptState state = IOSimpleLockLockDisableInterrupt(fFlipCacheLock);
    if (!fStopping) {
        retain();
        const bool alreadyQueued = deadline ? thread_call_enter_delayed(call, deadline) : thread_call_enter(call);
        if (alreadyQueued) release();
    }
    IOSimpleLockUnlockEnableInterrupt(fFlipCacheLock, state);
}
void NVRMNVDAFramebuffer::stop(IOService *provider)
{
    if (fFlipCacheLock) {
        IOInterruptState state = IOSimpleLockLockDisableInterrupt(fFlipCacheLock);
        fStopping = 1;
        IOSimpleLockUnlockEnableInterrupt(fFlipCacheLock, state);
    }
    if (fHwVblCb && fKms && fDev) {
        fKms->unregisterVblankIntrCallback(fDev, fHead, fHwVblCb); fHwVblCb = nullptr;
    }
    if (fIndex < sizeof gFBs / sizeof gFBs[0]) {
        IORecursiveLock *sl = startLock();
        if (sl) IORecursiveLockLock(sl);
        bool mine = gFBs[fIndex] == this;
        if (mine) gFBs[fIndex] = nullptr;
        if (sl) IORecursiveLockUnlock(sl);
        if (mine) release();
    }
    thread_call_t *calls[] = {&fHwVblCall, &fReflip, &fGpuWake, &fBars, &fVblTimer, &fSampler, &fHeadProbe, &fCursorCall};
    for (unsigned i = 0; i < sizeof calls / sizeof calls[0]; ++i) {
        thread_call_t call = *calls[i];
        if (!call) continue;
        if (thread_call_cancel(call)) release();
        while (!thread_call_free(call)) {
            if (thread_call_cancel(call)) release();
            IOSleep(1);
        }
        *calls[i] = nullptr;
    }
    // vblFire retries a refused registration; one that passed its stopping check before fStopping was set can register
    // after the unregister above. The timer is drained now, so a callback registered that way is removed here.
    if (fHwVblCb && fKms && fDev) {
        fKms->unregisterVblankIntrCallback(fDev, fHead, fHwVblCb); fHwVblCb = nullptr;
    }
    if (fBacklight) { fBacklight->detach(this); OSSafeReleaseNULL(fBacklight); }
    if (fGamma) { IOFree(fGamma, sizeof *fGamma); fGamma = nullptr; fGammaOn = false; }
    if (fWakeGate) { if (IOWorkLoop *wl = getWorkLoop()) wl->removeEventSource(fWakeGate); OSSafeReleaseNULL(fWakeGate); }
    if (fSleepTap) { fSleepTap->remove(); fSleepTap = nullptr; }
    super::stop(provider);
}

// The internal panel's brightness control, when NVKMS registered a backlight for it (needs the NBCI _DSM).
void NVRMNVDAFramebuffer::publishBacklight()
{
    IOService *rm = getProvider();
    while (rm && strcmp(rm->getMetaClass()->getClassName(), "NVRM") != 0) rm = rm->getProvider();
    const OSSymbol *fn = OSSymbol::withCString("NVRMBacklight");
    NvU32 level = 0;
    IOReturn r = (rm && fn) ? rm->callPlatformFunction(fn, false, &level, NULL, NULL, NULL) : kIOReturnNotFound;
    OSSafeReleaseNULL(fn);
    if (r != kIOReturnSuccess) { FBLOG("fb%u: internal panel, but NVKMS registered no backlight (0x%x) -- no brightness control", fIndex, r); return; }
    fBacklight = NVRMBacklightHandler::withRM(rm, level);
    if (!fBacklight) { FBLOG("fb%u: backlight handler allocation failed", fIndex); return; }
    fBacklight->setName("backlight");
    if (!fBacklight->attach(this)) { OSSafeReleaseNULL(fBacklight); FBLOG("fb%u: backlight handler attach failed", fIndex); return; }
    fBacklight->registerService(kIOServiceSynchronous);
    FBLOG("fb%u: panel backlight at %u%% -- brightness keys and slider through AppleBacklightDisplay", fIndex, (unsigned)level);
}

// AppleBacklightDisplay::probe takes only an LCD connection. IODisplay itself never asks: NVRMFB answers
// kConnectionSupportsAppleSense with unsupported, so the display's identity still comes from the EDID.
IOReturn NVRMNVDAFramebuffer::getAppleSense(IOIndex connectIndex, UInt32 *senseType, UInt32 *primary, UInt32 *extended, UInt32 *displayType)
{
    if (connectIndex != 0 || !fInternal || !fBacklight) return kIOReturnUnsupported;
    if (senseType) *senseType = 0;
    if (primary) *primary = 0;
    if (extended) *extended = 0;
    if (displayType) *displayType = kGenericLCD;
    return kIOReturnSuccess;
}

void NVRMNVDAFramebuffer::free()
{
    if (fFlipCacheLock) { IOSimpleLockFree(fFlipCacheLock); fFlipCacheLock = nullptr; }
    if (fCurStateLock) { IOSimpleLockFree(fCurStateLock); fCurStateLock = nullptr; }
    fConsKva = nullptr; OSSafeReleaseNULL(fConsMap); OSSafeReleaseNULL(fConsMem);
    super::free();
}

IOReturn NVRMNVDAFramebuffer::enableController()
{
    FBLOG("enableController");
    const bool ok = applyMode();
    // A display that appeared after boot: tell the framework, through the connect handler it registered (fConnect),
    // that this connection is new. It has to happen after the mode set -- WindowServer re-probes a framebuffer that
    // has a mode, and firing it earlier leaves the display undetected -- so it is a delayed call.
    if (ok && fHotplugBorn && !fConnectFired && gConnectNotify) {
        uint64_t d; clock_interval_to_deadline(1500, kMillisecondScale, &d);
        thread_call_enter1_delayed(gConnectNotify, this, d);
        FBLOG("fb%u: connect interrupt scheduled (1.5 s after the mode set)", fIndex);
    }
    return ok ? kIOReturnSuccess : kIOReturnError;
}
IOReturn NVRMNVDAFramebuffer::setDisplayMode(IODisplayModeID mode, IOIndex depth)
{
    FBLOG("fb%u setDisplayMode 0x%x depth %d (current 0x%x)", fIndex, (unsigned)mode, (int)depth, (unsigned)fCurId);
    if (depth != 0) return kIOReturnUnsupportedMode;
    if (mode == kModeId) {
        if (fCurId == kModeId) return applyMode() ? kIOReturnSuccess : kIOReturnError;
        return switchMode(kModeId, fBootMode, fBootW, fBootH);
    }
    int i = dtIndex(mode);
    if (i < 0) return kIOReturnUnsupportedMode;
    if (mode == fCurId) return fModeSet ? kIOReturnSuccess : (applyMode() ? kIOReturnSuccess : kIOReturnError);
    struct NvKmsKapiDisplayMode m = {};
    if (!edidModeFor(&fDT[i], &m)) dtToKapi(&fDT[i], &m);
    const NvU32 w = fDT[i].horizontalScaled ? fDT[i].horizontalScaled : fDT[i].horizontalActive;
    const NvU32 h = fDT[i].verticalScaled ? fDT[i].verticalScaled : fDT[i].verticalActive;
    return switchMode(mode, m, w, h);
}
void NVRMNVDAFramebuffer::dtToKapi(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *m)
{
    nvu_zero(m, sizeof *m);
    struct NvKmsKapiDisplayModeTimings &k = m->timings;
    k.pixelClockHz = (NvU32)d->pixelClock;
    k.hVisible = d->horizontalActive; k.hSyncStart = d->horizontalActive + d->horizontalSyncOffset;
    k.hSyncEnd = k.hSyncStart + d->horizontalSyncPulseWidth; k.hTotal = d->horizontalActive + d->horizontalBlanking;
    k.vVisible = d->verticalActive; k.vSyncStart = d->verticalActive + d->verticalSyncOffset;
    k.vSyncEnd = k.vSyncStart + d->verticalSyncPulseWidth; k.vTotal = d->verticalActive + d->verticalBlanking;
    const NvU64 tot = (NvU64)k.hTotal * k.vTotal;
    k.refreshRate = tot ? (NvU32)((d->pixelClock * 1000ull + tot / 2) / tot) : 0;
    k.flags.hSyncPos = (d->horizontalSyncConfig & 1) ? 1 : 0; k.flags.hSyncNeg = k.flags.hSyncPos ? 0 : 1;
    k.flags.vSyncPos = (d->verticalSyncConfig & 1) ? 1 : 0;   k.flags.vSyncNeg = k.flags.vSyncPos ? 0 : 1;
}
bool NVRMNVDAFramebuffer::edidModeFor(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *out)
{
    if (!fKms || !fDev || !fDisplay || !d->pixelClock) return false;
    const NvU32 ht = d->horizontalActive + d->horizontalBlanking, vt = d->verticalActive + d->verticalBlanking;
    for (NvU32 i = 0; i < 256; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &pref);
        if (r < 0) break;
        if (r == 0 || !valid) continue;
        const struct NvKmsKapiDisplayModeTimings &k = m.timings;
        if (k.hVisible != d->horizontalActive || k.vVisible != d->verticalActive || k.hTotal != ht || k.vTotal != vt) continue;
        const NvU64 pc = k.pixelClockHz, want = d->pixelClock, diff = pc > want ? pc - want : want - pc;
        if (diff * 500 > want) continue;
        *out = m; fEdidHits++;
        return true;
    }
    return false;
}
// The scanout allocation, and the scaler and raster ceilings that gate which modes reach the user, have to cover the
// LARGEST raster this panel can drive -- a HiDPI desktop is backed by a surface the full size of the raster it sits on,
// and an unscaled native raster is scanned out of a surface its own size. Sizing those from the boot raster (the old
// 2x-boot, capped at 3840x2160 / 7680 wide) stranded every panel whose native raster was wider or taller than twice the
// mode the firmware happened to light: a 32:9 ultrawide (5120x1440, 7680x2160) that booted at 1920x1080 or 2560x1440 was
// capped at 3840 wide and offered only 16:9 rasters, and 5K/6K panels lost their native height. The ceilings now follow
// the panel's own NVKMS-validated mode list, bounded by the 8K scanout limit; the boot raster is the floor, and the
// 2x-boot heuristic is kept only as a fallback when the mode list reports nothing (so a panel with an unreadable mode
// list still gets one scaled step up). A size NVRAM/BAR cannot satisfy is retried smaller by the allocation loop.
void NVRMNVDAFramebuffer::computeScanoutCaps(NvU32 pitchAlign)
{
    NvU32 pmw = 0, pmh = 0;
    if (fKms && fDev && fDisplay) {
        for (NvU32 i = 0; i < 256; i++) {
            struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
            int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &pref);
            if (r < 0) break;
            if (r == 0 || !valid) continue;
            const struct NvKmsKapiDisplayModeTimings &k = m.timings;
            if (k.flags.interlaced || k.flags.doubleScan || !k.pixelClockHz) continue;
            if (k.hVisible > pmw) pmw = k.hVisible;
            if (k.vVisible > pmh) pmh = k.vVisible;
        }
    }
    NvU32 mw = pmw ? pmw : fW * 2;   // the panel's widest validated raster, or one step up when the list is empty
    NvU32 mh = pmh ? pmh : fH * 2;
    if (mw < fW) mw = fW;
    if (mh < fH) mh = fH;
    if (mw > 7680) mw = 7680;        // Blackwell/Ada scanout ceiling; above this NVKMS validates nothing anyway
    if (mh > 4320) mh = 4320;
    fScalerMaxW = mw; fScalerMaxH = mh; fRasterMaxW = mw;
    // nvfbhidpi=1 (opt-in, 2.0.5): HiDPI "looks like" modes are backed by a desktop up to twice the panel's raster
    // (2560x1600 panel, looks like 1440x900 -> 2880x1800 desktop) that NVKMS scales down to the raster, the way NVIDIA's
    // driver separates ViewportIn from ViewportOut. Capped at the largest raster, every such desktop was refused as
    // "larger than the scanout allocation" and custom HiDPI modes reverted (GH #221). It reserves more video memory for
    // scanout, so it stays opt-in until owners of HiDPI panels report it; a reserve that cannot be allocated falls back
    // to the boot size below.
    { uint32_t hidpi = 0;
      if (PE_parse_boot_argn("nvfbhidpi", &hidpi, sizeof hidpi) && hidpi) {
          fScalerMaxW = mw * 2 > 7680 ? 7680 : mw * 2;
          fScalerMaxH = mh * 2 > 4320 ? 4320 : mh * 2;
          FBLOG("nvfbhidpi: scaled desktops up to %ux%u on a %ux%u raster", fScalerMaxW, fScalerMaxH, mw, mh);
      } }
    const NvU64 mp_ = ((NvU64)fScalerMaxW * 4 + pitchAlign - 1) & ~((NvU64)pitchAlign - 1);
    fAllocSize = (mp_ * fScalerMaxH + 0xffff) & ~0xffffull;
    if (fAllocSize < fSize) fAllocSize = fSize;
}
// Users on HDMI and DisplayPort (RTX 3050/3060/4060, 144-240 Hz panels) saw only 60 Hz: macOS's EDID parse handed
// validateDetailedTiming nothing above 60 Hz for those panels, while NVKMS parses the same EDID (CTA-861 and DisplayID
// blocks, link limits) into its own validated mode list. Every refresh rate NVKMS validated for a raster macOS already
// lists, and that macOS did not offer, is added here: unscaled (macOS builds the scaled
// HiDPI desktops on top of them itself). The timings are NVKMS's exactly, so setDisplayMode commits a mode NVKMS itself validated.
// IDs are 0x10000 | NVKMS mode index << 8 | copy (0 = unscaled): below Apple's reserved 0x80000000 range and stable
// across repeated setDetailedTimings calls for the same display.
unsigned NVRMNVDAFramebuffer::addKmsRefreshModes(unsigned n)
{
    if (!fKms || !fDev || !fDisplay) return n;
    const unsigned cap = sizeof fDT / sizeof fDT[0], fromMac = n;
    // Publish a desktop timing for the boot raster too; the basic boot mode
    // alone is not a desktop-capable detailed timing.
    unsigned added = 0;
    for (NvU32 i = 0; i < 256 && n < cap; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &pref);
        if (r < 0) break;
        const struct NvKmsKapiDisplayModeTimings &k = m.timings;
        const NvU64 tot = (NvU64)k.hTotal * k.vTotal;
        const NvU32 hz = tot ? (NvU32)((k.pixelClockHz + tot / 2) / tot) : 0;
        // The first pass (before macOS proposes anything) lists every mode NVKMS enumerated and what became of it, so
        // a report with one refresh rate shows whether the panel's EDID offered another or NVKMS dropped it.
        if (!fromMac && !fKmsModesLogged)
            FBLOG("fb%u NVKMS mode %u: %ux%u @%u Hz pclk %u valid %u preferred %u%s%s", fIndex, i, k.hVisible, k.vVisible, hz,
                  k.pixelClockHz, (unsigned)valid, (unsigned)pref, r == 0 ? " (empty slot)" : "",
                  (k.flags.interlaced || k.flags.doubleScan) ? " interlaced/doublescan" : "");
        if (r == 0 || !valid) continue;
        if (k.flags.interlaced || k.flags.doubleScan || !k.pixelClockHz || k.hTotal <= k.hVisible || k.vTotal <= k.vVisible ||
            k.hSyncStart < k.hVisible || k.hSyncEnd < k.hSyncStart || k.vSyncStart < k.vVisible || k.vSyncEnd < k.vSyncStart) continue;
        int tmpl = -1; bool have = false, raster = false;
        for (unsigned j = 0; j < n; j++) {
            const IODetailedTimingInformationV2 &d = fDT[j];
            if (d.horizontalActive != k.hVisible || d.verticalActive != k.vVisible) continue;
            if (j < fromMac) raster = true;
            // only an unscaled entry is this refresh rate on the panel; macOS's scaled desktops built on a rate we
            // offered must not hide that rate's own mode (e.g. 1344x756@100 listed, 1920x1080@100 gone)
            if (d.horizontalScaled || d.verticalScaled) continue;
            const UInt64 dt = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
            const NvU32 dhz = dt ? (NvU32)((d.pixelClock + dt / 2) / dt) : 0;
            if (dhz + 1 >= hz && dhz <= hz + 1) { have = true; break; }
            if (tmpl < 0 && j < fromMac && !d.horizontalScaled && !d.verticalScaled) tmpl = (int)j;
        }
        // A raster macOS never proposed is offered too when NVKMS validated it for this display and it fits the scanout
        // memory and the scaler. Without this, rasters that exist only in the EDID's DisplayID extension (most 100-240 Hz
        // modes, issue #29) and every raster above a 1080p firmware raster (a 4K monitor stuck at 1920x1080, issue #49)
        // were never listed. switchMode re-checks the memory, and NVKMS validates the commit itself.
        // Only rasters LARGER than the boot raster: 1.14.0 offered every smaller NVKMS raster too (1280x1024, 1680x1050,
        // 720x576 ... on a 2560x1440 panel) and the login window came up black on a 2560x1440@180 DisplayPort monitor
        // until the user logged in blind; with this block removed it showed again on that machine. Smaller desktops still
        // come from macOS's own scaled modes on the native raster.
        const bool larger = fBootW && k.hVisible >= fBootW && k.vVisible >= fBootH && (k.hVisible > fBootW || k.vVisible > fBootH);
        const bool bootRaster = fBootW && k.hVisible == fBootW && k.vVisible == fBootH;
        if (!raster && !have && (bootRaster || larger) && fMemBytes && k.hVisible <= fRasterMaxW && k.vVisible <= fScalerMaxH) {
            NvU32 pitch = 0;
            if (nvrmCheckedPitch(k.hVisible, fPitchAlign, &pitch) && (NvU64)pitch * k.vVisible <= fMemBytes) raster = true;
        }
        if (have || !raster) {
            if (!fromMac && !fKmsModesLogged)
                FBLOG("fb%u NVKMS mode %u: not offered (%s)", fIndex, i, have ? "already listed at this rate"
                      : !fBootW ? "no boot raster yet" : (!bootRaster && !larger) ? "smaller than the boot raster (desktops that size come from macOS's scaled modes)"
                      : "does not fit the scanout memory or the scaler");
            continue;
        }
        IODetailedTimingInformationV2 b;
        if (tmpl >= 0) b = fDT[tmpl];
        else { nvu_zero(&b, sizeof b); b.horizontalActive = k.hVisible; b.verticalActive = k.vVisible; b.signalConfig = kIODigitalSignal; }
        b.pixelClock = b.minPixelClock = b.maxPixelClock = k.pixelClockHz;
        b.horizontalBlanking = k.hTotal - k.hVisible; b.horizontalSyncOffset = k.hSyncStart - k.hVisible;
        b.horizontalSyncPulseWidth = k.hSyncEnd - k.hSyncStart;
        b.verticalBlanking = k.vTotal - k.vVisible; b.verticalSyncOffset = k.vSyncStart - k.vVisible;
        b.verticalSyncPulseWidth = k.vSyncEnd - k.vSyncStart;
        b.horizontalSyncConfig = k.flags.hSyncPos ? 1 : 0; b.verticalSyncConfig = k.flags.vSyncPos ? 1 : 0;
        b.detailedTimingModeID = 0x10000u | (i << 8);
        // NVKMS validated this mode, so above DP 1.4's uncompressed limit (HBR3 x4 = 25.92 Gbit/s payload) it drives it
        // with DSC. Without DSC fields macOS judges the mode by its uncompressed bandwidth, declares the display offline and
        // the panel goes black (issue #77, RTX 5080 4K@120: 7 offline / 35 virtual framebuffers -> 0 / 0 with these
        // values). 257 = 16.06 bpp x16, what NVKMS itself picked there ("EVO-DSC bppX16 257 sliceCount 4"); the slices
        // follow its count for 4K, two below; the reporter's slice height (the full frame) is kept.
        if ((NvU64)k.pixelClockHz * 24 > 25920000000ull && !b.dscCompressedBitsPerPixel) {
            const UInt16 slices = k.hVisible >= 3840 ? 4 : 2;
            b.dscCompressedBitsPerPixel = 257;
            b.dscSliceWidth = (UInt16)((k.hVisible + slices - 1) / slices);
            b.dscSliceHeight = (UInt16)k.vVisible;
            FBLOG("fb%u %ux%u pclk %u needs DSC: %u slices of %u, 16.06 bpp", fIndex, k.hVisible, k.vVisible, k.pixelClockHz,
                  slices, b.dscSliceWidth);
        }
        fDT[n++] = b; added++;
        FBLOG("fb%u refresh rate from NVKMS: %ux%u @%u Hz (pclk %u) as dt 0x%x", fIndex, k.hVisible, k.vVisible, hz,
              k.pixelClockHz, b.detailedTimingModeID);
    }
    if (!fromMac) fKmsModesLogged = true;
    setProperty("NVRMRefreshModesAdded", (unsigned long long)added, 32);
    return n;
}
int NVRMNVDAFramebuffer::dtIndex(IODisplayModeID id) const
{
    for (unsigned i = 0; i < fNDT; i++) if ((IODisplayModeID)fDT[i].detailedTimingModeID == id) return (int)i;
    return -1;
}
IOReturn NVRMNVDAFramebuffer::switchMode(IODisplayModeID id, const struct NvKmsKapiDisplayMode &m, NvU32 w, NvU32 h)
{
    NVRMDisplayGate gate; if (!gate.valid()) return kIOReturnNoMemory;
    if (fSubmissionFaulted) return kIOReturnNotReady;
    if (!fKms || !fDev || !fMem || fConsoleAperture) return kIOReturnNotReady;
    NvU32 pitch = 0;
    if (!nvrmCheckedPitch(w, fPitchAlign, &pitch)) return kIOReturnBadArgument;
    if (!w || !h || (NvU64)pitch * h > fMemBytes) {
        FBLOG("switchMode 0x%x: %ux%u pitch %u needs %llu B, the scanout holds %llu -- REFUSED",
              (unsigned)id, w, h, pitch, (unsigned long long)pitch * h, (unsigned long long)fMemBytes);
        return kIOReturnNoMemory;
    }
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
    sp.width = w; sp.height = h; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
    struct NvKmsKapiSurface *ns = fKms->createSurface(fDev, &sp);
    if (!ns) { FBLOG("switchMode 0x%x: createSurface %ux%u pitch %u FAILED", (unsigned)id, w, h, pitch); return kIOReturnNoMemory; }
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); fKms->destroySurface(fDev, ns); return kIOReturnNoMemory; }
    NvBool ok = NV_FALSE;
    for (int commit = 0; commit < 2; commit++) {
        nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
        cfg->headsMask = 1u << fHead;
        struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
        nvrmHeadConfigBaseline(hr, fGammaOn ? fGamma : NULL);
        hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = m;
        hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = ns; lr->config.srcWidth = w; lr->config.srcHeight = h;
        lr->config.dstWidth = m.timings.hVisible; lr->config.dstHeight = m.timings.vVisible;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
        nvrmUnownedHeadsOff(cfg);
        ok = submitConfig(cfg, rep, commit ? NV_TRUE : NV_FALSE);
        FBLOG("switchMode 0x%x: %s %ux%u -> raster %ux%u @%u mHz pclk %u -> %u (flipResult %d)", (unsigned)id,
              commit ? "COMMIT" : "test", w, h, m.timings.hVisible, m.timings.vVisible, m.timings.refreshRate,
              m.timings.pixelClockHz, (unsigned)ok, (int)rep->flipResult);
        if (!ok) break;
    }
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (!ok) { if (!fSubmissionFaulted) fKms->destroySurface(fDev, ns); return kIOReturnUnsupportedMode; }
    struct NvKmsKapiSurface *old = fSurf;
    fSurf = ns; fFrontSurf = ns; fMode = m; fW = w; fH = h; fPitch = pitch;
    fRasterW = m.timings.hVisible; fRasterH = m.timings.vVisible; fCurId = id; fModeSet = true; fModeSwitches++;
    fCurShownKnown = false;   // the mode set named no cursor
    publishFlipWait();
    fHwVblTries = 0;   // a new mode gets its own hardware-vblank tries (fModeSet stays set, so vblFire would not reset them)
    if (old && old != ns) fKms->destroySurface(fDev, old);
    {
        const struct NvKmsKapiDisplayModeTimings &k5t = fMode.timings;
        UInt32 k5[13] = { 0x4b350001u, k5t.pixelClockHz, k5t.hVisible, k5t.hSyncStart, k5t.hSyncEnd, k5t.hTotal, k5t.vVisible, k5t.vSyncStart,
                          k5t.vSyncEnd, k5t.vTotal, k5t.refreshRate, k5t.flags.hSyncNeg ? 0u : 1u, k5t.flags.vSyncNeg ? 0u : 1u };
        OSData *k5d = OSData::withBytes(k5, sizeof k5);
        if (k5d) { setProperty("NVRMTiming", k5d); k5d->release(); }
    }
    setProperty("NVRMModeSwitches", (unsigned long long)fModeSwitches, 32);
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::validateDetailedTiming(void *description, IOByteCount descripSize)
{
    if (!description) return kIOReturnBadArgument;
    IODetailedTimingInformationV2 *d = nullptr;
    if (descripSize == sizeof(IOFBDisplayModeDescription))
        d = &((IOFBDisplayModeDescription *)description)->timingInfo.detailedInfo.v2;
    else if (descripSize == sizeof(IODetailedTimingInformationV2))
        d = (IODetailedTimingInformationV2 *)description;
    fValidateCalls++;
    if (!d || !fKms || !fDev || !fDisplay) return kIOReturnBadArgument;
    const char *why = nullptr;
    if (d->horizontalScaledInset || d->verticalScaledInset) why = "inset (no kIOScaleCanSupportInset)";
    else if (d->scalerFlags & ~(UInt32)kIOScaleStretchToFit) why = "rotation/axis flags";
    else if (d->horizontalBorderLeft | d->horizontalBorderRight | d->verticalBorderTop | d->verticalBorderBottom) why = "borders";
    else if (!d->pixelClock || !d->horizontalActive || !d->verticalActive) why = "empty timing";
    else if (d->horizontalScaled || d->verticalScaled) {
        const UInt32 sw = d->horizontalScaled, sh = d->verticalScaled;
        if (!sw || !sh) why = "one scaled axis";
        else if (sw > fScalerMaxW || sh > fScalerMaxH) why = "desktop larger than the scanout allocation";
        else if (sw > 2 * d->horizontalActive || sh > 2 * d->verticalActive) why = "downscale beyond 2x";
    }
    if (!why) {
        struct NvKmsKapiDisplayMode m = {};
        if (!edidModeFor(d, &m)) {
            dtToKapi(d, &m);
            if (!fKms->validateDisplayMode(fDev, fDisplay, &m)) why = "NVKMS validateDisplayMode refused the raster (not an EDID mode)";
        }
    }
    if (fValidateCalls <= 48 || why == nullptr)
        FBLOG("fb%u validateDetailedTiming #%u id 0x%x %ux%u%s%ux%u pclk %llu -> %s", fIndex, fValidateCalls, (unsigned)d->detailedTimingModeID,
              d->horizontalActive, d->verticalActive, d->horizontalScaled ? " desktop " : "", d->horizontalScaled, d->verticalScaled,
              (unsigned long long)d->pixelClock, why ? why : "OK");
    if (why) return kIOReturnUnsupportedMode;
    fValidateOk++;
    if (!d->minPixelClock) d->minPixelClock = d->pixelClock;
    if (!d->maxPixelClock) d->maxPixelClock = d->pixelClock;
    if (descripSize == sizeof(IOFBDisplayModeDescription)) {
        IODisplayModeInformation *info = &((IOFBDisplayModeDescription *)description)->info;
        const bool scaled = d->horizontalScaled && d->verticalScaled;
        const UInt64 tot = (UInt64)(d->horizontalActive + d->horizontalBlanking) * (d->verticalActive + d->verticalBlanking);
        info->maxDepthIndex = 0;
        info->nominalWidth = scaled ? d->horizontalScaled : d->horizontalActive;
        info->nominalHeight = scaled ? d->verticalScaled : d->verticalActive;
        info->refreshRate = tot ? (UInt32)((d->pixelClock << 16) / tot) : 0;
    }
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::setDetailedTimings(OSArray *array)
{
    if (!array) {
        fNDT = 0; fBootInDT = false; removeProperty(kIOFBDetailedTimingsKey);
        if (fCurId != kModeId) FBLOG("setDetailedTimings(NULL) while 0x%x is on the head -- it stays until the next setDisplayMode", (unsigned)fCurId);
        return kIOReturnSuccess;
    }
    unsigned n = 0, bad = 0;
    for (unsigned i = 0; i < array->getCount() && n < 64; i++) {  // 64 from macOS; the rest of fDT is for addKmsRefreshModes
        OSData *data = OSDynamicCast(OSData, array->getObject(i));
        if (!data || data->getLength() < sizeof(IODetailedTimingInformationV2)) { bad++; continue; }
        nvu_copy(&fDT[n++], data->getBytesNoCopy(), sizeof(IODetailedTimingInformationV2));
    }
    const unsigned fromMac = n;
    n = addKmsRefreshModes(n);
    fNDT = n;
    setProperty(kIOFBDetailedTimingsKey, array);
    fBootInDT = false;
    if (fCurId == kModeId || dtIndex(fCurId) < 0) {
        const NvU32 bootHz = (fBootMode.timings.refreshRate + 500) / 1000;
        for (unsigned i = 0; i < n; i++) {
            const IODetailedTimingInformationV2 &d = fDT[i];
            if (d.horizontalScaled || d.verticalScaled || d.horizontalActive != fBootW || d.verticalActive != fBootH) continue;
            const UInt64 tot = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
            const NvU32 hz = tot ? (NvU32)((d.pixelClock + tot / 2) / tot) : 0;
            if (hz != bootHz) continue;
            FBLOG("setDetailedTimings: boot raster %ux%u@%u is dt 0x%x -- that is the current mode now (no modeset)",
                  fBootW, fBootH, bootHz, d.detailedTimingModeID);
            fCurId = (IODisplayModeID)d.detailedTimingModeID; fBootInDT = true;
            break;
        }
    } else fBootInDT = true;
    FBLOG("fb%u setDetailedTimings: %u timing(s) installed (%u from macOS, %u refresh rates from NVKMS; %u malformed, %u offered), current 0x%x, EDID matches so far %u",
          fIndex, n, fromMac, n - fromMac, bad, array->getCount(), (unsigned)fCurId, fEdidHits);
    for (unsigned i = 0; i < n && i < 24; i++)
        FBLOG("  dt 0x%x raster %ux%u desktop %ux%u pclk %llu", fDT[i].detailedTimingModeID, fDT[i].horizontalActive, fDT[i].verticalActive,
              fDT[i].horizontalScaled, fDT[i].verticalScaled, (unsigned long long)fDT[i].pixelClock);
    setProperty("NVRMDetailedTimings", (unsigned long long)n, 32);
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::publishTimingCaps()
{
    IODisplayScalerInformation si; nvu_zero(&si, sizeof si);
    si.scalerFeatures = kIOScaleCanUpSamplePixels | kIOScaleCanDownSamplePixels;
    si.maxHorizontalPixels = fScalerMaxW; si.maxVerticalPixels = fScalerMaxH;
    OSData *s = OSData::withBytes(&si, sizeof si);
    if (s) { setProperty(kIOFBScalerInfoKey, s); s->release(); }
    IODisplayTimingRangeV1 r; nvu_zero(&r, sizeof r);
    r.minPixelClock = 25000000ull; r.maxPixelClock = 1200000000ull; r.maxPixelError = 0;
    r.supportedSyncFlags = kIORangeSupportsSeparateSyncs;
    r.supportedSignalLevels = 0; r.supportedSignalConfigs = kIODigitalSignal;
    r.minFrameRate = 23; r.maxFrameRate = 500; r.minLineRate = 10000; r.maxLineRate = 1000000;
    r.maxHorizontalTotal = 16384; r.maxVerticalTotal = 16384;
    r.charSizeHorizontalActive = r.charSizeHorizontalBlanking = r.charSizeHorizontalSyncOffset = r.charSizeHorizontalSyncPulse = 1;
    r.charSizeVerticalActive = r.charSizeVerticalBlanking = r.charSizeVerticalSyncOffset = r.charSizeVerticalSyncPulse = 1;
    r.charSizeHorizontalBorderLeft = r.charSizeHorizontalBorderRight = r.charSizeVerticalBorderTop = r.charSizeVerticalBorderBottom = 1;
    r.charSizeHorizontalTotal = r.charSizeVerticalTotal = 1;
    r.minHorizontalActiveClocks = 320;  r.maxHorizontalActiveClocks = 8192;
    r.minHorizontalBlankingClocks = 0;  r.maxHorizontalBlankingClocks = 8192;
    r.minHorizontalSyncOffsetClocks = 0; r.maxHorizontalSyncOffsetClocks = 8192;
    r.minHorizontalPulseWidthClocks = 1; r.maxHorizontalPulseWidthClocks = 8192;
    r.minVerticalActiveClocks = 200;    r.maxVerticalActiveClocks = 8192;
    r.minVerticalBlankingClocks = 0;    r.maxVerticalBlankingClocks = 8192;
    r.minVerticalSyncOffsetClocks = 0;  r.maxVerticalSyncOffsetClocks = 8192;
    r.minVerticalPulseWidthClocks = 1;  r.maxVerticalPulseWidthClocks = 8192;
    r.maxNumLinks = 1; r.minLink0PixelClock = 25000; r.maxLink0PixelClock = 1200000;
    r.supportedPixelEncoding = kIORangePixelEncodingRGB444; r.supportedBitsPerColorComponent = kIORangeBitsPerColorComponent8;
    r.supportedColorimetryModes = kIORangeColorimetryNativeRGB | kIORangeColorimetrysRGB; r.supportedDynamicRangeModes = kIORangeDynamicRangeSDR;
    OSData *g = OSData::withBytes(&r, sizeof r);
    if (g) { setProperty(kIOFBTimingRangeKey, g); g->release(); }
    FBLOG("fbmodes: published IOFBScalerInfo (up+down, max %ux%u) + IOFBTimingRange (25 MHz..1.2 GHz, 23..500 Hz)", fScalerMaxW, fScalerMaxH);
}
IOReturn NVRMNVDAFramebuffer::getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info)
{
    if (!info) return kIOReturnBadArgument;
    if (mode != kModeId) {
        int i = dtIndex(mode); if (i < 0) return kIOReturnBadArgument;
        const IODetailedTimingInformationV2 &d = fDT[i];
        const bool scaled = d.horizontalScaled && d.verticalScaled;
        const UInt64 tot = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
        nvu_zero(info, sizeof *info);
        info->nominalWidth = scaled ? d.horizontalScaled : d.horizontalActive;
        info->nominalHeight = scaled ? d.verticalScaled : d.verticalActive;
        info->refreshRate = tot ? (UInt32)((d.pixelClock << 16) / tot) : (60u << 16);
        info->maxDepthIndex = 0;
        info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | (scaled ? kDisplayModeNotPresetFlag : 0);
        if (!scaled && d.horizontalActive == fBootW && d.verticalActive == fBootH) info->flags |= kDisplayModeNativeFlag;
        info->imageWidth = fEdidMmW; info->imageHeight = fEdidMmH;
        return kIOReturnSuccess;
    }
    nvu_zero(info, sizeof *info);
    info->nominalWidth = fBootW ? fBootW : fW; info->nominalHeight = fBootH ? fBootH : fH; info->maxDepthIndex = 0;
    const NvU32 rr = fBootW ? fBootMode.timings.refreshRate : fMode.timings.refreshRate;
    info->refreshRate = rr ? (UInt32)(((UInt64)rr << 16) / 1000) : (60u << 16);
    info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
    info->imageWidth = fEdidMmW; info->imageHeight = fEdidMmH;
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info)
{
    if (!info) return kIOReturnUnsupportedMode;
    if (mode != kModeId) {
        int i = dtIndex(mode); if (i < 0) return kIOReturnUnsupportedMode;
        nvu_zero(info, sizeof *info);
        info->appleTimingID = kIOTimingIDInvalid;
        info->flags = kIODetailedTimingValid | ((fDT[i].horizontalScaled && fDT[i].verticalScaled) ? kIOScalingInfoValid : 0);
        info->detailedInfo.v2 = fDT[i];
        return kIOReturnSuccess;
    }
    nvu_zero(info, sizeof *info);
    info->appleTimingID = kIOTimingIDInvalid;
    info->flags = 0;
    {
        const struct NvKmsKapiDisplayModeTimings &t = (fBootW ? fBootMode : fMode).timings;
        if (t.pixelClockHz && t.hTotal > t.hVisible && t.vTotal > t.vVisible && t.hSyncStart >= t.hVisible && t.vSyncStart >= t.vVisible) {
            IODetailedTimingInformationV2 &d = info->detailedInfo.v2;
            d.pixelClock = d.minPixelClock = d.maxPixelClock = t.pixelClockHz;
            d.horizontalActive = t.hVisible; d.horizontalBlanking = t.hTotal - t.hVisible;
            d.horizontalSyncOffset = t.hSyncStart - t.hVisible; d.horizontalSyncPulseWidth = t.hSyncEnd - t.hSyncStart;
            d.verticalActive = t.vVisible; d.verticalBlanking = t.vTotal - t.vVisible;
            d.verticalSyncOffset = t.vSyncStart - t.vVisible; d.verticalSyncPulseWidth = t.vSyncEnd - t.vSyncStart;
            d.horizontalSyncConfig = t.flags.hSyncPos ? 1 : 0; d.verticalSyncConfig = t.flags.vSyncPos ? 1 : 0;
            d.detailedTimingModeID = (UInt32)kModeId;
            info->flags = kIODetailedTimingValid;
            FBLOG("getTimingInfoForDisplayMode(%d) -> detailed %ux%u pclk %u Hz total %ux%u (%u mHz)", (int)mode,
                  t.hVisible, t.vVisible, t.pixelClockHz, t.hTotal, t.vTotal, t.refreshRate);
            return kIOReturnSuccess;
        }
    }
    FBLOG("getTimingInfoForDisplayMode(%d) -> success", (int)mode);
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi)
{
    if (depth != 0 || aperture != kIOFBSystemAperture || !pi) return kIOReturnUnsupportedMode;
    NvU32 pw = fBootW ? fBootW : fW, ph = fBootH ? fBootH : fH, pp = fBootPitch ? fBootPitch : fPitch;
    if (mode != kModeId) {
        int i = dtIndex(mode); if (i < 0) return kIOReturnUnsupportedMode;
        pw = fDT[i].horizontalScaled ? fDT[i].horizontalScaled : fDT[i].horizontalActive;
        ph = fDT[i].verticalScaled ? fDT[i].verticalScaled : fDT[i].verticalActive;
        pp = (pw * 4 + fPitchAlign - 1) & ~(fPitchAlign - 1);
    }
    nvu_zero(pi, sizeof *pi);
    pi->activeWidth = pw; pi->activeHeight = ph; pi->bytesPerRow = pp; pi->bytesPerPlane = 0;
    pi->bitsPerPixel = 32; pi->pixelType = kIORGBDirectPixels; pi->componentCount = 3; pi->bitsPerComponent = 8;
    pi->componentMasks[0] = 0x00ff0000; pi->componentMasks[1] = 0x0000ff00; pi->componentMasks[2] = 0x000000ff;
    strlcpy(pi->pixelFormat, IO32BitDirectPixels, sizeof pi->pixelFormat);
    pi->flags = 0;
    FBLOG("getPixelInformation 0x%x ap %d -> %ux%u bytesPerRow %u", (unsigned)mode, (int)aperture, pw, ph, pp);
    return kIOReturnSuccess;
}
IOService *NVRMNVDAFramebuffer::pciNub()
{
    if (fPciNub) return fPciNub;
    for (IOService *s = getProvider(); s; s = s->getProvider())
        if (s->metaCast("IOPCIDevice")) { fPciNub = s; break; }
    return fPciNub;
}
IODeviceMemory *NVRMNVDAFramebuffer::apertureFor(NvU64 target, IOByteCount bytes)
{
    if (!target) return NULL;
    IOService *nub = pciNub();
    if (nub) {
        IOItemCount n = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < n; i++) {
            IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
            IOByteCount len = m->getLength();
            if (target < (NvU64)pa || bytes > len || target - (NvU64)pa > len - bytes) continue;
            IOMemoryDescriptor *s = IOSubMemoryDescriptor::withSubRange(
                m, (IOByteCount)(target - (NvU64)pa), bytes, kIODirectionNone);
            if (s) return (IODeviceMemory *)s;
        }
    }
    return (IODeviceMemory *)IOMemoryDescriptor::withAddressRange(
        target, bytes, kIODirectionNone | kIOMemoryMapperNone, NULL);
}

bool NVRMNVDAFramebuffer::verifyAperture(NvU64 phys, const char *label)
{
    const UInt32 SIG1 = 0xC0DE1111, SIG2 = 0xC0DE2222;
    if (!phys || !fKva) return false;
    volatile UInt32 *k = (volatile UInt32 *)fKva;
    k[0x1000 / 4] = SIG1;
    k[0x2000 / 4] = SIG2;
    __asm__ volatile("mfence" ::: "memory");
    if (k[0x1000 / 4] != SIG1 || k[0x2000 / 4] != SIG2) {
        FBLOG("verifyAperture(%s): CONTROL FAILED -- the write never landed through fKva; "
              "the readback says nothing about the address", label);
        return false;
    }
    IODeviceMemory *dm = apertureFor(phys, 0x4000);
    if (!dm) { FBLOG("verifyAperture(%s) 0x%llx: no descriptor", label, phys); return false; }
    IOMemoryMap *mm = dm->map(kIOMapWriteCombineCache);
    bool ok = false;
    if (!mm) {
        FBLOG("verifyAperture(%s) 0x%llx: map failed", label, phys);
    } else {
        volatile UInt32 *v = (volatile UInt32 *)mm->getVirtualAddress();
        UInt32 a = v[0x1000 / 4], b = v[0x2000 / 4];
        bool blind = (a == b);
        ok = (a == SIG1 && b == SIG2) && !blind;
        FBLOG("verifyAperture(%s) 0x%llx: +0x1000=%08x +0x2000=%08x  %s%s",
              label, phys, a, b, ok ? "*** MATCH ***" : "no match",
              blind ? "   [BLIND: both offsets identical]" : "");
        mm->release();
    }
    dm->release();
    return ok;
}

static volatile SInt64 gVramMappedBytes = 0;

// Parked = allocations NVKMS placed over the scanout or the boot console. WAS 24 MB / 24 slots: a 2560x1440 console is
// 25 MB, so the last MB could never be parked and every allocation landing there was refused, about once a second
// (issue #28). 64 MB / 64 slots covers a 5K console (59 MB).
#define NVRM_VRAM_PARK_CEILING (64ull * 1024ull * 1024ull)
// A 256 MB BAR1 cannot spare 64 MB of parked mappings (surfaces were refused and the screen stayed black with a cursor).
// 32 MB still covers a 2560x1440 console (25 MB, issue #28). Larger BAR1s keep 64 MB, enough for a 5K console.
static inline SInt64 nvrmParkCeiling(NvU64 barLen)
{
    if (barLen && barLen < (256ull << 20)) return (SInt64)(barLen / 4);   // 64 MB BAR1: 16 MB, not half the window
    return (SInt64)(barLen && barLen < (1ull << 30) ? 32ull * 1024ull * 1024ull : NVRM_VRAM_PARK_CEILING);
}
static struct { struct NvKmsKapiMemory *m; void *k; NvU64 n; } gParkedForever[64];
static int      gNParkedForever   = 0;
static volatile SInt64 gVramParkedBytes = 0;
static volatile SInt64 gCntGrant, gCntGrantBytes, gCntRelease, gCntReleaseBytes;
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_grants,        CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gCntGrant,        "vramGrant successes");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_grant_bytes,   CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gCntGrantBytes,   "bytes granted");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_releases,      CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gCntRelease,      "vramRelease calls");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_release_bytes, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gCntReleaseBytes, "bytes released");
static volatile SInt64 gVramBudgetBytes = 0;
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_budget_bytes, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gVramBudgetBytes, "the BAR1 grant budget in force");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_mapped_bytes,  CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gVramMappedBytes,  "the BAR1 budget counter, live");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_vram_display_bytes, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gVramDisplayBytes, "scanout+cursor the framebuffer itself maps into BAR1");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_vram_display_aware, CTLFLAG_RW | CTLFLAG_LOCKED, &gVramDisplayAware, 0, "1 = the accel budget reserves the BAR1 the display holds; 0 = the old fixed fraction");
static struct NVRMVramRequest gVramTest[256]; static int gVramTestN; static int gVramTestMode;
static int nvrmfb_vramtest_sysctl SYSCTL_HANDLER_ARGS
{
    int v = gVramTestN; int err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || req->newptr == 0) return err;
    if (!gFB) return ENXIO;
    if (v > 0) {
        while (gVramTestN < v && gVramTestN < 256) {
            struct NVRMVramRequest *r = &gVramTest[gVramTestN]; nvu_zero(r, sizeof *r);
            r->version = NVRM_VRAM_ABI_VERSION; r->size = 8323072ull;
            if (!gFB->vramGrant(r)) break;
            gVramTestN++;
        }
        kprintf("NVRM-fb: vramtest: holding %d x 8 MB\n", gVramTestN);
    } else {
        int n = gVramTestN;
        while (gVramTestN > 0) { struct NVRMVramRequest *r = &gVramTest[--gVramTestN]; if (gVramTestMode == 1) r->kva = nullptr; gFB->vramRelease(r); }
        kprintf("NVRM-fb: vramtest: released %d (mode %d)\n", n, gVramTestMode);
    }
    return 0;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_vramtest, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, NULL, 0, nvrmfb_vramtest_sysctl, "I",
            "grant N x 8 MB of VRAM through vramGrant (0 = release all)");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_vramtest_mode, CTLFLAG_RW | CTLFLAG_LOCKED, &gVramTestMode, 0, "1 = release without the kernel unmap");
static int gFlipInterval = 1;
// gFlipLatch and the counters are defined above submitConfig, where the completion wait drives them.
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch, CTLFLAG_RW | CTLFLAG_LOCKED, &gFlipLatch, 0, "1 = a committed flip waits for its completion before a surface retires (0 = debug bypass)");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_waits, CTLFLAG_RD | CTLFLAG_LOCKED, &gFlipLatchWaits, 0, "flips that waited for completion");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_guard_cleared, CTLFLAG_RD | CTLFLAG_LOCKED, &gGuardCleared, 0, "1 once the boot guard was cleared this boot");
// Registered here and not with the other counters: those are registered from the first video-memory grant, which a
// card with no display to light never makes, and the headless clear is exactly the case NVRM has to be able to read.
static void fbGuardCleared(const char *why)
{
    static volatile SInt32 once;
    if (OSCompareAndSwap(0, 1, &once)) sysctl_register_oid(&sysctl__debug_nvrmfb_guard_cleared);
    nvrm_guard_display_up(why);
    gGuardCleared = 1;
}
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_timeouts, CTLFLAG_RD | CTLFLAG_LOCKED, &gFlipLatchTimeouts, 0, "flips not on the panel within a refresh period and a quarter of their submit (waited on to the stuck limit)");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_max_us, CTLFLAG_RD | CTLFLAG_LOCKED, &gFlipLatchMaxUs, 0, "longest submit-to-completion wait (us)");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_late, CTLFLAG_RD | CTLFLAG_LOCKED, &gFlipLatchLate, 0, "presents skipped because the previous flip was still on its way to the panel");
static int gFlipLean = 1;
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_lean, CTLFLAG_RW | CTLFLAG_LOCKED, &gFlipLean, 0, "1 = lean pure flip (surface only)");
static SInt64 gFlipUsSum, gFlipN, gFlipUsMax;
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_interval, CTLFLAG_RW | CTLFLAG_LOCKED, &gFlipInterval, 0, "minPresentInterval of the pure (zero-copy) flip; tearing stays off");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_flip_us_sum, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gFlipUsSum, "microseconds spent in pure-flip applyModeSetConfig");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_flip_n, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gFlipN, "pure flips timed");
SYSCTL_QUAD(_debug, OID_AUTO, nvrmfb_flip_us_max, CTLFLAG_RD | CTLFLAG_LOCKED, (SInt64 *)&gFlipUsMax, "the slowest pure flip, us");
static bool gFbSysctl = false;
static void nvrmfbInstallSysctl(void)
{
    if (gFbSysctl) return; gFbSysctl = true;
    sysctl_register_oid(&sysctl__debug_nvrmfb_vram_grants);   sysctl_register_oid(&sysctl__debug_nvrmfb_vram_grant_bytes);
    sysctl_register_oid(&sysctl__debug_nvrmfb_vram_releases); sysctl_register_oid(&sysctl__debug_nvrmfb_vram_release_bytes);
    sysctl_register_oid(&sysctl__debug_nvrmfb_vram_mapped_bytes); sysctl_register_oid(&sysctl__debug_nvrmfb_vram_budget_bytes);
    sysctl_register_oid(&sysctl__debug_nvrmfb_vram_display_bytes); sysctl_register_oid(&sysctl__debug_nvrmfb_vram_display_aware);
    sysctl_register_oid(&sysctl__debug_nvrmfb_vramtest); sysctl_register_oid(&sysctl__debug_nvrmfb_vramtest_mode);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_interval); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_us_sum);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_n); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_us_max);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_lean);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_waits);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_timeouts); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_max_us);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_late);
}

bool NVRMNVDAFramebuffer::vramGrant(struct NVRMVramRequest *r)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    nvrmfbInstallSysctl();
    r->phys = 0; r->kva = nullptr; r->handle = nullptr; r->actualSize = 0; r->mappedTotal = 0;
    if (!fKms || !fDev) { FBLOG("nvAllocVram: no NVKMS device yet"); return false; }
    NvU64 want = (r->size + 0xFFFFull) & ~0xFFFFull;
    if (!want) return false;

    const SInt64 budget = (SInt64)nvrmSurfaceBudget(fBarLen, gVramDisplayBytes);
    gVramBudgetBytes = budget;
    // Parked blocks keep their BAR1 mapping (see below), so they count against the budget. On a 256 MB BAR1 (every
    // GTX 16, boards without Resizable BAR) the park ceiling is the smaller one, so surfaces keep most of the budget.
    SInt64 before = OSAddAtomic64((SInt64)want, &gVramMappedBytes) + gVramParkedBytes;
    if (before + (SInt64)want > budget) {
        OSAddAtomic64(-(SInt64)want, &gVramMappedBytes);
        // A full 256 MB window is refused thousands of times in a session (one size, retried every frame: 1,413 lines in
        // one user log), and each line costs a kernel log write on the path that is already out of room. Log the first
        // eight, then every 256th with the running count, and say once who holds the window: grants against releases
        // tell a working set from a leak.
        static volatile SInt32 refused = 0;
        const SInt32 nRefused = OSIncrementAtomic(&refused) + 1;
        if (nRefused <= 8 || (nRefused & 255) == 0)
            FBLOG("nvAllocVram(%llu): REFUSED (#%d) — BAR1 budget spent (%lld of %llu bytes). "
                  "This is the resizable-BAR ceiling, not a VRAM shortage.",
                  (unsigned long long)want, (int)nRefused, (long long)before, (unsigned long long)budget);
        if (nRefused == 1)
            FBLOG("BAR1 full: %lld grants (%lld bytes) vs %lld releases (%lld bytes); live %lld, console parks %lld bytes "
                  "in %d slots, display %lld, budget %lld, BAR1 %llu",
                  (long long)gCntGrant, (long long)gCntGrantBytes, (long long)gCntRelease, (long long)gCntReleaseBytes,
                  (long long)gVramMappedBytes, (long long)gVramParkedBytes, gNParkedForever, (long long)gVramDisplayBytes,
                  (long long)budget, (unsigned long long)fBarLen);
        return false;
    }

    struct NvKmsKapiMemory *mem = nullptr; void *kva = nullptr; NvU64 phys = 0;

    for (int attempt = 0; attempt < 8 && !mem; attempt++) {
        struct NvKmsKapiAllocateMemoryParams mp; nvu_zero(&mp, sizeof mp);
        NvU8 compressible = 0;
        mp.layout = NvKmsSurfaceMemoryLayoutPitch;
        mp.type   = NVKMS_KAPI_ALLOCATION_TYPE_SCANOUT;
        mp.size   = want;
        mp.useVideoMemory = NV_TRUE;
        mp.compressible   = &compressible;
        struct NvKmsKapiMemory *m = fKms->allocateMemory(fDev, &mp);
        // Both failures print what the budget counted at that moment: the real BAR1 limit of a 256 MB window is
        // somewhere under the budget, and only these lines can show where.
        if (!m) { FBLOG("nvAllocVram(%llu): allocateMemory failed (mapped %lld + parked %lld of %lld budget, BAR1 %llu)",
                        (unsigned long long)want, (long long)gVramMappedBytes, (long long)gVramParkedBytes, (long long)budget,
                        (unsigned long long)fBarLen); break; }
        void *k = nullptr;
        if (!fKms->mapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &k) || !k) {
            FBLOG("nvAllocVram(%llu): mapMemory(KERNEL) failed (mapped %lld + parked %lld of %lld budget, BAR1 %llu)",
                  (unsigned long long)want, (long long)gVramMappedBytes, (long long)gVramParkedBytes, (long long)budget,
                  (unsigned long long)fBarLen);
            fKms->freeMemory(fDev, m); break;
        }
        NvU64 p = fPhysForVa ? fPhysForVa(k) : 0;
        bool onScanout = p && fPhys  && fSize     && p < (fPhys + fSize)         && (p + want) > fPhys;
        bool onConsole = p && fConsPhys && fConsSize && p < (fConsPhys + fConsSize) && (p + want) > fConsPhys;
        if (!p) {
            // No physical address means RM's mapping table could not record this mapping. Parking it would hold one
            // more mapping per attempt and the next attempt fails the same way: release it and refuse this request.
            FBLOG("nvAllocVram(%llu): attempt %d has no physical address — released, request refused (RM mapping table full?)",
                  (unsigned long long)want, attempt);
            fKms->unmapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, k); fKms->freeMemory(fDev, m);
            break;
        }
        if (onScanout || onConsole) {
            FBLOG("nvAllocVram: attempt %d phys 0x%llx %s — parking it and rolling again",
                  attempt, (unsigned long long)p,
                  !p ? "has no physical address" : onScanout ? "OVERLAPS THE SCANOUT" : "OVERLAPS THE CONSOLE");
            if (gNParkedForever < (int)(sizeof gParkedForever / sizeof gParkedForever[0]) &&
                gVramParkedBytes + (SInt64)want <= nvrmParkCeiling(fBarLen)) {
                // kept allocated AND mapped: the overlap is judged by the BAR1 address of this mapping, so unmapping
                // it hands that address straight back and the next allocation lands on the console again (one machine
                // logged the same address 600 times, every attempt refused: black screen with a cursor)
                gParkedForever[gNParkedForever].m = m;
                gParkedForever[gNParkedForever].k = k;
                gParkedForever[gNParkedForever].n = want;
                gNParkedForever++;
                OSAddAtomic64((SInt64)want, &gVramParkedBytes);
            } else {
                fKms->unmapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, k); fKms->freeMemory(fDev, m);
                // A surface larger than the park ceiling (a 3840x2560 desktop is 37.5 MB, the ceiling on a 256 MB BAR1
                // is 32 MB) was never parked, so the same address came back on every attempt. Hold only the part of
                // the range that overlaps instead: a guard block from this address to the end of the console or
                // scanout, which the allocator hands out at the address just freed.
                const NvU64 rEnd = onScanout ? fPhys + fSize : fConsPhys + fConsSize;
                const NvU64 g = (p && rEnd > p) ? ((rEnd - p) + 0x1FFFFFull) & ~0x1FFFFFull : 0;
                bool guarded = false;
                if (g && g < want && gNParkedForever < (int)(sizeof gParkedForever / sizeof gParkedForever[0]) &&
                    gVramParkedBytes + (SInt64)g <= nvrmParkCeiling(fBarLen)) {
                    struct NvKmsKapiAllocateMemoryParams gp; nvu_zero(&gp, sizeof gp);
                    NvU8 gc = 0;
                    gp.layout = NvKmsSurfaceMemoryLayoutPitch; gp.type = NVKMS_KAPI_ALLOCATION_TYPE_SCANOUT;
                    gp.size = g; gp.useVideoMemory = NV_TRUE; gp.compressible = &gc;
                    struct NvKmsKapiMemory *gm = fKms->allocateMemory(fDev, &gp);
                    void *gk = nullptr;
                    if (gm && fKms->mapMemory(fDev, gm, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &gk) && gk) {
                        const NvU64 gpa = fPhysForVa ? fPhysForVa(gk) : 0;
                        const NvU64 rStart = onScanout ? fPhys : fConsPhys;
                        if (gpa && gpa < rEnd && gpa + g > rStart) {
                            gParkedForever[gNParkedForever].m = gm;
                            gParkedForever[gNParkedForever].k = gk;
                            gParkedForever[gNParkedForever].n = g;
                            gNParkedForever++;
                            OSAddAtomic64((SInt64)g, &gVramParkedBytes);
                            guarded = true;
                        } else {
                            fKms->unmapMemory(fDev, gm, NVKMS_KAPI_MAPPING_TYPE_KERNEL, gk); fKms->freeMemory(fDev, gm);
                        }
                    } else if (gm) {
                        fKms->freeMemory(fDev, gm);
                    }
                }
                FBLOG("nvAllocVram: %llu bytes cannot be parked (%lld bytes parked over %d slots) — %s",
                      (unsigned long long)want, (long long)gVramParkedBytes, gNParkedForever,
                      guarded ? "holding a guard block over the overlap instead" : "released, no guard block placed");
            }
            continue;
        }
        mem = m; kva = k; phys = p;
    }
    if (!mem) {
        OSAddAtomic64(-(SInt64)want, &gVramMappedBytes);
        // Who holds the window, once per boot: a 256 MB BAR1 that fills up cannot be told apart from a leak by the
        // failure lines alone; grants against releases can.
        static volatile SInt32 said = 0;
        if (OSCompareAndSwap(0, 1, &said))
            FBLOG("BAR1 full: %lld grants (%lld bytes) vs %lld releases (%lld bytes); live %lld, console parks %lld bytes "
                  "in %d slots, display %lld, budget %lld, BAR1 %llu",
                  (long long)gCntGrant, (long long)gCntGrantBytes, (long long)gCntRelease, (long long)gCntReleaseBytes,
                  (long long)gVramMappedBytes, (long long)gVramParkedBytes, gNParkedForever, (long long)gVramDisplayBytes,
                  (long long)budget, (unsigned long long)fBarLen);
        return false;
    }

    nvu_zero_wide(kva, (size_t)want);
    volatile NvU64 *q = (volatile NvU64 *)kva;
    q[0] = 0x5A5AA5A5C0FFEE01ull;
    NvU64 back = q[0];
    q[0] = 0;
    if (back != 0x5A5AA5A5C0FFEE01ull) {
        FBLOG("nvAllocVram(%llu): kva %p phys 0x%llx WROTE 0x5A5AA5A5C0FFEE01 AND READ 0x%llx — "
              "the aperture is not live; refusing to hand this out",
              (unsigned long long)want, kva, (unsigned long long)phys, (unsigned long long)back);
        fKms->unmapMemory(fDev, mem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, kva);
        fKms->freeMemory(fDev, mem);
        OSAddAtomic64(-(SInt64)want, &gVramMappedBytes);
        return false;
    }

    const UInt64 allocationCookie = gAllocations.insert(mem, want);
    if (!allocationCookie) {
        fKms->unmapMemory(fDev, mem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, kva);
        fKms->freeMemory(fDev, mem); OSAddAtomic64(-(SInt64)want, &gVramMappedBytes); return false;
    }
    r->allocationCookie = allocationCookie;
    advanceFlipEpoch();   // before the caller can see this memory, so it can never match an entry made for a freed one
    r->handle = mem; r->kva = kva; r->phys = (unsigned long long)phys;
    r->actualSize = (unsigned long long)want;
    r->mappedTotal = (unsigned long long)(before + (SInt64)want);
    fVramGrants++;
    OSAddAtomic64(1, &gCntGrant); OSAddAtomic64((SInt64)want, &gCntGrantBytes);
    if (fVramGrantLogged < 16) { fVramGrantLogged++;
        FBLOG("nvAllocVram #%u: %llu bytes REAL VRAM — kva %p phys 0x%llx  [BAR1 %llu / %llu]",
              fVramGrants, (unsigned long long)want, kva, (unsigned long long)phys,
              r->mappedTotal, (unsigned long long)budget); }
    return true;
}

bool NVRMNVDAFramebuffer::vramRelease(struct NVRMVramRequest *r)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    struct NvKmsKapiMemory *mem = (struct NvKmsKapiMemory *)r->handle;
    if (!mem || !fKms || !fDev) return false;
    if (!gAllocations.remove(mem, r->allocationCookie)) return false;
    advanceFlipEpoch();   // every cached surface made before this free stops matching (see gFlipEpoch)
    if (r->kva) fKms->unmapMemory(fDev, mem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, r->kva);
    fKms->freeMemory(fDev, mem);
    if (r->actualSize) OSAddAtomic64(-(SInt64)r->actualSize, &gVramMappedBytes);
    OSAddAtomic64(1, &gCntRelease); OSAddAtomic64((SInt64)r->actualSize, &gCntReleaseBytes);
    r->handle = nullptr; r->kva = nullptr; r->phys = 0; r->actualSize = 0; r->allocationCookie = 0;
    return true;
}

IOReturn NVRMNVDAFramebuffer::callPlatformFunction(const OSSymbol *functionName, bool waitForFunction,
                                               void *param1, void *param2, void *param3, void *param4)
{
    bool isAlloc = functionName && functionName->isEqualTo(NVRM_VRAM_FN_ALLOC);
    bool isFree  = functionName && functionName->isEqualTo(NVRM_VRAM_FN_FREE);
    if (functionName && functionName->isEqualTo("NVRMGpuPower")) {   // from NVRM: 0 standby, 1 resumed, 2 lost
        const uintptr_t st = (uintptr_t)param1;
        if (st == 0) {
            if (fModeSet && fSurf) fHeadSlept = true;   // nvKmsSuspend shuts every head down
            fSysSlept = true;                            // the next power-on is a system wake (familyDidWake)
            FBLOG("GPU going to standby: head %u off with it", fHead);
        } else if (st == 1) {
            if (fGpuWake) scheduleCall(fGpuWake, 0);
        } else {
            FBLOG("GPU lost on wake: head %u stays dark (restart needed)", fHead);
        }
        return kIOReturnSuccess;
    }
    if (functionName && functionName->isEqualTo(NVRM_VRAM_FN_SCANOUT)) {
        struct NVRMVramRequest *r = (struct NVRMVramRequest *)param1;
        if (!r || r->version != NVRM_VRAM_ABI_VERSION) return kIOReturnBadArgument;
        if (!fPhys || !fKva || !fPitch || !fH) { FBLOG("nvScanoutInfo: no scanout yet"); return kIOReturnNotReady; }
        r->phys       = fPhys;
        r->kva        = fKva;
        r->actualSize = (unsigned long long)fPitch * fH;
        r->width = fW; r->height = fH; r->pitch = fPitch;
        r->handle = fMem;
        // Asked once per frame: logging every answer was 92% of a 512 KiB Send-logs kernel log (1401 issue #16), about
        // 67 s of history. Log only when the answer changes.
        if (fPhys != fScanLogPhys || fW != fScanLogW || fH != fScanLogH || fPitch != fScanLogPitch) {
            fScanLogPhys = fPhys; fScanLogW = fW; fScanLogH = fH; fScanLogPitch = fPitch;
            FBLOG("nvScanoutInfo -> phys 0x%llx %ux%u pitch %u (%llu bytes)",
                  (unsigned long long)fPhys, fW, fH, fPitch, (unsigned long long)r->actualSize);
        }
        return kIOReturnSuccess;
    }
    if (functionName && functionName->isEqualTo(NVRM_GPUVA_FN_HANDLES)) {
        struct NVRMRmHandles *h = (struct NVRMRmHandles *)param1;
        if (!h || h->version != NVRM_GPUVA_ABI_VERSION) return kIOReturnBadArgument;
        if (!fDev) { FBLOG("nvRmHandles: no kapi device yet"); return kIOReturnNotReady; }
        h->kapiDevice = fDev;
        h->hClient = 0; h->hDevice = 0; h->hSubDevice = 0;
        FBLOG("nvRmHandles -> kapiDevice %p", fDev);
        return kIOReturnSuccess;
    }
    if (functionName && functionName->isEqualTo("nvFlipToSurfacePure")) {
        struct NVRMFlipRequest *f = (struct NVRMFlipRequest *)param1;
        if (!f || f->version != NVRM_FLIP_ABI_VERSION) return kIOReturnBadArgument;
        int rc = -1;
        const bool home = (f->flags & 1u) != 0;
        const bool pureHome = home && fModeSet;
        if (pureHome) { fHomePure++; if (fHomePure <= 4) FBLOG("home flip #%u: pure (head %u already lit, no modeset)", fHomePure, fHead); }
        bool ok = flipToMemory(home ? nullptr : (struct NvKmsKapiMemory *)f->kapiMemory,
                               f->width, f->height, f->pitch, &rc, !home || pureHome, f->allocationCookie);
        f->flipResult = rc;
        // Busy: the previous flip is still on its way to the panel; the caller keeps its frame and presents the next.
        return ok ? kIOReturnSuccess : rc == (int)NV_KMS_FLIP_RESULT_IN_PROGRESS ? kIOReturnBusy : kIOReturnIOError;
    }
    if (functionName && functionName->isEqualTo(NVRM_FLIP_FN)) {
        struct NVRMFlipRequest *f = (struct NVRMFlipRequest *)param1;
        if (!f || f->version != NVRM_FLIP_ABI_VERSION) return kIOReturnBadArgument;
        int rc = -1;
        bool ok = flipToMemory((struct NvKmsKapiMemory *)f->kapiMemory,
                               f->width, f->height, f->pitch, &rc, false, f->allocationCookie);
        f->flipResult = rc;
        return ok ? kIOReturnSuccess : rc == (int)NV_KMS_FLIP_RESULT_IN_PROGRESS ? kIOReturnBusy : kIOReturnIOError;
    }
    const char *name = functionName ? functionName->getCStringNoCopy() : "(null)";
    if (!isAlloc && !isFree)
        return super::callPlatformFunction(functionName, waitForFunction, param1, param2, param3, param4);

    struct NVRMVramRequest *r = (struct NVRMVramRequest *)param1;
    if (!r) return kIOReturnBadArgument;
    if (r->version != NVRM_VRAM_ABI_VERSION) {
        FBLOG("%s: ABI mismatch — caller says %u, we speak %u. REBUILD BOTH KEXTS.",
              name, r->version, NVRM_VRAM_ABI_VERSION);
        return kIOReturnUnsupported;
    }
    if (isAlloc) return vramGrant(r)   ? kIOReturnSuccess : kIOReturnNoMemory;
    return              vramRelease(r) ? kIOReturnSuccess : kIOReturnBadArgument;
}

bool NVRMNVDAFramebuffer::flipToMemory(struct NvKmsKapiMemory *mem, unsigned w, unsigned h,
                                   unsigned pitch, int *rcOut, bool pure, UInt64 cookie)
{
    NVRMDisplayGate gate; if (!gate.valid() || fSubmissionFaulted) return false;
    if (mem) {
        const UInt64 bytes = gAllocations.size(mem, cookie);
        if (!bytes || !w || !h || w > 65535 || h > 65535 || (UInt64)w * 4 > pitch || (UInt64)pitch * h > bytes) return false;
    }
    if (rcOut) *rcOut = -1;
    if (!fKms || !fDev || (!mem && !fSurf)) return false;
    if (!mem) { w = fW; h = fH; pitch = fPitch; }
    if (w != fW || h != fH || !pitch) {
        if (fFlipRejects < 8) { fFlipRejects++;
            FBLOG("flipToMemory REFUSED %ux%u pitch %u -- the mode is %ux%u pitch %u",
                  w, h, pitch, fW, fH, fPitch); }
        return false;
    }
    struct NvKmsKapiSurface *surf = mem ? nullptr : fSurf;
    // Sampled before the lookup and stored with a new entry: an entry made across a grant/release keeps the older epoch
    // and so can never look current. Flips on one framebuffer come from its own display pipe; no other one reads this cache.
    const UInt64 epoch = readFlipEpoch();
    const bool cacheable = mem && epoch != (UInt64)kFlipEpochLimit;
    if (cacheable) {
        IOInterruptState state = IOSimpleLockLockDisableInterrupt(fFlipCacheLock);
        // The epoch is part of the match. The key is the NvKmsKapiMemory pointer, which a buffer allocated after a free
        // can reuse at the same address; matching an entry recorded in an older epoch would scan out its stale surface
        // over the reallocated memory, which strobes the panel black every other frame after a refresh or resolution
        // change. Requiring the current epoch makes a reused address always take a fresh surface.
        for (unsigned i = 0; i < fFlipCacheN; i++)
            if (fFlipCache[i].epoch == epoch && fFlipCache[i].cookie == cookie && fFlipCache[i].mem == mem &&
                fFlipCache[i].pitch == pitch && fFlipCache[i].w == w && fFlipCache[i].h == h) { surf = fFlipCache[i].surf; break; }
        IOSimpleLockUnlockEnableInterrupt(fFlipCacheLock, state);
    }
    if (!surf) {
        // Prior accepted submissions complete before returning; no non-front cache entry is in flight.
        unsigned slot = 16;
        for (unsigned i = 0; i < 16; ++i) if (!fFlipCache[i].surf) { slot = i; break; }
        if (slot == 16) for (unsigned i = 0; i < 16; ++i)
            if (fFlipCache[i].surf != fFrontSurf) { slot = i; break; }
        if (slot == 16 || !cacheable) return false;
        NVFlipEntry &entry = fFlipCache[slot];
        if (entry.surf) {
            // Until the latest flip has landed, the panel may still be scanning the front before it, which is one of
            // these non-front slots. A late flip costs this present (the caller keeps the frame); a stuck one faults.
            const int prior = priorFlipState();
            if (prior != kPriorDone) {
                if (prior == kPriorStuck) {
                    flipFaultLog("prior completion unavailable (surface reuse)");
                    fSubmissionFaulted = true;
                }
                if (prior == kPriorLate) OSIncrementAtomic((volatile SInt32 *)&gFlipLatchLate);
                if (rcOut) *rcOut = prior == kPriorLate ? (int)NV_KMS_FLIP_RESULT_IN_PROGRESS : -1;
                return false;
            }
            fKms->destroySurface(fDev, entry.surf);
            if (entry.pin) fKms->freeMemory(fDev, entry.pin);
            entry = {};
        }
        struct NvKmsKapiMemory *pin = fKms->dupMemory(fDev, fDev, mem);
        if (!pin) return false;
        struct NvKmsKapiCreateSurfaceParams sp = {};
        sp.planes[0].memory = pin; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
        sp.width = w; sp.height = h; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
        surf = fKms->createSurface(fDev, &sp);
        if (!surf) { fKms->freeMemory(fDev, pin); return false; }
        entry.mem = mem; entry.surf = surf; entry.pin = pin; entry.cookie = cookie; entry.epoch = epoch;
        entry.w = w; entry.h = h; entry.pitch = pitch;
        if (slot >= fFlipCacheN) fFlipCacheN = slot + 1;
    }
    struct NvKmsKapiRequestedModeSetConfig *cfg =
        (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep =
        (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr);
    hr->modeSetConfig.bActive = NV_TRUE;
    hr->modeSetConfig.numDisplays = 1;
    hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    if (!pure) hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    const bool lean = pure && gFlipLean;
    if (lean) {
        hr->flags.olutFpNormScaleChanged = NV_FALSE;
        for (unsigned i = 0; i < NVKMS_KAPI_LAYER_MAX; i++) hr->layerRequestedConfig[i].flags.cscChanged = NV_FALSE;
    }
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = surf;
    lr->config.srcWidth = w; lr->config.srcHeight = h;
    lr->config.dstWidth = fRasterW; lr->config.dstHeight = fRasterH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
    lr->config.minPresentInterval = pure ? (NvU8)(gFlipInterval < 0 ? 0 : gFlipInterval > 4 ? 4 : gFlipInterval) : 0;
    lr->config.tearing = NV_FALSE;
    lr->flags.surfaceChanged = NV_TRUE;
    if (!lean)
        lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    const uint64_t ft0 = mach_absolute_time();
    NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, false, true);
    if (pure) { const SInt64 us = (SInt64)((mach_absolute_time() - ft0) / 1000);
        OSAddAtomic64(us, &gFlipUsSum); OSAddAtomic64(1, &gFlipN); if (us > gFlipUsMax) gFlipUsMax = us; }
    // IN_PROGRESS on a refusal is the previous flip still on its way (submitConfig's late path, or NVKMS's own answer
    // to a flip over an outstanding one): the caller keeps its frame and presents the next.
    int rc = ok ? (int)rep->flipResult : rep->flipResult == NV_KMS_FLIP_RESULT_IN_PROGRESS ? (int)NV_KMS_FLIP_RESULT_IN_PROGRESS : -1;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (rcOut) *rcOut = rc;
    const bool accepted = ok && rc == NV_KMS_FLIP_RESULT_SUCCESS;
    if (accepted) fFrontSurf = surf;
    fFlipCount++;
    if (fFlipCount <= 8 || (fFlipCount % 600) == 0)
        FBLOG("flipToMemory #%u -> ok %u rc %d (surface %p memory %p)",
              fFlipCount, (unsigned)ok, rc, surf, mem);
    return accepted;
}

IODeviceMemory *NVRMNVDAFramebuffer::getVRAMRange()
{
    NvU64 target = fPhys ? fPhys : fConsPhys;
    IOService *nub = pciNub();
    if (nub && target) {
        IOItemCount n = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < n; i++) {
            IODeviceMemory *m = (IODeviceMemory *)nub->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
            IOByteCount len = m->getLength();
            if (target < (NvU64)pa || target >= ((NvU64)pa + len)) continue;
            m->retain();
            if (fVramLogged < 3) { fVramLogged++;
                FBLOG("getVRAMRange -> WHOLE BAR%u pa 0x%llx len %llu (was the %llu-byte scanout subrange)",
                      (unsigned)i, (unsigned long long)pa, (unsigned long long)len,
                      (unsigned long long)((NvU64)fPitch * fH + 128)); }
            return m;
        }
    }
    if (fVramLogged < 3) { fVramLogged++;
        FBLOG("getVRAMRange: no BAR contains phys 0x%llx — falling back to the scanout aperture",
              (unsigned long long)target); }
    return getApertureRange(kIOFBSystemAperture);
}

IODeviceMemory *NVRMNVDAFramebuffer::getApertureRange(IOPixelAperture aperture)
{
    if (aperture != kIOFBSystemAperture) return NULL;
    NvU64 target = (fConsoleAperture && fConsPhys) ? fConsPhys : fPhys;
    if (!target) return NULL;
    const NvU64 owned = fConsoleAperture ? fConsSize : fMemBytes;
    IOByteCount bytes = (IOByteCount)nvrmApertureBytes(fPitch, fH, owned);
    if (!bytes) return NULL;
    IOService *nub = pciNub();
    if (nub) {
        IOItemCount n = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < n; i++) {
            IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
            IOByteCount len = m->getLength();
            if (target < (NvU64)pa || bytes > len || target - (NvU64)pa > len - bytes) continue;
            IOMemoryDescriptor *sub = IOSubMemoryDescriptor::withSubRange(
                m, (IOByteCount)(target - (NvU64)pa), bytes, kIODirectionNone);
            if (sub) {
                if (fApertureLogged < 3) { fApertureLogged++;
                    FBLOG("getApertureRange -> SUBRANGE of BAR%u (pa 0x%llx len %llu) off 0x%llx size %llu",
                          (unsigned)i, (unsigned long long)pa, (unsigned long long)len,
                          target - (NvU64)pa, (unsigned long long)bytes); }
                return (IODeviceMemory *)sub;
            }
        }
        if (fApertureLogged < 3)
            FBLOG("getApertureRange: no BAR of %u contains phys 0x%llx (+%llu)",
                  (unsigned)n, target, (unsigned long long)bytes);
    } else if (fApertureLogged < 3) {
        FBLOG("getApertureRange: no IOPCIDevice ancestor found");
    }
    if (fApertureLogged < 3) { fApertureLogged++;
        FBLOG("getApertureRange -> FALLBACK withAddressRange 0x%llx size %llu (MapperNone)",
              target, (unsigned long long)bytes); }
    return (IODeviceMemory *)IOMemoryDescriptor::withAddressRange(
        target, bytes, kIODirectionNone | kIOMemoryMapperNone, NULL);
}
IOReturn NVRMNVDAFramebuffer::getAttribute(IOSelect attribute, uintptr_t *value)
{
    FBSEL(attribute);
    if (attribute == kIOWindowServerActiveAttribute) { if (value) *value = 1; FBLOG("getAttribute 'wsrv' -> 1"); return kIOReturnSuccess; }
    if (attribute == kIOHardwareCursorAttribute) {
        uintptr_t v = (fCursorEnabled && fCurSurf && fCurKva) ? 1 : 0;
        if (value) *value = v;
        FBLOG("getAttribute '%s' -> %lu (hardware cursor)", _s, (unsigned long)v);
        return kIOReturnSuccess;
    }
    IOReturn r = super::getAttribute(attribute, value);
    // WindowServer polls some attributes many times a second ('hdcs', HDCP state: 2,019 of 2,703 framebuffer lines in
    // one report, issue #216), which buried every real error. An attribute is logged again only when its answer changes.
    static struct { IOSelect a; IOReturn r; uintptr_t v; } seen[16];
    static unsigned nseen;
    const uintptr_t v = value ? *value : 0;
    unsigned i = 0;
    while (i < nseen && seen[i].a != attribute) i++;
    if (i == nseen || seen[i].r != r || seen[i].v != v) {
        FBLOG("getAttribute '%s' -> 0x%x val %lu", _s, r, (unsigned long)v);
        if (i == nseen && nseen < 16) nseen++;
        if (i < 16) { seen[i].a = attribute; seen[i].r = r; seen[i].v = v; }
    }
    return r;
}
IOReturn NVRMNVDAFramebuffer::getAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t *value)
{
    FBSEL(attribute);
    switch (attribute) {
    case kConnectionEnable: if (value) *value = 1; FBLOG("getAFC '%s' -> enable 1", _s); return kIOReturnSuccess;
    case kConnectionFlags:  if (value) *value = fInternal ? kIOConnectionBuiltIn : 0; return kIOReturnSuccess;
    case kConnectionColorModesSupported:       if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionColorDepthsSupported:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerColorDepth:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerDepthsSupported: if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionColorMode:                 if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionCheckEnable:               if (value) *value = 1; return kIOReturnSuccess;
    case kConnectionDisplayFlags:              if (value) *value = 0; return kIOReturnSuccess;
    case kConnectionSupportsHLDDCSense:
        FBLOG("fb%u getAFC HLDDCSense -> %s (EDID %u B)", fIndex, fEdidSize >= 128 ? "YES" : "no", (unsigned)fEdidSize);
        return fEdidSize >= 128 ? kIOReturnSuccess : kIOReturnUnsupported;
    case kConnectionSupportsAppleSense: case kConnectionSupportsLLDDCSense: case kConnectionDisplayParameterCount: case kConnectionDisplayParameters:
        return kIOReturnUnsupported;
    default: {
        IOReturn r = super::getAttributeForConnection(connectIndex, attribute, value);
        FBLOG("getAFC '%s' -> super 0x%x val %lu", _s, r, value ? (unsigned long)*value : 0ul);
        return r;
    }
    }
}
// Display sleep turns the head off through NVKMS and wake commits the mode and the front surface again, as NVIDIA's
// Linux driver does for DPMS. WAS: both were only logged, so a sleeping display kept showing its last frame, frozen,
// and never went to standby (issue #90). -nvrmnodpms restores the old behaviour.
bool NVRMNVDAFramebuffer::headOff()
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    if (!fKms || !fDev) return false;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr);
    hr->modeSetConfig.bActive = NV_FALSE;
    hr->modeSetConfig.numDisplays = 0;
    hr->flags.activeChanged = hr->flags.displaysChanged = NV_TRUE;
    NvBool ok = submitConfig(cfg, rep, NV_TRUE);
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok ? true : false;
}
bool NVRMNVDAFramebuffer::setHeadPower(bool awake)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    char nb[8] = { 0 };
    const bool dpms = !PE_parse_boot_argn("-nvrmnodpms", nb, sizeof nb);
    if (OSAddAtomic(0, &fStopping)) return false;
    if (awake) {
        // fHeadSlept is set by display sleep (DPMS) and by a GPU standby, so it is honoured with -nvrmnodpms too.
        if (!fHeadSlept) return true;
        // On a system wake this framebuffer and NVRM are both power children of the GPU's PCI device and macOS may
        // wake either first: while NVRM has not resumed the GPU, never push to NVKMS here.
        // NVRM's "NVRMGpuPower" 1 lights the head once the GPU is back.
        if (!rmGpuReady()) {
            FBLOG("display wake: GPU not resumed yet - head %u is lit when NVRM resumes it", fHead);
            return true;
        }
        const bool ok = reflip();
        // A rejected wake must remain retryable on the next power request.
        if (ok) fHeadSlept = false;
        FBLOG("display wake: head %u mode and surface committed again -> %s (rc %d)", fHead, ok ? "ok" : "FAILED", fReflipRc);
        return ok;
    }
    if (!dpms || !fModeSet || !fSurf || fHeadSlept) return true;
    if (!rmGpuReady()) {   // NVRM already put the GPU in standby, and NVKMS turned every head off with it
        fHeadSlept = true;
        FBLOG("display sleep: GPU already in standby - NVKMS turned head %u off", fHead);
        return true;
    }
    const bool ok = headOff();
    if (ok) fHeadSlept = true;
    FBLOG("display sleep: head %u off -> %s", fHead, ok ? "ok (monitor goes to standby)" : "FAILED (last frame stays)");
    return ok;
}
IOReturn NVRMNVDAFramebuffer::setPowerState(unsigned long ordinal, IOService *device)
{
    FBLOG("setPowerState %lu", (unsigned long)ordinal);
    if (ordinal) {
        fPowerOn = true;
        // a failed wake stays retryable (fHeadSlept kept) but WindowServer is still told the display is on, as 1.14.7 did:
        // without the notification it never redraws, and the head can sit black with nothing left to retry it (review)
        (void)setHeadPower(true);
        // A system sleep cleared pagingState (IOGraphics' system work, with kIOFBNotifyWillSleep to every client).
        // This override replaces IOFramebuffer::setPowerState, so the family's wake work never runs, and
        // handleEvent() does nothing off the controller's power thread - so no client ever saw kIOFBNotifyDidWake,
        // and WindowServer's wake acknowledgement waited for pagingState forever (spindump + iogdiagnose:
        // PS 0, every client's last event 3 = WillSleep). Do the family's wake work here instead; it delivers
        // DidPowerOn too. Only after a real system sleep: at start pagingState is still clear too, and the wake work
        // then sent every client a DidWake at boot.
        if (!pagingState && fSysSlept) { fSysSlept = false; familyDidWake(); }
        else handleEvent(kIOFBNotifyDidPowerOn);
    } else {
        handleEvent(kIOFBNotifyWillPowerOff);
        fPowerOn = false;
        (void)setHeadPower(false);
    }
    return kIOPMAckImplied;
}
IOReturn NVRMNVDAFramebuffer::setAttribute(IOSelect attribute, uintptr_t value)
{
    char c[5] = { (char)(attribute >> 24), (char)(attribute >> 16), (char)(attribute >> 8), (char)attribute, 0 };
    if (attribute == kIOWindowServerActiveAttribute) {
        FBLOG("setAttribute 'wsrv' %lu -> success", (unsigned long)value);
        return kIOReturnSuccess; }
    if (attribute == kIOPowerAttribute) {
        FBLOG("setAttribute POWER '%s' -> %lu", c, (unsigned long)value);
        if (value) {
            IOReturn r = super::setAttribute(attribute, value);
            handleEvent(kIOFBNotifyDidPowerOn);
            return (r == kIOReturnUnsupported) ? kIOReturnSuccess : r;
        }
        handleEvent(kIOFBNotifyWillPowerOff);
        IOReturn r = super::setAttribute(attribute, value);
        return (r == kIOReturnUnsupported) ? kIOReturnSuccess : r;
    }
    IOReturn r = super::setAttribute(attribute, value);
    FBLOG("setAttribute '%s' value %lu -> 0x%x", c, (unsigned long)value, r);
    return r;
}
// What IOGraphics does when a framebuffer powers on after system sleep (IOFBController::computeState, then
// IOFramebuffer::handleEvent(kIOFBNotifyDidWake) in IOGraphics-599): tell the clients (NVAccel's NVDisplayPipe,
// AppleDisplay, ...) the framebuffer woke, set pagingState and wake WindowServer, which sleeps in _extEntry on
// fServerConnect until pagingState is set. Under the framebuffer gate, so WindowServer cannot miss the wakeup.
void NVRMNVDAFramebuffer::familyDidWake()
{
    IOWorkLoop *wl = getWorkLoop();
    if (!fWakeGate && wl) {   // a command gate on the framebuffer's own IOGraphics workloop (the one WindowServer sleeps on)
        fWakeGate = IOCommandGate::commandGate(this);
        if (fWakeGate && wl->addEventSource(fWakeGate) != kIOReturnSuccess) OSSafeReleaseNULL(fWakeGate);
    }
    if (!fWakeGate) {
        FBLOG("system wake: no command gate on the framebuffer workloop - WindowServer stays waiting for paging");
        return;
    }
    IOReturn r = fWakeGate->runAction(OSMemberFunctionCast(IOCommandGate::Action, this, &NVRMNVDAFramebuffer::familyDidWakeGated));
    FBLOG("system wake: family wake work -> 0x%x", r);
}
IOReturn NVRMNVDAFramebuffer::sleepTap(OSObject *self, void *, IOFramebuffer *, IOIndex event, void *info)
{
    NVRMNVDAFramebuffer *fb = OSDynamicCast(NVRMNVDAFramebuffer, self);
    if (!fb || info) return kIOReturnSuccess;
    if (event == kIOFBNotifyWillSleep) OSIncrementAtomic(&fb->fWillSleepOff);
    else if (event == kIOFBNotifyDidWake && fb->fWillSleepOff > 0) OSDecrementAtomic(&fb->fWillSleepOff);
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::familyDidWakeGated(void *, void *, void *, void *)
{
    // handleEvent(kIOFBNotifyDidWake, true) delivers DidWake twice: info true from its case, then info false at its
    // end. The second one re-enables the accelerators that WillSleep(0) disabled at sleep: without it
    // IOGraphicsAccelerator2 stayed disabled (no displayModeDidChange after wake), so every IOSurface lock/purge waited
    // in acceleratorWaitEnabled - Terminal hung, moved windows left black areas, restart stalled.
    // IOGraphicsAccelerator2 counts WillSleep(false) as a mode change in progress, and a sleep that starts from a dark
    // wake sends another one without a DidWake in between: after four maintenance dark wakes one DidWake(false) left
    // the count at 4 and the accelerator off. Send as many DidWake(false) as WillSleep(false) arrived.
    const SInt32 owed = fWillSleepOff;
    IOReturn r1 = deliverFramebufferNotification(kIOFBNotifyDidWake, (void *)true);
    IOReturn r2 = deliverFramebufferNotification(kIOFBNotifyDidWake, (void *)false);
    for (SInt32 i = 1; i < owed; ++i)
        r2 = deliverFramebufferNotification(kIOFBNotifyDidWake, (void *)false);
    IOReturn r3 = deliverFramebufferNotification(kIOFBNotifyDidPowerOn, (void *)false);
    pagingState = true;
    fWakeGate->commandWakeup(&fServerConnect, false);   // IOGraphicsWorkLoop::wakeupGate, as handleEvent(DidWake) does
    FBLOG("system wake: clients told DidWake true and %d x false (0x%x/0x%x) for %d sleep(s), DidPowerOn (0x%x), "
          "paging on, WindowServer woken", owed > 1 ? (int)owed : 1, r1, r2, (int)owed, r3);
    fWillSleepOff = 0;
    return kIOReturnSuccess;
}
// NVRM answers "NVRMGpuReady" with NotReady while the GPU is in system sleep or lost. An NVRM without power
// management does not know the call: treat that as ready, the behaviour before.
bool NVRMNVDAFramebuffer::rmGpuReady()
{
    IOService *rm = getProvider();
    while (rm && strcmp(rm->getMetaClass()->getClassName(), "NVRM") != 0) rm = rm->getProvider();
    const OSSymbol *fn = OSSymbol::withCString("NVRMGpuReady");
    IOReturn r = (rm && fn) ? rm->callPlatformFunction(fn, false, NULL, NULL, NULL, NULL) : kIOReturnUnsupported;
    OSSafeReleaseNULL(fn);
    return r != kIOReturnNotReady;
}
// After NVRM resumed the GPU: commit the mode and the front surface again (NVKMS shut every head down for the
// standby), unless the display itself is still asleep - then setPowerState(1) does it.
void NVRMNVDAFramebuffer::gpuWakeFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    NVRMDisplayGate gate; if (!gate.valid()) return;
    if (!me->fHeadSlept || !me->fPowerOn || !me->fModeSet || !me->fSurf) {
        FBLOG("GPU back: head %u left as is (slept %u, power %u)", me->fHead, (unsigned)me->fHeadSlept, (unsigned)me->fPowerOn);
        return;
    }
    const bool ok = me->setHeadPower(true);   // a rejected wake stays retryable on the next power request
    FBLOG("GPU back: head %u re-lit -> %s", me->fHead, ok ? "ok" : "FAILED");
}
IOReturn NVRMNVDAFramebuffer::setAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t value)
{
    FBSEL(attribute);
    // Head power follows setPowerState only, the one path that has shipped; a second path here was new on every monitor
    // with no report asking for it, and its errors were new to callers that always got success (1.15 review).
    if (attribute == kConnectionPower) { FBLOG("setAFC '%s' POWER %lu -> success", _s, (unsigned long)value); return kIOReturnSuccess; }
    switch (attribute) {
    case kConnectionColorModesSupported:
    case kConnectionColorDepthsSupported:
    case kConnectionControllerColorDepth:
    case kConnectionControllerDepthsSupported:
    case kConnectionColorMode:
    case kConnectionDisplayFlags:
    case kConnectionFlags:
        FBLOG("setAFC '%s' value %lu -> ACCEPTED", _s, (unsigned long)value);
        return kIOReturnSuccess;
    default: break;
    }
    IOReturn r = super::setAttributeForConnection(connectIndex, attribute, value);
    FBLOG("setAFC '%s' value %lu -> 0x%x", _s, (unsigned long)value, r);
    return r;
}
IOReturn NVRMNVDAFramebuffer::getDDCBlock(IOIndex, UInt32 blockNumber, IOSelect blockType, IOOptionBits, UInt8 *data, IOByteCount *length)
{
    if (blockType != kIODDCBlockTypeEDID || blockNumber == 0 || blockNumber * 128 > fEdidSize) return kIOReturnUnsupported;
    if (*length < 128) return kIOReturnBadArgument;
    nvu_copy(data, fEdid + (blockNumber - 1) * 128, 128); *length = 128;
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::registerForInterruptType(IOSelect type, IOFBInterruptProc proc, OSObject *target, void *ref, void **interruptRef)
{
    char t[5] = { (char)(type >> 24), (char)(type >> 16), (char)(type >> 8), (char)type, 0 };
    if (type == kIOFBVBLInterruptType) {
        fVbl = { proc, target, ref }; if (interruptRef) *interruptRef = &fVbl; fVblReg++;
        FBLOG("registerForInterruptType '%s' -> VBL REGISTERED (proc %p)", t, proc);
        return kIOReturnSuccess;
    }
    if (type == kIOFBConnectInterruptType) {
        fConnect = { proc, target, ref }; if (interruptRef) *interruptRef = &fConnect;
        FBLOG("registerForInterruptType '%s' -> CONNECT REGISTERED", t);
        return kIOReturnSuccess;
    }
    FBLOG("registerForInterruptType '%s' -> REFUSED (unsupported)", t);
    return kIOReturnUnsupported;
}
