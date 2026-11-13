#!/usr/bin/env python3
"""A Metal device the plugin refuses before Metal's own init no longer crashes the program, and a program built against
an old SDK is not refused on a supported macOS (#571).

Canon CaptureOnTouch on macOS 15.7.2 died in MTLCopyAllDevices: -[NVMTLDevice initWithAcceleratorPort:] returned nil
before [super initWithAcceleratorPort:], ARC released the object, and -[_MTLDevice dealloc] called dispatch_release on a
NULL queue. The refusal it took was the macOS version check: the kernel answers kern.osproductversion with "10.16" to a
program built before the macOS 11 SDK, and atoi("10.16") is 10.

Arms: the version helper (plugin/nvmtl_os_major.h) on real answers, with the old atoi reading as the negative control;
the source of the init (every refusal before super init keeps the object); and the ARC mechanism itself, run on a stand-in
class whose dealloc reports when it runs on an object its init never set up (the plain `return nil` arm must fire)."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
device = (root / 'plugin/NVMTLDevice.m').read_text()
errors = checks = 0


def check(ok, name):
    global errors, checks
    checks += 1
    print(('ok   ' if ok else 'FAIL ') + name)
    errors += not ok


# 1. the source: between the start of initWithAcceleratorPort: and [super initWithAcceleratorPort:], no bare return nil
start = device.index('- (instancetype)initWithAcceleratorPort:(mach_port_t)port {')
sup = device.index('self = [super initWithAcceleratorPort:port];', start)
head = device[start:sup]
check('return nil;' not in head, 'no bare `return nil` before super init (each refusal keeps the object)')
check(head.count('return nvmtl_refuse_before_init(self);') >= 3,
      'the version refusals and the card refusal all go through nvmtl_refuse_before_init')
check('CFBridgingRetain(obj)' in device[device.index('static id nvmtl_refuse_before_init'):start],
      'nvmtl_refuse_before_init keeps one reference')
check('nvmtl_os_major_from(v, d)' in head and '"kern.osrelease"' in head, 'the version check reads kern.osrelease too')

fixture = r'''
#import <Foundation/Foundation.h>
#include <stdio.h>
#include "nvmtl_os_major.h"
static int bad_deallocs, errors, checks;
static void check(int ok, const char *name) { checks++; printf("%s %s\n", ok ? "ok  " : "FAIL", name); if (!ok) errors++; }
@interface Base : NSObject { @public void *queue; } - (instancetype)initPort:(int)p; @end
@implementation Base
- (instancetype)initPort:(int)p { if ((self = [super init])) queue = (void *)1; return self; }
- (void)dealloc { if (!queue) bad_deallocs++; }   /* Metal's dealloc would dispatch_release(NULL) here */
@end
static id nvmtl_refuse_before_init(id obj) { (void)CFBridgingRetain(obj); return nil; }
@interface Plain : Base @end
@implementation Plain - (instancetype)initPort:(int)p { if (p == 0) return nil; return [super initPort:p]; } @end
@interface Kept : Base @end
@implementation Kept - (instancetype)initPort:(int)p { if (p == 0) return nvmtl_refuse_before_init(self); return [super initPort:p]; } @end
int main(void) {
    check(nvmtl_os_major_from("15.7.2", "24.6.0") == 15, "15.7.2 reads 15");
    check(nvmtl_os_major_from("26.7.3", "25.7.0") == 26, "26.7.3 reads 26");
    check(nvmtl_os_major_from("10.16", "24.6.0") == 15, "an old-SDK program on macOS 15 (10.16, Darwin 24) reads 15");
    check(nvmtl_os_major_from("10.16", "25.2.0") == 26, "an old-SDK program on macOS 26 (10.16, Darwin 25) reads 26");
    check(nvmtl_os_major_from("", "24.1.0") == 15, "an unreadable product version falls back to Darwin");
    check(nvmtl_os_major_from(NULL, NULL) == 0, "nothing readable reads 0 (refused as unknown)");
    check(nvmtl_os_major_from("27.0", "26.0.0") == 27, "macOS 27 still reads 27 (still refused by the 2.x range)");
    check(nvmtl_os_major_from("10.15.7", "19.6.0") == 10, "a real 10.15 stays 10");
    check(atoi("10.16") < 15, "negative control: the old atoi reading of 10.16 is below 15 (the refusal #571 hit)");
    @autoreleasepool { (void)[[Plain alloc] initPort:0]; }
    check(bad_deallocs == 1, "negative control: a plain return nil before super init runs dealloc on an uninitialized object");
    bad_deallocs = 0;
    @autoreleasepool { (void)[[Kept alloc] initPort:0]; }
    check(bad_deallocs == 0, "nvmtl_refuse_before_init: no dealloc on the uninitialized object");
    @autoreleasepool { id k = [[Kept alloc] initPort:1]; check(k != nil, "an accepted device still initializes"); }
    check(bad_deallocs == 0, "and is freed normally");
    printf("%s: %d checks, %d failures\n", errors ? "FAIL" : "PASS", checks, errors);
    return errors ? 1 : 0;
}
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-device-refusal-') as temporary:
    path = pathlib.Path(temporary)
    (path / 'test.m').write_text(fixture)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-g', '-fsanitize=address,undefined', str(path / 'test.m'),
                        '-I' + str(root / 'plugin'), '-framework', 'Foundation', '-o', str(path / 'test')],
                       capture_output=True, text=True, timeout=300)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit(1)
    rc = subprocess.run([str(path / 'test')], timeout=60).returncode

print(f'{"FAIL" if errors or rc else "PASS"}: {checks} source checks, {errors} failures')
sys.exit(1 if errors or rc else 0)
