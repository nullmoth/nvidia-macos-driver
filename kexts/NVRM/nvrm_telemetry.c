/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <nv.h>
#include <nv-priv.h>
#include <os/os.h>
#include <osapi.h>
#include <nvport/nvport.h>

#include "gpu/gpu.h"
#include "gpu/gpu_user_shared_data.h"
#include "gpu/gpu_shared_data_map.h"
#include "core/thread_state.h"
#include "core/locks.h"
#include "rmapi/rmapi.h"
#include "rmapi/rmapi_utils.h"
#include "class/cl00de.h"
#include "ctrl/ctrl00de.h"

#include "nvrm_telemetry.h"
#include "nvrm_telemetry_decode.h"

typedef char NVRMTelemetryFanCountMatchesRUSD[
    (NVRM_TELEMETRY_FAN_COUNT == RUSD_FAN_COOLER_MAX_COOLERS) ? 1 : -1];

typedef struct NVRMTelemetryState {
    OBJGPU *pGpu;
    NvHandle hClient;
    NvHandle hDevice;
    NvHandle hSubdevice;
    NvHandle hRusdObject;
    NvU64 pmGeneration;
    NvU32 knownFanMask;
    NvBool pollingIntervalConfigured;
} NVRMTelemetryState;

static NV_STATUS
_nvrmTelemetryFreeRusdObject(NVRMTelemetryState *state)
{
    RM_API *pRmApi;
    NV_STATUS status;

    if (state->hRusdObject == 0)
        return NV_OK;

    pRmApi = rmapiGetInterface(RMAPI_GPU_LOCK_INTERNAL);
    if (pRmApi == NULL)
        return NV_ERR_INVALID_STATE;

    status = pRmApi->Free(pRmApi, state->hClient, state->hRusdObject);
    if (status == NV_OK)
    {
        state->hRusdObject = 0;
        state->pollingIntervalConfigured = NV_FALSE;
    }
    return status;
}

static NV_STATUS
_nvrmTelemetryEnsureRusdObject(OBJGPU *pGpu, NVRMTelemetryState *state)
{
    NV00DE_ALLOC_PARAMETERS params;
    RM_API *pRmApi;
    NV_STATUS status;

    pRmApi = rmapiGetInterface(RMAPI_GPU_LOCK_INTERNAL);
    if (pRmApi == NULL)
        return NV_ERR_INVALID_STATE;

    if (state->hClient == 0)
    {
        status = rmapiutilAllocClientAndDeviceHandles(pRmApi, pGpu,
                                                       &state->hClient,
                                                       &state->hDevice,
                                                       &state->hSubdevice);
        if (status != NV_OK)
            return status;
    }

    if (state->hRusdObject == 0)
    {
        portMemSet(&params, 0, sizeof(params));
        params.polledDataMask = NV00DE_RUSD_POLL_CLOCK |
                                NV00DE_RUSD_POLL_POWER |
                                NV00DE_RUSD_POLL_FAN;

        // NV00DE's RM object contributes this request to the driver's
        // per-client mask union. Freeing it removes only our own request.
        status = pRmApi->Alloc(pRmApi, state->hClient,
                               state->hSubdevice, &state->hRusdObject,
                               RM_USER_SHARED_DATA, &params, sizeof(params));
        if (status != NV_OK)
            return status;
    }

    if (!state->pollingIntervalConfigured)
    {
        NV00DE_CTRL_REQUEST_POLL_INTERVAL_PARAM interval = { 0 };
        interval.pollingIntervalMs = 500;
        status = pRmApi->Control(pRmApi, state->hClient,
                                 state->hRusdObject,
                                 NV00DE_CTRL_CMD_REQUEST_POLL_INTERVAL,
                                 &interval, sizeof(interval));
        if (status == NV_OK)
            state->pollingIntervalConfigured = NV_TRUE;
        return status;
    }

    return NV_OK;
}

static NvBool
_nvrmTelemetryReadPower(NV00DE_SHARED_DATA *pSharedData,
                        struct NVRMTelemetry *sample)
{
    RUSD_AVG_POWER_USAGE avg = { 0 };
    RUSD_INST_POWER_USAGE instant = { 0 };
    NvBool valid;
    NvBool any = NV_FALSE;

    valid = RUSD_READ_DATA(pSharedData, avgPowerUsage, &avg) &&
            avg.lastModifiedTimestamp < RUSD_SEQ_START &&
            RUSD_SEQ_DATA_VALID(avg.lastModifiedTimestamp);
    if (nvrmTelemetryDecodeU32(valid, avg.lastModifiedTimestamp,
                               avg.info.averageGpuPower,
                               &sample->averagePowerMilliwatts,
                               &sample->averagePowerTimestamp))
    {
        sample->validMask |= NVRM_TELEMETRY_VALID_AVERAGE_POWER;
        any = NV_TRUE;
    }

