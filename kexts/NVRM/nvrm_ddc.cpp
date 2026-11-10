/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// DDC/CI for NVRMFB. The resource manager registers an I2C adapter per DDC port and per DisplayPort AUX channel at GPU
// init (RmI2cAddGpuPorts) through nv_i2c_add_adapter(); on this platform that returned nothing, so the table stayed empty.
// Here the adapters are recorded, and nvrm_ddc_transfer() runs one DDC write or read for a display through
// rm_i2c_transfer(), the resource manager's own transfer (it takes the RM locks, and picks DP AUX or the I2C engine from
// how the adapter was registered).
#include "nv-xnu.h"
extern "C" {
#include "nvtypes.h"
#include "nvos.h"
#include "nvmisc.h"
#include "nv-kernel-rmapi-ops.h"
#include "ctrl/ctrl0073/ctrl0073specific.h"
void NV_API_CALL rm_kernel_rmapi_op(nvidia_stack_t *sp, void *ops_cmd);
NV_STATUS NV_API_CALL rm_i2c_transfer(nvidia_stack_t *, nv_state_t *, void *, nv_i2c_cmd_t, NvU8, NvU8, NvU32, NvU8 *);
nv_state_t *nv_xnu_gpu_state(void);
}
#include "nvrm_ddc_abi.h"

namespace {
enum { kMaxAdapters = 32 };
struct Adapter { nv_state_t *nv; NvU32 port; bool used; };
Adapter gAd[kMaxAdapters];
IOLock *gAdLock;
void lockInit() { if (!gAdLock) gAdLock = IOLockAlloc(); }
}

// Strong definitions: they replace the weak stubs in nv_xnu_stubs.cpp. The returned cookie is only compared by the
// resource manager, never dereferenced.
extern "C" void *NV_API_CALL nv_i2c_add_adapter(nv_state_t *nv, NvU32 port)
{
    // Opt-in (boot-arg nvrmfbddc=1, the same switch NVRMFB reads): without it the stub's old answer, no adapter, stands.
    UInt32 on = 0;
    if (!PE_parse_boot_argn("nvrmfbddc", &on, sizeof on) || !on) return NULL;
    lockInit();
    if (!gAdLock) return NULL;
    void *cookie = NULL;
    IOLockLock(gAdLock);
    for (unsigned i = 0; i < kMaxAdapters; i++)
        if (gAd[i].used && gAd[i].nv == nv && gAd[i].port == port) { cookie = (void *)(uintptr_t)(i + 1); break; }
    if (!cookie)
        for (unsigned i = 0; i < kMaxAdapters; i++)
            if (!gAd[i].used) { gAd[i] = { nv, port, true }; cookie = (void *)(uintptr_t)(i + 1); break; }
    IOLockUnlock(gAdLock);
    return cookie;
}
extern "C" void NV_API_CALL nv_i2c_del_adapter(nv_state_t *nv, void *cookie)
{
    lockInit();
    const uintptr_t i = (uintptr_t)cookie;
    if (!gAdLock || !i || i > kMaxAdapters) return;
    IOLockLock(gAdLock);
    if (gAd[i - 1].used && gAd[i - 1].nv == nv) gAd[i - 1].used = false;
    IOLockUnlock(gAdLock);
}

