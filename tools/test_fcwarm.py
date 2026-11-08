#!/usr/bin/env python3
"""Function-constant warm-up: a recorded variant is translated ahead of the caller into exactly the file a live miss
writes.

WindowServer's main thread translated Core Animation's fixed_frag variants one after another after a driver update
(each 0.5-2 s; the watchdog kills it after 40 s without a check-in). The plugin now records slow sets beside the cache
and, the first time a process asks for any variant of that AIR text, translates the recorded siblings on background
threads. This compiles the production code (the cache key, nvmtl_translate_with_fc, nvmtl_translate_with_fc_cached
and the warm-up) out of NVMTLLibraryLoad.m against the real libnvmtl_translate.dylib, and runs "boots":

  boot 1  live misses for five sets: files + records
  update  every cache file deleted (a new translator build sweeps them), records kept
  boot 2  the caller asks for ONE set; the other four must appear in the background, while one never recorded must not
  ref     a fresh cache, each set translated live on its own: names and bytes must equal boot 2's warmed files
  join    with the files gone again, a second set asked for right after the first is read or joined, never translated
  timing  five sets asked for at once: one live miss and the rest joined, against five misses without the warm-up
  off     NVMTL_FCWARM_OFF=1: boot 2 warms nothing (the control that must fire)

usage: test_fcwarm.py <libnvmtl_translate.dylib> <fixed_frag .ll> <sets file: idx:hex,... per line> [repo root]"""
import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

lib, ll, sets_file = sys.argv[1:4]
root = pathlib.Path(sys.argv[4] if len(sys.argv) > 4 else pathlib.Path(__file__).resolve().parents[1])
src = (root / "plugin/NVMTLLibraryLoad.m").read_text()
fails = 0


def arm(name, ok, detail=""):
    global fails
    fails += not ok
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else f" ({str(detail).strip()[-500:]})"))


def cut(start, end):
    i = src.index(start)
    return src[i:src.index(end, i)]


