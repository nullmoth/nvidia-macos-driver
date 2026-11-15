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
#include <sys/vnode.h>
#include <sys/uio.h>
#include <sys/fcntl.h>
#include <sys/stat.h>
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
#include "../nvrm_igpu_proxy.h"
#include "../nvrm_audio_match.h"
#include "../../NMHDA/nmhda_topology.h"
#include <libkern/c++/OSArray.h>

// Every callback this driver schedules runs one at a time. A plain thread_call that is entered again while it is still
// running starts again on another thread, so a vblank or cursor callback stuck behind the display gate began a new
// thread on every vblank until the 'high' thread_call group reached its 500-thread cap and the kernel panicked (#295).
// THREAD_CALL_OPTIONS_ONCE folds those entries into one more run after the current one returns.
static inline thread_call_t nvrmfbCall(thread_call_func_t fn, thread_call_param_t param)
{
    return thread_call_allocate_with_options(fn, param, THREAD_CALL_PRIORITY_HIGH, THREAD_CALL_OPTIONS_ONCE);
}
#include "nvrm-hud.h"
#include "nvrm-fb-safety.h"
#include "nvrmfb_scanout_reserve.h"
#include <IOKit/i2c/IOI2CInterface.h>
#include <IOKit/IOMessage.h>
#include "../nvrm_ddc_abi.h"
#include "../nvrm_off_bar.h"
#include "../nvrm_edp_backlight.h"
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
    explicit NVRMDisplayGate(IORecursiveLock *gate = startLock()) : lock(gate) { if (lock) IORecursiveLockLock(lock); }
    ~NVRMDisplayGate() { if (lock) IORecursiveLockUnlock(lock); }
    bool valid() const { return lock != nullptr; }
};
// The display gate with a deadline, for entry points a WindowServer thread sits in (mode changes). The recursive lock is shared by
// every framebuffer and held across slow work (start-up enumeration, hot-plug relights, flip waits); an unbounded wait here
// was a WindowServer watchdog kill after two minutes. A timeout fails the call and says who is waiting.
class NVRMBoundedGate {
    IORecursiveLock *lock; bool got;
public:
    explicit NVRMBoundedGate(unsigned ms) : lock(startLock()), got(false) {
        if (lock) got = nvrmTryUntil([this] { return IORecursiveLockTryLock(lock) != 0; }, [](unsigned step) { IOSleep(step); }, ms, 2);
    }
    ~NVRMBoundedGate() { if (lock && got) IORecursiveLockUnlock(lock); }
    bool held() const { return got; }
};
static const unsigned kModeChangeGateMs = 20000;
static NVRMAllocationTable<struct NvKmsKapiMemory, 4096> gAllocations;
// The bundled NVKMS headers emit conversion warnings; keep them scoped to these external declarations.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wsign-conversion"
#pragma clang diagnostic ignored "-Wimplicit-int-conversion"
extern "C" {
#include "nvkms.h"
#include "nvrm_gpuva_abi.h"
}
#pragma clang diagnostic pop
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
// Kernel log volume. The lines that fire once per query or per allocation (the attribute and timing answers WindowServer
// asks for again and again, every video memory grant) reach the kernel log only with the boot argument nvfblog=1. They
// still go to the diagnostic trace that Send logs collects. Start-up, mode changes, errors and warnings always log.
static int gFbVerbose = -1;
static inline bool fbVerbose(void)
{
    if (gFbVerbose < 0) { uint32_t g = 0; gFbVerbose = PE_parse_boot_argn("nvfblog", &g, sizeof g) && g ? 1 : 0; }
    return gFbVerbose == 1;
}
#define FBLOGV(fmt, ...) do { if (fbVerbose()) { IOLog("NVRM-fb: " fmt "\n", ##__VA_ARGS__); kprintf("NVRM-fb: " fmt "\n", ##__VA_ARGS__); } \
    { char _tb[220]; snprintf(_tb, sizeof _tb, fmt, ##__VA_ARGS__); FBTRACE(_tb); } } while (0)
static const char kPixelFormats[] = IO32BitDirectPixels "\0";
static const UInt32 kNVRMFBSetTransform = 0x00000400;   // kIOFBSetTransform
static const IODisplayModeID kModeId = 1;
static const unsigned kTraceMax = 900;

// EDID override (see nvrmEdidValid in nvrm-fb-safety.h). NVKMS validates modes against a display's EDID only, so a timing the
// EDID does not list can never be set; an HDMI dummy plug offers 1920x1080 at 60 Hz at most. NVKMS takes a replacement EDID
// with each dynamic-display query (overrideEdid) and does not keep it, so every query has to carry it. The file is read once
// per display handle.
struct nvrm_edid_override { NvKmsKapiDisplay handle; NvU16 size; bool tried; NvU8 buf[NVKMS_KAPI_EDID_BUFFER_SIZE]; };
static struct nvrm_edid_override gEdidOv[8];
static NvU16 nvrmEdidOverrideRead(const NvU8 *real, NvU8 *out, NvU32 cap)
{
    char name[24], path[96];
    nvrmEdidOverrideName(real, name, sizeof name);
    snprintf(path, sizeof path, "/Library/NullMoth/edid-override/%s", name);
    vfs_context_t ctx = vfs_context_create(NULL); vnode_t vp = NULLVP; NvU16 got = 0;
    if (vnode_open(path, FREAD, 0, 0, &vp, ctx) == 0) {
        struct vnode_attr va; VATTR_INIT(&va); VATTR_WANTED(&va, va_data_size); VATTR_WANTED(&va, va_uid); VATTR_WANTED(&va, va_mode);
        if (vnode_getattr(vp, &va, ctx) != 0) IOLog("NVRM-fb: edid override %s: no attributes, ignored\n", path);
        else if (!nvrmFileTrusted(va.va_uid, va.va_mode)) IOLog("NVRM-fb: edid override %s: not root-owned or writable by others, ignored\n", path);
        else if (va.va_data_size < 128 || va.va_data_size > cap) IOLog("NVRM-fb: edid override %s: %llu bytes, ignored\n", path, (unsigned long long)va.va_data_size);
        else {
            uio_t u = uio_create(1, 0, UIO_SYSSPACE, UIO_READ);
            if (u) {
                uio_addiov(u, CAST_USER_ADDR_T(out), (user_size_t)va.va_data_size);
                const int err = VNOP_READ(vp, u, 0, ctx);
                const NvU32 n = (NvU32)va.va_data_size;
                if (err || uio_resid(u) != 0) IOLog("NVRM-fb: edid override %s: read error %d\n", path, err);
                else if (!nvrmEdidValid(out, n) || !nvrmEdidSameDisplay(out, real)) IOLog("NVRM-fb: edid override %s: not a valid EDID for this display, ignored\n", path);
                else { got = (NvU16)n; IOLog("NVRM-fb: edid override %s: %u bytes in use\n", path, n); }
                uio_free(u);
            }
        }
        vnode_close(vp, FREAD, ctx);
    }
    vfs_context_rele(ctx);
    return got;
}
static bool nvrmRootFile(const char *path)
{
    vfs_context_t ctx = vfs_context_create(NULL); vnode_t vp = NULLVP; bool ok = false;
    if (vnode_lookup(path, 0, &vp, ctx) == 0) {
        struct vnode_attr va; VATTR_INIT(&va); VATTR_WANTED(&va, va_uid); VATTR_WANTED(&va, va_mode);
        if (vnode_getattr(vp, &va, ctx) == 0) {
            ok = nvrmFileTrusted(va.va_uid, va.va_mode);
            if (!ok) IOLog("NVRM-fb: %s: not root-owned or writable by others, ignored\n", path);
        }
        vnode_put(vp);
    }
    vfs_context_rele(ctx);
    return ok;
}
// getDynamicDisplayInfo with the display's EDID override, when there is one
static NvBool nvrmDynamicDisplayInfo(struct NvKmsKapiFunctionsTable *kms, struct NvKmsKapiDevice *dev, struct NvKmsKapiDynamicDisplayParams *dd)
{
    const NvKmsKapiDisplay h = dd->handle;
    if (!kms->getDynamicDisplayInfo(dev, dd)) return NV_FALSE;
    if (!dd->connected || dd->edid.bufferSize < 128) return NV_TRUE;
    struct nvrm_edid_override *o = nullptr;
    for (auto &e : gEdidOv) if (e.tried && e.handle == h) { o = &e; break; }
    if (!o) for (auto &e : gEdidOv) if (!e.tried) { o = &e; o->handle = h; o->tried = true; o->size = nvrmEdidOverrideRead(dd->edid.buffer, o->buf, sizeof o->buf); break; }
    if (!o || !o->size) return NV_TRUE;
    dd->overrideEdid = NV_TRUE; dd->edid.bufferSize = o->size; nvu_copy(dd->edid.buffer, o->buf, o->size);
    if (!kms->getDynamicDisplayInfo(dev, dd)) { IOLog("NVRM-fb: edid override for display 0x%x: NVKMS refused it\n", h); return NV_FALSE; }
    return NV_TRUE;
}

// Laptop panel brightness. macOS gives the brightness keys and the brightness slider to its built-in
// display, an AppleBacklightDisplay. IOGraphics creates one (AppleBacklightDisplay::probe) for a connection
// that reports kIOConnectionBuiltIn and an LCD from getAppleSense(), provided a service named "backlight"
// existed when the first framebuffer was opened (gIOFBHaveBacklight, IOFramebuffer::open). On Macs that is
// the ACPI PNLF device; here it is this handler, published by the panel's framebuffer before super::start().
// IODisplay finds it below the framebuffer (searchParameterHandlers) and offers "brightness" and
// "linear-brightness" through it; both use the normalized NVKMS request range. The level goes to NVRM
// ("NVRMBacklight": nvKmsSetBacklight on the backlight NVKMS registered), from a thread call, because
// IODisplay calls doIntegerSet() with the framebuffer lock held.
// AppleBacklightDisplay's public source uses 0..255 when no power-state range is published.
static const SInt32 kAppleBacklightDefaultMax = 255;
static const UInt32 kNVRMBacklightSaveDelaySeconds = 2;
#define BLLOG(fmt, ...) do { IOLog("NVRM-fb: " fmt "\n", ##__VA_ARGS__); kprintf("NVRM-fb: " fmt "\n", ##__VA_ARGS__); } while (0)

class NVRMBacklightHandler : public IODisplayParameterHandler {
    OSDeclareDefaultStructors(NVRMBacklightHandler)
    IOService *fRM = nullptr;
    const OSSymbol *fFn = nullptr;
    thread_call_t fApply = nullptr;
    thread_call_t fPersist = nullptr;
    volatile SInt32 fPending = -1;
    SInt32 fLevel = kNVRMBacklightMax;
    volatile SInt32 fUserLevel = kNVRMBacklightMax;
    volatile SInt32 fPersistPending = -1;
    volatile UInt64 fPersistDeadline = 0;
    SInt32 fRestoreLevel = -1;
    bool fEdp = false; unsigned fEdpGpu = 0, fEdpDisplay = 0; EdpBacklightCaps fEdpCaps = {};   // DPCD brightness fallback, see publishBacklight
    bool fHaveSaved = false;            // NVRAM held a saved level when this handler was made
    volatile bool fUserSeen = false;    // a user request other than a start-up default has arrived
    volatile bool fUserMoved = false;   // a user request below the maximum has arrived (ends external restores)
    OSDictionary *fParameters = nullptr;
    static void applyFire(thread_call_param_t me, thread_call_param_t);
    static void persistFire(thread_call_param_t me, thread_call_param_t);
    static const OSSymbol *sBrightnessProbe, *sLinearBrightnessProbe;
public:
    static NVRMBacklightHandler *withRM(IOService *rm, NvU32 level);
    unsigned edpGpu() const { return fEdpGpu; } unsigned edpDisplay() const { return fEdpDisplay; } const EdpBacklightCaps &edpCaps() const { return fEdpCaps; }
    void useEdpAux(unsigned gpu, unsigned display, const EdpBacklightCaps &caps) { fEdp = true; fEdpGpu = gpu; fEdpDisplay = display; fEdpCaps = caps; }
    bool setDisplay(IODisplay *display) override;
    bool doIntegerSet(OSDictionary *params, const OSSymbol *paramName, UInt32 value) override;
    bool doDataSet(const OSSymbol *, OSData *) override { return false; }
    bool doUpdate() override { return true; }
    void free() override;
    int restoreExternal(SInt32 level);
};
OSDefineMetaClassAndStructors(NVRMBacklightHandler, IODisplayParameterHandler)
// The internal panel's handler, for debug.nvrmfb_backlight_restore. Set by publishBacklight once the handler is
// registered, cleared by stop() before the handler is released; read and retained under gBacklightLock.
static IOLock *gBacklightLock = nullptr;
static NVRMBacklightHandler *gBacklightHandler = nullptr;
static void nvrmBacklightPublishGlobal(NVRMBacklightHandler *h)
{
    if (!gBacklightLock) { IOLock *l = IOLockAlloc(); if (l && !OSCompareAndSwapPtr(nullptr, l, (void * volatile *)&gBacklightLock)) IOLockFree(l); }
    if (!gBacklightLock) return;
    IOLockLock(gBacklightLock);
    if (h) h->retain();
    NVRMBacklightHandler *old = gBacklightHandler;
    gBacklightHandler = h;
    IOLockUnlock(gBacklightLock);
    if (old) old->release();
}
static void nvrmBacklightClearGlobal(NVRMBacklightHandler *h)
{
    if (!gBacklightLock || !h) return;
    IOLockLock(gBacklightLock);
    const bool mine = gBacklightHandler == h;
    if (mine) gBacklightHandler = nullptr;
    IOLockUnlock(gBacklightLock);
    if (mine) h->release();
}
// Write-only: a logical level 0..65535 to restore while the user has not moved the brightness this boot. Firmware that
// drops NVRAM writes (OpenCore with DisableVariableWrite and no emulated NVRAM) brings /options back with an old level at
// every restart; the installed backlight job keeps its own copy of the last level and writes it here when /options
// disagrees with it. Reading returns -1.
static int nvrmfbBacklightRestoreSysctl SYSCTL_HANDLER_ARGS
{
    int v = -1;
    int err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || !req->newptr) return err;
    if (!gBacklightLock) return ENODEV;
    IOLockLock(gBacklightLock);
    NVRMBacklightHandler *h = gBacklightHandler;
    if (h) h->retain();
    IOLockUnlock(gBacklightLock);
    if (!h) return ENODEV;
    err = h->restoreExternal((SInt32)v);
    h->release();
    return err;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_backlight_restore, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, NULL, 0,
            nvrmfbBacklightRestoreSysctl, "I", "restore a saved panel brightness (0..65535) before the user has moved it this boot");
const OSSymbol *NVRMBacklightHandler::sBrightnessProbe = nullptr;
const OSSymbol *NVRMBacklightHandler::sLinearBrightnessProbe = nullptr;

// One DPCD transaction through NVRM (nvrm_ddc.cpp): at most 16 bytes within 0x700..0x7ff.
static IOReturn nvrmDpAux(IOService *rm, unsigned gpu, unsigned display, bool write, unsigned addr, unsigned char *d, unsigned len)
{
    const OSSymbol *fn = OSSymbol::withCString(NVRM_DPAUX_FN);
    struct NVRMDpAuxRequest *q = (struct NVRMDpAuxRequest *)IOMalloc(sizeof *q);
    if (!rm || !fn || !q || len > kNvrmDpAuxMax) { OSSafeReleaseNULL(fn); if (q) IOFree(q, sizeof *q); return kIOReturnNoMemory; }
    nvu_zero(q, sizeof *q);
    q->gpuId = gpu; q->displayId = display; q->write = write ? 1 : 0; q->addr = addr; q->len = len;
    if (write) nvu_copy(q->data, d, len);
    IOReturn r = rm->callPlatformFunction(fn, false, q, NULL, NULL, NULL);
    if (r == kIOReturnSuccess && !write) nvu_copy(d, q->data, len);
    IOFree(q, sizeof *q); fn->release();
    return r;
}
// Brightness over the eDP AUX channel: read the current mode and enable bytes, write what changes (nvrm_edp_backlight.h).
static IOReturn nvrmEdpSet(NVRMBacklightHandler *h, IOService *rm, unsigned percent);
NVRMBacklightHandler *NVRMBacklightHandler::withRM(IOService *rm, NvU32 level)
{
    NVRMBacklightHandler *h = OSTypeAlloc(NVRMBacklightHandler);
    if (!h || !h->init()) { OSSafeReleaseNULL(h); return nullptr; }
    h->fRM = rm; rm->retain();
    h->fFn = OSSymbol::withCStringNoCopy("NVRMBacklight");
    if (!sBrightnessProbe) sBrightnessProbe = OSSymbol::withCStringNoCopy("brightness-probe");
    if (!sLinearBrightnessProbe) sLinearBrightnessProbe = OSSymbol::withCStringNoCopy("linear-brightness-probe");
    h->fApply = nvrmfbCall(applyFire, h);
    h->fPersist = thread_call_allocate_with_options(persistFire, h, THREAD_CALL_PRIORITY_LOW, THREAD_CALL_OPTIONS_ONCE);
    h->fLevel = (SInt32)nvrm_backlight_from_percent(level <= 100 ? level : 100);
    UInt32 saved;
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (options && nvrm_backlight_read(options, &saved)) {
        h->fUserLevel = (SInt32)saved;
        h->fRestoreLevel = (SInt32)saved;
        h->fHaveSaved = true;
    } else {
        h->fUserLevel = h->fLevel;
    }
    OSSafeReleaseNULL(options);
    if (!h->fFn || !h->fApply || !h->fPersist) { h->release(); return nullptr; }
    return h;
}

void NVRMBacklightHandler::free()
{
    if (fApply) { thread_call_cancel(fApply); while (!thread_call_free(fApply)) { thread_call_cancel(fApply); IOSleep(1); } fApply = nullptr; }
    if (fPersist) { thread_call_cancel(fPersist); while (!thread_call_free(fPersist)) { thread_call_cancel(fPersist); IOSleep(1); } fPersist = nullptr; }
    OSSafeReleaseNULL(fFn);
    OSSafeReleaseNULL(fRM);
    OSSafeReleaseNULL(fParameters);
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
    IODisplay::addParameter(params, gIODisplayBrightnessKey, 0, kNVRMBacklightMax);
    SInt32 initial = OSAddAtomic(0, &fUserLevel);
    IODisplay::setParameter(params, gIODisplayBrightnessKey, initial);
    IODisplay::addParameter(params, gIODisplayLinearBrightnessKey, 0, kNVRMBacklightMax);
    IODisplay::setParameter(params, gIODisplayLinearBrightnessKey, initial);
    // CoreDisplay converts between user and linear brightness by writing one probe and reading the other.
    IODisplay::addParameter(params, sBrightnessProbe, 0, kNVRMBacklightMax);
    IODisplay::setParameter(params, sBrightnessProbe, initial);
    IODisplay::addParameter(params, sLinearBrightnessProbe, 0, kNVRMBacklightMax);
    IODisplay::setParameter(params, sLinearBrightnessProbe, initial);
    display->setProperty(gIODisplayParametersKey, params);
    // The restore sysctl reads fParameters from its own thread: swap under the lock it snapshots under, release after.
    if (gBacklightLock) IOLockLock(gBacklightLock);
    OSDictionary *replaced = fParameters;
    fParameters = params;
    if (gBacklightLock) IOLockUnlock(gBacklightLock);
    OSSafeReleaseNULL(replaced);
    if (fRestoreLevel >= 0) {
        fPending = fRestoreLevel;
        fRestoreLevel = -1;
        thread_call_enter(fApply);
    }
    BLLOG("backlight: brightness parameters on %s, logical level %d/65535", display->getName(), (int)fLevel);
    return true;
}

// User changes arrive with the parameter's own { min, max, value } (IODisplay::setForKey stores the value);
// AppleBacklightDisplay's power states (dim, display sleep, lid) pass the whole IODisplayParameters.
bool NVRMBacklightHandler::doIntegerSet(OSDictionary *params, const OSSymbol *paramName, UInt32 value)
{
    const bool probe = paramName == sBrightnessProbe || paramName == sLinearBrightnessProbe;
    if (!probe && paramName != gIODisplayBrightnessKey && paramName != gIODisplayLinearBrightnessKey) return false;
    SInt32 level = value > kNVRMBacklightMax ? kNVRMBacklightMax : (SInt32)value;
    if (probe) {
        // No transfer function is reported by NVKMS; preserve the normalized request without applying it.
        return fParameters && IODisplay::setParameter(fParameters,
            paramName == sBrightnessProbe ? sLinearBrightnessProbe : sBrightnessProbe, level);
    }
    bool userRequest = params && params->getObject(gIODisplayValueKey);
    // The three flags are also written by the restore sysctl; decide and record under the lock it uses.
    if (gBacklightLock) IOLockLock(gBacklightLock);
    const bool startupDefault = nvrm_backlight_startup_default(userRequest, (UInt32)level, fHaveSaved, fUserSeen, OSAddAtomic(0, &fUserLevel));
    if (!startupDefault && userRequest) {
        fUserSeen = true;
        if (level < (SInt32)kNVRMBacklightMax) fUserMoved = true;
    }
    if (gBacklightLock) IOLockUnlock(gBacklightLock);
    if (startupDefault) {
        BLLOG("backlight: first user request is the maximum with a saved level -- start-up default, not saved");
        userRequest = false;
    }
    if (userRequest) {
        uint64_t deadline;
        clock_interval_to_deadline(kNVRMBacklightSaveDelaySeconds, kSecondScale, &deadline);
        __atomic_store_n(&fPersistDeadline, deadline, __ATOMIC_SEQ_CST);
        __atomic_store_n(&fUserLevel, level, __ATOMIC_SEQ_CST);
        if (fParameters) IODisplay::setParameter(fParameters,
            paramName == gIODisplayBrightnessKey ? gIODisplayLinearBrightnessKey : gIODisplayBrightnessKey, level);
    }
    else {
        SInt32 target = OSAddAtomic(0, &fUserLevel);
        if (value == kAppleBacklightDefaultMax || level > target) level = target;
    }
    BLLOG("backlight: %s request logical=%d/65535 user-target=%d/65535",
        userRequest ? "user" : "power", (int)level, (int)OSAddAtomic(0, &fUserLevel));
    fPending = level;
    thread_call_enter(fApply);
    return true;
}

// debug.nvrmfb_backlight_restore: the same restore setDisplay makes from /options, for a level kept outside NVRAM.
int NVRMBacklightHandler::restoreExternal(SInt32 level)
{
    if (level < 0 || level > (SInt32)kNVRMBacklightMax) return EINVAL;
    // This runs on the caller's thread while the display's own thread may be in setDisplay or doIntegerSet: the check,
    // the flags and a retained snapshot of the parameter dictionary are taken under one lock.
    OSDictionary *shown = nullptr;
    if (gBacklightLock) IOLockLock(gBacklightLock);
    const bool allowed = nvrm_backlight_external_restore_ok(fUserMoved, level);
    if (allowed) {
        __atomic_store_n(&fUserLevel, level, __ATOMIC_SEQ_CST);
        fHaveSaved = true;
        fUserSeen = false;
        shown = fParameters;
        if (shown) shown->retain();
    }
    if (gBacklightLock) IOLockUnlock(gBacklightLock);
    if (!allowed) {
        BLLOG("backlight: external restore of logical %d refused -- the user already moved the brightness this boot", (int)level);
        return EBUSY;
    }
    // Treated like a level read from /options at start: the display stack's start-up request for the maximum is then
    // clamped to it instead of replacing it. Requests seen so far were all for the maximum (fUserMoved is clear), so
    // they were start-up defaults too, and the next one is treated the same way.
    if (shown) {
        IODisplay::setParameter(shown, gIODisplayBrightnessKey, level);
        IODisplay::setParameter(shown, gIODisplayLinearBrightnessKey, level);
        shown->release();
    }
    // Keep /options in step for this boot, so the variable shows what the panel shows; a firmware that keeps writes
    // stores it as usual.
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    if (options) (void)nvrm_backlight_write(options, (UInt32)level);
    OSSafeReleaseNULL(options);
    BLLOG("backlight: external restore logical=%d/65535 (a level kept outside NVRAM)", (int)level);
    fPending = level;
    thread_call_enter(fApply);
    return 0;
}

void NVRMBacklightHandler::applyFire(thread_call_param_t p, thread_call_param_t)
{
    NVRMBacklightHandler *me = (NVRMBacklightHandler *)p;
    SInt32 v;
    do { v = me->fPending; } while (!OSCompareAndSwap((UInt32)v, (UInt32)-1, (volatile UInt32 *)&me->fPending));
    if (v < 0) return;
    NvU32 percent = nvrm_backlight_to_percent((UInt32)v);
    IOReturn r = me->fEdp ? nvrmEdpSet(me, me->fRM, percent) : me->fRM->callPlatformFunction(me->fFn, false, &percent, (void *)1, NULL, NULL);
    if (r == kIOReturnSuccess) {
        me->fLevel = v;
        BLLOG("backlight: applied logical=%d/65535 hardware=%u%% user-target=%d/65535",
            (int)v, (unsigned)percent, (int)OSAddAtomic(0, &me->fUserLevel));
        uint64_t deadline = __atomic_load_n(&me->fPersistDeadline, __ATOMIC_SEQ_CST);
        if (deadline && OSAddAtomic(0, &me->fUserLevel) == v) {
            // Only a successfully applied user target becomes eligible for persistence.
            __atomic_store_n(&me->fPersistPending, v, __ATOMIC_SEQ_CST);
            thread_call_enter_delayed(me->fPersist, deadline);
        }
    } else BLLOG("backlight: setting %u%% failed (0x%x)", (unsigned)percent, r);
}

void NVRMBacklightHandler::persistFire(thread_call_param_t p, thread_call_param_t)
{
    NVRMBacklightHandler *me = (NVRMBacklightHandler *)p;
    SInt32 target = OSAddAtomic(0, &me->fPersistPending);
    if (target < 0 || OSAddAtomic(0, &me->fUserLevel) != target) return;
    uint64_t deadline = __atomic_load_n(&me->fPersistDeadline, __ATOMIC_SEQ_CST);
    if (mach_absolute_time() < deadline) {
        thread_call_enter_delayed(me->fPersist, deadline);
        return;
    }
    IORegistryEntry *options = IORegistryEntry::fromPath("/options", gIODTPlane);
    bool saved = options && nvrm_backlight_write(options, (UInt32)target);
    OSSafeReleaseNULL(options);
    if (saved) {
        OSCompareAndSwap((UInt32)target, (UInt32)-1, (volatile UInt32 *)&me->fPersistPending);
    } else {
        BLLOG("backlight: saving logical target %d failed; retrying", (int)target);
        clock_interval_to_deadline(kNVRMBacklightSaveDelaySeconds, kSecondScale, &deadline);
        thread_call_enter_delayed(me->fPersist, deadline);
    }
}

// 1 once this boot's guard clear happened (a head lit, or nothing to light): NVRM reads it (debug.nvrmfb_guard_cleared)
// before putting its start count back after a settle wait, so a display brought up by hand during the wait is not
// counted as a failed start (nvrm_off.h).
static int gGuardCleared;
static void fbGuardCleared(const char *why);
class NVRMNVDAFramebuffer;

class NVRMDdcTransaction {
    bool *flag;
    bool acquired;
public:
    explicit NVRMDdcTransaction(bool *busy) : flag(busy), acquired(!__atomic_exchange_n(flag, true, __ATOMIC_ACQUIRE)) {}
    ~NVRMDdcTransaction() { if (acquired) __atomic_store_n(flag, false, __ATOMIC_RELEASE); }
    bool held() const { return acquired; }
};

// Some displays start answering DDC/CI after the framebuffer publishes the bus.
// Allow about 26 seconds of scheduled waits before giving up on this display attachment.
static constexpr unsigned kNVRMDdcProbeFirstDelayMs = 350;
static constexpr unsigned kNVRMDdcProbeMaxDelayMs = 2000;
static constexpr unsigned kNVRMDdcProbeMaxRetries = 12;
static constexpr unsigned kNVRMDdcProbeFirstReplyMs = 40;
static constexpr unsigned kNVRMDdcProbeReplyMs = 80;
static constexpr unsigned kNVRMDdcProbeMaxReplyMs = 500;

class NVRMExternalBrightness : public IODisplayParameterHandler {
    OSDeclareDefaultStructors(NVRMExternalBrightness)
    NVRMNVDAFramebuffer *fFramebuffer = nullptr;
    OSDictionary *fParameters = nullptr;
    UInt32 fGeneration = 0;
    UInt32 fMaximum = 0;
    IODisplay *fRetryDisplay = nullptr;
    UInt32 fRetryGeneration = 0;
    unsigned fRetriesLeft = 0;
    unsigned fRetryDelayMs = 0;
    bool publishParameters(IODisplay *display, bool &retry);
public:
    void retryProbe();
    void cancelProbe() { OSSafeReleaseNULL(fRetryDisplay); }
    void setFramebuffer(NVRMNVDAFramebuffer *framebuffer);
    void free() override;
    bool setDisplay(IODisplay *display) override;
    bool doIntegerSet(OSDictionary *params, const OSSymbol *key, UInt32 value) override;
    bool doDataSet(const OSSymbol *, OSData *) override { return false; }
    bool doUpdate() override { return true; }
};
OSDefineMetaClassAndStructors(NVRMExternalBrightness, IODisplayParameterHandler)

class NVRMNVDAFramebuffer : public IOFramebuffer {
    friend class NVRMExternalBrightness;
    OSDeclareDefaultStructors(NVRMNVDAFramebuffer)
    struct NvKmsKapiFunctionsTable *fKms = nullptr; NvU32 fGpuId = 0; nvrm_phys_for_va_t fPhysForVa = nullptr;
    struct NvKmsKapiDevice *fDev = nullptr;
    NvKmsKapiDisplay fDisplay = 0; NvKmsKapiConnector fConnector = 0; NvU32 fHead = 0;
    unsigned fIndex = 0; bool fOwnsHead = false;
    bool fBootConsole = false;
    // Integrated-GPU panel proxy (boot-arg nvfbigpu=1, BIOS Hybrid): no NVIDIA display is connected and the panel is lit
    // by the integrated GPU in stolen memory. Head 0 still starts, so the accelerator gets its NVIDIA device and VRAM
    // through it as usual; the scanout it reports is a CPU mapping of the panel's framebuffer, so the accelerator
    // takes its copy path and NVKMS never sees a flip. Decision logic: nvrm_igpu_proxy.h.
    bool fProxy = false;
    uint64_t fPxBase = 0, fPxBytes = 0;
    uint32_t fPxW = 0, fPxH = 0, fPxPitch = 0;
    IOMemoryMap *fPxMap = nullptr;
    bool proxyFind();
    bool proxyMap();
    bool fInternal = false;     // NVKMS static display info: laptop panel (eDP/LVDS)
    // Variable refresh rate (boot-arg nvfbvrr=1): the display supports it, WindowServer asked for it, and the vertical range
    // the EDID gives. fVrrOn follows the last kIOFBVariableRefreshRate set and is carried by every full mode set.
    uintptr_t fGammaScale[3] = { 0x10000, 0x10000, 0x10000 };   // 'gslr' 'gslg' 'gslb': 1.0 in 16.16 until WindowServer sets one
    bool fVrrSupported = false, fVrrOn = false; unsigned fVrrMinHz = 0, fVrrMaxHz = 0;
    // 10-bit and HDR10 (boot-args nvfb10bit=1, nvfbhdr=1). fHdrCapable: the display's EDID lists PQ. fHdrActive: the mode on the
    // head (or being set) is an HDR10 timing. fScan30: the scanout surface is X2B10G10R10 (told to the accelerator in the
    // nvScanoutInfo reply). fHdrMeta: the mastering metadata WindowServer last sent (kIOFBHDRMetaDataAttribute).
    bool fHdrWant = false;   // the mode being set is HDR10
    bool fPendingScaled = false; unsigned long long fPendingLoad = 0;   // set by setDisplayMode for the mode about to be switched to
    unsigned long long fScalerLimit = 0;   // learned head-scaler load limit (nvrmScalerLoad), 0 = none known
    bool trialScaledMode(const IODetailedTimingInformationV2 &d);
    void probeScalerLimit();
    bool fHdrCapable = false, fHdrActive = false, fScan30 = false, fHdrMetaSet = false; unsigned fHdrMaxLumCode = 0;
    struct NvKmsHDRStaticMetadata fHdrMeta = {};
    IOReturn setHdrMeta(const void *meta);
    static bool vrrAllowed();
    IOReturn setVrr(bool on);
    friend void nvrmfbColorChanged(bool needsModeset);   // a debug.nvrmfb_color_* sysctl changed: apply to every lit head
    bool applyVibrance();
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
    // Surround (boot-arg nvfbsurround=<mask of framebuffer indices>): this framebuffer is the master of a group of displays that
    // appear as one wide display. fSur[0] is the master itself. See nvrmSurroundPlan.
    struct SurMember { NvKmsKapiDisplay display; NvU32 head; struct NvKmsKapiDisplayMode mode; NvU32 x, w; };
    static void surroundCheck();   // after a hot-plug scan: degrade or re-form the surround group
    bool fHidden = false;   // a surround member: offline as far as WindowServer is concerned, audio and head still its own
    SurMember fSur[kNvrmSurMaxMembers]; unsigned fSurN = 0; bool fSurActive = false; unsigned fSurTotalW = 0, fSurH = 0, fSurHz = 0;
    bool planSurround(unsigned masterIndex);
    unsigned addSurroundMode(unsigned n);
    IOReturn switchSurround(IODisplayModeID id);
    void surroundReplicate(struct NvKmsKapiRequestedModeSetConfig *cfg);
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
    IODetailedTimingInformationV2 fDT[512]; unsigned fNDT = 0;
    // 64 slots stay free for the refresh rates NVKMS adds and the alias of the mode on the head.
    static const unsigned kMaxMacTimings = 448;
    IODisplayModeID fCurId = 1;
    struct NvKmsKapiDisplayMode fBootMode = {};
    NvU32 fBootW = 0, fBootH = 0, fBootPitch = 0;
    NvU32 fRasterW = 0, fRasterH = 0;
    // The gamma table WindowServer last set (Night Shift, True Tone, ColorSync calibration, fades), in NVKMS's output-LUT
    // form. Every modeset carries it so a mode change does not drop it; setGammaTable commits it on its own.
    struct NvKmsLutRamps *fGamma = nullptr;
    bool fGammaOn = false;
    // NVKMS 615 takes the output LUT as a surface (the legacy ramp array is gone). Two surfaces, written alternately:
    // the one the head is scanning through is never rewritten underneath it. They live as long as the framebuffer.
    struct NvKmsKapiMemory *fOlutMem[2] = {};
    struct NvKmsKapiSurface *fOlutSurf[2] = {};
    void *fOlutKva[2] = {};
    unsigned fOlutNext = 0;
    struct NvKmsKapiSurface *olutFromGamma();
    unsigned fGammaCommits = 0, fGammaFails = 0;
    IOReturn nvrmSetGamma(UInt32 channels, UInt32 count, UInt32 width, const void *data);
    static void dtSurfaceSize(const IODetailedTimingInformationV2 *d, NvU32 *w, NvU32 *h);
    UInt32 fCurTransform = 0;
    SInt32 fCurHotX = 0, fCurHotY = 0, fCurTurnedHotX = 0, fCurTurnedHotY = 0;
    UInt32 fTransform = 0;
    volatile UInt32 fServerFresh = 0;
    IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler) override;
    void publishStartupTiming();
    void publishRotatedSizes(bool force = true);
    bool fRotSizesOurs = false;   // the display's scale-resolutions were put there by publishRotatedSizes
    bool dtIsCurrent(const IODetailedTimingInformationV2 &d) const;
    bool dtIsBootRaster(const IODetailedTimingInformationV2 &d) const;
    IODetailedTimingInformationV2 fCurDT = {};
    volatile UInt32 fNewIdOnce = 0;
    NvU32 fPitchAlign = 256;
    NvU32 fScalerMaxW = 0, fScalerMaxH = 0;
    // Widest raster offered when it fits the scanout memory. Separate from the scaler limit: a 5120x1440 monitor whose
    // firmware started at 2560x1440 has a 3840-wide scaler limit, yet 5120x1440 needs less memory than 3840x2160.
    NvU32 fRasterMaxW = 0;
    NvU64 fAllocSize = 0, fMemBytes = 0, fModesAllocSize = 0;
    // The scanout and cursor bytes this head mapped into BAR1 (video memory only), counted once into gVramDisplayBytes.
    NvU64 fScanoutBarBytes = 0, fCursorBarBytes = 0;
    unsigned fModeSwitches = 0, fValidateCalls = 0, fValidateOk = 0;
    bool fBootInDT = false;
    bool fKmsModesLogged = false;   // the per-mode NVKMS listing is printed once, on the first enumeration
    int dtIndex(IODisplayModeID id) const;
    bool edidModeFor(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *m);
    unsigned addKmsRefreshModes(unsigned n);
    unsigned addDsrModes(unsigned n);
    unsigned addRotatedDesktops(unsigned n);
    void computeScanoutCaps(NvU32 pitchAlign);
    unsigned fEdidHits = 0;
    static void dtToKapi(const IODetailedTimingInformationV2 *d, struct NvKmsKapiDisplayMode *m);
    IOReturn switchMode(IODisplayModeID id, const struct NvKmsKapiDisplayMode &m, NvU32 w, NvU32 h, UInt32 transform);
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
    // The connect interrupt came due before the framework had registered its handler; registerForInterruptType fires
    // it as soon as the handler arrives (see connectNotifyFire).
    volatile SInt32 fConnectOwed = 0;
    bool fEdidSwapped = false;
    void adoptSwappedDisplay();
    void takeEdid(const NvU8 *src, NvU32 size);
    void clearDisplayAudioProperties();
    void publishDisplayAudioProperties();
    void refreshDisplayAudioProperties();
    void publishDisplayAudioPropertiesNow();
    static void audioFire(thread_call_param_t, thread_call_param_t);
    thread_call_t fAudioCall = nullptr;
    IONotifier *fAudioReady = nullptr, *fCodecReady = nullptr;
    static bool audioReady(void *, void *, IOService *, IONotifier *);
    uint64_t fAudioGeneration = 0;
    unsigned fAudioAttempt = 0;
    bool fAudioValid = false;
    uint8_t fAudioCodec[4] = {};
    thread_call_t fConnectNotify = nullptr;
    void notifyConnectionChanged(unsigned delayMs);
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
    uint64_t fEnableAbs = 0;    // when enableController ran, for the first-primary-flip timing line
    bool fFirstPrimaryLogged = false;   // a login screen that never gets one is the black-screen-with-cursor signature
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
    bool fDpyLost = false;   // the connector is disconnected; re-sense after its EDID is restored
    // macOS pushes an all-black output LUT right after a display comes back; only that window is ignored so a
    // deliberate fade or blackout still applies.
    UInt64 fBlackLutDeadline = 0;
    bool fRelightAudioValid = false;   // the reconnect carried a readable EDID, so cached audio may be republished
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
    static void hotplugPollFire(thread_call_param_t, thread_call_param_t);
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
    using IOFramebuffer::setProperty;
    bool setProperty(const OSSymbol *key, OSObject *value) override {
        if (key && key->isEqualTo("IOFBTransform")) {   // kIOFBTransformKey
            OSNumber *n = OSDynamicCast(OSNumber, value);
            const UInt32 v = n ? (UInt32)n->unsigned64BitValue() : 0;
            const UInt32 t = (v & kIOScaleRotateFlags) | ((v & 0xf) << 4);
            if (t != fTransform) { fTransform = t; fNDT = 0; }
            publishRotatedSizes();
        }
        return IOFramebuffer::setProperty(key, value);
    }
    void free() override;
    IOReturn enableController() override;
    const char *getPixelFormats() override { return kPixelFormats; }
    // macOS builds its mode list from getDisplayModes BEFORE it calls setDetailedTimings, and never re-reads it: a
    // mode added only in setDetailedTimings never reached WindowServer (36 added, 0 shown). So the
    // display's native-raster modes at every refresh rate NVKMS validated are offered from the first query on.
    IOItemCount getDisplayModeCount() override {
        if (fProxy) return 1;
        if (!fNDT) {
            unsigned n = 0;
            if (fModeSet && fCurId != kModeId && fCurDT.pixelClock) { fDT[0] = fCurDT; fDT[0].detailedTimingModeID = (UInt32)fCurId; n = 1; }
            fNDT = addKmsRefreshModes(n);
        }
        return (listBootMode() ? 1 : 0) + fNDT; }
    IOReturn getDisplayModes(IODisplayModeID *allDisplayModes) override {
        unsigned k = 0;
        if (fProxy) { allDisplayModes[0] = kModeId; return kIOReturnSuccess; }
        if (listBootMode()) allDisplayModes[k++] = kModeId;
        for (unsigned i = 0; i < fNDT; i++) allDisplayModes[k++] = (IODisplayModeID)fDT[i].detailedTimingModeID;
        return kIOReturnSuccess; }
    bool listBootMode() const {
        if (fBootInDT) return false;
        if (!fTransform || !fBootW) return true;
        for (unsigned i = 0; i < fNDT; i++) if (!(fDT[i].detailedTimingModeID & 0x80000000u) && dtIsBootRaster(fDT[i])) return false;
        return true; }
    IOReturn validateDetailedTiming(void *description, IOByteCount descripSize) override;
    IOReturn setDetailedTimings(OSArray *array) override;
    IOReturn getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info) override;
    IOReturn getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info) override;
    UInt64 getPixelFormatsForDisplayMode(IODisplayModeID, IOIndex) override { return 0; }
    IOReturn getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi) override;
    IOReturn getCurrentDisplayMode(IODisplayModeID *mode, IOIndex *depth) override;
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
#if !defined(NM_ERA) || defined(NM_ERA_HAS_GAMMA_SYNC)   // the five-argument form exists from 10.13
    IOReturn setGammaTable(UInt32 ch, UInt32 n, UInt32 w, void *d, bool) override { return nvrmSetGamma(ch, n, w, d); }
