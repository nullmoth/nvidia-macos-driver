#!/usr/bin/env python3
"""Run the production notebook manager and RM helper with gated IOKit doubles.

The timer double implements the SDK's work-loop gate and disable/generation
contract. Firmware calls use real thread contention so suspend/stop must drain
an entered poll, and queued callbacks must become inert before RM teardown.
"""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--sanitizer', choices=('address,undefined', 'thread'), default='address,undefined')
args = parser.parse_args()

def without_includes(text):
    return re.sub(r'^#include[^\n]*\n', '', text, flags=re.M)

manager = without_includes((ROOT/'kexts/NVRM/nvrm_power.cpp').read_text())
helper = without_includes((ROOT/'kexts/NVRM/nvrm_pmgr.c').read_text())
fixture = r'''
#include <atomic>
#include <cassert>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <thread>
#include <vector>
#include "nvrm_power_policy.h"
using UInt32 = unsigned;
using NvU8 = unsigned char;
using NvBool = unsigned char;
using NV_STATUS = unsigned;
using IOReturn = int;
enum { NV_OK, NV_ERR_INVALID_ARGUMENT, NV_ERR_NOT_SUPPORTED, NV_ERR_GPU_NOT_FULL_POWER,
       NV_ERR_GENERIC, kIOReturnSuccess=0, NV_TRUE=1, NV_FALSE=0,
       API_LOCK_FLAGS_NONE=0, RM_LOCK_MODULES_OSAPI=0, GPUS_LOCK_FLAGS_NONE=0,
       THREAD_STATE_FLAGS_NONE=0, PDB_PROP_GPU_IS_MOBILE=1,
       kIOTimerEventSourceOptionsPriorityWorkLoop=255 };
struct Latch {
    std::mutex mutex; std::condition_variable cv; bool done=false;
    void signal() { std::lock_guard<std::mutex> l(mutex); done=true; cv.notify_all(); }
    void wait() {
        std::unique_lock<std::mutex> l(mutex);
        assert(cv.wait_for(l, std::chrono::seconds(5), [&] { return done; }));
    }
};
static bool failLoop, failTimer, failAttach;
static unsigned objects, loops, timers, registrations, controls, queries, refreshes;
static bool bootPresent=true, battery=true, ac=true, noPm;
static int bootValue=1;
static NV_STATUS apiStatus=NV_OK, gpuStatus=NV_OK, controlStatus=NV_OK;
static thread_local unsigned apiDepth, gpuDepth, loopDepth;
static Latch *controlEntered, *controlRelease, *actionAttempt;
static std::vector<int> sent;
struct OSObject {
    unsigned refs=1;
    OSObject() { ++objects; }
    virtual ~OSObject() { --objects; }
    virtual bool init() { return true; }
    virtual void free() {}
    void retain() { ++refs; }
    void release() { if (!--refs) { free(); delete this; } }
};
#define OSDeclareDefaultStructors(type)
#define OSDefineMetaClassAndStructors(type, base)
#define kprintf(...) ((void)0)
struct IOTimerEventSource;
struct IOWorkLoop : OSObject {
    std::recursive_mutex mutex;
    IOTimerEventSource *timer=nullptr;
    using Action = IOReturn (*)(OSObject *,void *,void *,void *,void *);
    IOWorkLoop() { ++loops; }
    ~IOWorkLoop() { --loops; }
    static IOWorkLoop *workLoop() { return failLoop?nullptr:new IOWorkLoop; }
    IOReturn addEventSource(IOTimerEventSource *);
    IOReturn removeEventSource(IOTimerEventSource *);
    IOReturn runAction(Action action, OSObject *owner, void *a=nullptr, void *b=nullptr,
                       void *c=nullptr, void *d=nullptr) {
        if (actionAttempt) actionAttempt->signal();
        std::lock_guard<std::recursive_mutex> lock(mutex);
        ++loopDepth; auto result=action(owner,a,b,c,d); --loopDepth; return result;
    }
};
static IOTimerEventSource *lastTimer;
struct IOTimerEventSource : OSObject {
    using Action = void (*)(OSObject *,IOTimerEventSource *);
    OSObject *owner; Action action; IOWorkLoop *loop=nullptr;
    bool enabled=true, pending=false; unsigned ms=0;
    std::atomic<unsigned> generation{0};
    IOTimerEventSource(OSObject *o, Action a):owner(o),action(a) { ++timers; lastTimer=this; }
    ~IOTimerEventSource() { --timers; lastTimer=nullptr; }
    static IOTimerEventSource *timerEventSource(unsigned options, OSObject *owner, Action action) {
        assert(options==kIOTimerEventSourceOptionsPriorityWorkLoop);
        return failTimer?nullptr:new IOTimerEventSource(owner,action);
    }
    void disable() { enabled=false; ++generation; }
    void enable() { enabled=true; }
    void cancelTimeout() { pending=false; ++generation; }
    int setTimeoutMS(unsigned value) { assert(loopDepth); ms=value; pending=true; return 0; }
    void fire(Latch *queued=nullptr, Latch *proceed=nullptr) {
        unsigned scheduled=generation.load();
        if (queued) queued->signal();
        if (proceed) proceed->wait();
        std::lock_guard<std::recursive_mutex> lock(loop->mutex);
        if (!enabled || !pending || scheduled!=generation.load()) return;
        pending=false; ++loopDepth; action(owner,this); --loopDepth;
    }
};
IOReturn IOWorkLoop::addEventSource(IOTimerEventSource *t) {
    if (failAttach) return 1;
    timer=t; t->loop=this; t->retain(); return 0;
}
IOReturn IOWorkLoop::removeEventSource(IOTimerEventSource *t) {
    std::lock_guard<std::recursive_mutex> lock(mutex);
    assert(timer==t && !t->enabled && !t->pending);
    t->loop=nullptr; timer=nullptr; t->release(); return 0;
}
static bool OSCompareAndSwap(unsigned expected, unsigned desired, volatile unsigned *where) {
    return __atomic_compare_exchange_n(where,&expected,desired,false,__ATOMIC_SEQ_CST,__ATOMIC_SEQ_CST);
}
static bool PE_parse_boot_argn(const char *name, void *out, unsigned size) {
    if (!std::strcmp(name,"-nvrmnopm")) return noPm;
    assert(!std::strcmp(name,"nvrmtgp") && size==sizeof(int));
    if (bootPresent) *static_cast<int *>(out)=bootValue; return bootPresent;
}
extern "C" void nv_xnu_power_refresh() { assert(!apiDepth && !gpuDepth); ++refreshes; }
extern "C" void nv_xnu_power_state(bool *plugged, bool *present) {
    if (plugged) *plugged=ac; if (present) *present=battery;
}
struct sysctl_req { void *newptr=nullptr; int *oldptr=nullptr; };
struct sysctl_oid { void *arg1; int flags; };
enum { CTLTYPE_INT=1, CTLFLAG_RW=2, CTLFLAG_RD=4, CTLFLAG_LOCKED=8 };
#define SYSCTL_PROC(parent, number, name, flags, value, arg2, handler, fmt, description) \
    static sysctl_oid sysctl__debug_##name{value,flags}
static int sysctl_handle_int(sysctl_oid *, int *value, int, sysctl_req *req) {
    if (req->oldptr) *req->oldptr=*value;
    if (req->newptr) *value=*static_cast<int *>(req->newptr); return 0;
}
static void sysctl_register_oid(sysctl_oid *) { ++registrations; }
static void sysctl_unregister_oid(sysctl_oid *) { assert(registrations); --registrations; }

// RM-internal ABI doubles. The actual helper below must still acquire the API
// lock before GPU locks and check mobile/full-power before the one control.
struct OBJGPU;
struct RM_API { NV_STATUS (*Control)(RM_API *,unsigned,unsigned,unsigned,void *,unsigned); };
struct OBJGPU {
    bool mobile=true, fullPower=true;
    unsigned hInternalClient=1, hInternalSubdevice=2;
    bool (*getProperty)(OBJGPU *,int)=[](OBJGPU *gpu,int property) {
        assert(apiDepth==1 && gpuDepth==1 && property==PDB_PROP_GPU_IS_MOBILE); ++queries; return gpu->mobile;
    };
};
struct nv_state_t { OBJGPU *gpu; };
using nvidia_stack_t = int;
struct THREAD_STATE_NODE {};
struct NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE_PARAMS { NvBool bEnable; };
enum { NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE=0x20800ad2 };
static NV_STATUS firmware(RM_API *,unsigned client,unsigned subdevice,unsigned command,void *data,unsigned size) {
    assert(loopDepth && apiDepth==1 && gpuDepth==1 && client==1 && subdevice==2);
    assert(command==0x20800ad2 && size==sizeof(NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE_PARAMS));
    auto *params=static_cast<NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE_PARAMS *>(data);
    ++controls; sent.push_back(params->bEnable);
    if (controlEntered) { controlEntered->signal(); controlRelease->wait(); }
    return controlStatus;
}
static RM_API physicalApi{firmware};
#define NV_GET_NV_PRIV_PGPU(nv) ((nv)->gpu)
#define FULL_GPU_SANITY_CHECK(gpu) ((gpu)->fullPower)
#define GPU_GET_PHYSICAL_RMAPI(gpu) (&physicalApi)
#define NV_ENTER_RM_RUNTIME(sp,fp) do { (void)(sp); (fp)=nullptr; } while (0)
#define NV_EXIT_RM_RUNTIME(sp,fp) do { (void)(sp); (void)(fp); } while (0)
static void threadStateInit(THREAD_STATE_NODE *,int) {}
static void threadStateFree(THREAD_STATE_NODE *,int) {}
static NV_STATUS rmapiLockAcquire(int,int) { assert(!apiDepth && !gpuDepth); if (!apiStatus) ++apiDepth; return apiStatus; }
static NV_STATUS rmDeviceGpuLocksAcquire(OBJGPU *,int,int) { assert(apiDepth==1 && !gpuDepth); if (!gpuStatus) ++gpuDepth; return gpuStatus; }
static void rmDeviceGpuLocksRelease(OBJGPU *,int,void *) { assert(gpuDepth==1); --gpuDepth; }
static void rmapiLockRelease() { assert(apiDepth==1 && !gpuDepth); --apiDepth; }
extern "C" {
@@HELPER@@
}
@@MANAGER@@

static unsigned checks;
static void check(bool ok, const char *message) {
    ++checks; if (!ok) { std::fprintf(stderr,"FAIL: %s\n",message); std::abort(); }
}
static int read(int *value) { return __atomic_load_n(value,__ATOMIC_RELAXED); }
static int live(int value) {
    sysctl_req req{&value,nullptr}; return nvrm_tgp_sysctl(&sysctl__debug_nvrm_tgp_enable,&gTgpEnable,0,&req);
}
static NVRMPower *create(nv_state_t *gpu) {
    auto *p=nvrm_power_create(); check(p!=nullptr,"eligible opted-in laptop creates a manager");
    nvrm_power_start(p,gpu); return p;
}
static void destroy(NVRMPower *p) {
    nvrm_power_stop(p); nvrm_power_release(p);
    check(!objects && !loops && !timers && !gTgpClaimed,"all manager and timer ownership is released");
}
int main() {
    OBJGPU hardware; nv_state_t gpu{&hardware};
    bootPresent=false; check(!nvrm_power_create() && !objects && !registrations,"absent boot argument is completely inert");
    bootPresent=true;
    for (int value : {0,2,-1}) { bootValue=value; check(!nvrm_power_create(),"only nvrmtgp=1 opts in"); }
    bootValue=1; battery=false;
    check(!nvrm_power_create() && !objects && !controls,"desktop is inert even with the boot argument"); battery=true;
    noPm=true; check(!nvrm_power_create() && !objects,"disabled system PM cannot start an undrainable poller"); noPm=false;
    for (int failure=0;failure<3;++failure) {
        failLoop=failure==0; failTimer=failure==1; failAttach=failure==2;
        check(!nvrm_power_create(),"optional allocation failure refuses the manager");
        check(!objects && !gTgpClaimed,"partial initialization unwinds every resource");
    }
    failLoop=failTimer=failAttach=false;
    auto *p=create(&gpu); auto *timer=lastTimer;
    check(timer->pending && timer->ms==2000 && !controls,"first control waits for an up adapter and initial delay");
    check(!nvrm_power_create(),"the legacy single-adapter telemetry has a single owner");
    timer->fire(); check(sent.back()==1 && read(&gTgpState)==1,"mains enables the firmware-configured budget");
    unsigned before=controls; timer->fire(); check(controls==before && timer->ms==5000,"unchanged mains does not repeat controls");
    ac=false; timer->fire(); check(sent.back()==0 && read(&gTgpState)==0,"unplugging resets the budget");
    ac=true; timer->fire(); battery=false; timer->fire();
    check(sent.back()==0 && read(&gTgpAc)==-1,"lost battery metadata resets rather than impersonating mains");
    battery=true; timer->fire(); check(read(&gTgpState)==1,"a valid charger reading can enable again");
    check(live(0)==0,"live rollback accepts zero"); timer->fire(); check(sent.back()==0,"live rollback resets on mains");
    check(live(3)==EINVAL && read(&gTgpEnable)==0,"invalid sysctl values cannot change policy");
    int attempted=1, observed=-9; sysctl_req ro{&attempted,nullptr}, rd{nullptr,&observed};
    check(nvrm_tgp_sysctl(&sysctl__debug_nvrm_tgp_state,&gTgpState,0,&ro)==EPERM && read(&gTgpState)==0,
          "telemetry cannot be written through the sysctl handler");
    check(!nvrm_tgp_sysctl(&sysctl__debug_nvrm_tgp_state,&gTgpState,0,&rd) && observed==0,
          "telemetry reads use the successful command state");
    check(live(1)==0,"live following can be restored"); timer->fire();
    std::thread settings([&] { for (int i=0;i<100;++i) { assert(!live(i%2)); int value; sysctl_req r{nullptr,&value};
        assert(!nvrm_tgp_sysctl(&sysctl__debug_nvrm_tgp_state,&gTgpState,0,&r)); } });
    std::thread polling([&] { for (int i=0;i<100;++i) timer->fire(); });
    settings.join(); polling.join(); live(1); timer->fire();
    check(read(&gTgpState)==1,"concurrent live settings and firmware-state reads remain coherent");
    nvrm_power_suspend(p); before=controls; timer->fire();
    check(!timer->pending && controls==before,"suspend prevents queued work and rearming");
    nvrm_power_resume(p); timer->fire(); check(controls==before+1 && sent.back()==1,"successful resume forgets firmware state and reapplies");
    hardware.mobile=false; nvrm_power_suspend(p); nvrm_power_resume(p); before=controls;
    for (int i=0;i<8;++i) timer->fire();
    check(controls==before && read(&gTgpLastStatus)==NV_ERR_NOT_SUPPORTED,"a non-mobile GPU cannot reach firmware");
    hardware.mobile=true; nvrm_power_suspend(p); nvrm_power_resume(p); hardware.fullPower=false;
    timer->fire(); check(controls==before && read(&gTgpLastStatus)==NV_ERR_GPU_NOT_FULL_POWER,"non-running hardware refuses the control");
    hardware.fullPower=true; apiStatus=NV_ERR_GENERIC; timer->fire(); gpuStatus=NV_ERR_GENERIC; apiStatus=NV_OK; timer->fire();
    check(!apiDepth && !gpuDepth && controls==before,"failed API/GPU lock admission unwinds without controls"); gpuStatus=NV_OK;
    nvrm_power_suspend(p); nvrm_power_resume(p); controlStatus=NV_ERR_GENERIC; before=controls;
    for (int i=0;i<8;++i) timer->fire();
    check(controls==before+NVRM_TGP_MAX_FAILS,"unsuccessful enable attempts are bounded");
    controlStatus=NV_OK; nvrm_power_suspend(p); nvrm_power_resume(p); timer->fire();
    ac=false; controlStatus=NV_ERR_GENERIC; before=controls;
    for (int i=0;i<7;++i) timer->fire();
    check(controls==before+7 && sent.back()==0,"failed disables never silence battery rollback");
    controlStatus=NV_OK; timer->fire(); check(read(&gTgpState)==0,"a later successful rollback clears failure state");
    ac=true; timer->fire();

    // Reserve a callback before it takes the gate, then invalidate it. Entry
    // counters cannot cover this scheduling window; timer generation does.
    Latch queued, proceed;
    std::thread late([&] { timer->fire(&queued,&proceed); }); queued.wait();
    nvrm_power_suspend(p); before=controls; proceed.signal(); late.join();
    check(controls==before && !timer->pending,"a callback queued before suspend is inert after the drain");
    nvrm_power_resume(p);

    for (bool stopping : {false,true}) {
        Latch entered, release, attempt;
        controlEntered=&entered; controlRelease=&release;
        std::thread poll([&] { timer->fire(); }); entered.wait();
        std::atomic<bool> finished{false}; actionAttempt=&attempt;
        std::thread lifecycle([&] { if (stopping) nvrm_power_stop(p); else nvrm_power_suspend(p); finished=true; });
        attempt.wait();
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        check(!finished.load(),"suspend/stop waits for the complete in-flight firmware call");
        release.signal(); poll.join(); lifecycle.join(); actionAttempt=nullptr; controlEntered=controlRelease=nullptr;
        check(finished && !timer->pending && !timer->enabled,"the drain cancels the poll's pending rearm");
        if (!stopping) { nvrm_power_resume(p); }
    }
    before=controls; nvrm_power_resume(p); nvrm_power_start(p,&gpu); timer->fire();
    check(controls==before && !timer->pending,"late resume/start cannot resurrect a stopped adapter");
    destroy(p);
    p=create(&gpu); nvrm_power_stop(p); before=controls; nvrm_power_start(p,&gpu);
    check(!lastTimer->pending && controls==before,"stop before adapter bring-up is terminal"); destroy(p);
    p=nvrm_power_create(); nvrm_power_suspend(p); nvrm_power_start(p,&gpu);
    check(!lastTimer->pending,"bring-up while paused cannot arm a timer");
    nvrm_power_resume(p); lastTimer->fire(); check(sent.back()==1,"resume can activate delayed bring-up"); destroy(p);
    nvrm_power_fini(); check(!registrations,"module stop unregisters the telemetry handlers");
    std::printf("PASS: %u production manager/control checks\n",checks);
}
'''.replace('@@HELPER@@', helper)

