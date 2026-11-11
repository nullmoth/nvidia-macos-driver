/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
class NVAccel;
#include "IOAccelResource2.h"
#include "IOAccelSysMemory.h"
#include "IOAccelVidMemory.h"
#include "IOAccelLegacySurface.h"
#include "IOAccelLegacyDisplayPipe.h"
#include "IOAccelGLContext2.h"
#include "IOAccel2DContext2.h"
#include "IOAccelCLContext2.h"
#include "IOAccelVideoContext2.h"
#include "nvres_fields.h"
class NVResource : public IOAccelResource2 {
    OSDeclareDefaultStructors(NVResource)
    NM_TAHOE_FWD(NVResource)
public:
    class NVAccel *fAccel = nullptr;   // the accelerator that made this object (NVAccel::resolve)
    unsigned int fPbLogged = 0;
    void rebuildPagingBuffer() APPLE_KEXT_OVERRIDE { if (fPbLogged < 4) { fPbLogged++; ALOG("NVResource::rebuildPagingBuffer -- nothing to rebuild until there are page tables (rung 5)"); } }
    uint64_t getLevelOffset(unsigned char a0, unsigned char a1, int* a2) APPLE_KEXT_OVERRIDE {
        const unsigned char *r = (const unsigned char *)this;
        if (a2) {
            unsigned long long pitch = nvResPitch(r);
            unsigned base = r[kNvResBaseLevel];
            *a2 = (int)(pitch >> ((a1 >= base) ? (unsigned)(a1 - base) : 0u));
        }
        return (unsigned long long)a0 * nvResFaceSize(r);
    }
    uint64_t getBackingLevelOffset(unsigned char a0, unsigned char a1, int* a2) APPLE_KEXT_OVERRIDE {
        const unsigned char *r = (const unsigned char *)this;
        if (a2) {
            unsigned long long pitch = nvResBackingPitch(r);
            unsigned base = r[kNvResBaseLevel];
            *a2 = (int)(pitch >> ((a1 >= base) ? (unsigned)(a1 - base) : 0u));
        }
        return (unsigned long long)a0 * nvResFaceSize(r);
    }
    uint64_t calculateIOSurfaceDeviceCacheVRAMBytes(unsigned long long* a0, unsigned long long* a1) APPLE_KEXT_OVERRIDE {
        const unsigned char *r = (const unsigned char *)this;
        unsigned long long size = nvResFaceSize(r);
#if defined(NM_ERA) && NM_ERA < 1014
        // Before 10.14 the second result is a 32-bit count (Apple's own driver declares it so); writing 8 bytes would
        // run past the caller's variable.
        if (a1) *(unsigned int *)a1 = 1;
#else
        if (a1) *a1 = 1;
#endif
        if (a0) *a0 = size;
        return size;
    }
    bool addToAperture() APPLE_KEXT_OVERRIDE;
    void removeFromAperture() APPLE_KEXT_OVERRIDE;
    void *getApertureMemoryDescriptor(unsigned long long *a0) APPLE_KEXT_OVERRIDE;
    void free() APPLE_KEXT_OVERRIDE;
#if defined(NM_ERA) && NM_ERA < 1012
    // Before 10.12 the family declares only the two short forms, both pure; the driver supplies them.
    void pageon(IOAccelEvent *, bool) APPLE_KEXT_OVERRIDE { }
    void pageoff(IOAccelEvent *ev, bool a1, bool *out) APPLE_KEXT_OVERRIDE { pageoff(ev, a1, out, 0ull); }
    void pageoff(IOAccelEvent *, bool, bool *, unsigned long long);
#else
    void pageoff(IOAccelEvent *, bool, bool *, unsigned long long) APPLE_KEXT_OVERRIDE;
#endif
    unsigned int fApLogged = 0;
    IOMemoryDescriptor *fApDesc = nullptr;
    // Set by the plugin's surface-dirty escape: the allocation cookie of the VRAM the GPU wrote for this resource. The
    // family's own "VRAM newest" bit can be set on a VRAM copy nothing ever rendered into (freshly allocated,
    // zero-filled); a zero-copy flip of that copy, or a page-off from it, puts a black frame on the panel while the real
    // pixels sit in the surface's system memory. The display pipe compares this with the current allocation's cookie
    // before trusting the family bit, so VRAM re-allocated under the same resource counts as unwritten again (a parked
    // allocation that comes back keeps its cookie, and so its record). 0 = never written.
    unsigned long long fPluginWroteCookie = 0;
};
OSDefineMetaClassAndStructors(NVResource, IOAccelResource2)
class NVSysMemory : public IOAccelSysMemory {
    OSDeclareDefaultStructors(NVSysMemory)
    NM_TAHOE_FWD(NVSysMemory)
public:
};
OSDefineMetaClassAndStructors(NVSysMemory, IOAccelSysMemory)
class NVVidMemory : public IOAccelVidMemory {
    OSDeclareDefaultStructors(NVVidMemory)
    NM_TAHOE_FWD(NVVidMemory)
public:
    class NVAccel *fAccel = nullptr;   // the accelerator that made this object (NVAccel::resolve)
    void              *fRmHandle = nullptr;
    unsigned long long fAllocationCookie = 0;
    void              *fKva      = nullptr;
    unsigned long long fPhys     = 0;
    unsigned long long fBytes    = 0;
    bool fScanout = false;
    // VRAM kept across deallocPhysical/allocPhysical (debug.nvaccel_vm_sticky): see NVVidMemory::deallocPhysical
    void              *fParkHandle = nullptr;
    void              *fParkKva    = nullptr;
    unsigned long long fParkPhys = 0, fParkBytes = 0, fParkCookie = 0;
    void releaseVram(void *handle, void *kva, unsigned long long bytes, unsigned long long cookie);
    void releaseParked();

