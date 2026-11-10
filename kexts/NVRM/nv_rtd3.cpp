/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Opt-in D3cold for a discrete laptop GPU: evaluates the platform power resource's _OFF and _ON methods through the
 * GPU's ACPI node (nv_rtd3_policy.h has the rules). Nothing here runs unless the boot argument nvrtd3=1 is set, and the
 * power resource's ACPI path comes from the boot argument string nvrtd3res (for example \_SB.PCI0.PEG1.PG01), because
 * the _PR3 package's references are not readable through the registry on every machine.
 *
 * The callers (the driver's idle and wake paths, owned by the power-management code) ask nv_rtd3_may_off first.
 */
#include <IOKit/IOService.h>
#include <IOKit/IOLib.h>
#include <IOKit/acpi/IOACPIPlatformDevice.h>
#include <pexpert/pexpert.h>
#include <string.h>
#include "nv_rtd3_policy.h"

static struct nv_rtd3 g_rtd3;
static char g_res[96];
static int g_init;

static void nv_rtd3_init(void)
{
    if (g_init) return;
    g_init = 1;
    uint32_t v = 0;
    g_rtd3.enabled = PE_parse_boot_argn("nvrtd3", &v, sizeof v) && v == 1;
    g_rtd3.have_resource = PE_parse_boot_argn("nvrtd3res", g_res, sizeof g_res) && g_res[0] == '\\';
    if (g_rtd3.enabled)
        kprintf("NVRM-xnu: RTD3 %s%s\n", g_rtd3.have_resource ? "resource " : "needs nvrtd3res=<ACPI path> to act", g_rtd3.have_resource ? g_res : "");
}

static bool evalMethod(IOACPIPlatformDevice *acpi, const char *method)
{
    // The firmware's power-resource methods are called only with nvrtd3=1 on the boot line, read here as well as by the
    // policy above: a method of this kind behaves differently on every board.
    { uint32_t on = 0; if (!PE_parse_boot_argn("nvrtd3", &on, sizeof on) || on != 1) return false; }
    char name[128];
    snprintf(name, sizeof name, "%s.%s", g_res, method);
    OSObject *o = nullptr;
    IOReturn r = acpi->evaluateObject(name, &o);
    OSSafeReleaseNULL(o);
    kprintf("NVRM-xnu: RTD3 %s -> 0x%x\n", name, r);
    return r == kIOReturnSuccess;
}

extern "C" int nv_rtd3_may_off(int rm_suspended, int heads_lit, int clients, unsigned idle_s)
{
    nv_rtd3_init();
    return nv_rtd3_may_power_off(&g_rtd3, rm_suspended, heads_lit, clients, idle_s);
}

extern "C" int nv_rtd3_reachable(void) { nv_rtd3_init(); return nv_rtd3_gpu_reachable(&g_rtd3); }

extern "C" int nv_rtd3_power_off(IOACPIPlatformDevice *acpi)
{
    nv_rtd3_init();
    if (!acpi || !g_rtd3.enabled || !g_rtd3.have_resource || g_rtd3.state != NV_RTD3_ON) return 0;
    bool ok = evalMethod(acpi, "_OFF");
    nv_rtd3_off_done(&g_rtd3, ok);
    return ok;
}

extern "C" int nv_rtd3_power_on(IOACPIPlatformDevice *acpi)
{
    nv_rtd3_init();
    if (!acpi || g_rtd3.state != NV_RTD3_OFF) return 1;
    bool ok = evalMethod(acpi, "_ON");
    nv_rtd3_on_done(&g_rtd3, ok);
    return ok;
}

/* ---- the monitor: a thread call that polls once a second while the GPU is on and four times a second while it is off,
 * so the first client or framebuffer after power-off wakes it promptly (calls that arrive meanwhile wait at the RM gate
 * the suspend closed). Armed by the driver once the adapter is up; does nothing unless nvrtd3=1. */
#include <kern/thread_call.h>
static thread_call_t g_call;
static struct nv_rtd3_ops g_ops;
static unsigned g_idle;
static volatile int g_stop;

static void nv_rtd3_poll(thread_call_param_t, thread_call_param_t)
{
    if (g_stop) return;
    const unsigned period_ms = g_rtd3.state == NV_RTD3_ON ? 1000 : 250;
    int r = nv_rtd3_tick(&g_rtd3, &g_ops, &g_idle, 1);
    if (r == NV_RTD3_TICK_POWERED_OFF) kprintf("NVRM-xnu: RTD3 GPU powered off after %u s idle\n", NV_RTD3_IDLE_SECONDS);
    else if (r == NV_RTD3_TICK_OFF_FAILED) kprintf("NVRM-xnu: RTD3 power-off failed, GPU resumed and left on\n");
    else if (r == NV_RTD3_TICK_WOKE) kprintf("NVRM-xnu: RTD3 GPU powered on for a client\n");
    else if (r == NV_RTD3_TICK_WAKE_FAILED) kprintf("NVRM-xnu: RTD3 power-on FAILED, the GPU stays unreachable\n");
    uint64_t dl; clock_interval_to_deadline(period_ms, kMillisecondScale, &dl);
    if (!g_stop) thread_call_enter_delayed(g_call, dl);
}

extern "C" void nv_rtd3_arm(const struct nv_rtd3_ops *ops)
{
    nv_rtd3_init();
    if (!g_rtd3.enabled || !g_rtd3.have_resource || g_call) return;
    g_ops = *ops;
    g_call = thread_call_allocate(nv_rtd3_poll, nullptr);
    if (!g_call) return;
    uint64_t dl; clock_interval_to_deadline(1000, kMillisecondScale, &dl);
    thread_call_enter_delayed(g_call, dl);
    kprintf("NVRM-xnu: RTD3 armed (idle %u s, resource %s)\n", NV_RTD3_IDLE_SECONDS, g_res);
}

extern "C" void nv_rtd3_disarm(void)
{
    g_stop = 1;
    // thread_call_cancel_wait is not exported to kexts (a kext that names it cannot be linked into the kernel collection):
    // cancel, and retry the free until a tick that was already running is out. The tick never re-arms once g_stop is set.
    if (g_call) {
        thread_call_cancel(g_call);
        for (int i = 0; i < 100 && !thread_call_free(g_call); i++) { IOSleep(10); thread_call_cancel(g_call); }
        g_call = nullptr;
    }
}

/* System wake: the resource must be on before the driver's own resume touches the GPU. */
extern "C" int nv_rtd3_before_system_resume(void)
{
    if (g_rtd3.state != NV_RTD3_OFF) return 1;
    int ok = g_ops.acpi_on ? g_ops.acpi_on(g_ops.ctx) : 0;
    nv_rtd3_on_done(&g_rtd3, ok);
    g_idle = 0;
    return ok;
}
