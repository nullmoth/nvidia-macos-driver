#!/usr/bin/env python3
"""Replay initial mode publication and preserve the smaller-raster restriction."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
start = source.index('unsigned NVRMNVDAFramebuffer::addKmsRefreshModes(')
method = source[start:source.index('int NVRMNVDAFramebuffer::dtIndex(', start)]
ds = source.index('unsigned NVRMNVDAFramebuffer::addDsrModes(')
method_dsr = source[ds:source.index('\n}\n', ds) + 3]
fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>
using UInt32=uint32_t;using NvU32=uint32_t;using NvU64=uint64_t;using UInt64=uint64_t;using UInt16=uint16_t;using NvBool=bool;
enum { NV_TRUE=1,NV_FALSE=0,kIODigitalSignal=1 };
#define FBLOG(...) ((void)0)
#include "nvrm-fb-safety.h"
struct DpyRec { unsigned hdaConnectorType = 0; };
static DpyRec gDpy[4]; static int gNDpy = 1;
#ifdef SMALL_ON
static bool PE_parse_boot_argn(const char *, uint32_t *v, size_t) { *v = 1; return true; }
#else
static bool PE_parse_boot_argn(const char *, uint32_t *, size_t) { return false; }
#endif
static bool nvrmRootFile(const char *) { return false; }
#define FBLOGV(...) ((void)0)
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
    NvU32 detailedTimingModeID=0,signalConfig=0,scalerFlags=0;
    UInt16 dscCompressedBitsPerPixel=0,dscSliceWidth=0,dscSliceHeight=0;
};
struct Kms {
    std::vector<NvKmsKapiDisplayMode> modes;
    int getDisplayMode(void *,unsigned,unsigned index,NvKmsKapiDisplayMode *mode,NvBool *valid,NvBool *pref) {
        if(index>=modes.size())return -1;*mode=modes[index];*valid=true;*pref=index==1;return 1;
    }
};
struct NVRMNVDAFramebuffer {
    bool fInternal=false;Kms *fKms=nullptr;void *fDev=this;unsigned fDisplay=1,fIndex=0;
    unsigned fBootW=2560,fBootH=1600,fScalerMaxW=3840,fScalerMaxH=2160,fRasterMaxW=5120,fPitchAlign=256;
    NvU64 fMemBytes=64ULL<<20;NvKmsKapiDisplayMode fBootMode;bool fKmsModesLogged=false;
    IODetailedTimingInformationV2 fDT[16];
    void setProperty(const char *,unsigned long long,unsigned) {}
    unsigned addKmsRefreshModes(unsigned n);
    unsigned addDsrModes(unsigned n);
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
    // DSR-style supersampled desktops: a native 2560x1600@60 raster gets 1.20x..4.00x desktops within the scaler cap
    {
        Kms one;one.modes={mode(2560,1600,60)};
        NVRMNVDAFramebuffer g;g.fKms=&one;g.fBootW=2560;g.fBootH=1600;g.fBootMode=one.modes[0];g.fScalerMaxW=5120;g.fScalerMaxH=3200;
        g.fMemBytes=256ULL<<20;
        unsigned base=g.addKmsRefreshModes(0);assert(base==1);
        unsigned dn2=g.addDsrModes(base);
        assert(dn2==1+7);                                   // all seven factors fit under 2x per axis
        assert(g.fDT[1].horizontalScaled>2560 && g.fDT[1].verticalScaled>1600 && (g.fDT[1].horizontalScaled&1)==0);
        assert(g.fDT[7].horizontalScaled==5120 && g.fDT[7].verticalScaled==3200);   // 4.00x pixel count = 2x per axis
        assert((g.fDT[3].detailedTimingModeID>>16)==2 && (g.fDT[3].detailedTimingModeID&0xfff)==60);
        assert(g.addDsrModes(dn2)==dn2);                    // already listed: nothing added twice
        NVRMNVDAFramebuffer capd=g;capd.fScalerMaxW=3000;capd.fScalerMaxH=1900;
        unsigned c2=capd.addDsrModes(1);assert(c2>1 && c2<8); // the scaler cap removes the large ones
        NVRMNVDAFramebuffer tight;tight.fKms=&one;tight.fBootW=2560;tight.fBootH=1600;tight.fBootMode=one.modes[0];tight.fScalerMaxW=5120;tight.fScalerMaxH=3200;
        tight.fMemBytes=1024;tight.fDT[0]=g.fDT[0];
        assert(tight.addDsrModes(1)==1);                    // no scanout memory: none offered
    }
    // derived rates: a native 240 Hz mode also offers 120 and 60 Hz; 165 Hz (below 240) offers none
    Kms hi;hi.modes={mode(2560,1600,240)};
    NVRMNVDAFramebuffer d;d.fKms=&hi;d.fBootMode=hi.modes[0];
    unsigned dn=d.addKmsRefreshModes(0);
    assert(dn==3);
    assert(d.fDT[1].detailedTimingModeID==0x10001 && d.fDT[2].detailedTimingModeID==0x10002);
    auto rate=[&](const IODetailedTimingInformationV2 &t){UInt64 tot=(UInt64)(t.horizontalActive+t.horizontalBlanking)*(t.verticalActive+t.verticalBlanking);return (unsigned)((t.pixelClock+tot/2)/tot);};
    assert(rate(d.fDT[0])==240 && rate(d.fDT[1])==120 && rate(d.fDT[2])==60);
    assert(d.fDT[1].horizontalBlanking==d.fDT[0].horizontalBlanking && !d.fDT[1].dscCompressedBitsPerPixel);
    assert(d.addKmsRefreshModes(dn)==dn);   // idempotent: nothing listed twice
    Kms lo;lo.modes={mode(2560,1600,165)};
    NVRMNVDAFramebuffer l;l.fKms=&lo;l.fBootMode=lo.modes[0];
    assert(l.addKmsRefreshModes(0)==1);
    puts("PASS: boot 165 Hz desktop timing, stable IDs, no duplicate, scaled mode coexistence, smaller-raster exclusion, memory and timing guards, ultrawide wider than the scaler but not past the raster limit or memory");
}
'''
SMALL = r'''
int main() {
    // nvfbsmall=1: a raster smaller than the boot raster is offered too (default: it is not, checked in the other build)
    Kms k;k.modes={mode(2560,1600,60),mode(1280,720,60)};
    NVRMNVDAFramebuffer f;f.fKms=&k;f.fBootMode=k.modes[0];
    assert(f.addKmsRefreshModes(0)==2);
    puts("PASS small");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + '\n#include "nvrm-fb-safety.h"\n' + method + method_dsr + checks)
    subprocess.run(['clang++', '-std=c++17', '-I', str(root / 'kexts/NVRMFB/fb'),
                    str(path / 'test.cpp'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
    (path / 'small.cpp').write_text(fixture + '\n#include "nvrm-fb-safety.h"\n' + method + method_dsr + SMALL)
    subprocess.run(['clang++', '-std=c++17', '-DSMALL_ON', '-I', str(root / 'kexts/NVRMFB/fb'),
                    str(path / 'small.cpp'), '-o', str(path / 'small')], check=True)
    subprocess.run([str(path / 'small')], check=True)