    valid = RUSD_READ_DATA(pSharedData, instPowerUsage, &instant) &&
            instant.lastModifiedTimestamp < RUSD_SEQ_START &&
            RUSD_SEQ_DATA_VALID(instant.lastModifiedTimestamp);
    if (nvrmTelemetryDecodeU32(valid, instant.lastModifiedTimestamp,
                               instant.info.instGpuPower,
                               &sample->instantPowerMilliwatts,
                               &sample->instantPowerTimestamp))
    {
        sample->validMask |= NVRM_TELEMETRY_VALID_INSTANT_POWER;
        any = NV_TRUE;
    }

    return any;
}

static NvBool
_nvrmTelemetryReadClocks(NV00DE_SHARED_DATA *pSharedData,
                         struct NVRMTelemetry *sample)
{
    RUSD_CLK_PUBLIC_DOMAIN_INFOS clocks = { 0 };
    NvBool valid;

    valid = RUSD_READ_DATA(pSharedData, clkPublicDomainInfos, &clocks) &&
            clocks.lastModifiedTimestamp < RUSD_SEQ_START &&
            RUSD_SEQ_DATA_VALID(clocks.lastModifiedTimestamp);
    if (!nvrmTelemetryDecodeClockPair(
            valid, clocks.lastModifiedTimestamp,
            clocks.info[RUSD_CLK_PUBLIC_DOMAIN_GRAPHICS].actualClkKHz,
            clocks.info[RUSD_CLK_PUBLIC_DOMAIN_MEMORY].actualClkKHz,
            &sample->coreClockKHz, &sample->memoryClockKHz,
            &sample->clocksTimestamp))
        return NV_FALSE;

    sample->validMask |= NVRM_TELEMETRY_VALID_CLOCKS;
    return NV_TRUE;
}

static NvBool
_nvrmTelemetryReadFans(NV00DE_SHARED_DATA *pSharedData,
                       NVRMTelemetryState *state,
                       struct NVRMTelemetry *sample)
{
    RUSD_FAN_COOLER_STATUS fans = { 0 };
    NvBool valid;

    valid = RUSD_READ_DATA(pSharedData, fanCoolerStatus, &fans) &&
            fans.lastModifiedTimestamp < RUSD_SEQ_START &&
            RUSD_SEQ_DATA_VALID(fans.lastModifiedTimestamp);
    if (!nvrmTelemetryDecodeFans(valid, fans.lastModifiedTimestamp,
                                 fans.info.rpmCurr, &state->knownFanMask,
                                 sample->fanRPM, &sample->fanValidMask,
                                 &sample->fansTimestamp))
        return NV_FALSE;

    sample->validMask |= NVRM_TELEMETRY_VALID_FANS;
    return NV_TRUE;
}

NV_STATUS
nvrm_query_telemetry(nv_state_t *nv, NvU64 pmGeneration,
                     void **opaqueState, struct NVRMTelemetry *sample)
{
    NVRMTelemetryState *state;
    NV00DE_SHARED_DATA *pSharedData;
    OBJGPU *pGpu;
    THREAD_STATE_NODE threadState;
    NV_STATUS status = NV_ERR_INVALID_ARGUMENT;
    void *fp;
    nvidia_stack_t *sp = NULL;

    if (sample == NULL || opaqueState == NULL)
        return NV_ERR_INVALID_ARGUMENT;

    portMemSet(sample, 0, sizeof(*sample));
    sample->version = NVRM_TELEMETRY_ABI_VERSION;
    sample->pmGeneration = pmGeneration;

    if (nv == NULL)
        return NV_ERR_INVALID_ARGUMENT;
    pGpu = NV_GET_NV_PRIV_PGPU(nv);
    if (pGpu == NULL)
        return NV_ERR_NOT_SUPPORTED;

    NV_ENTER_RM_RUNTIME(sp, fp);
    threadStateInit(&threadState, THREAD_STATE_FLAGS_NONE);

    state = (NVRMTelemetryState *)*opaqueState;
    if (state == NULL)
    {
        state = (NVRMTelemetryState *)portMemAllocNonPaged(sizeof(*state));
        if (state == NULL)
        {
            status = NV_ERR_NO_MEMORY;
            goto done;
        }
        portMemSet(state, 0, sizeof(*state));
        state->pGpu = pGpu;
        state->pmGeneration = pmGeneration;
        *opaqueState = state;
    }
    else if (state->pGpu != pGpu)
    {
        status = NV_ERR_INVALID_STATE;
        goto done;
    }

    if ((status = rmapiLockAcquire(API_LOCK_FLAGS_NONE, RM_LOCK_MODULES_OSAPI)) != NV_OK)
        goto done;
    if ((status = rmDeviceGpuLocksAcquire(pGpu, GPUS_LOCK_FLAGS_NONE, RM_LOCK_MODULES_OSAPI)) == NV_OK)
    {
        if (pGpu->userSharedData.pMapBuffer == NULL)
        {
            if (pGpu->userSharedData.pMemDesc != NULL)
                status = NV_ERR_INVALID_STATE;
            else
                status = gpuCreateRusdMemory_HAL(pGpu);
            if (status == NV_OK && pGpu->userSharedData.pMapBuffer == NULL)
                status = NV_ERR_NOT_SUPPORTED;
        }

        if (state->pmGeneration != pmGeneration)
        {
            if (status == NV_OK)
                status = _nvrmTelemetryFreeRusdObject(state);
            if (status == NV_OK)
                state->pmGeneration = pmGeneration;
        }

        if (status == NV_OK)
            status = _nvrmTelemetryEnsureRusdObject(pGpu, state);

        if (status == NV_OK)
        {
            pSharedData = (NV00DE_SHARED_DATA *)pGpu->userSharedData.pMapBuffer;
            (void)_nvrmTelemetryReadPower(pSharedData, sample);
            (void)_nvrmTelemetryReadClocks(pSharedData, sample);
            (void)_nvrmTelemetryReadFans(pSharedData, state, sample);
        }

        rmDeviceGpuLocksRelease(pGpu, GPUS_LOCK_FLAGS_NONE, NULL);
    }
    rmapiLockRelease();

done:
    threadStateFree(&threadState, THREAD_STATE_FLAGS_NONE);
    NV_EXIT_RM_RUNTIME(sp, fp);
    return status;
}

