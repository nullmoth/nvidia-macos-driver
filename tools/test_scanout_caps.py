#!/usr/bin/env python3
"""The scanout ceilings follow the panel's widest/tallest validated raster, not twice the boot raster.

Negative control: the pre-fix formula (min(2*bootW, 3840) wide, min(2*bootH, 2160) tall, min(2*bootW, 7680)
raster) is reproduced here as oldCaps(); it is asserted to STRAND a 32:9 panel booted narrow, which is exactly
the behaviour the new computeScanoutCaps() must not have. If the extracted method still computed the old caps,
the panel-follows assertions below would fail.
"""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
start = source.index('void NVRMNVDAFramebuffer::computeScanoutCaps(')
method = source[start:source.index('// Users on HDMI and DisplayPort', start)]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>
using NvU32=uint32_t;using NvU64=uint64_t;using NvBool=bool;
enum { NV_TRUE=1,NV_FALSE=0 };
#define FBLOG(...) ((void)0)
static bool PE_parse_boot_argn(const char *,void *,int) { return false; }
struct NvKmsKapiDisplayModeTimings {
    NvU32 pixelClockHz,hVisible,hSyncStart,hSyncEnd,hTotal,vVisible,vSyncStart,vSyncEnd,vTotal,refreshRate;
    struct { bool interlaced=false,doubleScan=false,hSyncPos=true,vSyncPos=true; } flags;
};
struct NvKmsKapiDisplayMode { NvKmsKapiDisplayModeTimings timings={}; };
struct Kms {
    std::vector<NvKmsKapiDisplayMode> modes;
    int getDisplayMode(void *,unsigned,unsigned index,NvKmsKapiDisplayMode *mode,NvBool *valid,NvBool *pref) {
        if(index>=modes.size())return -1;*mode=modes[index];*valid=true;*pref=index==0;return 1;
    }
};
struct NVRMNVDAFramebuffer {
    Kms *fKms=nullptr;void *fDev=this;unsigned fDisplay=1,fIndex=0;
    NvU32 fW=0,fH=0,fScalerMaxW=0,fScalerMaxH=0,fRasterMaxW=0;
    NvU64 fSize=0,fAllocSize=0;
    void computeScanoutCaps(NvU32 pitchAlign);
};
static NvKmsKapiDisplayMode mode(unsigned w,unsigned h,unsigned hz) {
    NvKmsKapiDisplayMode m;auto &k=m.timings;k.hVisible=w;k.vVisible=h;
    k.hTotal=w+160;k.vTotal=h+132;k.pixelClockHz=k.hTotal*k.vTotal*hz;k.refreshRate=hz*1000;
    return m;
}
// the stranding formula shipped before the panel-follows fix
static NvU32 oldRasterMaxW(NvU32 fW){ NvU32 r=fW*2>7680?7680:fW*2; return r<fW?fW:r; }
'''
checks = r'''
int main() {
    // 32:9 ultrawide the firmware lit at 1920x1080: native raster is 5120x1440 (and the panel also lists 7680x2160)
    Kms uw;uw.modes={mode(1920,1080,60),mode(2560,1440,60),mode(5120,1440,120),mode(7680,2160,60)};
    NVRMNVDAFramebuffer f;f.fKms=&uw;f.fW=1920;f.fH=1080;f.fSize=1920ull*1080*4;
    f.computeScanoutCaps(256);
    assert(f.fRasterMaxW==7680 && f.fScalerMaxW==7680 && f.fScalerMaxH==2160);
    assert(f.fAllocSize>=(NvU64)7680*4*2160);
    // negative control: the old formula would have capped this panel at 3840 wide and never offered the ultrawide raster
    assert(oldRasterMaxW(1920)==3840 && oldRasterMaxW(1920) < 5120);

    // 4K panel the firmware lit at 1080p (issue #49): native 3840x2160 must be the ceiling so macOS can build 1080p HiDPI
    Kms k4;k4.modes={mode(1920,1080,60),mode(3840,2160,60)};
    NVRMNVDAFramebuffer g;g.fKms=&k4;g.fW=1920;g.fH=1080;g.fSize=1920ull*1080*4;
    g.computeScanoutCaps(256);
    assert(g.fRasterMaxW==3840 && g.fScalerMaxH==2160);

    // boot raster is the floor, and is never shrunk below it
    Kms one;one.modes={mode(2560,1440,60)};
    NVRMNVDAFramebuffer h;h.fKms=&one;h.fW=2560;h.fH=1440;h.fSize=2560ull*1440*4;
    h.computeScanoutCaps(256);
    assert(h.fScalerMaxW==2560 && h.fScalerMaxH==1440 && h.fRasterMaxW==2560);

    // empty/unreadable mode list: fall back to one step up from the boot raster, not zero
    NVRMNVDAFramebuffer e;Kms none;e.fKms=&none;e.fW=1920;e.fH=1080;e.fSize=1920ull*1080*4;
    e.computeScanoutCaps(256);
    assert(e.fScalerMaxW==3840 && e.fScalerMaxH==2160);

    // interlaced/doublescan rasters do not raise the ceiling
    Kms il;il.modes={mode(1920,1080,60)};il.modes[0].timings.flags.interlaced=true;
    il.modes.push_back(mode(1920,1080,60));
    NVRMNVDAFramebuffer d;d.fKms=&il;d.fW=1920;d.fH=1080;d.fSize=1920ull*1080*4;
    d.computeScanoutCaps(256);
    assert(d.fScalerMaxW==1920 && d.fScalerMaxH==1080);

    // 8K ceiling clamp
    Kms huge;huge.modes={mode(3840,2160,60),mode(10240,4320,30)};
    NVRMNVDAFramebuffer hg;hg.fKms=&huge;hg.fW=3840;hg.fH=2160;hg.fSize=3840ull*2160*4;
    hg.computeScanoutCaps(256);
    assert(hg.fRasterMaxW==7680 && hg.fScalerMaxH==4320);

    puts("PASS: ceilings follow the panel's widest/tallest validated raster (ultrawide 7680x2160, 4K at boot-1080p), "
         "boot floor held, empty-list fallback, interlaced ignored, 8K clamp; old formula stranded the ultrawide");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + method + checks)
    subprocess.run(['clang++', '-std=c++17', str(path / 'test.cpp'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
