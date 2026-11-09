// channel.h: a GPFIFO channel on one engine (here NVENC) with one engine object, a push buffer, and submission
// through the usermode doorbell. Sequence as NVK on this driver: notifier + context DMA, USERD, GPFIFO ring,
// channel (engineType), bind, object, schedule, work-submit token.
#pragma once
#include "rm.h"

typedef struct {
    Rm *rm;
    RmMem notifier, userd, gpfifo, push;
    NvHandle hCtxDma, hChannel, hObject, hUsermode;
    RmConn usermodeMap; volatile NvU32 *doorbell;
    NvU32 token, gpPut, entries;
    NvU32 *cur;                        // write position in push
    NvU32 *segStart;                   // start of the not yet submitted commands
} Chan;

// engineType: NV2080_ENGINE_TYPE_*; objClass/objParams: the engine object (e.g. 0xc9b7 + NV_NVENC_ALLOCATION_PARAMETERS)
NV_STATUS chan_create(Rm *rm, Chan *c, NvU32 engineType, NvU32 objClass, void *objParams);
void chan_destroy(Chan *c);
// incrementing method header + data: method byte offset, on subchannel subch
void chan_mthd(Chan *c, int subch, NvU32 mthd, int count, ...);
NV_STATUS chan_submit(Chan *c);                // everything since the last submit
NvU32 chan_gpget(Chan *c);                     // how far the channel has fetched
bool chan_rewind(Chan *c);                     // reuse the push buffer from its start (only when everything was fetched)
size_t chan_push_used(Chan *c);                // bytes of push buffer in use
NvU32 chan_error(Chan *c);                     // RM's error notifier status (0 = none)
// the channel's last MMU fault (NV906F_CTRL_CMD_GET_MMU_FAULT_INFO; RM clears it on read)
NV_STATUS chan_fault(Chan *c, NvU64 *addr, NvU32 *type, char str[32]);
