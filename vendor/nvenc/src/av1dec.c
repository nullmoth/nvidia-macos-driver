// av1dec.c: see av1dec.h. Section numbers refer to the AV1 specification.
#include "av1dec.h"
#include "blocklinear.h"
#include "av1tables.h"
#include "av1probs.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "class/clc56f.h"
#include "class/cl2080_notification.h"
#include "clc9b0.h"
#include "nvdec_drv.h"

static NvU32 hi(NvU64 a) { return (NvU32)(a >> 8); }
static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
static int alloc(Rm *rm, RmMem *m, size_t size)
{
    if (rm_mem_alloc(rm, m, (size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) return -1;
    memset(m->cpu, 0, m->size);
    return 0;
}

int av1dec_open(Av1Dec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 127) & ~127u; d->maxH = (maxH + 127) & ~127u;
    // AV1 decoding came with Ampere's NVDEC (GA10x 0xc7b0, Ada 0xc9b0); Turing (0xc4b0) and GA100 (0xc6b0) have none
    const NvU32 cls = rm_class(rm, RM_CLS_NVDEC);
    if (cls != 0xc7b0 && cls != 0xc9b0) { fprintf(stderr, "av1dec: this GPU's NVDEC (class 0x%x) has no AV1\n", cls); return -1; }
    d->bit_depth = bit_depth; d->bps = bit_depth > 8 ? 2 : 1;
    d->pitch = (size_t)d->maxW * (size_t)d->bps;
    for (int i = 0; i < 8; i++) d->ref[i] = -1;
    for (int i = 0; i < AV1_SURFACES; i++) d->twin[i] = -1;
    d->cdf_size = AV1_PROBS_B;
    for (int q = 0; q < 4; q++) { if (!(d->cdf_tab[q] = malloc(AV1_PROBS_B))) return -1; av1_default_probs(q, d->cdf_tab[q]); }
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "av1dec: channel failed\n"); return -1; }
    // filter buffer: the six neighbour areas nvdec_drv.h sizes, one after the other
    const size_t W = d->maxW, H = d->maxH;
    d->flt_top = 0;
    d->flt_vert = d->flt_top + AV1_DBLK_TOP_BUF_SIZE(W);
    d->flt_cdef = d->flt_vert + AV1_DBLK_LEFT_BUF_SIZE(H);
    d->flt_lr = d->flt_cdef + AV1_CDEF_LEFT_BUF_SIZE(H);
    d->flt_hus = d->flt_lr + AV1_LR_LEFT_BUF_SIZE(H);
    d->flt_fgs = d->flt_hus + AV1_HUS_LEFT_BUF_SIZE(H);
    const size_t flt = d->flt_fgs + AV1_FGS_LEFT_BUF_SIZE(H);
    const size_t bsCap = W * H * 2 > (4u << 20) ? W * H * 2 : (4u << 20);
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x2000 }, { &d->bs, bsCap }, { &d->cdf_def, d->cdf_size + 4096 }, { &d->cdf_out, d->cdf_size + 4096 },
        { &d->tileinfo, AV1_TILEINFO_BUF_SIZE }, { &d->tilestream, AV1_TILESTREAMINFO_BUF_SIZE }, { &d->filter, flt },
        { &d->intra, AV1_INTRA_TOP_BUF_SIZE * (size_t)d->bps }, { &d->gm, AV1_GLOBAL_MOTION_BUF_SIZE }, { &d->fg, AV1_FGS_BUF_SIZE },
        { &d->status, 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++) if (alloc(rm, b[i].m, b[i].size)) { fprintf(stderr, "av1dec: alloc failed\n"); return -1; }
    for (int i = 0; i < 8; i++) if (alloc(rm, &d->cdf[i], d->cdf_size + 4096)) return -1;
    if (alloc(rm, &d->zseg, AV1_SEGMENT_ID_BUF_SIZE(W, H))) return -1;
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    for (int i = 0; i < AV1_SURFACES; i++)
        if (alloc(rm, &d->luma[i], luma) || alloc(rm, &d->chroma[i], chroma) ||
            alloc(rm, &d->tmv[i], AV1_TEMPORAL_MV_BUF_SIZE(W, H)) || alloc(rm, &d->seg[i], AV1_SEGMENT_ID_BUF_SIZE(W, H))) {
            fprintf(stderr, "av1dec: surface alloc failed\n"); return -1;
        }
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void av1dec_close(Av1Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->cdf_def, &d->cdf_out, &d->tileinfo, &d->tilestream, &d->filter, &d->intra, &d->gm, &d->fg, &d->status, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < 8; i++) rm_mem_free(d->rm, &d->cdf[i]);
    rm_mem_free(d->rm, &d->zseg);
    for (int i = 0; i < AV1_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); rm_mem_free(d->rm, &d->tmv[i]); rm_mem_free(d->rm, &d->seg[i]); }
    for (int q = 0; q < 4; q++) free(d->cdf_tab[q]);
}

void av1dec_release(Av1Dec *d, int surf) { if (surf >= 0 && surf < AV1_SURFACES) d->busy[surf] = false; }

void av1dec_read_nv12(Av1Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    const size_t b = (size_t)d->bps;
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w * b, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, ((w + 1) & ~1u) * b, (h + 1) / 2, d->pitch);
}

static unsigned sample(const uint8_t *p, size_t x, size_t y, size_t pitch, int bps, int shift)
{
    if (bps == 1) return p[nvenc_in_offset(x, y, pitch)];
    const size_t o = nvenc_in_offset(2 * x, y, pitch);
    return (unsigned)(p[o] | p[o + 1] << 8) >> shift;
}

void av1dec_read_i420(Av1Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
{
    const uint8_t *L = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    const unsigned cw = (w + 1) / 2, ch = (h + 1) / 2;
    const int b = d->bps, sh = b == 2 ? 16 - d->bit_depth : 0;
    #define PUT(i, v) do { if (b == 1) dst[i] = (uint8_t)(v); else { dst[2 * (i)] = (uint8_t)(v); dst[2 * (i) + 1] = (uint8_t)((v) >> 8); } } while (0)
    const size_t U = (size_t)w * h, V = U + (size_t)cw * ch;
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x++) PUT((size_t)y * w + x, sample(L, x, y, d->pitch, b, sh));
    for (unsigned y = 0; y < ch; y++) for (unsigned x = 0; x < cw; x++) {
        PUT(U + (size_t)y * cw + x, sample(C, 2 * x, y, d->pitch, b, sh));
        PUT(V + (size_t)y * cw + x, sample(C, 2 * x + 1, y, d->pitch, b, sh));
    }
    #undef PUT
}

