#!/usr/bin/env python3
"""Output colour controls reach the head's mode set; vibrance keeps greys grey; headers without the range field still build.

nvrmApplyColorPrefs() is lifted from nvrm-fb.cpp and run against a fake head config with and without the colorRange member
(kexts/NVRM/patches/620-color-range.patch adds it); the saturation matrix is compiled from nvrm-fb-safety.h. The control is the
old driver: no setting was ever applied, a mode set always asked NVKMS for automatic format, depth and range."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['if (full) nvrmApplyColorPrefs(vh);', 'nvrmfb_color_vibrance', 'PE_parse_boot_argn("nvfbbpc"', 'PE_parse_boot_argn("nvfbrange"',
             'nvrmSaturationMatrix(gVibrance, sm)', 'friend void nvrmfbColorChanged(bool needsModeset)']:
    assert need in src, 'missing: ' + need
a = src.index('template <typename C> static auto nvrmSetColorFormat'); b = src.index('static void nvrmHeadConfigBaseline')
lifted = src[a:b].replace('static void nvrmApplyColorPrefs(struct NvKmsKapiHeadRequestedConfig *hr)', 'template <class H> static void nvrmApplyColorPrefs(H *hr)')   # same body, any config type
patch = (root / 'kexts/NVRM/patches/620-color-range.patch').read_text()
assert '+    NvU32 colorRange;' in patch and 'colorRangeSpecified = NV_TRUE' in patch
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstdint>
#include <initializer_list>
#include "nvrm-fb-safety.h"
typedef uint32_t NvU32;
static const NvU32 kNvrmColorFormatAuto = 3;
static int gColorBpc = 0, gColorFormat = 3, gColorRange = 0, gVibrance = 0;
#define NV_TRUE 1
struct ModeSetNew { NvU32 colorFormat = 3, colorBpc = 0, colorRange = 0; };
struct ModeSetOld { NvU32 colorFormat = 3, colorBpc = 0; };     // headers without 620-color-range.patch
template <class M> struct Head { M modeSetConfig; };
''' + lifted + r'''
int main() {
    Head<ModeSetNew> n; Head<ModeSetOld> o;
    nvrmApplyColorPrefs(&n); nvrmApplyColorPrefs(&o);              // nothing set: nothing changes
    assert(n.modeSetConfig.colorFormat == 3 && n.modeSetConfig.colorBpc == 0 && n.modeSetConfig.colorRange == 0 && o.modeSetConfig.colorBpc == 0);
    gColorBpc = 10; gColorFormat = 2; gColorRange = 2;
    nvrmApplyColorPrefs(&n); nvrmApplyColorPrefs(&o);
    assert(n.modeSetConfig.colorBpc == 10 && n.modeSetConfig.colorFormat == 2 && n.modeSetConfig.colorRange == 2);   // 10 bpc, 4:4:4, limited
    assert(o.modeSetConfig.colorBpc == 10 && o.modeSetConfig.colorFormat == 2);                                     // old header: still builds, range skipped
    assert(nvrmColorBpcValid(0) && nvrmColorBpcValid(12) && !nvrmColorBpcValid(7) && !nvrmColorBpcValid(16));
    assert(nvrmColorFormatValid(3) && !nvrmColorFormatValid(4) && nvrmColorRangeValid(2) && !nvrmColorRangeValid(3));
    int m[3][3];
    nvrmSaturationMatrix(0, m);                                                     // 0 = identity
    for (int r = 0; r < 3; r++) for (int c = 0; c < 3; c++) assert((int)(m[r][c] - (r == c ? 65536 : 0)) >= -1 && (int)(m[r][c] - (r == c ? 65536 : 0)) <= 1);
    for (int v : {-100, -50, 0, 37, 100}) {                                         // greys stay grey at every setting
        nvrmSaturationMatrix(v, m);
        for (int r = 0; r < 3; r++) assert(m[r][0] + m[r][1] + m[r][2] == 65536);
    }
    nvrmSaturationMatrix(-100, m);                                                  // -100 = monochrome: every row is the luma weights
    for (int r = 1; r < 3; r++) for (int c = 0; c < 3; c++) assert(m[r][c] == m[0][c]);
    assert(m[0][1] == 46871);
    nvrmSaturationMatrix(100, m);                                                   // +100 doubles the chroma: diagonal above 1.0, off-diagonals negative
    assert(m[0][0] > 65536 && m[0][1] < 0 && m[1][1] > 65536);
    nvrmSaturationMatrix(900, m); int hi = m[0][0]; nvrmSaturationMatrix(100, m); assert(hi == m[0][0]);   // clamped
    puts("PASS: depth, format and range reach the head config (old headers still build), vibrance keeps greys, is identity at 0, mono at -100");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    r = subprocess.run(['clang++','-std=c++17','-Wall',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')], capture_output=True, text=True)
    if r.returncode: print(r.stderr[-2500:]); raise SystemExit(1)
    subprocess.run([str(d/'t')],check=True)
