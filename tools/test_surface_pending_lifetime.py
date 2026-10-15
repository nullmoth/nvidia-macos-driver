#!/usr/bin/env python3
"""Exercise pending-table reuse through the production resource destructor."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
table = source[source.index('enum { kNVSVPend ='):source.index('static void    *gNVSVRoot;')]
start = source.index('void NVResource::free()')
destructor = source[start:source.index('bool NVVidMemory::allocPhysical()', start)]
fixture = r'''
#include <cassert>
#include <cstdio>
#include <mutex>
#include <thread>
using IOSimpleLock = std::mutex;
typedef long long SInt64;  // a kernel type the extracted destructor now uses
static void IOSimpleLockLock(IOSimpleLock *lock) { lock->lock(); }
static void IOSimpleLockUnlock(IOSimpleLock *lock) { lock->unlock(); }
struct Descriptor { unsigned releases=0; void release() { releases++; } };
static unsigned gCntResFree;
static void OSAddAtomic64(int n, unsigned *value) { *value+=n; }
struct IOAccelResource2 { unsigned freed=0; void free() { freed++; } };
struct NVResource : IOAccelResource2 { Descriptor *fApDesc=nullptr; void free(); };
'''
checks = r'''
int main() {
    NVResource beforeInit; beforeInit.free(); assert(beforeInit.freed==1);
    IOSimpleLock lock; gNVSVPendLock=&lock;
    NVResource oldOwner, newOwner, unrelated;
    assert(nvSVPend(7,1,&oldOwner)); assert(nvSVPend(7,1,&newOwner));
    oldOwner.free(); assert(nvSVPend(7,2));
    unrelated.free(); assert(nvSVPend(7,2));
    newOwner.free(); assert(!nvSVPend(7,2));
    Descriptor descriptor;
    for(unsigned id=1;id<=10000;id++) {
        NVResource resource; resource.fApDesc=&descriptor;
        assert(nvSVPend(id,1,&resource)); resource.free();
        assert(resource.freed==1 && !resource.fApDesc && !nvSVPend(id,2));
    }
    assert(descriptor.releases==10000);
    NVResource owners[kNVSVPend];
    for(unsigned i=0;i<kNVSVPend;i++) assert(nvSVPend(i+1,1,&owners[i]));
    assert(!nvSVPend(kNVSVPend+1,1,&unrelated));
    owners[17].free(); assert(nvSVPend(kNVSVPend+1,1,&unrelated));
    unrelated.free();
    for(unsigned i=0;i<kNVSVPend;i++) nvSVForgetResource(&owners[i]);
    NVResource reused;
    for(unsigned i=0;i<1000;i++) {
        assert(nvSVPend(1,1,&reused)); assert(nvSVPend(2,1,&reused));
        nvSVForgetResource(&reused); assert(!nvSVPend(1,2) && !nvSVPend(2,2));
    }
    std::thread workers[4];
    for(unsigned w=0;w<4;w++) workers[w]=std::thread([w] {
        NVResource owner;
        for(unsigned i=0;i<1000;i++) {
            assert(nvSVPend(100+w,1,&owner));
            nvSVForgetResource(&owner);
        }
    });
    for(auto &worker:workers) worker.join();
    for(unsigned i=0;i<kNVSVPend;i++) assert(!gNVSVPendId[i] && !gNVSVPendResource[i]);
    puts("PASS: destructor cleanup, newer owners, 10000 lifetimes, exhaustion recovery, pointer reuse, concurrent owners");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + table + destructor + checks)
    subprocess.run(['clang++', '-std=c++17', '-pthread', str(path / 'test.cpp'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
