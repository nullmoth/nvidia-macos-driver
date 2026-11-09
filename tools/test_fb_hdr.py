#!/usr/bin/env python3
"""HDR10 and 30-bit scanout, end to end across NVRMFB and NVAccel, off unless nvfbhdr=1 / nvfb10bit=1.

Pixel conversions are tested in tools/test_iop_10bit.c; here: the display's HDR block is read from a real CTA-861 layout, the
scanout reply flag is the same bit on both sides, and the call sites are present: HDR timings advertised only for PQ
displays, an HDR10 timing is a different mode from the SDR timing of the same raster, a 30-bit surface for the mode set,
the infoframe/colorimetry/passthrough on every config with the gamma ramp off, and the accelerator's converter keyed on the
reply flag. The control is the old driver: SDR only, an 8-bit surface, one copy loop."""
import filecmp, pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
acc = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
for need in ['r.supportedDynamicRangeModes |= kIORangeDynamicRangeHDR10', 'if (gHdrOn && fHdrCapable) {',
             '(d.dynamicRange == kIODynamicRangeHDR10) == fHdrActive', 'NvKmsSurfaceMemoryFormatX2B10G10R10',
             'r->flags = fScan30 ? NVRM_VRAM_SCANOUT_30BIT : 0;', 'mc.hdrInfoFrame.eotf = NVKMS_INFOFRAME_EOTF_ST2084',
             'mc.colorimetry = NVKMS_OUTPUT_COLORIMETRY_BT2100', 'mc.postcompColorPassthrough = NV_TRUE', 'mc.olut.enabled = NV_FALSE',
             'attribute == kIOFBHDRMetaDataAttribute', 'PE_parse_boot_argn("nvfbhdr"', 'PE_parse_boot_argn("nvfb10bit"']:
    assert need in fb, 'missing in NVRMFB: ' + need
for need in ['q.flags & NVRM_VRAM_SCANOUT_30BIT', 'nv_iop_convert_row(dp, sp, w, srcFmt, dst30)', 'fmt != NV_IOP_FMT_RGHA']:
    assert need in acc, 'missing in NVAccel: ' + need
for a in ['NVRMFB', 'NVRM', 'NMIntelFB']:
    assert 'NVRM_VRAM_SCANOUT_30BIT 0x1u' in (root / f'kexts/{a}/nvrm_vram_abi.h').read_text(), a
assert filecmp.cmp(root / 'kexts/NVRMFB/nvrm_vram_abi.h', root / 'kexts/NVRM/nvrm_vram_abi.h', shallow=False)
fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include "nvrm-fb-safety.h"
// EDID with a CTA-861 extension carrying: a video data block, then the HDR static metadata block
static void edid(unsigned char *e, bool pq) {
    memset(e, 0, 256); e[126] = 1;
    unsigned char *x = e + 128; x[0] = 0x02; x[1] = 3;
    unsigned i = 4;
    x[i++] = (2 << 5) | 2; x[i++] = 0x10; x[i++] = 0x1f;                    // video data block, 2 bytes
    x[i++] = (7 << 5) | 6; x[i++] = 6; x[i++] = pq ? 0x05 : 0x01; x[i++] = 0x01; x[i++] = 0x6b; x[i++] = 0x5b; x[i++] = 0x20;
    x[2] = i;                                                                // where the detailed timings start
}
int main() {
    unsigned char e[256]; unsigned code = 0;
    edid(e, true);  assert(nvrmEdidHdrPq(e, 256, &code) && code == 0x6b);
    edid(e, false); assert(!nvrmEdidHdrPq(e, 256, &code));                    // HDR block without PQ: no HDR10
    memset(e, 0, 256); assert(!nvrmEdidHdrPq(e, 256, &code));                 // no extension
    edid(e, true); assert(!nvrmEdidHdrPq(e, 128, &code) && !nvrmEdidHdrPq(nullptr, 256, &code));
    edid(e, true); e[128 + 4 + 3 + 1] = 5;                                    // a different extended tag
    assert(!nvrmEdidHdrPq(e, 256, &code));
    assert(nvrmHdrLuminanceFromCode(0) == 0 && nvrmHdrLuminanceFromCode(32) == 100 && nvrmHdrLuminanceFromCode(64) == 200);
    assert(nvrmHdrLuminanceFromCode(0x6b) > 500 && nvrmHdrLuminanceFromCode(0x6b) < 1000);                   // 0x6b = 107: about 50 * 2^(107/32)
    assert(nvrmHdrLuminanceFromCode(255) > 12000 && nvrmHdrLuminanceFromCode(255) < 13000);   // 50 * 2^(255/32)
    puts("PASS: HDR block parsed from a CTA-861 extension, PQ required, luminance code decoded");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
