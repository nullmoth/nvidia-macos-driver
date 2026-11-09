/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Notebook power budget. A notebook GPU boots at the VBIOS default power limit; the laptop's real budget is applied by
 * telling the GPU firmware to enter TGP mode. With boot-arg nvrmtgp=1 this manager does that once the GPU is up, only
 * while on mains power, and follows the charger with a 5 s poll. Off by default: with the boot-arg absent nothing here
 * runs. The idea comes from the NvGfx project (watchthelight-labs/NvGfx); the code is ours.
 *
 *   debug.nvrm_tgp_enable       RW  1 follow the power source (default), 0 hold the VBIOS default (live rollback)
 *   debug.nvrm_tgp_state        RO  what the firmware was last told: 1 on, 0 off, -1 nothing yet
 *   debug.nvrm_tgp_ac           RO  last power source: 1 mains, 0 battery, -1 unknown
 *   debug.nvrm_tgp_last_status  RO  NV_STATUS of the last control
 *   debug.nvrm_tgp_applies      RO  controls sent
 */

#include "nv-xnu.h"
#include <kern/thread_call.h>
#include <kern/clock.h>
#include <sys/sysctl.h>
#include <IOKit/IOLib.h>
#include "nvrm_power_policy.h"

extern "C" void nv_xnu_power_refresh(void);                       // nv_xnu_power.cpp: re-reads the battery service
extern "C" NV_STATUS nvrm_pmgr_configure_tgp(nv_state_t *nv, NvU8 enable);   // nvrm_pmgr.c

#define NVRM_TGP_FIRST_MS  2000u
#define NVRM_TGP_PERIOD_MS 5000u

static int gTgpEnable = 1, gTgpState = -1, gTgpAc = -1, gTgpLastStatus = 0, gTgpApplies = 0;
SYSCTL_INT(_debug, OID_AUTO, nvrm_tgp_enable, CTLFLAG_RW | CTLFLAG_LOCKED, &gTgpEnable, 0,
           "1 = TGP mode on while on mains power, 0 = hold the VBIOS default power limit");
SYSCTL_INT(_debug, OID_AUTO, nvrm_tgp_state, CTLFLAG_RD | CTLFLAG_LOCKED, &gTgpState, 0, "TGP mode last sent: 1 on, 0 off, -1 none");
SYSCTL_INT(_debug, OID_AUTO, nvrm_tgp_ac, CTLFLAG_RD | CTLFLAG_LOCKED, &gTgpAc, 0, "last power source: 1 mains, 0 battery, -1 unknown");
SYSCTL_INT(_debug, OID_AUTO, nvrm_tgp_last_status, CTLFLAG_RD | CTLFLAG_LOCKED, &gTgpLastStatus, 0, "NV_STATUS of the last TGP control");
SYSCTL_INT(_debug, OID_AUTO, nvrm_tgp_applies, CTLFLAG_RD | CTLFLAG_LOCKED, &gTgpApplies, 0, "TGP mode controls sent");

static thread_call_t gTgpCall;
static nv_state_t *gTgpNv;
static volatile SInt32 gTgpStopping, gTgpRunning, gTgpPaused;
static unsigned gTgpFails;

static void nvrm_tgp_arm(uint32_t ms)
{
    if (!gTgpCall || gTgpStopping) return;
    uint64_t dl;
    clock_interval_to_deadline(ms, kMillisecondScale, &dl);
    thread_call_enter_delayed(gTgpCall, dl);
}

