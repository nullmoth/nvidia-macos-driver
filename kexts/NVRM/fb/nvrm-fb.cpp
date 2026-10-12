/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <sys/sysctl.h>
#include <kern/thread_call.h>
#include <mach/mach_time.h>
#include <IOKit/IODeviceMemory.h>
#include <IOKit/IOPlatformExpert.h>
#include <IOKit/IOSubMemoryDescriptor.h>
#include <IOKit/graphics/IOFramebuffer.h>
#include <IOKit/graphics/IOGraphicsTypes.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/OSAtomic.h>
#include "nvrm-hud.h"
extern "C" {
#include "nvkms.h"
#include "nvrm_gpuva_abi.h"
}
#include <libkern/OSAtomic.h>
#include "nvrm_vram_abi.h"
typedef NvU64 (*nvrm_phys_for_va_t)(const void *va);
#include <mach/kmod.h>
extern "C" kern_return_t _start(kmod_info_t *ki, void *d); extern "C" kern_return_t _stop(kmod_info_t *ki, void *d);
extern "C" { KMOD_EXPLICIT_DECL(com.nullmoth.NVRMFB, "0.1", _start, _stop) }
extern "C" kern_return_t _start(kmod_info_t *ki, void *d) { return KERN_SUCCESS; }
extern "C" kern_return_t _stop(kmod_info_t *ki, void *d)  { return KERN_SUCCESS; }
static inline void nvu_zero(void *p, size_t n) { __asm__ __volatile__("rep stosb" : "+D"(p), "+c"(n) : "a"(0) : "memory"); }
static inline void nvu_copy(void *d, const void *s, size_t n) { __asm__ __volatile__("rep movsb" : "+D"(d), "+S"(s), "+c"(n) : : "memory"); }
#define FBTRACE(buf) do { trace(buf); } while (0)
#define FBSEL(a) char _s[5] = { (char)((a) >> 24), (char)((a) >> 16), (char)((a) >> 8), (char)(a), 0 }
#define FBLOG(fmt, ...) do { IOLog("NVRM-fb: " fmt "\n", ##__VA_ARGS__); kprintf("NVRM-fb: " fmt "\n", ##__VA_ARGS__); \
    { char _tb[220]; snprintf(_tb, sizeof _tb, fmt, ##__VA_ARGS__); FBTRACE(_tb); } } while (0)
static const char kPixelFormats[] = IO32BitDirectPixels "\0";
static const IODisplayModeID kModeId = 1;
static const unsigned kTraceMax = 900;

class NVRMNVDAFramebuffer : public IOFramebuffer {
    OSDeclareDefaultStructors(NVRMNVDAFramebuffer)
    struct NvKmsKapiFunctionsTable *fKms = nullptr; NvU32 fGpuId = 0; nvrm_phys_for_va_t fPhysForVa = nullptr;
    struct NvKmsKapiDevice *fDev = nullptr;
    NvKmsKapiDisplay fDisplay = 0; NvKmsKapiConnector fConnector = 0; NvU32 fHead = 0;
    unsigned fIndex = 0;
    bool fBootConsole = false;
    struct NvKmsKapiDisplayMode fMode = {};
    struct NvKmsKapiMemory *fMem = nullptr; struct NvKmsKapiSurface *fSurf = nullptr;
    void *fKva = nullptr; NvU64 fPhys = 0, fSize = 0; NvU32 fPitch = 0, fW = 0, fH = 0;
    IODeviceMemory *fConsMem = nullptr; IOMemoryMap *fConsMap = nullptr;
    void *fConsKva = nullptr; NvU64 fConsPhys = 0, fConsSize = 0; NvU32 fConsPitch = 0;
    NvU64 fVtFbBase = 0, fVtFbSize = 0;
    void *fUserPhys = nullptr;
    IOService *fPciNub = nullptr;
    IOService *pciNub();
    IOMemoryDescriptor *fBarMem = nullptr; IOMemoryMap *fBarMap = nullptr;
    void *fBarKva = nullptr; NvU64 fBarLen = 0;
    void surveyBar1();
    bool fConsoleAperture = false;
    NvU64 fBar1Base = 0;
    NvU8 fEdid[NVKMS_KAPI_EDID_BUFFER_SIZE]; NvU16 fEdidSize = 0;
    bool fModeSet = false;
    unsigned fLastOk = 0; int fFlipResult = -99;
    unsigned fParkedCount = 0;
    struct NvKmsKapiMemory *fCurMem = nullptr; struct NvKmsKapiSurface *fCurSurf = nullptr;
    void *fCurKva = nullptr; NvU32 fCurDim = 0;
    NvU32 fCurMaxPx = 0, fCurCompModes = 0;
    SInt32 fCurX = 0, fCurY = 0; bool fCurVisible = false; bool fCurHaveImage = false;
    unsigned fCurApplies = 0; int fCurRc = 0;
    bool fCursorEnabled = false;
    bool cursorInit();
    bool cursorApply();
    uint64_t fVblNext = 0;
    unsigned fHudFrames = 0;
    NvU32 fNumHeads = 0, fNumConnectors = 0;
    struct { IOFBInterruptProc proc; OSObject *target; void *ref; } fVbl = {}, fConnect = {};
    thread_call_t fSampler = nullptr; unsigned fSamples = 0;
    static void trace(const char *s);
    thread_call_t fVblTimer = nullptr;
    thread_call_t fBars = nullptr;
    struct NVFlipEntry { struct NvKmsKapiMemory *mem; struct NvKmsKapiSurface *surf;
                         unsigned w, h, pitch; };
    NVFlipEntry fFlipCache[16] = {};
    unsigned    fFlipCacheN = 0, fFlipCount = 0, fFlipRejects = 0;
    struct NvKmsKapiSurface *fFrontSurf = nullptr;
    bool flipToMemory(struct NvKmsKapiMemory *mem, unsigned w, unsigned h, unsigned pitch, int *rcOut, bool pure = false);
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
    static void eventCallback(const struct NvKmsKapiEvent *event) { FBLOG("kapi event type %d", (int)event->type); }
    bool kapiInit();
    bool applyMode();
    void hudDraw();
public:
    bool start(IOService *provider) override;
    void stop(IOService *provider) override;
    IOReturn enableController() override;
    const char *getPixelFormats() override { return kPixelFormats; }
    IOItemCount getDisplayModeCount() override { return 1; }
    IOReturn getDisplayModes(IODisplayModeID *allDisplayModes) override { allDisplayModes[0] = kModeId; return kIOReturnSuccess; }
    IOReturn getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info) override;
    IOReturn getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info) override;
    UInt64 getPixelFormatsForDisplayMode(IODisplayModeID, IOIndex) override { return 0; }
    IOReturn getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi) override;
    IOReturn getCurrentDisplayMode(IODisplayModeID *mode, IOIndex *depth) override { if (mode) *mode = kModeId; if (depth) *depth = 0; return kIOReturnSuccess; }
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
    IOReturn setAttribute(IOSelect attribute, uintptr_t value) override;
    IOReturn setPowerState(unsigned long ordinal, IOService *device) override;
    IOReturn setCursorImage(void *cursorImage) override;
    IOReturn setCursorState(SInt32 x, SInt32 y, bool visible) override;
    bool isConsoleDevice(void) override { return fBootConsole; }
    IOReturn setGammaTable(UInt32, UInt32, UInt32, void *) override { return kIOReturnSuccess; }
    IOReturn setGammaTable(UInt32, UInt32, UInt32, void *, bool) override { return kIOReturnSuccess; }
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
    NvKmsKapiDisplay handle; NvKmsKapiConnector connector; NvU32 headMask;
    NvU32 edidSize; NvU8 edid[NVKMS_KAPI_EDID_BUFFER_SIZE];
};
static struct NvKmsKapiDevice *gDev;
static struct nvrm_dpy_rec gDpy[NVKMS_KAPI_MAX_CONNECTORS * 2];
static int gNDpy = -1;
static int gBootHead = -2;
static NvKmsKapiDisplay gBootDpy = 0;
static struct NvKmsKapiDisplayMode gBootMode;
static NvU32 gHeadsTaken;

