// la.h: lookahead analysis as NVIDIA's driver does it (docs/traces/quality-linux vla, T7.09): each frame scaled down 4x
// (box filter, on the CPU here; the driver uses a CUDA kernel) and encoded on NVENC as an H.264 P picture at constant QP
// 40 against the previous scaled frame. The status gives the frame's intra and inter complexity (sums of the blocks' best
// intra / inter SATD), from which the encoder's lookahead hints (ab_beta, prev_act) and scene cuts follow.
#pragma once
#include "enc.h"

typedef struct {
    Enc e;
    int srcW, srcH;                 // source size
    int w, h;                       // scaled size (whole 16x16 blocks: the coded size / 4, rounded up to 16)
    uint8_t *y, *uv;                // scaled NV12 frame
    bool started;
} La;

typedef struct {
    unsigned intra, inter;          // complexity; inter 0 for the first frame (and after la_push(restart))
    unsigned bits;                  // size of the QP 40 picture
    double ms;                      // engine time
} LaStat;

int  la_open(La *l, int width, int height, int engine);   // 0 = ok
void la_close(La *l);
// analyse one frame (NV12, 8-bit); restart: no reference (after a cut the encoder ignores)
int  la_push(La *l, const uint8_t *y, size_t ys, const uint8_t *uv, size_t uvs, bool restart, LaStat *st);