#endif
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
    bool hasDDCConnect(IOIndex) override { return !fDpyLost && !fHidden && fEdidSize >= 128; }
    IOReturn getDDCBlock(IOIndex connectIndex, UInt32 blockNumber, IOSelect blockType, IOOptionBits options, UInt8 *data, IOByteCount *length) override;
    // DDC/CI (boot-arg nvrmfbddc=1): one I2C bus per external display, transfers run by NVRM (nvrm_ddc.cpp).
    IOReturn doI2CRequest(UInt32 bus, struct IOI2CBusTiming *timing, struct IOI2CRequest *request) override;
    bool fDdcOn = false;
    NVRMExternalBrightness *fDdcBrightness = nullptr;
    IOCommandGate *fDdcBrightnessGate = nullptr;
    thread_call_t fDdcBrightnessCall = nullptr;
    thread_call_t fDdcProbeCall = nullptr;
    IORecursiveLock *fDdcSinkLock = nullptr; // native DDC writes and changes to the sink; never taken by flips
    bool fDdcInFlight = false;
    bool fDdcPowered = false;
    UInt32 fDdcSinkGeneration = 1;
    UInt32 fDdcFastReadGeneration = 0;
    UInt64 fDdcPending = 0; // generation in high word, requested VCP value + 1 in low word
    UInt64 fDdcPendingAt = 0; // paired with fDdcPending under fFlipCacheLock
    UInt64 fDdcWriteAfter = 0; // accessed while holding fDdcInFlight
    UInt32 fDdcBusyRetries = 0; // accessed only by the serial brightness callback
    void publishDdcBus();
    void publishDdcBrightness();
    static void ddcBrightnessFire(thread_call_param_t, thread_call_param_t);
    IOReturn writeDdcBrightness();
    static void ddcProbeFire(thread_call_param_t, thread_call_param_t);
    IOReturn ddcProbeGated(void *, void *, void *, void *);
    void scheduleDdcProbe(unsigned delayMs);
    bool readDdcBrightness(UInt32 &value, UInt32 &maximum, UInt32 &generation, unsigned delayMs);
    bool queueDdcBrightness(UInt32 value, UInt32 generation);
    IOReturn ddcTransfer(unsigned op, unsigned addr7, unsigned char *data, unsigned len);
    IOReturn ddcPowerGated(void *on, void *, void *, void *);
    void ddcSetPowered(bool on);
    IOReturn registerForInterruptType(IOSelect type, IOFBInterruptProc proc, OSObject *target, void *ref, void **interruptRef) override;
    IOReturn unregisterInterrupt(void *interruptRef) override { return kIOReturnSuccess; }
    IOReturn setInterruptState(void *interruptRef, UInt32 state) override { return kIOReturnSuccess; }
};
OSDefineMetaClassAndStructors(NVRMNVDAFramebuffer, IOFramebuffer)
struct nvrm_dpy_rec {
    NvKmsKapiDisplay handle; NvKmsKapiConnector connector; NvU32 headMask; NvBool internal;
    NvU32 hdaConnectorType; NvBool mst, connected, vrr; NvU32 edidSize; NvU8 edid[NVKMS_KAPI_EDID_BUFFER_SIZE];
};
static NvU32 nvrmHdaConnectorType(const struct NvKmsKapiConnectorInfo *info)
{
    if (info->signalFormat == NVKMS_CONNECTOR_SIGNAL_FORMAT_DP) return 0x400;
    if (info->signalFormat == NVKMS_CONNECTOR_SIGNAL_FORMAT_TMDS && info->type == NVKMS_CONNECTOR_TYPE_HDMI)
        return 0x800;
    return 0;
}
static NvU32 nvrmQueryHdaConnectorType(struct NvKmsKapiFunctionsTable *kms, struct NvKmsKapiDevice *device,
                                       NvKmsKapiConnector connector)
{
    if (!kms || !kms->getConnectorInfo) return 0;
    struct NvKmsKapiConnectorInfo info = {};
    return kms->getConnectorInfo(device, connector, &info) ? nvrmHdaConnectorType(&info) : 0;
}
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
static unsigned gBootRetry = kNvrmBootRetries;   // boot-time "nothing connected" retries done; kNvrmBootRetries = none left
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

