// rcerr.c: RM's robust-channel error log (NV2080_CTRL_CMD_RC_GET_ERROR_COUNT / _GET_ERROR_V2), records as hex.
//   ./rcerr [-v] [last N]          prints the newest N records (default 2); -c clears the history afterwards
#include "rm.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "ctrl/ctrl2080/ctrl2080rc.h"

int main(int argc, char **argv)
{
    Rm rm = { 0 }; int ai = 1, clear = 0;
    for (; ai < argc && argv[ai][0] == '-'; ai++) { if (!strcmp(argv[ai], "-v")) rm.verbose = true; if (!strcmp(argv[ai], "-c")) clear = 1; }
    int last = ai < argc ? atoi(argv[ai]) : 2;
    if (rm_open(&rm)) return 1;
    NV2080_CTRL_RC_GET_ERROR_COUNT_PARAMS cnt = { 0 };
    NV_STATUS st = rm_ctrl(&rm, rm.subdevice, NV2080_CTRL_CMD_RC_GET_ERROR_COUNT, &cnt, sizeof cnt);
    printf("RC_GET_ERROR_COUNT -> 0x%x, %u errors\n", st, cnt.errorCount);
    static NV2080_CTRL_RC_GET_ERROR_V2_PARAMS e;
    for (NvU32 i = cnt.errorCount > (NvU32)last ? cnt.errorCount - (NvU32)last : 0; i < cnt.errorCount; i++) {
        memset(&e, 0, sizeof e); e.whichBuffer = i;
        st = rm_ctrl(&rm, rm.subdevice, NV2080_CTRL_CMD_RC_GET_ERROR_V2, &e, sizeof e);
        printf("record %u -> 0x%x, %u bytes\n", i, st, e.outputRecordSize);
        NvU32 n = e.outputRecordSize < sizeof e.recordBuffer ? e.outputRecordSize : sizeof e.recordBuffer;
        for (NvU32 k = 0; k < n; k += 4) {
            NvU32 w; memcpy(&w, e.recordBuffer + k, 4);
            printf("%s%08x", k % 32 ? " " : (k ? "\n  " : "  "), w);
        }
        printf("\n");
    }
    if (clear) printf("clean history -> 0x%x\n", rm_ctrl(&rm, rm.subdevice, NV2080_CTRL_CMD_RC_SET_CLEAN_ERROR_HISTORY, NULL, 0));
    rm_close(&rm);
    return 0;
}
