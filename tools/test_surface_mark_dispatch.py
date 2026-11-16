#!/usr/bin/env python3
"""Exercise production surface-mark dispatch and readiness without building kexts.

The OS/service/GPU doubles retain real mutex contention. A negative control moves
the client gate ahead of the same production dispatch and must time out in the
capture -> completion -> client gate -> accelerator lock cycle.
"""
import argparse
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools/rotation'))
from test_support import method

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sanitizer', choices=('address,undefined', 'thread'), default='address,undefined')
args = parser.parse_args()
nvrm = (ROOT / 'kexts/NVRM/NVRM.cpp').read_text()
accel = (ROOT / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
pm = (ROOT / 'kexts/NVRM/nv-xnu.h').read_text()
escape = method(nvrm, 'IOReturn NVRMUserClient::esc(')
locked_escape = escape.replace('    Gate g(fGate);\n', '', 1)
locked_escape = locked_escape.replace('NVRMUserClient::esc(', 'LockedClient::esc(', 1)
locked_escape = locked_escape.replace('\n{\n', '\n{\n    Gate g(fGate);\n', 1)
mark_dispatch = method(accel, 'static IOReturn nvAccelSurfVram(')
mark_dispatch = mark_dispatch[mark_dispatch.index('    if (p->version'):mark_dispatch.index('    // The plugin re-marks')]
pending = accel[accel.index('static int gSurfGpuOn ='):accel.index('SYSCTL_INT(_debug, OID_AUTO, nvaccel_surface_gpu,')]

fixture = r'''
#include <atomic>
#include <cassert>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <thread>
#include <vector>
#define NVRM_SS_KERNEL_ABI
#include "nvrm_surfshare_abi.h"
using SInt32 = int32_t;
using SInt64 = int64_t;
using NvU16 = uint16_t;
using NV_STATUS = unsigned;
using IOReturn = int;
using task_t = void *;
enum { kIOReturnSuccess=0, kIOReturnNotAttached=1, kIOReturnNotReady=2,
       kIOReturnBadArgument=3, kIOReturnError=4, NV_OK=0, NV_TRUE=1, NV_FALSE=0,
       NV_ESC_CARD_INFO=0xc8, NV_ESC_CHECK_VERSION_STR=0xd2,
       NV_GPU_BAR_INDEX_REGS=0, NV_GPU_BAR_INDEX_FB=1, kMillisecondScale=1, THREAD_UNINT=0 };
#define kprintf(...) ((void)0)
#define nvu_memset std::memset
struct Latch {
    std::mutex mutex;
    std::condition_variable cv;
    bool signaled=false;
    void signal() { std::lock_guard<std::mutex> l(mutex); signaled=true; cv.notify_all(); }
    void wait() {
        std::unique_lock<std::mutex> l(mutex);
        assert(cv.wait_for(l, std::chrono::seconds(5), [&] { return signaled; }));
    }
};
struct IOLock { std::mutex mutex; std::condition_variable cv; };
static thread_local unsigned gateDepth, pmDepth;
static thread_local const char *caller="WindowServer";
enum Role { Main, Capture, Ordinary, Completion };
static thread_local Role role=Main;
static IOLock familyLock, *observedGate;
static Latch *gateAttempt, *dirtyEntered, *captureSleeping, *markEntered, *releaseMark;
static bool blockMark;
static IOLock *IOLockAlloc() { return new IOLock; }
static void IOLockFree(IOLock *l) { delete l; }
static void IOLockLock(IOLock *l) {
    if (l==observedGate && role==Ordinary && gateAttempt) gateAttempt->signal();
    l->mutex.lock();
}
static void IOLockUnlock(IOLock *l) { l->mutex.unlock(); }
static void IOLockWakeup(IOLock *l, void *, bool) { l->cv.notify_all(); }
static uint64_t mach_absolute_time() {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count();
}
static void clock_interval_to_deadline(unsigned ms, unsigned scale, uint64_t *deadline) {
    assert(scale==kMillisecondScale); *deadline=mach_absolute_time()+uint64_t(ms)*1000000;
}
static void absolutetime_to_nanoseconds(uint64_t time, uint64_t *ns) { *ns=time; }
static void IOLockSleepDeadline(IOLock *l, void *, uint64_t deadline, unsigned) {
    std::unique_lock<std::mutex> lock(l->mutex, std::adopt_lock);
    if (role==Capture && captureSleeping) captureSleeping->signal();
    l->cv.wait_until(lock, std::chrono::steady_clock::time_point(std::chrono::nanoseconds(deadline)));
    lock.release();  // IOLockSleepDeadline returns with the original lock held.
}
static bool OSCompareAndSwapPtr(void *old, void *value, void *volatile *address) {
    return __atomic_compare_exchange_n(address, &old, value, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
}
static bool OSCompareAndSwap(SInt32 old, SInt32 value, SInt32 *address) {
    return __atomic_compare_exchange_n(address, &old, value, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
}
static void OSAddAtomic64(SInt64 value, SInt64 *address) { __atomic_fetch_add(address, value, __ATOMIC_RELAXED); }
static void proc_selfname(char *name, int length) { std::snprintf(name, length, "%s", caller); }
static task_t current_task() { return reinterpret_cast<void *>(1); }
@@PENDING@@
@@GPU_ESCAPE@@
static bool isPending(unsigned id) {
    IOLockLock(gSurfPendLock);
    bool found=false; for (auto p:gSurfPend) if (p.id==id) found=true;
    IOLockUnlock(gSurfPendLock); return found;
}
static std::atomic<unsigned> dictionaries, dictionaryDrops, symbolDrops, serviceCalls;
static bool noMatch, noService, noSymbol;
static unsigned discoveryGoal, discoveries;
static std::mutex discoveryMutex;
static std::condition_variable discoveryCv;
struct OSDictionary { void release() { dictionaryDrops++; delete this; } };
struct OSSymbol {
    const char *name;
    static const OSSymbol *withCStringNoCopy(const char *name) { return noSymbol?nullptr:new OSSymbol{name}; }
    void release() const { symbolDrops++; delete this; }
};
struct IOService {
    std::atomic<unsigned> references{1};
    static OSDictionary *serviceMatching(const char *name) {
        assert(!std::strcmp(name,"NVAccel"));
        if (noMatch) return nullptr;
        dictionaries++; return new OSDictionary;
    }
    static IOService *waitForMatchingService(OSDictionary *, uint64_t);
    void release() { assert(references.fetch_sub(1)>1); }
    IOReturn callPlatformFunction(const OSSymbol *, bool, void *, void *, void *, void *);
} service;
IOService *IOService::waitForMatchingService(OSDictionary *m, uint64_t timeout) {
    assert(m && timeout==100000000);
    if (noService) return nullptr;
    service.references++;
    if (discoveryGoal) {
        std::unique_lock<std::mutex> lock(discoveryMutex);
        discoveries++; discoveryCv.notify_all();
        assert(discoveryCv.wait_for(lock,std::chrono::seconds(5),[] { return discoveries==discoveryGoal; }));
    }
    return &service;
}
IOReturn IOService::callPlatformFunction(const OSSymbol *sym, bool wait, void *data, void *task, void *command, void *last) {
    assert(sym && !std::strcmp(sym->name,NVACCEL_SS_FN) && !wait && task==current_task() && !last);
    serviceCalls++;
    auto *p=static_cast<NVRMSurfShareEsc *>(data);
    const unsigned cmd=static_cast<unsigned>(reinterpret_cast<uintptr_t>(command));
    if (blockMark && cmd==NVRM_ESC_SURF_DIRTY && (p->plane & NVRM_SS_PLANE_GPU_MARK)) {
        markEntered->signal(); releaseMark->wait();
    }
    // Actual NVAccel mark/version dispatch and caller authorization run before
    // the ordinary surface/cache path (whose family lock is modeled below).
@@MARK_DISPATCH@@
    if (role==Ordinary && dirtyEntered) dirtyEntered->signal();
    std::lock_guard<std::mutex> family(familyLock.mutex);
    p->status=0; return kIOReturnSuccess;
}
static std::atomic<unsigned> pmEntries, pmExits, mapClears, stateAccesses, rmCalls, cleanups;
static int nv_xnu_pm_enter() { pmEntries++; pmDepth++; return 1; }
static void nv_xnu_pm_exit(int counted) { assert(counted && pmDepth); pmDepth--; pmExits++; }
@@PM_USE@@;
static void mapPendingClearThread() { assert(gateDepth); mapClears++; }
@@MAP_SCOPE@@;
struct Bar { uint64_t cpu_address, size; };
struct nv_state_t { unsigned pci_info=0, gpu_id=0, interrupt_line=0; Bar bars[2]={}; };
struct nv_ioctl_card_info_t {
    unsigned valid, pci_info, gpu_id, interrupt_line;
    uint64_t reg_address, reg_size, fb_address, fb_size;
    unsigned minor_number;
};
static NV_STATUS rm_perform_version_check(void *, void *, unsigned) { assert(gateDepth && pmDepth); return NV_OK; }
static NV_STATUS rm_ioctl(void *, nv_state_t *, int *, unsigned, void *, unsigned) {
    assert(gateDepth && pmDepth); rmCalls++; return NV_OK;
}
static void rm_cleanup_file_private(void *, nv_state_t *, int *) { assert(gateDepth && pmDepth); cleanups++; }
static bool nv_xnu_chat() { return false; }
static void nv_pm_close(uint64_t) {}
struct Owner { unsigned fPassDone=2; nv_state_t fNv; };
struct Mapping { struct { bool valid=true; } context; };
struct NVRMUserClient {
    Owner owner;
    Owner *fOwner=&owner;
    IOLock gate, mapLock;
    IOLock *fGate=&gate, *fMapLock=&mapLock;
    SInt32 fCleaned=0;
    bool fIsCtl=true;
    int fNvfp=0, fFd=-1;
    uint64_t fPm=0;
    Mapping *fMapList=nullptr;
    std::atomic<unsigned> terminations{0};
    struct Gate {
        IOLock *l;
        explicit Gate(IOLock *lock):l(lock) { if (l) { IOLockLock(l); gateDepth++; } }
        ~Gate() { if (l) { gateDepth--; IOLockUnlock(l); } }
    };
    nv_state_t *nvState() { stateAccesses++; return &fOwner->fNv; }
    void terminate() { terminations++; }
    IOReturn esc(uint32_t,void *,uint32_t);
    IOReturn clientClose();
};
static IOLock *gNvClientsLock;
static NVRMUserClient *gNvClients[1];
struct LockedClient:NVRMUserClient { IOReturn esc(uint32_t,void *,uint32_t); };
@@HELPERS@@
@@ESCAPE@@
@@LOCKED_ESCAPE@@
@@CLOSE@@
static NVRMSurfShareEsc request(unsigned id=0, unsigned plane=NVRM_SS_PLANE_GPU_MARK) {
    NVRMSurfShareEsc p={}; p.version=NVRM_SURFSHARE_VERSION; p.surfaceID=id; p.plane=plane;
    p.hSrcClient=p.hSrcMemory=0xdead; return p;
}
template<class Client> static void cycle(bool expectTimeout) {
    Client client;
    auto p=request(7,NVRM_SS_PLANE_GPU_MARK|1);
    assert(client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p)==kIOReturnSuccess && !p.status);
    Latch sleeping, dirtied;
    captureSleeping=&sleeping; dirtyEntered=&dirtied;
    std::atomic<bool> copied{false}; bool ready=false;
    std::atomic<unsigned> pixels{0}; unsigned readback=0;
    const auto timeouts=gCntSurfTimeouts;
    std::thread capture([&] {
        role=Capture;
        std::lock_guard<std::mutex> family(familyLock.mutex);
        ready=nvSurfWaitReady(7); readback=pixels; copied=true;
    });
    sleeping.wait(); assert(!copied && isPending(7));
    std::thread commit([&] {
        role=Ordinary; auto dirty=request(7,0);
        assert(client.esc(NVRM_ESC_SURF_DIRTY,&dirty,sizeof dirty)==kIOReturnSuccess);
    });
    dirtied.wait(); assert(!copied);
    std::thread completion([&] {
        role=Completion; pixels=0x1234; auto done=request(7);
        assert(client.esc(NVRM_ESC_SURF_DIRTY,&done,sizeof done)==kIOReturnSuccess && !done.status);
    });
    capture.join(); commit.join(); completion.join();
    assert(copied && readback==0x1234 && ready!=expectTimeout);
    assert(gCntSurfTimeouts-timeouts==expectTimeout && !isPending(7));
    captureSleeping=dirtyEntered=nullptr;
}
static void gated(NVRMUserClient &client, unsigned cmd, NVRMSurfShareEsc p, unsigned size,
                  IOReturn expected, unsigned calls) {
    Latch attempted;
    observedGate=client.fGate; gateAttempt=&attempted;
    const unsigned before=serviceCalls, pm=pmEntries, clears=mapClears, states=stateAccesses;
    std::atomic<bool> finished{false};
    client.gate.mutex.lock();
    std::thread ordinary([&] {
        role=Ordinary; assert(client.esc(cmd,&p,size)==expected); finished=true;
    });
    attempted.wait(); assert(!finished && serviceCalls==before && pmEntries==pm);
    client.gate.mutex.unlock(); ordinary.join();
    assert(finished && serviceCalls==before+calls && pmEntries==pm+1 && mapClears==clears+2 && stateAccesses==states+1);
    if (p.version!=NVRM_SURFSHARE_VERSION && calls) assert(p.status==NVRM_SS_BAD_VERSION);
    observedGate=nullptr; gateAttempt=nullptr;
}
static void dispatchChecks() {
    NVRMUserClient client;
    auto p=request(9,NVRM_SS_PLANE_GPU_MARK|1);
    assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && !p.status && isPending(9));
    const unsigned pm=pmEntries, clears=mapClears, states=stateAccesses;
    client.gate.mutex.lock();
    std::thread completion([&] {
        auto done=request(9); assert(!client.esc(NVRM_ESC_SURF_DIRTY,&done,sizeof done) && !done.status);
    });
    completion.join(); client.gate.mutex.unlock();
    assert(!isPending(9) && pmEntries==pm && mapClears==clears && stateAccesses==states);
    gated(client,NVRM_ESC_SURF_DIRTY,request(),sizeof p-1,kIOReturnBadArgument,0);
    gated(client,NVRM_ESC_SURF_DIRTY,request(),sizeof p+1,kIOReturnBadArgument,0);
    auto bad=request(); bad.version++;
    gated(client,NVRM_ESC_SURF_DIRTY,bad,sizeof bad,kIOReturnSuccess,1);
    gated(client,NVRM_ESC_SURF_DIRTY,request(9,0),sizeof p,kIOReturnSuccess,1);
    gated(client,NVRM_ESC_SURF_DIRTY,request(9,NVRM_SS_PLANE_TRYLOCK),sizeof p,kIOReturnSuccess,1);
    gated(client,NVRM_ESC_SURF_VRAM,request(),sizeof p,kIOReturnSuccess,1);
    gated(client,0x42,request(),sizeof p,kIOReturnSuccess,0); assert(rmCalls==1);
    // Adapter readiness/PM accounting still guard ordinary RM use. Marks
    // remain stateless, including during bring-up or system PM transitions.
    client.owner.fPassDone=1; const unsigned beforePm=pmEntries, beforeStates=stateAccesses;
    assert(client.esc(0x42,&p,sizeof p)==kIOReturnNotReady);
    assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && !p.status);
    assert(pmEntries==beforePm && stateAccesses==beforeStates); client.owner.fPassDone=2;
    // Production NVAccel authorization refuses other processes and disabled marks.
    caller="OBS"; p=request(9);
    assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && p.status==NVRM_SS_OFF && isPending(9));
    caller="WindowServer"; gSurfGpuOn=0;
    assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && p.status==NVRM_SS_OFF && isPending(9));
    gSurfGpuOn=1; assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && !p.status && !isPending(9));
    // An already-running stateless mark can finish while real clientClose
    // tears down RM/mappings. New marks after close never enter the service.
    Latch entered, released; markEntered=&entered; releaseMark=&released; blockMark=true;
    Mapping mapping; client.fMapList=&mapping;
    std::thread inFlight([&] { auto done=request(); assert(!client.esc(NVRM_ESC_SURF_DIRTY,&done,sizeof done)); });
    entered.wait(); assert(!client.clientClose() && client.terminations==1 && cleanups==1 && !mapping.context.valid);
    released.signal(); inFlight.join(); blockMark=false;
    const unsigned calls=serviceCalls;
    client.gate.mutex.lock();
    assert(client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p)==kIOReturnNotAttached && serviceCalls==calls);
    client.gate.mutex.unlock();
    assert(pmEntries==pmExits && !gateDepth && !pmDepth);
    puts("PASS: narrow dispatch, ordinary gate/PM/mapping scope, authorization, adapter readiness and concurrent close");
}
int main(int argc,char **argv) {
    assert(argc==2); const char *mode=argv[1];
    if (!std::strcmp(mode,"cache")) {
        constexpr unsigned count=8; discoveryGoal=count;
        NVRMUserClient clients[count]; std::vector<std::thread> threads;
        for (unsigned i=0;i<count;i++) threads.emplace_back([&,i] {
            auto p=request(); assert(!clients[i].esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && !p.status);
        });
        for (auto &t:threads) t.join();
        assert(discoveries==count && service.references==2 && dictionaries==count && dictionaryDrops==count);
        assert(serviceCalls==count && symbolDrops==count && !pmEntries && !mapClears && !stateAccesses);
        auto p=request(); assert(!clients[0].esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p));
        assert(dictionaries==count && service.references==2);
        puts("PASS: concurrent accelerator discovery retains one reference and releases every losing reference");
    } else if (!std::strcmp(mode,"dispatch")) dispatchChecks();
    else if (!std::strcmp(mode,"cycle")) {
        for (unsigned i=0;i<8;i++) cycle<NVRMUserClient>(false);
        puts("PASS: capture readiness completes through marks while commit holds the client gate");
    } else if (!std::strcmp(mode,"locked-control")) {
        cycle<LockedClient>(true);
        puts("PASS: negative control reproduces the 100 ms capture/client-gate lock cycle");
    } else {
        noMatch=!std::strcmp(mode,"no-match"); noService=!std::strcmp(mode,"no-service"); noSymbol=!std::strcmp(mode,"no-symbol");
        assert(noMatch || noService || noSymbol); NVRMUserClient client; auto p=request();
        assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && p.status==NVRM_SS_OFF && !p.hSrcClient && !p.hSrcMemory);
        assert(dictionaries==dictionaryDrops && !serviceCalls && !pmEntries && !mapClears && !stateAccesses);
        noMatch=noService=noSymbol=false;
        assert(!client.esc(NVRM_ESC_SURF_DIRTY,&p,sizeof p) && !p.status && serviceCalls==1 && service.references==2);
        puts("PASS: unavailable service/symbol fails closed and a later call can retry");
    }
}
'''
fixture = fixture.replace('@@PENDING@@', pending).replace('@@GPU_ESCAPE@@', method(accel, 'static unsigned nvSurfGpuEscape('))
fixture = fixture.replace('@@MARK_DISPATCH@@', mark_dispatch)
fixture = fixture.replace('@@PM_USE@@', method(pm, 'struct NvXnuPmUse {'))
fixture = fixture.replace('@@MAP_SCOPE@@', method(nvrm, 'struct NvMapPendingScope {'))
fixture = fixture.replace('@@HELPERS@@', method(nvrm, 'static IOService *nvrmSurfaceAccelerator(') + '\n' +
                          method(nvrm, 'static IOReturn nvrmSurfaceEscape('))
fixture = fixture.replace('@@ESCAPE@@', escape).replace('@@LOCKED_ESCAPE@@', locked_escape)
fixture = fixture.replace('@@CLOSE@@', method(nvrm, 'IOReturn NVRMUserClient::clientClose('))
with tempfile.TemporaryDirectory(prefix='nv-surface-mark-') as directory:
    path = Path(directory)
    (path / 'test.cpp').write_text(fixture)
    subprocess.run(['xcrun', 'clang++', '-std=c++17', '-O1', '-g', '-pthread', '-Wall', '-Wextra', '-Werror',
                    '-Wno-unused-parameter', '-Wno-unused-variable', '-Wno-unused-function',
                    '-fsanitize=' + args.sanitizer, '-fno-sanitize-recover=all',
                    '-I' + str(ROOT / 'kexts/NVRM'), str(path / 'test.cpp'), '-o', str(path / 'test')], check=True)
    for mode in ('cache', 'dispatch', 'cycle', 'locked-control', 'no-match', 'no-service', 'no-symbol'):
        subprocess.run([str(path / 'test'), mode], check=True, timeout=20)
