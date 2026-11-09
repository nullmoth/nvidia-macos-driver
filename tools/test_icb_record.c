/* test_icb_record.c - the GPU-written ICB record and its gather rule (plugin/nvmtl_icb_record.h).
   cc -I plugin tools/test_icb_record.c -o /tmp/test_icb_record && /tmp/test_icb_record */
#include <stdio.h>
#include "nvmtl_icb_record.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    nvmtl_icb_record r; memset(&r, 0, sizeof r);
    r.flags = NVMTL_ICB_REC_ARMED; r.type = 3; r.count = 36; r.instances = 2; r.start = 5; r.base_instance = 9; r.pso = 0xAB00000042ull;
    uint32_t o[5];
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, o);
    arm("an armed draw of the wanted pipeline inside the range keeps its four counts", o[0] == 36 && o[1] == 2 && o[2] == 5 && o[3] == 9);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso + 1, 3, 0, 10, o);
    arm("the same draw is emptied for any other pipeline", !o[0] && !o[1]);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 4, 10, o);
    arm("a slot below the range is emptied", !o[0]);
    nvmtl_icb_gather_ref(&r, 14, 0, r.pso, 3, 4, 10, o);
    arm("a slot past the range is emptied (4 + 10 = 14 is outside)", !o[0]);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 4, 0, 10, o);
    arm("a draw of another primitive type is emptied for this type's candidate", !o[0]);
    r.flags = 0; nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, o);
    arm("a reset slot is emptied", !o[0]);
    r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_INDEXED; nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, o);
    arm("an indexed draw is emptied (its index buffer cannot be bound from a GPU address)", !o[0]);
    r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_COMPUTE; r.grid[0] = 4; r.grid[1] = 5; r.grid[2] = 6;
    nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, o);
    arm("an armed dispatch for a compute pass keeps its three group counts", o[0] == 4 && o[1] == 5 && o[2] == 6);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, o);
    arm("a dispatch is emptied in a render pass, and a draw in a compute pass", !o[0] && !o[1] && !o[2]);
    r.flags = NVMTL_ICB_REC_ARMED; nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, o);
    arm("a draw is emptied when the compute candidates are gathered", !o[0]);
    r.pso = 0; nvmtl_icb_gather_ref(&r, 3, 0, 0, 3, 0, 10, o);
    arm("a command that set no pipeline (inherited) matches pipeline 0", o[0] == 36);
    arm("the SPIR-V has its magic and push constants are 56 bytes", nvmtl_icb_gather_spv[0] == 0x07230203u && sizeof(nvmtl_icb_gather_args) == 56);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
