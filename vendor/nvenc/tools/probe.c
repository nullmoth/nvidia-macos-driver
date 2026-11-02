// probe.c: what this GPU offers for a user-space NVENC: engines, and the channel / video / memory classes.
//   ./probe [-v]
#include "rm.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "ctrl/ctrl0080/ctrl0080gpu.h"
#include "ctrl/ctrl2080/ctrl2080gpu.h"

static const char *classname(NvU32 c)
{
    switch (c & 0xff) {
    case 0x6f: return "channel GPFIFO";  case 0xb7: return "video encoder (NVENC)"; case 0xb0: return "video decoder (NVDEC)";
    case 0xb5: return "copy engine";     case 0xc0: return "compute";              case 0x97: return "3D";
    case 0xd1: return "NVJPG";           case 0xfa: return "OFA";                  case 0x61: return "usermode (doorbell)";
    case 0xf1: return "VA space";        default: return NULL;
    }
}

int main(int argc, char **argv)
{
    Rm rm = { .verbose = argc > 1 && !strcmp(argv[1], "-v") };
    if (rm_open(&rm)) return 1;
    printf("GPU %04x:%02x:%02x.%x, device id 0x%04x\n", rm.card.pci_info.domain, rm.card.pci_info.bus, rm.card.pci_info.slot,
           rm.card.pci_info.function, rm.card.pci_info.device_id);
    static NV2080_CTRL_GPU_GET_ENGINES_V2_PARAMS eng; memset(&eng, 0, sizeof eng);
    if (rm_ctrl(&rm, rm.subdevice, NV2080_CTRL_CMD_GPU_GET_ENGINES_V2, &eng, sizeof eng) == NV_OK) {
        printf("engines (%u):", eng.engineCount);
        for (NvU32 i = 0; i < eng.engineCount; i++) printf(" 0x%x", eng.engineList[i]);
        printf("\n");
    }
    static NV0080_CTRL_GPU_GET_CLASSLIST_V2_PARAMS cl; memset(&cl, 0, sizeof cl);
    if (rm_ctrl(&rm, rm.device, NV0080_CTRL_CMD_GPU_GET_CLASSLIST_V2, &cl, sizeof cl) == NV_OK) {
        printf("classes (%u), interesting ones:\n", cl.numClasses);
        for (NvU32 i = 0; i < cl.numClasses; i++) {
            NvU32 c = cl.classList[i]; const char *n = classname(c);
            if (n && c >= 0x9000) printf("  0x%04x  %s\n", c, n);
        }
        printf("all:"); for (NvU32 i = 0; i < cl.numClasses; i++) printf(" %04x", cl.classList[i]); printf("\n");
    }
    rm_close(&rm);
    return 0;
}
