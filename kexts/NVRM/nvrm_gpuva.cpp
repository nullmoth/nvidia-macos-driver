/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include "nv-xnu.h"
#include <IOKit/IOLib.h>
#include <IOKit/IOLocks.h>

extern "C" {
#include "nvtypes.h"
#include "nvos.h"
#include "nvmisc.h"
#include "nv-kernel-rmapi-ops.h"
void NV_API_CALL rm_kernel_rmapi_op(nvidia_stack_t *sp, void *ops_cmd);
}

#include "nvrm_gpuva_abi.h"

#define NVRM_CLS_FERMI_VASPACE_A     0x000090f1
#define NVRM_CLS_NV50_MEMORY_VIRTUAL 0x000050a0

static inline void nvGvZero(void *p, unsigned long n)
{ volatile unsigned char *b = (volatile unsigned char *)p; while (n--) *b++ = 0; }

#define GVLOG(fmt, ...) kprintf("NVRM-gpuva: " fmt "\n", ##__VA_ARGS__)

enum { kNvGpuVaMaxDevices = 4 };
struct NvGpuVaSpace {
    NvU32 hClient;
    NvU32 hDevice;
    NvU32 hVaSpace;
};
static NvGpuVaSpace gSpaces[kNvGpuVaMaxDevices];
static unsigned     gSpaceCount;
static IOLock      *gSpaceLock;
static unsigned     gAllocLogged, gMapLogged;

extern "C" unsigned int nvrm_kapi_mem_handle(const void *mem);
extern "C" int nvrm_kapi_dev_handles(const void *dev, unsigned int *c, unsigned int *d, unsigned int *s);

static bool nvGpuVaResolve(struct NVRMGpuVaRequest *r)
{
    unsigned int c = 0, d = 0, sd = 0;
    if (!nvrm_kapi_dev_handles(r->kapiDevice, &c, &d, &sd) || !c || !d) {
        GVLOG("no RM handles: kapiDevice %p", r->kapiDevice);
        return false;
    }
    r->hClient = c; r->hDevice = d;
    return true;
}

static void nvGpuVaInitOnce(void)
{
    if (!gSpaceLock) gSpaceLock = IOLockAlloc();
}

static NvV32 nvGpuVaRmAlloc(NvU32 hRoot, NvU32 hParent, NvU32 hClass,
                            void *pParams, NvU32 paramsSize, NvU32 *hOut)
{
    nvidia_kernel_rmapi_ops_t ops;
    nvGvZero(&ops, sizeof ops);
    ops.op = NV04_ALLOC;
    ops.params.alloc.hRoot            = hRoot;
    ops.params.alloc.hObjectParent    = hParent;
    ops.params.alloc.hObjectNew       = 0;
    ops.params.alloc.hClass           = (NvV32)hClass;
    ops.params.alloc.pAllocParms      = (NvP64)(NvUPtr)pParams;
    ops.params.alloc.pRightsRequested = (NvP64)0;
    ops.params.alloc.paramsSize       = paramsSize;
    ops.params.alloc.flags            = NVOS64_FLAGS_NONE;
    rm_kernel_rmapi_op(NULL, &ops);
    if (ops.params.alloc.status == 0 && hOut) *hOut = ops.params.alloc.hObjectNew;
    return ops.params.alloc.status;
}

static NvV32 nvGpuVaRmFree(NvU32 hRoot, NvU32 hParent, NvU32 hObject)
{
    nvidia_kernel_rmapi_ops_t ops;
    nvGvZero(&ops, sizeof ops);
    ops.op = NV01_FREE;
    ops.params.free.hRoot         = hRoot;
    ops.params.free.hObjectParent = hParent;
    ops.params.free.hObjectOld    = hObject;
    rm_kernel_rmapi_op(NULL, &ops);
    return ops.params.free.status;
}

