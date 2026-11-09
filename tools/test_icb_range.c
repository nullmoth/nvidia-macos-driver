/* test_icb_range.c - GPU-chosen execution ranges for indirect command buffers (plugin/nvmtl_icb_range.h).
   cc -I plugin tools/test_icb_range.c -o /tmp/test_icb_range && /tmp/test_icb_range
   The SPIR-V is validated separately: spirv-val --target-env vulkan1.2 on the words of nvmtl_icb_mask_spv. */
#include <stdio.h>
#include <string.h>
#include "nvmtl_icb_range.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
static void fill(uint32_t *w, uint32_t slots) { for (uint32_t i = 0; i < slots; i++) nvmtl_icb_record_draw(w + i * NVMTL_ICB_RECORD_WORDS, 100 + i, 3, 7, 9); }
int main(void)
{
    uint32_t w[8 * NVMTL_ICB_RECORD_WORDS];
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 2, 3);
    int in_ok = 1, out_ok = 1;
    for (uint32_t i = 0; i < 8; i++) {
        const uint32_t *r = w + i * NVMTL_ICB_RECORD_WORDS; const int in = i >= 2 && i < 5;
        if (in && !(r[0] == 100 + i && r[1] == 3)) in_ok = 0;
        if (!in && (r[0] || r[1])) out_ok = 0;
    }
    arm("slots inside the GPU's range keep their draw counts", in_ok);
    arm("slots outside the range (before and after) are emptied", out_ok);
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 2, 3);
    arm("masking leaves the start and base-instance words alone", w[0 * 5 + 2] == 7 && w[0 * 5 + 3] == 9 && w[6 * 5 + 2] == 7);
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 0, 0);
    int all = 1; for (uint32_t i = 0; i < 8; i++) all &= w[i * 5] == 0 && w[i * 5 + 1] == 0;
    arm("a zero-length range empties every slot", all);
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 0, 8);
    all = 1; for (uint32_t i = 0; i < 8; i++) all &= w[i * 5] == 100 + i;
    arm("a range covering the whole buffer empties none", all);
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 6, 1000);
    arm("a range running past the last slot is clamped by the slot count, not read out of bounds", w[5 * 5] == 0 && w[6 * 5] == 106 && w[7 * 5] == 107);
    fill(w, 8); nvmtl_icb_mask_ref(w, 8, NVMTL_ICB_ZERO_DRAW, 0xFFFFFFFFu, 2);
    arm("location + length overflowing 32 bits does not wrap into the buffer", w[0] == 0 && w[7 * 5] == 0);
    uint32_t d[5]; nvmtl_icb_record_dispatch(d, 4, 5, 6); uint32_t dd[10]; memcpy(dd, d, sizeof d); memcpy(dd + 5, d, sizeof d);
    nvmtl_icb_mask_ref(dd, 2, NVMTL_ICB_ZERO_DISPATCH, 1, 1);
    arm("a masked dispatch has all three group counts zeroed", dd[0] == 0 && dd[1] == 0 && dd[2] == 0 && dd[5] == 4 && dd[6] == 5 && dd[7] == 6);
    uint32_t r[5]; nvmtl_icb_record_draw_indexed(r, 36, 2, -4, 11);
    arm("an indexed record keeps the signed base vertex in word 3 and base instance in word 4", (int32_t)r[3] == -4 && r[4] == 11 && r[2] == 0);
    arm("record sizes: five words, twenty bytes", NVMTL_ICB_RECORD_BYTES == 20u && sizeof(nvmtl_icb_mask_args) == 32);
    arm("groups of 64: 1 -> 1, 64 -> 1, 65 -> 2", nvmtl_icb_mask_groups(1) == 1 && nvmtl_icb_mask_groups(64) == 1 && nvmtl_icb_mask_groups(65) == 2);
    arm("an empty or oversized ICB has no scratch records", nvmtl_icb_slots(0) == 0 && nvmtl_icb_slots(NVMTL_ICB_MAX_SLOTS + 1) == 0 && nvmtl_icb_slots(512) == 512);
    arm("the compute pass starts with the SPIR-V magic and is whole words", nvmtl_icb_mask_spv[0] == 0x07230203u && sizeof nvmtl_icb_mask_spv % 4 == 0);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
