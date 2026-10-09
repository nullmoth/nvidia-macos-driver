/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>
#include <IOKit/pci/IOPCIDevice.h>
#include <libkern/libkern.h>

extern "C" {
enum UvmPmaGpuMemoryType_tag {};
#define NVRM 1
#include "cpuopsys.h"
#include "nv.h"
#include "os-interface.h"
#include "nv-kernel-rmapi-ops.h"
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
void *kern_os_malloc(size_t size);
void  kern_os_free(void *addr);
}
