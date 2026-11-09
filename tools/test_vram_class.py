#!/usr/bin/env python3
"""BAR1 fragmentation on a 256 MB window: an offline model of RM's mapping window (first fit, address ordered) driven by
the surface sizes of issues #364 and #462, with exact sizes and with the size classes of nvrm-vram-class.h."""
import shutil, subprocess, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[1]
fb = (root / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text(encoding="utf-8")
failures = []
def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok: failures.append(name)

hdr = root / "kexts/NVRMFB/fb/nvrm-vram-class.h"
arm("the size class header exists", hdr.exists())
harness = r'''
#include <cstdio>
#include <vector>
#include <algorithm>
#include "nvrm-vram-class.h"
typedef unsigned long long u64;
#define CHECK(name, cond) do { const bool c_ = (cond); std::printf("%s %s\n", c_ ? "ok  " : "FAIL", name); if (!c_) bad++; } while (0)
static const u64 MB = 1ULL << 20;
struct Blk { u64 a, n; };
static std::vector<Blk> used;
static u64 inUse() { u64 s = 0; for (auto &b : used) s += b.n; return s; }
static bool alloc(u64 total, u64 n, u64 *addr) {   // RM's window: first fit over an address-ordered free list
    std::sort(used.begin(), used.end(), [](const Blk &x, const Blk &y){ return x.a < y.a; });
    u64 cur = 0;
    for (auto &b : used) { if (b.a - cur >= n) { *addr = cur; used.push_back({cur, n}); return true; } cur = b.a + b.n; }
    if (total - cur >= n) { *addr = cur; used.push_back({cur, n}); return true; }
    return false;
}
// the sizes the reports show: the 1080p window surface, larger and smaller windows, cursor and menu sized ones
static const u64 kSizes[] = { 8323072, 8323072, 8323072, 8323072, 2088960, 4177920, 524288, 1048576, 3145728, 6291456, 12582912 };
static unsigned rnd(unsigned &s) { s = s * 1664525u + 1013904223u; return s >> 8; }
struct Result { unsigned refused, frag; };
static Result run(bool classes) {
    const u64 total = 243 * MB;   // the budget the logs show for a 256 MB BAR1
    used.clear(); std::vector<Blk> live; Result r = {0, 0}; unsigned s = 7;
    for (int step = 0; step < 60000; step++) {
        const bool grow = live.empty() || (rnd(s) % 100) < (inUse() < 200 * MB ? 70u : 35u);
        if (grow) {
            u64 n = kSizes[rnd(s) % (sizeof kSizes / sizeof *kSizes)]; if (classes) n = nvrmVramSizeClass(n);
            u64 a;
            if (!alloc(total, n, &a)) { r.refused++; if (inUse() + n <= total) r.frag++; continue; }
            live.push_back({a, n});
        } else {
            size_t i = rnd(s) % live.size(); Blk l = live[i]; live.erase(live.begin() + i);
            for (size_t k = 0; k < used.size(); k++) if (used[k].a == l.a) { used.erase(used.begin() + k); break; }
        }
    }
    return r;
}
int main() {
    int bad = 0;
    Result a = run(false), c = run(true);
    std::printf("exact sizes:   %u refused, %u with the bytes free but no hole\n", a.refused, a.frag);
    std::printf("size classes:  %u refused, %u with the bytes free but no hole\n", c.refused, c.frag);
    CHECK("the model reproduces refusals with free bytes but no contiguous hole (the #364 shape)", a.frag > 0);
    CHECK("size classes cut those refusals by at least a third", c.frag * 3 <= a.frag * 2);
    CHECK("size classes: 8323072 -> 8 MB, 2088960 -> 2 MB, 3145728 -> 4 MB, 12582912 -> 16 MB, 20 MB unchanged, tiny -> 512 KB",
          nvrmVramSizeClass(8323072) == 8 * MB && nvrmVramSizeClass(2088960) == 2 * MB && nvrmVramSizeClass(3145728) == 4 * MB &&
          nvrmVramSizeClass(12582912) == 16 * MB && nvrmVramSizeClass(20 * MB) == 20 * MB && nvrmVramSizeClass(4096) == 512 * 1024);
    return bad;
}
'''
if hdr.exists():
    cxx = shutil.which("clang++") or shutil.which("g++")
    with tempfile.TemporaryDirectory(prefix="vram-class-") as t:
        t = Path(t); (t / "t.cpp").write_text(harness)
        subprocess.run([cxx, "-std=c++17", "-O1", "-Wall", "-Wextra", "-I", str(hdr.parent), str(t / "t.cpp"), "-o", str(t / "t")], check=True)
        r = subprocess.run([str(t / "t")], capture_output=True, text=True); print(r.stdout, end="")
        if r.returncode: failures.append("class harness")
arm("a small BAR1 rounds request sizes to classes", "want = nvrmVramSizeClass(want)" in fb)
if failures:
    print("FAILED:", ", ".join(failures)); sys.exit(1)
print("PASS")
