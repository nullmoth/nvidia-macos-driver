/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Notebook TGP mode, adapted from upstream dfe54e0329308a8acd8de5e1246354454fcb557a.
 * The firmware control was identified by watchthelight-labs/NvGfx. It enables
 * the platform-configured budget; it accepts no wattage from this driver.
 *
 * Opt-in only (nvrmtgp=1), with a battery service and a mobile GPU. A dedicated
 * IOWorkLoop serializes polling, suspend, resume and stop. Disabling the timer
 * under that gate drains an entered callback and invalidates a queued callback
 * before RM can suspend or release the GPU. No callback-entry counter, bounded
 * drain or raw thread-call ownership is needed. The owner survives until free().
 */

#include "nv-xnu.h"
#include "nvrm_power.h"
#include "nvrm_power_policy.h"
#include <IOKit/IOWorkLoop.h>
#include <IOKit/IOTimerEventSource.h>
#include <pexpert/pexpert.h>
#include <sys/sysctl.h>
#include <sys/errno.h>

extern "C" void nv_xnu_power_refresh(void);
extern "C" void nv_xnu_power_state(bool *acPlugged, bool *haveBattery);
extern "C" NV_STATUS nvrm_pmgr_configure_tgp(nv_state_t *, NvU8 enable);

static const unsigned kTgpFirstMs = 2000, kTgpPeriodMs = 5000;
// The existing nvmtl-smi ABI reports the first opted-in notebook adapter.
static volatile UInt32 gTgpClaimed;
static int gTgpEnable = 1, gTgpState = -1, gTgpAc = -1, gTgpLastStatus, gTgpApplies;
static bool gTgpSysctlsUp;

static int nvrm_tgp_sysctl(struct sysctl_oid *oidp, void *arg1, int, struct sysctl_req *req)
{
    int *value = static_cast<int *>(arg1);
    int local = __atomic_load_n(value, __ATOMIC_RELAXED);
    int error = sysctl_handle_int(oidp, &local, 0, req);
    if (error || !req->newptr) return error;
    if (value != &gTgpEnable) return EPERM;
    if (local != 0 && local != 1) return EINVAL;
    __atomic_store_n(value, local, __ATOMIC_RELAXED);
    return 0;
}
#define TGP_SYSCTL(name, flags, value, description) \
    SYSCTL_PROC(_debug, OID_AUTO, name, CTLTYPE_INT | (flags) | CTLFLAG_LOCKED, \
                &(value), 0, nvrm_tgp_sysctl, "I", description)
TGP_SYSCTL(nvrm_tgp_enable, CTLFLAG_RW, gTgpEnable, "1 = follow mains power, 0 = reset TGP mode");
TGP_SYSCTL(nvrm_tgp_state, CTLFLAG_RD, gTgpState, "last successful TGP control: 1 on, 0 off, -1 none");
TGP_SYSCTL(nvrm_tgp_ac, CTLFLAG_RD, gTgpAc, "last notebook power source: 1 mains, 0 battery, -1 unknown");
TGP_SYSCTL(nvrm_tgp_last_status, CTLFLAG_RD, gTgpLastStatus, "NV_STATUS of the last TGP control");
TGP_SYSCTL(nvrm_tgp_applies, CTLFLAG_RD, gTgpApplies, "number of attempted TGP controls");
#undef TGP_SYSCTL
static struct sysctl_oid *const kTgpOids[] = {
    &sysctl__debug_nvrm_tgp_enable, &sysctl__debug_nvrm_tgp_state,
    &sysctl__debug_nvrm_tgp_ac, &sysctl__debug_nvrm_tgp_last_status, &sysctl__debug_nvrm_tgp_applies
};