// setup_shear (7.11.3.6): the warp's shear parameters; 0 when the model is not valid for warping
static const int div_lut[257] = {
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768, 15709, 15650, 15592, 15534, 15477, 15420, 15364,
    15308, 15252, 15197, 15142, 15087, 15033, 14980, 14926, 14873, 14821, 14769, 14717, 14665, 14614, 14564, 14513, 14463, 14413,
    14364, 14315, 14266, 14218, 14170, 14122, 14075, 14028, 13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662, 13618, 13574,
    13530, 13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107, 13066, 13026, 12985, 12945, 12906, 12866, 12827,
    12788, 12749, 12710, 12672, 12633, 12596, 12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228, 12193, 12157,
    12122, 12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848, 11815, 11782, 11749, 11716, 11683, 11651, 11619, 11586, 11555,
    11523, 11491, 11460, 11429, 11398, 11367, 11336, 11305, 11275, 11245, 11215, 11185, 11155, 11125, 11096, 11067, 11038, 11009,
    10980, 10951, 10923, 10894, 10866, 10838, 10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538, 10512,
    10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255, 10230, 10205, 10180, 10156, 10131, 10107, 10082, 10058,
    10034, 10010, 9986, 9963, 9939, 9916, 9892, 9869, 9846, 9823, 9800, 9777, 9754, 9732, 9709, 9687, 9664, 9642, 9620, 9598,
    9576, 9554, 9533, 9511, 9489, 9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259, 9239, 9218, 9198, 9178,
    9158, 9138, 9118, 9098, 9079, 9059, 9039, 9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867, 8849, 8830, 8812, 8793,
    8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630, 8613, 8595, 8577, 8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439,
    8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306, 8289, 8273, 8257, 8240, 8224, 8208, 8192 };
static int floor_log2_u64(unsigned long long x) { int s = 0; while (x > 1) { x >>= 1; s++; } return s; }
static long long round2s(long long x, int n) { if (n == 0) return x; return x >= 0 ? (x + (1ll << (n - 1))) >> n : -((-x + (1ll << (n - 1))) >> n); }
static void resolve_divisor(long long dd, int *shift, long long *div)
{
    const long long a = dd < 0 ? -dd : dd;
    const int n = floor_log2_u64((unsigned long long)a);
    const long long e = a - (1ll << n);
    const long long f = n > 8 ? round2s(e, n - 8) : e << (8 - n);
    *shift = n + 14;
    *div = dd < 0 ? -div_lut[f] : div_lut[f];
}
bool av1_shear(const int32_t m[6], int16_t sh[4])
{
    if (m[2] <= 0) return false;
    const long long alpha0 = m[2] - (1 << 16 > 0 ? 1 << 16 : 0), beta0 = m[3];
    int dshift; long long div;
    resolve_divisor(m[2], &dshift, &div);
    const long long v = (long long)m[4] << 16, gamma0 = round2s(v * div, dshift);
    const long long w = (long long)m[3] * m[4], delta0 = m[5] - round2s(w * div, dshift) - (1 << 16);
    #define CL(x) ((x) < -32768 ? -32768 : (x) > 32767 ? 32767 : (x))
    const long long alpha = round2s(CL(alpha0), 6) * 64, beta = round2s(CL(beta0), 6) * 64;
    const long long gamma = round2s(CL(gamma0), 6) * 64, delta = round2s(CL(delta0), 6) * 64;
    #undef CL
    sh[0] = (int16_t)alpha; sh[1] = (int16_t)beta; sh[2] = (int16_t)gamma; sh[3] = (int16_t)delta;
    const long long a4 = alpha < 0 ? -alpha : alpha, b4 = beta < 0 ? -beta : beta, g4 = gamma < 0 ? -gamma : gamma, d4 = delta < 0 ? -delta : delta;
    if (4 * a4 + 7 * b4 >= (1 << 16)) return false;
    if (4 * g4 + 4 * d4 >= (1 << 16)) return false;
    return true;
}

