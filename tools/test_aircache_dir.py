#!/usr/bin/env python3
"""The AIR cache folder is one the process can really write. A sandbox can answer access(W_OK) with yes and then refuse
the open (WindowServer's sandbox allows writes only inside its own named folders), so the old choice kept a folder
nothing could be stored in and every boot decoded every shader again. Compiles the production nvmtl_aircache_dir and
runs it with access() answering yes for everything: the shared folder refusing writes must lose to the process's own
named cache folder, and a writable shared folder must still win."""
import os, pathlib, stat, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
start = src.index('static NSString *nvmtl_aircache_dir(void)')
end = src.index('static _Atomic unsigned long gSpvHit')
funcs = src[start:end]
wstart = src.index('static BOOL nvmtl_dir_writable(NSString *d) {')
wend = src.index('\n}\n', wstart) + 3
writable = src[wstart:wend]
HARNESS = r'''
#import <Foundation/Foundation.h>
#include <unistd.h>
#include <limits.h>
#define nvlog(...) ((void)0)
#define access(p, m) 0   /* a sandbox answers yes here and refuses at open */
#define NVMTL_AIRCACHE_DIR_VAL getenv("TEST_SHARED")
static size_t test_confstr(char *buf, size_t len) { const char *u = getenv("TEST_UCD"); return (size_t)snprintf(buf, len, "%s", u) + 1; }
#define confstr(name, buf, len) test_confstr(buf, len)
#define NSTemporaryDirectory() (@(getenv("TEST_TMP")))
#define NSSearchPathForDirectoriesInDomains(a, b, c) (@[ @(getenv("TEST_CACHES")) ])
#define getprogname() "WindowServer"
#define NVMTL_AIRCACHE_DIR "__SHARED__"
#include "nvmtl_failed_air.h"   /* the slice below also holds the keeper for AIR the translator refused */
''' + writable + funcs + r'''
int main(void) { NSString *d = nvmtl_aircache_dir(); printf("%s\n", d ? d.UTF8String : "(none)"); return 0; }
'''
fails = 0
def check(name, ok, detail):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + f' ({detail})'); fails += not ok

with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    shared = d / 'shared'; shared.mkdir()
    ucd = d / 'ucd'; ucd.mkdir()
    caches = d / 'caches_ro'; caches.mkdir()
    tmp = d / 'tmp'; tmp.mkdir()
    exe = d / 'a'
    (d / 'a.m').write_text(HARNESS.replace('__SHARED__', str(shared)))
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-I', str(root / 'plugin'), '-o', str(exe), str(d / 'a.m'), '-framework', 'Foundation'], capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-1500:]); sys.exit('could not compile nvmtl_aircache_dir - check is UNKNOWN, not clean')
    def run():
        env = dict(os.environ, TEST_UCD=str(ucd), TEST_TMP=str(tmp), TEST_CACHES=str(caches))
        env.pop('NVMTL_AIRCACHE', None)
        return subprocess.run([str(exe)], capture_output=True, text=True, env=env, timeout=60).stdout.strip()
    try:
        a = run()
        check('a shared folder that takes writes is used', a == str(shared), a)
        os.chmod(shared, stat.S_IRUSR | stat.S_IXUSR)   # the sandbox case: the folder looks usable but refuses the write
        os.chmod(caches, stat.S_IRUSR | stat.S_IXUSR)
        b = run()
        check('a shared folder that refuses writes loses to the process-named cache folder',
              b == str(ucd / 'WindowServer' / 'nvmtl' / 'aircache') or b == str(ucd / 'nvmtl' / 'aircache'), b)
        check('the choice is a folder that really takes a write', b and os.access(b, os.W_OK), b)
    finally:
        for p in (shared, caches): os.chmod(p, 0o755)
sys.exit(1 if fails else 0)