class NVRMFBClaim : public IOService
{
    OSDeclareDefaultStructors(NVRMFBClaim)
public:
    bool start(IOService *provider) override
    {
        if (!IOService::start(provider)) return false;
        IOLog("NVRMFB: NVRMFBClaim holds the IOFramebuffer match category on %s - IONDRVFramebuffer blocked\n",
              provider ? provider->getName() : "?");
        return true;
    }
};
OSDefineMetaClassAndStructors(NVRMFBClaim, IOService)

#define super IOFramebuffer

static void nvrmHeadConfigBaseline(struct NvKmsKapiHeadRequestedConfig *hr)
{
    hr->modeSetConfig.olutFpNormScale = NVKMS_OLUT_FP_NORM_SCALE_DEFAULT;
    hr->flags.olutFpNormScaleChanged = NV_TRUE;
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
            FBLOG("dpy %u/%u 0x%x: connected %u edid %u B connector 0x%x headMask 0x%x%s",
                  i, n, handles[i], dd->connected, dd->edid.bufferSize,
                  gs ? (unsigned)si.connectorHandle : 0u, gs ? (unsigned)si.headMask : 0u,
                  (dd->connected && !gs) ? " STATIC INFO FAILED" : "");
            if (!dd->connected || !gs) continue;
            if (gNDpy >= (int)(sizeof gDpy / sizeof gDpy[0])) { FBLOG("dpy table full at %d", gNDpy); break; }
            struct nvrm_dpy_rec *r = &gDpy[gNDpy++];
            r->handle = handles[i]; r->connector = si.connectorHandle; r->headMask = si.headMask;
            r->edidSize = dd->edid.bufferSize > sizeof r->edid ? (NvU32)sizeof r->edid : dd->edid.bufferSize;
            nvu_copy(r->edid, dd->edid.buffer, r->edidSize);
        }
        IOFree(dd, sizeof *dd);
        if (getProvider()) getProvider()->setProperty("connected-displays", (unsigned long long)gNDpy, 32);
        FBLOG("ENUMERATED: %d connected display(s) of %u handle(s), %u heads available",
              gNDpy, n, fNumHeads);
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
    if (gNDpy <= 0 || fIndex >= (unsigned)gNDpy) {
        FBLOG("index %u: only %d display(s) connected -- nothing to drive, not starting", fIndex, gNDpy);
        return false;
    }
    {
        struct nvrm_dpy_rec *r = &gDpy[fIndex];
        fDisplay = r->handle; fConnector = r->connector;
        fEdidSize = (NvU16)(r->edidSize > sizeof fEdid ? sizeof fEdid : r->edidSize);
        nvu_copy(fEdid, r->edid, fEdidSize);
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
        gHeadsTaken |= (1u << fHead);
        FBLOG("index %u -> display 0x%x connector 0x%x headMask 0x%x -> head %u (taken now 0x%x)",
              fIndex, fDisplay, (unsigned)fConnector, (unsigned)r->headMask, fHead,
              (unsigned)gHeadsTaken);
    }
    bool have = false;
    for (NvU32 i = 0; i < 256; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, preferred = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &preferred);
        if (r < 0) break;
        if (r == 0 || !valid) continue;
        if (!have || preferred) { fMode = m; have = true; }
        if (preferred) break;
    }
    if (!have) { FBLOG("no valid mode"); return false; }
    if (gBootHead < 0 && fIndex == 0) fBootConsole = true;
    if (gBootHead >= 0 && fDisplay == gBootDpy && (int)fHead == gBootHead) {
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
        mp.size   = fSize;
        mp.useVideoMemory = kTryOrder[t];
        mp.compressible   = &compressible;
        fMem = fKms->allocateMemory(fDev, &mp);
        if (!fMem) { FBLOG("%s: allocateMemory(%llu) failed", what, fSize); continue; }
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
                         && (kernelPhys < consEnd) && ((kernelPhys + fSize) > consBase);
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
        if (haveAperture) { FBLOG("%s ACCEPTED -- aperture 0x%llx", what, fPhys); break; }
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
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = fPitch;
    sp.width = fW; sp.height = fH; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
    fSurf = fKms->createSurface(fDev, &sp);
    if (!fSurf) {
        FBLOG("createSurface failed");
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
        if (!cm) cm = IOMemoryDescriptor::withAddressRange(fConsPhys, (IOByteCount)fConsSize,
                                                          kIODirectionNone | kIOMemoryMapperNone, NULL);
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
        if (r == kIOReturnSuccess && img && dims[0] && dims[1] && dims[2] >= dims[0] * 4) {
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
static IOLock *startLock()
{
    static IOLock *volatile gL = nullptr;
    if (!gL) {
        IOLock *l = IOLockAlloc();
        if (l && !OSCompareAndSwapPtr(nullptr, l, (void *volatile *)&gL)) IOLockFree(l);
    }
    return gL;
}
void NVRMNVDAFramebuffer::sampleFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    if (!me->fKva) return;
    volatile uint32_t *px = (volatile uint32_t *)((me->fConsoleAperture && me->fConsKva) ? me->fConsKva : me->fKva);
    unsigned w = me->fW, h = me->fH, ppr = me->fPitch / 4;
    uint32_t sum = 0; unsigned notGrey = 0, n = 0;
    for (unsigned y = 0; y < h; y += 8) for (unsigned x = 0; x < w; x += 8) { uint32_t v = px[y * ppr + x] & 0xffffff; sum = sum * 33 + v; if (v != 0x303030) notGrey++; n++; }
    FBLOG("sample %u: sum 0x%08x not-grey %u/%u px(100,100)=0x%08x px(960,540)=0x%08x px(1800,1000)=0x%08x modeset %u | phys 0x%llx size %llu pitch %u %ux%u ok %u flip %d | disp 0x%x head %u conn 0x%x heads %u conns %u reflips %u rc %d",
          me->fSamples, sum, notGrey, n,
          px[100 * ppr + 100], px[540 * ppr + 960], px[1000 * ppr + 1800], me->fModeSet,
          me->fPhys, me->fSize, me->fPitch, me->fW, me->fH, me->fLastOk, me->fFlipResult,
          me->fDisplay, me->fHead, me->fConnector, me->fNumHeads, me->fNumConnectors,
          me->fReflips, me->fReflipRc);
    FBLOG("  vblreg %u vblcalls %llu", me->fVblReg, me->fVblCalls);
    if (me->fConsKva && me->fConsPitch >= 4) {
        volatile uint32_t *cp = (volatile uint32_t *)me->fConsKva;
        unsigned cppr = me->fConsPitch / 4; uint32_t csum = 0;
        for (unsigned y = 0; y < h; y += 8) for (unsigned x = 0; x < w; x += 8) csum = csum * 33 + (cp[y * cppr + x] & 0xffffff);
        FBLOG("  cons@0x%llx: sum 0x%08x px(100,100)=0x%06x px(960,540)=0x%06x px(1800,1000)=0x%06x",
              me->fConsPhys, csum, cp[100 * cppr + 100] & 0xffffff,
              cp[540 * cppr + 960] & 0xffffff, cp[1000 * cppr + 1800] & 0xffffff);
    }
    me->fSamples++;
    uint64_t deadline; clock_interval_to_deadline(10, kSecondScale, &deadline); thread_call_enter_delayed(me->fSampler, deadline);
}
bool NVRMNVDAFramebuffer::reflip()
{
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
    nvrmHeadConfigBaseline(hr);
    hr->modeSetConfig.bActive = NV_TRUE;
    hr->modeSetConfig.numDisplays = 1;
    hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = front;
    lr->config.srcWidth = fW; lr->config.srcHeight = fH;
    lr->config.dstWidth = fW; lr->config.dstHeight = fH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
    lr->config.minPresentInterval = 0;
    lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged =
        lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    fReflipRc = ok ? (int)rep->flipResult : -1;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok ? true : false;
}
void NVRMNVDAFramebuffer::reflipFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    if (!me->fReflip) return;
    bool rok = me->reflip(); me->fReflips++;
    FBLOG("reflip %u -> ok %u rc %d (head %u display 0x%x surface %p)",
          me->fReflips, (unsigned)rok, me->fReflipRc, me->fHead, me->fDisplay, me->fSurf);
    static const unsigned kGap[8] = { 12, 12, 12, 12, 12, 15, 15, 30 };
    if (me->fReflips >= 8) { return; }
    uint64_t deadline; clock_interval_to_deadline(kGap[me->fReflips], kSecondScale, &deadline);
    thread_call_enter_delayed(me->fReflip, deadline);
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
    FBLOG("cursor plane READY: %ux%u A8R8G8B8 pitch %u (device maxCursorSizeInPixels %u, validCompModes 0x%x)",
          dim, dim, dim * 4, fCurMaxPx, fCurCompModes);
    return true;
}
bool NVRMNVDAFramebuffer::cursorApply()
{
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
    struct NvKmsKapiCursorRequestedConfig *cr = &cfg->headRequestedConfig[fHead].cursorRequestedConfig;
    cr->surface = (fCurVisible && fCurHaveImage) ? fCurSurf : nullptr;
    cr->compParams.compMode    = NVKMS_COMPOSITION_BLENDING_MODE_PREMULT_ALPHA;
    cr->compParams.surfaceAlpha = 0;
    cr->dstX = (NvS16)fCurX; cr->dstY = (NvS16)fCurY;
    cr->flags.surfaceChanged = NV_TRUE; cr->flags.dstXYChanged = NV_TRUE;
    NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    fCurRc = ok ? (int)rep->flipResult : -1;
    if (fCurApplies < 3 || (fCurApplies % 1000) == 0)
        FBLOG("cursor apply %u: ok %u rc %d at %d,%d visible %u",
              fCurApplies, (unsigned)ok, fCurRc, (int)fCurX, (int)fCurY, (unsigned)fCurVisible);
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
    if (!cursorApply()) { fCurHaveImage = false; cursorApply(); return kIOReturnUnsupported; }
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::setCursorState(SInt32 x, SInt32 y, bool visible)
{
    if (!fCurSurf) return kIOReturnUnsupported;
    if (x == fCurX && y == fCurY && visible == fCurVisible && fCurRc == NV_KMS_FLIP_RESULT_SUCCESS)
        return kIOReturnSuccess;
    fCurX = x; fCurY = y; fCurVisible = visible;
    return cursorApply() ? kIOReturnSuccess : kIOReturnError;
}
void NVRMNVDAFramebuffer::barsFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
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
    thread_call_enter_delayed(me->fBars, deadline);
}
void NVRMNVDAFramebuffer::vblFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    if (!me->fVblTimer) return;
    if (me->fVbl.proc) { me->fVbl.proc(me->fVbl.target, me->fVbl.ref); me->fVblCalls++; }
    uint64_t period = 0, now = 0;
    nanoseconds_to_absolutetime(16666667ull, &period);
    clock_get_uptime(&now);
    if (me->fVblNext == 0) me->fVblNext = now;
    me->fVblNext += period;
    if (me->fVblNext <= now) me->fVblNext = now + period;
    thread_call_enter_delayed(me->fVblTimer, me->fVblNext);
}
bool NVRMNVDAFramebuffer::setHeadActive(bool active)
{
    if (!fKms || !fDev) return false;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr);
    hr->modeSetConfig.bActive = active ? NV_TRUE : NV_FALSE;
    hr->flags.activeChanged = NV_TRUE;
    if (active) {
        hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = fMode;
        hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = fSurf;
        lr->config.srcWidth = fW; lr->config.srcHeight = fH;
        lr->config.dstWidth = fW; lr->config.dstHeight = fH;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
        lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged =
            lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    }
    NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    int rc = (int)rep->flipResult;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    FBLOG("HEAD PROBE: setHeadActive(%u) on head %u -> ok %u rc %d", (unsigned)active, fHead, (unsigned)ok, rc);
    return ok ? true : false;
}
void NVRMNVDAFramebuffer::headProbeFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
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
    thread_call_enter_delayed(me->fHeadProbe, d);
}
bool NVRMNVDAFramebuffer::applyMode()
{
    if (fConsoleAperture) {
        FBLOG("applyMode: skipped (console aperture mode -- the GOP drives this panel)");
        fModeSet = true; return true;
    }
    if (fModeSet) return true;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) return false;
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    nvrmHeadConfigBaseline(hr);
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay; hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = fSurf; lr->config.srcWidth = fW; lr->config.srcHeight = fH; lr->config.dstWidth = fW; lr->config.dstHeight = fH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
    lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    fLastOk = (unsigned)ok; fFlipResult = (int)rep->flipResult;
    FBLOG("applyModeSetConfig -> %u (flipResult %d)", ok, (int)rep->flipResult);
    if (ok && fKms->framebufferConsoleDisabled) {
        fKms->framebufferConsoleDisabled(fDev);
        FBLOG("framebufferConsoleDisabled() sent -- NVKMS releases the old GOP console");
    }
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    fModeSet = ok;
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
bool NVRMNVDAFramebuffer::start(IOService *provider)
{
    {
        int gate = 0;
        if (!PE_parse_boot_argn("nvfb", &gate, sizeof gate) || !gate) {
            IOLog("NVRMFB: boot-arg nvfb=1 absent: NOT starting (boot-loop guard)\n");
            return false;
        }
    }
    fKms = (struct NvKmsKapiFunctionsTable *)(uintptr_t)propU64(provider, "nvkms-kapi");
    fGpuId = (NvU32)propU64(provider, "gpu-id");
    fIndex = (unsigned)propU64(provider, "fb-index");
    if (fIndex == 0 && gFB != this) { retain(); gFB = this; }
    fPhysForVa = (nvrm_phys_for_va_t)(uintptr_t)propU64(provider, "phys-for-va");
    if (!fKms || !fPhysForVa) { FBLOG("nub lacks nvkms-kapi/phys-for-va properties"); return false; }
    setProperty("IOFBDependentID", (unsigned long long)0x4E56524D00000001ull, 64);
    setProperty("IOFBDependentIndex", (unsigned long long)fIndex, 32);
    setProperty("IOFBMemorySize", (unsigned long long)(8ull << 30), 64);
    IOLock *sl = startLock();
    if (!sl) { FBLOG("start lock alloc failed -- refusing to start"); return false; }
    IOLockLock(sl); bool kapiOk = kapiInit(); IOLockUnlock(sl);
    if (!kapiOk) {
        FBLOG("kapiInit failed -- refusing to start. super::start() was NOT called, so there is "
              "nothing half-registered to unwind and the machine stays up without a display.");
        return false;
    }
    setName("NVRMFramebuffer");
    if (!super::start(provider)) { FBLOG("super::start failed after kapiInit succeeded"); return false; }
    fVblTimer = thread_call_allocate(vblFire, this);
    if (fVblTimer) { uint64_t d; clock_interval_to_deadline(16667, kMicrosecondScale, &d); thread_call_enter_delayed(fVblTimer, d); }
    FBLOG("vbl timer %s", fVblTimer ? "armed (60 Hz)" : "ALLOCATE FAILED");
    fReflip = thread_call_allocate(reflipFire, this);
    FBLOG("reflip keep-alive NOT armed: enableController's modeset owns the head, no boot-time re-syncs");
    fBars = thread_call_allocate(barsFire, this);
    uint32_t hudGate = 0;
    if (fBars && PE_parse_boot_argn("nvhud", &hudGate, sizeof hudGate) && hudGate) {
        uint64_t d; clock_interval_to_deadline(45, kSecondScale, &d);
        thread_call_enter_delayed(fBars, d);
        FBLOG("PROBE GRID + HUD ARMED (boot-arg nvhud=1): fires at t+45 s");
    } else {
        FBLOG("probe grid + HUD OFF -- macOS only (add nvhud=1 to boot-args to show them)");
    }
    fHeadProbe = thread_call_allocate(headProbeFire, this);
    FBLOG("head probe retired (answered: we own head %u)", fHead);
    uint32_t sampleGate = 0;
    if (PE_parse_boot_argn("nvfbsample", &sampleGate, sizeof sampleGate) && sampleGate) {
        fSampler = thread_call_allocate(sampleFire, this);
        if (fSampler) { uint64_t deadline; clock_interval_to_deadline(10, kSecondScale, &deadline); thread_call_enter_delayed(fSampler, deadline); }
        FBLOG("pixel sampler ARMED (boot-arg nvfbsample=1): every 10 s");
    } else {
        FBLOG("pixel sampler OFF -- add nvfbsample=1 to boot-args to arm it");
    }
    registerPowerDriver(this, kFBPowerStates, 2);
    FBLOG("registerPowerDriver(2 states) done");
    FBLOG("started on %s", provider->getName());
    return true;
}
void NVRMNVDAFramebuffer::stop(IOService *provider) { if (fReflip) { thread_call_t r = fReflip; fReflip = nullptr; thread_call_cancel(r); thread_call_free(r); } if (fBars) { thread_call_t b = fBars; fBars = nullptr; thread_call_cancel(b); thread_call_free(b); } if (fVblTimer) { thread_call_t t = fVblTimer; fVblTimer = nullptr; thread_call_cancel(t); thread_call_free(t); } if (fSampler) { thread_call_cancel(fSampler); thread_call_free(fSampler); fSampler = nullptr; } if (fHeadProbe) { thread_call_t h = fHeadProbe; fHeadProbe = nullptr; thread_call_cancel(h); thread_call_free(h); } super::stop(provider); }