// ---- film grain (7.18.3): the decoder adds the grain itself; the driver gives it the three scaling functions (256
// entries each) and the grain templates cropped to 64x64 luma / 32x32 chroma (AV1FilmGrainMemory; mesa-nova
// av1_film_grain.rs) ----
static int round2i(int x, int n) { return n == 0 ? x : (x + (1 << (n - 1))) >> n; }
static unsigned fg_random(uint16_t *r, int bits)
{
    const uint16_t v = *r, bit = (uint16_t)((v ^ (v >> 1) ^ (v >> 3) ^ (v >> 12)) & 1);
    *r = (uint16_t)((v >> 1) | (bit << 15));
    return (unsigned)(*r >> (16 - bits)) & ((1u << bits) - 1);
}
static void scaling_function(const int *xs, const int *ys, int n, uint8_t lut[256])   // piecewise linear, as libaom
{
    memset(lut, 0, 256);
    if (n <= 0) return;
    for (int x = 0; x < xs[0]; x++) lut[x] = (uint8_t)ys[0];
    for (int p = 0; p + 1 < n; p++) {
        const int dx = xs[p + 1] - xs[p], delta = (ys[p + 1] - ys[p]) * ((65536 + (dx >> 1)) / dx);
        for (int x = 0; x < dx; x++) lut[xs[p] + x] = (uint8_t)(ys[p] + ((x * delta + 32768) >> 16));
    }
    for (int x = xs[n - 1]; x < 256; x++) lut[x] = (uint8_t)ys[n - 1];
}
enum { LUMA_W = 82, LUMA_H = 73, CHROMA_W = 44, CHROMA_H = 38 };
static void chroma_grain(const Av1FilmGrain *g, uint16_t seed, int n, const int *coeffs, const int *luma, int shift, int ar_shift,
                         int gmin, int gmax, int *grain)
{
    memset(grain, 0, sizeof(int) * CHROMA_W * CHROMA_H);
    if (n == 0 && !g->chroma_scaling_from_luma) return;
    uint16_t r = seed;
    for (int i = 0; i < CHROMA_W * CHROMA_H; i++) grain[i] = round2i(Gaussian_Sequence[fg_random(&r, 11)], shift);
    const int lag = g->ar_coeff_lag;
    for (int y = 3; y < CHROMA_H; y++) for (int x = 3; x < CHROMA_W - 3; x++) {
        int sum = 0, pos = 0;
        for (int dy = -lag; dy <= 0; dy++) for (int dx = -lag; dx <= lag; dx++) {
            const int c = coeffs[pos] - 128;
            if (dy == 0 && dx == 0) {
                if (g->num_y_points > 0) {
                    const int ly = ((y - 3) << 1) + 3, lx = ((x - 3) << 1) + 3;
                    const int l = luma[ly * LUMA_W + lx] + luma[ly * LUMA_W + lx + 1] + luma[(ly + 1) * LUMA_W + lx] + luma[(ly + 1) * LUMA_W + lx + 1];
                    sum += round2i(l, 2) * c;
                }
                goto done;
            }
            sum += grain[(y + dy) * CHROMA_W + x + dx] * c; pos++;
        }
    done: {
            const int v = grain[y * CHROMA_W + x] + round2i(sum, ar_shift);
            grain[y * CHROMA_W + x] = v < gmin ? gmin : v > gmax ? gmax : v;
        }
    }
}
static void crop(const int *grain, int w, int x0, int size, int16_t *dst)
{
    for (int y = 0; y < size; y++) for (int x = 0; x < size; x++) dst[y * size + x] = (int16_t)grain[(x0 + y) * w + x0 + x];
}
static void film_grain_setup(const Av1FilmGrain *g, int bd, av1_fgs_cfg_t *cfg, AV1FilmGrainMemory *mem)
{
    const int fromLuma = g->chroma_scaling_from_luma;
    memset(cfg, 0, sizeof *cfg); memset(mem, 0, sizeof *mem);
    cfg->apply_grain = 1; cfg->overlap_flag = (unsigned short)g->overlap_flag;
    cfg->clip_to_restricted_range = (unsigned short)g->clip_to_restricted_range; cfg->chroma_scaling_from_luma = (unsigned short)fromLuma;
    cfg->num_y_points_b = g->num_y_points > 0; cfg->num_cb_points_b = g->num_cb_points > 0; cfg->num_cr_points_b = g->num_cr_points > 0;
    cfg->scaling_shift = (unsigned short)(g->grain_scaling_minus_8 + 8); cfg->sw_random_seed = (unsigned short)g->grain_seed;
    if (fromLuma) { cfg->cb_luma_mult = 64; cfg->cr_luma_mult = 64; }
    else {
        cfg->cb_mult = (char)(g->cb_mult - 128); cfg->cb_luma_mult = (char)(g->cb_luma_mult - 128); cfg->cb_offset = (short)(g->cb_offset - 256);
        cfg->cr_mult = (char)(g->cr_mult - 128); cfg->cr_luma_mult = (char)(g->cr_luma_mult - 128); cfg->cr_offset = (short)(g->cr_offset - 256);
    }
    scaling_function(g->point_y_value, g->point_y_scaling, g->num_y_points, mem->scaling_lut_y);
    if (fromLuma) { memcpy(mem->scaling_lut_cb, mem->scaling_lut_y, 256); memcpy(mem->scaling_lut_cr, mem->scaling_lut_y, 256); }
    else {
        scaling_function(g->point_cb_value, g->point_cb_scaling, g->num_cb_points, mem->scaling_lut_cb);
        scaling_function(g->point_cr_value, g->point_cr_scaling, g->num_cr_points, mem->scaling_lut_cr);
    }
    const int gmin = -(128 << (bd - 8)), gmax = (128 << (bd - 8)) - 1;
    const int shift = 12 - bd + g->grain_scale_shift, ar_shift = g->ar_coeff_shift_minus_6 + 6, lag = g->ar_coeff_lag;
    static int luma[LUMA_W * LUMA_H], cb[CHROMA_W * CHROMA_H], cr[CHROMA_W * CHROMA_H];
    memset(luma, 0, sizeof luma);
    if (g->num_y_points > 0) {
        uint16_t r = (uint16_t)g->grain_seed;
        for (int i = 0; i < LUMA_W * LUMA_H; i++) luma[i] = round2i(Gaussian_Sequence[fg_random(&r, 11)], shift);
        for (int y = 3; y < LUMA_H; y++) for (int x = 3; x < LUMA_W - 3; x++) {
            int sum = 0, pos = 0;
            for (int dy = -lag; dy <= 0; dy++) for (int dx = -lag; dx <= lag; dx++) {
                if (dy == 0 && dx == 0) goto ldone;
                sum += luma[(y + dy) * LUMA_W + x + dx] * (g->ar_coeffs_y_plus_128[pos] - 128); pos++;
            }
        ldone: {
                const int v = luma[y * LUMA_W + x] + round2i(sum, ar_shift);
                luma[y * LUMA_W + x] = v < gmin ? gmin : v > gmax ? gmax : v;
            }
        }
    }
    chroma_grain(g, (uint16_t)(g->grain_seed ^ 0xb524), g->num_cb_points, g->ar_coeffs_cb_plus_128, luma, shift, ar_shift, gmin, gmax, cb);
    chroma_grain(g, (uint16_t)(g->grain_seed ^ 0x49d8), g->num_cr_points, g->ar_coeffs_cr_plus_128, luma, shift, ar_shift, gmin, gmax, cr);
    crop(luma, LUMA_W, 9, 64, mem->cropped_luma_grain_block);
    crop(cb, CHROMA_W, 6, 32, mem->cropped_cb_grain_block);
    crop(cr, CHROMA_W, 6, 32, mem->cropped_cr_grain_block);
}


// superres upscaling parameters of one plane (7.16)
static void superres_steps(int fw, int uw, int sub, unsigned short *step, unsigned short *init)
{
    const int dw = (fw + sub) >> sub, upw = (uw + sub) >> sub;
    const int s = ((dw << 14) + (upw / 2)) / upw;
    const int err = upw * s - (dw << 14);
    int x = (-((upw - dw) << 13) + upw / 2) / upw + (1 << 7) - err / 2;
    *step = (unsigned short)s; *init = (unsigned short)(x & ((1 << 14) - 1));
}