// RM_ERR status codes are NV_STATUS values; 0 is NV_OK. Anything else is returned to the caller in r->status.
extern "C" int nvrm_ddc_transfer(struct NVRMDdcRequest *r)
{
    if (!r) return -1;
    r->status = 0;
    if (!nvrmDdcAddrAllowed(r->addr7) || !r->len || r->len > kNvrmDdcMax || (r->op != kNvrmDdcWrite && r->op != kNvrmDdcRead)
        || (r->op == kNvrmDdcWrite && !nvrmDdcWriteLenAllowed(r->addr7, r->len)))
        { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    nv_state_t *nv = nv_xnu_gpu_state();
    if (!nv || nv->gpu_id != r->gpuId || !nv->rmapi.hClient || !nv->rmapi.hDisp) { r->status = NV_ERR_OBJECT_NOT_FOUND; return -1; }

    // The display's DDC port: the resource manager numbers adapters by (port - 1), as rm_i2c_add_adapter() does.
    NV0073_CTRL_SPECIFIC_GET_I2C_PORTID_PARAMS pid;
    for (unsigned i = 0; i < sizeof pid; i++) ((unsigned char *)&pid)[i] = 0;
    pid.displayId = r->displayId;
    nvidia_kernel_rmapi_ops_t ops;
    for (unsigned i = 0; i < sizeof ops; i++) ((unsigned char *)&ops)[i] = 0;
    ops.op = NV04_CONTROL;
    ops.params.control.hClient    = nv->rmapi.hClient;
    ops.params.control.hObject    = nv->rmapi.hDisp;
    ops.params.control.cmd        = NV0073_CTRL_CMD_SPECIFIC_GET_I2C_PORTID;
    ops.params.control.params     = (NvP64)(NvUPtr)&pid;
    ops.params.control.paramsSize = sizeof pid;
    rm_kernel_rmapi_op(NULL, &ops);
    if (ops.params.control.status != 0) { r->status = ops.params.control.status; return -1; }
    if (!pid.ddcPortId) { r->status = NV_ERR_OBJECT_NOT_FOUND; return -1; }

    void *cookie = NULL;
    lockInit();
    if (gAdLock) {
        IOLockLock(gAdLock);
        for (unsigned i = 0; i < kMaxAdapters; i++)
            if (gAd[i].used && gAd[i].nv == nv && gAd[i].port == pid.ddcPortId - 1) { cookie = (void *)(uintptr_t)(i + 1); break; }
        IOLockUnlock(gAdLock);
    }
    if (!cookie) { r->status = NV_ERR_OBJECT_NOT_FOUND; return -1; }
    r->status = rm_i2c_transfer(NULL, nv, cookie, r->op == kNvrmDdcWrite ? NV_I2C_CMD_WRITE : NV_I2C_CMD_READ,
                                (NvU8)r->addr7, 0, r->len, r->data);
    return r->status == 0 ? 0 : -1;
}

// One native DPCD read or write on the display's AUX channel (NV0073_CTRL_CMD_DP_AUXCH_CTRL, public control interface).
// Only the eDP panel-control range is reachable: DPCD 0x700 to 0x7ff, where the backlight registers are.
#include "ctrl/ctrl0073/ctrl0073dp.h"
extern "C" int nvrm_dpaux_transfer(struct NVRMDpAuxRequest *r)
{
    if (!r) return -1;
    r->status = 0;
    if (!r->len || r->len > kNvrmDpAuxMax || r->addr < 0x700 || r->addr >= 0x800 || r->addr + r->len > 0x800) { r->status = NV_ERR_INVALID_ARGUMENT; return -1; }
    nv_state_t *nv = nv_xnu_gpu_state();
    if (!nv || nv->gpu_id != r->gpuId || !nv->rmapi.hClient || !nv->rmapi.hDisp) { r->status = NV_ERR_OBJECT_NOT_FOUND; return -1; }
    NV0073_CTRL_DP_AUXCH_CTRL_PARAMS p;
    for (unsigned i = 0; i < sizeof p; i++) ((unsigned char *)&p)[i] = 0;
    p.displayId = r->displayId;
    p.cmd = DRF_DEF(0073_CTRL, _DP_AUXCH_CMD, _TYPE, _AUX) | DRF_DEF(0073_CTRL, _DP_AUXCH_CMD, _I2C_MOT, _FALSE) |
            (r->write ? DRF_DEF(0073_CTRL, _DP_AUXCH_CMD, _REQ_TYPE, _WRITE) : DRF_DEF(0073_CTRL, _DP_AUXCH_CMD, _REQ_TYPE, _READ));
    p.addr = r->addr; p.size = r->len;
    if (r->write) for (unsigned i = 0; i < r->len; i++) p.data[i] = r->data[i];
    nvidia_kernel_rmapi_ops_t ops;
    for (unsigned i = 0; i < sizeof ops; i++) ((unsigned char *)&ops)[i] = 0;
    ops.op = NV04_CONTROL;
    ops.params.control.hClient = nv->rmapi.hClient; ops.params.control.hObject = nv->rmapi.hDisp;
    ops.params.control.cmd = NV0073_CTRL_CMD_DP_AUXCH_CTRL;
    ops.params.control.params = (NvP64)(NvUPtr)&p; ops.params.control.paramsSize = sizeof p;
    rm_kernel_rmapi_op(NULL, &ops);
    if (ops.params.control.status != 0) { r->status = ops.params.control.status; return -1; }
    if (p.replyType != NV0073_CTRL_DP_AUXCH_REPLYTYPE_ACK) { r->status = NV_ERR_INVALID_STATE; return -1; }
    if (!r->write) for (unsigned i = 0; i < r->len && i < p.size; i++) r->data[i] = p.data[i];
    return 0;
}
