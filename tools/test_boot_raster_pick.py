#!/usr/bin/env python3
"""A head's boot raster must be the panel's native resolution, not whatever NVKMS enumerates first.

fBootW/fBootH is taken from the mode picked here, and it sets the raster/scaler ceilings and which
refresh rates addKmsRefreshModes() may publish. On a second monitor NVKMS often flags no preferred
mode; taking the first validated mode then made the boot raster depend on enumeration order, and a
small raster landing first capped the native high-refresh modes out of the list (second display at
60/75 Hz or black). The real selection loop is lifted from nvrm-fb.cpp. The negative control is the
pre-fix loop (first validated mode) and must pick the small raster."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()

anchor = 'bool have = false; NvU64 bestArea = 0; NvU32 bestClock = 0;'
start = source.index(anchor)
end = source.index('if (!have) { FBLOG("no valid mode"); return false; }', start)
end = source.index('\n', end)
region = source[start:end]
assert 'area > bestArea' in region, 'the boot-raster pick no longer prefers the native (largest) raster'

# Pre-fix loop: first validated mode wins, upgraded only on a preferred flag.
control = r'''    bool have = false;
    for (NvU32 i = 0; i < 256; i++) {
        struct NvKmsKapiDisplayMode m = {}; NvBool valid = NV_FALSE, preferred = NV_FALSE;
        int r = fKms->getDisplayMode(fDev, fDisplay, i, &m, &valid, &preferred);
        if (r < 0) break;
        if (r == 0 || !valid) continue;
        if (!have || preferred) { fMode = m; have = true; }
        if (preferred) break;
    }'''

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <vector>
#include "nvrm-fb-safety.h"
using NvU32=uint32_t; using NvU64=uint64_t; using NvBool=bool;
enum { NV_TRUE=1, NV_FALSE=0 };
#define FBLOG(...) ((void)0)
struct NvKmsKapiDisplayModeTimings { NvU32 pixelClockHz=0,hVisible=0,vVisible=0,hTotal=0,vTotal=0,refreshRate=0; };
struct NvKmsKapiDisplayMode { NvKmsKapiDisplayModeTimings timings; };
struct Row { NvKmsKapiDisplayMode m; bool valid; bool preferred; };
struct Kms {
    std::vector<Row> rows;
    int getDisplayMode(void *, unsigned, NvU32 i, NvKmsKapiDisplayMode *m, NvBool *valid, NvBool *pref) {
        if (i >= rows.size()) return -1;
        *m = rows[i].m; *valid = rows[i].valid; *pref = rows[i].preferred; return 1;
    }
};
struct FB {
    Kms *fKms=nullptr; void *fDev=this; unsigned fDisplay=1;
    NvKmsKapiDisplayMode fMode{};
    bool pick() {
SELECTION
        return true;
    }
};
static NvKmsKapiDisplayMode mode(NvU32 w, NvU32 h, NvU32 hz) {
    NvKmsKapiDisplayMode m; auto &t = m.timings;
    t.hVisible=w; t.vVisible=h; t.hTotal=w+160; t.vTotal=h+90;
    t.pixelClockHz=(NvU32)((NvU64)t.hTotal*t.vTotal*hz); t.refreshRate=hz*1000; return m;
}
int main() {
    // No preferred flag: a small raster is enumerated first, the native 1440p last, at two rates.
    std::vector<Row> list = {
        { mode(1024,768,60), true, false },
        { mode(1280,720,60), true, false },
        { mode(1920,1080,60), true, false },
        { mode(2560,1440,60), true, false },
        { mode(2560,1440,144), true, false },
    };
    { Kms k; k.rows=list; FB f; f.fKms=&k; assert(f.pick());
      assert(f.fMode.timings.hVisible==2560 && f.fMode.timings.vVisible==1440 &&
             "boot raster must be the native 1440p raster, not the first-enumerated 1024x768");
      // tie on area resolves to the higher pixel clock => the native raster's top rate
      assert(f.fMode.timings.refreshRate==144000); }
    // A preferred flag wins outright even when it is not first.
    { std::vector<Row> p = list; p[2].preferred = true; // 1920x1080 flagged preferred
      Kms k; k.rows=p; FB f; f.fKms=&k; assert(f.pick());
      assert(f.fMode.timings.hVisible==1920 && f.fMode.timings.vVisible==1080); }
    { Kms k; k.rows = {
          { mode(65536,1440,60), true, true },
          { mode(2560,0,60), true, true },
          { mode(2560,1440,60), true, false } };
      FB f; f.fKms=&k; assert(f.pick());
      assert(f.fMode.timings.hVisible==2560 && f.fMode.timings.vVisible==1440); }
    { Kms k; k.rows = { { mode(0,1440,60), true, true },
                       { mode(2560,65536,60), true, true } };
      FB f; f.fKms=&k; assert(!f.pick()); }
    assert(nvrmKmsRasterValid(1,1) && nvrmKmsRasterValid(65535,65535));
    assert(!nvrmKmsRasterValid(65536,1) && !nvrmKmsRasterValid(1,0));
    printf("OK\n");
    return 0;
}
'''

def build_and_run(selection):
    src = fixture.replace('SELECTION', selection)
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(src)
        subprocess.run(['c++', '-std=c++17', '-O0', '-I', str(root / 'kexts/NVRMFB/fb'),
                        str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)

r = build_and_run(region)
assert r.returncode == 0 and r.stdout.strip() == 'OK', \
    f'fixed pick failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print('fixed source: PASS (native raster chosen with no preferred flag; preferred flag still wins)')

r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect the first-enumerated boot raster bug'
print('negative control (first validated mode): FAILS as required (chose the small raster)')
print('PASS')