static int decode_frame(Av1Dec *d)
{
    Av1Frame *f = &d->f; const Av1Seq *s = &d->st.seq;
    // an inter frame whose references were never decoded: the stream was joined after its key frame (ffmpeg drops the
    // first sample when its first VideoToolbox session finds no decoder: libnvencvt registers while that session looks
    // for one). Such a frame and the slots it refreshes are left out until a key frame or an intra-only frame refills
    // them, as dav1d does; decoding it would show the frame's residuals on whatever the surface held.
    if (!f->frame_is_intra) {
        bool miss = false;
        for (int i = 0; i < 7; i++) if (d->ref[f->ref_frame_idx[i]] < 0) miss = true;
        if (miss) {
            for (int i = 0; i < 8; i++) if (f->refresh_frame_flags & (1 << i)) d->ref[i] = -1;
            av1_end_frame(&d->st, f);
            d->dec_surf = -1; d->skipped = true; d->nskipped++;
            return 0;
        }
    }
    if ((unsigned)f->upscaled_width > d->maxW || (unsigned)f->frame_height > d->maxH) { fprintf(stderr, "av1dec: frame larger than the session\n"); return -1; }
    // what the engine decodes (T9.03): 4:2:0 or monochrome (written as 4:2:0 with neutral chroma), 8/10-bit, 128x128 up
    // (NVIDIA's minimum; smaller frames came out grey with status 0xb), 256 tiles
    if (!s->subsampling_x || !s->subsampling_y || s->bit_depth > 10 || f->upscaled_width < 128 || f->frame_height < 128 || f->tile_cols * f->tile_rows > 256) {
        fprintf(stderr, "av1dec: %dx%d %s%d-bit is beyond what NVDEC decodes here\n", f->upscaled_width, f->frame_height,
                !s->subsampling_y ? "4:2:2/4:4:4 " : "", s->bit_depth);
        return -1;
    }
    int surf = -1;
    for (int i = 0; i < AV1_SURFACES && surf < 0; i++) {
        bool used = d->busy[i];
        for (int k = 0; k < 8; k++) if (d->ref[k] == i) used = true;
        for (int k = 0; k < AV1_SURFACES; k++) if (d->twin[k] == i) used = true;
        if (!used) surf = i;
    }
    if (surf < 0) { fprintf(stderr, "av1dec: no free surface\n"); return -1; }

    // CDF tables to read: the defaults for this quantizer, or those saved with the primary reference frame
    const bool fresh = f->primary_ref_frame == AV1_PRIMARY_REF_NONE;
    if (fresh) memcpy(d->cdf_def.cpu, d->cdf_tab[av1_q_ctx(f->base_q_idx)], d->cdf_size);
    RmMem *cdf_in = fresh ? &d->cdf_def : &d->cdf[f->ref_frame_idx[f->primary_ref_frame]];
    memset(d->cdf_out.cpu, 0, d->cdf_size);
    // tiles: 16 bytes each in the tile size buffer: width and height in superblocks, then the first and one past the last
    // byte of the tile's data (AV1TileInfo_OLD's layout; mesa-nova av1.rs)
    AV1TileInfo_OLD *ti = d->tileinfo.cpu;
    const int sbShift = s->use_128x128_superblock ? 5 : 4;
    for (int t = 0; t < d->ntiles; t++) {
        const int r = d->tile[t].row, c = d->tile[t].col, k = r * f->tile_cols + c;
        memset(&ti[k], 0, sizeof ti[k]);
        ti[k].width_in_sb = (unsigned char)((f->mi_col_starts[c + 1] - f->mi_col_starts[c] + (1 << sbShift) - 1) >> sbShift);
        ti[k].height_in_sb = (unsigned char)((f->mi_row_starts[r + 1] - f->mi_row_starts[r] + (1 << sbShift) - 1) >> sbShift);
        const unsigned s0 = (unsigned)d->tile[t].start, e0 = (unsigned)d->tile[t].end;
        memcpy(&ti[k].tile_start_b0, &s0, 4); memcpy(&ti[k].tile_end_b0, &e0, 4);
    }

    nvdec_av1_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    ps->stream_len = (unsigned)d->bsN;
    ps->use_128x128_superblock = (unsigned)s->use_128x128_superblock; ps->bit_depth = (unsigned)s->bit_depth;
    ps->chroma_format = s->mono_chrome ? 0 : s->subsampling_x && s->subsampling_y ? 1 : s->subsampling_x ? 2 : 3;   // 0 = monochrome (mesa-nova)
    ps->enable_filter_intra = (unsigned)s->enable_filter_intra; ps->enable_intra_edge_filter = (unsigned)s->enable_intra_edge_filter;
    ps->enable_interintra_compound = (unsigned)s->enable_interintra_compound; ps->enable_masked_compound = (unsigned)s->enable_masked_compound;
    ps->enable_dual_filter = (unsigned)s->enable_dual_filter; ps->reserved10 = (unsigned)s->enable_order_hint;   // "0 - disable order hint"
    ps->enable_jnt_comp = (unsigned)s->enable_jnt_comp;
    // CDEF only when it would do something (mesa-nova)
    const bool cdef_on = s->enable_cdef && !f->coded_lossless && !f->allow_intrabc &&
                         !(f->cdef_bits == 0 && !f->cdef_y_pri[0] && !f->cdef_y_sec[0] && !f->cdef_uv_pri[0] && !f->cdef_uv_sec[0]);
    ps->enable_cdef = cdef_on;
    // film grain: the decoder writes the frame with grain to a display picture, the reference stays without
    int disp = -1;
    // (also for a hidden frame that can be shown later: its show_existing_frame outputs this twin, 7.21 loads the same
    // grain parameters)
    const bool grain = s->film_grain_params_present && f->fg.apply_grain;
    if (grain && (f->show_frame || f->showable_frame)) {
        for (int i = 0; i < AV1_SURFACES && disp < 0; i++) {
            bool used = d->busy[i] || i == surf;
            for (int k = 0; k < AV1_SURFACES; k++) if (d->twin[k] == i) used = true;
            for (int k = 0; k < 8; k++) if (d->ref[k] == i) used = true;
            if (!used) disp = i;
        }
        if (disp < 0) { fprintf(stderr, "av1dec: no free surface for the film grain output\n"); return -1; }
        film_grain_setup(&f->fg, s->bit_depth, &ps->fgs_cfg, d->fg.cpu); ps->enable_fgs = 1;
    }
    ps->frame_type = (unsigned)f->frame_type; ps->disable_cdf_update = (unsigned)f->disable_cdf_update;
    ps->show_frame = 1;   // always 1 (mesa-nova)
    ps->allow_screen_content_tools = (unsigned)f->allow_screen_content_tools; ps->cur_frame_force_integer_mv = (unsigned)f->force_integer_mv;
    ps->scale_denom_minus9 = f->use_superres ? (unsigned)(f->superres_denom - 9) : 0;
    ps->allow_intrabc = (unsigned)f->allow_intrabc; ps->allow_high_precision_mv = (unsigned)f->allow_high_precision_mv;
    ps->interp_filter = (unsigned)f->interpolation_filter; ps->switchable_motion_mode = (unsigned)f->is_motion_mode_switchable;
    ps->use_ref_frame_mvs = (unsigned)f->use_ref_frame_mvs; ps->refresh_frame_context = (unsigned)!f->disable_frame_end_update_cdf;
    ps->delta_q_present_flag = (unsigned)f->delta_q_present; ps->delta_q_res = (unsigned)f->delta_q_res;
    ps->delta_lf_present_flag = (unsigned)f->delta_lf_present; ps->delta_lf_res = (unsigned)f->delta_lf_res; ps->delta_lf_multi = (unsigned)f->delta_lf_multi;
    ps->coded_lossless = (unsigned)f->coded_lossless; ps->tile_enabled = (unsigned)(f->tile_cols * f->tile_rows > 1);
    ps->superres_is_scaled = (unsigned)f->use_superres;
    ps->tile_cols = (unsigned)f->tile_cols; ps->tile_rows = (unsigned)f->tile_rows; ps->context_update_tile_id = (unsigned)f->context_update_tile_id;
    if (cdef_on) { ps->cdef_damping_minus_3 = (unsigned)(f->cdef_damping - 3); ps->cdef_bits = (unsigned)f->cdef_bits; }
    static const unsigned txmode[3] = { 0, 3, 4 };   // "0:ONLY4x4, 3:LARGEST, 4:SELECT"
    ps->frame_tx_mode = txmode[f->tx_mode]; ps->frame_reference_mode = f->reference_select ? 2 : 0;
    ps->skip_mode_flag = (unsigned)f->skip_mode_present;
    ps->skip_ref0 = (unsigned)f->skip_mode_frame[0]; ps->skip_ref1 = (unsigned)f->skip_mode_frame[1];
    ps->allow_warp = (unsigned)f->allow_warped_motion; ps->reduced_tx_set_used = (unsigned)f->reduced_tx_set;
    ps->superres_upscaled_width = (unsigned short)f->upscaled_width;
    ps->superres_luma_step = ps->superres_chroma_step = 14;   // without superres (mesa-nova)
    if (f->use_superres) {
        superres_steps(f->frame_width, f->upscaled_width, 0, &ps->superres_luma_step, &ps->superres_init_luma_subpel_x);
        superres_steps(f->frame_width, f->upscaled_width, s->subsampling_x, &ps->superres_chroma_step, &ps->superres_init_chroma_subpel_x);
    }
    ps->base_qindex = (unsigned char)f->base_q_idx; ps->y_dc_delta_q = (char)f->delta_q_y_dc;
    ps->u_dc_delta_q = (char)f->delta_q_u_dc; ps->v_dc_delta_q = (char)f->delta_q_v_dc; ps->u_ac_delta_q = (char)f->delta_q_u_ac; ps->v_ac_delta_q = (char)f->delta_q_v_ac;
    ps->qm_y = (unsigned char)(f->using_qmatrix ? f->qm_y : 15); ps->qm_u = (unsigned char)(f->using_qmatrix ? f->qm_u : 15); ps->qm_v = (unsigned char)(f->using_qmatrix ? f->qm_v : 15);
    for (int i = 0; i < (cdef_on ? 1 << f->cdef_bits : 0); i++) {
        ps->cdef_y_pri_strength |= (unsigned)f->cdef_y_pri[i] << (4 * i); ps->cdef_uv_pri_strength |= (unsigned)f->cdef_uv_pri[i] << (4 * i);
        ps->cdef_y_sec_strength |= (unsigned)(f->cdef_y_sec[i] == 4 ? 3 : f->cdef_y_sec[i]) << (2 * i);
        ps->cdef_uv_sec_strength |= (unsigned)(f->cdef_uv_sec[i] == 4 ? 3 : f->cdef_uv_sec[i]) << (2 * i);
    }
    ps->segment_enabled = (unsigned char)f->seg_enabled; ps->segment_update_map = (unsigned char)f->seg_update_map;
    ps->segment_temporal_update = (unsigned char)f->seg_temporal_update;
    // the features enabled: alt-q as magnitude with the signs in segment_quant_sign, REF_FRAME plus 1 (0 on key frames),
    // SKIP and GLOBALMV as 1 (mesa-nova)
    if (f->seg_enabled) {
        unsigned sign = 0;
        for (int i = 0; i < 8; i++) for (int j = 0; j < 8; j++) {
            if (!f->seg_feature_enabled[i][j]) continue;
            const int v = f->seg_feature_data[i][j];
            int e = v;
            if (j == 0) { sign |= (unsigned)(v < 0) << i; e = v < 0 ? -v : v; if (e > 255) e = 255; }
            else if (j == 5) e = f->frame_type == AV1_KEY_FRAME ? 0 : v + 1;
            else if (j >= 6) e = 1;
            ps->segment_feature_data[i][j] = (short)e;
            ps->last_active_segid = (unsigned char)i;
            if (j >= 5) ps->segid_preskip = 1;
        }
        ps->segment_quant_sign = sign;
    }
    ps->filter_level[0] = (unsigned char)f->lf_level[0]; ps->filter_level[1] = (unsigned char)f->lf_level[1];
    ps->filter_level_u = (unsigned char)f->lf_level[2]; ps->filter_level_v = (unsigned char)f->lf_level[3];
    ps->lf_sharpness_level = (unsigned char)f->lf_sharpness;
    for (int i = 0; i < 8; i++) ps->lf_ref_deltas[i] = (char)(f->lf_delta_enabled ? f->lf_ref_deltas[i] : 0);
    for (int i = 0; i < 2; i++) ps->lf_mode_deltas[i] = (char)(f->lf_delta_enabled ? f->lf_mode_deltas[i] : 0);
    static const int lr_size_code[5] = { 0, 0, 0, 0, 0 }; (void)lr_size_code;
    // loop restoration: type and log2(unit size) - 5 per plane, 2 bits each; 0x3f sizes when it is off
    if (f->uses_lr || (s->enable_restoration && !f->all_lossless && !f->allow_intrabc))
        for (int p = 0; p < 3; p++) {
            ps->lr_type |= (unsigned char)((f->lr_type[p] & 3) << (2 * p));
            const int sz = f->lr_size[p], code = sz >= 256 ? 3 : sz >= 128 ? 2 : sz >= 64 ? 1 : 0;
            ps->lr_unit_size |= (unsigned char)(code << (2 * p));
        }
    else ps->lr_unit_size = 0x3f;
    // the frame's coded width (before superres upscaling); the references' sizes are their upscaled ones (mesa-nova)
    ps->current_frame.width = (unsigned short)f->frame_width; ps->current_frame.height = (unsigned short)f->frame_height;
    ps->current_frame.stride[0] = ps->current_frame.stride[1] = (unsigned short)(d->pitch / 16);
    ps->current_frame.frame_buffer_idx = (unsigned)surf;
    for (int i = 0; i < 7 && (!f->frame_is_intra || f->allow_intrabc); i++) {
        ref_frame_struct_s *rf = &ps->ref_frame[i];
        const int slot = f->frame_is_intra ? -1 : f->ref_frame_idx[i], rs = slot >= 0 ? d->ref[slot] : -1;
        const unsigned rw = rs >= 0 ? d->w[rs] : (unsigned)f->upscaled_width, rh = rs >= 0 ? d->h[rs] : (unsigned)f->frame_height;
        rf->info.width = (unsigned short)rw; rf->info.height = (unsigned short)rh;
        rf->info.stride[0] = rf->info.stride[1] = (unsigned short)(d->pitch / 16);
        rf->info.frame_buffer_idx = (unsigned)(rs >= 0 ? rs : surf);
        rf->sf.x_scale_fp = (short)((((int)rw << 14) + f->frame_width / 2) / f->frame_width);
        rf->sf.y_scale_fp = (short)((((int)rh << 14) + f->frame_height / 2) / f->frame_height);
        rf->sign_bias = (unsigned char)f->ref_frame_sign_bias[i + 1];
        rf->wmtype = (unsigned char)f->gm_type[i + 1];
    }
    ps->tileformat = 1; ps->gob_height = 0;
    ps->Av1FltTopOffset = (unsigned)(d->flt_top >> 8); ps->Av1FltVertOffset = (unsigned)(d->flt_vert >> 8);
    ps->Av1CdefVertOffset = (unsigned)(d->flt_cdef >> 8); ps->Av1LrVertOffset = (unsigned)(d->flt_lr >> 8);
    ps->Av1HusVertOffset = (unsigned)(d->flt_hus >> 8); ps->Av1FgsVertOffset = (unsigned)(d->flt_fgs >> 8);

    // references: distance in display order; motion field projections (libaom's av1_setup_motion_field, as mesa-nova
    // chooses them) and the segment IDs predicted from
    int proj[3] = { -1, -1, -1 }; int segRead = -1;
    {
        const Av1Seq *sq = s;
        struct { int surf, intra, order_hint, saved[8], seg; unsigned cw, uw, h; bool ok; } R[7];
        for (int i = 0; i < 7; i++) {
            const int slot = f->frame_is_intra ? -1 : f->ref_frame_idx[i];
            R[i].ok = slot >= 0 && d->ref[slot] >= 0;
            if (!R[i].ok) continue;
            const Av1Ref *ar = &d->st.ref[slot];
            R[i].surf = d->ref[slot]; R[i].intra = ar->frame_type == AV1_KEY_FRAME || ar->frame_type == AV1_INTRA_ONLY_FRAME;
            R[i].order_hint = ar->order_hint; memcpy(R[i].saved, ar->saved_order_hints, sizeof R[i].saved);
            R[i].cw = d->coded_w[R[i].surf]; R[i].uw = d->w[R[i].surf]; R[i].h = d->h[R[i].surf]; R[i].seg = d->seg_on[R[i].surf];
            ref_frame_struct_s *rf = &ps->ref_frame[i];
            rf->frame_off = (short)av1_get_relative_dist(sq, f->order_hint, R[i].order_hint);
            rf->roffset = (short)av1_get_relative_dist(sq, R[i].order_hint, f->order_hint);
            rf->sign_bias = rf->roffset > 0;
        }
        #define SAME(r) ((((r).cw + 7) >> 3) == (((unsigned)f->frame_width + 7) >> 3) && (((r).h + 7) >> 3) == (((unsigned)f->frame_height + 7) >> 3))
        int names[4], nn = 0;
        if (!f->frame_is_intra) {
            #define REFN(n) (R[(n) - 1])
            #define PROJECTS(n) (REFN(n).ok && !REFN(n).intra && SAME(REFN(n)))
            #define AHEAD(n) (REFN(n).ok && av1_get_relative_dist(sq, REFN(n).order_hint, f->order_hint) > 0)
            int stamp = 2;
            if (REFN(1).ok) {
                const int altOfLast = REFN(1).saved[7], golden = REFN(4).ok ? REFN(4).order_hint : 0;
                if (altOfLast != golden && PROJECTS(1)) names[nn++] = 1;
                stamp--;
            }
            for (int k = 0; k < 2; k++) { const int n = k ? 6 : 5; if (AHEAD(n) && PROJECTS(n)) { names[nn++] = n; stamp--; } }
            if (AHEAD(7) && stamp >= 0 && PROJECTS(7)) { names[nn++] = 7; stamp--; }
            if (stamp >= 0 && PROJECTS(2)) names[nn++] = 2;
            if (nn > 3) nn = 3;
            #undef REFN
            #undef PROJECTS
            #undef AHEAD
        }
        unsigned mfmv = 0;
        for (int k = 0; k < nn; k++) {
            const int n = names[k]; const typeof(R[0]) *src = &R[n - 1];
            proj[k] = src->surf;
            mfmv |= (unsigned)n << (3 + 3 * k);
            const int dd = av1_get_relative_dist(sq, f->order_hint, src->order_hint);
            if (f->use_ref_frame_mvs && dd >= -31 && dd <= 31) {
                mfmv |= 1u << k;
                for (int j = 0; j < 7; j++) {
                    const int saved = src->saved[1 + j];
                    ps->mfmv_offset[k][j] = (short)av1_get_relative_dist(sq, src->order_hint, saved);
                    ps->mfmv_side[k][j] = (char)(av1_get_relative_dist(sq, saved, src->order_hint) > 0);
                }
            }
        }
        ps->use_temporal0_mvs = mfmv & 1; ps->use_temporal1_mvs = (mfmv >> 1) & 1; ps->use_temporal2_mvs = (mfmv >> 2) & 1;
        ps->mf1_type = (mfmv >> 3) & 7; ps->mf2_type = (mfmv >> 6) & 7; ps->mf3_type = (mfmv >> 9) & 7;
        if (f->seg_enabled && f->primary_ref_frame != AV1_PRIMARY_REF_NONE && R[f->primary_ref_frame].ok && SAME(R[f->primary_ref_frame])) {
            ps->prevsegid_flag = 1;
            segRead = R[f->primary_ref_frame].seg ? R[f->primary_ref_frame].surf : -1;
        }
        #undef SAME
    }
    {   // global motion: per reference 32 bytes: the six warp parameters (32-bit), then the shear alpha, beta, gamma,
        // delta (16-bit), or 0x8000 each when the model cannot warp (mesa-nova av1.rs; shear: spec 7.11.3.6)
        uint8_t *g = d->gm.cpu; memset(g, 0, 7 * 32);
        for (int i = 0; i < 7; i++) {
            int32_t m[6] = { 0, 0, 1 << 16, 0, 0, 1 << 16 }; int16_t sh[4] = { 0, 0, 0, 0 };
            if (!f->frame_is_intra) {
                for (int k = 0; k < 6; k++) m[k] = f->gm_params[i + 1][k];
                if (!av1_shear(m, sh) || ps->ref_frame[i].sf.x_scale_fp != (1 << 14) || ps->ref_frame[i].sf.y_scale_fp != (1 << 14))
                    for (int k = 0; k < 4; k++) sh[k] = (int16_t)0x8000;
            }
            memcpy(g + i * 32, m, 24); memcpy(g + i * 32 + 24, sh, 8);
        }
    }
    Chan *c = &d->chan;
    chan_rewind(c);
    #define BUF(m, a) chan_mthd(c, 0, (m), 1, hi(a))
    // clc9b0.h names no AV1 application ID: it is AVD (0x12), which arrived with the AV1 methods in Ampere's clc7b0.h
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_AVD);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_AV1) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1) | DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _ERR_CONCEAL_ON, 1) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _MBTIMER_ON, 1));
    BUF(NVC9B0_SET_DRV_PIC_SETUP_OFFSET, d->setup.gpu);
    BUF(NVC9B0_SET_IN_BUF_BASE_OFFSET, d->bs.gpu);
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, d->picture_index++);   // frames decoded so far (mesa-nova)
    BUF(NVC9B0_SET_NVDEC_STATUS_OFFSET, d->status.gpu);
    BUF(NVC9B0_SET_TILE_SIZE_BUF_OFFSET, d->tileinfo.gpu);
    BUF(NVC9B0_SET_FILTER_BUFFER_OFFSET, d->filter.gpu);
    BUF(NVC9B0_SET_INTRA_TOP_BUF_OFFSET, d->intra.gpu);
    BUF(NVC9B0_AV1_SET_PROB_TAB_READ_BUF_OFFSET, cdf_in->gpu);
    BUF(NVC9B0_AV1_SET_PROB_TAB_WRITE_BUF_OFFSET, d->cdf_out.gpu);
    BUF(NVC9B0_AV1_SET_SEGMENT_READ_BUF_OFFSET, segRead >= 0 ? d->seg[segRead].gpu : d->zseg.gpu);
    BUF(NVC9B0_AV1_SET_SEGMENT_WRITE_BUF_OFFSET, d->seg[surf].gpu);
    BUF(NVC9B0_AV1_SET_COL_MVWRITE_BUF_OFFSET, d->tmv[surf].gpu);
    for (int i = 0; i < 3; i++) BUF(NVC9B0_AV1_SET_COL_MV0_READ_BUF_OFFSET + 4 * (NvU32)i, d->tmv[proj[i] >= 0 ? proj[i] : surf].gpu);
    BUF(NVC9B0_AV1_SET_GLOBAL_MODEL_BUF_OFFSET, d->gm.gpu);
    BUF(NVC9B0_AV1_SET_FILM_GRAIN_BUF_OFFSET, d->fg.gpu);
    BUF(NVC9B0_AV1_SET_TILE_STREAM_INFO_BUF_OFFSET, d->tilestream.gpu);
    // pictures: the references LAST..ALTREF in 0..6 (the frame itself for intra frames and missing references), the frame
    // decoded in 8 (mesa-nova av1.rs)
    for (int i = 0; i < 7; i++) {
        const int slot = f->frame_is_intra ? -1 : f->ref_frame_idx[i], rs = slot >= 0 && d->ref[slot] >= 0 ? d->ref[slot] : surf;
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->luma[rs].gpu));
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->chroma[rs].gpu));
    }
    BUF(NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * 8, d->luma[surf].gpu); BUF(NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * 8, d->chroma[surf].gpu);
    if (disp >= 0) {
        chan_mthd(c, 0, NVC9B0_SET_DISPLAY_BUF_LUMA_OFFSET, 1, hi(d->luma[disp].gpu));
        chan_mthd(c, 0, NVC9B0_SET_DISPLAY_BUF_CHROMA_OFFSET, 1, hi(d->chroma[disp].gpu));
    }
    #undef BUF
    chan_mthd(c, 0, NVC9B0_EXECUTE, 1, 0u);
    const NvU32 val = ++d->semval; volatile NvU32 *sv = d->sem.cpu;
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_A, 3, (NvU32)(d->sem.gpu >> 32), (NvU32)d->sem.gpu, val);
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_D, 1, DRF_DEF(C9B0, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B0, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B0, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    const double t0 = now_ms();
    chan_submit(c);
    while (sv[0] != val && now_ms() - t0 < 1000) { }
    d->engine_ms = now_ms() - t0;
    if (sv[0] != val) { fprintf(stderr, "av1dec: no completion (error notifier 0x%x)\n", chan_error(c)); return -1; }
    const nvdec_status_s *st = d->status.cpu;
    d->err_status = st->error_status; d->ok_blocks = st->mbs_correctly_decoded;
    if (d->verbose)
        printf("  av1dec: surf %2d type %d%s %dx%d q %d tiles %d -> status 0x%x, ok %u, err %u, %.2f ms\n", surf, f->frame_type,
               f->show_frame ? "" : " (hidden)", f->upscaled_width, f->frame_height, f->base_q_idx, d->ntiles, st->error_status,
               st->mbs_correctly_decoded, st->mbs_in_error, d->engine_ms);
    // reference update (7.20): the surface and the CDFs to save (written by the engine unless frame-end update is off)
    for (int i = 0; i < 8; i++) if (f->refresh_frame_flags & (1 << i)) {
        d->ref[i] = surf;
        memcpy(d->cdf[i].cpu, f->disable_frame_end_update_cdf ? cdf_in->cpu : d->cdf_out.cpu, d->cdf_size);
    }
    d->w[surf] = (unsigned)f->upscaled_width; d->h[surf] = (unsigned)f->frame_height; d->coded_w[surf] = (unsigned)f->frame_width;
    d->seg_on[surf] = f->seg_enabled;
    av1_end_frame(&d->st, f);
    // twins of surfaces no longer referenced are free again
    for (int k = 0; k < AV1_SURFACES; k++) if (d->twin[k] >= 0) {
        bool ref = false; for (int i = 0; i < 8; i++) if (d->ref[i] == k) ref = true;
        if (!ref) d->twin[k] = -1;
    }
    d->dec_surf = surf;
    if (disp >= 0) { d->w[disp] = d->w[surf]; d->h[disp] = d->h[surf]; }
    if (!f->show_frame) { if (disp >= 0) d->twin[surf] = disp; return 0; }
    if (disp >= 0) surf = disp;
    d->busy[surf] = true; d->cur_surf = surf; d->cur_w = f->upscaled_width; d->cur_h = f->frame_height;
    return 1;
}