    bool allocPhysical() APPLE_KEXT_OVERRIDE;
    void deallocPhysical() APPLE_KEXT_OVERRIDE;
    void free() APPLE_KEXT_OVERRIDE;

    uint64_t getPhysicalSegment(unsigned long long a0, unsigned long long* a1) APPLE_KEXT_OVERRIDE {
        const unsigned long long total = *(const volatile unsigned long long *)((const unsigned char *)this + 0x40);
        if (!fPhys || a0 >= total) { if (a1) *a1 = 0; return 0; }
        if (a1) *a1 = total - a0;
        return fPhys + a0;
    }
};
OSDefineMetaClassAndStructors(NVVidMemory, IOAccelVidMemory)
class NVSurface : public IOAccelLegacySurface {
    OSDeclareDefaultStructors(NVSurface)
    NM_TAHOE_FWD(NVSurface)
public:
#if defined(NM_ERA) && NM_ERA < 1015
    // macOS 10.13 and 10.14: one surface class, with these two where the newer family has the sync command and a pad.
    // canTmpAlloc answers 1, as Apple's own NVIDIA surface does on these releases.
    void submitSwapFlush(IOAccelEvent* a0, IOAccelResource2* a1, IOAccelResource2* a2, IOAccelSwapFlushRec const* a3, unsigned int a4, unsigned int a5, unsigned int a6, IOAccelBounds const* a7) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::submitSwapFlush"); }
    bool canTmpAlloc(unsigned int a0) APPLE_KEXT_OVERRIDE { return true; }
    // The family keeps these two to itself on these releases (not exported), so a vendor surface supplies its own,
    // as every Apple vendor driver of the time does.
    void submitCopyForward(IOAccelEvent* a0, unsigned int a1, IOAccelResource2* a2, IOAccelResource2* a3, IOAccelBounds const* a4, unsigned int a5) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::submitCopyForward"); }
    void submitUpdate(unsigned int a0, IOAccelBounds* a1, unsigned int a2) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::submitUpdate"); }
#else
    void setSyncCommand(unsigned int* a0, unsigned int& a1, unsigned int& a2, _AMDSurfaceSwapSyncOptions const& a3) const APPLE_KEXT_OVERRIDE { ALOG("NVSurface::setSyncCommand"); }
    void _pure335() APPLE_KEXT_OVERRIDE { ALOG("NVSurface::_pure335"); }
#endif
    bool copyFromBuffer(int a0, int a1, int a2, int a3, unsigned int a4, unsigned int a5, IOAccelEvent* a6, IOAccelResource2* a7, IOAccelSysMemory* a8, unsigned int a9, unsigned long long a10, unsigned int a11) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::copyFromBuffer"); return false; }
    bool copyToBuffer(int a0, int a1, int a2, int a3, unsigned int a4, unsigned int a5, IOAccelEvent* a6, IOAccelResource2* a7, IOAccelSysMemory* a8, unsigned int a9, unsigned long long a10, unsigned int a11) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::copyToBuffer"); return false; }
    void submitSwapCopy(IOAccelEvent* a0, IOAccelResource2* a1, IOAccelResource2* a2) APPLE_KEXT_OVERRIDE {
        (void)a0;
        nvAccelSwapProbe(a1, a2);
        IOAccelResource2 *src = nvAccelResHasContent(a2) ? a2
                              : nvAccelResHasContent(a1) ? a1 : nullptr;
        if (src && !nvAccelFlipTo(src)) nvAccelCopyToScanout(src);
    }
    void shapeSurface(unsigned int a0, unsigned short a1, unsigned short a2) APPLE_KEXT_OVERRIDE {
        const unsigned w = a1, h = a2;
        uint32_t smode = *(const volatile uint32_t *)((const uint8_t *)this + 0x11B0);
        unsigned bpp = ((smode >> 16) & 0xf) + 1;
        if (bpp < 4 || bpp > 16) bpp = 4;
        unsigned pitch = (unsigned)((w * bpp + 255u) & ~255u);
        unsigned size  = pitch * h;
        IOAccelResource2 **bufs = (IOAccelResource2 **)((uint8_t *)this + 0x11C0);
        unsigned present = 0, shaped = 0;
        for (unsigned i = 0; i < 32; i++) {
            if (!(a0 & (1u << i))) continue;
            IOAccelResource2 *r = OSDynamicCast(IOAccelResource2, bufs[i]);
            if (!r) continue;
            present++;
            if (r->initSurfaceBuffer(this, w, h, size, pitch, 0)) shaped++;
        }
        ALOG("NVSurface::shapeSurface(mode 0x%x, %u x %u) smode 0x%x bpp %u pitch %u size %u -> %u present, %u shaped",
             a0, w, h, smode, bpp, pitch, size, present, shaped);
        static int gShapeDumps = 0;
        if (gShapeDumps < 4) { gShapeDumps++;
            const volatile unsigned long long *qq = (const volatile unsigned long long *)((const unsigned char *)this + 0x1160);
            for (unsigned o = 0; o < 0x60; o += 16)
                ALOG("   surf+0x%04x: %016llx %016llx", 0x1160 + o, qq[o / 8], qq[o / 8 + 1]);
        }
    }
    uint32_t getDirtyBufferBitsFromPrivateModeBits(unsigned long long a0) APPLE_KEXT_OVERRIDE { ALOG("NVSurface::getDirtyBufferBitsFromPrivateModeBits(0x%llx)", (unsigned long long)a0); return 0; }
};
OSDefineMetaClassAndStructors(NVSurface, IOAccelLegacySurface)
class NVDisplayPipe : public IOAccelLegacyDisplayPipe {
    OSDeclareDefaultStructors(NVDisplayPipe)
    NM_TAHOE_FWD(NVDisplayPipe)
public:
    class NVAccel *fAccel = nullptr;   // the accelerator that made this object (NVAccel::resolve)
    IOAccelMemory *initFramebufferResource(unsigned int index, IOAccelResource2 *res) APPLE_KEXT_OVERRIDE;
    IOReturn validateTransaction(IOAccelDisplayPipeTransaction2 *txn) APPLE_KEXT_OVERRIDE;
    IOReturn performTransaction(IOAccelDisplayPipeTransaction2 *txn) APPLE_KEXT_OVERRIDE;
    bool     isTransactionComplete(IOAccelDisplayPipeTransaction2 *txn) APPLE_KEXT_OVERRIDE;
    void     free() APPLE_KEXT_OVERRIDE;
    enum { kIopSrcSlots = 4 };
    struct NVIopSrc { unsigned id; IOMemoryDescriptor *md; IOMemoryMap *map; } fIopSrc[kIopSrcSlots];
    unsigned fIopNext;
    const void *iopMapSurface(unsigned id, IOMemoryDescriptor *md, unsigned long long need);
    void        iopDropSources();
};
OSDefineMetaClassAndStructors(NVDisplayPipe, IOAccelLegacyDisplayPipe)
class NVGLContext : public IOAccelGLContext2 {
    OSDeclareDefaultStructors(NVGLContext)
    NM_TAHOE_FWD(NVGLContext)
public:
#if defined(NM_ERA) && NM_ERA < 1015
    // the same four pure slots, under the names the 10.14 vendor drivers give them
    void addDrawableToChannel(IOAccelDrawable2* a0) APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::addDrawableToChannel"); }
    void removeDrawableFromChannel(IOAccelDrawable2* a0) APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::removeDrawableFromChannel"); }
    // Out-parameters are always written: the family reads them after the call.
    void setCompatibleSurfaceMode(unsigned long long* a0, unsigned long long a1, int a2) APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::setCompatibleSurfaceMode"); if (a0) *a0 = 0; }
    void get_temp_allocation_info(IOAccelDrawable2* a0, unsigned int* a1, unsigned int* a2) APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::get_temp_allocation_info"); if (a1) *a1 = 0; if (a2) *a2 = 0; }
#else
    void _pure362() APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::_pure362"); }
    void _pure363() APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::_pure363"); }
    void _pure364() APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::_pure364"); }
    void _pure367() APPLE_KEXT_OVERRIDE { ALOG("NVGLContext::_pure367"); }
#endif
};
OSDefineMetaClassAndStructors(NVGLContext, IOAccelGLContext2)
class NV2DContext : public IOAccel2DContext2 {
    OSDeclareDefaultStructors(NV2DContext)
    NM_TAHOE_FWD(NV2DContext)
public:
#if defined(NM_ERA) && NM_ERA < 1015
    void blitCopy(IOAccelEvent* a0, IOAccelResource2* a1, IOAccelResource2* a2, IOAccel2DBlitRectStruc* a3, unsigned int a4) APPLE_KEXT_OVERRIDE { ALOG("NV2DContext::blitCopy"); }
    void blitFill(IOAccelEvent* a0, unsigned int a1, IOAccelResource2* a2, IOAccel2DBlitRectStruc* a3, unsigned int a4) APPLE_KEXT_OVERRIDE { ALOG("NV2DContext::blitFill"); }
#else
    void _pure362() APPLE_KEXT_OVERRIDE { ALOG("NV2DContext::_pure362"); }
    void _pure363() APPLE_KEXT_OVERRIDE { ALOG("NV2DContext::_pure363"); }
#endif
};
OSDefineMetaClassAndStructors(NV2DContext, IOAccel2DContext2)
class NVCLContext : public IOAccelCLContext2 {
    OSDeclareDefaultStructors(NVCLContext)
    NM_TAHOE_FWD(NVCLContext)
public:
};
OSDefineMetaClassAndStructors(NVCLContext, IOAccelCLContext2)
class NVVideoContext : public IOAccelVideoContext2 {
    OSDeclareDefaultStructors(NVVideoContext)
    NM_TAHOE_FWD(NVVideoContext)
public:
};
OSDefineMetaClassAndStructors(NVVideoContext, IOAccelVideoContext2)
