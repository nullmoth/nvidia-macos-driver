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