# Guard the adapter lifecycle wiring independently of the mock manager.
nvrm = (ROOT/'kexts/NVRM/NVRM.cpp').read_text()
start = nvrm[nvrm.index('bool NVRM::start('):nvrm.index('extern "C" int sysctlbyname')]
go = nvrm[nvrm.index('IOReturn NVRM::go('):nvrm.index('bool NVRM::interruptFilter(')]
stop = nvrm[nvrm.index('void NVRM::stop('):nvrm.index('void NVRM::free()')]
sleep = nvrm[nvrm.index('void NVRM::gpuSuspend()'):nvrm.index('void NVRM::gpuResume()')]
wake = nvrm[nvrm.index('void NVRM::gpuResume()'):nvrm.index('void NVRM::pciSaveForSleep()')]
assert start.index('nv_xnu_power_refresh();') < start.index('fPower = nvrm_power_create();') < start.index('kernel_thread_start(')
assert go.index('fAdapter = true;') < go.index('nvrm_power_start(fPower, &fNv);')
assert stop.index('nvrm_power_stop(fPower);') < stop.index('PMstop();') < stop.index('rm_shutdown_adapter(')
assert sleep.index('nvrm_power_suspend(fPower);') < sleep.index('nv_xnu_pm_close(') < sleep.index('NV_PM_ACTION_STANDBY')
assert wake.index('nv_xnu_pm_open();') < wake.index('if (ok) nvrm_power_resume(fPower);')