void
nvrm_free_telemetry(void **opaqueState)
{
    NVRMTelemetryState *state;
    OBJGPU *pGpu;
    THREAD_STATE_NODE threadState;
    NV_STATUS lockStatus;
    void *fp;
    nvidia_stack_t *sp = NULL;

    if (opaqueState == NULL || *opaqueState == NULL)
        return;

    state = (NVRMTelemetryState *)*opaqueState;
    pGpu = state->pGpu;

    NV_ENTER_RM_RUNTIME(sp, fp);
    threadStateInit(&threadState, THREAD_STATE_FLAGS_NONE);

    if (pGpu != NULL)
    {
        lockStatus = rmapiLockAcquire(API_LOCK_FLAGS_NONE, RM_LOCK_MODULES_OSAPI);
        if (lockStatus == NV_OK)
        {
            lockStatus = rmDeviceGpuLocksAcquire(pGpu, GPUS_LOCK_FLAGS_NONE,
                                                  RM_LOCK_MODULES_OSAPI);
            if (lockStatus == NV_OK)
            {
                RM_API *pRmApi = rmapiGetInterface(RMAPI_GPU_LOCK_INTERNAL);
                NV_STATUS freeStatus = NV_ERR_INVALID_STATE;
                if (pRmApi != NULL && state->hClient != 0)
                    // Freeing our root RM client also tears down child
                    // resources, including the RUSD polling object.
                    freeStatus = pRmApi->Free(pRmApi, state->hClient, state->hClient);
                if (freeStatus != NV_OK && state->hClient != 0)
                {
                    NV_PRINTF(LEVEL_ERROR,
                              "NVRM telemetry: owned RM client free failed (0x%x); trying child/device/client cleanup\n",
                              freeStatus);
                    if (pRmApi != NULL)
                        rmapiutilFreeClientAndDeviceHandles(pRmApi, &state->hClient,
                                                            &state->hDevice,
                                                            &state->hSubdevice);
                }
                state->hRusdObject = 0;
                state->hClient = 0;
                state->hDevice = 0;
                state->hSubdevice = 0;
                rmDeviceGpuLocksRelease(pGpu, GPUS_LOCK_FLAGS_NONE, NULL);
            }
            else if (state->hClient != 0)
            {
                NV_PRINTF(LEVEL_ERROR,
                          "NVRM telemetry: GPU lock acquire failed during owned RM client cleanup (0x%x)\n",
                          lockStatus);
            }
            rmapiLockRelease();
        }
        else if (state->hClient != 0)
            NV_PRINTF(LEVEL_ERROR,
                      "NVRM telemetry: RM API lock acquire failed during owned RM client cleanup (0x%x)\n",
                      lockStatus);
    }
    else if (state->hClient != 0)
        NV_PRINTF(LEVEL_ERROR,
                  "NVRM telemetry: GPU state missing during owned RM client cleanup\n");

    threadStateFree(&threadState, THREAD_STATE_FLAGS_NONE);
    NV_EXIT_RM_RUNTIME(sp, fp);

    portMemFree(state);
    *opaqueState = NULL;
}
