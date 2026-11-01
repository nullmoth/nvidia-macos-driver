#!/usr/bin/env python3
"""Check plane identity, history gaps and expiration using real IOSurface metadata."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
objects = (root / "plugin/NVMTLObjects.m").read_text()
start = objects.index("typedef struct { uint32_t surf, from, to, plane;")
production = objects[start:objects.index("// Opt-in only. A read-write reader lock", start)]
fixture = r'''
#import <Foundation/Foundation.h>
#import <CoreVideo/CoreVideo.h>
#import <IOSurface/IOSurface.h>
#include "nvmtl_reader_skip.h"
#include <os/lock.h>
#include <stdatomic.h>
#include <assert.h>
#include <time.h>
#define nvlog(...) ((void)0)
'''
checks = r'''
static CVPixelBufferRef make_surface(void) {
    CVPixelBufferRef pixel=NULL;
    NSDictionary *properties=@{(id)kCVPixelBufferIOSurfacePropertiesKey:@{}};
    assert(CVPixelBufferCreate(NULL,64,32,kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        (__bridge CFDictionaryRef)properties,&pixel)==kCVReturnSuccess);
    return pixel;
}
int main(void) { @autoreleasepool {
    if(!nvmtl_plane_seed_on()) { puts("PASS: plane-seed rollback switch"); return 0; }
    CVPixelBufferRef a=make_surface(),b=make_surface();
    IOSurfaceRef surface=CVPixelBufferGetIOSurface(a),other=CVPixelBufferGetIOSurface(b);
    nvmtl_own_bump_add(surface,1,10,11);
    assert(nvmtl_own_bumps_cover(surface,0,10,11));
    assert(!nvmtl_own_bumps_cover(surface,1,10,11));
    assert(!nvmtl_own_bumps_cover(other,0,10,11));
    nvmtl_own_bump_add(surface,0,11,12);
    assert(!nvmtl_own_bumps_cover(surface,0,10,12));
    nvmtl_own_bump_add(surface,1,13,14);
    assert(!nvmtl_own_bumps_cover(surface,0,11,14)); // foreign seed gap
    g_own_bumps[(g_own_bump_next-1)%64].recorded=clock_gettime_nsec_np(CLOCK_UPTIME_RAW)-2000000001ull;
    assert(!nvmtl_own_bumps_cover(surface,0,13,14));
    nvmtl_own_bump_add(surface,1,UINT32_MAX,0);
    assert(!nvmtl_own_bumps_cover(surface,0,UINT32_MAX,0));
    // A private-plane candidate may include known writes to another plane
    // before the final unaccounted seed bump. Same-plane writes must reset
    // the streak and never be treated as reader-only.
    uint32_t last=1000,streak=0;
    for(unsigned i=0;i<5;i++) {
        nvmtl_own_bump_add(surface,1,last,last+1);
        uint32_t now=last+2;
        BOOL others=nvmtl_own_bumps_cover(surface,0,last,now-1);
        assert(others);
        BOOL skip=nvmtl_reader_skip_observe(last,now,&streak,false,true,true,others);
        assert(skip==(i==4));
        last=now;
    }
    last=2000; streak=0;
    for(unsigned i=0;i<5;i++) {
        nvmtl_own_bump_add(surface,0,last,last+1);
        uint32_t now=last+2;
        BOOL others=nvmtl_own_bumps_cover(surface,0,last,now-1);
        assert(!others);
        assert(!nvmtl_reader_skip_observe(last,now,&streak,false,true,true,others));
        last=now;
    }
    nvmtl_own_bump_add(surface,1,20,21);
    CFDictionaryRef values=IOSurfaceCopyAllValues(surface);
    for(id key in (__bridge NSDictionary *)values) {
        if([key isKindOfClass:[NSString class]] && [key hasPrefix:@"NVMTLPlaneSeed."])
            IOSurfaceSetValue(surface,(__bridge CFStringRef)key,(__bridge CFNumberRef)@9999);
    }
    CFRelease(values);
    assert(!nvmtl_own_bumps_cover(surface,0,20,21)); // identity cookie changed
    nvmtl_own_bump_add(surface,1,30,31);
    assert(nvmtl_own_bumps_cover(surface,0,30,31));
    for(uint32_t i=100;i<180;i++) nvmtl_own_bump_add(surface,1,i,i+1);
    assert(!nvmtl_own_bumps_cover(surface,0,100,180));
    assert(nvmtl_own_bumps_cover(surface,0,179,180));
    CVPixelBufferRelease(a); CVPixelBufferRelease(b);
    puts("PASS: same-plane invalidation, other-plane reuse, plane-isolated reader candidates, foreign seed gaps, cookies, wrap and bounded history");
} }
'''
with tempfile.TemporaryDirectory() as directory:
    source = Path(directory) / "plane.m"
    source.write_text(fixture + production + checks)
    binary = Path(directory) / "plane"
    subprocess.run(["xcrun", "clang", "-fobjc-arc", "-fsanitize=address,undefined", "-I", str(root / "plugin"), str(source),
                    "-o", str(binary), "-framework", "Foundation", "-framework", "CoreVideo",
                    "-framework", "IOSurface"], check=True)
    subprocess.run([str(binary)], check=True)
    import os
    subprocess.run([str(binary)], check=True, env=dict(os.environ, NVMTL_PLANE_SEED="0"))
