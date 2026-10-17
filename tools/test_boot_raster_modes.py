#!/usr/bin/env python3
"""Replay initial mode publication and preserve the smaller-raster restriction."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
start = source.index('unsigned NVRMNVDAFramebuffer::addKmsRefreshModes(')
method = source[start:source.index('int NVRMNVDAFramebuffer::dtIndex(', start)]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>
using NvU32=uint32_t;using NvU64=uint64_t;using UInt64=uint64_t;using UInt16=uint16_t;using NvBool=bool;
enum { NV_TRUE=1,NV_FALSE=0,kIODigitalSignal=1 };
#define FBLOG(...) ((void)0)
static void nvu_zero(void *p,size_t n){memset(p,0,n);}
struct NvKmsKapiDisplayModeTimings {
    NvU32 pixelClockHz,hVisible,hSyncStart,hSyncEnd,hTotal,vVisible,vSyncStart,vSyncEnd,vTotal,refreshRate;
    struct { bool interlaced=false,doubleScan=false,hSyncPos=true,vSyncPos=true; } flags;
};
struct NvKmsKapiDisplayMode { NvKmsKapiDisplayModeTimings timings={}; };
struct IODetailedTimingInformationV2 {
    UInt64 pixelClock=0,minPixelClock=0,maxPixelClock=0;
    NvU32 horizontalActive=0,verticalActive=0,horizontalScaled=0,verticalScaled=0;
    NvU32 horizontalBlanking=0,verticalBlanking=0,horizontalSyncOffset=0,verticalSyncOffset=0;
    NvU32 horizontalSyncPulseWidth=0,verticalSyncPulseWidth=0,horizontalSyncConfig=0,verticalSyncConfig=0;
    NvU32 detailedTimingModeID=0,signalConfig=0;
    UInt16 dscCompressedBitsPerPixel=0,dscSliceWidth=0,dscSliceHeight=0;
};
struct Kms {
    std::vector<NvKmsKapiDisplayMode> modes;
    int getDisplayMode(void *,unsigned,unsigned index,NvKmsKapiDisplayMode *mode,NvBool *valid,NvBool *pref) {
        if(index>=modes.size())return -1;*mode=modes[index];*valid=true;*pref=index==1;return 1;
    }
};
struct NVRMNVDAFramebuffer {
    Kms *fKms=nullptr;void *fDev=this;unsigned fDisplay=1,fIndex=0;
    unsigned fBootW=2560,fBootH=1600,fScalerMaxW=3840,fScalerMaxH=2160,fRasterMaxW=5120,fPitchAlign=256;
    NvU64 fMemBytes=64ULL<<20;NvKmsKapiDisplayMode fBootMode;bool fKmsModesLogged=false;
    IODetailedTimingInformationV2 fDT[16];
    void setProperty(const char *,unsigned long long,unsigned) {}
    unsigned addKmsRefreshModes(unsigned n);
};
static NvKmsKapiDisplayMode mode(unsigned w,unsigned h,unsigned hz) {
    NvKmsKapiDisplayMode m;auto &k=m.timings;k.hVisible=w;k.vVisible=h;
    k.hTotal=w+160;k.vTotal=h+132;k.hSyncStart=w+32;k.hSyncEnd=w+64;
    k.vSyncStart=h+3;k.vSyncEnd=h+8;k.pixelClockHz=k.hTotal*k.vTotal*hz;k.refreshRate=hz*1000;
    return m;
}
'''
checks = r'''
int main() {
    Kms kms;kms.modes={mode(2560,1600,60),mode(2560,1600,165),mode(1280,720,60)};
    NVRMNVDAFramebuffer f;f.fKms=&kms;f.fBootMode=kms.modes[1];
    unsigned n=f.addKmsRefreshModes(0);assert(n==2);
    assert(f.fDT[0].detailedTimingModeID==0x10000 && f.fDT[1].detailedTimingModeID==0x10100);
    assert(f.fDT[1].pixelClock==777321600 && !f.fDT[1].horizontalScaled);
    assert(f.addKmsRefreshModes(n)==n);
    NVRMNVDAFramebuffer scaled;scaled.fKms=&kms;scaled.fBootMode=kms.modes[1];
    scaled.fDT[0]=f.fDT[1];scaled.fDT[0].horizontalScaled=1920;scaled.fDT[0].verticalScaled=1200;
    assert(scaled.addKmsRefreshModes(1)==3);
    NVRMNVDAFramebuffer tooSmall;tooSmall.fKms=&kms;tooSmall.fMemBytes=1024;
    assert(tooSmall.addKmsRefreshModes(0)==0);
    kms.modes[1].timings.flags.interlaced=true;
    NVRMNVDAFramebuffer bad;bad.fKms=&kms;bad.fBootMode=f.fBootMode;
    assert(bad.addKmsRefreshModes(0)==1);
    // a 5120x1440 monitor that the firmware started at 2560x1440: wider than the scaler limit, but it fits the memory
    Kms wide;wide.modes={mode(2560,1440,60),mode(5120,1440,60),mode(7680,2160,60)};
    NVRMNVDAFramebuffer uw;uw.fKms=&wide;uw.fBootW=2560;uw.fBootH=1440;uw.fBootMode=wide.modes[0];
    assert(uw.addKmsRefreshModes(0)==2 && uw.fDT[1].horizontalActive==5120);
    NVRMNVDAFramebuffer uwSmall;uwSmall.fKms=&wide;uwSmall.fBootW=2560;uwSmall.fBootH=1440;uwSmall.fBootMode=wide.modes[0];
    uwSmall.fMemBytes=16ULL<<20;
    assert(uwSmall.addKmsRefreshModes(0)==1);
    puts("PASS: boot 165 Hz desktop timing, stable IDs, no duplicate, scaled mode coexistence, smaller-raster exclusion, memory and timing guards, ultrawide wider than the scaler but not past the raster limit or memory");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + '\n#include "nvrm-fb-safety.h"\n' + method + checks)
    subprocess.run(['clang++', '-std=c++17', '-I', str(root / 'kexts/NVRMFB/fb'),
                    str(path / 'test.cpp'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
