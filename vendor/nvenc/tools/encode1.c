// encode1.c (milestone M2): encode one H.264 IDR frame on NVENC from a synthetic NV12 picture, through src/enc.c.
//   ./encode1 [-v] [-hevc] W H QP out.h264|out.h265 out.nv12
// Writes an Annex B stream (our SPS + PPS + the slice NAL NVENC produced) and the source frame (tightly packed NV12)
// for h264check. The M2 experiment version (NVENC_VAR knobs, NVENC_REPLAY) is commit d56225c.
#include "enc.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void pattern(uint8_t *y, uint8_t *uv, int W, int H)
{
    for (int r = 0; r < H; r++)
        for (int c = 0; c < W; c++) {
            int v = ((c + r) & 255) / 2 + ((((c >> 6) ^ (r >> 6)) & 1) ? 90 : 0);
            if (abs(c - W / 3) < 16) v = 235;
            y[r * W + c] = (uint8_t)(16 + v * 219 / 255);
        }
    for (int r = 0; r < (H + 1) / 2; r++)
        for (int c = 0; c < W / 2; c++) {
            uv[r * W + 2 * c] = (uint8_t)(128 + ((c * 3) % 120) - 60);
            uv[r * W + 2 * c + 1] = (uint8_t)(128 + ((r * 2) % 120) - 60);
        }
}

int main(int argc, char **argv)
{
    int ai = 1; bool verbose = false;
    if (argc > 1 && !strcmp(argv[1], "-v")) { verbose = true; ai++; }
    bool hevc = false;
    if (argc > ai && !strcmp(argv[ai], "-hevc")) { hevc = true; ai++; }
    if (argc - ai < 5) { fprintf(stderr, "usage: encode1 [-v] [-hevc] W H QP out.h264|out.h265 out.nv12   (ENC1_FILL=v: flat luma v)\n"); return 2; }
    int W = atoi(argv[ai]), H = atoi(argv[ai + 1]), QP = atoi(argv[ai + 2]), CH = (H + 1) / 2;
    uint8_t *y = malloc((size_t)W * H), *uv = malloc((size_t)W * CH);
    pattern(y, uv, W, H);
    if (getenv("ENC1_BLOCK")) {   // black picture with one bright 16x16 block at block (bx, by) = ENC1_BLOCK "bx,by"
        int bx = 0, by = 0; sscanf(getenv("ENC1_BLOCK"), "%d,%d", &bx, &by);
        memset(y, 16, (size_t)W * H); memset(uv, 128, (size_t)W * CH);
        for (int r = 0; r < 16; r++) for (int c = 0; c < 16; c++)   // ENC1_RAMP: a ramp inside the block (left->right on top, top->bottom below)
            y[(size_t)(by * 16 + r) * W + bx * 16 + c] = getenv("ENC1_RAMP") ? (uint8_t)(r < 8 ? 40 + c * 12 : 40 + r * 12) : 235;
    }
    if (getenv("ENC1_FILL")) { memset(y, atoi(getenv("ENC1_FILL")), (size_t)W * H); memset(uv, 128, (size_t)W * CH); }   // flat test picture
    FILE *f = fopen(argv[ai + 4], "wb"); fwrite(y, 1, (size_t)W * H, f); fwrite(uv, 1, (size_t)W * CH, f); fclose(f);

    Enc e; EncConfig cfg = { .codec = hevc ? ENC_HEVC : ENC_H264, .width = W, .height = H, .qp_i = QP, .qp_p = QP, .num_refs = 1, .verbose = verbose };
    if (enc_open(&e, &cfg)) return 1;
    enc_load_nv12(&e, y, W, uv, W);
    EncFrame fr; int rc = enc_encode(&e, true, &fr);
    printf("%dx%d QP %d: %s, %.3f ms, %u bits, ucode error %u, avgQP %u, intra MBs %u\n", W, H, QP, rc ? "FAILED" : "done", fr.ms,
           fr.bits, fr.ucode_error, fr.avg_qp, fr.intra_mbs);
    if (!rc) {
        uint8_t hdr[256]; size_t n = enc_headers(&e, hdr);
        f = fopen(argv[ai + 3], "wb"); fwrite(hdr, 1, n, f); fwrite(fr.data, 1, fr.size, f); fclose(f);
        printf("wrote %s: %zu header + %zu slice bytes\n", argv[ai + 3], n, fr.size);
    }
    enc_close(&e);
    return rc ? 1 : 0;
}
