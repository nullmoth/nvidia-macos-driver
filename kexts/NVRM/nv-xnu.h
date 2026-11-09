/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
// IOPCIDevice (with its setBusMasterEnable) is marked deprecated in favour of PCIDriverKit, which runs in user space and
// cannot replace a kernel extension's PCI access; that deprecation is ignored from here on in this kext's sources.
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/libkern.h>

extern "C" {
enum UvmPmaGpuMemoryType_tag {};
#define NVRM 1
// NVIDIA's open-gpu-kernel-modules headers (third-party) carry their own integer-conversion warnings under the kernel
// compiler's defaults; the exception is scoped to these includes and our code below stays checked.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wsign-conversion"
#pragma clang diagnostic ignored "-Wimplicit-int-conversion"
#pragma clang diagnostic ignored "-Wshorten-64-to-32"
#pragma clang diagnostic ignored "-Wc99-designator"
#include "cpuopsys.h"
#include "nv.h"
#include "os-interface.h"
#include "nv-kernel-rmapi-ops.h"
#pragma clang diagnostic pop
}
#undef NVRM

#define NV_XNU_STUB_HIT(name) do { static bool _hit = false; if (!_hit) { _hit = true; kprintf("NVRM-xnu: STUB %s\n", (name)); } } while (0)

struct nv_xnu_pci_slot {
    IOPCIDevice *pci;
    NvU32 domain; NvU8 bus, slot, function;
};
extern "C" {
void nv_xnu_register_pci(IOPCIDevice *pci, NvU32 domain, NvU8 bus, NvU8 slot, NvU8 function);
void nv_xnu_register_all_pci(void);
NvBool nvkms_xnu_backlight(NvBool set, NvU32 *percent);   // os-nvkms-xnu.cpp: laptop panel brightness
void *kern_os_malloc(size_t size);
void  kern_os_free(void *addr);
// System sleep gate (NVRM.cpp), the nv_system_pm_lock of nv.c: macOS does not freeze processes for sleep, so every
// call into RM made for a client waits here while the GPU is in standby. Returns whether the caller was counted.
int  nv_xnu_pm_enter(void);
void nv_xnu_pm_exit(int counted);
}
struct NvXnuPmUse {
    int counted;
    explicit NvXnuPmUse(bool use = true) : counted(use ? nv_xnu_pm_enter() : 0) {}
    ~NvXnuPmUse() { nv_xnu_pm_exit(counted); }
};