parts = [
    cut("static _Atomic unsigned long gSpvHit, gSpvMiss, gSpvStored;", "static _Atomic unsigned long long gSpvAdded;"),
    cut("@interface NVMTLAirDigest : NSObject", "static BOOL nvmtl_air_unroll_wanted(NSData *air);"),
    cut("static __thread int g_fc_expect_link;", "static int nvmtl_reflect_air("),
]
HARNESS = r'''
#import <Foundation/Foundation.h>
#include <CommonCrypto/CommonDigest.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <objc/runtime.h>
#include <os/lock.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <dirent.h>
#include <stdatomic.h>
#include <time.h>
#include <unistd.h>
#define nvlog(...) (fprintf(stderr, __VA_ARGS__), fputc('\n', stderr))
typedef int (*nvmtl_fc_fn)(const char *ll, const char *stage, const uint32_t *idx, const uint32_t *sizes,
                           const uint8_t *const *payloads, size_t n, uint8_t **out, size_t *out_len, char *err, size_t err_len);
typedef void (*nvmtl_free_fn)(uint8_t *p, size_t len);
static nvmtl_fc_fn xlate_fc;
static nvmtl_free_fn xlate_free;
static int nvmtl_xlate_ready(void) { return xlate_fc != NULL; }
static char gXlateTag[80] = "testbuild";
// the cache folder and file names: the plugin's naming, in a folder the test chooses
static NSString *nvmtl_spvcache_dir(void) { return @(getenv("TEST_CACHE")); }
static NSString *nvmtl_spvcache_path_digest(const unsigned char h[CC_SHA256_DIGEST_LENGTH], NSString *stage) {
    char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
    for (int i = 0; i < CC_SHA256_DIGEST_LENGTH; i++) snprintf(hex + 2 * i, 3, "%02x", h[i]);
    return [nvmtl_spvcache_dir() stringByAppendingPathComponent:[NSString stringWithFormat:@"%s.%@.%s.c3.spv", hex, stage, gXlateTag]];
}
BOOL nvmtl_needs_visible_link(NSString *air, NSString *why) { (void)air; (void)why; return NO; }
// declared in NVMTLObjects.m in the plugin's single translation unit
enum { NVMTL_AIRF_KNOWN = 1, NVMTL_AIRF_DYLD_TABLE = 2, NVMTL_AIRF_ISECT_TABLE = 4 };
''' + "\n".join(parts) + r'''
static NSArray *test_sets(const char *file) {
    NSMutableArray *out = [NSMutableArray array];
    for (NSString *line in [[NSString stringWithContentsOfFile:@(file) encoding:NSUTF8StringEncoding error:NULL] componentsSeparatedByString:@"\n"]) {
        if (!line.length) continue;
        NSMutableDictionary *d = [NSMutableDictionary dictionary];
        for (NSString *kv in [line componentsSeparatedByString:@","]) {
            NSArray *p = [kv componentsSeparatedByString:@":"];
            NSMutableArray *b = [NSMutableArray array];
            NSString *h = p[1];
            for (NSUInteger i = 0; i + 1 < h.length + 1 && i < h.length; i += 2)
                [b addObject:@((uint8_t)strtoul([h substringWithRange:NSMakeRange(i, 2)].UTF8String, NULL, 16))];
            d[p[0]] = b;
        }
        [out addObject:d];
    }
    return out;
}
// what the plugin's newFunctionWithName:constantValues: hands over: the dictionary plus packed index/size/payload arrays
static NSData *test_live(NSString *ll, NSDictionary *c, double *ms) {
    size_t n = c.count, k = 0;
    uint32_t idx[256], sz[256]; const uint8_t *pay[256]; uint8_t bytes[256][16];
    for (NSString *key in c) {
        NSArray *a = c[key]; idx[k] = (uint32_t)key.integerValue; sz[k] = (uint32_t)a.count;
        for (NSUInteger j = 0; j < a.count; j++) bytes[k][j] = [a[j] unsignedCharValue];
        pay[k] = bytes[k]; k++;
    }
    NSString *e = nil;
    uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    NSData *d = nvmtl_translate_with_fc_cached(ll, @"fragment", @"fixed_frag_lph_cpf", c, idx, sz, pay, n, &e);
    if (ms) *ms = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6;
    if (!d) fprintf(stderr, "translate failed: %s\n", e.UTF8String);
    return d;
}
static void test_wait_warm(void) {
    for (int i = 0; i < 1200; i++) {
        os_unfair_lock_lock(&gFcWarmLock);
        const NSUInteger busy = gFcWarmInFlight.count;
        os_unfair_lock_unlock(&gFcWarmLock);
        if (!busy) break;
        usleep(100000);
    }
    nvmtl_cache_io_drain();
}
int main(int argc, char **argv) {
    @autoreleasepool {
        void *h = dlopen(getenv("TEST_LIB"), RTLD_NOW);
        if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 2; }
        xlate_fc = (nvmtl_fc_fn)dlsym(h, "nvmtl_translate_fc"); xlate_free = (nvmtl_free_fn)dlsym(h, "nvmtl_translate_free");
        NSString *ll = [NSString stringWithContentsOfFile:@(getenv("TEST_LL")) encoding:NSUTF8StringEncoding error:NULL];
        NSArray *sets = test_sets(getenv("TEST_SETS"));
        // argv: a list of set numbers to ask for, in order; "w" waits for the warm-up to finish
        for (int i = 1; i < argc; i++) {
            if (!strcmp(argv[i], "w")) { test_wait_warm(); continue; }
            double ms = 0; NSData *d = test_live(ll, sets[(NSUInteger)atoi(argv[i])], &ms);
            printf("ASKED %s %lu B %.0f ms\n", argv[i], (unsigned long)d.length, ms);
        }
        test_wait_warm();
    }
    return 0;
}
'''

work = pathlib.Path(tempfile.mkdtemp(prefix="fcwarm-"))
(work / "t.m").write_text(HARNESS)
cc = subprocess.run(["clang", "-fobjc-arc", "-O1", "-w", "-framework", "Foundation", str(work / "t.m"), "-o", str(work / "t")],
                    capture_output=True, text=True)
arm("the production warm-up and cache code compile out of NVMTLLibraryLoad.m", cc.returncode == 0, cc.stderr)
if cc.returncode:
    sys.exit(1)


