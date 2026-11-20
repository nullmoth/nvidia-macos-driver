/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// NVIDIA's open-gpu-kernel-modules headers (third-party) carry their own integer-conversion warnings under the kernel
// compiler's defaults; the exception is scoped to these includes and our code below stays checked.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wsign-conversion"
#pragma clang diagnostic ignored "-Wimplicit-int-conversion"
#pragma clang diagnostic ignored "-Wshorten-64-to-32"
#include <nv.h>
#include <nv-priv.h>
#include <os/os.h>
#include <osapi.h>
#include "gpu/gpu.h"
#include "kernel/gpu/bus/kern_bus.h"
#include "published/hopper/gh100/dev_vm.h"
#pragma clang diagnostic pop

int nvrm_bar1_console_state(nv_state_t *nv, NvU64 *pStaticStart);
int nvrm_bar1_block(nv_state_t *nv, NvU32 *pBlock);

/*
 * Where the verbose console may write VRAM through BAR1 while RM starts.
 *   0  RM has not begun its BAR1 setup: BAR1 still reaches VRAM the way the firmware left it, offset 0 = VRAM 0.
 *   1  RM is building its BAR1 address space: the window is about to change meaning, write nothing.
 *   2  RM's static BAR1 mapping exists: VRAM offset X is at BAR1 offset (*pStaticStart + X), once BAR1 is bound.
 * The fields are read without RM's locks. Each is a pointer or flag that RM sets once, in order, during kbusInitBar1
 * (address space first, then the static mapping, then the bind); a stale read can only report an earlier state.
 */
int nvrm_bar1_console_state(nv_state_t *nv, NvU64 *pStaticStart)
{
    if (pStaticStart) *pStaticStart = 0;
    if (nv == NULL) return 0;
    OBJGPU *pGpu = NV_GET_NV_PRIV_PGPU(nv);
    if (pGpu == NULL) return 0;
    KernelBus *pKernelBus = GPU_GET_KERNEL_BUS(pGpu);
    if (pKernelBus == NULL) return 0;
    if (pKernelBus->bar1[0].pVAS == NULL) return 0;
    if (!pKernelBus->bar1[0].bStaticBar1Enabled) return 1;
    if (pStaticStart) *pStaticStart = pKernelBus->bar1[0].staticBar1.startOffset;
    return 2;
}

/*
 * The BAR1 bind register (Hopper and later, BAR1_BLOCK_LOW_ADDR: MODE bit 9, physical 0 / virtual 1). RM building its
 * BAR1 address space does not change what BAR1 reaches; only the bind does, and RM binds after the static mapping is
 * made. Returns 0 when no GPU object exists yet, 1 with *pBlock read, -1 on an older chip (no such register here).
 */
int nvrm_bar1_block(nv_state_t *nv, NvU32 *pBlock)
{
    if (pBlock) *pBlock = 0;
    if (nv == NULL) return 0;
    OBJGPU *pGpu = NV_GET_NV_PRIV_PGPU(nv);
    if (pGpu == NULL) return 0;
    if (!IsGH100orBetter(pGpu)) return -1;
    if (pBlock) *pBlock = GPU_VREG_RD32(pGpu, NV_VIRTUAL_FUNCTION_PRIV_FUNC_BAR1_BLOCK_LOW_ADDR);
    return 1;
}