// CoreDisplay of macOS 10.13 and 10.14 decides whether to use IOPresentment (and with it the Metal compositor) by
// looking for a service of class AppleIntelFramebuffer, NVDA or AMDFramebuffer. This marker carries that class name
// beside our framebuffer; it does nothing else. Built only where the install-time option asks for it, because the
// name collides with Apple's own NVIDIA kext if an older NVIDIA card makes that kext load.
#ifdef NVRMFB_PUBLISH_CLASS_NVDA
class NVDA : public IOService
{
    OSDeclareDefaultStructors(NVDA)
public:
    bool start(IOService *provider) APPLE_KEXT_OVERRIDE {
        if (!IOService::start(provider)) return false;
        registerService();
        return true;
    }
};
OSDefineMetaClassAndStructors(NVDA, IOService)
#endif

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
        // Boot-arg nvoffbar=1: NVRM repairs a lost BAR1 and publishes where it put it ("nvrm-bar1-placed"); the firmware drew at
        // VRAM offset 0, so the screen is at the start of that BAR1. This only reads a property: if NVRM has not placed BAR1 yet
        // the probe declines, and NVRM's registerService() after the placement matches this class again.
        {
            UInt32 offbar = 0;
            if (PE_parse_boot_argn("nvoffbar", &offbar, sizeof offbar) && offbar) {
                OSNumber *placed = OSDynamicCast(OSNumber, provider->getProperty("nvrm-bar1-placed"));
                const NvU64 scr = placed ? placed->unsigned64BitValue() : 0;
                for (IOItemCount i = 0; scr && i < provider->getDeviceMemoryCount(); i++) {
                    IOMemoryDescriptor *m = provider->getDeviceMemoryWithIndex(i);
                    if (!m || (NvU64)m->getPhysicalSegment(0, 0, kIOMemoryMapperNone) != scr || m->getLength() < bytes) continue;
                    IOMemoryDescriptor *sub = IOSubMemoryDescriptor::withSubRange(m, 0, (IOByteCount)bytes, kIODirectionNone);
                    if (!sub) return NULL;
                    OSSafeReleaseNULL(fAp);
                    fAp = (IODeviceMemory *)sub; fV = v; fV.v_baseAddr = (unsigned long)scr;
                    PE_Video nv = v; nv.v_baseAddr = (unsigned long)scr;   // the kernel console must draw there too
                    getPlatform()->setConsoleInfo(&nv, kPEBaseAddressChange);
                    IOLog("NVRMFB: driver off - the screen is at the placed BAR1 0x%llx (was 0x%llx), %lux%lu\n", scr, base,
                          (unsigned long)v.v_width, (unsigned long)v.v_height);
                    return this;
                }
                if (!scr) IOLog("NVRMFB: driver off - BAR1 not placed yet (nvoffbar=1); waiting for NVRM to match this card again\n");
                else IOLog("NVRMFB: driver off - BAR1 placed at 0x%llx but no aperture of the card starts there\n", scr);
            }
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
// The output LUT the way nvidia-drm 615 sets a CRTC's gamma: a 1025-entry UNORM16 surface, no VSS segments.
static void nvrmHeadOlut(struct NvKmsKapiHeadRequestedConfig *hr, struct NvKmsKapiSurface *olut)
{
    hr->modeSetConfig.olut.enabled = olut ? NV_TRUE : NV_FALSE;
    hr->modeSetConfig.olut.lutSurface = olut;
    hr->modeSetConfig.olut.offset = 0;
    hr->modeSetConfig.olut.vssSegments = 0;
    hr->modeSetConfig.olut.lutEntries = olut ? NVKMS_LUT_ARRAY_SIZE + 1 : 0;
    hr->flags.olutChanged = NV_TRUE;
}
// NVIDIA 620 added NvKmsKapiHeadModeSetConfig::colorFormat and applies it on EVERY full mode set ("always specify the
// color format" in nvkms-kapi.c). Its zero is RGB, and a specified RGB never falls back: a mode that only fits the link
// as YCbCr (4K at high refresh over HDMI 2.0) would be refused, a black screen. Through 615 the format was never
// specified, so NVKMS tried RGB and fell back. UNKNOWN keeps that exact behaviour (it is what Linux DRM's "auto" maps
// to). Member detection keeps this file building against 615 headers, which have no such field, for baseline builds.
// 3 = NV_KMS_DPY_ATTRIBUTE_REQUESTED_COLOR_FORMAT_UNKNOWN (620 nvkms-api-types.h; 615 has no such constant).
static const NvU32 kNvrmColorFormatAuto = 3;
template <typename C> static auto nvrmAutoColorFormat(C *c, int) -> decltype(c->colorFormat = kNvrmColorFormatAuto, void())
{ c->colorFormat = kNvrmColorFormatAuto; }
template <typename C> static void nvrmAutoColorFormat(C *, long) {}

// Output colour controls. Set from boot-args nvfbbpc (6/8/10/12), nvfbformat (0 RGB, 1 YCbCr 4:2:2, 2 YCbCr 4:4:4),
// nvfbrange (1 full, 2 limited) and nvfbvib (0..200, 100 = off; below is duller, above more vivid), and live through the
// debug.nvrmfb_color_* sysctls. All default to "automatic": NVKMS decides, as before. They apply to every head.
static NvU32 gSurroundMask = 0, gSurroundMaskOrig = 0;   // boot-arg nvfbsurround; the active mask is 0 while the group is degraded
static bool gRotScale = false;                 // boot-arg nvfbrotscale
static bool gHdrOn = false, gTenBit = false;   // boot-args nvfbhdr / nvfb10bit
static int gColorBpc = 0, gColorFormat = (int)kNvrmColorFormatAuto, gColorRange = 0, gVibrance = 0;
// The fallbacks below let this file compile against an NVKMS interface without the fields, and then the colour controls do
// nothing at all: every build did exactly that while the archives came from a tree missing 620-color-range.patch. A build
// of this kext must see the fields; build_rm620.sh makes the tree that has them.
template <typename C> static auto nvrmHasColorFields(int) -> decltype(((C *)0)->colorRange, ((C *)0)->colorFormat, char());
template <typename C> static long nvrmHasColorFields(long);
static_assert(sizeof(nvrmHasColorFields<decltype(((struct NvKmsKapiHeadRequestedConfig *)0)->modeSetConfig)>(0)) == 1,
              "nvkms-kapi.h has no colorRange/colorFormat: build against the tree build_rm620.sh patched");
// The range field exists only in headers carrying 620-color-range.patch; detection keeps baseline builds compiling.
template <typename C> static auto nvrmSetColorFormat(C *c, NvU32 v, int) -> decltype(c->colorFormat = v, void()) { c->colorFormat = v; }
template <typename C> static void nvrmSetColorFormat(C *, NvU32, long) {}
template <typename C> static auto nvrmSetColorRange(C *c, unsigned v, int) -> decltype(c->colorRange = v, void()) { c->colorRange = v; }
template <typename C> static void nvrmSetColorRange(C *, unsigned, long) {}
template <typename F> static auto nvrmFlagColorRange(F *f, int) -> decltype(f->colorRangeChanged = NV_TRUE, void()) { f->colorRangeChanged = NV_TRUE; }
template <typename F> static void nvrmFlagColorRange(F *, long) {}
static void nvrmApplyColorPrefs(struct NvKmsKapiHeadRequestedConfig *hr)
{
    if ((NvU32)gColorFormat != kNvrmColorFormatAuto) nvrmSetColorFormat(&hr->modeSetConfig, (NvU32)gColorFormat, 0);
    if (gColorBpc) hr->modeSetConfig.colorBpc = (NvU32)gColorBpc;
    if (gColorRange) nvrmSetColorRange(&hr->modeSetConfig, (unsigned)gColorRange, 0);
}
static void nvrmHeadConfigBaseline(struct NvKmsKapiHeadRequestedConfig *hr, struct NvKmsKapiSurface *olut = NULL)
{
    nvrmAutoColorFormat(&hr->modeSetConfig, 0);   // colorBpc 0 = UNKNOWN and allmMode 0 = DISABLED already match 615
    hr->modeSetConfig.olutFpNormScale = NVKMS_OLUT_FP_NORM_SCALE_DEFAULT;
    hr->flags.olutFpNormScaleChanged = NV_TRUE;
    if (olut) nvrmHeadOlut(hr, olut);
    for (unsigned i = 0; i < NVKMS_KAPI_LAYER_MAX; i++) {
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[i];
        nvu_zero(&lr->config.csc, sizeof lr->config.csc);
        lr->config.csc.m[0][0] = 0x10000;
        lr->config.csc.m[1][1] = 0x10000;
        lr->config.csc.m[2][2] = 0x10000;
        if (gVibrance && i == NVKMS_KAPI_LAYER_PRIMARY_IDX) {
            int sm[3][3]; nvrmSaturationMatrix(gVibrance, sm);
            for (int r = 0; r < 3; r++) for (int c = 0; c < 3; c++) lr->config.csc.m[r][c] = sm[r][c];
        }
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
            if (!nvrmDynamicDisplayInfo(fKms, fDev, dd)) {
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
            r->connected = dd->connected ? NV_TRUE : NV_FALSE; r->mst = si.isDpMST; r->vrr = dd->vrrSupported;
            r->handle = handles[i]; r->connector = si.connectorHandle; r->headMask = si.headMask; r->internal = si.internal;
            r->hdaConnectorType = si.internal ? 0 : nvrmQueryHdaConnectorType(fKms, fDev, si.connectorHandle);
            r->edidSize = dd->edid.bufferSize > sizeof r->edid ? (NvU32)sizeof r->edid : dd->edid.bufferSize;
            nvu_copy(r->edid, dd->edid.buffer, r->edidSize);
        }
        IOFree(dd, sizeof *dd);
        if (getProvider()) getProvider()->setProperty("connected-displays", (unsigned long long)gNDpy, 32);
        FBLOG("ENUMERATED: %d connected display(s) of %u handle(s), %u heads available",
              gNDpy, n, fNumHeads);
        if (gNDpy == 0 && n > 0) {   // nothing answered yet: look again shortly (nvfbbootretry=0 turns this off)
            UInt32 off = 1; PE_parse_boot_argn("nvfbbootretry", &off, sizeof off);
            gBootRetry = off ? 0 : kNvrmBootRetries;    // the counter counts up; kNvrmBootRetries = already used up
            if (off) FBLOG("no display answered at start: will look again %u times, %u s apart", (unsigned)kNvrmBootRetries, nvrmBootRetryDelayMs(0) / 1000);
            if (off) hotplugKick();
        }
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
    if (gNDpy <= 0 && fIndex == 0 && proxyFind()) {
        fProxy = true; fCurId = kModeId; fBootConsole = true;
        FBLOG("fb0: no NVIDIA display; PROXY for the integrated GPU's panel %ux%u pitch %u: the NVIDIA GPU renders and "
              "frames are copied to the panel's framebuffer", fPxW, fPxH, fPxPitch);
        return true;
    }
    if (gNDpy <= 0 || fIndex >= (unsigned)gNDpy) {
        FBLOG("index %u: only %d display(s) connected -- nothing to drive, not starting", fIndex, gNDpy);
        fSpareIndex = gNDpy > 0;
        return false;
    }
    {   // Surround (nvfbsurround): read the mask once. Members start as ordinary framebuffers, hidden from WindowServer (fHidden).
        static bool maskRead; if (!maskRead) { maskRead = true; UInt32 v = 0; if (PE_parse_boot_argn("nvfbsurround", &v, sizeof v)) gSurroundMask = gSurroundMaskOrig = v & 0xff; }
    }
    {
        struct nvrm_dpy_rec *r = &gDpy[fIndex];
        fDisplay = r->handle; fConnector = r->connector; fInternal = r->internal; fVrrSupported = r->vrr && vrrAllowed();
        if (fInternal) FBLOG("fb%u: display 0x%x is internal -- reporting kIOConnectionBuiltIn", fIndex, fDisplay);
        takeEdid(r->edid, r->edidSize);
        FBLOG("fb%u: EDID %u B, panel %ux%u mm (imageWidth/imageHeight)", fIndex, (unsigned)fEdidSize,
              (unsigned)fEdidMmW, (unsigned)fEdidMmH);
        publishDisplayAudioProperties();
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
        fHidden = nvrmSurroundIsMember(gSurroundMask, fIndex, (unsigned)gNDpy);
        if (fHidden) FBLOG("fb%u: member of the surround group led by index %u -- hidden from WindowServer, its head serves the group", fIndex, nvrmSurroundMaster(gSurroundMask, (unsigned)gNDpy));
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
        if (!nvrmKmsRasterValid(m.timings.hVisible, m.timings.vVisible)) {
            FBLOG("skip mode %u with raster %ux%u outside NVKMS layer range", i,
                  m.timings.hVisible, m.timings.vVisible);
            continue;
        }
        if (preferred) { fMode = m; have = true; break; }
        const NvU64 area = (NvU64)m.timings.hVisible * m.timings.vVisible;
        if (!have || area > bestArea || (area == bestArea && m.timings.pixelClockHz > bestClock)) {
            fMode = m; bestArea = area; bestClock = m.timings.pixelClockHz; have = true;
        }
    }
    if (!have) { FBLOG("no valid mode"); return false; }
    if (gBootHead < 0 && fIndex == 0) fBootConsole = true;
    fKmsMode = fMode; fHaveKmsMode = true;
    if (gBootHead >= 0 && fDisplay == gBootDpy && (int)fHead == gBootHead &&
        nvrmKmsRasterValid(gBootMode.timings.hVisible, gBootMode.timings.vVisible)) {
        // the fallback for a refused firmware timing must be the same raster (it scans out the same surface): the
        // validated mode at the boot size whose refresh is closest to the firmware's
        bool same = false; NvU32 bestGap = 0;
        for (NvU32 i = 0; i < 256; i++) {
            struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, preferred = NV_FALSE;
            int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &preferred);
            if (r < 0) break;
            if (r == 0 || !valid || !nvrmKmsRasterValid(m.timings.hVisible, m.timings.vVisible) ||
                m.timings.hVisible != gBootMode.timings.hVisible ||
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
    } else if (gBootHead >= 0 && fDisplay == gBootDpy && (int)fHead == gBootHead) {
        fBootConsole = true;
        FBLOG("firmware boot raster %ux%u is outside the NVKMS layer range; using selected mode %ux%u",
              gBootMode.timings.hVisible, gBootMode.timings.vVisible,
              fMode.timings.hVisible, fMode.timings.vVisible);
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
        // the floor is extra: when it cannot be had, the size the mode list asks for is tried before the boot size
        if (!fMem && fModesAllocSize && fModesAllocSize < mp.size) {
            FBLOG("%s: allocateMemory(%llu) with the reserve floor failed -- trying the mode list's %llu", what,
                  (unsigned long long)mp.size, (unsigned long long)fModesAllocSize);
            fAllocSize = fModesAllocSize; mp.size = fModesAllocSize;
            fMem = fKms->allocateMemory(fDev, &mp);
        }
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
        FBLOG("%s: kva %s contiguous %u kernelPhys <phys> userPhys <phys> %s",
              what, fKva ? "set" : "null", (unsigned)contig,
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
    publishStartupTiming();
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = fPitch;
    sp.width = fW; sp.height = fH; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
    fScan30 = false; fHdrActive = false;   // the first scanout surface is 8-bit; a 30-bit one comes with a mode change
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
    FBLOG("console probe phys <phys> pitch %u size %llu kva %s | SURFACE phys <phys> pitch %u size %llu",
          fConsPitch, fConsSize, fConsKva ? "set" : "null", fPitch, fSize);
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
// The parent of the display nubs, remembered when a framebuffer first starts; see nvrmPickNubParent.
static IOService *gNubParent = nullptr;
// Tell the framework to re-sense this connection after its state changes. The per-framebuffer call uses the registered
// kIOFBConnectInterruptType handler; scheduleCall supplies its queued-work retain and stop-time cancellation.
void NVRMNVDAFramebuffer::notifyConnectionChanged(unsigned delayMs)
{
    if (fConnectNotify) {
        uint64_t deadline; clock_interval_to_deadline(delayMs, kMillisecondScale, &deadline);
        scheduleCall(fConnectNotify, deadline);
    }
}
void NVRMNVDAFramebuffer::connectNotifyFire(thread_call_param_t p0, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = (NVRMNVDAFramebuffer *)p0;
    if (!me) return;
    NVRMCallReference callReference(me);
    if (OSAddAtomic(0, &me->fStopping)) return;
    if (!me->fConnect.proc) {
        // IOFramebuffer::open registers the connect handler after enableController, and on a display plugged in after
        // boot the open can take longer than the 1.5 s delay. Dropping the notification here left such a display dark
        // until a restart: nothing fired it again. Owe it instead; registerForInterruptType pays it.
        OSCompareAndSwap(0, 1, &me->fConnectOwed);
        FBLOG("fb%u connect notify: nothing registered yet -- it fires when the framework registers its connect handler", me->fIndex);
        return;
    }
    OSCompareAndSwap(1, 0, &me->fConnectOwed);
    me->fConnectFired = true;
    FBLOG("fb%u -> connect interrupt (the framework's registered handler)", me->fIndex);
    me->fConnect.proc(me->fConnect.target, me->fConnect.ref);
}
// A different monitor on this framebuffer's connector: take its EDID (what getDDCBlock hands macOS), its physical size,
// its display-audio ELD, and the scaler/raster ceilings its own mode list allows. The scanout allocation made at start
// is kept: it backs the surface WindowServer is drawing into, and it cannot be replaced under it. A new monitor whose
// largest raster needs more than that allocation is offered what fits (the mode check refuses the rest as larger than
// the scanout allocation) and gets its full size at the next start; the log says so.
// The five AppleHDA association properties extend the approach in PR #284 by Hi-Jiajun.
static OSObject *copyGpuHdaGfx(IOService *provider)
{
    IORegistryEntry *entry = provider;
    while (entry) {
        if (entry->metaCast("IOPCIDevice")) {
            OSObject *value = entry->copyProperty("hda-gfx");
            if (!OSDynamicCast(OSData, value) && !OSDynamicCast(OSString, value)) OSSafeReleaseNULL(value);
            return value;
        }
        entry = entry->getParentEntry(gIOServicePlane);
    }
    return nullptr;
}

void NVRMNVDAFramebuffer::clearDisplayAudioProperties()
{
    NVRMDisplayGate gate;
    ++fAudioGeneration;
    fAudioValid = false;
    removeProperty("nvda-eld");
    removeProperty("hda-gfx");
    removeProperty("connector-type");
    removeProperty("port-number");
    removeProperty("av-signal-type");
    removeProperty("audio-codec-info");
    removeProperty("IODisplayEDID");
}

#include "nvrm-audio-auto.inc"

void NVRMNVDAFramebuffer::publishDisplayAudioPropertiesNow()
{
    if (!fAudioValid) { removeProperty("audio-codec-info"); return; }
    if (fInternal || fEdidSize < 128 || fIndex >= (unsigned)gNDpy) return;

    const NvU32 connectorType = gDpy[fIndex].hdaConnectorType;
    const int eldConnector = connectorType == 0x800 ? NVRM_ELD_CONN_HDMI : NVRM_ELD_CONN_DP;
    uint8_t eld[NVRM_ELD_MAX];
    size_t eldn = nvrm_build_eld(fEdid, fEdidSize, eldConnector, eld);
    if (!eldn) return;
    if (OSData *data = OSData::withBytes(eld, (unsigned)eldn)) {
        setProperty("nvda-eld", data);
        data->release();
    }

    OSObject *pairing = copyGpuHdaGfx(getProvider());
    const bool hasPairing = pairing != nullptr;
    if (pairing) { setProperty("hda-gfx", pairing); pairing->release(); }
    else setProperty("hda-gfx", "onboard-1");

    OSObject *codecInfoObject = OSData::withBytes(fAudioCodec, sizeof fAudioCodec);
    OSData *codecInfo = OSDynamicCast(OSData, codecInfoObject);
    if (!codecInfo) return;

    struct nvrm_cea_scan cea;
    nvrm_scan_cea(fEdid, fEdidSize, &cea);
    if (!cea.has_audio || !connectorType || !hasPairing) {
        OSSafeReleaseNULL(codecInfoObject);
        return;
    }

    NvU8 connector[4] = { (NvU8)connectorType, (NvU8)(connectorType >> 8),
                          (NvU8)(connectorType >> 16), (NvU8)(connectorType >> 24) };
    unsigned external = 0;
    for (int i = 0; i < gNDpy; ++i)
        if (gDpy[i].connected && !gDpy[i].internal) ++external;
    NvU8 port[4] = {};
    if (!nvrm_audio_port_number(fAudioCodec[2], external, &port[0])) {
        OSSafeReleaseNULL(codecInfoObject);
        return;
    }
    NvU8 signal[4] = { 8, 0, 0, 0 };
    bool published = setProperty("connector-type", connector, sizeof connector);
    if (published) published = setProperty("port-number", port, sizeof port);
    if (published) published = setProperty("av-signal-type", signal, sizeof signal);
    if (published) published = setProperty("audio-codec-info", codecInfo);
    if (published) published = setProperty("IODisplayEDID", fEdid, fEdidSize);
    OSSafeReleaseNULL(codecInfoObject);
    if (!published) {
        clearDisplayAudioProperties();
        FBLOG("fb%u: failed to publish display-audio association", fIndex);
    }
}

static bool nvrmEdidSerialString(const NvU8 *edid, char *out, unsigned outSize)
{
    for (unsigned k = 0; k < 4; k++) {
        const NvU8 *d = edid + 54 + 18 * k;
        if (d[0] || d[1] || d[2] || d[3] != 0xff) continue;
        unsigned len = 13;
        while (len && (d[5 + len - 1] == ' ' || d[5 + len - 1] == 0x0a || d[5 + len - 1] == 0)) len--;
        if (!len || len + 1 > outSize) return false;
        nvu_copy(out, d + 5, len); out[len] = 0;
        return true;
    }
    return false;
}
void NVRMNVDAFramebuffer::takeEdid(const NvU8 *src, NvU32 size)
{
    fEdidSize = (NvU16)(size > sizeof fEdid ? sizeof fEdid : size);
    nvu_copy(fEdid, src, fEdidSize);
    const bool fastBrightness = fIndex < (unsigned)gNDpy &&
        nvrmDdcFastBrightnessRead(fEdid, fEdidSize, gDpy[fIndex].hdaConnectorType);
    const UInt32 generation = fastBrightness ? __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED) : 0;
    __atomic_store_n(&fDdcFastReadGeneration, generation, __ATOMIC_RELEASE);
    nvrmEdidPanelSizeMm(fEdid, fEdidSize, &fEdidMmW, &fEdidMmH);
    if (!nvrmEdidVerticalRange(fEdid, fEdidSize, &fVrrMinHz, &fVrrMaxHz)) fVrrMinHz = fVrrMaxHz = 0;
    fHdrCapable = nvrmEdidHdrPq(fEdid, fEdidSize, &fHdrMaxLumCode);
    if (fEdidSize < 128) return;
    bool twin = false;
    for (int k = 0; k < gNDpy && !twin; k++) {
        const struct nvrm_dpy_rec *o = &gDpy[k];
        if (o->handle == fDisplay || o->edidSize < 128) continue;
        twin = memcmp(o->edid + 8, fEdid + 8, 8) == 0;
    }
    if (!twin) return;
    char str[14];
    const NvU32 was = (NvU32)fEdid[12] | ((NvU32)fEdid[13] << 8) | ((NvU32)fEdid[14] << 16) | ((NvU32)fEdid[15] << 24);
    if (!nvrmEdidSerialString(fEdid, str, sizeof str)) {
        FBLOG("fb%u: another display has this EDID identity (serial number %u) and no serial-number string tells them apart", fIndex, was);
        return;
    }
    NvU32 h = 2166136261u;
    for (const char *c = str; *c; c++) { h ^= (NvU8)*c; h *= 16777619u; }
    h &= 0x7fffffffu;
    if (!h) h = 1;
    fEdid[12] = (NvU8)h; fEdid[13] = (NvU8)(h >> 8); fEdid[14] = (NvU8)(h >> 16); fEdid[15] = (NvU8)(h >> 24);
    NvU8 sum = 0;
    for (unsigned i = 0; i < 127; i++) sum = (NvU8)(sum + fEdid[i]);
    fEdid[127] = (NvU8)(0u - sum);
    FBLOG("fb%u: EDID serial number %u is shared with another display; serial string %s gives %u", fIndex, was, str, h);
}
void NVRMNVDAFramebuffer::adoptSwappedDisplay()
{
    if (fIndex >= (unsigned)gNDpy) return;
    const struct nvrm_dpy_rec *r = &gDpy[fIndex];
    takeEdid(r->edid, r->edidSize);
    publishDisplayAudioProperties();
    const NvU64 keepAlloc = fAllocSize;
    const NvU32 oldW = fScalerMaxW, oldH = fScalerMaxH;
    computeScanoutCaps(fPitchAlign);
    // the new monitor keeps its own ceilings when its largest desktop fits in the memory this start really allocated
    const bool short_ = !nvrmfb_scanout_fits(fScalerMaxW, fScalerMaxH, fPitchAlign, fMemBytes);
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
// Boot-arg nvfbhotplugpoll=<seconds> (1..60): look for added and removed displays on a timer as well. Some laptops (a GTX 1660 Ti
// with its mini-DP and HDMI wired to the GPU, issue 327) never deliver the display-change event while the GPU sleeps, so a
// plug or unplug at run time is not noticed at all. The poll is the same scan the event runs (hotplugFire), so a quiet bus costs
// one display query per period and changes nothing.
static thread_call_t gHotplugPoll = nullptr;
static uint32_t gHotplugPollSec = 0;
void NVRMNVDAFramebuffer::hotplugPollFire(thread_call_param_t, thread_call_param_t)
{
    hotplugKick();
    uint64_t d; clock_interval_to_deadline(gHotplugPollSec, kSecondScale, &d);
    if (gHotplugPoll) thread_call_enter_delayed(gHotplugPoll, d);
}
// A member display went away: every display is its own again. All members are back: the group re-forms. Runs on the hot-plug thread.
void NVRMNVDAFramebuffer::surroundCheck()
{
    if (!gSurroundMaskOrig) return;
    unsigned idx[kNvrmSurMaxMembers]; const unsigned total = nvrmSurroundMembers(gSurroundMaskOrig, (unsigned)gNDpy, idx);
    if (total < 2) return;
    unsigned up = 0;
    for (unsigned k = 0; k < total; k++) if (gDpy[idx[k]].connected && gFBs[idx[k]] && !gFBs[idx[k]]->fDpyLost) up++;
    NVRMNVDAFramebuffer *master = gFBs[idx[0]];
    if (!master) return;
    const bool formed = gSurroundMask != 0, degraded = !formed;
    const NvrmSurAction act = nvrmSurroundNext(formed, degraded, total, up);
    if (act == kSurNone) return;
    if (act == kSurDegrade) {
        FBLOG("surround: %u of %u members connected -- the displays become separate again", up, total);
        gSurroundMask = 0;
        master->fSurActive = false; master->fSurN = 0;
        for (unsigned k = 1; k < total; k++) if (NVRMNVDAFramebuffer *fb = gFBs[idx[k]]) {
            { NVRMDisplayGate sink(fb->fDdcSinkLock); fb->fHidden = false; }
            fb->notifyConnectionChanged(500);
        }
        master->notifyConnectionChanged(500);
    } else {
        FBLOG("surround: all %u members are back -- the group re-forms", total);
        gSurroundMask = gSurroundMaskOrig;
        master->fSurActive = false; master->fSurN = 0;   // re-planned when WindowServer reads the mode list again
        for (unsigned k = 1; k < total; k++) if (NVRMNVDAFramebuffer *fb = gFBs[idx[k]]) {
            { NVRMDisplayGate sink(fb->fDdcSinkLock); fb->fHidden = true; }
            fb->notifyConnectionChanged(500);
        }
        master->notifyConnectionChanged(500);
    }
}
void NVRMNVDAFramebuffer::hotplugFire(thread_call_param_t, thread_call_param_t)
{
    NVRMNVDAFramebuffer *me = gFB;
    if (!me || !me->fKms || !me->fDev || gNDpy < 0) return;
    NvU32 n = 0; NvKmsKapiDisplay handles[NVKMS_KAPI_MAX_CONNECTORS * 2];
    if (!me->fKms->getDisplays(me->fDev, &n, NULL)) return;
    if (n > sizeof handles / sizeof handles[0]) n = sizeof handles / sizeof handles[0];
    if (n && !me->fKms->getDisplays(me->fDev, &n, handles)) return;
    struct NvKmsKapiDynamicDisplayParams *dd = (struct NvKmsKapiDynamicDisplayParams *)IOMalloc(sizeof *dd);
    if (!dd) return;
    int added = 0;
    NVRMNVDAFramebuffer *relight[16]; unsigned nrelight = 0;
    NVRMNVDAFramebuffer *notify[16]; unsigned nnotify = 0;
    IORecursiveLock *sl = startLock();
    if (sl) IORecursiveLockLock(sl);
    for (NvU32 i = 0; i < n; i++) {
        bool known = false; int kdpy = -1;
        for (int k = 0; k < gNDpy; k++) if (gDpy[k].handle == handles[i]) { known = true; kdpy = k; break; }
        nvu_zero(dd, sizeof *dd); dd->handle = handles[i];
        bool dynOk = nvrmDynamicDisplayInfo(me->fKms, me->fDev, dd);
        if (known) {
            // 1401/driver issue #110: switching the monitor to another input and back left it with no picture. The link
            // drops, the head stops scanning out, and this path skipped every display it already knew. A known display
            // that was seen disconnected and is connected again gets its head lit with the same full mode set the
            // display-wake path uses (reflip). Only on that transition, so other display events cause no modeset.
            if (!dynOk) continue;
            gDpy[kdpy].connected = dd->connected ? NV_TRUE : NV_FALSE;
            bool audioTypeChanged = false;
            if (dd->connected && kdpy >= 0) {
                NvU32 connectorType = gDpy[kdpy].internal ? 0 :
                    nvrmQueryHdaConnectorType(me->fKms, me->fDev, gDpy[kdpy].connector);
                audioTypeChanged = connectorType != gDpy[kdpy].hdaConnectorType;
                gDpy[kdpy].hdaConnectorType = connectorType;
            }
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
                if (!dd->connected) {
                    if (!fb->fDpyLost) {
                        FBLOG("hot-plug: display 0x%x went away (fb %u)", handles[i], fb->fIndex);
                        {
                            NVRMDisplayGate sink(fb->fDdcSinkLock);
                            fb->fDpyLost = true;
                            __atomic_add_fetch(&fb->fDdcSinkGeneration, 1u, __ATOMIC_RELAXED);
                        }
                        fb->fEdidSize = 0;
                        fb->clearDisplayAudioProperties();
                        if (nnotify < sizeof notify / sizeof notify[0]) { fb->retain(); notify[nnotify++] = fb; }
                    }
                } else if (fb->fDpyLost || swapped) {
                    if (fb->fDpyLost && kdpy >= 0) {
                        const struct nvrm_dpy_rec *r = &gDpy[kdpy];
                        fb->takeEdid(r->edid, r->edidSize);
                        { NVRMDisplayGate sink(fb->fDdcSinkLock); fb->fDpyLost = false; }
                        fb->fRelightAudioValid = dd->edid.bufferSize >= 128;
                        fb->fBlackLutDeadline = 0;
                        clock_interval_to_deadline(15000, kMillisecondScale, &fb->fBlackLutDeadline);
                    }
                    if (swapped) {
                        NVRMDisplayGate sink(fb->fDdcSinkLock);
                        fb->fEdidSwapped = true;
                        __atomic_add_fetch(&fb->fDdcSinkGeneration, 1u, __ATOMIC_RELAXED);
                    }
                    if (nrelight < sizeof relight / sizeof relight[0]) {
                        fb->retain(); relight[nrelight++] = fb;
                    }
                } else if (audioTypeChanged) fb->publishDisplayAudioProperties();
            }
            continue;
        }
        if (!dynOk || !dd->connected) continue;
        struct NvKmsKapiStaticDisplayInfo si = {};
        if (!me->fKms->getStaticDisplayInfo(me->fDev, handles[i], &si)) continue;
        if (gNDpy >= (int)(sizeof gDpy / sizeof gDpy[0])) break;
        struct nvrm_dpy_rec *r = &gDpy[gNDpy];
        r->connected = dd->connected ? NV_TRUE : NV_FALSE; r->mst = si.isDpMST; r->vrr = dd->vrrSupported;
            r->handle = handles[i]; r->connector = si.connectorHandle; r->headMask = si.headMask; r->internal = si.internal;
        r->hdaConnectorType = si.internal ? 0 : nvrmQueryHdaConnectorType(me->fKms, me->fDev, si.connectorHandle);
        r->edidSize = dd->edid.bufferSize > sizeof r->edid ? (NvU32)sizeof r->edid : dd->edid.bufferSize;
        nvu_copy(r->edid, dd->edid.buffer, r->edidSize);
        FBLOG("hot-plug: display 0x%x connector 0x%x headMask 0x%x connected after boot -> index %d",
              handles[i], (unsigned)si.connectorHandle, (unsigned)si.headMask, gNDpy);
        gNDpy++; added++;
    }
    for (int k = 0; k < gNDpy; k++) {
        bool listed = false;
        for (NvU32 i = 0; i < n; i++) if (gDpy[k].handle == handles[i]) { listed = true; break; }
        if (listed) continue;
        nvu_zero(dd, sizeof *dd); dd->handle = gDpy[k].handle;
        if (!me->fKms->getDynamicDisplayInfo(me->fDev, dd) || dd->connected) continue;
        gDpy[k].connected = false;
        for (unsigned f = 0; f < sizeof gFBs / sizeof gFBs[0]; f++) {
            NVRMNVDAFramebuffer *fb = gFBs[f];
            if (!fb || fb->fDisplay != gDpy[k].handle || fb->fDpyLost) continue;
            FBLOG("hot-plug: known display 0x%x omitted from NVKMS list and confirmed disconnected (fb %u)",
                  gDpy[k].handle, fb->fIndex);
            {
                NVRMDisplayGate sink(fb->fDdcSinkLock);
                fb->fDpyLost = true;
                __atomic_add_fetch(&fb->fDdcSinkGeneration, 1u, __ATOMIC_RELAXED);
            }
            fb->fEdidSize = 0;
            fb->clearDisplayAudioProperties();
            if (nnotify < sizeof notify / sizeof notify[0]) { fb->retain(); notify[nnotify++] = fb; }
        }
    }
    for (unsigned i = 0; i < sizeof gFBs / sizeof gFBs[0]; ++i)
        if (gFBs[i] && !gFBs[i]->fDpyLost) gFBs[i]->publishDisplayAudioProperties();
    if (sl) IORecursiveLockUnlock(sl);
    IOFree(dd, sizeof *dd);
    for (unsigned f = 0; f < nnotify; f++) {
        notify[f]->notifyConnectionChanged(500);
        notify[f]->release();
    }
    for (unsigned f = 0; f < nrelight; f++) {
        NVRMNVDAFramebuffer *fb = relight[f];
        bool ok = !OSAddAtomic(0, &fb->fStopping) && !fb->fHeadSlept && fb->reflip();
        FBLOG("hot-plug: display 0x%x is back (fb %u): head %u lit again -> ok %u rc %d",
              fb->fDisplay, fb->fIndex, fb->fHead, (unsigned)ok, fb->fReflipRc);
        if (!OSAddAtomic(0, &fb->fStopping) && !fb->fDpyLost) {
            const bool swapped = fb->fEdidSwapped;
            if (swapped) {
                fb->adoptSwappedDisplay();
                NVRMDisplayGate sink(fb->fDdcSinkLock);
                fb->fEdidSwapped = false;
            }
            else if (fb->fRelightAudioValid) fb->publishDisplayAudioProperties();
            else {
                fb->clearDisplayAudioProperties();
                FBLOG("fb%u: reconnect has no readable EDID -- display-audio properties stay cleared", fb->fIndex);
            }
            fb->fRelightAudioValid = false;
            fb->notifyConnectionChanged(500);
        }
        fb->release();
    }
    surroundCheck();
    if (!added) {   // still nothing: a boot that found no display keeps looking, 3 s apart, a few times
        if (gNDpy == 0 && gBootRetry < kNvrmBootRetries) {
            const unsigned ms = nvrmBootRetryDelayMs(gBootRetry++);
            if (ms && gHotplug) { uint64_t d; clock_interval_to_deadline(ms, kMillisecondScale, &d); thread_call_enter_delayed(gHotplug, d); }
        }
        return;
    }
    IOService *nub0 = me->getProvider(), *gpu = nvrmPickNubParent<IOService>(nub0 ? nub0->getProvider() : nullptr, gNubParent);
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
// The trace was published as the registry property "NVRMTrace" on every log line. Registry properties
// carry no privilege, so `ioreg` handed any process the whole 32 KB - including the pointers logged in
// the boot path: the VBL IOFBInterruptProc (a kext text address, i.e. the KASLR slide), NVKMS heap
// pointers, and the kernel virtual and physical addresses of the console and VRAM mappings. Measured
// 2026-10-18 as an ordinary user: 440 lines, 2 text pointers, 3 kernel VAs, 6 physical addresses.
// Publishing is now opt-in, and the %p specifiers below are gone in their own right.
static int nvfbTracePublish(void)
{
    static int on = -1;
    if (on < 0) { uint32_t g = 0; on = PE_parse_boot_argn("nvfbtrace", &g, sizeof g) && g; }
    return on;
}
void NVRMNVDAFramebuffer::trace(const char *s)
{
    if (gTraceFull || gTraceN >= (SInt32)kTraceMax) return;
    size_t n = strlen(s);
    SInt32 off = OSAddAtomic((SInt32)(n + 1), &gTraceLen);
    if (off < 0 || (size_t)off + n + 2 >= sizeof gTrace) { gTraceFull = 1; return; }
    const size_t offset = (size_t)off;
    memcpy(gTrace + offset, s, n); gTrace[offset + n] = '\n';
    OSIncrementAtomic(&gTraceN);
    NVRMNVDAFramebuffer *me = gFB;
    if (!me || !nvfbTracePublish()) return;
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
// 1 = at 357 Hz and above a latch setting of 1 is run as 2 (see nvrmFlipLatchMode); 0 keeps the synchronous wait at any rate.
static int gFlipLatchAuto = 1;
static int gFlipLatchWaits = 0, gFlipLatchTimeouts = 0, gFlipLatchMaxUs = 0, gFlipLatchLate = 0;
// gFlipLatch == 2 (opt in): priorFlipState() at the head of the next submit verifies the flip instead of this submit
// waiting for it. The wait costs half a refresh period on the present path (2.08 ms of a 4.17 ms 240 Hz frame, #300).
static int gFlipLatchDeferred = 0;
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
    {   // a full mode set carries the variable-refresh state, so a mode change does not drop it
        struct NvKmsKapiHeadRequestedConfig *vh = &cfg->headRequestedConfig[fHead];
        const bool full = vh->modeSetConfig.bActive && (vh->flags.modeChanged || vh->flags.activeChanged || vh->flags.colorBpcChanged ||
                                                        vh->flags.colorFormatChanged);
        if (full) nvrmApplyColorPrefs(vh);
        if (gHdrOn && fHdrCapable && vh->modeSetConfig.bActive) {
            // HDR10: PQ in BT.2100 colorimetry, 10 bits, the composited pixels passed through untouched. Every config for the
            // head says so (a flip that left it out would fall back to SDR); a full mode set also flags the change, and also
            // when it turns HDR off.
            struct NvKmsKapiHeadModeSetConfig &mc = vh->modeSetConfig;
            if (fHdrActive) {
                mc.hdrInfoFrame.enabled = NV_TRUE; mc.hdrInfoFrame.eotf = NVKMS_INFOFRAME_EOTF_ST2084;
                mc.hdrInfoFrame.staticMetadata = fHdrMeta;
                mc.colorimetry = NVKMS_OUTPUT_COLORIMETRY_BT2100;
                mc.postcompColorPassthrough = NV_TRUE;
                if (!gColorBpc && mc.colorBpc == 0) mc.colorBpc = 10;
                mc.olut.enabled = NV_FALSE; mc.olut.lutSurface = NULL; mc.olut.lutEntries = 0;   // no gamma ramp on PQ content
            }
            if (full) {
                vh->flags.hdrInfoFrameChanged = vh->flags.colorimetryChanged = vh->flags.postcompColorPassthroughChanged = NV_TRUE;
                vh->flags.olutChanged = NV_TRUE;
            }
        }
        // NVKMS lets a flip float with the display's refresh only when the config it is given still says VRR is on, so every
        // config for an active head carries it; the change flag goes with full mode sets.
        if (fVrrOn && fVrrSupported && vh->modeSetConfig.bActive) {
            vh->modeSetConfig.vrrEnabled = NV_TRUE;
            if (vh->flags.modeChanged || vh->flags.activeChanged) vh->flags.vrrEnabledChanged = NV_TRUE;
        }
    }
    surroundReplicate(cfg);
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
        // Until this head has shown one primary flip, every rejected primary is logged with the surface it carried:
        // the login window's first frame being refused is the one case the counters above cannot tell apart.
        if (!fFirstPrimaryLogged && tracked) {
            const struct NvKmsKapiLayerConfig *pc =
                &cfg->headRequestedConfig[fHead].layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].config;
            FBLOG("primary rejected before the first shown flip on head %u: result %d src %ux%u+%u+%u dst %ux%u%+d%+d",
                  fHead, (int)rep->flipResult, (unsigned)pc->srcWidth, (unsigned)pc->srcHeight, (unsigned)pc->srcX,
                  (unsigned)pc->srcY, (unsigned)pc->dstWidth, (unsigned)pc->dstHeight, (int)pc->dstX,
                  (int)pc->dstY);
        }
        setProperty("NVRMRejectedCommits", (unsigned long long)fRejected, 32);
        return NV_FALSE;
    }
    if (cursorOnly) return NV_TRUE;
    if (!tracked) { fSubmissionSeen = false; return NV_TRUE; }
    if (!fFirstPrimaryLogged) {
        fFirstPrimaryLogged = true;
        uint64_t ns = 0;
        if (fEnableAbs) absolutetime_to_nanoseconds(submit0 - fEnableAbs, &ns);
        FBLOG("first accepted primary flip on head %u, %llu ms after enableController (%u rejected before it)%s",
              fHead, (unsigned long long)(ns / 1000000), fRejected, bootstrap ? " [bootstrap]" : "");
        setProperty("NVRMFirstPrimaryFlipMs", (unsigned long long)(ns / 1000000), 64);
    }
    fSubmissionSeen = true;
    fFlipSubmitAbs = submit0;
    // Initial acceptance permits console handoff, never surface retirement. The owned boot surface remains pinned.
    if (bootstrap || !gFlipLatch) return NV_TRUE;
    if (nvrmFlipLatchMode(gFlipLatch, gFlipLatchAuto, flipWaitUs() * 800ull) == 2) { OSIncrementAtomic((volatile SInt32 *)&gFlipLatchDeferred); return NV_TRUE; }
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
    nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
    hr->modeSetConfig.bActive = NV_TRUE;
    hr->modeSetConfig.numDisplays = 1;
    hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = front;
    lr->config.srcWidth = (NvU16)fW; lr->config.srcHeight = (NvU16)fH;
    lr->config.dstWidth = (NvU16)fRasterW; lr->config.dstHeight = (NvU16)fRasterH;
    lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE;
    lr->config.minPresentInterval = 0;
    lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged =
        lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
    NvBool ok = submitConfig(cfg, rep, NV_TRUE);
    fReflipRc = ok ? (int)rep->flipResult : -1;
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok ? true : false;
}
// A table whose every entry is zero would black the panel for as long as it stays applied. macOS sends one when a
// display is added again and does not follow it with the normal table, so the picture never returns.
static bool nvrmLutIsBlack(UInt32 channels, UInt32 count, UInt32 width, const void *data)
{
    for (unsigned c = 0; c < (channels == 3 ? 3u : 1u); c++)
        for (UInt32 i = 0; i < count; i++) {
            const UInt32 v = width <= 8 ? ((const UInt8 *)data)[c * count + i] : ((const UInt16 *)data)[c * count + i];
            if (v) return false;
        }
    return true;
}
// setGammaTable: WindowServer's CLUT (1 or 3 channels, planar, `count` entries of `width` bits; 8-bit entries are bytes,
// wider ones 16-bit words, as IOGraphics passes them) resampled to NVKMS's 1024-entry 16-bit ramps and committed as the
// head's output LUT on its own: no mode change and no flip wait, so Night Shift's gradual changes cannot blank or stall
// the screen. A failed commit keeps the previous LUT; -nvrmnogamma keeps the old behaviour (accepted, not applied).
// Builds the next output-LUT surface from fGamma, in nvidia-drm 615's layout (create_drm_olut_surface_vss, non-VSS):
// R16G16B16A16, one row; a VSS header of NVKMS_LUT_VSS_HEADER_SIZE bytes left zero, then NVKMS_LUT_ARRAY_SIZE + 1
// entries of UNORM16 red, green, blue (alpha 0), the last a copy of the one before it. Pitch-linear scanout memory,
// video memory first as nvidia-drm does on a GPU with VRAM, system memory if that fails. NULL when it cannot be made.
struct NvKmsKapiSurface *NVRMNVDAFramebuffer::olutFromGamma()
{
    if (!fGamma || !fKms || !fDev) return nullptr;
    const unsigned slot = fOlutNext & 1;
    const NvU32 hdr = NVKMS_LUT_VSS_HEADER_SIZE / NVKMS_LUT_CAPS_LUT_ENTRY_SIZE, n = NVKMS_LUT_ARRAY_SIZE + 1;
    const NvU64 bytes = (((NvU64)(hdr + n) * NVKMS_LUT_CAPS_LUT_ENTRY_SIZE) + 255) & ~(NvU64)255;
    if (!fOlutSurf[slot]) {
        for (int vid = 1; vid >= 0 && !fOlutSurf[slot]; vid--) {
            struct NvKmsKapiAllocateMemoryParams mp = {}; NvU8 compressible = 0;
            mp.layout = NvKmsSurfaceMemoryLayoutPitch;
            mp.type = NVKMS_KAPI_ALLOCATION_TYPE_SCANOUT;
            mp.size = bytes;
            mp.useVideoMemory = vid ? NV_TRUE : NV_FALSE;
            mp.compressible = &compressible;
            struct NvKmsKapiMemory *m = fKms->allocateMemory(fDev, &mp);
            if (!m) continue;
            void *k = nullptr;
            if (!fKms->mapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, &k) || !k) { fKms->freeMemory(fDev, m); continue; }
            struct NvKmsKapiCreateSurfaceParams sp = {};
            sp.format = NvKmsSurfaceMemoryFormatR16G16B16A16;
            sp.width = hdr + n; sp.height = 1;
            sp.planes[0].memory = m; sp.planes[0].offset = 0; sp.planes[0].pitch = (NvU32)bytes;
            struct NvKmsKapiSurface *surf = fKms->createSurface(fDev, &sp);
            if (!surf) { fKms->unmapMemory(fDev, m, NVKMS_KAPI_MAPPING_TYPE_KERNEL, k); fKms->freeMemory(fDev, m); continue; }
            fOlutMem[slot] = m; fOlutKva[slot] = k; fOlutSurf[slot] = surf;
            FBLOG("gamma: output-LUT surface %u in %s (%llu bytes)", slot, vid ? "video memory" : "system memory",
                  (unsigned long long)bytes);
        }
        if (!fOlutSurf[slot]) { FBLOG("gamma: could not make an output-LUT surface"); return nullptr; }
    }
    volatile NvU16 *lut = (volatile NvU16 *)fOlutKva[slot];
    for (NvU32 i = 0; i < hdr * 4; i++) lut[i] = 0;
    for (NvU32 i = 0; i < NVKMS_LUT_ARRAY_SIZE; i++) {
        volatile NvU16 *e = lut + (size_t)(hdr + i) * 4;
        e[0] = fGamma->red[i]; e[1] = fGamma->green[i]; e[2] = fGamma->blue[i]; e[3] = 0;
    }
    volatile NvU16 *last = lut + (size_t)(hdr + n - 1) * 4, *prev = last - 4;
    for (int c = 0; c < 4; c++) last[c] = prev[c];
    __atomic_thread_fence(__ATOMIC_SEQ_CST);   // the table is in memory before NVKMS is told to read it
    fOlutNext = slot ^ 1;
    return fOlutSurf[slot];
}
IOReturn NVRMNVDAFramebuffer::nvrmSetGamma(UInt32 channels, UInt32 count, UInt32 width, const void *data)
{
    if (fProxy) return kIOReturnSuccess;
    char nb[8] = { 0 };
    if (PE_parse_boot_argn("-nvrmnogamma", nb, sizeof nb)) return kIOReturnSuccess;
    if (!data || (channels != 1 && channels != 3) || count < 2 || count > 4096 || width < 1 || width > 16) {
        FBLOG("gamma: refused a table (%u channels, %u entries, %u bits)", channels, count, width);
        return kIOReturnBadArgument;
    }
    if (nvrmLutIsBlack(channels, count, width, data) && fBlackLutDeadline &&
        mach_absolute_time() < fBlackLutDeadline) {
        FBLOG("gamma: all-black table ignored after a reconnect (previous LUT kept)");
        return kIOReturnSuccess;
    }
    fBlackLutDeadline = 0;
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
    struct NvKmsKapiSurface *olut = olutFromGamma();
    if (!olut) {
        FBLOG("gamma: no output-LUT surface on head %u; previous LUT kept", fHead);
        IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
        return kIOReturnSuccess;
    }
    nvrmHeadOlut(hr, olut);
    const NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    if (ok) fGammaCommits++; else fGammaFails++;
    if (fGammaCommits + fGammaFails <= 3 || !ok)
        FBLOGV("gamma: %u-channel %u x %u-bit table on head %u -> %s (flipResult %d; %u applied, %u failed)", channels, count,
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
    FBLOGV("reflip %u -> ok %u rc %d (head %u display 0x%x surface %s)",
          me->fReflips, (unsigned)rok, me->fReflipRc, me->fHead, me->fDisplay, me->fSurf ? "set" : "null");
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
    if (!fCursorCall) fCursorCall = nvrmfbCall(cursorFire, this);
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
    UInt32 w = ci.cursorWidth, h = ci.cursorHeight; const UInt32 *img = staging; UInt32 *turned = nullptr;
    SInt32 hx = (SInt32)ci.cursorHotSpotX, hy = (SInt32)ci.cursorHotSpotY, thx = hx, thy = hy;
    if (converted && fCurTransform && w && h && (UInt64)w * h * sizeof(UInt32) <= bytes) {
        const bool swap = (fCurTransform & kIOScaleSwapAxes) != 0;
        const bool ix = (fCurTransform & kIOScaleInvertX) != 0, iy = (fCurTransform & kIOScaleInvertY) != 0;
        const UInt32 dw = swap ? h : w, dh = swap ? w : h;
        turned = (UInt32 *)IOMalloc(bytes);
        if (turned) {
            for (UInt32 y = 0; y < dh; y++) for (UInt32 x = 0; x < dw; x++) {
                const UInt32 ux = iy ? dw - 1 - x : x, uy = ix ? dh - 1 - y : y;
                const UInt32 sx = swap ? uy : ux, sy = swap ? ux : uy;
                turned[y * dw + x] = staging[sy * w + sx];
            }
            thx = swap ? hy : hx; thy = swap ? hx : hy;
            if (iy) thx = (SInt32)dw - thx - 1;
            if (ix) thy = (SInt32)dh - thy - 1;
            img = turned; w = dw; h = dh;
        }
    }
    bool packed = converted && nvrmPackCursor((UInt32 *)fCurKva, fCurDim, img, w, h);
    if (packed) { fCurHotX = hx; fCurHotY = hy; fCurTurnedHotX = thx; fCurTurnedHotY = thy; }
    if (turned) IOFree(turned, bytes);
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
    {
        const bool swap = (fCurTransform & kIOScaleSwapAxes) != 0;
        const NvU32 tw = swap ? fH : fW, th = swap ? fW : fH;
        const bool scaledDesk = fCurId != kModeId && fCurDT.horizontalScaled && fCurDT.verticalScaled;
        const NvU32 dw = scaledDesk ? fCurDT.horizontalScaled : tw, dh = scaledDesk ? fCurDT.verticalScaled : th;
        SInt32 px = x + fCurHotX, py = y + fCurHotY;
        if (dw && dh && (dw != tw || dh != th)) {
            px = (SInt32)(((SInt64)px * (SInt64)tw) / (SInt64)dw);
            py = (SInt32)(((SInt64)py * (SInt64)th) / (SInt64)dh);
        }
        if (fCurTransform) {
            if (swap) { const SInt32 t = px; px = py; py = t; }
            if (fCurTransform & kIOScaleInvertY) px = (SInt32)fW - 1 - px;
            if (fCurTransform & kIOScaleInvertX) py = (SInt32)fH - 1 - py;
            x = px - fCurTurnedHotX; y = py - fCurTurnedHotY;
        } else { x = px - fCurHotX; y = py - fCurHotY; }
    }
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
    nvrmHeadConfigBaseline(hr, active && fGammaOn && fGamma ? olutFromGamma() : NULL);
    hr->modeSetConfig.bActive = active ? NV_TRUE : NV_FALSE;
    hr->flags.activeChanged = NV_TRUE;
    if (active) {
        hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = fMode;
        hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = fSurf;
        lr->config.srcWidth = (NvU16)fW; lr->config.srcHeight = (NvU16)fH;
        lr->config.dstWidth = (NvU16)fRasterW; lr->config.dstHeight = (NvU16)fRasterH;
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
    nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay; hr->modeSetConfig.mode = fMode;
    hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    lr->config.surface = fSurf; lr->config.srcWidth = (NvU16)fW; lr->config.srcHeight = (NvU16)fH;
    lr->config.dstWidth = (NvU16)fRasterW; lr->config.dstHeight = (NvU16)fRasterH;
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
        nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
        hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = fMode;
        lr->config.surface = fSurf; lr->config.srcWidth = (NvU16)fW; lr->config.srcHeight = (NvU16)fH;
        lr->config.dstWidth = (NvU16)fRasterW; lr->config.dstHeight = (NvU16)fRasterH;
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
    fDdcSinkLock = IORecursiveLockAlloc();
    struct NVRMDisplayPointers displayPtrs = {};
    displayPtrs.version = NVRM_DISPLAY_PTRS_ABI_VERSION;
    {
        const OSSymbol *displayPtrsFn = OSSymbol::withCString(NVRM_DISPLAY_PTRS_FN);
        if (displayPtrsFn) { provider->callPlatformFunction(displayPtrsFn, false, &displayPtrs, NULL, NULL, NULL); displayPtrsFn->release(); }
    }
    fKms = (struct NvKmsKapiFunctionsTable *)displayPtrs.kapi;
    fGpuId = (NvU32)propU64(provider, "gpu-id");
    fIndex = (unsigned)propU64(provider, "fb-index");
    fConnectNotify = nvrmfbCall(connectNotifyFire, this);
    if (!fConnectNotify) { FBLOG("fb%u: connect notification allocation failed -- refusing to start", fIndex); return false; }
    nvrmDiscoverBar1(provider, &fBarLen, &fBar1Base);
    FBLOG("BAR1 metadata: %llu MB at 0x%llx; normal surface budget %llu MB",
          (unsigned long long)(fBarLen >> 20), (unsigned long long)fBar1Base,
          (unsigned long long)(nvrmVramBudget(fBarLen) >> 20));
    if (fIndex == 0 && gFB != this) { retain(); gFB = this; }
    if (fIndex == 0 && !gHotplug) gHotplug = nvrmfbCall(hotplugFire, nullptr);
    if (fIndex == 0 && gHotplug && !gHotplugPoll) {
        UInt32 sec = 0;
        if (PE_parse_boot_argn("nvfbhotplugpoll", &sec, sizeof sec) && nvrmHotplugPollValid(sec)) {
            gHotplugPollSec = sec; gHotplugPoll = nvrmfbCall(hotplugPollFire, nullptr);
            if (gHotplugPoll) { uint64_t d; clock_interval_to_deadline(sec, kSecondScale, &d); thread_call_enter_delayed(gHotplugPoll, d); }
            FBLOG("hot-plug poll every %u s (nvfbhotplugpoll)", sec);
        }
    }
    if (!gNubParent && provider->getProvider()) { gNubParent = provider->getProvider(); gNubParent->retain(); }
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
    if (fHotplugBorn) FBLOG("fb%u: a display appeared after boot -- the connect interrupt will be fired once the head has a mode", fIndex);
    fPhysForVa = (nvrm_phys_for_va_t)displayPtrs.physForVa;
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
    if (fProxy) {
        if (!super::start(provider)) { FBLOG("proxy: super::start failed"); return false; }
        fVblTimer = nvrmfbCall(vblFire, this);
        if (fVblTimer) { uint64_t d; clock_interval_to_deadline(16667, kMicrosecondScale, &d); scheduleCall(fVblTimer, d); }
        fHwVblWanted = false;
        registerPowerDriver(this, kFBPowerStates, 2);
        FBLOG("proxy: started on %s (vbl timer %s)", provider->getName(), fVblTimer ? "armed" : "ALLOCATE FAILED");
        fbGuardCleared("the integrated-GPU panel proxy started");
        return true;
    }
    { char loc[12]; if (nvrmFbLocation(fIndex, loc, sizeof loc)) setLocation(loc); }   // distinct registry path per head
    if (fInternal) publishBacklight();   // before super::start(): IOFramebuffer::open looks for "backlight"
    {   // output colour controls from the boot line (see gColorBpc); invalid values are ignored
        static bool done; if (!done) { done = true;
            UInt32 v = 0;
            if (PE_parse_boot_argn("nvfbbpc", &v, sizeof v) && nvrmColorBpcValid(v)) gColorBpc = (int)v;
            if (PE_parse_boot_argn("nvfbformat", &v, sizeof v) && nvrmColorFormatValid(v)) gColorFormat = (int)v;
            if (PE_parse_boot_argn("nvfbrange", &v, sizeof v) && nvrmColorRangeValid(v)) gColorRange = (int)v;
            if (PE_parse_boot_argn("nvfbvib", &v, sizeof v) && v <= 200) gVibrance = (int)v - 100;
            gHdrOn = PE_parse_boot_argn("nvfbhdr", &v, sizeof v) && v;
            gRotScale = PE_parse_boot_argn("nvfbrotscale", &v, sizeof v) && v;
            gTenBit = PE_parse_boot_argn("nvfb10bit", &v, sizeof v) && v;
            if (gHdrOn || gTenBit) FBLOG("%s%s", gTenBit ? "10-bit scanout on mode changes (nvfb10bit=1) " : "", gHdrOn ? "HDR10 advertised for PQ displays (nvfbhdr=1)" : "");
            if (gColorBpc || gColorFormat != 3 || gColorRange || gVibrance)
                FBLOG("colour controls: bpc %d format %d range %d vibrance %d", gColorBpc, gColorFormat, gColorRange, gVibrance); } }
    publishDdcBus();                     // before super::start() too: IOFramebuffer::open creates the I2C interface from it
    if (fDdcOn) publishDdcBrightness();
    if (!super::start(provider)) {
        if (fDdcBrightness) { fDdcBrightness->detach(this); OSSafeReleaseNULL(fDdcBrightness); }
        if (fDdcBrightnessCall) { thread_call_free(fDdcBrightnessCall); fDdcBrightnessCall = nullptr; }
        if (fDdcProbeCall) { thread_call_free(fDdcProbeCall); fDdcProbeCall = nullptr; }
        NVRMDisplayGate gate; if (gate.valid() && fOwnsHead) { gHeadsTaken &= ~(1u << fHead); fOwnsHead = false; }
        FBLOG("super::start failed after kapiInit succeeded"); return false;
    }
    fSleepTap = addFramebufferNotification(&NVRMNVDAFramebuffer::sleepTap, this, nullptr);
    if (fHidden) {   // the group's master offers the combined mode once every member is up: tell it this one is
        NVRMNVDAFramebuffer *master = gFBs[nvrmSurroundMaster(gSurroundMask, (unsigned)gNDpy) & 7];
        if (master && master != this) master->notifyConnectionChanged(500);
    }
    if (!fSleepTap) FBLOG("sleep/wake notification tap not registered - a wake after dark wakes may leave the accelerator off");
    fVblTimer = nvrmfbCall(vblFire, this);
    if (fVblTimer) { uint64_t d; clock_interval_to_deadline(16667, kMicrosecondScale, &d); scheduleCall(fVblTimer, d); }
    FBLOG("vbl timer %s", fVblTimer ? "armed (fallback)" : "ALLOCATE FAILED");
    fHwVblCall = nvrmfbCall(hwVblDeliver, this);
    UInt32 hwvblArg = 1; PE_parse_boot_argn("nvhwvbl", &hwvblArg, sizeof hwvblArg);
    fHwVblWanted = hwvblArg && fHwVblCall && fKms && fDev && fKms->registerVblankIntrCallback && fHead < 8;
    FBLOG("hw vblank on head %u: %s", fHead, !hwvblArg ? "OFF (boot-arg nvhwvbl=0) - the timer is the VBL"
                                       : fHwVblWanted ? "deferred until NVKMS latches a flip on this head" : "not available - the timer stays the VBL");
    fReflip = nvrmfbCall(reflipFire, this);
    fGpuWake = nvrmfbCall(gpuWakeFire, this);
    FBLOG("reflip keep-alive NOT armed: enableController's modeset owns the head, no boot-time re-syncs");
    fBars = nvrmfbCall(barsFire, this);
    uint32_t hudGate = 0;
    if (fBars && PE_parse_boot_argn("nvhud", &hudGate, sizeof hudGate) && hudGate) {
        uint64_t d; clock_interval_to_deadline(45, kSecondScale, &d);
        scheduleCall(fBars, d);
        FBLOG("PROBE GRID + HUD ARMED (boot-arg nvhud=1): fires at t+45 s");
    } else {
        FBLOG("probe grid + HUD OFF -- macOS only (add nvhud=1 to boot-args to show them)");
    }
    fHeadProbe = nvrmfbCall(headProbeFire, this);
    FBLOG("head probe retired (answered: we own head %u)", fHead);
    uint32_t sampleGate = 0;
    if (PE_parse_boot_argn("nvfbsample", &sampleGate, sizeof sampleGate) && sampleGate) {
        fSampler = nvrmfbCall(sampleFire, this);
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
    if (fAudioReady) { fAudioReady->remove(); fAudioReady = nullptr; }
    if (fCodecReady) { fCodecReady->remove(); fCodecReady = nullptr; }
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
    thread_call_t *calls[] = {&fHwVblCall, &fReflip, &fGpuWake, &fBars, &fVblTimer, &fSampler, &fHeadProbe, &fCursorCall, &fConnectNotify, &fAudioCall, &fDdcBrightnessCall, &fDdcProbeCall};
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
    if (fDdcBrightness) {
        fDdcBrightness->cancelProbe();
        fDdcBrightness->detach(this);
        OSSafeReleaseNULL(fDdcBrightness);
    }
    if (fDdcBrightnessGate) {
        if (IOWorkLoop *loop = getWorkLoop()) loop->removeEventSource(fDdcBrightnessGate);
        OSSafeReleaseNULL(fDdcBrightnessGate);
    }
    if (fBacklight) { nvrmBacklightClearGlobal(fBacklight); fBacklight->detach(this); OSSafeReleaseNULL(fBacklight); }
    if (fGamma) { IOFree(fGamma, sizeof *fGamma); fGamma = nullptr; fGammaOn = false; }
    if (fWakeGate) { if (IOWorkLoop *wl = getWorkLoop()) wl->removeEventSource(fWakeGate); OSSafeReleaseNULL(fWakeGate); }
    if (fSleepTap) { fSleepTap->remove(); fSleepTap = nullptr; }
    super::stop(provider);
}

// The internal panel's brightness control, when NVKMS registered a backlight for it (needs the NBCI _DSM).
static IOReturn nvrmEdpSet(NVRMBacklightHandler *h, IOService *rm, unsigned percent)
{
    unsigned char mode = 0, ctl = 0;
    IOReturn r = nvrmDpAux(rm, h->edpGpu(), h->edpDisplay(), false, kEdpBacklightMode, &mode, 1);
    if (r == kIOReturnSuccess) r = nvrmDpAux(rm, h->edpGpu(), h->edpDisplay(), false, kEdpDisplayCtl, &ctl, 1);
    if (r != kIOReturnSuccess) return r;
    EdpWrite w[5]; const unsigned n = edpBacklightPlan(h->edpCaps(), mode, ctl, percent, w);
    for (unsigned i = 0; i < n && r == kIOReturnSuccess; i++) { unsigned char b = w[i].value; r = nvrmDpAux(rm, h->edpGpu(), h->edpDisplay(), true, w[i].addr, &b, 1); }
    return r;
}
void NVRMNVDAFramebuffer::publishBacklight()
{
    IOService *rm = getProvider();
    while (rm && strcmp(rm->getMetaClass()->getClassName(), "NVRM") != 0) rm = rm->getProvider();
    const OSSymbol *fn = OSSymbol::withCString("NVRMBacklight");
    NvU32 level = 0;
    IOReturn r = (rm && fn) ? rm->callPlatformFunction(fn, false, &level, NULL, NULL, NULL) : kIOReturnNotFound;
    OSSafeReleaseNULL(fn);
    bool viaAux = false; EdpBacklightCaps caps = {};
    if (r != kIOReturnSuccess) {
        // NVKMS knows no way to dim this panel. If the panel takes brightness over the eDP AUX channel (its DPCD says so), do
        // that; a panel that only has the PWM pin stays without a control. Off with boot-arg nvfbedpbl=0.
        UInt32 on = 1;
        unsigned char cap[2] = { 0, 0 };
        PE_parse_boot_argn("nvfbedpbl", &on, sizeof on);
        if (on && rm && nvrmDpAux(rm, fGpuId, (unsigned)fDisplay, false, kEdpGeneralCap1, cap, 2) == kIOReturnSuccess) {
            caps = edpBacklightCaps(cap[0], cap[1]);
            FBLOG("fb%u: no NVKMS backlight; eDP DPCD 0x701=0x%02x 0x702=0x%02x -> AUX brightness %s", fIndex, cap[0], cap[1], caps.auxSet ? "supported" : "not offered by the panel");
            if (caps.auxSet) {
                unsigned char msb = 0, lsb = 0;
                if (nvrmDpAux(rm, fGpuId, (unsigned)fDisplay, false, kEdpBacklightMsb, &msb, 1) == kIOReturnSuccess) {
                    if (caps.twoBytes) (void)nvrmDpAux(rm, fGpuId, (unsigned)fDisplay, false, kEdpBacklightLsb, &lsb, 1);
                    const unsigned raw = caps.twoBytes ? ((unsigned)msb << 8 | lsb) : msb, full = caps.twoBytes ? 0xFFFFu : 0xFFu;
                    level = raw >= full ? 100 : (raw * 100 + full / 2) / full;
                } else level = 100;
                viaAux = true; r = kIOReturnSuccess;
            }
        }
    }
    if (r != kIOReturnSuccess) { FBLOG("fb%u: internal panel, but NVKMS registered no backlight (0x%x) -- no brightness control", fIndex, r); return; }
    fBacklight = NVRMBacklightHandler::withRM(rm, level);
    if (fBacklight && viaAux) fBacklight->useEdpAux(fGpuId, (unsigned)fDisplay, caps);
    if (!fBacklight) { FBLOG("fb%u: backlight handler allocation failed", fIndex); return; }
    fBacklight->setName("backlight");
    if (!fBacklight->attach(this)) { OSSafeReleaseNULL(fBacklight); FBLOG("fb%u: backlight handler attach failed", fIndex); return; }
    fBacklight->registerService(kIOServiceSynchronous);
    nvrmBacklightPublishGlobal(fBacklight);
    { static volatile SInt32 once; if (OSCompareAndSwap(0, 1, &once)) sysctl_register_oid(&sysctl__debug_nvrmfb_backlight_restore); }
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
    if (fDdcSinkLock) { IORecursiveLockFree(fDdcSinkLock); fDdcSinkLock = nullptr; }
    fConsKva = nullptr; OSSafeReleaseNULL(fConsMap); OSSafeReleaseNULL(fConsMem);
    OSSafeReleaseNULL(fPxMap);
    super::free();
}

// The panel this head stands in for: boot-arg nvfbigpu=1, an Intel display-class device on the bus, and a 32-bit firmware
// console that is not inside this GPU's own BARs (nvrm_proxy_decide).
bool NVRMNVDAFramebuffer::proxyFind()
{
    int on = 0;
    PE_parse_boot_argn("nvfbigpu", &on, sizeof on);
    PE_Video v; nvu_zero(&v, sizeof v);
    struct nvrm_proxy_console c = {};
    if (on && getPlatform() && getPlatform()->getConsoleInfo(&v) == kIOReturnSuccess) {
        c.base = (uint64_t)v.v_baseAddr & ~3ull; c.width = (uint32_t)v.v_width; c.height = (uint32_t)v.v_height;
        c.pitch = (uint32_t)v.v_rowBytes; c.depth = (uint32_t)v.v_depth;
    }
    int intel = 0;
    if (on) {
        if (OSDictionary *m = IOService::serviceMatching("IOPCIDevice")) {
            if (OSIterator *it = IOService::getMatchingServices(m)) {
                while (OSObject *o = it->getNextObject()) {
                    IOService *d = OSDynamicCast(IOService, o);
                    OSData *ven = d ? OSDynamicCast(OSData, d->getProperty("vendor-id")) : nullptr;
                    OSData *cls = d ? OSDynamicCast(OSData, d->getProperty("class-code")) : nullptr;
                    if (!ven || !cls || ven->getLength() < 2 || cls->getLength() < 3) continue;
                    const uint8_t *vb = (const uint8_t *)ven->getBytesNoCopy(), *cb = (const uint8_t *)cls->getBytesNoCopy();
                    if ((vb[0] | (vb[1] << 8)) == 0x8086 && cb[2] == 0x03) { intel = 1; break; }
                }
                it->release();
            }
            m->release();
        }
    }
    struct nvrm_proxy_range bars[NVRM_PROXY_MAX_BARS] = {};
    unsigned nb = 0;
    if (on) if (IOService *nub = pciNub()) {
        IOItemCount n = nub->getDeviceMemoryCount();
        for (IOItemCount i = 0; i < n && nb < NVRM_PROXY_MAX_BARS; i++) {
            IODeviceMemory *m = (IODeviceMemory *)nub->getDeviceMemoryWithIndex(i);
            if (m) { bars[nb].base = (uint64_t)m->getPhysicalAddress(); bars[nb].size = (uint64_t)m->getLength(); nb++; }
        }
    }
    const enum nvrm_proxy_verdict why = nvrm_proxy_decide(on, &c, intel, bars, nb);
    if (why != NVRM_PROXY_OK) {
        if (on) FBLOG("nvfbigpu=1 but no proxy (reason %d: 1 off, 2 no console, 3 bad format, 4 too large, 5 no Intel GPU, 6 own console)", (int)why);
        return false;
    }
    fPxBase = c.base; fPxW = c.width; fPxH = c.height; fPxPitch = c.pitch;
    fPxBytes = ((uint64_t)fPxPitch * fPxH + 0xfffull) & ~0xfffull;
    return true;
}
bool NVRMNVDAFramebuffer::proxyMap()
{
    unsigned c = 1; PE_parse_boot_argn("nvfbigpucache", &c, sizeof c);   // 1 write-combined (default), 2 uncached
    IODeviceMemory *dm = IODeviceMemory::withRange((IOPhysicalAddress)fPxBase, (IOPhysicalLength)fPxBytes);
    if (dm) { fPxMap = dm->map(c == 2 ? kIOMapInhibitCache : kIOMapWriteCombineCache); dm->release(); }
    FBLOG("proxy: panel framebuffer mapped %s -> %s", c == 2 ? "uncached" : "write-combined", fPxMap ? "ok" : "FAILED");
    return fPxMap != nullptr;
}

IOReturn NVRMNVDAFramebuffer::enableController()
{
    FBLOG("enableController");
    if (fProxy) return kIOReturnSuccess;
    if (fDdcBrightness && !fDdcBrightnessGate) {
        if (IOWorkLoop *loop = getWorkLoop()) {
            fDdcBrightnessGate = IOCommandGate::commandGate(this);
            if (fDdcBrightnessGate && loop->addEventSource(fDdcBrightnessGate) != kIOReturnSuccess)
                OSSafeReleaseNULL(fDdcBrightnessGate);
        }
    }
    fEnableAbs = mach_absolute_time();
    const bool ok = applyMode();
    // A failed boot mode set must not fail the open. IOFramebuffer::open() marks a framebuffer whose enableController
    // failed as dead for good, WindowServer's IOServiceOpen on it fails (xnu reports the masked error as
    // 0xe00002c7, kIOReturnUnsupported) and CoreDisplay aborts in CGXMappedDisplayStart ("Driver Error: FB: x of y
    // opened"). Measured on 2.7.2: "applyModeSetConfig -> 0" 0.7 s before "IOServiceOpen failed(0xe00002c7), index(0)",
    // 4 of 5 such boots with the abort. Opened, the head is still there for WindowServer's own setDisplayMode, which
    // tries the mode set again (fModeSet stays false).
    if (!ok) FBLOG("fb%u: the boot mode set failed -- opening anyway so WindowServer can set a mode (a failed open "
                   "would leave this framebuffer dead and WindowServer aborting)", fIndex);
    if (ok) refreshDisplayAudioProperties();
    // A display that appeared after boot: tell the framework, through the connect handler it registered (fConnect),
    // that this connection is new. It has to happen after the mode set -- WindowServer re-probes a framebuffer that
    // has a mode, and firing it earlier leaves the display undetected -- so it is a delayed call.
    if (ok && fHotplugBorn && !fConnectFired) {
        notifyConnectionChanged(1500);
        FBLOG("fb%u: connect interrupt scheduled (1.5 s after the mode set)", fIndex);
    }
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::setDisplayMode(IODisplayModeID mode, IOIndex depth)
{
    FBLOG("fb%u setDisplayMode 0x%x depth %d (current 0x%x)", fIndex, (unsigned)mode, (int)depth, (unsigned)fCurId);
    if (fProxy) return (mode == kModeId && depth == 0) ? kIOReturnSuccess : kIOReturnUnsupportedMode;
    NVRMBoundedGate modeGate(kModeChangeGateMs);   // held to the end of the call; the inner gates are recursive
    if (!modeGate.held()) {
        FBLOG("fb%u setDisplayMode 0x%x: the display gate was not free in %u ms (another framebuffer is starting, relighting or "
              "waiting on a flip) -- the mode change is refused so WindowServer is not left in the kernel; it will ask again",
              fIndex, (unsigned)mode, kModeChangeGateMs);
        return kIOReturnTimeout;
    }
    fNewIdOnce = 0;
    if (depth != 0) return kIOReturnUnsupportedMode;
    IOReturn result;
    if ((mode >> 16) == 4 && fSurN) {
        result = dtIndex(mode) >= 0 ? switchSurround(mode) : kIOReturnUnsupportedMode;
    } else if (mode == kModeId) {
        fPendingScaled = false;
        result = fCurId == kModeId
            ? (applyMode() ? kIOReturnSuccess : kIOReturnError)
            : (fHdrWant = false, switchMode(kModeId, fBootMode, fBootW, fBootH, 0));
    } else {
        int i = dtIndex(mode);
        if (i < 0) return kIOReturnUnsupportedMode;
        if (mode == fCurId && dtIsCurrent(fDT[i])) {
            result = fModeSet ? kIOReturnSuccess : (applyMode() ? kIOReturnSuccess : kIOReturnError);
        } else {
            struct NvKmsKapiDisplayMode m = {};
            if (!edidModeFor(&fDT[i], &m)) dtToKapi(&fDT[i], &m);
            NvU32 w = 0, h = 0; dtSurfaceSize(&fDT[i], &w, &h);
            const IODetailedTimingInformationV2 dt = fDT[i];
            fHdrWant = gHdrOn && fHdrCapable && dt.dynamicRange == kIODynamicRangeHDR10;
            {   // is it a scaled mode, and how heavy
                const UInt64 tot = (UInt64)(dt.horizontalActive + dt.horizontalBlanking) * (dt.verticalActive + dt.verticalBlanking);
                const unsigned hz = tot ? (unsigned)((dt.pixelClock + tot / 2) / tot) : 0;
                fPendingScaled = dt.horizontalScaled && dt.verticalScaled;
                fPendingLoad = nvrmScalerLoad(dt.horizontalActive, dt.verticalActive, dt.horizontalScaled, dt.verticalScaled, hz);
            }
            result = switchMode(mode, m, w, h, dt.scalerFlags & kIOScaleRotateFlags);
            if (result == kIOReturnSuccess) fCurDT = dt;
        }
    }
    if (result == kIOReturnSuccess) refreshDisplayAudioProperties();
    return result;
}
void NVRMNVDAFramebuffer::publishStartupTiming()
{
    IOTimingInformation ti; nvu_zero(&ti, sizeof ti);
    if (getTimingInfoForDisplayMode(fCurId, &ti) != kIOReturnSuccess || !(ti.flags & kIODetailedTimingValid)) return;
    OSData *d = OSData::withBytes(&ti, sizeof ti);
    if (d) { setProperty("startup-timing", d); d->release(); }   // kIOFBStartupTimingPrefsKey
}
IOReturn NVRMNVDAFramebuffer::getCurrentDisplayMode(IODisplayModeID *mode, IOIndex *depth)
{
    IODisplayModeID id = fCurId;
    const UInt32 once = fNewIdOnce;
    if (once && OSCompareAndSwap(once, 0, &fNewIdOnce)) id = (IODisplayModeID)once;
    if (mode) *mode = id;
    if (depth) *depth = 0;
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::publishRotatedSizes(bool force)
{
    IORegistryEntry *disp = nullptr;
    if (OSIterator *it = getChildIterator(gIOServicePlane)) {
        while (OSObject *o = it->getNextObject()) {
            IORegistryEntry *e = OSDynamicCast(IORegistryEntry, o);
            if (e && e->metaCast("IODisplayConnect")) { disp = e->getChildEntry(gIOServicePlane); break; }
        }
        it->release();
    }
    if (!disp) { if (force) FBLOG("fb%u: no display nub to offer the rotated sizes to", fIndex); return; }
    OSObject *ao = disp->copyProperty("IODisplayAttributes");
    OSDictionary *attrs = OSDynamicCast(OSDictionary, ao);
    if (!attrs) {
        if (ao) ao->release();
        if (force) FBLOG("fb%u: the display nub has no attributes from WindowServer yet", fIndex);
        return;
    }
    const bool want = (fTransform & kIOScaleSwapAxes) && fBootW && fBootH;
    const bool have = attrs->getObject("scale-resolutions") != nullptr;
    // Withdraw only what this driver offered. A display override can carry its own scale-resolutions (the usual way
    // to turn HiDPI on for a monitor); removing those on a display that is not rotated took its HiDPI sizes away.
    if ((!want && (!have || !fRotSizesOurs)) || (want && have && !force)) { attrs->release(); return; }
    OSDictionary *d = OSDictionary::withDictionary(attrs);
    attrs->release();
    if (!d) return;
    if (!want) d->removeObject("scale-resolutions");
    else {
        OSArray *a = OSArray::withCapacity(2);
        if (!a) { d->release(); return; }
        const NvU32 sizes[2][2] = { { fBootW, fBootH }, { fRasterW, fRasterH } };
        for (unsigned i = 0; i < 2; i++) {
            if (!sizes[i][0] || !sizes[i][1] || (i && sizes[i][0] == sizes[0][0] && sizes[i][1] == sizes[0][1])) continue;
            const UInt32 e[5] = { OSSwapHostToBigInt32(sizes[i][0]), OSSwapHostToBigInt32(sizes[i][1]), OSSwapHostToBigInt32(0x3),
                                  0, OSSwapHostToBigInt32(kDisplayModeValidForMirroringFlag) };
            OSData *od = OSData::withBytes(e, sizeof e);
            if (od) { a->setObject(od); od->release(); }
        }
        d->setObject("scale-resolutions", a);
        a->release();
    }
    fRotSizesOurs = want;
    disp->setProperty("IODisplayAttributes", d);
    d->release();
    if (want) FBLOG("fb%u: scale-resolutions %ux%u%s offered to macOS for the rotated layout (transform 0x%x)", fIndex, fBootW, fBootH,
                    (fRasterW != fBootW || fRasterH != fBootH) ? " and the current raster" : "", (unsigned)fTransform);
    else FBLOG("fb%u: scale-resolutions withdrawn (transform 0x%x)", fIndex, (unsigned)fTransform);
}
bool NVRMNVDAFramebuffer::dtIsCurrent(const IODetailedTimingInformationV2 &d) const
{
    if (fSurActive || (d.detailedTimingModeID >> 16) == 4) return fSurActive && fModeSet && (d.detailedTimingModeID >> 16) == 4 && (d.detailedTimingModeID & 0xfff) == fSurHz;
    NvU32 w = 0, h = 0; dtSurfaceSize(&d, &w, &h);
    return fModeSet && w == fW && h == fH && d.horizontalActive == fRasterW && d.verticalActive == fRasterH
        && (d.scalerFlags & kIOScaleRotateFlags) == fCurTransform && d.pixelClock == fMode.timings.pixelClockHz
        && (d.dynamicRange == kIODynamicRangeHDR10) == fHdrActive;   // the same raster as an HDR10 timing is another mode
}
void NVRMNVDAFramebuffer::dtSurfaceSize(const IODetailedTimingInformationV2 *d, NvU32 *w, NvU32 *h)
{
    *w = d->horizontalActive; *h = d->verticalActive;
}
static void dtRasterTurned(const IODetailedTimingInformationV2 *d, NvU32 *w, NvU32 *h)
{
    const bool swap = (d->scalerFlags & kIOScaleSwapAxes) != 0;
    *w = swap ? d->verticalActive : d->horizontalActive; *h = swap ? d->horizontalActive : d->verticalActive;
}
bool NVRMNVDAFramebuffer::dtIsBootRaster(const IODetailedTimingInformationV2 &d) const
{
    if (d.horizontalScaled || d.verticalScaled || d.horizontalActive != fBootW || d.verticalActive != fBootH) return false;
    const UInt64 tot = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
    return tot && (NvU32)((d.pixelClock + tot / 2) / tot) == (fBootMode.timings.refreshRate + 500) / 1000;
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
      uint32_t dsrOn = 0; PE_parse_boot_argn("nvfbdsr", &dsrOn, sizeof dsrOn);
      if ((PE_parse_boot_argn("nvfbhidpi", &hidpi, sizeof hidpi) && hidpi) || dsrOn) {
          fScalerMaxW = mw * 2 > 7680 ? 7680 : mw * 2;
          fScalerMaxH = mh * 2 > 4320 ? 4320 : mh * 2;
          FBLOG("nvfbhidpi: scaled desktops up to %ux%u on a %ux%u raster", fScalerMaxW, fScalerMaxH, mw, mh);
      } }
    const NvU64 mp_ = ((NvU64)fScalerMaxW * 4 + pitchAlign - 1) & ~((NvU64)pitchAlign - 1);
    fAllocSize = (mp_ * fScalerMaxH + 0xffff) & ~0xffffull;
    // the reservation has a floor that does not depend on the first EDID (nvrmfb_scanout_reserve.h); the ceilings
    // above stay those of the mode list
    fModesAllocSize = fAllocSize > fSize ? fAllocSize : fSize;   // what the mode list alone asks for
    fAllocSize = nvrmfb_scanout_reserve(fAllocSize, fSize, pitchAlign);
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
            FBLOGV("fb%u NVKMS mode %u: %ux%u @%u Hz pclk %u valid %u preferred %u%s%s", fIndex, i, k.hVisible, k.vVisible, hz,
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
        // nvfbsmall=1 offers the smaller ones as well: where NVKMS refuses macOS's scaled modes on the native raster (a
        // 4K-only firmware raster on an HDMI dummy plug), they are the only way to a desktop below the boot size.
        static int small = -1;
        if (small < 0) { uint32_t v = 0; const bool arg = PE_parse_boot_argn("nvfbsmall", &v, sizeof v);
                         small = nvrmSmallRastersWanted(arg, v, !arg && nvrmRootFile("/Library/NullMoth/nvfb-small")) ? 1 : 0;
                         if (small) FBLOG("nvfbsmall: rasters smaller than the boot raster are offered too"); }
        const bool smaller = small && fBootW && !bootRaster && !larger;
        if (!raster && !have && (bootRaster || larger || smaller) && fMemBytes && k.hVisible <= fRasterMaxW && k.vVisible <= fScalerMaxH) {
            NvU32 pitch = 0;
            if (nvrmCheckedPitch(k.hVisible, fPitchAlign, &pitch) && (NvU64)pitch * k.vVisible <= fMemBytes) raster = true;
        }
        if (have || !raster) {
            if (!fromMac && !fKmsModesLogged)
                FBLOGV("fb%u NVKMS mode %u: not offered (%s)", fIndex, i, have ? "already listed at this rate"
                      : !fBootW ? "no boot raster yet" : (!bootRaster && !larger && !smaller) ? "smaller than the boot raster (desktops that size come from macOS's scaled modes; nvfbsmall=1 offers it)"
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
        const NvU64 dscLimit = nvrmDscThresholdBps(fIndex < (unsigned)gNDpy && gNDpy > 0 ? gDpy[fIndex].hdaConnectorType : 0,
                                                   fInternal);
        if (dscLimit && (NvU64)k.pixelClockHz * 24 > dscLimit && !b.dscCompressedBitsPerPixel) {
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
    // Derived refresh rates: a 240/300 Hz panel is also driven at half and a quarter of its rate (same blanking, pixel
    // clock divided), so a desktop that does not need the rate can run at 120/60 Hz at lower power. Only for a native
    // NVKMS mode of 240 Hz and up whose raster is already listed unscaled, and only where the rate is not listed yet.
    // IDs use the copy byte: 1 = half, 2 = quarter.
    for (NvU32 i = 0; i < 256 && n < cap; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &pref);
        if (r < 0) break;
        if (r == 0 || !valid) continue;
        const struct NvKmsKapiDisplayModeTimings &k = m.timings;
        if (k.flags.interlaced || k.flags.doubleScan || !k.pixelClockHz || k.hTotal <= k.hVisible || k.vTotal <= k.vVisible ||
            k.hSyncStart < k.hVisible || k.hSyncEnd < k.hSyncStart || k.vSyncStart < k.vVisible || k.vSyncEnd < k.vSyncStart) continue;
        const NvU64 tot = (NvU64)k.hTotal * k.vTotal;
        const NvU32 hz = (NvU32)((k.pixelClockHz + tot / 2) / tot);
        if (hz < kNvrmDerivedRateMinHz) continue;
        for (unsigned copy = 1; copy <= 2 && n < cap; copy++) {
            const NvU32 div = copy == 1 ? 2 : 4, dhzWant = (hz + div / 2) / div;
            if (dhzWant < kNvrmDerivedRateFloorHz) continue;
            int tmpl = -1; bool listed = false;
            for (unsigned j = 0; j < n; j++) {
                const IODetailedTimingInformationV2 &d = fDT[j];
                if (d.horizontalActive != k.hVisible || d.verticalActive != k.vVisible || d.horizontalScaled || d.verticalScaled) continue;
                const UInt64 dt = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
                const NvU32 dhz = dt ? (NvU32)((d.pixelClock + dt / 2) / dt) : 0;
                if (dhz + 1 >= dhzWant && dhz <= dhzWant + 1) listed = true;
                if (tmpl < 0) tmpl = (int)j;
            }
            // the raster must already be offered (it passed the memory and scaler checks above), and the rate not
            if (listed || tmpl < 0) continue;
            IODetailedTimingInformationV2 b = fDT[tmpl];
            b.pixelClock = b.minPixelClock = b.maxPixelClock = k.pixelClockHz / div;
            b.horizontalBlanking = k.hTotal - k.hVisible; b.horizontalSyncOffset = k.hSyncStart - k.hVisible;
            b.horizontalSyncPulseWidth = k.hSyncEnd - k.hSyncStart;
            b.verticalBlanking = k.vTotal - k.vVisible; b.verticalSyncOffset = k.vSyncStart - k.vVisible;
            b.verticalSyncPulseWidth = k.vSyncEnd - k.vSyncStart;
            b.horizontalSyncConfig = k.flags.hSyncPos ? 1 : 0; b.verticalSyncConfig = k.flags.vSyncPos ? 1 : 0;
            b.dscCompressedBitsPerPixel = 0; b.dscSliceWidth = 0; b.dscSliceHeight = 0;
            b.detailedTimingModeID = 0x10000u | (i << 8) | copy;
            // A derived timing is not one of the display's own modes, so NVKMS decides at mode set whether the panel can
            // take it, and some cannot (eDP panels that accept only their EDID timings). The test pass of switchMode then
            // refuses it, and WindowServer still shows the rate as selected while the panel stays where it was. Ask
            // NVKMS the same question here (the timing setDisplayMode will submit) and offer only what it accepts.
            {
                struct NvKmsKapiDisplayMode dm = {};
                dtToKapi(&b, &dm);
                if (!fKms->validateDisplayMode || !fKms->validateDisplayMode(fDev, fDisplay, &dm)) {
                    FBLOG("fb%u derived refresh rate %ux%u @%u Hz (native %u Hz / %u): NVKMS refused the timing -- not offered",
                          fIndex, k.hVisible, k.vVisible, dhzWant, hz, div);
                    continue;
                }
            }
            fDT[n++] = b; added++;
            FBLOG("fb%u derived refresh rate: %ux%u @%u Hz (native %u Hz / %u) as dt 0x%x", fIndex, k.hVisible, k.vVisible,
                  dhzWant, hz, div, b.detailedTimingModeID);
        }
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
IOReturn NVRMNVDAFramebuffer::switchMode(IODisplayModeID id, const struct NvKmsKapiDisplayMode &m, NvU32 w, NvU32 h, UInt32 transform)
{
    NVRMDisplayGate gate; if (!gate.valid()) return kIOReturnNoMemory;
    if (fSubmissionFaulted) return kIOReturnNotReady;
    if (!fKms || !fDev || !fMem || fConsoleAperture) return kIOReturnNotReady;
    NvU32 pitch = 0;
    if (!nvrmKmsRasterValid(w, h) || !nvrmKmsRasterValid(m.timings.hVisible, m.timings.vVisible) ||
        !nvrmCheckedPitch(w, fPitchAlign, &pitch)) return kIOReturnBadArgument;
    if ((NvU64)pitch * h > fMemBytes) {
        FBLOG("switchMode 0x%x: %ux%u pitch %u needs %llu B, the scanout holds %llu -- REFUSED",
              (unsigned)id, w, h, pitch, (unsigned long long)pitch * h, (unsigned long long)fMemBytes);
        return kIOReturnNoMemory;
    }
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
    // A 30-bit mode (HDR10, or nvfb10bit=1) scans an X2B10G10R10 surface; the accelerator is told in the nvScanoutInfo reply.
    const bool ten = fHdrWant || gTenBit;
    const bool hdrBefore = fHdrActive;
    fHdrActive = fHdrWant;
    sp.width = w; sp.height = h; sp.format = ten ? NvKmsSurfaceMemoryFormatX2B10G10R10 : NvKmsSurfaceMemoryFormatX8R8G8B8;
    struct NvKmsKapiSurface *ns = fKms->createSurface(fDev, &sp);
    if (!ns) { fHdrActive = hdrBefore; FBLOG("switchMode 0x%x: createSurface %ux%u pitch %u FAILED", (unsigned)id, w, h, pitch); return kIOReturnNoMemory; }
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); fKms->destroySurface(fDev, ns); return kIOReturnNoMemory; }
    NvBool ok = NV_FALSE;
    // Boot-arg nvfbkeepfront=1: carry the frame on screen through the mode set (see nvrmKeepFrontSurface).
    UInt32 keepArg = 0; PE_parse_boot_argn("nvfbkeepfront", &keepArg, sizeof keepArg);
    const bool keepFront = nvrmKeepFrontSurface(keepArg != 0, fFrontSurf != nullptr, fFrontSurf == fSurf, fW, fH, w, h, fCurTransform, transform);
    struct NvKmsKapiSurface *shown = keepFront ? fFrontSurf : ns;
    for (int commit = 0; commit < 2; commit++) {
        nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
        cfg->headsMask = 1u << fHead;
        struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
        nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
        hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = m;
        hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = shown; lr->config.srcWidth = (NvU16)w; lr->config.srcHeight = (NvU16)h;
        lr->config.dstWidth = (NvU16)m.timings.hVisible; lr->config.dstHeight = (NvU16)m.timings.vVisible;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
        nvrmUnownedHeadsOff(cfg);
        ok = submitConfig(cfg, rep, commit ? NV_TRUE : NV_FALSE);
        FBLOG("switchMode 0x%x: %s %ux%u -> raster %ux%u @%u mHz pclk %u rotation 0x%x -> %u (flipResult %d)", (unsigned)id,
              commit ? "COMMIT" : "test", w, h, m.timings.hVisible, m.timings.vVisible, m.timings.refreshRate,
              m.timings.pixelClockHz, (unsigned)transform, (unsigned)ok, (int)rep->flipResult);
        if (!ok) break;
    }
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (!ok) {
        fHdrActive = hdrBefore;
        if (!fSubmissionFaulted) fKms->destroySurface(fDev, ns);
        if (fPendingScaled) {   // a scaled mode the head refused: remember how heavy, so validateDetailedTiming stops offering such modes
            const unsigned long long before = fScalerLimit;
            fScalerLimit = nvrmScalerLearn(fScalerLimit, fPendingLoad);
            if (fScalerLimit != before) FBLOG("fb%u: the head refused a scaled mode of load %llu pixels/s; scaled modes above %llu are no longer offered", fIndex, fPendingLoad, fScalerLimit);
        }
        return kIOReturnUnsupportedMode;
    }
    struct NvKmsKapiSurface *old = fSurf;
    fScan30 = ten;
    fSurf = ns; fFrontSurf = shown; fMode = m; fW = w; fH = h; fPitch = pitch;
    fRasterW = m.timings.hVisible; fRasterH = m.timings.vVisible; fCurId = id; fModeSet = true; fModeSwitches++;
    fCurShownKnown = false;   // the mode set named no cursor
    publishFlipWait();
    fHwVblTries = 0;   // a new mode gets its own hardware-vblank tries (fModeSet stays set, so vblFire would not reset them)
    fCurTransform = transform;
    if (old && old != ns) fKms->destroySurface(fDev, old);
    {
        const struct NvKmsKapiDisplayModeTimings &k5t = fMode.timings;
        UInt32 k5[13] = { 0x4b350001u, k5t.pixelClockHz, k5t.hVisible, k5t.hSyncStart, k5t.hSyncEnd, k5t.hTotal, k5t.vVisible, k5t.vSyncStart,
                          k5t.vSyncEnd, k5t.vTotal, k5t.refreshRate, k5t.flags.hSyncNeg ? 0u : 1u, k5t.flags.vSyncNeg ? 0u : 1u };
        OSData *k5d = OSData::withBytes(k5, sizeof k5);
        if (k5d) { setProperty("NVRMTiming", k5d); k5d->release(); }
    }
    setProperty("NVRMModeSwitches", (unsigned long long)fModeSwitches, 32);
    setProperty("NVRMRotation", (unsigned long long)transform, 32);
    publishStartupTiming();
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::validateDetailedTiming(void *description, IOByteCount descripSize)
{
    if (!description) return kIOReturnBadArgument;
    if (fProxy) return kIOReturnUnsupportedMode;
    IODetailedTimingInformationV2 *d = nullptr;
    if (descripSize == sizeof(IOFBDisplayModeDescription))
        d = &((IOFBDisplayModeDescription *)description)->timingInfo.detailedInfo.v2;
    else if (descripSize == sizeof(IODetailedTimingInformationV2))
        d = (IODetailedTimingInformationV2 *)description;
    fValidateCalls++;
    if (!d || !fKms || !fDev || !fDisplay) return kIOReturnBadArgument;
    const char *why = nullptr;
    if (d->horizontalScaledInset || d->verticalScaledInset) why = "inset (no kIOScaleCanSupportInset)";
    else if (d->scalerFlags & ~(UInt32)(kIOScaleStretchToFit | kIOScaleRotateFlags)) why = "unknown scaler flags";
    else if (d->horizontalBorderLeft | d->horizontalBorderRight | d->verticalBorderTop | d->verticalBorderBottom) why = "borders";
    else if (!d->pixelClock || !d->horizontalActive || !d->verticalActive) why = "empty timing";
    else if ((d->scalerFlags & kIOScaleRotateFlags) && !(d->horizontalScaled && d->verticalScaled)) why = "rotation without a desktop size";
    else if ((d->scalerFlags & kIOScaleRotateFlags) && fBootW && (d->horizontalActive < fBootW || d->verticalActive < fBootH))
        why = "rotated raster smaller than the boot raster";
    else if (d->horizontalScaled || d->verticalScaled) {
        NvU32 rw = 0, rh = 0; dtRasterTurned(d, &rw, &rh);
        if (!d->horizontalScaled || !d->verticalScaled) why = "one scaled axis";
        else if ((d->scalerFlags & kIOScaleRotateFlags) && !nvrmRotatedDesktopOk(gRotScale, d->horizontalScaled, d->verticalScaled, rw, rh)) why = "scaled rotated desktop";
        else if ((d->horizontalScaled > rw || d->verticalScaled > rh) && (d->horizontalActive != fBootW || d->verticalActive != fBootH))
            why = "desktop larger than a raster smaller than the panel";
        else if (d->horizontalActive > fScalerMaxW || d->verticalActive > fScalerMaxH) why = "raster larger than the scanout allocation";
        else if (fScalerLimit) {   // the head already refused a scaled mode this heavy
            const UInt64 tot = (UInt64)(d->horizontalActive + d->horizontalBlanking) * (d->verticalActive + d->verticalBlanking);
            const unsigned hz = tot ? (unsigned)((d->pixelClock + tot / 2) / tot) : 0;
            if (!nvrmScalerAllows(fScalerLimit, nvrmScalerLoad(d->horizontalActive, d->verticalActive, d->horizontalScaled, d->verticalScaled, hz)))
                why = "beyond the head scaler's pixel rate";
        }
    }
    if (!why) {
        struct NvKmsKapiDisplayMode m = {};
        if (!edidModeFor(d, &m)) {
            dtToKapi(d, &m);
            if (!fKms->validateDisplayMode(fDev, fDisplay, &m)) why = "NVKMS validateDisplayMode refused the raster (not an EDID mode)";
        }
    }
    if (fValidateCalls <= 48 || why == nullptr)
        FBLOGV("fb%u validateDetailedTiming #%u id 0x%x %ux%u%s%ux%u rotation 0x%x pclk %llu -> %s", fIndex, fValidateCalls, (unsigned)d->detailedTimingModeID,
              d->horizontalActive, d->verticalActive, d->horizontalScaled ? " desktop " : "", d->horizontalScaled, d->verticalScaled,
              (unsigned)(d->scalerFlags & kIOScaleRotateFlags), (unsigned long long)d->pixelClock, why ? why : "OK");
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
        info->flags |= kDisplayModeAcceleratorBackedFlag;
    }
    return kIOReturnSuccess;
}
// One non-committing trial of a scaled timing: the same config switchMode would test, with a throw-away surface. true = the head
// could scan it. Used by probeScalerLimit() (boot-arg nvfbscalerprobe=1).
bool NVRMNVDAFramebuffer::trialScaledMode(const IODetailedTimingInformationV2 &d)
{
    struct NvKmsKapiDisplayMode m = {};
    if (!edidModeFor(&d, &m)) dtToKapi(&d, &m);
    NvU32 w = 0, h = 0, pitch = 0; dtSurfaceSize(&d, &w, &h);
    if (!nvrmKmsRasterValid(w, h) || !nvrmCheckedPitch(w, fPitchAlign, &pitch) || (NvU64)pitch * h > fMemBytes) return false;
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
    sp.width = w; sp.height = h; sp.format = fScan30 ? NvKmsSurfaceMemoryFormatX2B10G10R10 : NvKmsSurfaceMemoryFormatX8R8G8B8;
    struct NvKmsKapiSurface *ns = fKms->createSurface(fDev, &sp);
    if (!ns) return false;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    NvBool ok = NV_FALSE;
    if (cfg && rep) {
        nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
        cfg->headsMask = 1u << fHead;
        struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
        nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
        hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
        hr->modeSetConfig.mode = m;
        hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
        struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
        lr->config.surface = ns; lr->config.srcWidth = (NvU16)w; lr->config.srcHeight = (NvU16)h;
        lr->config.dstWidth = (NvU16)m.timings.hVisible; lr->config.dstHeight = (NvU16)m.timings.vVisible;
        lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
        lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
        nvrmUnownedHeadsOff(cfg);
        ok = submitConfig(cfg, rep, NV_FALSE);
    }
    if (cfg) IOFree(cfg, sizeof *cfg);
    if (rep) IOFree(rep, sizeof *rep);
    if (!fSubmissionFaulted) fKms->destroySurface(fDev, ns);
    return ok != NV_FALSE;
}
// Find the head's scaler limit by binary search over the scaled timings' loads: about log2(n) trials instead of one refused mode
// set per timing. Only with the head lit and idle; the result is the same fScalerLimit the refusals teach.
void NVRMNVDAFramebuffer::probeScalerLimit()
{
    if (!fModeSet || !fSurf || fHeadSlept || fConsoleAperture || fSubmissionFaulted || !rmGpuReady() || !fKms || !fDev) return;
    static const unsigned kMax = 512;
    unsigned long long *loads = (unsigned long long *)IOMalloc(kMax * sizeof *loads);
    unsigned *idx = (unsigned *)IOMalloc(kMax * sizeof *idx);
    if (!loads || !idx) { if (loads) IOFree(loads, kMax * sizeof *loads); if (idx) IOFree(idx, kMax * sizeof *idx); return; }
    unsigned n = 0;
    for (unsigned i = 0; i < fNDT && n < kMax; i++) {
        const IODetailedTimingInformationV2 &d = fDT[i];
        if (!d.horizontalScaled || !d.verticalScaled || (d.scalerFlags & kIOScaleRotateFlags)) continue;
        const UInt64 tot = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
        const unsigned hz = tot ? (unsigned)((d.pixelClock + tot / 2) / tot) : 0;
        const unsigned long long load = nvrmScalerLoad(d.horizontalActive, d.verticalActive, d.horizontalScaled, d.verticalScaled, hz);
        // ascending insertion by load; the index remembers one timing that has that load
        unsigned j = n; while (j > 0 && loads[j - 1] > load) { loads[j] = loads[j - 1]; idx[j] = idx[j - 1]; j--; }
        loads[j] = load; idx[j] = i; n++;
    }
    if (n >= 2) {
        unsigned trials = 0;
        auto test = [&](unsigned long long load) {
            for (unsigned k = 0; k < n; k++) if (loads[k] == load) return trialScaledMode(fDT[idx[k]]);
            return false;
        };
        const unsigned long long limit = nvrmScalerSearch(loads, n, test, &trials);
        if (limit != ~0ull) fScalerLimit = nvrmScalerLearn(0, limit + 1);
        FBLOG("fb%u: scaler probe: %u scaled timings, %u trial(s) -> %s", fIndex, n, trials,
              limit == ~0ull ? "no limit" : "limit found");
        if (limit != ~0ull) FBLOG("fb%u: scaled modes above %llu pixels/s are not offered", fIndex, fScalerLimit);
    }
    IOFree(loads, kMax * sizeof *loads); IOFree(idx, kMax * sizeof *idx);
}
// Surround planning (master only): the members' valid modes, one common height and refresh rate, one head each.
bool NVRMNVDAFramebuffer::planSurround(unsigned masterIndex)
{
    unsigned idx[kNvrmSurMaxMembers]; const unsigned n = nvrmSurroundMembers(gSurroundMask, (unsigned)gNDpy, idx);
    fSurN = 0;
    if (n < 2 || idx[0] != masterIndex || !fKms || !fDev) return false;
    static NvrmSurMode modes[kNvrmSurMaxMembers][kNvrmSurMaxModes]; static struct NvKmsKapiDisplayMode kmodes[kNvrmSurMaxMembers][kNvrmSurMaxModes];
    unsigned count[kNvrmSurMaxMembers] = {};
    for (unsigned k = 0; k < n; k++) {
        const NvKmsKapiDisplay h = gDpy[idx[k]].handle;
        for (NvU32 i = 0; i < 256 && count[k] < kNvrmSurMaxModes; i++) {
            struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, pref = NV_FALSE;
            const int r = fKms->getDisplayMode(fDev, h, i, &m, &valid, &pref);
            if (r < 0) break;
            const struct NvKmsKapiDisplayModeTimings &t = m.timings;
            if (r == 0 || !valid || t.flags.interlaced || t.flags.doubleScan || !t.pixelClockHz || !t.hTotal || !t.vTotal) continue;
            const NvU64 tot = (NvU64)t.hTotal * t.vTotal;
            modes[k][count[k]] = { t.hVisible, t.vVisible, (unsigned)((t.pixelClockHz + tot / 2) / tot) };
            kmodes[k][count[k]] = m; count[k]++;
        }
    }
    unsigned pick[kNvrmSurMaxMembers], total = 0, height = 0, hz = 0;
    if (!nvrmSurroundPlan(modes, count, n, pick, &total, &height, &hz)) { FBLOG("surround: the %u displays share no height and refresh rate -- not grouped", n); return false; }
    NvU32 x = 0;
    for (unsigned k = 0; k < n; k++) {   // each member's head is the one its own (hidden) framebuffer took
        const struct nvrm_dpy_rec &r = gDpy[idx[k]];
        NVRMNVDAFramebuffer *mf = k ? gFBs[idx[k]] : this;
        if (!mf || mf->fDisplay != r.handle || mf->fDpyLost) { FBLOG("surround: member index %u is not up yet -- not grouped", idx[k]); return false; }
        fSur[k].display = r.handle; fSur[k].head = mf->fHead; fSur[k].mode = kmodes[k][pick[k]]; fSur[k].x = x; fSur[k].w = modes[k][pick[k]].w;
        x += fSur[k].w;
    }
    fSurN = n; fSurTotalW = total; fSurH = height; fSurHz = hz;
    FBLOG("surround: %u displays as one %ux%u @%u Hz display", n, total, height, hz);
    return true;
}
// The one mode a surround group offers, in place of the master's own: the combined raster. IDs 0x40000 | refresh rate.
unsigned NVRMNVDAFramebuffer::addSurroundMode(unsigned n)
{
    if (!fSurN && gSurroundMask && !fHidden) (void)planSurround(fIndex);   // planned when the list is built: every member is up by now, or the master offers its own modes
    if (!fSurN) return n;
    const struct NvKmsKapiDisplayModeTimings &k = fSur[0].mode.timings;
    IODetailedTimingInformationV2 b; nvu_zero(&b, sizeof b);
    b.horizontalActive = fSurTotalW; b.verticalActive = fSurH; b.signalConfig = kIODigitalSignal;
    b.horizontalBlanking = k.hTotal - k.hVisible; b.horizontalSyncOffset = k.hSyncStart - k.hVisible; b.horizontalSyncPulseWidth = k.hSyncEnd - k.hSyncStart;
    b.verticalBlanking = k.vTotal - k.vVisible; b.verticalSyncOffset = k.vSyncStart - k.vVisible; b.verticalSyncPulseWidth = k.vSyncEnd - k.vSyncStart;
    // a pixel clock for the combined raster, only so the numbers add up for macOS (each head runs its own timing)
    const UInt64 tot = (UInt64)(b.horizontalActive + b.horizontalBlanking) * (b.verticalActive + b.verticalBlanking);
    b.pixelClock = b.minPixelClock = b.maxPixelClock = tot * fSurHz;
    b.detailedTimingModeID = 0x40000u | fSurHz;
    fDT[0] = b;
    FBLOG("fb%u: surround mode %ux%u @%u Hz offered (dt 0x%x)", fIndex, fSurTotalW, fSurH, fSurHz, b.detailedTimingModeID);
    return 1;
}
// Copy the master's head config to every other member, each with its display, its mode and its slice of the shared surface.
void NVRMNVDAFramebuffer::surroundReplicate(struct NvKmsKapiRequestedModeSetConfig *cfg)
{
    if (!fSurActive || !(cfg->headsMask & (1u << fHead))) return;
    const struct NvKmsKapiHeadRequestedConfig master = cfg->headRequestedConfig[fHead];
    for (unsigned k = 0; k < fSurN; k++) {
        const SurMember &m = fSur[k];
        struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[m.head];
        if (k) {
            if (cfg->headsMask & (1u << m.head)) continue;           // the caller already filled this head in
            *hr = master; cfg->headsMask |= 1u << m.head;
            hr->modeSetConfig.displays[0] = m.display; hr->modeSetConfig.mode = m.mode;
            hr->cursorRequestedConfig.surface = NULL; hr->cursorRequestedConfig.flags.surfaceChanged = NV_FALSE; hr->cursorRequestedConfig.flags.dstXYChanged = NV_FALSE;
        }
        struct NvKmsKapiLayerConfig &lc = hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].config;
        if (lc.surface && lc.srcWidth == fSurTotalW) {
            lc.srcX = (NvU16)m.x; lc.srcWidth = (NvU16)m.w; lc.dstWidth = (NvU16)m.mode.timings.hVisible; lc.dstHeight = (NvU16)m.mode.timings.vVisible;
            hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].flags.srcXYChanged = hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].flags.srcWHChanged =
                hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX].flags.dstWHChanged = NV_TRUE;
        }
    }
}
// Light every member head with its slice of one surface as wide as the group (tiled-display style).
IOReturn NVRMNVDAFramebuffer::switchSurround(IODisplayModeID id)
{
    if (!fSurN) return kIOReturnUnsupportedMode;
    NvU32 pitch = 0; const NvU32 w = fSurTotalW, h = fSurH;
    if (!nvrmCheckedPitch(w, fPitchAlign, &pitch) || (NvU64)pitch * h > fMemBytes) {
        FBLOG("surround 0x%x: %ux%u needs %llu B, the scanout holds %llu -- REFUSED", (unsigned)id, w, h, (unsigned long long)pitch * h, (unsigned long long)fMemBytes);
        return kIOReturnNoMemory;
    }
    struct NvKmsKapiCreateSurfaceParams sp = {};
    sp.planes[0].memory = fMem; sp.planes[0].offset = 0; sp.planes[0].pitch = pitch;
    sp.width = w; sp.height = h; sp.format = NvKmsSurfaceMemoryFormatX8R8G8B8;
    struct NvKmsKapiSurface *ns = fKms->createSurface(fDev, &sp);
    if (!ns) return kIOReturnNoMemory;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); fKms->destroySurface(fDev, ns); return kIOReturnNoMemory; }
    NvBool ok = NV_FALSE;
    for (int commit = 0; commit < 2; commit++) {
        nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
        for (unsigned k = 0; k < fSurN; k++) {
            const SurMember &m = fSur[k];
            cfg->headsMask |= 1u << m.head;
            struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[m.head];
            nvrmHeadConfigBaseline(hr, fGammaOn && fGamma ? olutFromGamma() : NULL);
            hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = m.display;
            hr->modeSetConfig.mode = m.mode;
            hr->flags.activeChanged = hr->flags.displaysChanged = hr->flags.modeChanged = NV_TRUE;
            struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
            lr->config.surface = ns; lr->config.srcX = (NvU16)m.x; lr->config.srcY = 0; lr->config.srcWidth = (NvU16)m.w; lr->config.srcHeight = (NvU16)h;
            lr->config.dstWidth = (NvU16)m.mode.timings.hVisible; lr->config.dstHeight = (NvU16)m.mode.timings.vVisible;
            lr->config.compParams.compMode = NVKMS_COMPOSITION_BLENDING_MODE_OPAQUE; lr->config.minPresentInterval = 1;
            lr->flags.surfaceChanged = lr->flags.srcXYChanged = lr->flags.srcWHChanged = lr->flags.dstXYChanged = lr->flags.dstWHChanged = NV_TRUE;
        }
        nvrmUnownedHeadsOff(cfg);
        ok = submitConfig(cfg, rep, commit ? NV_TRUE : NV_FALSE);
        FBLOG("surround 0x%x: %s %ux%u over %u heads -> %u (flipResult %d)", (unsigned)id, commit ? "COMMIT" : "test", w, h, fSurN, (unsigned)ok, (int)rep->flipResult);
        if (!ok) break;
    }
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (!ok) { if (!fSubmissionFaulted) fKms->destroySurface(fDev, ns); return kIOReturnUnsupportedMode; }
    struct NvKmsKapiSurface *old = fSurf;
    fSurf = ns; fFrontSurf = ns; fSurActive = true; fMode = fSur[0].mode; fW = w; fH = h; fPitch = pitch;
    fRasterW = w; fRasterH = h; fCurId = id; fModeSet = true; fModeSwitches++; fCurShownKnown = false;
    publishFlipWait(); fHwVblTries = 0; fCurTransform = 0;
    if (old && old != ns) fKms->destroySurface(fDev, old);
    return kIOReturnSuccess;
}
// nvfbrotscale=1: while the head is rotated, turned desktops smaller than the raster next to each native one (see
// nvrmRotatedDesktop). IDs 0x30000 | factor << 12 | refresh rate.
unsigned NVRMNVDAFramebuffer::addRotatedDesktops(unsigned n)
{
    const unsigned cap = sizeof fDT / sizeof fDT[0], base = n;
    if (!gRotScale || !fCurTransform) return n;
    for (unsigned s = 0; s < base; s++) {
        const IODetailedTimingInformationV2 src = fDT[s];
        if ((src.scalerFlags & kIOScaleRotateFlags) != fCurTransform || !src.horizontalScaled || !src.pixelClock) continue;
        NvU32 rw = 0, rh = 0; dtRasterTurned(&src, &rw, &rh);
        if (src.horizontalScaled != rw || src.verticalScaled != rh) continue;   // only the native-desktop entries seed new ones
        const UInt64 tot = (UInt64)(src.horizontalActive + src.horizontalBlanking) * (src.verticalActive + src.verticalBlanking);
        const unsigned hz = tot ? (unsigned)((src.pixelClock + tot / 2) / tot) : 0;
        if (!hz || hz > 0xfff) continue;
        for (unsigned f = 0; f < kNvrmRotFactors && n < cap; f++) {
            unsigned dw = 0, dh = 0;
            if (!nvrmRotatedDesktop(rw, rh, f, &dw, &dh)) continue;
            bool listed = false;
            for (unsigned j = 0; j < n && !listed; j++)
                listed = fDT[j].horizontalActive == src.horizontalActive && fDT[j].verticalActive == src.verticalActive &&
                         fDT[j].horizontalScaled == dw && fDT[j].verticalScaled == dh && fDT[j].pixelClock == src.pixelClock;
            if (listed) continue;
            IODetailedTimingInformationV2 b = src;
            b.horizontalScaled = dw; b.verticalScaled = dh;
            b.detailedTimingModeID = 0x30000u | (f << 12) | hz;
            fDT[n++] = b;
        }
    }
    if (n != base) FBLOG("fb%u: %u rotated desktop(s) of other sizes added (nvfbrotscale=1)", fIndex, n - base);
    return n;
}
// DSR-style supersampled desktops (boot-arg nvfbdsr=1, which also reserves the scanout memory nvfbhidpi does): for every refresh
// rate the panel runs at its native raster, desktops of 1.20x to 4.00x the pixel count that the head scales down. Only sizes
// that fit the scaler cap and the scanout memory, and only where macOS did not already list that desktop at that rate. IDs are
// 0x20000 | factor << 12 | refresh rate, below Apple's reserved 0x80000000 range and stable across calls.
unsigned NVRMNVDAFramebuffer::addDsrModes(unsigned n)
{
    const unsigned cap = sizeof fDT / sizeof fDT[0], base = n;
    if (!fBootW || !fBootH) return n;
    for (unsigned s = 0; s < base; s++) {
        const IODetailedTimingInformationV2 src = fDT[s];
        if (src.horizontalActive != fBootW || src.verticalActive != fBootH || src.horizontalScaled || src.verticalScaled || !src.pixelClock) continue;
        const UInt64 tot = (UInt64)(src.horizontalActive + src.horizontalBlanking) * (src.verticalActive + src.verticalBlanking);
        const unsigned hz = tot ? (unsigned)((src.pixelClock + tot / 2) / tot) : 0;
        if (!hz || hz > 0xfff) continue;
        UInt32 flags = 0;   // scaler flags macOS used for its own scaled modes on this raster, if it has any
        for (unsigned j = 0; j < base; j++)
            if (fDT[j].horizontalActive == fBootW && fDT[j].verticalActive == fBootH && fDT[j].horizontalScaled) { flags = fDT[j].scalerFlags; break; }
        for (unsigned f = 0; f < kNvrmDsrFactors && n < cap; f++) {
            unsigned dw = 0, dh = 0;
            if (!nvrmDsrSize(fBootW, fBootH, f, fScalerMaxW, fScalerMaxH, &dw, &dh)) continue;
            NvU32 pitch = 0;
            if (!nvrmCheckedPitch(fBootW, fPitchAlign, &pitch) || !fMemBytes || (NvU64)pitch * fBootH > fMemBytes) continue;
            bool listed = false;
            for (unsigned j = 0; j < n && !listed; j++) {
                const IODetailedTimingInformationV2 &d = fDT[j];
                if (d.horizontalActive != fBootW || d.verticalActive != fBootH || d.horizontalScaled != dw || d.verticalScaled != dh) continue;
                const UInt64 dt = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
                const unsigned dhz = dt ? (unsigned)((d.pixelClock + dt / 2) / dt) : 0;
                if (dhz + 1 >= hz && dhz <= hz + 1) listed = true;
            }
            if (listed) continue;
            IODetailedTimingInformationV2 b = src;
            b.horizontalScaled = dw; b.verticalScaled = dh; b.scalerFlags = flags;
            b.detailedTimingModeID = 0x20000u | (f << 12) | hz;
            fDT[n++] = b;
        }
    }
    if (n != base) FBLOG("fb%u: %u supersampled desktop(s) added (nvfbdsr=1)", fIndex, n - base);
    return n;
}
IOReturn NVRMNVDAFramebuffer::setDetailedTimings(OSArray *array)
{
    if (fProxy) return kIOReturnSuccess;
    if (!array) {
        fNDT = 0; fBootInDT = false; fNewIdOnce = 0; removeProperty(kIOFBDetailedTimingsKey);
        if (fCurId != kModeId) FBLOG("setDetailedTimings(NULL) while 0x%x is on the head -- it stays until the next setDisplayMode", (unsigned)fCurId);
        return kIOReturnSuccess;
    }
    unsigned n = 0, bad = 0;
    // More timings than fit: keep the highest refresh rates (and the mode on the head), not the first ones in
    // arrival order, which dropped every 144-240 Hz scaled mode on panels macOS lists 500+ timings for.
    const unsigned offered = array->getCount();
    unsigned char *keep = nullptr; unsigned *rate = nullptr; unsigned char *must = nullptr;
    if (offered > kMaxMacTimings) {
        keep = (unsigned char *)IOMalloc(offered); must = (unsigned char *)IOMalloc(offered);
        rate = (unsigned *)IOMalloc(offered * sizeof(unsigned));
        if (keep && must && rate) {
            for (unsigned i = 0; i < offered; i++) {
                rate[i] = 0; must[i] = 0;
                OSData *data = OSDynamicCast(OSData, array->getObject(i));
                if (!data || data->getLength() < sizeof(IODetailedTimingInformationV2)) continue;
                IODetailedTimingInformationV2 t; nvu_copy(&t, data->getBytesNoCopy(), sizeof t);
                const UInt64 tot = (UInt64)(t.horizontalActive + t.horizontalBlanking) * (t.verticalActive + t.verticalBlanking);
                rate[i] = tot ? (unsigned)((t.pixelClock + tot / 2) / tot) : 0;
                must[i] = dtIsCurrent(t) || dtIsBootRaster(t);
            }
            nvrmPickTimings(rate, must, offered, kMaxMacTimings, keep);
        }
        FBLOG("fb%u setDetailedTimings: %u timings from macOS, %u kept (highest refresh rates first)", fIndex, offered, kMaxMacTimings);
    }
    for (unsigned i = 0; i < offered && n < kMaxMacTimings; i++) {
        if (keep && rate && must && !keep[i]) continue;
        OSData *data = OSDynamicCast(OSData, array->getObject(i));
        if (!data || data->getLength() < sizeof(IODetailedTimingInformationV2)) { bad++; continue; }
        nvu_copy(&fDT[n++], data->getBytesNoCopy(), sizeof(IODetailedTimingInformationV2));
    }
    if (keep) IOFree(keep, offered);
    if (must) IOFree(must, offered);
    if (rate) IOFree(rate, offered * sizeof(unsigned));
    const unsigned fromMac = n;
    publishRotatedSizes();
    fNewIdOnce = 0;
    n = addKmsRefreshModes(n);
    { UInt32 dsr = 0; if (PE_parse_boot_argn("nvfbdsr", &dsr, sizeof dsr) && dsr) n = addDsrModes(n); }
    n = addRotatedDesktops(n);
    if (gSurroundMask && !fHidden) { const unsigned s = addSurroundMode(0); if (s) n = s; }   // a surround group offers its combined mode only
    fNDT = n;
    setProperty(kIOFBDetailedTimingsKey, array);
    fBootInDT = false;
    if (fCurId != kModeId && fModeSet && (dtIndex(fCurId) < 0 || !dtIsCurrent(fDT[dtIndex(fCurId)]))) {
        int found = -1;
        for (unsigned i = 0; i < n && found < 0; i++) if (dtIsCurrent(fDT[i])) found = (int)i;
        if (found >= 0 && fCurTransform) {
            fNewIdOnce = fDT[found].detailedTimingModeID;
            FBLOG("fb%u setDetailedTimings: rotated mode on the head is dt 0x%x now; 0x%x kept, the new ID answered once",
                  fIndex, (unsigned)fNewIdOnce, (unsigned)fCurId);
            found = -1;
        }
        if (found >= 0) {
            FBLOG("fb%u setDetailedTimings: the mode on the head (raster %ux%u, desktop %ux%u, rotation 0x%x) is dt 0x%x now, was 0x%x",
                  fIndex, fRasterW, fRasterH, fCurDT.horizontalScaled, fCurDT.verticalScaled, (unsigned)fCurTransform,
                  fDT[found].detailedTimingModeID, (unsigned)fCurId);
            fCurId = (IODisplayModeID)fDT[found].detailedTimingModeID;
        } else if (n < sizeof fDT / sizeof fDT[0] && fCurDT.pixelClock) {
            fDT[n] = fCurDT; fDT[n].detailedTimingModeID = (UInt32)fCurId; n++; fNDT = n;
            FBLOG("fb%u setDetailedTimings: the mode on the head (dt 0x%x) is not in the new list; kept as an alias", fIndex, (unsigned)fCurId);
        }
    }
    if (fCurId == kModeId || dtIndex(fCurId) < 0 || !dtIsCurrent(fDT[dtIndex(fCurId)])) {
        for (unsigned i = 0; i < n; i++) {
            const IODetailedTimingInformationV2 &d = fDT[i];
            if (!dtIsBootRaster(d)) continue;
            FBLOG("setDetailedTimings: boot raster %ux%u@%u is dt 0x%x -- that is the current mode now (no modeset)",
                  fBootW, fBootH, (fBootMode.timings.refreshRate + 500) / 1000, d.detailedTimingModeID);
            fCurId = (IODisplayModeID)d.detailedTimingModeID; fBootInDT = true;
            break;
        }
    } else fBootInDT = true;
    FBLOG("fb%u setDetailedTimings: %u timing(s) installed (%u from macOS, %u refresh rates from NVKMS; %u malformed, %u offered), current 0x%x, EDID matches so far %u",
          fIndex, n, fromMac, n - fromMac, bad, array->getCount(), (unsigned)fCurId, fEdidHits);
    for (unsigned i = 0; i < n && i < 24; i++)
        FBLOGV("  dt 0x%x raster %ux%u desktop %ux%u rotation 0x%x pclk %llu", fDT[i].detailedTimingModeID, fDT[i].horizontalActive, fDT[i].verticalActive,
              fDT[i].horizontalScaled, fDT[i].verticalScaled, (unsigned)(fDT[i].scalerFlags & kIOScaleRotateFlags), (unsigned long long)fDT[i].pixelClock);
    setProperty("NVRMDetailedTimings", (unsigned long long)n, 32);
    {   // optional active probe of the head scaler (boot-arg nvfbscalerprobe=1), once per head
        UInt32 pr = 0; static bool probed[16];
        if (!fScalerLimit && fIndex < 16 && !probed[fIndex] && PE_parse_boot_argn("nvfbscalerprobe", &pr, sizeof pr) && pr) {
            probed[fIndex] = true; probeScalerLimit();
        }
    }
    publishStartupTiming();
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::publishTimingCaps()
{
    IODisplayScalerInformation si; nvu_zero(&si, sizeof si);
    si.scalerFeatures = kIOScaleCanUpSamplePixels | kIOScaleCanDownSamplePixels | kIOScaleCanRotate;
    const NvU32 maxPx = fScalerMaxW > fScalerMaxH ? fScalerMaxW : fScalerMaxH;
    si.maxHorizontalPixels = maxPx; si.maxVerticalPixels = maxPx;
    OSData *s = OSData::withBytes(&si, sizeof si);
    if (s) { setProperty(kIOFBScalerInfoKey, s); s->release(); }
    IODisplayTimingRangeV1 r; nvu_zero(&r, sizeof r);
    r.minPixelClock = 25000000ull; r.maxPixelClock = (UInt64)kNvrmMaxPixelClockMHz * 1000000ull; r.maxPixelError = 0;
    r.supportedSyncFlags = kIORangeSupportsSeparateSyncs | (fVrrSupported && fVrrMinHz ? kIORangeSupportsVRR : 0);
    r.supportedSignalLevels = 0; r.supportedSignalConfigs = kIODigitalSignal;
    r.minFrameRate = 23; r.maxFrameRate = kNvrmMaxFrameRate; r.minLineRate = 10000; r.maxLineRate = 1000000;
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
    if (gHdrOn && fHdrCapable) {   // a PQ-capable display: HDR10 timings in BT.2020/BT.2100 are offered next to the SDR ones
        r.supportedColorimetryModes |= kIORangeColorimetryBT2020 | kIORangeColorimetryBT2100;
        r.supportedDynamicRangeModes |= kIORangeDynamicRangeHDR10;
    }
    OSData *g = OSData::withBytes(&r, sizeof r);
    if (g) { setProperty(kIOFBTimingRangeKey, g); g->release(); }
    setProperty(kIOFBProbeOptionsKey, (unsigned long long)kNVRMFBSetTransform, 32);
    FBLOG("fbmodes: published IOFBScalerInfo (up+down+rotate, max %ux%u) + IOFBTimingRange (25 MHz..%u MHz, 23..%u Hz) + IOFBProbeOptions 0x%x",
          fScalerMaxW, fScalerMaxH, (unsigned)kNvrmMaxPixelClockMHz, (unsigned)kNvrmMaxFrameRate, (unsigned)kNVRMFBSetTransform);
}
IOReturn NVRMNVDAFramebuffer::getInformationForDisplayMode(IODisplayModeID mode, IODisplayModeInformation *info)
{
    if (!info) return kIOReturnBadArgument;
    if (fProxy) {
        if (mode != kModeId) return kIOReturnBadArgument;
        nvu_zero(info, sizeof *info);
        info->nominalWidth = fPxW; info->nominalHeight = fPxH; info->refreshRate = 60u << 16; info->maxDepthIndex = 0;
        info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag;
        return kIOReturnSuccess;
    }
    if (mode != kModeId) {
        int i = dtIndex(mode); if (i < 0) return kIOReturnBadArgument;
        const IODetailedTimingInformationV2 &d = fDT[i];
        const UInt64 tot = (UInt64)(d.horizontalActive + d.horizontalBlanking) * (d.verticalActive + d.verticalBlanking);
        nvu_zero(info, sizeof *info);
        info->nominalWidth = (d.horizontalScaled && d.verticalScaled) ? d.horizontalScaled : d.horizontalActive;
        info->nominalHeight = (d.horizontalScaled && d.verticalScaled) ? d.verticalScaled : d.verticalActive;
        NvU32 rw = 0, rh = 0; dtRasterTurned(&d, &rw, &rh);
        const bool scaled = d.horizontalScaled && d.verticalScaled && !(d.horizontalScaled == rw && d.verticalScaled == rh);
        info->refreshRate = tot ? (UInt32)((d.pixelClock << 16) / tot) : (60u << 16);
        info->maxDepthIndex = 0;
        // without ValidForHiRes, scaled modes stay out of the public mode list and HiDPI "looks like" sizes never show (#354)
        info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeAcceleratorBackedFlag
            | (scaled ? kDisplayModeNotPresetFlag | kDisplayModeValidForHiResFlag : 0);
        if (!scaled && d.horizontalActive == fBootW && d.verticalActive == fBootH)
            info->flags |= (fTransform ? 0 : kDisplayModeNativeFlag) | kDisplayModeDefaultFlag;
        info->imageWidth = fEdidMmW; info->imageHeight = fEdidMmH;
        return kIOReturnSuccess;
    }
    nvu_zero(info, sizeof *info);
    info->nominalWidth = fBootW ? fBootW : fW; info->nominalHeight = fBootH ? fBootH : fH; info->maxDepthIndex = 0;
    const NvU32 rr = fBootW ? fBootMode.timings.refreshRate : fMode.timings.refreshRate;
    info->refreshRate = rr ? (UInt32)(((UInt64)rr << 16) / 1000) : (60u << 16);
    info->flags = kDisplayModeValidFlag | kDisplayModeSafeFlag | kDisplayModeDefaultFlag | kDisplayModeAcceleratorBackedFlag;
    info->imageWidth = fEdidMmW; info->imageHeight = fEdidMmH;
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getTimingInfoForDisplayMode(IODisplayModeID mode, IOTimingInformation *info)
{
    if (!info) return kIOReturnUnsupportedMode;
    const bool swapped = (fTransform & kIOScaleSwapAxes) != 0;
    if (mode != kModeId) {
        int i = dtIndex(mode); if (i < 0) return kIOReturnUnsupportedMode;
        nvu_zero(info, sizeof *info);
        info->appleTimingID = kIOTimingIDInvalid;
        info->detailedInfo.v2 = fDT[i];
        if (fVrrSupported && fVrrMinHz) {
            const IODetailedTimingInformationV2 &t = fDT[i];
            const UInt64 tot = (UInt64)(t.horizontalActive + t.horizontalBlanking) * (t.verticalActive + t.verticalBlanking);
            const unsigned hz = tot ? (unsigned)((t.pixelClock + tot / 2) / tot) : 0;
            info->detailedInfo.v2.verticalBlankingExtension = nvrmVrrBlankExtension(t.verticalActive + t.verticalBlanking, hz, fVrrMinHz, fVrrMaxHz);
        }
        IODetailedTimingInformationV2 &d = info->detailedInfo.v2;
        if (swapped && !(d.detailedTimingModeID & 0x80000000u) && !d.horizontalScaled && !d.verticalScaled) {
            d.horizontalScaled = d.horizontalActive; d.verticalScaled = d.verticalActive;
        }
        info->flags = kIODetailedTimingValid | ((d.horizontalScaled && d.verticalScaled) ? kIOScalingInfoValid : 0);
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
            if (swapped) { d.horizontalScaled = d.horizontalActive; d.verticalScaled = d.verticalActive; info->flags |= kIOScalingInfoValid; }
            FBLOGV("getTimingInfoForDisplayMode(%d) -> detailed %ux%u pclk %u Hz total %ux%u (%u mHz)", (int)mode,
                  t.hVisible, t.vVisible, t.pixelClockHz, t.hTotal, t.vTotal, t.refreshRate);
            return kIOReturnSuccess;
        }
    }
    FBLOGV("getTimingInfoForDisplayMode(%d) -> success", (int)mode);
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::getPixelInformation(IODisplayModeID mode, IOIndex depth, IOPixelAperture aperture, IOPixelInformation *pi)
{
    if (depth != 0 || aperture != kIOFBSystemAperture || !pi) return kIOReturnUnsupportedMode;
    if (fProxy) {
        if (mode != kModeId) return kIOReturnUnsupportedMode;
        nvu_zero(pi, sizeof *pi);
        pi->bytesPerRow = fPxPitch; pi->activeWidth = fPxW; pi->activeHeight = fPxH;
        pi->bitsPerPixel = 32; pi->pixelType = kIORGBDirectPixels; pi->componentCount = 3; pi->bitsPerComponent = 8;
        pi->componentMasks[0] = 0x00ff0000; pi->componentMasks[1] = 0x0000ff00; pi->componentMasks[2] = 0x000000ff;
        strlcpy(pi->pixelFormat, IO32BitDirectPixels, sizeof pi->pixelFormat);
        return kIOReturnSuccess;
    }
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
    FBLOGV("getPixelInformation 0x%x ap %d -> %ux%u bytesPerRow %u", (unsigned)mode, (int)aperture, pw, ph, pp);
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
#include "nvrm-vram-class.h"

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
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch, CTLFLAG_RW | CTLFLAG_LOCKED, &gFlipLatch, 0, "1 = a committed flip waits for its completion before a surface retires (0 = debug bypass, 2 = deferred: the next submit verifies it)");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_auto, CTLFLAG_RW | CTLFLAG_LOCKED, &gFlipLatchAuto, 0, "1 = at 357 Hz and above latch mode 1 runs as 2 (deferred); 0 = always wait");
SYSCTL_INT(_debug, OID_AUTO, nvrmfb_flip_latch_deferred, CTLFLAG_RD | CTLFLAG_LOCKED, &gFlipLatchDeferred, 0, "flips verified by the next submit instead of waited for");
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
void nvrmfbColorChanged(bool needsModeset);
static int nvrmfbColorSysctl SYSCTL_HANDLER_ARGS
{
    int v = *(int *)arg1;
    int err = sysctl_handle_int(oidp, &v, 0, req);
    if (err || req->newptr == 0) return err;
    const int which = arg2;   // 0 bpc, 1 format, 2 range, 3 vibrance
    if ((which == 0 && !(v >= 0 && nvrmColorBpcValid((unsigned)v))) || (which == 1 && !(v >= 0 && nvrmColorFormatValid((unsigned)v))) ||
        (which == 2 && !(v >= 0 && nvrmColorRangeValid((unsigned)v))) || (which == 3 && (v < -100 || v > 100))) return EINVAL;
    if (v == *(int *)arg1) return 0;
    *(int *)arg1 = v;
    nvrmfbColorChanged(which != 3);
    return 0;
}
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_color_bpc, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, &gColorBpc, 0, nvrmfbColorSysctl, "I", "output bits per channel: 0 automatic, 6, 8, 10, 12");
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_color_format, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, &gColorFormat, 1, nvrmfbColorSysctl, "I", "output colour format: 0 RGB, 1 YCbCr 4:2:2, 2 YCbCr 4:4:4, 3 automatic");
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_color_range, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, &gColorRange, 2, nvrmfbColorSysctl, "I", "output range: 0 automatic, 1 full, 2 limited");
SYSCTL_PROC(_debug, OID_AUTO, nvrmfb_color_vibrance, CTLTYPE_INT | CTLFLAG_RW | CTLFLAG_LOCKED, &gVibrance, 3, nvrmfbColorSysctl, "I", "saturation gain in percent, -100 (grey) to 100 (double); 0 off");
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
    sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_late); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_deferred); sysctl_register_oid(&sysctl__debug_nvrmfb_flip_latch_auto);
    sysctl_register_oid(&sysctl__debug_nvrmfb_color_bpc); sysctl_register_oid(&sysctl__debug_nvrmfb_color_format);
    sysctl_register_oid(&sysctl__debug_nvrmfb_color_range); sysctl_register_oid(&sysctl__debug_nvrmfb_color_vibrance);
}