static NvV32 nvGpuVaSpaceFor(NvU32 hClient, NvU32 hDevice, NvU32 *hVaOut)
{
    nvGpuVaInitOnce();
    if (!gSpaceLock) return NV_ERR_INVALID_STATE;
    IOLockLock(gSpaceLock);
    for (unsigned i = 0; i < gSpaceCount; i++) {
        if (gSpaces[i].hClient == hClient && gSpaces[i].hDevice == hDevice) {
            *hVaOut = gSpaces[i].hVaSpace;
            IOLockUnlock(gSpaceLock);
            return 0;
        }
    }
    if (gSpaceCount >= kNvGpuVaMaxDevices) {
        IOLockUnlock(gSpaceLock);
        GVLOG("no room for another VA space (%u already)", gSpaceCount);
        return NV_ERR_INSUFFICIENT_RESOURCES;
    }
    NV_VASPACE_ALLOCATION_PARAMETERS vap;
    nvGvZero(&vap, sizeof vap);
    vap.flags = NV_VASPACE_ALLOCATION_FLAGS_RETRY_PTE_ALLOC_IN_SYS;
    NvU32 hVa = 0;
    NvV32 st = nvGpuVaRmAlloc(hClient, hDevice, NVRM_CLS_FERMI_VASPACE_A, &vap, sizeof vap, &hVa);
    if (st != 0 || !hVa) {
        IOLockUnlock(gSpaceLock);
        GVLOG("FERMI_VASPACE_A refused for client 0x%x device 0x%x -> status 0x%x", hClient, hDevice, st);
        return st ? st : NV_ERR_GENERIC;
    }
    gSpaces[gSpaceCount].hClient  = hClient;
    gSpaces[gSpaceCount].hDevice  = hDevice;
    gSpaces[gSpaceCount].hVaSpace = hVa;
    gSpaceCount++;
    IOLockUnlock(gSpaceLock);
    GVLOG("FERMI_VASPACE_A 0x%x created for client 0x%x device 0x%x", hVa, hClient, hDevice);
    *hVaOut = hVa;
    return 0;
}

extern "C" int nvrm_gpuva_alloc(struct NVRMGpuVaRequest *r)
{
    if (!r || r->version != NVRM_GPUVA_ABI_VERSION) return -1;
    if (!nvGpuVaResolve(r)) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    r->status = 0; r->gpuva = 0; r->hVirt = 0;
    if (!r->hClient || !r->hDevice || !r->size) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }

    NvU32 hVa = 0;
    NvV32 st = nvGpuVaSpaceFor(r->hClient, r->hDevice, &hVa);
    if (st != 0) { r->status = st; return -1; }

    NV_MEMORY_ALLOCATION_PARAMS vp;
    nvGvZero(&vp, sizeof vp);
    vp.owner    = r->hClient;
    vp.type     = NVOS32_TYPE_IMAGE;
    vp.flags    = NVOS32_ALLOC_FLAGS_VIRTUAL;
    vp.size     = r->size;
    vp.hVASpace = hVa;
    if (r->align && (r->align & (r->align - 1)) == 0 && r->align <= (1ull << 30)) {
        vp.alignment = r->align;
        vp.flags |= NVOS32_ALLOC_FLAGS_ALIGNMENT_FORCE;
    }

    NvU32 hVirt = 0;
    st = nvGpuVaRmAlloc(r->hClient, r->hDevice, NVRM_CLS_NV50_MEMORY_VIRTUAL, &vp, sizeof vp, &hVirt);
    if (st != 0 || !hVirt) {
        r->status = st ? st : NV_ERR_GENERIC;
        if (gAllocLogged < 8) { gAllocLogged++;
            GVLOG("NV50_MEMORY_VIRTUAL refused %llu bytes (align %llu) -> status 0x%x",
                  (unsigned long long)r->size, (unsigned long long)r->align, st); }
        return -1;
    }
    r->hVirt = hVirt;
    r->gpuva = vp.offset;
    if (gAllocLogged < 8) { gAllocLogged++;
        GVLOG("va alloc %llu bytes align %llu -> gpuva 0x%llx (hVirt 0x%x)",
              (unsigned long long)r->size, (unsigned long long)r->align,
              (unsigned long long)r->gpuva, hVirt); }
    return 0;
}