static void nvrm_tgp_tick_body(void)
{
    // The answer nv_acpi_get_powersource gives is the one the last registry walk found, refreshed only at start and on
    // wake. This runs on our own thread with no firmware lock held, so walk first or a removed charger goes unseen.
    nv_xnu_power_refresh();
    NvU32 plugged = 0;
    gTgpAc = nv_acpi_get_powersource(&plugged) == NV_OK ? (plugged ? 1 : 0) : -1;
    const int want = nvrm_tgp_want(gTgpEnable, gTgpAc, gTgpState);
    if (want >= 0) {
        NV_STATUS st = nvrm_pmgr_configure_tgp(gTgpNv, (NvU8)want);
        gTgpLastStatus = (int)st;
        gTgpApplies++;
        if (st == NV_OK) {
            gTgpState = want;
            gTgpFails = 0;
            kprintf("NVRM-xnu: TGP mode %s (%s)\n", want ? "on" : "off", gTgpAc == 1 ? "mains" : "battery");
        } else if (nvrm_tgp_gave_up(++gTgpFails)) {
            kprintf("NVRM-xnu: TGP mode %s refused %u times (last 0x%x), giving up until the next resume\n", want ? "on" : "off", gTgpFails, st);
            return;
        }
    }
    nvrm_tgp_arm(NVRM_TGP_PERIOD_MS);
}

static void nvrm_tgp_tick(thread_call_param_t, thread_call_param_t)
{
    OSIncrementAtomic(&gTgpRunning);
    if (!gTgpStopping && !gTgpPaused && gTgpNv) nvrm_tgp_tick_body();
    OSDecrementAtomic(&gTgpRunning);
}

extern "C" void nvrm_power_start(nv_state_t *nv)
{
    int v = 0;
    if (!PE_parse_boot_argn("nvrmtgp", &v, sizeof(v)) || !v) return;
    if (gTgpCall || !nv) return;
    static bool sysctlsUp;
    if (!sysctlsUp) {
        sysctlsUp = true;
        sysctl_register_oid(&sysctl__debug_nvrm_tgp_enable); sysctl_register_oid(&sysctl__debug_nvrm_tgp_state);
        sysctl_register_oid(&sysctl__debug_nvrm_tgp_ac); sysctl_register_oid(&sysctl__debug_nvrm_tgp_last_status);
        sysctl_register_oid(&sysctl__debug_nvrm_tgp_applies);
    }
    gTgpNv = nv;
    gTgpStopping = 0; gTgpPaused = 0; gTgpFails = 0; gTgpState = -1;
    gTgpCall = thread_call_allocate(nvrm_tgp_tick, NULL);
    if (!gTgpCall) { kprintf("NVRM-xnu: TGP: no thread call\n"); return; }
    kprintf("NVRM-xnu: TGP mode manager armed (nvrmtgp=1), first check in %u ms\n", NVRM_TGP_FIRST_MS);
    nvrm_tgp_arm(NVRM_TGP_FIRST_MS);
}

// A suspended GPU takes no control and the firmware forgets TGP mode across the power cycle.
extern "C" void nvrm_power_suspend(void)
{
    if (!gTgpCall) return;
    OSCompareAndSwap(0, 1, &gTgpPaused);
    thread_call_cancel(gTgpCall);
    for (int i = 0; i < 1000 && gTgpRunning; i++) IOSleep(10);
}
extern "C" void nvrm_power_resume(void)
{
    if (!gTgpCall) return;
    gTgpState = nvrm_tgp_state_after_resume(); gTgpFails = 0;
    OSCompareAndSwap(1, 0, &gTgpPaused);
    nvrm_tgp_arm(NVRM_TGP_FIRST_MS);
}

extern "C" void nvrm_power_stop(void)
{
    if (!gTgpCall) return;
    OSCompareAndSwap(0, 1, &gTgpStopping);
    // thread_call_cancel_wait is not exported to kexts: cancel, wait for a running tick (it never re-arms once
    // gTgpStopping is set), then free (thread_call_free fails while the call is still pending).
    thread_call_cancel(gTgpCall);
    for (int i = 0; i < 1000 && gTgpRunning; i++) IOSleep(10);
    thread_call_cancel(gTgpCall);
    for (int i = 0; i < 100 && !thread_call_free(gTgpCall); i++) { IOSleep(10); thread_call_cancel(gTgpCall); }
    gTgpCall = NULL;
    gTgpNv = NULL;
}
