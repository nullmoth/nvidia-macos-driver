// channel.c: see channel.h.
#include "channel.h"
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include "class/cl0002.h"
#include "class/clc56f.h"
#include "class/clc361.h"
#include "class/clc561.h"
#include "alloc/alloc_channel.h"
#include "ctrl/ctrla06f/ctrla06fgpfifo.h"
#include "ctrl/ctrl906f.h"
#include "ctrl/ctrlc36f.h"

#define CHK(x) do { NV_STATUS s_ = (x); if (s_) { fprintf(stderr, "chan: %s -> 0x%x\n", #x, s_); return s_; } } while (0)

NV_STATUS chan_create(Rm *rm, Chan *c, NvU32 engineType, NvU32 objClass, void *objParams)
{
    memset(c, 0, sizeof *c); c->rm = rm; c->entries = 0x8000;
    c->usermodeMap.fd = -1;
    // doorbell page (usermode class): writing a channel's work-submit token there makes the host fetch GPPut
    c->hUsermode = rm_handle(rm);
    CHK(rm_alloc(rm, rm->subdevice, c->hUsermode, rm_class(rm, RM_CLS_USERMODE), NULL));   // Turing / Ampere / Ada: same page layout
    void *um; CHK(rm_map_cpu(rm, rm->subdevice, c->hUsermode, 4096, false, &c->usermodeMap, &um));
    c->doorbell = (volatile NvU32 *)((NvU8 *)um + NVC361_NOTIFY_CHANNEL_PENDING);

    CHK(rm_mem_alloc(rm, &c->notifier, 0x1000, 0x1000, true, true, true));     // error + work-submit-token notifiers
    CHK(rm_mem_alloc(rm, &c->userd, 0x80000, 0x10000, false, true, true));     // channel control (GPPut/GPGet), as NVK
    CHK(rm_mem_alloc(rm, &c->gpfifo, c->entries * 8, 0x1000, true, true, true));
    CHK(rm_mem_alloc(rm, &c->push, 0x10000, 0x1000, true, true, true));
    c->cur = c->segStart = c->push.cpu;

    NV_CONTEXT_DMA_ALLOCATION_PARAMS cd = { .flags = DRF_DEF(OS03, _FLAGS, _MAPPING, _KERNEL) | DRF_DEF(OS03, _FLAGS, _HASH_TABLE, _DISABLE),
                                           .hMemory = c->notifier.hPhys, .offset = 0, .limit = c->notifier.size - 1 };
    c->hCtxDma = rm_handle(rm);
    CHK(rm_alloc(rm, rm->device, c->hCtxDma, NV01_CONTEXT_DMA, &cd));

    NV_CHANNEL_ALLOC_PARAMS cp = { .hObjectError = c->hCtxDma, .gpFifoOffset = c->gpfifo.gpu, .gpFifoEntries = c->entries,
                                   .hVASpace = rm->vaspace, .hUserdMemory = { c->userd.hPhys }, .userdOffset = { 0 },
                                   .engineType = engineType };
    c->hChannel = rm_handle(rm);
    CHK(rm_alloc(rm, rm->device, c->hChannel, rm_class(rm, RM_CLS_CHANNEL), &cp));   // same GPFIFO / method format since Volta
    NVA06F_CTRL_BIND_PARAMS bp = { .engineType = engineType };
    CHK(rm_ctrl(rm, c->hChannel, NVA06F_CTRL_CMD_BIND, &bp, sizeof bp));
    c->hObject = rm_handle(rm);
    CHK(rm_alloc(rm, c->hChannel, c->hObject, objClass, objParams));
    NVA06F_CTRL_GPFIFO_SCHEDULE_PARAMS sp = { .bEnable = NV_TRUE };
    CHK(rm_ctrl(rm, c->hChannel, NVA06F_CTRL_CMD_GPFIFO_SCHEDULE, &sp, sizeof sp));
    NVC36F_CTRL_GPFIFO_SET_WORK_SUBMIT_TOKEN_NOTIF_INDEX_PARAMS np = { .index = NV_CHANNELGPFIFO_NOTIFICATION_TYPE_WORK_SUBMIT_TOKEN };
    CHK(rm_ctrl(rm, c->hChannel, NVC36F_CTRL_CMD_GPFIFO_SET_WORK_SUBMIT_TOKEN_NOTIF_INDEX, &np, sizeof np));
    NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN_PARAMS tp = { 0 };
    CHK(rm_ctrl(rm, c->hChannel, NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN, &tp, sizeof tp));
    c->token = tp.workSubmitToken;
    return NV_OK;
}

