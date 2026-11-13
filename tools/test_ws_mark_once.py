#!/usr/bin/env python3
"""WindowServer's commit marks each IOSurface plane it writes once per command buffer, not once per part and view (#566).

A spindump of WindowServer while a video played had 73 of its 125 kernel-call samples in nvmtlSubmitWithEvents >
nvmtl_vk_surface_mark_newest > nvRmApiSurfShare: the commit marked a surface's VRAM newest for every part of the command
buffer that wrote it and for every texture wrapping it, each mark an ioctl that takes the accelerator lock. The production
loop from nvmtl_ws_mark_pending (plugin/NVMTLEventSync.m) runs here on stand-in parts and textures with the kernel call
counted. An optional argument checks another NVMTLEventSync.m (the 2.9 one marks once per texture per part and fails the
first arm)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'plugin/NVMTLEventSync.m'
source = source_path.read_text()
start = source.index('    if (holds->early) {', source.index('static void nvmtl_ws_mark_pending('))
end = source.index('holds->dirtied = dirtied;', start) + len('holds->dirtied = dirtied;')
loop = source[start:end] + '\n    }\n'

fixture = r'''
#import <Foundation/Foundation.h>
#include <stdio.h>
typedef struct { uint32_t id; } Surf;
static uint32_t IOSurfaceGetID(Surf *s) { return s->id; }
static unsigned calls, errors, checks; static int refuse_id;
static int nvmtl_vk_surface_dirty(uint32_t sid, uint32_t plane) { calls++; return (int)sid == refuse_id ? -1 : 0; }
@interface NVMTLTexture : NSObject { @public Surf *_surf; BOOL _vramOn; NSUInteger _plane; } @end
@implementation NVMTLTexture @end
@interface NVMTLSubmissionPart : NSObject { @public NSSet *surfaces; } @end
@implementation NVMTLSubmissionPart @end
@interface Holds : NSObject { @public BOOL early; NSSet *dirtied; } @end
@implementation Holds @end
@interface CB : NSObject { @public NSMutableArray *_parts; } @end
@implementation CB @end
static void check(int ok, const char *name) { checks++; printf("%s %s\n", ok ? "ok  " : "FAIL", name); if (!ok) errors++; }
static void run(CB *cb, Holds *holds) {
''' + loop + r'''
}
static NVMTLTexture *tex(Surf *s, NSUInteger plane) { NVMTLTexture *t = [NVMTLTexture new]; t->_surf = s; t->_vramOn = YES; t->_plane = plane; return t; }
int main(void) { @autoreleasepool {
    Surf video = { 7 }, desk = { 9 }, other = { 11 };
    NVMTLTexture *v0 = tex(&video, 0), *v0view = tex(&video, 0), *v1 = tex(&video, 1), *d = tex(&desk, 0);
    NVMTLTexture *off = tex(&other, 0); off->_vramOn = NO;
    CB *cb = [CB new]; cb->_parts = [NSMutableArray new];
    for (int p = 0; p < 4; p++) { NVMTLSubmissionPart *part = [NVMTLSubmissionPart new];
        part->surfaces = [NSSet setWithObjects:v0, v0view, v1, d, off, nil]; [cb->_parts addObject:part]; }
    [cb->_parts addObject:[NSNull null]];
    Holds *h = [Holds new]; h->early = YES;
    run(cb, h);
    check(calls == 3, "4 parts x 4 VRAM textures over 3 surface planes: 3 kernel marks, not 16");
    check([h->dirtied containsObject:v0] && [h->dirtied containsObject:v0view] && [h->dirtied containsObject:v1] && [h->dirtied containsObject:d],
          "every texture of a marked plane is still recorded as dirtied (its completion re-mark is a try)");
    check(![h->dirtied containsObject:off], "a texture without shared VRAM is neither marked nor recorded");
    calls = 0; refuse_id = 7; h = [Holds new]; h->early = YES;
    run(cb, h);
    check(calls == 3 && ![h->dirtied containsObject:v0] && ![h->dirtied containsObject:v0view] && ![h->dirtied containsObject:v1],
          "a refused mark is asked once and its textures are not recorded as dirtied (they take the copy-out)");
    check([h->dirtied containsObject:d], "negative control: another surface in the same command buffer is still marked");
    calls = 0; refuse_id = 0;
    CB *two = [CB new]; two->_parts = [NSMutableArray arrayWithObject:cb->_parts[0]];
    run(two, [Holds new]); run(two, [Holds new]);
    check(calls == 0, "without the early ordering nothing is marked at commit");
    Holds *a = [Holds new]; a->early = YES; Holds *b = [Holds new]; b->early = YES;
    run(two, a); run(two, b);
    check(calls == 6, "negative control: a second command buffer marks again (the once is per commit, not forever)");
    printf("%s: %u checks, %u failures\n", errors ? "FAIL" : "PASS", checks, errors);
    return errors ? 1 : 0;
} }
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-ws-mark-once-') as temporary:
    path = pathlib.Path(temporary)
    (path / 'test.m').write_text(fixture)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-g', '-fsanitize=address,undefined', str(path / 'test.m'),
                        '-framework', 'Foundation', '-o', str(path / 'test')], capture_output=True, text=True, timeout=300)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit(1)
    sys.exit(subprocess.run([str(path / 'test')], timeout=60).returncode)