class NVRMPower : public OSObject {
    OSDeclareDefaultStructors(NVRMPower)
public:
    bool init() override;
    void free() override;
    void start(nv_state_t *nv);
    void suspend();
    void resume();
    void stop();
private:
    IOWorkLoop *fLoop = nullptr;
    IOTimerEventSource *fTimer = nullptr;
    nv_state_t *fNv = nullptr;
    bool fAttached = false, fPaused = false, fStopping = false;
    unsigned fFails = 0;
    static void tick(OSObject *owner, IOTimerEventSource *timer);
    enum Change { kStart, kSuspend, kResume, kStop };
    void change(Change operation, nv_state_t *nv = nullptr);
    static IOReturn transition(OSObject *, void *, void *, void *, void *);
    void arm(unsigned ms);  // all state below is protected by fLoop's gate
    void poll();
    void apply(int want);
};
OSDefineMetaClassAndStructors(NVRMPower, OSObject)

bool NVRMPower::init()
{
    if (!OSObject::init()) return false;
    fLoop = IOWorkLoop::workLoop();
    // Execute on this work loop, rather than blocking a shared high-priority
    // thread-call worker on an RM firmware operation.
    fTimer = IOTimerEventSource::timerEventSource(
        kIOTimerEventSourceOptionsPriorityWorkLoop, this, &NVRMPower::tick);
    if (!fLoop || !fTimer) return false;
    fTimer->disable();
    fAttached = fLoop->addEventSource(fTimer) == kIOReturnSuccess;
    return fAttached;
}

void NVRMPower::arm(unsigned ms)
{
    if (fAttached && fNv && !fPaused && !fStopping) fTimer->setTimeoutMS(ms);
}

void NVRMPower::start(nv_state_t *nv)
{
    change(kStart, nv);
}

void NVRMPower::apply(int want)
{
    NV_STATUS status = nvrm_pmgr_configure_tgp(fNv, static_cast<NvU8>(want));
    __atomic_store_n(&gTgpLastStatus, static_cast<int>(status), __ATOMIC_RELAXED);
    __atomic_fetch_add(&gTgpApplies, 1, __ATOMIC_RELAXED);
    if (status == NV_OK) {
        __atomic_store_n(&gTgpState, want, __ATOMIC_RELAXED);
        fFails = 0;
        kprintf("NVRM-xnu: notebook TGP mode %s\n", want ? "on" : "off");
    } else {
        if (fFails < NVRM_TGP_MAX_FAILS && ++fFails == NVRM_TGP_MAX_FAILS)
            kprintf("NVRM-xnu: notebook TGP control refused (0x%x); further enables held until resume\n", status);
    }
}

void NVRMPower::poll()
{
    // Registry walks must never run under RM's API/GPU locks. A battery service
    // that disappears is unknown here, never evidence that a laptop is on AC.
    nv_xnu_power_refresh();
    bool ac = false, battery = false;
    nv_xnu_power_state(&ac, &battery);
    const int source = battery ? (ac ? 1 : 0) : -1;
    __atomic_store_n(&gTgpAc, source, __ATOMIC_RELAXED);
    const int state = __atomic_load_n(&gTgpState, __ATOMIC_RELAXED);
    const int want = nvrm_tgp_want(__atomic_load_n(&gTgpEnable, __ATOMIC_RELAXED), source, state);
    // Bound unsuccessful enables, but keep checking for charger removal and
    // live rollback. An applied budget must still be reset after a failure.
    if (want >= 0 && (!nvrm_tgp_gave_up(fFails) || (want == 0 && state == 1))) apply(want);
    arm(kTgpPeriodMs);
}

void NVRMPower::tick(OSObject *owner, IOTimerEventSource *)
{
    NVRMPower *power = static_cast<NVRMPower *>(owner);
    if (power->fNv && !power->fPaused && !power->fStopping) power->poll();
}

void NVRMPower::suspend()
{
    change(kSuspend);
}

void NVRMPower::resume()
{
    change(kResume);
}

void NVRMPower::stop()
{
    change(kStop);
}

void NVRMPower::change(Change operation, nv_state_t *nv)
{
    if (fLoop && fTimer)
        fLoop->runAction(&NVRMPower::transition, this,
                         reinterpret_cast<void *>(static_cast<uintptr_t>(operation)), nv);
}