def boot(cache, asks, extra=None):
    env = dict(os.environ, TEST_LIB=lib, TEST_LL=ll, TEST_SETS=sets_file, TEST_CACHE=str(cache), **(extra or {}))
    t = time.time()
    p = subprocess.run([str(work / "t"), *asks], capture_output=True, text=True, env=env, timeout=900)
    return p, time.time() - t


def files(cache):
    return {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in pathlib.Path(cache).glob("*.c3.spv")}


A, B, C, D = (work / x for x in "ABCD")
for d in (A, B, C, D):
    d.mkdir()
# sets 1..4 are the recorded siblings (generic and specific), 0 is the one the caller asks for, 5 is never recorded
p, _ = boot(A, ["0", "1", "2", "3", "4"])
boot1 = files(A)
arm("boot 1: five live misses leave five cache files", len(boot1) == 5, p.stdout + p.stderr)
rec = (A / ".fcwarm.v1").read_text().splitlines() if (A / ".fcwarm.v1").exists() else []
arm("  and five records beside them (each took over 100 ms)", len(rec) == 5, rec)
for f in A.glob("*.c3.spv"):
    f.unlink()

p, secs = boot(A, ["0"])
boot2 = files(A)
arm("boot 2: asking for one set brings the four recorded siblings in the background", len(boot2) == 5, p.stdout + p.stderr)
arm("  and says so", "4 recorded variant(s) of this function translating in the background" in p.stderr, p.stderr)

ref = {}
for k in ("1", "2", "3", "4", "5"):
    shutil.rmtree(B); B.mkdir()
    boot(B, [k])
    ref[k] = files(B)
same = all(len(ref[k]) == 1 and next(iter(ref[k].items())) in boot2.items() for k in ("1", "2", "3", "4"))
arm("each warmed file has the live file's name and bytes (sets 1-4)", same, {k: ref[k] for k in ref} | {"boot2": boot2})
arm("negative control: a set nobody recorded (5) is not warmed", not (set(ref["5"]) & set(boot2)), (ref["5"], boot2))
diff_key = len({next(iter(ref[k])) for k in ("1", "2", "3", "4", "5")}) == 5
arm("negative control: a different constant set is a different cache file", diff_key, ref)

# join: records present, no files; ask for 0 then 1 at once - 1 is already running in the background
for f in A.glob("*.c3.spv"):
    f.unlink()
p, _ = boot(A, ["0", "1"])
arm("a sibling asked for right after the first set is read or joined, never translated again",
    p.stderr.count("fc cache: miss") == 1 and ("fc cache: HIT" in p.stderr or "JOINED" in p.stderr), p.stderr[-800:])
arm("  and gets the same bytes", files(A).items() >= {next(iter(ref["1"].items()))}, files(A))

shutil.copy(A / ".fcwarm.v1", C / ".fcwarm.v1")
p, _ = boot(C, ["0"], {"NVMTL_FCWARM_OFF": "1"})
arm("control: NVMTL_FCWARM_OFF=1 warms nothing", len(files(C)) == 1, (files(C), p.stderr[-300:]))

# timing: what the asking thread pays for five sets, cold, with and without warm records
shutil.copy(A / ".fcwarm.v1", D / ".fcwarm.v1")
p_w, _ = boot(D, ["0", "1", "2", "3", "4"])
E = work / "E"; E.mkdir()
p_c, _ = boot(E, ["0", "1", "2", "3", "4"], {"NVMTL_FCWARM_OFF": "1"})
asked = lambda out: sum(float(l.split()[-2]) for l in out.splitlines() if l.startswith("ASKED"))
w, c = asked(p_w.stdout), asked(p_c.stdout)
arm("asking for all five at once: one live miss, the rest joined or read", p_w.stderr.count("fc cache: miss") == 1
    and "JOINED the background translation" in p_w.stderr, p_w.stderr[-800:])
arm("control: without the warm-up all five are live misses", p_c.stderr.count("fc cache: miss") == 5, p_c.stderr[-400:])
print(f"     asking thread, five cold sets: {c:.0f} ms without warm-up, {w:.0f} ms with records ({p_w.stdout.count('ASKED')} asked)")
arm("the asking thread waits less with the warm-up", w < c, (w, c))
print(f"{fails} failed")
sys.exit(1 if fails else 0)