extern "C" int nvrm_gpuva_map(struct NVRMGpuVaRequest *r)
{
    if (!r || r->version != NVRM_GPUVA_ABI_VERSION) return -1;
    if (!nvGpuVaResolve(r)) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    r->status = 0;
    if (!r->hMemory) r->hMemory = nvrm_kapi_mem_handle(r->kapiMemory);
    if (!r->hClient || !r->hDevice || !r->hVirt || !r->hMemory || !r->size) {
        r->status = NV_ERR_INVALID_ARGUMENT; return -1;
    }
    nvidia_kernel_rmapi_ops_t ops;
    nvGvZero(&ops, sizeof ops);
    ops.op = NV04_MAP_MEMORY_DMA;
    ops.params.mapMemoryDma.hClient = r->hClient;
    ops.params.mapMemoryDma.hDevice = r->hDevice;
    ops.params.mapMemoryDma.hDma    = r->hVirt;
    ops.params.mapMemoryDma.hMemory = r->hMemory;
    ops.params.mapMemoryDma.offset  = 0;
    ops.params.mapMemoryDma.length  = r->size;
    ops.params.mapMemoryDma.flags   = DRF_DEF(OS46, _FLAGS, _PAGE_KIND, _VIRTUAL)
                                    | (r->isVidmem ? DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _DISABLE)
                                                   : DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _ENABLE));
    ops.params.mapMemoryDma.dmaOffset = 0;
    rm_kernel_rmapi_op(NULL, &ops);
    r->status = ops.params.mapMemoryDma.status;
    if (r->status != 0) {
        if (gMapLogged < 8) { gMapLogged++;
            GVLOG("MAP_MEMORY_DMA hVirt 0x%x hMem 0x%x %llu bytes %s -> status 0x%x",
                  r->hVirt, r->hMemory, (unsigned long long)r->size,
                  r->isVidmem ? "vidmem" : "sysmem", r->status); }
        return -1;
    }
    r->gpuva = (unsigned long long)ops.params.mapMemoryDma.dmaOffset;
    if (gMapLogged < 8) { gMapLogged++;
        GVLOG("MAPPED hVirt 0x%x <- hMem 0x%x  %llu bytes %s  dmaOffset 0x%llx",
              r->hVirt, r->hMemory, (unsigned long long)r->size,
              r->isVidmem ? "vidmem" : "sysmem",
              (unsigned long long)ops.params.mapMemoryDma.dmaOffset); }
    return 0;
}

extern "C" int nvrm_gpuva_free(struct NVRMGpuVaRequest *r)
{
    if (!r || r->version != NVRM_GPUVA_ABI_VERSION) return -1;
    if (!nvGpuVaResolve(r)) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    r->status = 0;
    if (!r->hClient || !r->hDevice || !r->hVirt) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    if (r->hMemory) {
        nvidia_kernel_rmapi_ops_t ops;
        nvGvZero(&ops, sizeof ops);
        ops.op = NV04_UNMAP_MEMORY_DMA;
        ops.params.unmapMemoryDma.hClient   = r->hClient;
        ops.params.unmapMemoryDma.hDevice   = r->hDevice;
        ops.params.unmapMemoryDma.hDma      = r->hVirt;
        ops.params.unmapMemoryDma.hMemory   = r->hMemory;
        ops.params.unmapMemoryDma.flags     = 0;
        ops.params.unmapMemoryDma.dmaOffset = r->gpuva;
        rm_kernel_rmapi_op(NULL, &ops);
        if (ops.params.unmapMemoryDma.status != 0)
            GVLOG("UNMAP_MEMORY_DMA hVirt 0x%x -> status 0x%x (freeing anyway)",
                  r->hVirt, ops.params.unmapMemoryDma.status);
    }
    if (r->flags & NVRM_GPUVA_FLAG_UNMAP_ONLY) return 0;
    r->status = nvGpuVaRmFree(r->hClient, r->hDevice, r->hVirt);
    return r->status ? -1 : 0;
}
