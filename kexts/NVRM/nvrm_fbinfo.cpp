/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
#include <IOKit/IOLib.h>

extern "C" {
#include "nvtypes.h"
#include "nvos.h"
#include "nvmisc.h"
#include "nv-kernel-rmapi-ops.h"
#include "class/cl0080.h"
#include "class/cl2080.h"

void NV_API_CALL rm_kernel_rmapi_op(nvidia_stack_t *sp, void *ops_cmd);
}

#define FBILOG(fmt, ...) kprintf("NVRM-fbinfo: " fmt "\n", ##__VA_ARGS__)

enum {
    kClsRootClient   = 0x00000041u,
    kClsDevice0      = 0x00000080u,
    kClsSubdevice0   = 0x00002080u,
    kCmdGpuAttachIds = 0x00000215u,
    kCmdFbGetInfoV2  = 0x20801303u,
    kCmdMcGetArch    = 0x20801701u,   /* NV2080_CTRL_CMD_MC_GET_ARCH_INFO */
    kCmdClassListV2  = 0x00800292u,   /* NV0080_CTRL_CMD_GPU_GET_CLASSLIST_V2 */
    kClassListMax    = 200u,
    kFbIdxRamSize    = 0x00000007u,
    kFbIdxTotalRam   = 0x00000008u,
    kGpuInvalidId    = 0xffffffffu,
    kMaxAttachedGpus = 32u
};

static inline void fbiZero(void *p, unsigned long n)
{ volatile unsigned char *b = (volatile unsigned char *)p; while (n--) *b++ = 0; }

struct FbiAttachIds { NvU32 gpuIds[kMaxAttachedGpus]; NvU32 failedId; };

struct FbiInfo     { NvU32 index; NvU32 data; };
struct FbiArch     { NvU32 architecture; NvU32 implementation; NvU32 revision; NvU8 subRevision; };
struct FbiClasses  { NvU32 numClasses; NvU32 classList[kClassListMax]; };
struct FbiGetInfo  { NvU32 fbInfoListSize; struct FbiInfo fbInfoList[0x80]; };

static NvV32 fbiAlloc(NvU32 hRoot, NvU32 hParent, NvU32 hClass, void *pParams, NvU32 size, NvU32 *hOut)
{
    nvidia_kernel_rmapi_ops_t ops;
    fbiZero(&ops, sizeof ops);
    ops.op = NV04_ALLOC;
    ops.params.alloc.hRoot            = hRoot;
    ops.params.alloc.hObjectParent    = hParent;
    ops.params.alloc.hObjectNew       = 0;
    ops.params.alloc.hClass           = (NvV32)hClass;
    ops.params.alloc.pAllocParms      = (NvP64)(NvUPtr)pParams;
    ops.params.alloc.pRightsRequested = (NvP64)0;
    ops.params.alloc.paramsSize       = size;
    ops.params.alloc.flags            = NVOS64_FLAGS_NONE;
    rm_kernel_rmapi_op(NULL, &ops);
    if (ops.params.alloc.status == 0 && hOut) *hOut = ops.params.alloc.hObjectNew;
    return ops.params.alloc.status;
}

static NvV32 fbiControl(NvU32 hClient, NvU32 hObject, NvU32 cmd, void *pParams, NvU32 size)
{
    nvidia_kernel_rmapi_ops_t ops;
    fbiZero(&ops, sizeof ops);
    ops.op = NV04_CONTROL;
    ops.params.control.hClient    = hClient;
    ops.params.control.hObject    = hObject;
    ops.params.control.cmd        = cmd;
    ops.params.control.params     = (NvP64)(NvUPtr)pParams;
    ops.params.control.paramsSize = size;
    rm_kernel_rmapi_op(NULL, &ops);
    return ops.params.control.status;
}

static NvV32 fbiFree(NvU32 hRoot, NvU32 hParent, NvU32 hObject)
{
    nvidia_kernel_rmapi_ops_t ops;
    fbiZero(&ops, sizeof ops);
    ops.op = NV01_FREE;
    ops.params.free.hRoot         = hRoot;
    ops.params.free.hObjectParent = hParent;
    ops.params.free.hObjectOld    = hObject;
    rm_kernel_rmapi_op(NULL, &ops);
    return ops.params.free.status;
}

