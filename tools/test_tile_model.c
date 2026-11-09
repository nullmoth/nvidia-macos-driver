/* test_tile_model.c - the tile-function semantics the compute emulation must reproduce, with the numbers measured on an Apple GPU by
   tools/tile_oracle/tile_oracle.m. The model applies the tile kernels of that oracle to every pixel and sample of a 70x40 area cut
   into 16x16 tiles (partial tiles at the right and bottom), the way the emulation dispatches one threadgroup per tile.
   cc tools/test_tile_model.c -o /tmp/test_tile_model && /tmp/test_tile_model */
#include <stdio.h>
#include <math.h>
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
enum { W = 70, H = 40, TW = 16, TH = 16, SAMPLES = 4 };
static float red[H][W], green[H][W], blue[H][W], d[H][W];
static float msred[H][W][SAMPLES];
int main(void)
{
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        red[y][x] = 0.25f; green[y][x] = 0.5f; blue[y][x] = 0.75f; d[y][x] = 2.0f;
        for (int s = 0; s < SAMPLES; s++) msred[y][x][s] = 0.25f;
    }
    int groups = 0, covered = 0;
    for (int ty = 0; ty < (H + TH - 1) / TH; ty++) for (int tx = 0; tx < (W + TW - 1) / TW; tx++) {
        groups++;
        for (int ly = 0; ly < TH; ly++) for (int lx = 0; lx < TW; lx++) {
            const int x = tx * TW + lx, y = ty * TH + ly;
            if (x >= W || y >= H) continue;   /* the emulation's image access past the attachment is a no-op */
            covered++;
            red[y][x] *= 2.0f; d[y][x] += 1.0f;                                   /* tile_k, g = 2 */
            for (int s = 0; s < SAMPLES; s++) msred[y][x][s] += 0.125f * (float)s; /* tile_ms */
        }
    }
    int ok1 = 1, ok2 = 1;
    for (int y = 0; y < H; y += 7) for (int x = 0; x < W; x += 9) {
        if (red[y][x] != 0.5f || green[y][x] != 0.5f || blue[y][x] != 0.75f || d[y][x] != 3.0f) ok1 = 0;
        float r = 0; for (int s = 0; s < SAMPLES; s++) r += msred[y][x][s]; r /= SAMPLES;
        if (fabsf(r - 0.4375f) > 1e-6f) ok2 = 0;
    }
    arm("a 70x40 area needs 5x3 = 15 threadgroups of 16x16 tiles", groups == 15);
    arm("every pixel is visited exactly once, partial tiles included", covered == W * H);
    arm("the oracle's pixel arm: red x 2 = 0.5, green and blue untouched, d + 1 = 3.0 at every sampled pixel", ok1);
    arm("the oracle's MSAA arm: per-sample red = 0.25 + 0.125 * sample, resolved 0.4375", ok2);
    arm("sample 3 alone holds 0.625", fabsf(msred[10][10][3] - 0.625f) < 1e-6f);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