IOReturn NVRMNVDAFramebuffer::enableController()
{
    FBLOG("enableController");
    return applyMode() ? kIOReturnSuccess : kIOReturnError;
}
IOReturn NVRMNVDAFramebuffer::setDisplayMode(IODisplayModeID mode, IOIndex depth)
{
    FBLOG("setDisplayMode %d depth %d", (int)mode, (int)depth);
    if (mode != kModeId || depth != 0) return kIOReturnUnsupportedMode;
    return applyMode() ? kIOReturnSuccess : kIOReturnError;
}
IOReturn NVRMNVDAFramebuffer::getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info)
{
    if (mode != kModeId || !info) return kIOReturnBadArgument;
    nvu_zero(info, sizeof *info);
    info->nominalWidth = fW; info->nominalHeight = fH; info->maxDepthIndex = 0;
    info->refreshRate = fMode.timings.refreshRate ? (UInt32)(((UInt64)fMode.timings.refreshRate << 16) / 1000) : (60u << 16);
    info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info)
{
    if (mode != kModeId || !info) return kIOReturnUnsupportedMode;
    nvu_zero(info, sizeof *info);
    info->appleTimingID = kIOTimingIDInvalid;
    info->flags = 0;
    FBLOG("getTimingInfoForDisplayMode(%d) -> success", (int)mode);
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi)
{
    if (mode != kModeId || depth != 0 || aperture != kIOFBSystemAperture || !pi) return kIOReturnUnsupportedMode;
    nvu_zero(pi, sizeof *pi);
    pi->activeWidth = fW; pi->activeHeight = fH; pi->bytesPerRow = fPitch; pi->bytesPerPlane = 0;
    pi->bitsPerPixel = 32; pi->pixelType = kIORGBDirectPixels; pi->componentCount = 3; pi->bitsPerComponent = 8;
    pi->componentMasks[0] = 0x00ff0000; pi->componentMasks[1] = 0x0000ff00; pi->componentMasks[2] = 0x000000ff;
    strlcpy(pi->pixelFormat, IO32BitDirectPixels, sizeof pi->pixelFormat);
    pi->flags = 0;
    FBLOG("getPixelInformation ap %d -> %ux%u bytesPerRow %u", (int)aperture, fW, fH, fPitch);
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
            if (target < (NvU64)pa || (target + bytes) > ((NvU64)pa + len)) continue;
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

#define NVRM_VRAM_PARK_CEILING (24ull * 1024ull * 1024ull)
static struct { struct NvKmsKapiMemory *m; void *k; NvU64 n; } gParkedForever[24];
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
    sysctl_register_oid(&sysctl__debug_nvrmfb_vramtest); sysctl_register_oid(&sysctl__debug_nvrmfb_vramtest_mode);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_interval); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_us_sum);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_n); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_us_max);
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_lean);
}

