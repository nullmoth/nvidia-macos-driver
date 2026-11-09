// la.c: lookahead analysis (la.h, T7.09)
#include "la.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int la_open(La *l, int width, int height, int engine)
{
    memset(l, 0, sizeof *l);
    // NVIDIA's sizes: 1920x1080 -> 480x272 (the 1088-row coded height / 4), 3840x2160 -> 960x544
    l->srcW = width; l->srcH = height;
    l->w = (width + 63) / 64 * 16; l->h = (height + 63) / 64 * 16;
    if (!(l->y = malloc((size_t)l->w * l->h * 3 / 2))) return -1;
    l->uv = l->y + (size_t)l->w * l->h;
    // the driver's lookahead setup: H.264, constant QP 40, one reference (the previous scaled frame), CAVLC
    EncConfig c = { .codec = ENC_H264, .width = l->w, .height = l->h, .qp_i = 40, .qp_p = 40, .num_refs = 1, .engine = engine,
                    .depth = 1, .slices = 1, .h264_profile = 77, .la_pass = true };
    if (enc_open(&l->e, &c)) { free(l->y); l->y = NULL; return -1; }
    return 0;
}

void la_close(La *l)
{
    if (l->y) enc_close(&l->e);
    free(l->y); l->y = NULL;
}

// 4x4 box average of one plane (cw x ch samples of `bpp` interleaved components) into a dw x dh plane; edges replicated
static void down4(uint8_t *dst, int dw, int dh, const uint8_t *src, size_t ss, int sw, int sh, int bpp)
{
    for (int r = 0; r < dh; r++) {
        const uint8_t *row[4];
        for (int k = 0; k < 4; k++) { int sr = 4 * r + k; row[k] = src + (size_t)(sr < sh ? sr : sh - 1) * ss; }
        uint8_t *d = dst + (size_t)r * dw * bpp;
        for (int c = 0; c < dw; c++) {
            for (int p = 0; p < bpp; p++) {
                unsigned s = 0;
                for (int k = 0; k < 4; k++)
                    for (int j = 0; j < 4; j++) { int sc = 4 * c + j; s += row[k][(size_t)(sc < sw ? sc : sw - 1) * bpp + p]; }
                d[c * bpp + p] = (uint8_t)((s + 8) >> 4);
            }
        }
    }
}

int la_push(La *l, const uint8_t *y, size_t ys, const uint8_t *uv, size_t uvs, bool restart, LaStat *st)
{
    const int sw = l->srcW, sh = l->srcH;   // smaller than 4x the scaled size: the edges replicate
    down4(l->y, l->w, l->h, y, ys, sw, sh, 1);
    down4(l->uv, l->w / 2, l->h / 2, uv, uvs, sw / 2, sh / 2, 2);
    enc_load_nv12(&l->e, l->y, (size_t)l->w, l->uv, (size_t)l->w);
    EncFrame f;
    int rc = enc_encode(&l->e, restart || !l->started, &f);
    l->started = rc == 0;
    if (st) { st->intra = f.intra_cost; st->inter = f.idr ? 0 : f.inter_cost; st->bits = f.bits; st->ms = f.ms; }
    return rc;
}
