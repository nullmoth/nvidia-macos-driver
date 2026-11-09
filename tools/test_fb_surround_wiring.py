#!/usr/bin/env python3
"""Surround wiring (nvfbsurround=<mask>, off by default): members stand down, the master's config is copied to each member head with its slice.

surroundReplicate() is lifted from nvrm-fb.cpp and run on fake kapi types with the same field names: after a flip config for the
master (surface as wide as the group), every member head has its own display, its own mode, a source offset equal to the widths
before it, a source width of its own, and no cursor. Control: rel-29 has no group at all, so one head ever got the whole wide
surface and the other displays stayed dark."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
for need in ['PE_parse_boot_argn("nvfbsurround"', 'fHidden = nvrmSurroundIsMember(gSurroundMask, fIndex', 'static void surroundCheck();', 'surroundCheck();\n    if (!added)', 'hasDDCConnect(IOIndex) override { return !fDpyLost && !fHidden', 'surroundReplicate(cfg);', 'result = dtIndex(mode) >= 0 ? switchSurround(mode)',
             '!fSurActive) ? 1 : 0;   // one cursor plane cannot span heads', 'const unsigned s = addSurroundMode(0); if (s) n = s;']:
    assert need in src, need
# members keep their own framebuffer, so HDMI/DP audio (published per framebuffer at start) is untouched: nothing in the audio path looks at fHidden
aud = src[src.index('void NVRMNVDAFramebuffer::publishDisplayAudioPropertiesNow'):src.index('static bool nvrmEdidSerialString')]
assert 'fHidden' not in aud and 'fSurN' not in aud, 'audio publication must not depend on the surround state'
assert 'return false;   // surround member' not in src, 'members must not decline to start (audio would go)'
a = src.index('void NVRMNVDAFramebuffer::surroundReplicate'); fn = src[a:src.index('\n}\n', a) + 3]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include "nvrm-fb-safety.h"
typedef uint8_t NvBool; typedef uint16_t NvU16; typedef int16_t NvS16; typedef uint32_t NvU32; typedef uint32_t NvKmsKapiDisplay;
#define NV_TRUE 1
#define NV_FALSE 0
#define NVKMS_KAPI_LAYER_PRIMARY_IDX 0
struct Surface { int id; };
struct Mode { struct { unsigned hVisible, vVisible; } timings; };
struct LayerFlags { NvBool srcXYChanged, srcWHChanged, dstWHChanged; };
struct LayerCfg { Surface *surface; NvU16 srcX, srcY, srcWidth, srcHeight; NvS16 dstX, dstY; NvU16 dstWidth, dstHeight; };
struct LayerReq { LayerCfg config; LayerFlags flags; };
struct CursorReq { Surface *surface; struct { NvBool surfaceChanged, dstXYChanged; } flags; };
struct ModeSetCfg { NvKmsKapiDisplay displays[1]; Mode mode; };
struct HeadReq { ModeSetCfg modeSetConfig; LayerReq layerRequestedConfig[2]; CursorReq cursorRequestedConfig; };
struct NvKmsKapiRequestedModeSetConfig { NvU32 headsMask; HeadReq headRequestedConfig[4]; };
struct NvKmsKapiHeadRequestedConfig : HeadReq {};
struct NVRMNVDAFramebuffer {
    struct SurMember { NvKmsKapiDisplay display; NvU32 head; Mode mode; NvU32 x, w; };
    SurMember fSur[kNvrmSurMaxMembers]; unsigned fSurN = 0; bool fSurActive = false; unsigned fSurTotalW = 0; NvU32 fHead = 0;
    void surroundReplicate(NvKmsKapiRequestedModeSetConfig *cfg);
};
''' + fn.replace('const struct NvKmsKapiHeadRequestedConfig master = ', 'const HeadReq master = ').replace('struct NvKmsKapiHeadRequestedConfig *hr', 'HeadReq *hr').replace('struct NvKmsKapiLayerConfig &lc', 'LayerCfg &lc') + r'''
int main() {
    NVRMNVDAFramebuffer f; f.fSurN = 3; f.fSurActive = true; f.fSurTotalW = 7680; f.fHead = 1;
    unsigned widths[3] = { 2560, 2560, 2560 }; unsigned heads[3] = { 1, 0, 3 };
    for (unsigned k = 0; k < 3; k++) { f.fSur[k] = { 100 + k, heads[k], { { 2560, 1440 } }, nvrmSurroundOffset(widths, k), widths[k] }; }
    Surface wide{1}; NvKmsKapiRequestedModeSetConfig cfg = {};
    cfg.headsMask = 1u << 1; HeadReq &m = cfg.headRequestedConfig[1];       // a flip config for the master head
    m.modeSetConfig.displays[0] = 100; m.layerRequestedConfig[0].config = { &wide, 0, 0, 7680, 1440, 0, 0, 7680, 1440 };
    m.cursorRequestedConfig.surface = &wide; m.cursorRequestedConfig.flags.surfaceChanged = 1;
    f.surroundReplicate(&cfg);
    assert(cfg.headsMask == ((1u << 1) | (1u << 0) | (1u << 3)));
    const LayerCfg &l0 = cfg.headRequestedConfig[1].config_dummy_placeholder;
    return 0;
}
'''
fixture = fixture.replace("    const LayerCfg &l0 = cfg.headRequestedConfig[1].config_dummy_placeholder;\n    return 0;\n", r'''
    for (unsigned k = 0; k < 3; k++) {
        const HeadReq &h = cfg.headRequestedConfig[heads[k]]; const LayerCfg &l = h.layerRequestedConfig[0].config;
        assert(h.modeSetConfig.displays[0] == 100 + k);                       // its own display
        assert(l.srcX == 2560 * k && l.srcWidth == 2560 && l.dstWidth == 2560 && l.dstHeight == 1440);   // its slice of the wide surface
        assert(l.surface == &wide && h.layerRequestedConfig[0].flags.srcXYChanged);
        if (k) assert(!h.cursorRequestedConfig.surface && !h.cursorRequestedConfig.flags.surfaceChanged);   // one cursor plane: members have none
    }
    // a member head the caller already filled is left alone; a config that does not touch the master head is not touched
    NvKmsKapiRequestedModeSetConfig other = {}; other.headsMask = 1u << 2; f.surroundReplicate(&other); assert(other.headsMask == (1u << 2));
    f.fSurActive = false; NvKmsKapiRequestedModeSetConfig off = {}; off.headsMask = 1u << 1; f.surroundReplicate(&off); assert(off.headsMask == (1u << 1));
    puts("PASS: each member head gets its display, mode and slice; cursor stays on the master; inactive group changes nothing");
    return 0;
''')
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    r = subprocess.run(['clang++','-std=c++17','-Wall',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],capture_output=True,text=True)
    if r.returncode: print(r.stderr[-2500:]); raise SystemExit(1)
    subprocess.run([str(d/'t')],check=True)
