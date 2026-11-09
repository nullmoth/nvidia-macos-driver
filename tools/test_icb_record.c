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
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 0, 0, o);
    arm("an armed draw of the wanted pipeline inside the range keeps its four counts", o[0] == 36 && o[1] == 2 && o[2] == 5 && o[3] == 9);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso + 1, 3, 0, 10, NULL, 0, 0, o);
    arm("the same draw is emptied for any other pipeline", !o[0] && !o[1]);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 4, 10, NULL, 0, 0, o);
    arm("a slot below the range is emptied", !o[0]);
    nvmtl_icb_gather_ref(&r, 14, 0, r.pso, 3, 4, 10, NULL, 0, 0, o);
    arm("a slot past the range is emptied (4 + 10 = 14 is outside)", !o[0]);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 4, 0, 10, NULL, 0, 0, o);
    arm("a draw of another primitive type is emptied for this type's candidate", !o[0]);
    r.flags = 0; nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 0, 0, o);
    arm("a reset slot is emptied", !o[0]);
    r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_INDEXED; nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 0, 0, o);
    arm("an indexed draw is emptied for the plain-draw candidate", !o[0]);
    { uint16_t idx16[4] = { 7, 8, 9, 65535 }; uint32_t idx32[3] = { 100000, 5, 70000 }; uint32_t blk[8];
      r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_INDEXED; r.count = 4; r.index = (uintptr_t)idx16; r.base_vertex = -3; r.base_instance = 9;
      nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 1, 8, o);
      arm("an indexed draw under the cap keeps its counts, starts at slot*cap and keeps the signed base vertex", o[0] == 4 && o[1] == 2 && o[2] == 24 && (int32_t)o[3] == -3 && o[4] == 9);
      arm("the index copy widens 16-bit indices and keeps 65535 intact", nvmtl_icb_index_copy_ref(&r, 3, 8, 0, 10, blk) == 4 && blk[0] == 7 && blk[3] == 65535);
      r.flags |= NVMTL_ICB_REC_INDEX32; r.count = 3; r.index = (uintptr_t)idx32;
      arm("the index copy keeps 32-bit indices above 65535", nvmtl_icb_index_copy_ref(&r, 3, 8, 0, 10, blk) == 3 && blk[0] == 100000 && blk[2] == 70000);
      r.count = 9; nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 1, 8, o);
      arm("a command with more indices than the cap is skipped whole, not truncated (draw emptied)", !o[0] && !o[1]);
      arm("and its indices are not copied", nvmtl_icb_index_copy_ref(&r, 3, 8, 0, 10, blk) == 0);
      r.count = 4; r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_INDEXED; r.index = (uintptr_t)idx16;
      nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 0, 8, o);
      arm("the plain-draw candidate does not take an indexed command", !o[0]);
      nvmtl_icb_gather_ref(&r, 12, 0, r.pso, 3, 0, 10, NULL, 1, 8, o);
      arm("an indexed command outside the range is emptied and not copied", !o[0] && nvmtl_icb_index_copy_ref(&r, 12, 8, 0, 10, blk) == 0);
      arm("the scratch index budget shrinks the cap: 1000 slots fit 65536, 100000 slots get 655", nvmtl_icb_index_cap(1000, 65536) == 65536u && nvmtl_icb_index_cap(100000, 65536) == 671u);
      arm("an impossible budget gives cap 0 (the executor refuses and logs)", nvmtl_icb_index_cap(1u << 30, 65536) == 0);
      r.flags = NVMTL_ICB_REC_ARMED; r.count = 36; r.type = 3; }
    r.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_COMPUTE; r.grid[0] = 4; r.grid[1] = 5; r.grid[2] = 6;
    nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, NULL, 0, 0, o);
    arm("an armed dispatch for a compute pass keeps its three group counts", o[0] == 4 && o[1] == 5 && o[2] == 6);
    nvmtl_icb_gather_ref(&r, 3, 0, r.pso, 3, 0, 10, NULL, 0, 0, o);
    { const uint32_t want[3] = { 8, 1, 1 }, other[3] = { 16, 1, 1 }; r.threadgroup[0] = 8; r.threadgroup[1] = 1; r.threadgroup[2] = 1;
      nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, want, 0, 0, o);
      arm("a dispatch with the candidate's threadgroup size keeps its group counts", o[0] == 4 && o[1] == 5);
      nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, other, 0, 0, o);
      arm("a dispatch recorded with another threadgroup size is emptied for this candidate", !o[0] && !o[1] && !o[2]); }
    arm("a dispatch is emptied in a render pass, and a draw in a compute pass", !o[0] && !o[1] && !o[2]);
    r.flags = NVMTL_ICB_REC_ARMED; nvmtl_icb_gather_ref(&r, 3, 1, r.pso, 3, 0, 10, NULL, 0, 0, o);
    arm("a draw is emptied when the compute candidates are gathered", !o[0]);
    r.pso = 0; nvmtl_icb_gather_ref(&r, 3, 0, 0, 3, 0, 10, NULL, 0, 0, o);
    arm("a command that set no pipeline (inherited) matches pipeline 0", o[0] == 36);
    arm("the SPIR-V has its magic and push constants are 80 bytes", nvmtl_icb_gather_spv[0] == 0x07230203u && sizeof(nvmtl_icb_gather_args) == 80);
    {   /* per-command buffers: the table patch over a slot's table copy */
        uint64_t table[64]; for (int i = 0; i < 64; i++) table[i] = 0x1000 + i;   /* the inherited bindings, then self-pointers and sizes after 31 */
        nvmtl_icb_record rec; memset(&rec, 0, sizeof rec);
        rec.buffers[0] = 0xAAAA; rec.buffers[5] = 0xBBBB; rec.buffers[30] = 0xCCCC; rec.fragment_buffers[2] = 0xDDDD;
        nvmtl_icb_table_patch_ref(table, &rec, 0);
        arm("a vertex table takes the addresses the command recorded at their buffer indices", table[0] == 0xAAAA && table[5] == 0xBBBB && table[30] == 0xCCCC);
        arm("an index the command did not set keeps the inherited address", table[1] == 0x1001 && table[29] == 0x101d);
        arm("entries past the 31 buffers (self pointers, texture sizes) are never touched", table[31] == 0x101f && table[40] == 0x1028 && table[63] == 0x103f);
        arm("the vertex patch does not take the fragment record's entries", table[2] == 0x1002);
        nvmtl_icb_table_patch_ref(table, &rec, 1);
        arm("the fragment patch takes the fragment record's entries", table[2] == 0xDDDD);
        arm("the record's tables are 8-byte entries at bytes 80 and 328, 31 each, matching the shader's word offsets 20 and 82",
            offsetof(nvmtl_icb_record, buffers) / 4 == 20 && offsetof(nvmtl_icb_record, fragment_buffers) / 4 == 82 && sizeof rec.buffers == 31 * 8);
        {   /* a GPU-encoded dispatch: its kernel buffers are the set-0 table, the fragment table plays no part */
            uint64_t k0[64], k1[64]; for (int i = 0; i < 64; i++) k0[i] = k1[i] = 0x2000 + i;
            nvmtl_icb_record kr; memset(&kr, 0, sizeof kr); kr.flags = NVMTL_ICB_REC_ARMED | NVMTL_ICB_REC_COMPUTE; kr.buffers[0] = 0xE1; kr.buffers[3] = 0xE4;
            nvmtl_icb_table_patch_ref(k0, &kr, 0); nvmtl_icb_table_patch_ref(k1, &kr, 1);
            arm("a dispatch's recorded kernel buffers replace the inherited entries of the set-0 table", k0[0] == 0xE1 && k0[3] == 0xE4 && k0[1] == 0x2001);
            arm("the set-1 table of a dispatch stays exactly the inherited one (no fragment entries were recorded)", k1[0] == 0x2000 && k1[3] == 0x2003);
        }
        arm("a patch pass of 1000 slots needs ceil(31000/64) = 485 groups", nvmtl_icb_patch_groups(1000) == 485);
        arm("the largest patch fits the 65535-group dispatch limit", nvmtl_icb_patch_groups(NVMTL_ICB_MAX_PATCH_SLOTS) <= 65535u);
        arm("push constants are 24 bytes and the SPIR-V has its magic", sizeof(nvmtl_icb_patch_args) == 24 && nvmtl_icb_patch_spv[0] == 0x07230203u);
    }
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