bool NVRMNVDAFramebuffer::vramGrant(struct NVRMVramRequest *r)
{
    NVRMDisplayGate gate; if (!gate.valid()) return false;
    nvrmfbInstallSysctl();
    r->phys = 0; r->kva = nullptr; r->handle = nullptr; r->actualSize = 0; r->mappedTotal = 0;
    if (!fKms || !fDev) { FBLOG("nvAllocVram: no NVKMS device yet"); return false; }
    NvU64 want = (r->size + 0xFFFFull) & ~0xFFFFull;
    if (!want) return false;
    if (fBarLen && fBarLen <= (512ull << 20)) want = nvrmVramSizeClass(want);   // few hole sizes in a small window (see nvrm-vram-class.h)

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
        FBLOG("nvAllocVram(%llu): kva %s phys <phys> WROTE 0x5A5AA5A5C0FFEE01 AND READ 0x%llx — "
              "the aperture is not live; refusing to hand this out",
              (unsigned long long)want, kva ? "set" : "null", (unsigned long long)back);
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
        FBLOGV("nvAllocVram #%u: %llu bytes REAL VRAM — kva %s phys <phys>  [BAR1 %llu / %llu]",
              fVramGrants, (unsigned long long)want, kva ? "set" : "null",
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
    if (fProxy && functionName && (functionName->isEqualTo("nvFlipToSurfacePure") || functionName->isEqualTo(NVRM_FLIP_FN)))
        return kIOReturnUnsupported;   // nothing to flip: the panel belongs to the integrated GPU
    if (functionName && functionName->isEqualTo(NVRM_VRAM_FN_SCANOUT)) {
        struct NVRMVramRequest *r = (struct NVRMVramRequest *)param1;
        if (!r || r->version != NVRM_VRAM_ABI_VERSION) return kIOReturnBadArgument;
        if (fProxy) {   // CPU-only scanout (phys 0): the accelerator copies each frame into kva
            if (!fPxMap && !proxyMap()) return kIOReturnNotReady;
            r->phys = 0; r->kva = (void *)fPxMap->getVirtualAddress(); r->handle = nullptr;
            r->actualSize = fPxBytes; r->width = fPxW; r->height = fPxH; r->pitch = fPxPitch;
            return kIOReturnSuccess;
        }
        if (!fPhys || !fKva || !fPitch || !fH) { FBLOG("nvScanoutInfo: no scanout yet"); return kIOReturnNotReady; }
        r->phys       = fPhys;
        r->kva        = fKva;
        r->actualSize = (unsigned long long)fPitch * fH;
        r->width = fW; r->height = fH; r->pitch = fPitch;
        r->handle = fMem;
        r->flags = fScan30 ? NVRM_VRAM_SCANOUT_30BIT : 0;
        // Asked once per frame: logging every answer was 92% of a 512 KiB Send-logs kernel log (1401 issue #16), about
        // 67 s of history. Log only when the answer changes.
        if (fPhys != fScanLogPhys || fW != fScanLogW || fH != fScanLogH || fPitch != fScanLogPitch) {
            fScanLogPhys = fPhys; fScanLogW = fW; fScanLogH = fH; fScanLogPitch = fPitch;
            FBLOGV("nvScanoutInfo -> phys 0x%llx %ux%u pitch %u (%llu bytes)",
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
        FBLOGV("nvRmHandles -> kapiDevice %s", fDev ? "set" : "null");
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
        return ok ? kIOReturnSuccess : rc == (int)NV_KMS_FLIP_RESULT_IN_PROGRESS ? kIOReturnBusy : (fHeadSlept || !rmGpuReady()) ? kIOReturnNotReady : kIOReturnIOError;
    }
    if (functionName && functionName->isEqualTo(NVRM_FLIP_FN)) {
        struct NVRMFlipRequest *f = (struct NVRMFlipRequest *)param1;
        if (!f || f->version != NVRM_FLIP_ABI_VERSION) return kIOReturnBadArgument;
        int rc = -1;
        bool ok = flipToMemory((struct NvKmsKapiMemory *)f->kapiMemory,
                               f->width, f->height, f->pitch, &rc, false, f->allocationCookie);
        f->flipResult = rc;
        return ok ? kIOReturnSuccess : rc == (int)NV_KMS_FLIP_RESULT_IN_PROGRESS ? kIOReturnBusy : (fHeadSlept || !rmGpuReady()) ? kIOReturnNotReady : kIOReturnIOError;
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
    lr->config.srcWidth = (NvU16)w; lr->config.srcHeight = (NvU16)h;
    lr->config.dstWidth = (NvU16)fRasterW; lr->config.dstHeight = (NvU16)fRasterH;
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
        FBLOG("flipToMemory #%u -> ok %u rc %d (surface %s memory %s)",
              fFlipCount, (unsigned)ok, rc, surf ? "set" : "null", mem ? "set" : "null");
    if (accepted && mem && OSCompareAndSwap(1, 0, &fServerFresh) && (fTransform || fCurTransform) && fConnect.proc) {
        FBLOG("fb%u first WindowServer frame on a rotated head (0x%x): connect change", fIndex, (unsigned)fCurTransform);
        fConnect.proc(fConnect.target, fConnect.ref);
    }
    return accepted;
}

IOReturn NVRMNVDAFramebuffer::newUserClient(task_t owningTask, void *securityID, UInt32 type, IOUserClient **handler)
{
    const IOReturn rc = super::newUserClient(owningTask, securityID, type, handler);
    if (rc == kIOReturnSuccess && type == kIOFBServerConnectType) fServerFresh = 1;
    return rc;
}

IODeviceMemory *NVRMNVDAFramebuffer::getVRAMRange()
{
    if (fProxy) return IODeviceMemory::withRange((IOPhysicalAddress)fPxBase, (IOPhysicalLength)fPxBytes);
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
                FBLOGV("getVRAMRange -> WHOLE BAR%u pa 0x%llx len %llu (was the %llu-byte scanout subrange)",
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
    if (fProxy) return IODeviceMemory::withRange((IOPhysicalAddress)fPxBase, (IOPhysicalLength)fPxBytes);
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
                    FBLOGV("getApertureRange -> SUBRANGE of BAR%u (pa 0x%llx len %llu) off 0x%llx size %llu",
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
        FBLOGV("getApertureRange -> FALLBACK withAddressRange 0x%llx size %llu (MapperNone)",
              target, (unsigned long long)bytes); }
    return (IODeviceMemory *)IOMemoryDescriptor::withAddressRange(
        target, bytes, kIODirectionNone | kIOMemoryMapperNone, NULL);
}
// kIOFBHDRMetaDataAttribute: WindowServer sends the mastering display and light-level metadata of the content on screen. It
// goes out in the HDR infoframe on the next config that flags it; while HDR is active it is applied now.
IOReturn NVRMNVDAFramebuffer::setHdrMeta(const void *meta)
{
    if (!gHdrOn || !fHdrCapable || !meta) return kIOReturnUnsupported;
    const IOFBHDRMetaDataV1 &v = ((const IOFBHDRMetaData *)meta)->v1;
    struct NvKmsHDRStaticMetadata m; nvu_zero(&m, sizeof m);
    m.displayPrimaries[0].x = v.displayPrimary_X0; m.displayPrimaries[0].y = v.displayPrimary_Y0;
    m.displayPrimaries[1].x = v.displayPrimary_X1; m.displayPrimaries[1].y = v.displayPrimary_Y1;
    m.displayPrimaries[2].x = v.displayPrimary_X2; m.displayPrimaries[2].y = v.displayPrimary_Y2;
    m.whitePoint.x = v.displayPrimary_X; m.whitePoint.y = v.displayPrimary_Y;
    m.maxDisplayMasteringLuminance = v.desiredLuminance_Max; m.minDisplayMasteringLuminance = v.desiredLuminance_Min;
    m.maxCLL = v.desiredLightLevel_Max; m.maxFALL = v.desiredLightLevel_Avg;
    fHdrMeta = m; fHdrMetaSet = true;
    FBLOG("fb%u: HDR metadata: max %u cd/m2, CLL %u, FALL %u", fIndex, (unsigned)m.maxDisplayMasteringLuminance, (unsigned)m.maxCLL, (unsigned)m.maxFALL);
    if (!fHdrActive || !fModeSet || !fSurf || fHeadSlept || fConsoleAperture || !rmGpuReady()) return kIOReturnSuccess;
    NVRMDisplayGate gate; if (!gate.valid() || !fKms || !fDev) return kIOReturnSuccess;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return kIOReturnNoMemory; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->flags.hdrInfoFrameChanged = NV_TRUE;   // submitConfig fills in the infoframe from fHdrMeta
    const NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok ? kIOReturnSuccess : kIOReturnError;
}
bool NVRMNVDAFramebuffer::vrrAllowed()
{
    UInt32 on = 0;
    return PE_parse_boot_argn("nvfbvrr", &on, sizeof on) && on;
}
// Switch variable refresh on or off for the lit head: a commit that carries only that flag. Not lit yet: it is remembered
// and the next full mode set applies it.
IOReturn NVRMNVDAFramebuffer::setVrr(bool on)
{
    if (!fVrrSupported || !fVrrMinHz) return kIOReturnUnsupported;
    if (on == fVrrOn) return kIOReturnSuccess;
    fVrrOn = on;
    if (!fModeSet || !fSurf || fHeadSlept || fConsoleAperture || !rmGpuReady()) return kIOReturnSuccess;
    NVRMDisplayGate gate; if (!gate.valid() || !fKms || !fDev) return kIOReturnSuccess;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); fVrrOn = !on; return kIOReturnNoMemory; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    hr->modeSetConfig.vrrEnabled = on ? NV_TRUE : NV_FALSE; hr->flags.vrrEnabledChanged = NV_TRUE;
    const NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    FBLOG("fb%u: variable refresh %s -> %s", fIndex, on ? "on" : "off", ok ? "committed" : "REFUSED");
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    if (!ok) { fVrrOn = !on; return kIOReturnError; }
    return kIOReturnSuccess;
}
// Vibrance without a mode set: the primary layer's colour matrix, in a commit that carries only that.
bool NVRMNVDAFramebuffer::applyVibrance()
{
    if (!fModeSet || !fSurf || fHeadSlept || fConsoleAperture || !rmGpuReady()) return true;   // the next mode set carries it
    NVRMDisplayGate gate; if (!gate.valid() || !fKms || !fDev) return true;
    struct NvKmsKapiRequestedModeSetConfig *cfg = (struct NvKmsKapiRequestedModeSetConfig *)IOMalloc(sizeof *cfg);
    struct NvKmsKapiModeSetReplyConfig *rep = (struct NvKmsKapiModeSetReplyConfig *)IOMalloc(sizeof *rep);
    if (!cfg || !rep) { if (cfg) IOFree(cfg, sizeof *cfg); if (rep) IOFree(rep, sizeof *rep); return false; }
    nvu_zero(cfg, sizeof *cfg); nvu_zero(rep, sizeof *rep);
    cfg->headsMask = 1u << fHead;
    struct NvKmsKapiHeadRequestedConfig *hr = &cfg->headRequestedConfig[fHead];
    hr->modeSetConfig.bActive = NV_TRUE; hr->modeSetConfig.numDisplays = 1; hr->modeSetConfig.displays[0] = fDisplay;
    hr->modeSetConfig.mode = fMode;
    struct NvKmsKapiLayerRequestedConfig *lr = &hr->layerRequestedConfig[NVKMS_KAPI_LAYER_PRIMARY_IDX];
    nvu_zero(&lr->config.csc, sizeof lr->config.csc);
    int sm[3][3]; nvrmSaturationMatrix(gVibrance, sm);
    for (int r = 0; r < 3; r++) for (int c = 0; c < 3; c++) lr->config.csc.m[r][c] = sm[r][c];
    lr->flags.cscChanged = NV_TRUE;
    const NvBool ok = submitConfig(cfg, rep, NV_TRUE, false, true);
    FBLOG("fb%u: vibrance %d -> %s", fIndex, gVibrance, ok ? "committed" : "REFUSED");
    IOFree(cfg, sizeof *cfg); IOFree(rep, sizeof *rep);
    return ok;
}
void nvrmfbColorChanged(bool needsModeset)
{
    for (unsigned i = 0; i < 16; i++) {
        NVRMNVDAFramebuffer *fb = gFBs[i];
        if (!fb) continue;
        // bpc, format and range need a mode set: the head is lit again with the new settings. Vibrance does not.
        if (needsModeset) (void)fb->reflip(); else (void)fb->applyVibrance();
    }
}
IOReturn NVRMNVDAFramebuffer::getAttribute(IOSelect attribute, uintptr_t *value)
{
    FBSEL(attribute);
    if (attribute == kIOFBRedGammaScaleAttribute || attribute == kIOFBGreenGammaScaleAttribute || attribute == kIOFBBlueGammaScaleAttribute) {
        // CoreDisplay asks for the per-channel gamma scale at the login window and, with no answer, logs "can't get framebuffer
        // red gamma scaling factor" and skips the display's calibration profile until a user session sets it (issue 413). The
        // scale is 1.0 (16.16 fixed point) unless WindowServer set another one.
        const unsigned c = attribute == kIOFBRedGammaScaleAttribute ? 0 : attribute == kIOFBGreenGammaScaleAttribute ? 1 : 2;
        if (value) *value = fGammaScale[c];
        return kIOReturnSuccess;
    }
    if (attribute == kIOFBVariableRefreshRate) {
        if (!fVrrSupported || !fVrrMinHz) return kIOReturnUnsupported;
        if (value) *value = fVrrOn ? 1 : 0;
        return kIOReturnSuccess;
    }
    if (attribute == kIOWindowServerActiveAttribute) { if (value) *value = 1; FBLOGV("getAttribute 'wsrv' -> 1"); return kIOReturnSuccess; }
    if (attribute == kIOHardwareCursorAttribute) {
        uintptr_t v = (fCursorEnabled && fCurSurf && fCurKva && !fSurActive) ? 1 : 0;   // one cursor plane cannot span heads: software cursor
        if (value) *value = v;
        FBLOGV("getAttribute '%s' -> %lu (hardware cursor)", _s, (unsigned long)v);
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
        FBLOGV("getAttribute '%s' -> 0x%x val %lu", _s, r, (unsigned long)v);
        if (i == nseen && nseen < 16) nseen++;
        if (i < 16) { seen[i].a = attribute; seen[i].r = r; seen[i].v = v; }
    }
    return r;
}
IOReturn NVRMNVDAFramebuffer::getAttributeForConnection(IOIndex connectIndex, IOSelect attribute, uintptr_t *value)
{
    FBSEL(attribute);
    switch (attribute) {
    case kConnectionEnable: if (value) *value = (fDpyLost || fHidden) ? 0 : 1; FBLOGV("getAFC '%s' -> enable %u", _s, (unsigned)((fDpyLost || fHidden) ? 0 : 1)); return kIOReturnSuccess;
    case kConnectionFlags:  if (value) *value = fInternal ? kIOConnectionBuiltIn : 0; return kIOReturnSuccess;
    case kConnectionColorModesSupported:       if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionColorDepthsSupported:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerColorDepth:      if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionControllerDepthsSupported: if (value) *value = 0x00000002; return kIOReturnSuccess;
    case kConnectionColorMode:                 if (value) *value = 0x00000001; return kIOReturnSuccess;
    case kConnectionCheckEnable:               if (value) *value = (fDpyLost || fHidden) ? 0 : 1; return kIOReturnSuccess;
    case kConnectionDisplayFlags:              if (value) *value = 0; return kIOReturnSuccess;
    case kConnectionSupportsHLDDCSense:
        FBLOGV("fb%u getAFC HLDDCSense -> %s (EDID %u B)", fIndex, fEdidSize >= 128 ? "YES" : "no", (unsigned)fEdidSize);
        return fEdidSize >= 128 ? kIOReturnSuccess : kIOReturnUnsupported;
    case kConnectionDisplayParameterCount: if (value) *value = 0; return kIOReturnSuccess;
    case kConnectionDisplayParameters: return kIOReturnSuccess;
    case kConnectionSupportsAppleSense: case kConnectionSupportsLLDDCSense:
        return kIOReturnUnsupported;
    default: {
        IOReturn r = super::getAttributeForConnection(connectIndex, attribute, value);
        FBLOGV("getAFC '%s' -> super 0x%x val %lu", _s, r, value ? (unsigned long)*value : 0ul);
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
        if (ok) { fHeadSlept = false; publishDisplayAudioProperties(); }
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
    ddcSetPowered(ordinal != 0);
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
    if (attribute == kIOFBVariableRefreshRate) return setVrr(value != 0);
    if (attribute == kIOFBRedGammaScaleAttribute || attribute == kIOFBGreenGammaScaleAttribute || attribute == kIOFBBlueGammaScaleAttribute) {
        fGammaScale[attribute == kIOFBRedGammaScaleAttribute ? 0 : attribute == kIOFBGreenGammaScaleAttribute ? 1 : 2] = value;   // remembered and answered back
        return kIOReturnSuccess;
    }
    if (attribute == kIOFBHDRMetaDataAttribute) return value > 0x1000 ? setHdrMeta((const void *)value) : kIOReturnBadArgument;
    if (attribute == kIOWindowServerActiveAttribute) {
        FBLOGV("setAttribute 'wsrv' %lu -> success", (unsigned long)value);
        return kIOReturnSuccess; }
    if (attribute == kIOPowerAttribute) {
        FBLOGV("setAttribute POWER '%s' -> %lu", c, (unsigned long)value);
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
    FBLOGV("setAttribute '%s' value %lu -> 0x%x", c, (unsigned long)value, r);
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
    publishRotatedSizes(false);
    // Head power follows setPowerState only, the one path that has shipped; a second path here was new on every monitor
    // with no report asking for it, and its errors were new to callers that always got success (1.15 review).
    if (attribute == kConnectionPower) { FBLOGV("setAFC '%s' POWER %lu -> success", _s, (unsigned long)value); return kIOReturnSuccess; }
    switch (attribute) {
    case kConnectionColorModesSupported:
    case kConnectionColorDepthsSupported:
    case kConnectionControllerColorDepth:
    case kConnectionControllerDepthsSupported:
    case kConnectionColorMode:
    case kConnectionDisplayFlags:
    case kConnectionFlags:
        FBLOGV("setAFC '%s' value %lu -> ACCEPTED", _s, (unsigned long)value);
        return kIOReturnSuccess;
    default: break;
    }
    IOReturn r = super::setAttributeForConnection(connectIndex, attribute, value);
    FBLOGV("setAFC '%s' value %lu -> 0x%x", _s, (unsigned long)value, r);
    return r;
}

// ---- DDC/CI ----------------------------------------------------------------------------------------------------
// IOGraphics creates an IOFramebufferI2CInterface in IOFramebuffer::open() from the IOFBI2CInterfaceInfo property, so the
// property has to exist before super::start(). Publishing makes no RM call; the resource manager is reached on the first
// request. Off unless boot-arg nvrmfbddc=1 (NVRM reads the same switch before it registers its I2C adapters).
void NVRMNVDAFramebuffer::publishDdcBus()
{
    UInt32 on = 0;
    fDdcOn = false;
    if (fInternal || !PE_parse_boot_argn("nvrmfbddc", &on, sizeof on) || !on) return;
    OSArray *buses = OSArray::withCapacity(1);
    OSDictionary *bus = OSDictionary::withCapacity(4);
    // IOGraphics requires a bus ID to start the interface. It scopes bus 0 to this framebuffer.
    OSNumber *id = OSNumber::withNumber((UInt64)0, 32);
    OSNumber *type = OSNumber::withNumber((UInt64)kIOI2CBusTypeI2C, 32);
    OSNumber *tx = OSNumber::withNumber((UInt64)((1u << kIOI2CNoTransactionType) | (1u << kIOI2CSimpleTransactionType) |
                                                 (1u << kIOI2CDDCciReplyTransactionType)), 32);
    OSNumber *flags = OSNumber::withNumber((UInt64)0, 32);
    if (buses && bus && id && type && tx && flags) {
        bus->setObject(kIOI2CInterfaceIDKey, id);
        bus->setObject(kIOI2CBusTypeKey, type); bus->setObject(kIOI2CTransactionTypesKey, tx);
        bus->setObject(kIOI2CSupportedCommFlagsKey, flags);
        buses->setObject(bus);
        setProperty(kIOFBI2CInterfaceInfoKey, buses);
        fDdcOn = true;
        FBLOG("fb%u: DDC/CI bus published (nvrmfbddc=1)", fIndex);
    }
    OSSafeReleaseNULL(flags); OSSafeReleaseNULL(tx); OSSafeReleaseNULL(type); OSSafeReleaseNULL(id);
    OSSafeReleaseNULL(bus); OSSafeReleaseNULL(buses);
}
void NVRMExternalBrightness::setFramebuffer(NVRMNVDAFramebuffer *framebuffer)
{
    fFramebuffer = framebuffer;
    fFramebuffer->retain();
}

void NVRMExternalBrightness::free()
{
    OSSafeReleaseNULL(fRetryDisplay);
    OSSafeReleaseNULL(fParameters);
    OSSafeReleaseNULL(fFramebuffer);
    IODisplayParameterHandler::free();
}

bool NVRMExternalBrightness::setDisplay(IODisplay *display)
{
    OSSafeReleaseNULL(fRetryDisplay);
    OSSafeReleaseNULL(fParameters);
    if (!display || OSDynamicCast(IOBacklightDisplay, display) || OSAddAtomic(0, &fFramebuffer->fStopping)) return false;
    bool retry = false;
    if (publishParameters(display, retry)) return true;
    if (!retry) return false;
    fRetryDisplay = display;
    fRetryDisplay->retain();
    fRetryGeneration = __atomic_load_n(&fFramebuffer->fDdcSinkGeneration, __ATOMIC_RELAXED);
    fRetriesLeft = kNVRMDdcProbeMaxRetries;
    fRetryDelayMs = kNVRMDdcProbeFirstDelayMs;
    fFramebuffer->scheduleDdcProbe(fRetryDelayMs);
    return true;
}

void NVRMExternalBrightness::retryProbe()
{
    if (!fRetryDisplay) return;
    if (fRetryDisplay->isInactive() || !fRetryDisplay->getConnection() ||
        fRetryGeneration != __atomic_load_n(&fFramebuffer->fDdcSinkGeneration, __ATOMIC_RELAXED)) {
        OSSafeReleaseNULL(fRetryDisplay);
        return;
    }
    bool retry = true;
    if (publishParameters(fRetryDisplay, retry)) {
        IODisplay *display = fRetryDisplay;
        fRetryDisplay = nullptr;
        display->messageClients(kIOMessageServicePropertyChange);
        display->release();
        return;
    }
    if (!retry || !--fRetriesLeft) { OSSafeReleaseNULL(fRetryDisplay); return; }
    // Back off so a slow sink gets progressively more time without hammering the bus early on.
    if (fRetryDelayMs < kNVRMDdcProbeMaxDelayMs) {
        fRetryDelayMs *= 2;
        if (fRetryDelayMs > kNVRMDdcProbeMaxDelayMs) fRetryDelayMs = kNVRMDdcProbeMaxDelayMs;
    }
    fFramebuffer->scheduleDdcProbe(fRetryDelayMs);
}

bool NVRMExternalBrightness::publishParameters(IODisplay *display, bool &retry)
{
    retry = false;
    OSObject *old = display->copyProperty(gIODisplayParametersKey);
    OSDictionary *current = OSDynamicCast(OSDictionary, old);
    OSDictionary *params = current ? OSDictionary::withDictionary(current) : OSDictionary::withCapacity(4);
    OSSafeReleaseNULL(old);
    if (!params) return false;
    UInt32 value = 0;
    const bool ownsBrightness = !params->getObject(gIODisplayBrightnessKey);
    // A sink that is still coming up answers late, so the retry window grows with the retry delay.
    unsigned replyMs = kNVRMDdcProbeFirstReplyMs;
    if (fRetryDisplay) {
        replyMs = fRetryDelayMs > kNVRMDdcProbeReplyMs ? fRetryDelayMs : kNVRMDdcProbeReplyMs;
        if (replyMs > kNVRMDdcProbeMaxReplyMs) replyMs = kNVRMDdcProbeMaxReplyMs;
    }
    bool ok = ownsBrightness &&
        fFramebuffer->readDdcBrightness(value, fMaximum, fGeneration, replyMs);
    retry = ownsBrightness && !ok;
    if (ok) {
        ok = IODisplay::addParameter(params, gIODisplayBrightnessKey, 0, static_cast<SInt32>(fMaximum)) &&
            IODisplay::setParameter(params, gIODisplayBrightnessKey, static_cast<SInt32>(value));
    }

    // Use the external display brightness route with the verified VCP range.
    // Leave capabilities already published by an Apple or MCCS handler in place.
    OSData *capability = nullptr;
    if (ok && !display->getProperty(kIODisplayCapabilityStringKey)) {
        static constexpr char externalModel[] = "model(EDM)";
        capability = OSData::withBytes(externalModel, sizeof(externalModel) - 1);
        ok = capability && display->setProperty(kIODisplayCapabilityStringKey, capability);
    }
    if (ok && display->setProperty(gIODisplayParametersKey, params)) {
        OSSafeReleaseNULL(capability);
        fParameters = params;
        BLLOG("external brightness: published for %s at %u/%u", display->getName(), value, fMaximum);
        return true;
    }
    if (capability && display->getProperty(kIODisplayCapabilityStringKey) == capability)
        display->removeProperty(kIODisplayCapabilityStringKey);
    OSSafeReleaseNULL(capability);
    params->release();
    return false;
}

bool NVRMExternalBrightness::doIntegerSet(OSDictionary *params, const OSSymbol *key, UInt32 value)
{
    if (key != gIODisplayBrightnessKey || !params || !fParameters || value > fMaximum) return false;
    OSObject *range = fParameters->getObject(key);
    if (!range || (params != range && params->getObject(key) != range)) return false;
    return fFramebuffer->queueDdcBrightness(value, fGeneration);
}

void NVRMNVDAFramebuffer::publishDdcBrightness()
{
    if (!fDdcSinkLock) return;
    fDdcBrightness = OSTypeAlloc(NVRMExternalBrightness);
    if (!fDdcBrightness) return;
    if (!fDdcBrightness->init() || !fDdcBrightness->attach(this)) {
        OSSafeReleaseNULL(fDdcBrightness);
        return;
    }
    fDdcBrightness->setFramebuffer(this);
    fDdcBrightnessCall = nvrmfbCall(ddcBrightnessFire, this);
    fDdcProbeCall = nvrmfbCall(ddcProbeFire, this);
    if (!fDdcBrightnessCall || !fDdcProbeCall) {
        if (fDdcBrightnessCall) { thread_call_free(fDdcBrightnessCall); fDdcBrightnessCall = nullptr; }
        if (fDdcProbeCall) { thread_call_free(fDdcProbeCall); fDdcProbeCall = nullptr; }
        fDdcBrightness->detach(this);
        OSSafeReleaseNULL(fDdcBrightness);
        return;
    }
    fDdcBrightness->registerService(kIOServiceSynchronous);
}

void NVRMNVDAFramebuffer::scheduleDdcProbe(unsigned delayMs)
{
    uint64_t deadline;
    clock_interval_to_deadline(delayMs, kMillisecondScale, &deadline);
    scheduleCall(fDdcProbeCall, deadline);
}

void NVRMNVDAFramebuffer::ddcProbeFire(thread_call_param_t p0, thread_call_param_t)
{
    auto *fb = static_cast<NVRMNVDAFramebuffer *>(p0);
    NVRMCallReference callReference(fb);
    if (OSAddAtomic(0, &fb->fStopping) || !fb->fDdcBrightnessGate) return;
    const IOReturn r = fb->fDdcBrightnessGate->attemptAction(
        OSMemberFunctionCast(IOCommandGate::Action, fb, &NVRMNVDAFramebuffer::ddcProbeGated));
    if (r == kIOReturnCannotLock) fb->scheduleDdcProbe(kNVRMDdcProbeFirstDelayMs);
}

IOReturn NVRMNVDAFramebuffer::ddcProbeGated(void *, void *, void *, void *)
{
    if (OSAddAtomic(0, &fStopping)) return kIOReturnOffline;
    fDdcBrightness->retryProbe();
    return kIOReturnSuccess;
}

bool NVRMNVDAFramebuffer::readDdcBrightness(UInt32 &value, UInt32 &maximum, UInt32 &generation, unsigned delayMs)
{
    if (!fDdcBrightnessGate || OSAddAtomic(0, &fStopping)) return false;
    generation = __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED);
    unsigned char query[] = {0x51, 0x82, 0x01, 0x10, 0xac};
    unsigned char reply[11] = {};
    IOI2CRequest rq = {};
    rq.sendAddress = 0x6e;
    rq.sendTransactionType = kIOI2CSimpleTransactionType;
    rq.sendBytes = sizeof query;
    rq.sendBuffer = (uintptr_t)query;
    rq.replyAddress = 0x6f;
    rq.replyTransactionType = kIOI2CDDCciReplyTransactionType;
    rq.replyBytes = sizeof reply;
    rq.replyBuffer = (uintptr_t)reply;
    nanoseconds_to_absolutetime(uint64_t(delayMs) * 1000000, &rq.minReplyDelay);
    const IOReturn result = doI2CRequest(0, nullptr, &rq);
    unsigned char checksum = 0x50;
    for (const unsigned char byte : reply) checksum ^= byte;
    value = (UInt32(reply[8]) << 8) | reply[9];
    maximum = (UInt32(reply[6]) << 8) | reply[7];
    if (result != kIOReturnSuccess || rq.replyBytes != sizeof reply || checksum || !maximum || value > maximum ||
        reply[0] != 0x6e || reply[1] != 0x88 || reply[2] != 2 || reply[3] || reply[4] != 0x10 || reply[5] ||
        generation != __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED)) {
        FBLOG("fb%u: external brightness probe failed: rc 0x%x, %u bytes", fIndex, result, rq.replyBytes);
        return false;
    }
    return true;
}

bool NVRMNVDAFramebuffer::queueDdcBrightness(UInt32 value, UInt32 generation)
{
    if (!fFlipCacheLock || OSAddAtomic(0, &fStopping) ||
        generation != __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED))
        return false;
    const UInt64 request = (UInt64(generation) << 32) | (UInt64(value) + 1);
    IOInterruptState state = IOSimpleLockLockDisableInterrupt(fFlipCacheLock);
    fDdcPendingAt = mach_absolute_time();
    __atomic_store_n(&fDdcPending, request, __ATOMIC_RELEASE);
    IOSimpleLockUnlockEnableInterrupt(fFlipCacheLock, state);
    setProperty("NVRMExternalBrightnessRequested", value, 32);
    scheduleCall(fDdcBrightnessCall, 0);
    return true;
}

void NVRMNVDAFramebuffer::ddcBrightnessFire(thread_call_param_t p0, thread_call_param_t)
{
    auto *fb = static_cast<NVRMNVDAFramebuffer *>(p0);
    NVRMCallReference callReference(fb);
    if (OSAddAtomic(0, &fb->fStopping) || !__atomic_load_n(&fb->fDdcPending, __ATOMIC_ACQUIRE))
        return;
    const IOReturn r = fb->writeDdcBrightness();
    if (r == kIOReturnCannotLock) {
        uint64_t deadline;
        clock_interval_to_deadline(20, kMillisecondScale, &deadline);
        fb->scheduleCall(fb->fDdcBrightnessCall, deadline);
    }
}

IOReturn NVRMNVDAFramebuffer::writeDdcBrightness()
{
    NVRMDdcTransaction transaction(&fDdcInFlight);
    if (!transaction.held()) {
        ++fDdcBusyRetries;
        return kIOReturnCannotLock;
    }
    if (OSAddAtomic(0, &fStopping)) return kIOReturnOffline;
    if (mach_absolute_time() < fDdcWriteAfter) {
        scheduleCall(fDdcBrightnessCall, fDdcWriteAfter);
        return kIOReturnSuccess;
    }

    // RM serializes AUX/I2C itself. Only exclude changes to this sink, so a continuous
    // stream of flips on either head cannot starve a pending brightness write.
    NVRMDisplayGate sink(fDdcSinkLock);
    IOInterruptState state = IOSimpleLockLockDisableInterrupt(fFlipCacheLock);
    const UInt64 request = __atomic_exchange_n(&fDdcPending, 0, __ATOMIC_ACQ_REL);
    const UInt64 queuedAt = fDdcPendingAt;
    IOSimpleLockUnlockEnableInterrupt(fFlipCacheLock, state);
    const UInt32 busyRetries = fDdcBusyRetries;
    fDdcBusyRetries = 0;
    if (!sink.valid() || !request || OSAddAtomic(0, &fStopping) ||
        (request >> 32) != __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED) ||
        fDpyLost || fEdidSwapped || fHidden || !__atomic_load_n(&fDdcPowered, __ATOMIC_ACQUIRE) || !rmGpuReady())
        return kIOReturnOffline;

    const UInt32 value = static_cast<UInt32>(request) - 1;
    unsigned char msg[] = {0x51, 0x84, 0x03, 0x10, (unsigned char)(value >> 8), (unsigned char)value, 0x6e};
    for (unsigned i = 0; i + 1 < sizeof msg; ++i) msg[6] ^= msg[i];
    const UInt64 started = mach_absolute_time();
    const IOReturn r = ddcTransfer(kNvrmDdcWrite, 0x37, msg, sizeof msg);
    const UInt64 finished = mach_absolute_time();
    UInt64 queueNs, transferNs, settleNs;
    absolutetime_to_nanoseconds(started - queuedAt, &queueNs);
    absolutetime_to_nanoseconds(finished - started, &transferNs);
    absolutetime_to_nanoseconds(fDdcWriteAfter > queuedAt ? fDdcWriteAfter - queuedAt : 0, &settleNs);
    // Queue, transfer and settle time in microseconds, I2C retries, and the retired display-lock retry slot.
    UInt32 timing[] = { (UInt32)(queueNs / 1000), (UInt32)(transferNs / 1000),
                       (UInt32)(settleNs / 1000), busyRetries, 0 };
    setProperty("NVRMExternalBrightnessWriteTiming", timing, sizeof timing);
    setProperty("NVRMExternalBrightnessWriteStatus", (UInt32)r, 32);
    if (r == kIOReturnSuccess) setProperty("NVRMExternalBrightnessWritten", value, 32);
    clock_interval_to_deadline(40, kMillisecondScale, &fDdcWriteAfter);
    return r;
}

// IOFramebufferI2CInterface::startIO refuses a request while !isPowered() (pendingPowerState == 0). This class replaces
// setPowerState, which is what sets it in the family, so it stayed 0 and every request was answered "offline" before it
// reached doI2CRequest. Set it from the framebuffer's own gate: 1 = on, 0 = off, never 2 (restoreFramebuffer's branch),
// and pendingPowerChange is left alone so no power work starts.
IOReturn NVRMNVDAFramebuffer::ddcPowerGated(void *on, void *, void *, void *)
{
    NVRMDisplayGate sink(fDdcSinkLock);
    pendingPowerState = on ? 1 : 0;
    __atomic_store_n(&fDdcPowered, on != nullptr, __ATOMIC_RELEASE);
    return kIOReturnSuccess;
}
void NVRMNVDAFramebuffer::ddcSetPowered(bool on)
{
    if (!fDdcOn) return;
    IOWorkLoop *wl = getWorkLoop();
    if (!fWakeGate && wl) {
        fWakeGate = IOCommandGate::commandGate(this);
        if (fWakeGate && wl->addEventSource(fWakeGate) != kIOReturnSuccess) OSSafeReleaseNULL(fWakeGate);
    }
    if (fWakeGate) fWakeGate->runAction(OSMemberFunctionCast(IOCommandGate::Action, this, &NVRMNVDAFramebuffer::ddcPowerGated),
                                        on ? (void *)1 : (void *)0);
}
IOReturn NVRMNVDAFramebuffer::ddcTransfer(unsigned op, unsigned addr7, unsigned char *data, unsigned len)
{
    IOService *rm = getProvider();
    const OSSymbol *fn = OSSymbol::withCString(NVRM_DDC_FN);
    if (!rm || !fn) { OSSafeReleaseNULL(fn); return kIOReturnNotFound; }
    struct NVRMDdcRequest *q = (struct NVRMDdcRequest *)IOMalloc(sizeof *q);
    if (!q) { fn->release(); return kIOReturnNoMemory; }
    nvu_zero(q, sizeof *q);
    q->gpuId = fGpuId; q->displayId = (unsigned)fDisplay; q->op = op; q->addr7 = addr7; q->len = len;
    if (op == kNvrmDdcWrite) nvu_copy(q->data, data, len);
    IOReturn r = rm->callPlatformFunction(fn, false, q, NULL, NULL, NULL);
    if (r == kIOReturnSuccess && op == kNvrmDdcRead) nvu_copy(data, q->data, len);
    if (r != kIOReturnSuccess) FBLOG("fb%u: DDC %s 0x%02x len %u failed: rc 0x%x, RM status 0x%x", fIndex, op == kNvrmDdcWrite ? "write" : "read",
                                     addr7, len, (unsigned)r, q->status);
    IOFree(q, sizeof *q);
    fn->release();
    return r;
}
IOReturn NVRMNVDAFramebuffer::doI2CRequest(UInt32 bus, struct IOI2CBusTiming *, struct IOI2CRequest *rq)
{
    if (!rq) return kIOReturnBadArgument;
    IOReturn res = kIOReturnSuccess;
    if (!fDdcOn || bus != 0 || fDpyLost || !fKms) res = kIOReturnUnsupported;
    else if (rq->commFlags & kIOI2CUseSubAddressCommFlag) res = kIOReturnUnsupportedMode;
    else {
        NVRMDdcTransaction transaction(&fDdcInFlight);
        if (!transaction.held()) {
            rq->result = kIOReturnBusy;
            if (rq->completion) (*rq->completion)(rq);
            return kIOReturnBusy;
        }
        const UInt32 generation = __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED);
        const bool send = rq->sendTransactionType != kIOI2CNoTransactionType && rq->sendBytes;
        const bool reply = rq->replyTransactionType != kIOI2CNoTransactionType && rq->replyBytes;
        const bool simpleSend = rq->sendTransactionType == kIOI2CSimpleTransactionType;
        const bool okReply = rq->replyTransactionType == kIOI2CSimpleTransactionType || rq->replyTransactionType == kIOI2CDDCciReplyTransactionType;
        if ((send && !simpleSend) || (reply && !okReply) || (!send && !reply)) res = kIOReturnUnsupportedMode;
        else if ((send && (rq->sendBytes > kNvrmDdcMax || !rq->sendBuffer)) || (reply && !rq->replyBuffer)) res = kIOReturnBadArgument;
        else {
            if (send) {
                const unsigned a = nvrmDdcAddr7(rq->sendAddress, false);
                res = (a && nvrmDdcWriteLenAllowed(a, rq->sendBytes)) ? ddcTransfer(kNvrmDdcWrite, a, (unsigned char *)(uintptr_t)rq->sendBuffer, rq->sendBytes) : kIOReturnBadArgument;
                if (res != kIOReturnSuccess && res != kIOReturnBadArgument) res = kIOReturnNoDevice;
            }
            if (res == kIOReturnSuccess && reply) {
                const unsigned a = nvrmDdcAddr7(rq->replyAddress ? rq->replyAddress : rq->sendAddress | 1, true);
                uint64_t ns = 0; absolutetime_to_nanoseconds(rq->minReplyDelay, &ns);
                if (send) IOSleep(nvrmDdcReplyDelayMs(ns));
                unsigned want = rq->replyBytes > kNvrmDdcMax ? kNvrmDdcMax : rq->replyBytes;
                res = a ? ddcTransfer(kNvrmDdcRead, a, (unsigned char *)(uintptr_t)rq->replyBuffer, want) : kIOReturnBadArgument;
                if (res == kIOReturnSuccess) {
                    rq->replyBytes = rq->replyTransactionType == kIOI2CDDCciReplyTransactionType
                        ? nvrmDdcReplyLength((const unsigned char *)(uintptr_t)rq->replyBuffer, want) : want;
                    if (a == 0x37) {
                        static const unsigned char getBrightness[] = {0x51, 0x82, 0x01, 0x10, 0xac};
                        const bool fast = generation == __atomic_load_n(&fDdcFastReadGeneration, __ATOMIC_ACQUIRE) &&
                            generation == __atomic_load_n(&fDdcSinkGeneration, __ATOMIC_RELAXED) &&
                            send && rq->sendAddress == 0x6e && rq->sendBytes == sizeof getBrightness &&
                            memcmp((const void *)(uintptr_t)rq->sendBuffer, getBrightness, sizeof getBrightness) == 0;
                        UInt64 deadline;
                        clock_interval_to_deadline(fast ? 40 : 140, kMillisecondScale, &deadline);
                        if (deadline > fDdcWriteAfter) fDdcWriteAfter = deadline;
                    }
                } else if (res != kIOReturnBadArgument) res = kIOReturnNoDevice;
            }
        }
    }
    rq->result = res;
    if (rq->completion) (*rq->completion)(rq);
    return res == kIOReturnSuccess ? kIOReturnSuccess : res;
}
IOReturn NVRMNVDAFramebuffer::getDDCBlock(IOIndex, UInt32 blockNumber, IOSelect blockType, IOOptionBits, UInt8 *data, IOByteCount *length)
{
    if (fDpyLost || blockType != kIODDCBlockTypeEDID || blockNumber == 0 || blockNumber * 128 > fEdidSize) return kIOReturnUnsupported;
    if (*length < 128) return kIOReturnBadArgument;
    nvu_copy(data, fEdid + (blockNumber - 1) * 128, 128); *length = 128;
    return kIOReturnSuccess;
}
IOReturn NVRMNVDAFramebuffer::registerForInterruptType(IOSelect type, IOFBInterruptProc proc, OSObject *target, void *ref, void **interruptRef)
{
    char t[5] = { (char)(type >> 24), (char)(type >> 16), (char)(type >> 8), (char)type, 0 };
    if (type == kIOFBVBLInterruptType) {
        fVbl = { proc, target, ref }; if (interruptRef) *interruptRef = &fVbl; fVblReg++;
        FBLOG("registerForInterruptType '%s' -> VBL REGISTERED (proc %s)", t, proc ? "set" : "null");
        return kIOReturnSuccess;
    }
    if (type == kIOFBConnectInterruptType) {
        fConnect = { proc, target, ref }; if (interruptRef) *interruptRef = &fConnect;
        FBLOG("registerForInterruptType '%s' -> CONNECT REGISTERED", t);
        // A connect notification that came due before this registration (connectNotifyFire found no handler). It runs
        // from the thread call, not here: the framework is inside its own open path while it registers.
        if (proc && OSAddAtomic(0, &fConnectOwed)) {
            FBLOG("fb%u: the connect interrupt owed since the mode set fires now that the handler is registered", fIndex);
            notifyConnectionChanged(100);
        }
        return kIOReturnSuccess;
    }
    FBLOG("registerForInterruptType '%s' -> REFUSED (unsupported)", t);
    return kIOReturnUnsupported;
}