bool NVRMNVDAFramebuffer::vramGrant(struct NVRMVramRequest *r)
{
    nvrmfbInstallSysctl();
    r->phys = 0; r->kva = nullptr; r->handle = nullptr; r->actualSize = 0; r->mappedTotal = 0;
    if (!fKms || !fDev) { FBLOG("nvAllocVram: no NVKMS device yet"); return false; }
    NvU64 want = (r->size + 0xFFFFull) & ~0xFFFFull;
    if (!want) return false;

    const SInt64 budget = (SInt64)(fBarLen >= (4ull << 30) ? fBarLen / 2 : NVRM_VRAM_BAR1_BUDGET);
    gVramBudgetBytes = budget;
    SInt64 before = OSAddAtomic64((SInt64)want, &gVramMappedBytes) + gVramParkedBytes;
    if (before + (SInt64)want > budget) {
        OSAddAtomic64(-(SInt64)want, &gVramMappedBytes);
        FBLOG("nvAllocVram(%llu): REFUSED — BAR1 budget spent (%lld of %llu bytes). "
              "This is the resizable-BAR ceiling, not a VRAM shortage.",
              (unsigned long long)want, (long long)before, (unsigned long long)budget);
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
        if (!m) { FBLOG("nvAllocVram(%llu): allocateMemory failed", (unsigned long long)want); break; }
        void *k = nullptr;
        if (!fKms->mapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &k) || !k) {
            FBLOG("nvAllocVram(%llu): mapMemory(KERNEL) failed", (unsigned long long)want);
            fKms->freeMemory(fDev, m); break;
        }
        NvU64 p = fPhysForVa ? fPhysForVa(k) : 0;
        bool onScanout = p && fPhys  && fSize     && p < (fPhys + fSize)         && (p + want) > fPhys;
        bool onConsole = p && fConsPhys && fConsSize && p < (fConsPhys + fConsSize) && (p + want) > fConsPhys;
        if (!p || onScanout || onConsole) {
            FBLOG("nvAllocVram: attempt %d phys 0x%llx %s — parking it and rolling again",
                  attempt, (unsigned long long)p,
                  !p ? "has no physical address" : onScanout ? "OVERLAPS THE SCANOUT" : "OVERLAPS THE CONSOLE");
            if (gNParkedForever < (int)(sizeof gParkedForever / sizeof gParkedForever[0]) &&
                gVramParkedBytes + (SInt64)want <= (SInt64)NVRM_VRAM_PARK_CEILING) {
                gParkedForever[gNParkedForever].m = m;
                gParkedForever[gNParkedForever].k = k;
                gParkedForever[gNParkedForever].n = want;
                gNParkedForever++;
                OSAddAtomic64((SInt64)want, &gVramParkedBytes);
            } else {
                FBLOG("nvAllocVram: park ceiling reached (%lld bytes over %d slots) — releasing the reject",
                      (long long)gVramParkedBytes, gNParkedForever);
                fKms->unmapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, k); fKms->freeMemory(fDev, m);
            }
            continue;
        }
        mem = m; kva = k; phys = p;
    }
    if (!mem) { OSAddAtomic64(-(SInt64)want, &gVramMappedBytes); return false; }

    nvu_zero(kva, (size_t)want);
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
    struct NvKmsKapiMemory *mem = (struct NvKmsKapiMemory *)r->handle;
    if (!mem || !fKms || !fDev) return false;
    if (r->kva) fKms->unmapMemory(fDev, mem, NVKMS_KAPI_MAPPING_TYPE_KERNEL, r->kva);
    fKms->freeMemory(fDev, mem);
    if (r->actualSize) OSAddAtomic64(-(SInt64)r->actualSize, &gVramMappedBytes);
    OSAddAtomic64(1, &gCntRelease); OSAddAtomic64((SInt64)r->actualSize, &gCntReleaseBytes);
    r->handle = nullptr; r->kva = nullptr; r->phys = 0; r->actualSize = 0;
    return true;
}