void chan_destroy(Chan *c)
{
    Rm *rm = c->rm; if (!rm) return;
    if (c->hChannel) rm_free(rm, rm->device, c->hChannel);
    if (c->hCtxDma) rm_free(rm, rm->device, c->hCtxDma);
    rm_mem_free(rm, &c->push); rm_mem_free(rm, &c->gpfifo); rm_mem_free(rm, &c->userd); rm_mem_free(rm, &c->notifier);
    if (c->usermodeMap.conn) rm_unmap_cpu(rm, &c->usermodeMap);
    if (c->hUsermode) rm_free(rm, rm->subdevice, c->hUsermode);
    memset(c, 0, sizeof *c);
}

void chan_mthd(Chan *c, int subch, NvU32 mthd, int count, ...)
{
    *c->cur++ = DRF_NUM(C56F, _DMA, _SEC_OP, NVC56F_DMA_SEC_OP_INC_METHOD) | DRF_NUM(C56F, _DMA, _METHOD_COUNT, count) |
                DRF_NUM(C56F, _DMA, _METHOD_SUBCHANNEL, subch) | DRF_NUM(C56F, _DMA, _METHOD_ADDRESS, mthd >> 2);
    va_list ap; va_start(ap, count);
    for (int i = 0; i < count; i++) *c->cur++ = va_arg(ap, NvU32);
    va_end(ap);
}

NV_STATUS chan_submit(Chan *c)
{
    NvU64 va = c->push.gpu + (NvU64)((NvU8 *)c->segStart - (NvU8 *)c->push.cpu);
    NvU32 dwords = (NvU32)(c->cur - c->segStart);
    if (!dwords) return NV_OK;
    NvU32 *e = (NvU32 *)c->gpfifo.cpu + 2 * c->gpPut;
    e[0] = DRF_NUM(C56F, _GP_ENTRY0, _GET, (NvU32)va >> 2);
    e[1] = DRF_NUM(C56F, _GP_ENTRY1, _GET_HI, (NvU32)(va >> 32)) | DRF_NUM(C56F, _GP_ENTRY1, _LENGTH, dwords);
    c->gpPut = (c->gpPut + 1) % c->entries;
    c->segStart = c->cur;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    ((volatile AmpereAControlGPFifo *)c->userd.cpu)->GPPut = c->gpPut;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    *c->doorbell = c->token;
    return NV_OK;
}

bool chan_rewind(Chan *c)
{
    // the push buffer is linear: start over once the channel has fetched everything (GPGet == GPPut)
    if (c->cur != c->segStart || chan_gpget(c) != c->gpPut) return false;
    c->cur = c->segStart = c->push.cpu;
    return true;
}

NvU32 chan_gpget(Chan *c) { return ((volatile AmpereAControlGPFifo *)c->userd.cpu)->GPGet; }
NvU32 chan_error(Chan *c) { return ((volatile NvNotification *)c->notifier.cpu)[0].status; }

NV_STATUS chan_fault(Chan *c, NvU64 *addr, NvU32 *type, char str[32])
{
    NV906F_CTRL_GET_MMU_FAULT_INFO_PARAMS p = { 0 };
    NV_STATUS st = rm_ctrl(c->rm, c->hChannel, NV906F_CTRL_CMD_GET_MMU_FAULT_INFO, &p, sizeof p);
    *addr = ((NvU64)p.addrHi << 32) | p.addrLo; *type = p.faultType;
    memcpy(str, p.faultString, 32); str[31] = 0;
    return st;
}

size_t chan_push_used(Chan *c) { return (size_t)((NvU8 *)c->cur - (NvU8 *)c->push.cpu); }