IOReturn NVRMPower::transition(OSObject *owner, void *operation, void *gpu, void *, void *)
{
    NVRMPower *p = static_cast<NVRMPower *>(owner);
    // runAction waits for the entire poll, including its re-arm, and shares
    // the timer callback's gate. Lifecycle calls never hold RM's PM gate here.
    switch (static_cast<Change>(reinterpret_cast<uintptr_t>(operation))) {
    case kStart:
        if (!p->fStopping && !p->fNv && gpu) {
            p->fNv = static_cast<nv_state_t *>(gpu);
            if (!p->fPaused) { p->fTimer->enable(); p->arm(kTgpFirstMs); }
        }
        break;
    case kResume:
        if (p->fStopping) break;
        // Called only after successful RM/NVKMS resume and PM gate reopening.
        __atomic_store_n(&gTgpState, nvrm_tgp_state_after_resume(), __ATOMIC_RELAXED);
        p->fFails = 0;
        p->fPaused = false;
        p->fTimer->enable();
        p->arm(kTgpFirstMs);
        break;
    case kSuspend:
    case kStop:
        p->fTimer->disable();
        p->fTimer->cancelTimeout();
        if (static_cast<Change>(reinterpret_cast<uintptr_t>(operation)) == kStop) {
            p->fStopping = true;
            if (p->fNv && !p->fPaused && __atomic_load_n(&gTgpState, __ATOMIC_RELAXED) == 1) p->apply(0);
            p->fNv = nullptr;  // no callback can access RM after this drain
        }
        p->fPaused = true;
        break;
    }
    return kIOReturnSuccess;
}

void NVRMPower::free()
{
    stop();
    if (fAttached) { fLoop->removeEventSource(fTimer); fAttached = false; }
    if (fTimer) { fTimer->release(); fTimer = nullptr; }
    if (fLoop) { fLoop->release(); fLoop = nullptr; }
    OSCompareAndSwap(1, 0, &gTgpClaimed);
    OSObject::free();
}

NVRMPower *nvrm_power_create(void)
{
    int enabled = 0;
    if (!PE_parse_boot_argn("nvrmtgp", &enabled, sizeof enabled) || enabled != 1) return nullptr;
    char noPm[8] = {};
    if (PE_parse_boot_argn("-nvrmnopm", noPm, sizeof noPm)) return nullptr;
    bool battery = false;
    nv_xnu_power_state(nullptr, &battery);
    if (!battery || !OSCompareAndSwap(0, 1, &gTgpClaimed)) return nullptr;
    NVRMPower *power = new NVRMPower;
    if (!power) { OSCompareAndSwap(1, 0, &gTgpClaimed); return nullptr; }
    if (!power->init()) { power->release(); return nullptr; }
    __atomic_store_n(&gTgpState, -1, __ATOMIC_RELAXED);
    __atomic_store_n(&gTgpAc, -1, __ATOMIC_RELAXED);
    __atomic_store_n(&gTgpLastStatus, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&gTgpApplies, 0, __ATOMIC_RELAXED);
    if (!gTgpSysctlsUp) {
        for (auto oid : kTgpOids) sysctl_register_oid(oid);
        gTgpSysctlsUp = true;
    }
    kprintf("NVRM-xnu: optional notebook TGP manager ready (nvrmtgp=1)\n");
    return power;
}

void nvrm_power_start(NVRMPower *power, nv_state_t *nv) { if (power) power->start(nv); }
void nvrm_power_suspend(NVRMPower *power) { if (power) power->suspend(); }
void nvrm_power_resume(NVRMPower *power) { if (power) power->resume(); }
void nvrm_power_stop(NVRMPower *power) { if (power) power->stop(); }
void nvrm_power_release(NVRMPower *power) { if (power) power->release(); }
void nvrm_power_fini(void)
{
    if (gTgpSysctlsUp) {
        for (auto oid : kTgpOids) sysctl_unregister_oid(oid);
        gTgpSysctlsUp = false;
    }
}