extern "C" bool nvrm_query_vram_kb(NvU32 gpuId, NvU32 *pRamKb, NvU32 *pTotalKb, NvU32 *pStatus)
{
    NvU32 hClient = 0, hDevice = 0, hSub = 0;
    NvV32 st = 0;
    bool  ok = false;
    struct FbiAttachIds   at;
    NV0080_ALLOC_PARAMETERS dp;
    NV2080_ALLOC_PARAMETERS sp;
    struct FbiGetInfo     fp;

    if (pRamKb)   *pRamKb   = 0;
    if (pTotalKb) *pTotalKb = 0;
    if (pStatus)  *pStatus  = 0xffffffffu;

    do {
        st = fbiAlloc(0, 0, kClsRootClient, NULL, 0, &hClient);
        if (st != 0 || !hClient) { FBILOG("NV01_ROOT_CLIENT -> status 0x%x", (unsigned)st); break; }

        fbiZero(&at, sizeof at);
        at.gpuIds[0] = gpuId;
        at.gpuIds[1] = kGpuInvalidId;
        st = fbiControl(hClient, hClient, kCmdGpuAttachIds, &at, sizeof at);
        if (st != 0) { FBILOG("GPU_ATTACH_IDS(0x%08x) -> status 0x%x (failedId 0x%x)",
                              (unsigned)gpuId, (unsigned)st, (unsigned)at.failedId); break; }

        fbiZero(&dp, sizeof dp);
        dp.deviceId = 0; dp.hClientShare = hClient;
        st = fbiAlloc(hClient, hClient, kClsDevice0, &dp, sizeof dp, &hDevice);
        if (st != 0 || !hDevice) { FBILOG("NV01_DEVICE_0 -> status 0x%x", (unsigned)st); break; }

        fbiZero(&sp, sizeof sp);
        sp.subDeviceId = 0;
        st = fbiAlloc(hClient, hDevice, kClsSubdevice0, &sp, sizeof sp, &hSub);
        if (st != 0 || !hSub) { FBILOG("NV20_SUBDEVICE_0 -> status 0x%x", (unsigned)st); break; }

        fbiZero(&fp, sizeof fp);
        fp.fbInfoListSize = 2;
        fp.fbInfoList[0].index = kFbIdxRamSize;
        fp.fbInfoList[1].index = kFbIdxTotalRam;
        st = fbiControl(hClient, hSub, kCmdFbGetInfoV2, &fp, sizeof fp);
        if (st != 0) { FBILOG("FB_GET_INFO_V2 -> status 0x%x", (unsigned)st); break; }

        if (pRamKb)   *pRamKb   = fp.fbInfoList[0].data;
        if (pTotalKb) *pTotalKb = fp.fbInfoList[1].data;
        FBILOG("gpu 0x%08x: RAM_SIZE %u KB, TOTAL_RAM_SIZE %u KB",
               (unsigned)gpuId, (unsigned)fp.fbInfoList[0].data, (unsigned)fp.fbInfoList[1].data);
        ok = (fp.fbInfoList[0].data != 0);
    } while (0);

    if (pStatus) *pStatus = (NvU32)st;

    if (hClient) {
        NvV32 fst = fbiFree(hClient, hClient, hClient);
        if (fst != 0) FBILOG("freeing the root client -> status 0x%x", (unsigned)fst);
    }
    return ok;
}

/*
 * The chip's identity and engine classes, read from the RM the way NVIDIA's own clients choose their code path: the
 * architecture from MC_GET_ARCH_INFO, and for each engine type the newest class the RM lists for this GPU (3D xx97,
 * compute xxC0, copy xxB5, video encode xxB7, decode xxB0). Everything chip-specific in the macOS layer (copy engine,
 * scan-out model, video libraries, native shaders) is chosen from this one answer instead of a device-id range.
 */
