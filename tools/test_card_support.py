#!/usr/bin/env python3
"""CPU regressions for card startup, compiler selection, and report retention.
Run on macOS with Xcode command line tools: python3 tools/test_card_support.py.
Tests compile the production snippets; no GPU or installation is required.
"""
import json
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
def snippet(path, start, end):
    text = (ROOT / path).read_text()
    assert text.count(start) == 1, (path, start)
    return start + text.split(start, 1)[1].split(end, 1)[0]
def run(*args):
    return subprocess.check_output(args, text=True).strip()

TIMING = r'''
#include <initializer_list>
#include <cstdint>
#include <cassert>
#include <cstdio>
constexpr int kMillisecondScale=1;
uint64_t now=1000;
void clock_interval_to_deadline(uint32_t n,int,uint64_t *p){*p=now+n;}
void clock_interval_to_absolutetime_interval(uint32_t n,int,uint64_t *p){*p=n;}
// the cap thread call the production snippet records so it can re-arm itself (#317)
static void *cap = (void *)1; static void *fHoldCapCall = nullptr;
int main(){for (uint32_t fAutoGoSettleMs : {0u,500u,100000u,120000u,4294967295u}){
PRODUCTION
assert(dl-now==(uint64_t)fAutoGoSettleMs+40000);
assert(now+(uint64_t)fAutoGoSettleMs+1000<dl);
assert(now+(uint64_t)fAutoGoSettleMs+40001>dl);
assert(fHoldCapCall==cap && "the cap keeps its thread call to re-arm while the bring-up runs");
printf("PASS settle %u ms: deadline %llu ms\n",fAutoGoSettleMs,(unsigned long long)(dl-now));
}
assert(100000u>40000u); puts("PASS original 100 s settle exceeds original 40 s cap");}
'''
VENDOR = r'''
#import <Foundation/Foundation.h>
#include <stdint.h>
#include <string.h>
static const char *deviceName;
const char *nvmtl_vk_device_name(void){return deviceName;}
void nvlog(const char *fmt,...){ }
struct nvmtl_hwinfo {
    uint32_t magic;
    uint32_t version;
    uint32_t arch_family;
    uint32_t arch_subtype;
    uint32_t sm_major, sm_minor;
    uint32_t flags, reserved0;
    uint64_t reserved[4];
};
#define NVMTL_HWINFO_MAGIC   0x5748564Eu
#define NVMTL_FAMILY_NVIDIA  0x01000016u

PRODUCTION
int main(int argc,char **argv){deviceName=argc>1?argv[1]:NULL; const struct nvmtl_hwinfo *h=nvmtl_vendor_hwinfo();printf("%u %u %d\n",h->sm_major,h->sm_minor,nvmtl_vendor_chip_supported());}
'''
LOG = r'''
#include "nvmtl_log_failure.h"
#include <assert.h>
#include <stdio.h>
int main(void){
 const char *errors[]={"vk: vkCreateDevice -> -8","vk: missing vkCreateBuffer","vendor: cannot load plugin","unsupported memory type","sampler refused","GPU fault","FAILED pipeline","REFUSED allocation","vk: framebuffer fetch set could not be made (mask 0x7) - this draw is skipped","draw: index offset 64 outside its 32-byte buffer - draw skipped","vk: descriptor pool CEILING: 64 pools x 256 sets all live in one frame - draws are being skipped"};
 for(unsigned i=0;i<sizeof(errors)/sizeof(errors[0]);i++)assert(nvmtl_log_failure(errors[i]));
 assert(!nvmtl_log_failure("device RTX 3060, buffers live 8"));
 assert(!nvmtl_log_failure("vk: initialized successfully"));
 assert(!nvmtl_log_failure("library default.metallib loaded"));
 assert(!nvmtl_log_failure("library DEFAULT.metallib loaded"));
 assert(!nvmtl_log_failure("library key memo: hit 3 (12 B mem) - whole-library hash skipped; misses 0 stale 0 stored 3"));
 assert(nvmtl_log_failure("GPU_FAULT"));assert(!nvmtl_log_failure(0));
 puts("PASS 11 release failure messages retained (dropped draws included), normal chatter and null excluded");}
'''
UPLOAD = r'''
import Foundation
var files=(0..<50).map { URL(fileURLWithPath: "old-\($0).txt") }
files += ["hardware-map.json","crash-report.txt","driver-state.txt","driver-plugin-log.txt","collect.txt","driver-kernel-log.txt","driver-display.txt","previous-boot-kernel-log.txt","display-trace.txt","display-kernel.txt","display-ws.txt","gpu-all.txt","driver-update-log.txt","diagnostic-session.json","driver-wsreset.log.txt"].map {URL(fileURLWithPath:$0)}
files += (0..<3).map { URL(fileURLWithPath: "macos-WindowServer-\($0).ips.txt") }
            PRODUCTION
let sent=Set(files.prefix(important.count + 3).map { $0.lastPathComponent })  // every priority file + the 3 WindowServer reports
assert(Set(important).isSubset(of:sent))
assert(Set((0..<3).map { "macos-WindowServer-\($0).ips.txt" }).isSubset(of:sent))
print("PASS essential diagnostics and WindowServer reports survive 50 competing files")
'''