int av1dec_tu(Av1Dec *d, const uint8_t *buf, size_t n)
{
    int shown = 0;
    d->skipped = false;
    for (size_t pos = 0; pos < n;) {
        Av1Obu o; if (av1_obu(buf + pos, n - pos, &o)) { fprintf(stderr, "av1dec: bad OBU\n"); return -1; }
        const uint8_t *pl = buf + pos + o.header_len;
        d->st.temporal_id = o.temporal_id; d->st.spatial_id = o.spatial_id;
        const uint8_t *tg = NULL; size_t tgn = 0;
        if (o.type == AV1_OBU_SEQUENCE_HEADER) {
            Av1Seq sq;
            if (av1_parse_sequence_header(&sq, pl, o.payload_len)) return -1;
            // a new sequence with larger frames or another bit depth starts with a key frame: reallocate for it
            const unsigned W = (unsigned)sq.max_frame_width_minus_1 + 1, H = (unsigned)sq.max_frame_height_minus_1 + 1;
            if (W > d->maxW || H > d->maxH || sq.bit_depth != d->bit_depth) {
                Rm *rm = d->rm; const bool v = d->verbose;
                for (int i = 0; i < AV1_SURFACES; i++) if (d->busy[i]) { fprintf(stderr, "av1dec: new sequence while pictures are held\n"); return -1; }
                av1dec_close(d);
                if (av1dec_open(d, rm, W > d->maxW ? W : d->maxW, H > d->maxH ? H : d->maxH, sq.bit_depth)) return -1;
                d->verbose = v;
            }
            d->st.seq = sq;
        } else if (o.type == AV1_OBU_FRAME_HEADER || o.type == AV1_OBU_FRAME) {
            if (d->have_hdr && o.type == AV1_OBU_FRAME_HEADER) { pos += o.header_len + o.payload_len; continue; }   // a copy
            size_t hb;
            if (av1_parse_frame_header(&d->st, pl, o.payload_len, &d->f, &hb)) { fprintf(stderr, "av1dec: frame header error\n"); return -1; }
            if (d->f.show_existing_frame) {
                const int sfc = d->ref[d->f.frame_to_show_map_idx];
                if (sfc < 0) {   // a frame left out above (references never decoded): nothing to show
                    if (d->f.frame_type == AV1_KEY_FRAME) for (int i = 0; i < 8; i++) d->ref[i] = -1;
                    av1_end_frame(&d->st, &d->f);
                    d->dec_surf = -1; d->skipped = true; d->nskipped++;
                    pos += o.header_len + o.payload_len; continue;
                }
                if (d->f.frame_type == AV1_KEY_FRAME)   // a shown key frame refreshes every slot (7.21, 7.20)
                    for (int i = 0; i < 8; i++) { if (i != d->f.frame_to_show_map_idx) memcpy(d->cdf[i].cpu, d->cdf[d->f.frame_to_show_map_idx].cpu, d->cdf_size); d->ref[i] = sfc; }
                av1_end_frame(&d->st, &d->f);
                if (shown) av1dec_release(d, d->cur_surf);
                int out = sfc; d->dec_surf = -1;
                if (d->twin[sfc] >= 0) { out = d->twin[sfc]; d->twin[sfc] = -1; }   // its film-grain output
                d->busy[out] = true; d->cur_surf = out; d->cur_w = (int)d->w[out]; d->cur_h = (int)d->h[out]; shown = 1;
            } else {
                d->have_hdr = true; d->bsN = 0; d->ntiles = 0;
                if (o.type == AV1_OBU_FRAME) { tg = pl + hb; tgn = o.payload_len - hb; }
            }
        } else if (o.type == AV1_OBU_TILE_GROUP && d->have_hdr) { tg = pl; tgn = o.payload_len; }
        if (tg) {
            static Av1Tile tl[AV1_MAX_TILES]; int start; bool last;
            const int nt = av1_parse_tile_group(&d->f, tg, tgn, tl, AV1_MAX_TILES, &start, &last);
            if (nt < 0) { fprintf(stderr, "av1dec: tile group error\n"); return -1; }
            for (int t = 0; t < nt; t++) {   // each tile's data, back to back in the bitstream buffer
                if (d->bsN + tl[t].size + 64 > d->bs.size || d->ntiles >= AV1_MAX_TILES) return -1;
                memcpy((uint8_t *)d->bs.cpu + d->bsN, tg + tl[t].offset, tl[t].size);
                d->tile[d->ntiles].start = d->bsN; d->tile[d->ntiles].end = d->bsN + tl[t].size;
                d->tile[d->ntiles].row = tl[t].row; d->tile[d->ntiles].col = tl[t].col; d->ntiles++;
                d->bsN += tl[t].size;
            }
            if (last) {
                memset((uint8_t *)d->bs.cpu + d->bsN, 0, 64);
                d->have_hdr = false;
                const int prev = shown ? d->cur_surf : -1;
                const int r = decode_frame(d);
                if (r < 0) return -1;
                if (r == 1) { if (prev >= 0) av1dec_release(d, prev); shown = 1; }
            }
        }
        pos += o.header_len + o.payload_len;
    }
    return shown;
}