extern "C" bool nvrm_query_gpu_profile(NvU32 gpuId, NvU32 *pArch, NvU32 *pImpl, NvU32 *pCls3d, NvU32 *pClsCompute,
                                       NvU32 *pClsCopy, NvU32 *pClsEnc, NvU32 *pClsDec, NvU32 *pStatus)
{
    NvU32 hClient = 0, hDevice = 0, hSub = 0;
    NvV32 st = 0;
    bool  ok = false;
    struct FbiAttachIds   at;
    NV0080_ALLOC_PARAMETERS dp;
    NV2080_ALLOC_PARAMETERS sp;
    struct FbiArch        ap;
    static struct FbiClasses cl;   /* 804 bytes: off the kernel stack; callers run one at a time (attach) */

    *pArch = *pImpl = *pCls3d = *pClsCompute = *pClsCopy = *pClsEnc = *pClsDec = 0;
    if (pStatus) *pStatus = 0xffffffffu;
    do {
        st = fbiAlloc(0, 0, kClsRootClient, NULL, 0, &hClient);
        if (st != 0 || !hClient) { FBILOG("profile: NV01_ROOT_CLIENT -> status 0x%x", (unsigned)st); break; }
        fbiZero(&at, sizeof at);
        at.gpuIds[0] = gpuId;
        at.gpuIds[1] = kGpuInvalidId;
        st = fbiControl(hClient, hClient, kCmdGpuAttachIds, &at, sizeof at);
        if (st != 0) { FBILOG("profile: GPU_ATTACH_IDS -> status 0x%x", (unsigned)st); break; }
        fbiZero(&dp, sizeof dp);
        dp.deviceId = 0; dp.hClientShare = hClient;
        st = fbiAlloc(hClient, hClient, kClsDevice0, &dp, sizeof dp, &hDevice);
        if (st != 0 || !hDevice) { FBILOG("profile: NV01_DEVICE_0 -> status 0x%x", (unsigned)st); break; }
        fbiZero(&sp, sizeof sp);
        st = fbiAlloc(hClient, hDevice, kClsSubdevice0, &sp, sizeof sp, &hSub);
        if (st != 0 || !hSub) { FBILOG("profile: NV20_SUBDEVICE_0 -> status 0x%x", (unsigned)st); break; }

        fbiZero(&ap, sizeof ap);
        st = fbiControl(hClient, hSub, kCmdMcGetArch, &ap, sizeof ap);
        if (st != 0) { FBILOG("profile: MC_GET_ARCH_INFO -> status 0x%x", (unsigned)st); break; }
        *pArch = ap.architecture; *pImpl = ap.implementation;

        fbiZero(&cl, sizeof cl);
        st = fbiControl(hClient, hDevice, kCmdClassListV2, &cl, sizeof cl);
        if (st != 0) { FBILOG("profile: GET_CLASSLIST_V2 -> status 0x%x", (unsigned)st); break; }
        for (NvU32 i = 0; i < cl.numClasses && i < kClassListMax; i++) {
            const NvU32 c = cl.classList[i];
            if (c < 0xC000 || c > 0xFFFF) continue;   /* Turing (0xC5xx) and later; older compat classes are not ours */
            NvU32 *slot = nullptr;
            switch (c & 0xFF) {
                case 0x97: slot = pCls3d; break;
                case 0xC0: slot = pClsCompute; break;
                case 0xB5: slot = pClsCopy; break;
                case 0xB7: slot = pClsEnc; break;
                case 0xB0: slot = pClsDec; break;
            }
            if (slot && c > *slot) *slot = c;
        }
        FBILOG("profile: gpu 0x%08x arch 0x%x impl 0x%x 3d 0x%x compute 0x%x copy 0x%x enc 0x%x dec 0x%x (%u classes)",
               (unsigned)gpuId, (unsigned)ap.architecture, (unsigned)ap.implementation, (unsigned)*pCls3d,
               (unsigned)*pClsCompute, (unsigned)*pClsCopy, (unsigned)*pClsEnc, (unsigned)*pClsDec, (unsigned)cl.numClasses);
        ok = ap.architecture != 0;
    } while (0);
    if (pStatus) *pStatus = (NvU32)st;
    if (hClient) {
        NvV32 fst = fbiFree(hClient, hClient, hClient);
        if (fst != 0) FBILOG("profile: freeing the root client -> status 0x%x", (unsigned)fst);
    }
    return ok;
}