with tempfile.TemporaryDirectory(prefix='notebook-tgp-') as temporary:
    temporary = Path(temporary)
    variants = [('production', manager, fixture)]
    if args.sanitizer != 'thread':
        variants += [
            ('negative-mobile', manager, fixture.replace('if (!pGpu->getProperty(pGpu, PDB_PROP_GPU_IS_MOBILE))', 'if (false)')),
            ('negative-drain', manager, fixture.replace('std::lock_guard<std::recursive_mutex> lock(mutex);', '/* missing work-loop gate */')),
            ('negative-stop', manager.replace('p->fStopping = true;', '/* missing terminal stop */'), fixture),
        ]
    for name, production, test in variants:
        cpp=temporary/(name+'.cpp'); binary=temporary/name
        cpp.write_text(test.replace('@@MANAGER@@', production))
        subprocess.run(['xcrun','clang++','-std=c++17','-Wall','-Wextra','-Wno-unknown-pragmas',
                        '-Wno-unused-parameter','-Wno-unused-variable','-pthread',
                        '-fsanitize='+args.sanitizer,'-fno-omit-frame-pointer','-g',
                        '-I'+str(ROOT/'kexts/NVRM'),str(cpp),'-o',str(binary)],check=True)
        result=subprocess.run([str(binary)],capture_output=True,text=True,timeout=30)
        if name=='production':
            assert result.returncode==0,result.stdout+result.stderr
            print(result.stdout.strip())
        else:
            expected = {'negative-mobile':'a non-mobile GPU cannot reach firmware',
                        'negative-drain':'suspend/stop waits for the complete in-flight firmware call',
                        'negative-stop':'late resume/start cannot resurrect a stopped adapter'}[name]
            assert result.returncode!=0 and expected in result.stderr,result.stdout+result.stderr
            print('PASS: '+name+' detected')