IOReturn NVRMNVDAFramebuffer::callPlatformFunction(const OSSymbol *functionName, bool waitForFunction,
                                               void *param1, void *param2, void *param3, void *param4)
{
    bool isAlloc = functionName && functionName->isEqualTo(NVRM_VRAM_FN_ALLOC);
    bool isFree  = functionName && functionName->isEqualTo(NVRM_VRAM_FN_FREE);
    if (functionName && functionName->isEqualTo(NVRM_VRAM_FN_SCANOUT)) {
        struct NVRMVramRequest *r = (struct NVRMVramRequest *)param1;
        if (!r || r->version != NVRM_VRAM_ABI_VERSION) return kIOReturnBadArgument;
        if (!fPhys || !fKva || !fPitch || !fH) { FBLOG("nvScanoutInfo: no scanout yet"); return kIOReturnNotReady; }
        r->phys       = fPhys;
        r->kva        = fKva;
        r->actualSize = (unsigned long long)fPitch * fH;
        r->width = fW; r->height = fH; r->pitch = fPitch;
        r->handle = fMem;
        FBLOG("nvScanoutInfo -> phys 0x%llx %ux%u pitch %u (%llu bytes)",
              (unsigned long long)fPhys, fW, fH, fPitch, (unsigned long long)r->actualSize);
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
        if (!f || f->version != NVRM_GPUVA_ABI_VERSION) return kIOReturnBadArgument;
        int rc = -1;
        const bool home = (f->flags & 1u) != 0;
        bool ok = flipToMemory(home ? nullptr : (struct NvKmsKapiMemory *)f->kapiMemory,
                               f->width, f->height, f->pitch, &rc, !home);
        f->flipResult = rc;
        return ok ? kIOReturnSuccess : kIOReturnIOError;
    }
    if (functionName && functionName->isEqualTo(NVRM_FLIP_FN)) {
        struct NVRMFlipRequest *f = (struct NVRMFlipRequest *)param1;
        if (!f || f->version != NVRM_GPUVA_ABI_VERSION) return kIOReturnBadArgument;
        int rc = -1;
        bool ok = flipToMemory((struct NvKmsKapiMemory *)f->kapiMemory,
                               f->width, f->height, f->pitch, &rc);
        f->flipResult = rc;
        return ok ? kIOReturnSuccess : kIOReturnIOError;
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
                                   unsigned pitch, int *rcOut, bool pure)
{
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
    for (unsigned i = 0; mem && i < fFlipCacheN; i++)
        if (fFlipCache[i].mem == mem && fFlipCache[i].pitch == pitch &&
            fFlipCache[i].w == w && fFlipCache[i].h == h) { surf = fFlipCache[i].surf; break; }
    if (!surf) {
        struct NvKmsKapiCreateSurfaceParams sp = {};
        sp.planes[0].memory = mem; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
        sp.width = w; sp.height = h; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
        surf = fKms->createSurface(fDev, &sp);
        if (!surf) { FBLOG("flipToMemory: createSurface %ux%u pitch %u FAILED", w, h, pitch); return false; }
        if (fFlipCacheN < 16) {
            fFlipCache[fFlipCacheN].mem = mem; fFlipCache[fFlipCacheN].surf = surf;
            fFlipCache[fFlipCacheN].w = w; fFlipCache[fFlipCacheN].h = h;
            fFlipCache[fFlipCacheN].pitch = pitch; fFlipCacheN++;
            FBLOG("flipToMemory: new surface %p for memory %p (%ux%u pitch %u) [%u cached]",
                  surf, mem, w, h, pitch, fFlipCacheN);
        }
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
    lr->config.dstWidth = fW; lr->config.dstHeight = fH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
    lr->config.minPresentInterval = pure ? (NvU8)(gFlipInterval < 0 ? 0 : gFlipInterval > 4 ? 4 : gFlipInterval) : 0;
    lr->config.tearing = NV_FALSE;
    lr->flags.surfaceChanged = NV_TRUE;
    if (!lean)
        lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    const uint64_t ft0 = mach_absolute_time();
    NvBool ok = fKms->applyModeSetConfig(fDev, cfg, rep, NV_TRUE);
    if (pure) { const SInt64 us = (SInt64)((mach_absolute_time() - ft0) / 1000);
        OSAddAtomic64(us, &gFlipUsSum); OSAddAtomic64(1, &gFlipN); if (us > gFlipUsMax) gFlipUsMax = us; }
    int rc = ok ? (int)rep->flipResult : -1;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (rcOut) *rcOut = rc;
    if (ok) fFrontSurf = surf;
    fFlipCount++;
    if (fFlipCount <= 8 || (fFlipCount % 600) == 0)
        FBLOG("flipToMemory #%u -> ok %u rc %d (surface %p memory %p)",
              fFlipCount, (unsigned)ok, rc, surf, mem);
    return ok ? true : false;
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
    IOByteCount bytes = (IOByteCount)((NvU64)fPitch * fH + 128);
    IOService *nub = pciNub();
    if (nub) {
        IOItemCount n = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < n; i++) {
            IOMemoryDescriptor *m = nub->getDeviceMemoryWithIndex(i);
            if (!m) continue;
            addr64_t pa = m->getPhysicalSegment(0, 0, kIOMemoryMapperNone);
            IOByteCount len = m->getLength();
            if (target < (NvU64)pa || (target + bytes) > ((NvU64)pa + len)) continue;
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
    FBLOG("getAttribute '%s' -> 0x%x val %lu", _s, r, value ? (unsigned long)*value : 0ul);
    return r;
}
IOReturn NVRMNVDAFramebuffer::getAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t *value)
{
    FBSEL(attribute);
    switch (attribute) {
    case kConnectionEnable: if (value) *value = 1; FBLOG("getAFC '%s' -> enable 1", _s); return kIOReturnSuccess;
    case kConnectionFlags:  if (value) *value = 0; return kIOReturnSuccess;
    case kConnectionColorModesSupported:       if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionColorDepthsSupported:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerColorDepth:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerDepthsSupported: if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionColorMode:                 if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionCheckEnable:               if (value) *value = 1; return kIOReturnSuccess;
    case kConnectionDisplayFlags:              if (value) *value = 0; return kIOReturnSuccess;
    case kConnectionSupportsHLDDCSense: return fEdidSize >= 128 ? kIOReturnSuccess : kIOReturnUnsupported;
    case kConnectionSupportsAppleSense: case kConnectionSupportsLLDDCSense: case kConnectionDisplayParameterCount: case kConnectionDisplayParameters:
        return kIOReturnUnsupported;
    default: {
        IOReturn r = super::getAttributeForConnection(connectIndex, attribute, value);
        FBLOG("getAFC '%s' -> super 0x%x val %lu", _s, r, value ? (unsigned long)*value : 0ul);
        return r;
    }
    }
}
IOReturn NVRMNVDAFramebuffer::setPowerState(unsigned long ordinal, IOService *device)
{
    FBLOG("setPowerState %lu", (unsigned long)ordinal);
    if (ordinal) handleEvent(kIOFBNotifyDidPowerOn);
    else         handleEvent(kIOFBNotifyWillPowerOff);
    return kIOPMAckImplied;
}
IOReturn NVRMNVDAFramebuffer::setAttribute(IOSelect attribute, uintptr_t value)
{
    char c[5] = { (char)(attribute >> 24), (char)(attribute >> 16), (char)(attribute >> 8), (char)attribute, 0 };
    if (attribute == kIOWindowServerActiveAttribute) { FBLOG("setAttribute 'wsrv' %lu -> success", (unsigned long)value); return kIOReturnSuccess; }
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
IOReturn NVRMNVDAFramebuffer::setAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t value)
{
    FBSEL(attribute);
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
