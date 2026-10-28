#!/usr/bin/env python3
"""The SPIR-V cache folder is one the process can really write. Sandboxed agents (iconservicesagent, 241 log lines in
one upload) could not create the folder beside their AIR cache, so every store failed and every launch translated its
shaders again. Compiles the production nvmtl_dir_writable/nvmtl_spvcache_dir and runs three processes: the folder beside
the AIR cache writable; not writable (a read-only parent) with the temporary folder writable; neither writable."""
import os
import pathlib
import stat
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
if 'static NSString *nvmtl_spvcache_dir(void)' not in src:
    sys.exit('FAIL the SPIR-V cache folder is not chosen by a write test (nvmtl_spvcache_dir missing)')
start = src.index('static BOOL nvmtl_dir_writable(NSString *d)')
end = src.index('static NSString *nvmtl_spvcache_path(NSData *air, NSString *stage)')
funcs = src[start:end]

HARNESS = r'''
#import <Foundation/Foundation.h>
#include <unistd.h>
#include <dirent.h>
#include <errno.h>
#include <sys/stat.h>
#define nvlog(...) (fprintf(stderr, __VA_ARGS__), fputc('\n', stderr))
static NSString *nvmtl_aircache_dir(void) { return @(getenv("TEST_AIRCACHE")); }
#define NSTemporaryDirectory() (@(getenv("TEST_TMP")))
// the process's own cache folder (confstr _CS_DARWIN_USER_CACHE_DIR) comes from the test too
static size_t test_confstr(char *buf, size_t len) { const char *u = getenv("TEST_UCD"); return (size_t)snprintf(buf, len, "%s", u) + 1; }
#define confstr(name, buf, len) test_confstr(buf, len)
#include <CommonCrypto/CommonDigest.h>
// defined elsewhere in NVMTLLibraryLoad.m, outside the extracted functions
static char gXlateTag[80] = "test";
static void nvmtl_spvcache_trim_async(NSString *dir) { (void)dir; }
''' + funcs + r'''
int main(void) { NSString *d = nvmtl_spvcache_dir(); printf("%s\n", d ? d.UTF8String : "(none)"); return 0; }
'''

fails = 0


def check(name, ok, detail):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + f' ({detail})')
    fails += not ok


with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    c, exe = d / 'spv.m', d / 'spv'
    c.write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-o', str(exe), str(c), '-framework', 'Foundation'],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-1500:])
        sys.exit('could not compile nvmtl_spvcache_dir - check is UNKNOWN, not clean')
    good = d / 'good' / 'aircache'
    good.mkdir(parents=True)
    locked = d / 'locked'
    (locked / 'aircache').mkdir(parents=True)
    os.chmod(locked, stat.S_IRUSR | stat.S_IXUSR)
    tmp_ok = d / 'tmp'
    tmp_ok.mkdir()
    tmp_bad = d / 'tmpbad'
    tmp_bad.mkdir()
    os.chmod(tmp_bad, stat.S_IRUSR | stat.S_IXUSR)
    try:
        def run(air, tmpdir, ucd):
            env = dict(os.environ, TEST_AIRCACHE=str(air), TEST_TMP=str(tmpdir), TEST_UCD=str(ucd))
            return subprocess.run([str(exe)], capture_output=True, text=True, env=env, timeout=60).stdout.strip()
        ucd_ok = d / 'ucd'
        ucd_ok.mkdir()
        a = run(good, tmp_ok, ucd_ok)
        check('the folder beside the AIR cache is used when it can be written', a == str(d / 'good' / 'spvcache'), a)
        o = run(locked / 'aircache', tmp_ok, ucd_ok)
        check("a sandboxed process (WindowServer) caches in its own named cache folder first",
              o.startswith(str(ucd_ok)) and o.endswith('nvmtl/spvcache'), o)
        b = run(locked / 'aircache', tmp_ok, tmp_bad)
        check('with its own folder refused too, it falls back to the process temporary folder',
              b.startswith(str(tmp_ok)) and b.endswith('nvmtl/spvcache'), b)
        n = run(locked / 'aircache', tmp_bad, tmp_bad)
        check('with no writable folder the cache is off (no path, so no store attempts)', n == '(none)', n)
    finally:
        os.chmod(locked, stat.S_IRWXU)
        os.chmod(tmp_bad, stat.S_IRWXU)
sys.exit(1 if fails else 0)
