/* nvmtl-smi: a read-only report of the GPU's temperature, power, clocks, pstate, utilisation, throttle reasons and fan,
 * from NVIDIA's user-shared-data block (class 0xde, polled by the resource manager). Our own code on the public class
 * header; it reaches RM the way NVK does (the nvrm_xnu and nvRmApi layers), and only reads.
 * build: build.sh <patched Mesa tree>   run: nvmtl-smi [gpu index]
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "nvRmApi.h"
#include "ctrl/ctrl0000/ctrl0000gpu.h"
#include "class/cl0080.h"
#include "class/cl2080.h"
#include <sys/sysctl.h>
#include "rusd_format.h"
#include "tgp_format.h"

int main(int argc, char **argv)
{
    unsigned want = argc > 1 ? (unsigned)atoi(argv[1]) : 0;
    struct NvRmApi rm; memset(&rm, 0, sizeof rm);
    rm.fd = NVRM_OPEN(NVRM_CTL_NODE_NAME);
    if (rm.fd < 0) { fprintf(stderr, "nvmtl-smi: cannot open %s (is the driver loaded?)\n", NVRM_CTL_NODE_NAME); return 2; }
    NvHandle hClient = 0, hDevice = 0, hSub = 0, hRusd = 0;
    if (nvRmApiAlloc(&rm, 0, &hClient, NV01_ROOT_CLIENT, NULL) != NV_OK) { fprintf(stderr, "nvmtl-smi: no RM client\n"); return 2; }
    rm.hClient = hClient;
    NV0000_CTRL_GPU_GET_ATTACHED_IDS_PARAMS ids; memset(&ids, 0xff, sizeof ids);
    if (nvRmApiControl(&rm, hClient, NV0000_CTRL_CMD_GPU_GET_ATTACHED_IDS, &ids, sizeof ids) != NV_OK || want >= NV0000_CTRL_GPU_MAX_ATTACHED_GPUS || ids.gpuIds[want] == NV0000_CTRL_GPU_INVALID_ID) {
        fprintf(stderr, "nvmtl-smi: no GPU %u\n", want); return 2;
    }
    NV0000_CTRL_GPU_GET_ID_INFO_V2_PARAMS info = { .gpuId = ids.gpuIds[want] };
    if (nvRmApiControl(&rm, hClient, NV0000_CTRL_CMD_GPU_GET_ID_INFO_V2, &info, sizeof info) != NV_OK) { fprintf(stderr, "nvmtl-smi: no id info\n"); return 2; }
    NV0080_ALLOC_PARAMETERS dp = { .deviceId = info.deviceInstance, .hClientShare = hClient };
    NV2080_ALLOC_PARAMETERS sp = { .subDeviceId = info.subDeviceInstance };
    if (nvRmApiAlloc(&rm, hClient, &hDevice, NV01_DEVICE_0, &dp) != NV_OK || nvRmApiAlloc(&rm, hDevice, &hSub, NV20_SUBDEVICE_0, &sp) != NV_OK) {
        fprintf(stderr, "nvmtl-smi: no device\n"); return 2;
    }
    NV00DE_ALLOC_PARAMETERS rp = { .polledDataMask = NV00DE_RUSD_POLL_CLOCK | NV00DE_RUSD_POLL_PERF | NV00DE_RUSD_POLL_POWER |
                                   NV00DE_RUSD_POLL_THERMAL | NV00DE_RUSD_POLL_FAN };
    NvU32 st = nvRmApiAlloc(&rm, hSub, &hRusd, RM_USER_SHARED_DATA, &rp);
    if (st != NV_OK) { fprintf(stderr, "nvmtl-smi: the resource manager refused the shared-data block (0x%x)\n", st); return 3; }
    struct NvRmApiMapping map; memset(&map, 0, sizeof map);
    st = nvRmApiMapMemory(&rm, hDevice, hRusd, 0, sizeof(NV00DE_SHARED_DATA), false, 0, &map);
    if (st != NV_OK || !map.address) { fprintf(stderr, "nvmtl-smi: cannot map the shared-data block (0x%x)\n", st); return 3; }
    usleep(1500 * 1000);   /* polling starts at allocation; give it a pass */
    NV00DE_SHARED_DATA snap;
    memcpy(&snap, map.address, sizeof snap);
    rusd_format(&snap, stdout);
    /* the notebook power budget manager (boot-arg nvrmtgp=1) publishes its state as sysctls */
    struct tgp_view tv; memset(&tv, 0, sizeof tv);
    size_t len = sizeof tv.state;
    tv.present = sysctlbyname("debug.nvrm_tgp_state", &tv.state, &len, NULL, 0) == 0;
    if (tv.present) {
        len = sizeof tv.enable;      sysctlbyname("debug.nvrm_tgp_enable", &tv.enable, &len, NULL, 0);
        len = sizeof tv.ac;          sysctlbyname("debug.nvrm_tgp_ac", &tv.ac, &len, NULL, 0);
        len = sizeof tv.last_status; sysctlbyname("debug.nvrm_tgp_last_status", &tv.last_status, &len, NULL, 0);
        len = sizeof tv.applies;     sysctlbyname("debug.nvrm_tgp_applies", &tv.applies, &len, NULL, 0);
    }
    tgp_format(&tv, stdout);
    return 0;
}
