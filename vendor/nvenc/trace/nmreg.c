// nmreg.c: register one of nullmoth's closed encoder libraries with VideoToolbox in this process, the way their Metal
// plugin does on Blackwell (dlopen + VTRegisterVideoEncoderWithInfo with the library's factory), so a test client can
// be observed driving it on a GPU where the plugin does not register it (T6.03: runtime observation, option B).
// Nothing in the library is changed.
//   DYLD_INSERT_LIBRARIES=~/nvenc-dev/nmreg.dylib[:~/nvenc-dev/nvtrace.dylib] ./vttest hw hvc1 256 128 1
//   NMREG_LIB  library (default: the HEVC encoder in the installed plugin bundle)
//   NMREG_SYM  factory symbol (default NVEncHEVC_CreateInstance); NMREG_CODEC hvc1|avc1 (default hvc1)
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern OSStatus VTRegisterVideoEncoderWithInfo(FourCharCode, CFDictionaryRef, void *);

__attribute__((constructor)) static void nmreg(void)
{
    const char *lib = getenv("NMREG_LIB") ? getenv("NMREG_LIB") : "/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvenc_hevc.dylib";
    const char *sym = getenv("NMREG_SYM") ? getenv("NMREG_SYM") : "NVEncHEVC_CreateInstance";
    const char *cc = getenv("NMREG_CODEC") ? getenv("NMREG_CODEC") : "hvc1";
    FourCharCode codec = !strcmp(cc, "avc1") ? kCMVideoCodecType_H264 : kCMVideoCodecType_HEVC;
    void *h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
    void *fac = h ? dlsym(h, sym) : NULL;
    if (!fac) { fprintf(stderr, "nmreg: %s / %s: %s\n", lib, sym, dlerror()); return; }
    int rating = 1200;
    CFNumberRef r = CFNumberCreate(NULL, kCFNumberIntType, &rating);
    CFStringRef id = CFStringCreateWithFormat(NULL, NULL, CFSTR("org.nvenc.observe.%s"), cc);
    const void *k[] = { CFSTR("CMClassImplementationID"), CFSTR("CMClassImplementationName"), CFSTR("VTRating"), CFSTR("VTIsHardwareAccelerated") };
    const void *v[] = { id, CFSTR("nullmoth NVENC (observed)"), r, kCFBooleanTrue };
    CFDictionaryRef info = CFDictionaryCreate(NULL, k, v, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus st = VTRegisterVideoEncoderWithInfo(codec, info, fac);
    fprintf(stderr, "nmreg: %s from %s registered for %s -> %d\n", sym, lib, cc, (int)st);
    CFRelease(info); CFRelease(id); CFRelease(r);
}
