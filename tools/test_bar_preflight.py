#!/usr/bin/env python3
"""Compile boundary tests and fault-inject the actual pre-PCI staging block."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / "kexts/NVRM/NVRM.cpp").read_text(encoding="utf-8")
begin = source.index("    // Fully construct the replacement list")
end = source.index('    LOG("bar1: placing BAR1', begin)
staging = source[begin:end]
function = source[source.index("bool NVRM::placeLargeBar1()") :]
assert function.index("advertised host windows exceed") < function.index("configWrite")
assert function.index("preflight ready") < function.index("configWrite")
assert "withRange" not in function[function.index("configWrite") : function.index("void NVRM::publishGpuProfile")]

harness = r'''
#include <cassert>
#include <cstdint>
#include <vector>
#include <algorithm>
#include "nvrm_bar_limits.h"
using UInt64 = unsigned long long;
static int live = 0, failRange = 0, rangeCalls = 0, insertCalls = 0, failInsert = 0;
static bool failArray = false;
static int hardwareWrites = 0;
struct Obj {
    int refs = 1;
    Obj() { ++live; }
    virtual ~Obj() { --live; }
    void retain() { ++refs; }
    void release() { if (!--refs) delete this; }
};
struct IOMemoryDescriptor : Obj {
    UInt64 address, length;
    IOMemoryDescriptor(UInt64 a, UInt64 n) : address(a), length(n) {}
    UInt64 getPhysicalAddress() { return address; }
    UInt64 getLength() { return length; }
};
struct IODeviceMemory : IOMemoryDescriptor {
    IODeviceMemory(UInt64 a, UInt64 n) : IOMemoryDescriptor(a, n) {}
    static IODeviceMemory *withRange(UInt64 a, UInt64 n) {
        if (++rangeCalls == failRange) return nullptr;
        return new IODeviceMemory(a, n);
    }
};
struct OSArray : Obj {
    std::vector<Obj *> entries;
    static OSArray *withCapacity(unsigned) { return failArray ? nullptr : new OSArray; }
    unsigned getCount() { return static_cast<unsigned>(entries.size()); }
    Obj *getObject(unsigned i) { return entries.at(i); }
    bool setObject(Obj *p) {
        if (++insertCalls == failInsert) return false;
        assert(p); p->retain(); entries.push_back(p); return true;
    }
    ~OSArray() { for (auto *p : entries) p->release(); }
};
#define OSDynamicCast(T, p) dynamic_cast<T *>(p)
#define OSSafeReleaseNULL(p) do { if (p) (p)->release(); (p) = nullptr; } while (0)
#define LOG(...) do {} while (0)

static bool attempt(OSArray *mem, IOMemoryDescriptor *bar0) {
    UInt64 bar1Base = 0x10000000000ULL, bar1Size = 256ULL << 20;
    UInt64 bar3Base = bar1Base + bar1Size, bar3Size = 32ULL << 20, bar0Size = 16ULL << 20;
    unsigned lo10 = 0xc4000000, old18 = 1, lo14 = 0xb000000c;
    const UInt64 bar3Old = 0xc0000000;
__STAGING__
    // Stand-in for the first real configWrite, after the exact source block.
    ++hardwareWrites;
    assert(arr->getCount() == 4); // BAR0 + new BAR1 + new BAR3 + unrelated BAR
    for (auto *p : arr->entries) {
        auto *m = dynamic_cast<IOMemoryDescriptor *>(p);
        assert(m->getPhysicalAddress() != 0x1b0000000ULL); // full old 64-bit BAR1
        assert(m->getPhysicalAddress() != bar3Old);
    }
    return true;
}
static void stageTests(bool haveBar0) {
    OSArray *mem = new OSArray;
    auto add = [&](UInt64 address, UInt64 length) {
        auto *m = new IOMemoryDescriptor(address, length); mem->setObject(m); m->release(); return m;
    };
    auto *bar0 = haveBar0 ? add(0xc4000000, 16ULL << 20) : nullptr;
    add(0x1b0000000ULL, 256ULL << 20); add(0xc0000000, 32ULL << 20); add(0xd0000000, 4096);
    const int baseline = live;
    for (int failure = 0; failure <= (haveBar0 ? 3 : 4); ++failure) {
        failArray = failure == 1;
        failRange = failure >= 2 ? failure - 1 : 0;
        rangeCalls = insertCalls = hardwareWrites = 0;
        const bool ok = attempt(mem, bar0);
        assert(ok == (failure == 0)); assert(hardwareWrites == (ok ? 1 : 0)); assert(live == baseline);
    }
    failArray = false; failRange = 0;
    for (int failure = 1; failure <= 4; ++failure) {
        failInsert = failure; rangeCalls = insertCalls = hardwareWrites = 0;
        assert(!attempt(mem, bar0)); assert(hardwareWrites == 0); assert(live == baseline);
    }
    failInsert = 0; mem->release(); assert(live == 0);
}
int main() {
    const UInt64 limit = nvrmBarAddressMax(46, 12);
    assert(limit == 0xfffffffefffULL);
    assert(nvrmBarAddressMax(39, 12) == 0x7fffffffffULL);
    assert(nvrmBarAddressMax(64, 12) == limit);
    assert(nvrmBarAddressMax(0, 12) == 0);
    UInt64 end = 0;
    assert(!nvrmClipBarWindow(0x3800c0000000ULL, 0x3800ffffffffULL, limit, &end));
    assert(nvrmClipBarWindow(0x100000000ULL, 0x7fffffffffULL, limit, &end));
    assert(end == 0x7fffffffffULL);
    assert(nvrmClipBarWindow(limit - 0x10000000, ~0ULL, limit, &end) && end == limit);
    assert(!nvrmClipBarWindow(5, 4, limit, &end));
    assert(!nvrmClipBarWindow(limit + 1, ~0ULL, limit, &end));
    // Compare descriptor acceptance to the XNU condition, across boundary pages.
    for (UInt64 n : {32ULL << 20, 256ULL << 20, 8ULL << 30}) {
        assert(((limit - n + 1 + n + 4095) >> 12) <= 0xffffffffULL);
        assert(((limit - n + 2 + n + 4095) >> 12) > 0xffffffffULL);
    }
    stageTests(true); stageTests(false);
}
'''.replace("__STAGING__", staging)

cxx = os.environ.get("CXX") or shutil.which("clang++") or shutil.which("g++")
if not cxx:
    raise SystemExit("A C++ compiler is required")
test_root = (root / "build-out/tests").resolve()
assert test_root.is_relative_to(root.resolve())
test_root.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="bar-preflight-", dir=test_root) as temp:
    temp = Path(temp)
    src = temp / "test.cpp"
    binary = temp / ("test.exe" if os.name == "nt" else "test")
    src.write_text(harness, encoding="utf-8")
    subprocess.run([cxx, "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(root / "kexts/NVRM"), str(src), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
print("PASS: XNU/CPU address boundaries, actual staging allocation/insertion failures, no preflight PCI writes, cleanup and stale 64-bit BAR removal")
