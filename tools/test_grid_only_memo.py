#!/usr/bin/env python3
"""The grid-only scan of a kernel runs once per AIR text and kernel name, not once per pipeline state (#568).

VideoToolbox's pixel-transfer session builds a new compute pipeline for each frame it converts. Photo Booth's camera
thread (CMIOGraph > VTPixelTransferSessionTransferImage) had 854 samples in dispatchThreadgroups > nvmtl_tg_reshape >
nvmtl_air_grid_only: the whole module text scanned again for every frame. The production scan and its memo from
plugin/NVMTLLibraryLoad.m run here on a small AIR module, with the scan counted. An optional argument checks another
NVMTLLibraryLoad.m (the 2.9 one scans on every call and fails the first arm)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source_path = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / 'plugin/NVMTLLibraryLoad.m'
source = source_path.read_text()


def function(signature):
    # these functions hold braces inside string literals, so a function ends at the first line that is only "}"
    start = source.index(signature)
    return source[start:source.index('\n}\n', start) + 3]


tokens = function('static NSArray<NSString *> *nvmtl_air_tuple_tokens(NSString *line)')
scan = function('static BOOL nvmtl_air_grid_only_scan(NSString *ll, NSString *fname)').replace(
    'static BOOL nvmtl_air_grid_only_scan(', 'static BOOL real_scan(', 1)
memo_start = source.index('BOOL nvmtl_air_grid_only(NVMTLFunction *fn)')
memo_head = source.rfind('\n\n', 0, memo_start)
statics = ''.join(l + '\n' for l in source[memo_head:memo_start].splitlines() if l.startswith('static ') and l.endswith(';'))
memo = statics + function('BOOL nvmtl_air_grid_only(NVMTLFunction *fn)')

fixture = r'''
#import <Foundation/Foundation.h>
#include <os/lock.h>
#include <stdio.h>
@interface NVMTLFunction : NSObject { @public NSString *_air, *_fname; } @end
@implementation NVMTLFunction @end
static unsigned scans, errors, checks;
static void check(int ok, const char *name) { checks++; printf("%s %s\n", ok ? "ok  " : "FAIL", name); if (!ok) errors++; }
''' + tokens + scan + r'''
static BOOL nvmtl_air_grid_only_scan(NSString *ll, NSString *fname) { scans++; return real_scan(ll, fname); }
''' + memo + r'''
static NSString *module(const char *extra) {
    return [NSString stringWithFormat:@"define void @k(ptr addrspace(1) %%a) {\n  %s\n  ret void\n}\n"
        "!0 = !{ptr @k, !1, !2}\n!1 = !{}\n!2 = !{!3, !4}\n"
        "!3 = !{i32 0, !\"air.buffer\", !\"air.location_index\", i32 0, i32 1, !\"air.address_space\", i32 1}\n"
        "!4 = !{i32 1, !\"air.thread_position_in_grid\"}\n", extra];
}
static NVMTLFunction *fn(NSString *air, NSString *name) { NVMTLFunction *f = [NVMTLFunction new]; f->_air = air; f->_fname = name; return f; }
int main(void) { @autoreleasepool {
    NSString *plain = module(""), *barrier = module("call void @air.wg.barrier(i32 2, i32 1)");
    check(real_scan(plain, @"k") == YES && real_scan(barrier, @"k") == NO, "the scan itself: grid-only kernel YES, barrier kernel NO");
    scans = 0;
    BOOL all = YES;
    for (int frame = 0; frame < 60; frame++) all &= nvmtl_air_grid_only(fn(plain, @"k"));   /* a new pipeline per frame */
    check(scans == 1, "60 pipelines of one kernel of one AIR text: one scan, not 60");
    check(all, "and every pipeline gets the scan's answer (grid-only)");
    scans = 0;
    BOOL any = NO;
    for (int frame = 0; frame < 10; frame++) any |= nvmtl_air_grid_only(fn(barrier, @"k"));
    check(scans == 1 && !any, "negative control: a kernel with a barrier answers NO every time, from one scan");
    scans = 0;
    (void)nvmtl_air_grid_only(fn(plain, @"other"));
    check(scans == 1, "another kernel name in the same text is scanned for itself");
    check(nvmtl_air_grid_only(nil) == NO && nvmtl_air_grid_only(fn(@"", @"k")) == NO, "no function or no text answers NO");
    printf("%s: %u checks, %u failures\n", errors ? "FAIL" : "PASS", checks, errors);
    return errors ? 1 : 0;
} }
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-grid-only-memo-') as temporary:
    path = pathlib.Path(temporary)
    (path / 'test.m').write_text(fixture)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-g', '-fsanitize=address,undefined', str(path / 'test.m'),
                        '-framework', 'Foundation', '-o', str(path / 'test')], capture_output=True, text=True, timeout=300)
    if r.returncode:
        print(r.stderr[-3000:])
        sys.exit(1)
    sys.exit(subprocess.run([str(path / 'test')], timeout=60).returncode)
