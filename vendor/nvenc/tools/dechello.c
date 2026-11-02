// dechello.c (milestone M8, T8.01): a channel on NVDEC<n> with the NVC9B0 decoder object; two semaphore releases prove
// the path (the decoder's counterpart of hello.c):
// one by the channel's host (it fetched and ran our commands), one by the NVDEC engine itself.
//   ./dechello [-v] [engine]    engine 0 = NVDEC0 (default), 1..7 = NVDEC1..7
#include "channel.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "class/clc56f.h"
#include "class/cl2080_notification.h"
#include "clc9b0.h"

int main(int argc, char **argv)
{
    Rm rm = { 0 }; int eng = 0;
    for (int i = 1; i < argc; i++) { if (!strcmp(argv[i], "-v")) rm.verbose = true; else eng = atoi(argv[i]); }
    bool v = rm.verbose;
    if (rm_open(&rm)) return 1; rm.verbose = v;
    if (rm_va_init(&rm)) { fprintf(stderr, "VA space failed\n"); return 1; }
    RmMem sem; if (rm_mem_alloc(&rm, &sem, 0x1000, 0x1000, true, true, true)) { fprintf(stderr, "semaphore memory failed\n"); return 1; }
    volatile NvU32 *sv = sem.cpu; memset((void *)sv, 0, 64);

    Chan c; NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = (NvU32)eng };
    NvU32 engineType = eng ? NV2080_ENGINE_TYPE_NVDEC1 + (NvU32)eng - 1 : NV2080_ENGINE_TYPE_NVDEC0;
    if (chan_create(&rm, &c, engineType, rm_class(&rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "channel failed\n"); return 1; }
    printf("channel 0x%08x on engine 0x%x, object 0x%x (NVC9B0), token 0x%x\n", c.hChannel, engineType, c.hObject, c.token);
    printf("gpu: push 0x%llx gpfifo 0x%llx userd 0x%llx sem 0x%llx\n", (unsigned long long)c.push.gpu,
           (unsigned long long)c.gpfifo.gpu, (unsigned long long)c.userd.gpu, (unsigned long long)sem.gpu);

    // bind the decoder object to subchannel 0
    chan_mthd(&c, 0, NVC56F_SET_OBJECT, 1, rm_class(&rm, RM_CLS_NVDEC));
    // host semaphore release: value 0x11110001 at sem+0
    chan_mthd(&c, 0, NVC56F_SEM_ADDR_LO, 5, (NvU32)sem.gpu, (NvU32)(sem.gpu >> 32), 0x11110001u, 0u,
              DRF_DEF(C56F, _SEM_EXECUTE, _OPERATION, _RELEASE) | DRF_DEF(C56F, _SEM_EXECUTE, _RELEASE_WFI, _EN) |
              DRF_DEF(C56F, _SEM_EXECUTE, _PAYLOAD_SIZE, _32BIT));
    // engine semaphore release (NVDEC): value 0x22220002 at sem+16
    NvU64 a = sem.gpu + 16;
    chan_mthd(&c, 0, NVC9B0_SEMAPHORE_A, 3, (NvU32)(a >> 32), (NvU32)a, 0x22220002u);
    chan_mthd(&c, 0, NVC9B0_SEMAPHORE_D, 1, DRF_DEF(C9B0, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B0, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B0, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    chan_submit(&c);

    struct timespec t0, t; clock_gettime(CLOCK_MONOTONIC, &t0);
    double ms = 0;
    while (ms < 2000 && sv[4] != 0x22220002u) {
        clock_gettime(CLOCK_MONOTONIC, &t); ms = (t.tv_sec - t0.tv_sec) * 1e3 + (t.tv_nsec - t0.tv_nsec) / 1e6;
    }
    {
        volatile NvNotification *n = c.notifier.cpu; volatile AmpereAControlGPFifo *u = c.userd.cpu;
        for (int i = 0; i < 4; i++) printf("notifier[%d]: info32 0x%08x info16 0x%04x status 0x%04x\n", i, n[i].info32, n[i].info16, n[i].status);
        printf("userd readback: GPPut %u GPGet %u Put 0x%x Get 0x%x\n", u->GPPut, u->GPGet, u->Put, u->Get);
        NvU32 nt = n[NV_CHANNELGPFIFO_NOTIFICATION_TYPE_WORK_SUBMIT_TOKEN].info32;
        if (sv[4] != 0x22220002u && nt && nt != c.token) {
            printf("ringing the doorbell again with the notifier's token 0x%x\n", nt);
            *c.doorbell = nt; clock_gettime(CLOCK_MONOTONIC, &t0); ms = 0;
            while (ms < 1000 && sv[4] != 0x22220002u) { clock_gettime(CLOCK_MONOTONIC, &t); ms = (t.tv_sec - t0.tv_sec) * 1e3 + (t.tv_nsec - t0.tv_nsec) / 1e6; }
        }
    }
    printf("after %.3f ms: host semaphore 0x%08x (%s), NVDEC semaphore 0x%08x (%s), GPGet %u/%u, error notifier 0x%x\n", ms,
           sv[0], sv[0] == 0x11110001u ? "ok" : "MISSING", sv[4], sv[4] == 0x22220002u ? "ok" : "MISSING", chan_gpget(&c), c.gpPut, chan_error(&c));
    int ok = sv[0] == 0x11110001u && sv[4] == 0x22220002u;
    chan_destroy(&c); rm_mem_free(&rm, &sem); rm_close(&rm);
    printf("%s\n", ok ? "T8.01 OK: NVDEC executed our commands" : "T8.01 FAILED");
    return ok ? 0 : 1;
}
