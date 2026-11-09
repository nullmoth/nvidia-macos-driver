/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Sends the GPU firmware its platform power budget (TGP mode) for nvrm_power.cpp. Compiled with the RM's own command
 * line for osapi.c (rmcc.py) because it needs the GPU object. Only this one control is reachable from here: there is
 * deliberately no generic "issue any RM control" entry.
 *
 * The idea of lifting a notebook GPU to its platform limit through this internal control comes from the NvGfx project
 * (watchthelight-labs/NvGfx); this implementation is our own.
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
#include <core/thread_state.h>
#include <core/locks.h>
#include "rmapi/rmapi.h"
#include "ctrl/ctrl2080/ctrl2080internal.h"
#pragma clang diagnostic pop

NV_STATUS nvrm_pmgr_configure_tgp(nv_state_t *nv, NvU8 enable);

NV_STATUS nvrm_pmgr_configure_tgp(nv_state_t *nv, NvU8 enable)
{
    THREAD_STATE_NODE threadState;
    NV_STATUS st;
    void *fp;
    nvidia_stack_t *sp = NULL;
    OBJGPU *pGpu = nv ? NV_GET_NV_PRIV_PGPU(nv) : NULL;
    NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE_PARAMS params;

    if (pGpu == NULL) return NV_ERR_INVALID_ARGUMENT;
    params.bEnable = enable ? NV_TRUE : NV_FALSE;
    NV_ENTER_RM_RUNTIME(sp, fp);
    threadStateInit(&threadState, THREAD_STATE_FLAGS_NONE);
    /* Same order as every RM entry: the API lock, then the GPU locks (the GSP RPC path expects both held). */
    if ((st = rmapiLockAcquire(API_LOCK_FLAGS_NONE, RM_LOCK_MODULES_OSAPI)) == NV_OK) {
        if ((st = rmDeviceGpuLocksAcquire(pGpu, GPUS_LOCK_FLAGS_NONE, RM_LOCK_MODULES_OSAPI)) == NV_OK) {
            if (!FULL_GPU_SANITY_CHECK(pGpu)) {
                st = NV_ERR_GPU_NOT_FULL_POWER;
            } else {
                RM_API *pRmApi = GPU_GET_PHYSICAL_RMAPI(pGpu);
                st = pRmApi->Control(pRmApi, pGpu->hInternalClient, pGpu->hInternalSubdevice,
                                     NV2080_CTRL_CMD_INTERNAL_PMGR_PFM_REQ_HNDLR_CONFIGURE_TGP_MODE,
                                     &params, sizeof params);
            }
            rmDeviceGpuLocksRelease(pGpu, GPUS_LOCK_FLAGS_NONE, NULL);
        }
        rmapiLockRelease();
    }
    threadStateFree(&threadState, THREAD_STATE_FLAGS_NONE);
    NV_EXIT_RM_RUNTIME(sp, fp);
    return st;
}
