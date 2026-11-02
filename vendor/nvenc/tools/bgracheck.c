// bgracheck.c: BGRA input path (enc_load_bgra): converts a synthetic BGRA frame, encodes it, writes the stream and the
// expected NV12 (same BT.709 formula, pitch-linear) for h264check, and times the conversion.
//   ./bgracheck W H out.h264 expected.nv12
#include "enc.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

int main(int argc, char **argv)
{
    if (argc < 5) { fprintf(stderr, "usage: bgracheck W H out.h264 expected.nv12\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]), CH = (H + 1) / 2;
    size_t stride = (size_t)W * 4 + 64;
    uint8_t *bgra = malloc(stride * H), *y = malloc((size_t)W * H), *uv = malloc((size_t)W * CH);
    for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) {
        uint8_t *p = bgra + r * stride + 4 * c;
        p[0] = (uint8_t)((c * 255) / W); p[1] = (uint8_t)((r * 255) / H); p[2] = (uint8_t)((((c >> 5) ^ (r >> 5)) & 1) ? 230 : 40); p[3] = 255;
    }
    for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) {
        const uint8_t *p = bgra + r * stride + 4 * c;
        y[r * W + c] = (uint8_t)(16 + ((47 * p[2] + 157 * p[1] + 16 * p[0] + 128) >> 8));
    }
    for (int r = 0; r < CH; r++) for (int c = 0; c < W; c += 2) {
        int R = 0, G = 0, B = 0;
        for (int dy = 0; dy < 2; dy++) for (int dx = 0; dx < 2; dx++) {
            int rr = 2 * r + dy < H ? 2 * r + dy : H - 1, cc = c + dx < W ? c + dx : W - 1;
            const uint8_t *p = bgra + rr * stride + 4 * cc; R += p[2]; G += p[1]; B += p[0];
        }
        uv[r * W + c] = (uint8_t)(128 + ((-26 * R - 87 * G + 112 * B + 512) >> 10));
        uv[r * W + c + 1] = (uint8_t)(128 + ((112 * R - 102 * G - 10 * B + 512) >> 10));
    }
    FILE *f = fopen(argv[4], "wb"); fwrite(y, 1, (size_t)W * H, f); fwrite(uv, 1, (size_t)W * CH, f); fclose(f);
    Enc e; EncConfig cfg = { .width = W, .height = H, .qp_i = 20, .qp_p = 20, .num_refs = 1 };
    if (enc_open(&e, &cfg)) return 1;
    double best = 1e9, sum = 0; int n = 20;
    for (int i = 0; i < n; i++) { double t0 = now_ms(); enc_load_bgra(&e, bgra, stride); double t = now_ms() - t0; sum += t; if (t < best) best = t; }
    EncFrame fr; int rc = enc_encode(&e, true, &fr);
    printf("%dx%d BGRA -> NV12 tiled: %.3f ms avg, %.3f best; encode %s, %u bits\n", W, H, sum / n, best, rc ? "FAILED" : "ok", fr.bits);
    if (!rc) { uint8_t hdr[256]; size_t hn = enc_headers(&e, hdr); f = fopen(argv[3], "wb"); fwrite(hdr, 1, hn, f); fwrite(fr.data, 1, fr.size, f); fclose(f); }
    enc_close(&e);
    return rc;
}
