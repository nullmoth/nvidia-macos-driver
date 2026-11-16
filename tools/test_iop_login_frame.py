#!/usr/bin/env python3
"""Run the production display transaction against bounded host buffers.

Pixel-helper tests alone miss a display pipe that still refuses l10r. Exercise
performTransaction, source mapping and the Intel frame lease together, including
padding, invalid input, BGRA flips/copies and failure cleanup, under sanitizers.
No driver is built, installed or loaded.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def method(source, signature):
    start = source.index(signature + "\n{")
    return source[start:source.index("\n}\n", start) + 3]


HARNESS = r'''
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#include "nv_iop_pixel.h"
#include "panel_bridge_abi.h"
#include "nvrm_vram_abi.h"
using IOReturn = int;
using SInt32 = int32_t;
using SInt64 = int64_t;
enum { kIOReturnSuccess, kIOReturnBadArgument, kIOReturnUnsupported,
       kIOReturnNotAttached, kIOReturnOverrun, kIOReturnNoMemory,
       kIOReturnVMError, kIOReturnNotReady, kIOReturnBusy };
constexpr unsigned kNvMaxFB = 5, kIOMapReadOnly = 1;
#define ALOG(...) ((void)0)
#define NVIOPLOG(...) ((void)0)
#define NVIOP_FAIL(rc, ...) do { ++gCntIopFail; return (rc); } while (0)
static SInt64 gCntIopFail, gCntIopEmpty, gCntIopSrcRes, gCntIopSrcSurf,
    gCntIopOk, gCntIopOkHead[kNvMaxFB], gCntIopBlank, gCntIopCopyUs, gCntIopL10r;
static int gNvIop = 1, gIopMemcpy, gIopDirect;
static SInt64 OSAddAtomic64(SInt64 n, SInt64 *p) { const auto old = *p; *p += n; return old; }
static void OSIncrementAtomic(SInt32 *p) { ++*p; }
static uint64_t mach_absolute_time() { return 1000; }
static void absolutetime_to_nanoseconds(uint64_t t, uint64_t *out) { *out = t; }

struct OSObject {
    unsigned refs = 1;
    virtual ~OSObject() = default;
    void retain() { ++refs; }
    void release() { assert(refs > 1); --refs; }
};
#define OSDynamicCast(T, p) dynamic_cast<T *>(p)
struct OSSymbol {
    const char *name;
    static const OSSymbol *withCStringNoCopy(const char *s) { return new OSSymbol{s}; }
    void release() const { delete this; }
};
struct IOMemoryMap {
    void *address; size_t length;
    size_t getLength() const { return length; }
    uintptr_t getVirtualAddress() const { return reinterpret_cast<uintptr_t>(address); }
    void release() { delete this; }
};
struct IOMemoryDescriptor : OSObject {
    void *address = nullptr; size_t length = 0;
    bool failPrepare = false, failMap = false;
    int wired = 0;
    size_t getLength() const { return length; }
    int prepare() { if (failPrepare) return kIOReturnVMError; ++wired; return kIOReturnSuccess; }
    void complete() { assert(wired == 1); --wired; }
    IOMemoryMap *map(unsigned mode) {
        assert(mode == kIOMapReadOnly);
        return failMap ? nullptr : new IOMemoryMap{address, length};
    }
};
struct IOSurface {
    unsigned fmt = NV_IOP_FMT_L10R, width = 5, height = 3, pitch = 32;
    IOMemoryDescriptor *md = nullptr;
    unsigned getSurfaceID() const { return 77; }
    unsigned getPixelFormat() const { return fmt; }
    unsigned long long getWidth() const { return width; }
    unsigned long long getHeight() const { return height; }
    unsigned long long getBytesPerRow() const { return pitch; }
    IOMemoryDescriptor *getMemoryDescriptor() const { return md; }
};
struct NVVidMemory { void *fKva = nullptr; unsigned long long fBytes = 0; };
struct NVResource : OSObject { unsigned char fields[0x100] = {}; NVVidMemory *memory = nullptr; };
static NVVidMemory *nvres_vidmem(NVResource *r) { return r->memory; }
struct IOAccelDisplayPipeTransaction2 {
    IOSurface *surface; OSObject *resource = nullptr;
    IOSurface *getPlaneIOSurface(unsigned, unsigned) { return surface; }
    OSObject *getPlaneResource(unsigned, unsigned) { return resource; }
};
struct IOService : OSObject {
    bool intel = false, blocked = false, legacy = false, leased = false;
    int begins = 0, ends = 0, cancels = 0;
    unsigned completed = NMINTELFB_CANCEL;
    IOReturn endResult = kIOReturnSuccess;
    NMIntelFBFrame frame = {};
    bool metaCast(const char *name) { return intel && !strcmp(name, "NMIntelFB"); }
    IOReturn callPlatformFunction(const OSSymbol *s, bool, void *p, void *, void *, void *) {
        auto *q = static_cast<NMIntelFBFrame *>(p);
        assert(q->version == NMINTELFB_FRAME_VERSION);
        if (!strcmp(s->name, NMINTELFB_FN_BEGIN)) {
            ++begins;
            if (blocked) return kIOReturnBusy;
            if (legacy) return kIOReturnUnsupported;
            assert(!leased); leased = true; *q = frame; return kIOReturnSuccess;
        }
        assert(!strcmp(s->name, NMINTELFB_FN_END) && leased);
        leased = false; ++ends; completed = q->result;
        if (completed == NMINTELFB_CANCEL) ++cancels;
        return endResult;
    }
};
struct NVDisplayPipe {
    enum { kIopSrcSlots = 4 };
    struct NVIopSrc { IOMemoryMap *map = nullptr; IOMemoryDescriptor *md = nullptr; unsigned id = 0; };
    NVIopSrc fIopSrc[kIopSrcSlots]; unsigned fIopNext = 0;
    void iopDropSources();
    const void *iopMapSurface(unsigned, IOMemoryDescriptor *, unsigned long long);
    IOReturn performTransaction(IOAccelDisplayPipeTransaction2 *);
};
struct NVAccel {
    static NVAccel *gAccel;
    IOService *fb = nullptr; NVRMVramRequest scanout = {};
    IOService *pipeFramebuffer(NVDisplayPipe *, unsigned *head) { *head = 0; return fb; }
    IOReturn askFramebufferForScanout(NVRMVramRequest *q, IOService *) {
        assert(q->version == NVRM_VRAM_ABI_VERSION); *q = scanout; return kIOReturnSuccess;
    }
};
NVAccel *NVAccel::gAccel;
static bool ready = true, current = true, allowFlip = false, directDone = false;
static int waits, copyChecks, homes, memcpyCalls;
static bool nvSurfWaitReady(unsigned id) { assert(id == 77); ++waits; return ready; }
static bool nvAccelSurfCopyReady(IOSurface *, unsigned) { ++copyChecks; return current; }
static bool nvAccelIopFlip(IOService *fb, unsigned, IOSurface *s, void *) {
    return allowFlip && !fb->intel && s->fmt == NV_IOP_FMT_BGRA;
}
static void nvAccelIopHome(IOService *, unsigned) { ++homes; }
@@LEASE@@
enum NVIntelCopyResult { kIntelNoCopy, kIntelCpuCopy, kIntelGpuCopy, kIntelHeld, kIntelUncertain };
static NVIntelCopyResult nvAccelIntelDirect(NVAccel *, IOService *, unsigned, IOSurface *s, const NMIntelFBFrame &) {
    return directDone && s->fmt == NV_IOP_FMT_BGRA ? kIntelGpuCopy : kIntelNoCopy;
}
static void nvIntelCopyRows(void *dst, unsigned dp, const void *src, unsigned sp, unsigned bytes, unsigned h) {
    ++memcpyCalls;
    for (unsigned y = 0; y < h; ++y)
        memcpy(static_cast<char *>(dst) + y * dp, static_cast<const char *>(src) + y * sp, bytes);
}
@@DROP@@
@@MAP@@
@@TRANSACTION@@

static int bad;
#define CHECK(cond) do { if (!(cond)) { std::printf("FAIL line %d: %s\n", __LINE__, #cond); ++bad; } } while (0)
int main(int argc, char **argv) {
    assert(argc == 2); const std::string test = argv[1];
    const bool bgra = test.find("bgra") != std::string::npos || test == "resource";
    const bool intel = test.find("intel") != std::string::npos;
    const std::string kind = test.compare(0, 6, "intel-") == 0 ? test.substr(6) : test;
    IOService fb; fb.intel = intel;
    if (test == "intel-blocked") fb.blocked = true;
    if (test == "intel-legacy") fb.legacy = true;
    if (test == "intel-finish-refused") fb.endResult = kIOReturnBusy;
    if (test == "intel-memcpy") gIopMemcpy = 1;
    if (test == "intel-direct" || test == "intel-bgra-direct") gIopDirect = 2;
    directDone = test == "intel-bgra-direct";
    allowFlip = test == "bgra-flip" || test == "l10r-flip-refused";
    IOSurface s; s.fmt = bgra ? NV_IOP_FMT_BGRA : NV_IOP_FMT_L10R;
    if (test == "l10r-exact") s.pitch = 20;
    if (test == "source-wide") s.pitch = 64;
    unsigned dpitch = test == "l10r-exact" ? 20 : test == "source-wide" ? 24 : 40;
    const unsigned spr = s.pitch / 4, dpr = dpitch / 4;
    constexpr unsigned guard = 0x12345678, padding = 0xdeadbeef;
    std::vector<unsigned> src(2 + spr * s.height, guard), dst(2 + dpr * s.height, guard);
    std::fill(dst.begin() + 1, dst.end() - 1, padding);
    for (unsigned y = 0; y < s.height; ++y) for (unsigned x = 0; x < spr; ++x)
        src[1 + y * spr + x] = bgra ? 0x40102030u + y * 0x10000u + x :
            (((x + y) & 3u) << 30) | (((x * 211 + y * 101) & 1023u) << 20) |
            (((x * 43 + y * 137) & 1023u) << 10) | ((x * 157 + y * 67) & 1023u);
    const auto original = src;
    IOMemoryDescriptor md; md.address = src.data() + 1; md.length = spr * s.height * 4;
    s.md = &md;
    NVAccel a; a.fb = &fb;
    a.scanout.kva = dst.data() + 1; a.scanout.width = s.width;
    a.scanout.height = s.height; a.scanout.pitch = dpitch;
    fb.frame.version = NMINTELFB_FRAME_VERSION; fb.frame.kva = a.scanout.kva;
    fb.frame.width = s.width; fb.frame.height = s.height; fb.frame.pitch = dpitch;
    NVAccel::gAccel = &a;
    IOReturn expected = kIOReturnSuccess; bool copied = true;
    if (kind == "unknown-format") { s.fmt = 0x52474241; expected = kIOReturnUnsupported; }
    if (test == "short-source") { s.pitch = 16; expected = kIOReturnBadArgument; }
    if (test == "unaligned-source") { s.pitch = 23; expected = kIOReturnBadArgument; }
    if (kind == "zero-width") { s.width = 0; expected = kIOReturnBadArgument; }
    if (test == "oversized-width") { s.width = 16385; expected = kIOReturnBadArgument; }
    if (kind == "short-map") { md.length -= 4; expected = kIOReturnVMError; }
    if (test == "failed-map") { md.failMap = true; expected = kIOReturnVMError; }
    if (test == "failed-prepare") { md.failPrepare = true; expected = kIOReturnVMError; }
    if (test == "missing-descriptor") { s.md = nullptr; expected = kIOReturnNoMemory; }
    if (test == "short-destination") { a.scanout.pitch = 16; expected = kIOReturnBadArgument; }
    if (kind == "unaligned-destination") { a.scanout.pitch = 23; fb.frame.pitch = 23; expected = kIOReturnBadArgument; }
    if (test == "wrong-dimensions") { ++a.scanout.width; expected = kIOReturnBadArgument; }
    if (test == "missing-destination") { a.scanout.kva = nullptr; expected = kIOReturnNotReady; }
    if (test == "source-not-current") { current = false; copied = false; }
    if (test == "intel-not-ready") { ready = false; copied = false; }
    if (test == "intel-blocked" || test == "bgra-flip" || directDone) copied = false;
    if (expected != kIOReturnSuccess) copied = false;
    NVDisplayPipe pipe;
    NVVidMemory memory; memory.fKva = md.address; memory.fBytes = md.length;
    NVResource resource; resource.memory = &memory;
    auto *fields = reinterpret_cast<unsigned char *>(&resource);
    const unsigned short rw = s.width, rh = s.height;
    memcpy(fields + 0xb0, &rw, sizeof rw); memcpy(fields + 0xb2, &rh, sizeof rh);
    memcpy(fields + 0xb8, &s.pitch, sizeof s.pitch);
    IOAccelDisplayPipeTransaction2 txn{test == "resource" ? nullptr : &s,
                                     test == "resource" ? &resource : nullptr};
    const IOReturn result = pipe.performTransaction(&txn);
    CHECK(result == expected);
    CHECK(src == original);
    CHECK(dst.front() == guard && dst.back() == guard);
    if (copied) {
        const unsigned words = bgra ? std::min(spr, dpr) : s.width;
        for (unsigned y = 0; y < s.height; ++y) for (unsigned x = 0; x < dpr; ++x) {
            const unsigned v = x < words ? original[1 + y * spr + x] : padding;
            // Expected channel values, independent of the production conversion helper.
            const unsigned pixel = !bgra && x < words ?
                0xff000000u | (((v >> 22) & 255u) << 16) | (((v >> 12) & 255u) << 8) | ((v >> 2) & 255u) : v;
            CHECK(dst[1 + y * dpr + x] == pixel);
        }
    } else CHECK(std::all_of(dst.begin() + 1, dst.end() - 1, [](unsigned v) { return v == padding; }));
    const bool completed = copied && fb.endResult == kIOReturnSuccess;
    CHECK(gCntIopL10r == (!bgra && completed ? 1 : 0));
    if (intel && !fb.blocked && !fb.legacy) {
        CHECK(fb.begins == 1 && fb.ends == 1 && !fb.leased);
        CHECK(fb.completed == (directDone ? NMINTELFB_GPU_READY : copied ? NMINTELFB_CPU_READY : NMINTELFB_CANCEL));
    }
    if (!bgra) CHECK(memcpyCalls == 0);
    if (test == "bgra-flip") CHECK(waits == 0 && copyChecks == 0 && homes == 0 && gCntIopOk == 1);
    if (copied && !bgra) CHECK(waits == 1 && copyChecks == 1 && homes == 1);
    pipe.iopDropSources();
    CHECK(md.wired == 0 && md.refs == 1 && fb.refs == 1);
    std::printf("%s %s (return=%d expected=%d)\n", bad ? "FAIL" : "PASS", argv[1], result, expected);
    return bad ? 1 : 0;
}
'''

CASES = (
    "l10r", "l10r-exact", "source-wide", "l10r-flip-refused",
    "bgra", "bgra-flip", "resource", "intel-bgra", "intel-bgra-direct",
    "intel", "intel-memcpy", "intel-direct", "intel-legacy", "intel-finish-refused",
    "unknown-format", "short-source", "unaligned-source", "zero-width", "oversized-width",
    "short-map", "failed-map", "failed-prepare", "missing-descriptor",
    "short-destination", "unaligned-destination", "wrong-dimensions", "missing-destination",
    "source-not-current", "intel-not-ready", "intel-blocked",
    "intel-unknown-format", "intel-zero-width", "intel-short-map", "intel-unaligned-destination",
)


def run(source, output, cases, name):
    start = source.index("class NVIntelFrameLease {")
    lease = source[start:source.index("enum NVIntelCopyResult", start)]
    text = HARNESS.replace("@@LEASE@@", lease)
    for token, signature in (
        ("DROP", "void NVDisplayPipe::iopDropSources()"),
        ("MAP", "const void *NVDisplayPipe::iopMapSurface(unsigned id, IOMemoryDescriptor *md, unsigned long long need)"),
        ("TRANSACTION", "IOReturn NVDisplayPipe::performTransaction(IOAccelDisplayPipeTransaction2 *txn)"),
    ):
        text = text.replace("@@" + token + "@@", method(source, signature))
    cpp, exe = output / (name + ".cpp"), output / name
    cpp.write_text(text)
    subprocess.run([
        "xcrun", "clang++", "-std=c++17", "-O1", "-g",
        "-fsanitize=address,undefined", "-fno-omit-frame-pointer",
        "-I" + str(ROOT / "kexts/NVRM/accel"),
        "-I" + str(ROOT / "kexts/NVRM"),
        "-I" + str(ROOT / "kexts/NMIntelFB"), str(cpp), "-o", str(exe),
    ], check=True)
    results = []
    for case in cases:
        result = subprocess.run([str(exe), case], capture_output=True, text=True, timeout=15)
        assert 'AddressSanitizer' not in result.stderr and 'runtime error:' not in result.stderr, result.stderr
        if name == "transaction":
            print(result.stdout, end="")
            if result.stderr:
                print(result.stderr, end="")
        results.append(result.returncode)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT / "kexts/NVRM/accel/nvrm-accel.cpp")
    args = parser.parse_args()
    source = args.source.read_text()
    with tempfile.TemporaryDirectory(prefix="iop-login-test-") as temporary:
        output = Path(temporary)
        results = run(source, output, CASES, "transaction")
        if any(results):
            raise SystemExit(f"FAIL: {sum(bool(r) for r in results)} of {len(CASES)} transaction cases")
        assert 'sysctl_register_oid(&sysctl__debug_nvaccel_iop_l10r);' in source
        flip = method(source, "static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe)")
        assert 's->getPixelFormat() != 0x42475241u' in flip, "zero-copy flip must remain BGRA-only"
        controls = (
            ("refuse-l10r", "from10 = (fmt == NV_IOP_FMT_L10R);", "from10 = false;", "l10r"),
            ("raw-l10r", "dp[x] = nv_iop_l10r_to_x8r8g8b8(sp[x]);", "dp[x] = sp[x];", "l10r"),
            ("intel-raw-copy", "if (intelFrame.active() && !from10)", "if (intelFrame.active())", "intel"),
        )
        for name, old, new, case in controls:
            assert source.count(old) == 1, f"negative-control site changed: {name}"
            assert run(source.replace(old, new), output, [case], name)[0] == 1, f"negative control passed: {name}"
            print(f"PASS negative control detects {name}")
    print(f"PASS: {len(CASES)} production transaction cases, sanitizers and three negative controls")


if __name__ == "__main__":
    main()
