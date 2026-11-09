/* test_yuv422.c - the packed 4:2:2 formats (plugin/nvmtl_yuv422.h), checked against values measured on an Apple GPU.
   cc -I plugin tools/test_yuv422.c -o /tmp/test_yuv422 && /tmp/test_yuv422 */
#include <stdio.h>
#include <string.h>
#include "nvmtl_yuv422.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
static int rows_equal(const uint8_t *got, const uint8_t want[][3], int n)
{
    for (int x = 0; x < n; x++) for (int c = 0; c < 3; c++) if (got[4 * x + c] != want[x][c]) return 0;
    return 1;
}
int main(void)
{
    /* the same four pairs the measurement wrote: bytes 10 100 20 200 | 30 120 40 220 | 50 140 60 240 | 70 160 80 250 */
    const uint8_t packed[16] = { 10, 100, 20, 200, 30, 120, 40, 220, 50, 140, 60, 240, 70, 160, 80, 250 };
    /* measured sample results, R G B per pixel x = 0..7 */
    const uint8_t gbgr[8][3] = { { 200, 10, 100 }, { 205, 20, 105 }, { 215, 30, 115 }, { 225, 40, 125 },
                                 { 235, 50, 135 }, { 243, 60, 145 }, { 248, 70, 155 }, { 250, 80, 160 } };
    const uint8_t bgrg[8][3] = { { 20, 100, 10 }, { 25, 200, 15 }, { 35, 120, 25 }, { 45, 220, 35 },
                                 { 55, 140, 45 }, { 65, 240, 55 }, { 75, 160, 65 }, { 80, 250, 70 } };
    uint8_t out[32];
    nvmtl_y422_decode_row(NVMTL_PF_GBGR422, packed, 8, out);
    arm("GBGR422 decodes the eight measured pixels exactly (bytes G B G R)", rows_equal(out, gbgr, 8));
    nvmtl_y422_decode_row(NVMTL_PF_BGRG422, packed, 8, out);
    arm("BGRG422 decodes the eight measured pixels exactly (bytes B G R G)", rows_equal(out, bgrg, 8));
    nvmtl_y422_decode_row(NVMTL_PF_GBGR422, packed, 8, out);
    int alpha = 1; for (int x = 0; x < 8; x++) alpha &= out[4 * x + 3] == 255;
    arm("alpha reads 1", alpha);
    arm("pixel 0 takes its own pair's chroma at the left edge (clamped, no read before the row)", out[0] == 200 && out[2] == 100);
    arm("pixel 7 takes its own pair's chroma at the right edge (clamped, no read past the row)", out[28] == 250 && out[30] == 160);

    const uint8_t flat[8] = { 7, 90, 9, 180, 7, 90, 9, 180 };   /* two equal pairs: every pixel of the same luma has the same chroma */
    nvmtl_y422_decode_row(NVMTL_PF_GBGR422, flat, 4, out);
    arm("equal neighbouring pairs give their chroma unchanged at every pixel", out[0] == 180 && out[4] == 180 && out[8] == 180 && out[12] == 180
        && out[2] == 90 && out[6] == 90);
    arm("rounding is half up: 3*240+250 = 970, +2 = 972, >>2 = 243", nvmtl_y422_mix(240, 250) == 243 && nvmtl_y422_mix(240, 240) == 240);
    arm("a 255 mix never overflows a byte", nvmtl_y422_mix(255, 255) == 255 && nvmtl_y422_mix(0, 255) == 64);
    arm("a row of w pixels packs into 2w bytes", nvmtl_y422_row_bytes(8) == 16 && nvmtl_y422_row_bytes(1920) == 3840);
    arm("only whole pairs are writable regions", nvmtl_y422_region_ok(0, 8) && nvmtl_y422_region_ok(2, 2) && !nvmtl_y422_region_ok(1, 2) && !nvmtl_y422_region_ok(0, 3));
    arm("only formats 240 and 241 are 4:2:2", nvmtl_y422_is_format(240) && nvmtl_y422_is_format(241) && !nvmtl_y422_is_format(70) && !nvmtl_y422_is_format(242));
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