SUBMIT = r'''

#import <Foundation/Foundation.h>
#include <string.h>
#include <stdio.h>
static const char *program;
static const char *fixture_program(void) { return program; }
#define getprogname fixture_program
#define MTLCommandBufferStatusError 5
// an NVAccel without frame marks: WindowServer keeps its own wait for each composite
static BOOL nvmtl_ws_kernel_waits(void) { return NO; }
@interface Command : NSObject
@property int status;
@property int completed, scheduled, commits;
- (void)commit;
- (void)waitUntilCompleted;
- (void)waitUntilScheduled;
- (BOOL)commitAndWaitUntilSubmitted;
@end
@implementation Command
- (void)commit { self.commits++; }
- (void)waitUntilCompleted { self.completed++; }
- (void)waitUntilScheduled { self.scheduled++; }
PRODUCTION
@end
int main(int argc,char **argv) { @autoreleasepool {
 program=argc>1?argv[1]:NULL;
 BOOL ws=program && !strcmp(program,"WindowServer");
 Command *cb=[Command new];
 if (![cb commitAndWaitUntilSubmitted] || cb.commits!=1 ||
     cb.completed!=(ws?1:0) || cb.scheduled!=(ws?0:1)) return 42;
 cb.status=MTLCommandBufferStatusError;
 if ([cb commitAndWaitUntilSubmitted]) return 43;
 puts("PASS display completion ordering and error return");
 return 0;
}}
'''

def device_table():
    # the driver's own list is the source of truth; 1401's copy (nvidia_gsp_ids.json) is checked against it in 1401
    keys = re.search(r'^SUPPORTED_IDS="\n(.*?)\n"$', (ROOT / "package/install.sh").read_text(), re.S | re.M).group(1).split()
    assert keys == sorted(set(keys)) and all(re.fullmatch(r"[0-9A-F]{4}", k) for k in keys), "ids: sorted, unique, 4 upper-case hex"
    count = re.search(r"carries (\d+) distinct display-card PCI device IDs", (ROOT / "docs/CARD-SUPPORT.md").read_text())
    assert count and int(count.group(1)) == len(keys), ("docs/CARD-SUPPORT.md", count and count.group(1), len(keys))
    print("PASS device table: %d ids; the installer and CARD-SUPPORT.md agree" % len(keys))

device_table()
with tempfile.TemporaryDirectory(prefix="nullmoth-card-tests-") as directory:
    tmp = Path(directory)
    timing = snippet("kexts/NVRM/NVRM.cpp", "uint64_t dl, settle;", "retain(); thread_call_enter_delayed")
    (tmp / "timing.cpp").write_text(TIMING.replace("PRODUCTION", timing))
    run("xcrun", "clang++", "-std=c++11", str(tmp / "timing.cpp"), "-o", str(tmp / "timing"))
    print(run(str(tmp / "timing")))
    vendor = snippet("plugin/NVMTLVendorCompiler.m", "static const struct nvmtl_hwinfo *nvmtl_vendor_hwinfo(void) {", "static dispatch_data_t nvmtl_vendor_target_data")
    (tmp / "vendor.m").write_text(VENDOR.replace("PRODUCTION", vendor))
    run("xcrun", "clang", "-fblocks", str(tmp / "vendor.m"), "-framework", "Foundation", "-o", str(tmp / "vendor"))
    cases = {"TU106": "7 5 1", "GA100": "8 0 1", "GA10B": "8 7 1", "GA106": "8 6 1", "AD107": "8 9 1", "GH100": "9 0 1", "GB100": "10 0 1", "GB206": "12 0 1", "UNKNOWN": "0 0 0", "XX999": "0 0 0"}
    for chip, expected in cases.items():
        assert run(str(tmp / "vendor"), "NVIDIA (NVK " + chip + ")") == expected, chip
    assert run(str(tmp / "vendor")) == "0 0 0"
    print("PASS known SM targets and unknown-chip refusal")
    assert "return on == 1 && nvmtl_vendor_chip_supported();" in (ROOT / "plugin/NVMTLVendorCompiler.m").read_text()
    (tmp / "log.c").write_text(LOG)
    run("xcrun", "clang", "-I" + str(ROOT / "plugin"), str(tmp / "log.c"), "-o", str(tmp / "log"))
    print(run(str(tmp / "log")))
    submit = snippet("plugin/NVMTLObjects.m", "- (BOOL)commitAndWaitUntilSubmitted {", "- (void)commit {\n    [self nvmtlSubmitWithEvents];")
    (tmp / "submit.m").write_text(SUBMIT.replace("PRODUCTION", submit))
    run("xcrun", "clang", "-fobjc-arc", str(tmp / "submit.m"), "-framework", "Foundation", "-o", str(tmp / "submit"))
    for program in ("WindowServer", "mediaanalysisd", ""):
        print(run(str(tmp / "submit"), program))
    old = "- (BOOL)commitAndWaitUntilSubmitted { [self commit]; [self waitUntilScheduled]; return self.status != MTLCommandBufferStatusError; }"
    (tmp / "mutant.m").write_text(SUBMIT.replace("PRODUCTION", old))
    run("xcrun", "clang", "-fobjc-arc", str(tmp / "mutant.m"), "-framework", "Foundation", "-o", str(tmp / "mutant"))
    assert subprocess.run([str(tmp / "mutant"), "WindowServer"]).returncode == 42
    print("PASS original submission race rejected by the display test")
    # the log-upload checks moved with the app to 1401 (mac/tools/test_card_support.py)
print("PASS all card-support CPU regressions")
