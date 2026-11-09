/* nvmtl-boost: holds or drops one system-wide GPU boost request, so the boost can be switched at run time.
 * NVK asks for "boost to max" in every Metal client when the driver's conf has BOOST=1, and WindowServer never lets go, so
 * the GPU stays at its top performance state. With BOOST=0 in the conf no client boosts and this daemon alone decides: it
 * keeps a resource-manager client open that asks for BOOST_TO_MAX until cleared while the settings file says gpuboost=1,
 * and clears the request for gpuboost=0 (boost_policy.h). Closing the client (exit, crash, kill) clears it too.
 * Our own code on the public class headers; reaches RM the way nvmtl-smi does.
 *   nvmtl-boost [settings-file]   daemon, default /Library/NullMoth/gpuboost
 *   nvmtl-boost --once on|off     set once and exit (off clears this process's own request only)
 * build: build.sh <patched Mesa tree> <out>
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <fcntl.h>
#include <sys/stat.h>
#include "nvRmApi.h"
#include "ctrl/ctrl0000/ctrl0000gpu.h"
#include "ctrl/ctrl2080/ctrl2080perf.h"
#include "class/cl0080.h"
#include "class/cl2080.h"
#include "boost_policy.h"

static volatile sig_atomic_t gStop;
static void onsig(int s) { (void)s; gStop = 1; }

static struct NvRmApi rm;
static NvHandle hClient, hDevice, hSub;

static int rm_up(unsigned gpu)
{
    memset(&rm, 0, sizeof rm);
    rm.fd = NVRM_OPEN(NVRM_CTL_NODE_NAME);
    if (rm.fd < 0) { fprintf(stderr, "nvmtl-boost: cannot open %s (is the driver loaded?)\n", NVRM_CTL_NODE_NAME); return -1; }
    if (nvRmApiAlloc(&rm, 0, &hClient, NV01_ROOT_CLIENT, NULL) != NV_OK) { fprintf(stderr, "nvmtl-boost: no RM client\n"); return -1; }
    rm.hClient = hClient;
    NV0000_CTRL_GPU_GET_ATTACHED_IDS_PARAMS ids; memset(&ids, 0xff, sizeof ids);
    if (nvRmApiControl(&rm, hClient, NV0000_CTRL_CMD_GPU_GET_ATTACHED_IDS, &ids, sizeof ids) != NV_OK ||
        gpu >= NV0000_CTRL_GPU_MAX_ATTACHED_GPUS || ids.gpuIds[gpu] == NV0000_CTRL_GPU_INVALID_ID) { fprintf(stderr, "nvmtl-boost: no GPU %u\n", gpu); return -1; }
    NV0000_CTRL_GPU_GET_ID_INFO_V2_PARAMS info = { .gpuId = ids.gpuIds[gpu] };
    if (nvRmApiControl(&rm, hClient, NV0000_CTRL_CMD_GPU_GET_ID_INFO_V2, &info, sizeof info) != NV_OK) { fprintf(stderr, "nvmtl-boost: no id info\n"); return -1; }
    NV0080_ALLOC_PARAMETERS dp = { .deviceId = info.deviceInstance, .hClientShare = hClient };
    NV2080_ALLOC_PARAMETERS sp = { .subDeviceId = info.subDeviceInstance };
    if (nvRmApiAlloc(&rm, hClient, &hDevice, NV01_DEVICE_0, &dp) != NV_OK || nvRmApiAlloc(&rm, hDevice, &hSub, NV20_SUBDEVICE_0, &sp) != NV_OK) {
        fprintf(stderr, "nvmtl-boost: no device\n"); return -1;
    }
    return 0;
}

static int boost_send(int hold)
{
    NV2080_CTRL_PERF_BOOST_PARAMS p;
    memset(&p, 0, sizeof p);
    p.flags = hold ? DRF_DEF(2080, _CTRL_PERF_BOOST_FLAGS, _CMD, _BOOST_TO_MAX) : DRF_DEF(2080, _CTRL_PERF_BOOST_FLAGS, _CMD, _CLEAR);
    p.duration = hold ? NV2080_CTRL_PERF_BOOST_DURATION_INFINITE : 0;
    NvU32 st = nvRmApiControl(&rm, hSub, NV2080_CTRL_CMD_PERF_BOOST, &p, sizeof p);
    fprintf(stderr, "nvmtl-boost: %s -> 0x%x\n", hold ? "hold" : "release", (unsigned)st);
    return st == NV_OK ? 0 : -1;
}

/* the setting from the file; unset when the file is missing, not trusted or unreadable */
static enum boost_setting read_setting(const char *path)
{
    struct stat sb;
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return BOOST_UNSET;
    char buf[1024]; ssize_t n = -1;
    if (fstat(fd, &sb) == 0 && S_ISREG(sb.st_mode) && boost_file_trusted(sb.st_uid, sb.st_mode)) n = read(fd, buf, sizeof buf);
    close(fd);
    return n > 0 ? boost_parse(buf, (size_t)n) : BOOST_UNSET;
}

int main(int argc, char **argv)
{
    const char *path = "/Library/NullMoth/gpuboost";
    int held = 0;
    if (argc >= 3 && !strcmp(argv[1], "--once")) {
        if (rm_up(0)) return 2;
        int on = !strcmp(argv[2], "on");
        if (!on && strcmp(argv[2], "off")) { fprintf(stderr, "usage: nvmtl-boost --once on|off\n"); return 2; }
        int rc = boost_send(on);
        if (on && rc == 0) pause();   /* the request lives as long as this client */
        return rc ? 3 : 0;
    }
    if (argc > 1) path = argv[1];
    signal(SIGTERM, onsig); signal(SIGINT, onsig);
    if (rm_up(0)) return 2;
    while (!gStop) {
        enum boost_action a = boost_decide(read_setting(path), held);
        if (a == BOOST_HOLD && boost_send(1) == 0) held = 1;
        else if (a == BOOST_RELEASE && boost_send(0) == 0) held = 0;
        sleep(2);
    }
    if (held) boost_send(0);
    return 0;
}
