// encodeN.c (milestone M3): encode N frames of moving synthetic NV12 content through src/enc.c (IDR + P, constant QP).
//   ./encodeN [-v] [-hevc | -av1] W H N QP_I QP_P REFS out.h264|out.h265|out.ivf out.nv12   (AV1: QP = quantizer index 0..255)
//   COLOUR=p,t,m (H.273 colour description), HDR=1 (mastering display + content light level metadata on key frames)
// Environment: IDR_PERIOD=<n> (an IDR every n frames), RC=<RCMODE> BITRATE=<bit/s> MAXRATE=<bit/s> VBV=<bits>,
// SLICES=<n>, ENCN_STATIC=<n> (cycle through n fixed pictures), BITDEPTH=10 (HEVC / AV1: the 8-bit source widened),
// CHROMA444=1 (HEVC 4:4:4; the source file is then planar yuv444p).
// Writes the Annex B stream (SPS + PPS once, then every picture; AV1: an IVF file) and all source frames for h264check.
#include "enc.h"
#include "la.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void frame_synth(uint8_t *y, uint8_t *uv, int W, int H, int f)
{
    // moving diagonal gradient + 64 px checkerboard scrolling right + a sweeping bar
    for (int r = 0; r < H; r++)
        for (int c = 0; c < W; c++) {
            int v = ((c + r + f * 8) & 255) / 2 + (((((c - f * 4) >> 6) ^ (r >> 6)) & 1) ? 90 : 0);
            if (abs(c - (f * 24) % W) < 16) v = 235;
            y[r * W + c] = (uint8_t)(16 + v * 219 / 255);
        }
    for (int r = 0; r < (H + 1) / 2; r++)
        for (int c = 0; c < W / 2; c++) {
            uv[r * W + 2 * c] = (uint8_t)(128 + 60 * sin((c + f) * 0.02));
            uv[r * W + 2 * c + 1] = (uint8_t)(128 + 60 * cos((r + f) * 0.03));
        }
}

// SRC=file.nv12: frames from a raw NV12 file (display index f, looping) instead of the synthetic pattern
static void frame(uint8_t *y, uint8_t *uv, int W, int H, int f)
{
    static FILE *src; static long nframes = -1;
    const char *path = getenv("SRC");
    if (!path) { frame_synth(y, uv, W, H, f); return; }
    const size_t ys = (size_t)W * H, cs = (size_t)W * ((H + 1) / 2);
    if (!src) { src = fopen(path, "rb"); if (!src) { perror(path); exit(1); } fseek(src, 0, SEEK_END); nframes = ftell(src) / (long)(ys + cs); }
    fseek(src, (long)(f % nframes) * (long)(ys + cs), SEEK_SET);
    if (fread(y, 1, ys, src) != ys || fread(uv, 1, cs, src) != cs) { fprintf(stderr, "short read from %s\n", path); exit(1); }
}

int main(int argc, char **argv)
{
    int ai = 1; bool verbose = false;
    if (argc > 1 && !strcmp(argv[1], "-v")) { verbose = true; ai++; }
    bool hevc = false, av1 = false;
    if (argc > ai && !strcmp(argv[ai], "-hevc")) { hevc = true; ai++; }
    if (argc > ai && !strcmp(argv[ai], "-av1")) { av1 = true; ai++; }
    if (argc - ai < 8) { fprintf(stderr, "usage: encodeN [-v] W H N QP_I QP_P REFS out.h264 out.nv12\n"); return 2; }
    int W = atoi(argv[ai]), H = atoi(argv[ai + 1]), N = atoi(argv[ai + 2]), CH = (H + 1) / 2;
    EncConfig cfg = { .codec = av1 ? ENC_AV1 : hevc ? ENC_HEVC : ENC_H264, .width = W, .height = H, .qp_i = atoi(argv[ai + 3]), .qp_p = atoi(argv[ai + 4]), .num_refs = atoi(argv[ai + 5]),
                      .idr_period = getenv("IDR_PERIOD") ? atoi(getenv("IDR_PERIOD")) : 0, .verbose = verbose,
                      .rc_mode = getenv("RC") ? atoi(getenv("RC")) : 0, .bitrate = getenv("BITRATE") ? atoi(getenv("BITRATE")) : 0,
                      .max_bitrate = getenv("MAXRATE") ? atoi(getenv("MAXRATE")) : 0, .vbv_bits = getenv("VBV") ? atoi(getenv("VBV")) : 0,
                      .slices = getenv("SLICES") ? atoi(getenv("SLICES")) : 1, .bit_depth = getenv("BITDEPTH") ? atoi(getenv("BITDEPTH")) : 8,
                      .chroma444 = getenv("CHROMA444") != NULL, .lossless = getenv("LOSSLESS") != NULL, .bframes = getenv("BFRAMES") ? atoi(getenv("BFRAMES")) : 0, .quality = getenv("QUALITY") != NULL, .no_cu8 = getenv("NOCU8") != NULL, .aq = getenv("AQ") ? atoi(getenv("AQ")) : 0, .two_pass = getenv("TWOPASS") != NULL, .cabac = getenv("CABAC") != NULL, .h264_profile = getenv("HIGH") ? 100 : 77 };
    if (getenv("COLOUR")) sscanf(getenv("COLOUR"), "%d,%d,%d", &cfg.colour[0], &cfg.colour[1], &cfg.colour[2]);   // COLOUR=9,16,9 (H.273 codes)
    if (getenv("HDR")) {   // HDR=1: P3-D65 mastering display, 1000 / 0.005 cd/m2, MaxCLL 1000, MaxFALL 400 (SEI byte layout)
        static const uint8_t m[24] = { 0x33, 0xc2, 0x86, 0xc4, 0x1d, 0x4c, 0x0b, 0xb8, 0x84, 0xd0, 0x3e, 0x80, 0x3d, 0x13, 0x40, 0x42,
                                       0x00, 0x98, 0x96, 0x80, 0x00, 0x00, 0x00, 0x32 }, c[4] = { 0x03, 0xe8, 0x01, 0x90 };
        memcpy(cfg.mdcv, m, 24); memcpy(cfg.cll, c, 4); cfg.has_mdcv = cfg.has_cll = true;
    }
    Enc e; if (enc_open(&e, &cfg)) return 1;
    cfg.bframes = e.cfg.bframes;   // the encoder may have turned B pictures off (HEVC on NVENC 6.6)
    FILE *fh = fopen(argv[ai + 6], "wb"), *fs = fopen(argv[ai + 7], "wb");
    uint8_t hdr[256]; fwrite(hdr, 1, enc_headers(&e, hdr), fh);
    if (av1) {   // IVF file header
        uint8_t ih[32] = "DKIF"; ih[6] = 32; memcpy(ih + 8, "AV01", 4); ih[12] = (uint8_t)W; ih[13] = (uint8_t)(W >> 8);
        ih[14] = (uint8_t)H; ih[15] = (uint8_t)(H >> 8); ih[16] = 60; ih[20] = 1; ih[24] = (uint8_t)N; ih[25] = (uint8_t)(N >> 8);
        fwrite(ih, 1, 32, fh);
    }
    uint8_t *y = malloc((size_t)W * H), *uv = malloc((size_t)W * CH);
    // LOOKAHEAD=1 (T7.09, research): every frame through the lookahead analysis first (la.c: NVIDIA's quarter-size QP 40
    // H.264 pass), printed with -v; with SCENECUT=thr an IDR at each cut. (NVIDIA's per-picture lookahead hints, ab_beta /
    // prev_act, change nothing on this engine: identical output for any value)
    const int L = getenv("LOOKAHEAD") ? atoi(getenv("LOOKAHEAD")) : 0;
    double *laRatio = NULL;
    if (L > 0) {
        La la; if (la_open(&la, W, H, cfg.engine)) return 1;
        laRatio = calloc((size_t)N, sizeof *laRatio); double laMs = 0;
        for (int f = 0; f < N; f++) {
            frame(y, uv, W, H, f); LaStat st;
            if (la_push(&la, y, W, uv, W, false, &st)) { fprintf(stderr, "lookahead failed at frame %d\n", f); return 1; }
            laRatio[f] = st.inter && st.intra ? (double)st.inter / st.intra : -1; laMs += st.ms;
            if (verbose) printf("lookahead %3d: intra %u inter %u (%.3f) bits %u\n", f, st.intra, st.inter, laRatio[f], st.bits);
        }
        printf("lookahead %dx%d: %.3f ms per frame\n", la.w, la.h, laMs / N);
        la_close(&la);
    }
    // SCENECUT=thr (with LOOKAHEAD): an IDR where the lookahead's inter / intra complexity ratio passes thr (a cut: the
    // previous frame predicts nothing; steady content 0.7-0.85 on crowd_run / park_joy, cuts 1.6-2.2)
    const double cutThr = getenv("SCENECUT") ? atof(getenv("SCENECUT")) : 0;
    #define IS_CUT(f) (cutThr > 0 && laRatio && (f) > 0 && (f) < N && laRatio[f] > cutThr)
    if (cfg.bframes > 0) {   // BFRAMES=n (H.264, HEVC, AV1): IDR, then each anchor followed by the n B pictures before it
        for (int f = 0; f < N; f++) { frame(y, uv, W, H, f); fwrite(y, 1, (size_t)W * H, fs); fwrite(uv, 1, (size_t)W * CH, fs); }
        int fails = 0; double bits = 0, ms = 0; int prev = 0, coded = 0, tu = 0;
        // AV1: IVF frames = temporal units in display order (a hidden anchor goes with the next one; after the B pictures,
        // the unit that shows the anchor)
        #define IVF_TU(d, n) do { if (av1) { uint8_t fhd_[12] = { (uint8_t)(n), (uint8_t)((n) >> 8), (uint8_t)((n) >> 16), (uint8_t)((n) >> 24), (uint8_t)tu, (uint8_t)(tu >> 8) }; fwrite(fhd_, 1, 12, fh); tu++; } fwrite(d, 1, n, fh); } while (0)
        // IDR_PERIOD: an IDR every that many pictures in display order; the pictures before it are coded first (the last of
        // them as an anchor), so no B picture references across the IDR (as libnvencvt does)
        const int P = cfg.idr_period;
        for (int a = 0; a < N && !fails; ) {   // a: the next anchor in display order
            const bool key = a == 0 || (P > 0 && a % P == 0) || IS_CUT(a);
            int order[16], no = 0; order[no++] = a; for (int k = prev + 1; k < a; k++) order[no++] = k;
            if (a == 0) no = 1;
            for (int i = 0; i < no; i++) {
                const int disp = order[i]; const bool bp = i > 0;
                frame(y, uv, W, H, disp); enc_load_nv12(&e, y, W, uv, W);
                EncFrame fr;
                if (enc_encode_ex(&e, key && i == 0 && a > 0, bp, disp, &fr)) { printf("frame %d FAILED (ucode error %u)\n", disp, fr.ucode_error); fails++; break; }
                if (!fr.hidden) IVF_TU(fr.data, fr.size);
                bits += fr.bits; ms += fr.ms; coded++;
                if (av1 && bp && i == no - 1 && no > 1) { uint8_t se[16]; size_t sn = enc_av1_show_existing(&e, se); IVF_TU(se, sn); }
                if (verbose || coded <= 4) printf("coded %3d: display %3d %s%s poc %3u: %8u bits, %.3f ms, avgQP %u, intra %u inter %u MBs\n", coded - 1, disp, fr.idr ? "IDR" : bp ? "B  " : "P  ", fr.hidden ? " (hidden)" : "", fr.poc, fr.bits, fr.ms, fr.avg_qp, fr.intra_mbs, fr.inter_mbs);
            }
            prev = a; a = a == 0 ? (cfg.bframes + 1 < N ? cfg.bframes + 1 : N - 1) : (a + cfg.bframes + 1 < N ? a + cfg.bframes + 1 : (a == N - 1 ? N : N - 1));
            if (a < N) {   // the next IDR (period or cut): alone, right after the anchor before it
                int k = P > 0 ? (prev / P + 1) * P : N;
                for (int c = prev + 1; c < k && c <= a; c++) if (IS_CUT(c)) { k = c; break; }
                if (a >= k) a = k - 1 > prev ? k - 1 : k;
            }
        }
        printf("%d frames %dx%d with B pictures (%d between anchors): %.1f kbit/frame, %.2f Mbit/s at 60 fps, encode %.3f ms avg, %d failed\n",
               coded, W, H, cfg.bframes, bits / coded / 1000, bits / coded * 60 / 1e6, ms / coded, fails);
        fclose(fh); fclose(fs); enc_close(&e); return fails ? 1 : 0;
    }
    double bits = 0, ms = 0, msMax = 0, bitsSteady = 0, pMax = 0; int fails = 0, nSteady = 0, qpMin = 99, qpMax = 0;
    for (int f = 0; f < N; f++) {
        // ENCN_STATIC=n: cycle through n fixed pictures (A B A B ... for n = 2), each very different from the next
        int ns = getenv("ENCN_STATIC") ? atoi(getenv("ENCN_STATIC")) : 0;
        frame(y, uv, W, H, ns > 0 ? (f % ns) * 37 : f);
        if (cfg.chroma444) {   // full-resolution chroma, the same pattern as the 4:2:0 frames
            static uint8_t *u4, *v4; if (!u4) { u4 = malloc((size_t)W * H); v4 = malloc((size_t)W * H); }
            const int fr = ns > 0 ? (f % ns) * 37 : f;
            for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) {
                u4[(size_t)r * W + c] = (uint8_t)(128 + 60 * sin((c / 2 + fr) * 0.02) + ((c ^ r) & 8 ? 6 : 0));
                v4[(size_t)r * W + c] = (uint8_t)(128 + 60 * cos((r / 2 + fr) * 0.03) - ((c + r) & 4 ? 5 : 0));
            }
            fwrite(y, 1, (size_t)W * H, fs); fwrite(u4, 1, (size_t)W * H, fs); fwrite(v4, 1, (size_t)W * H, fs);
            enc_load_yuv444(&e, y, W, u4, W, v4, W);
        } else {
        fwrite(y, 1, (size_t)W * H, fs); fwrite(uv, 1, (size_t)W * CH, fs);
        enc_load_nv12(&e, y, W, uv, W);
        }
        EncFrame fr;
        if (enc_encode(&e, IS_CUT(f), &fr)) { printf("frame %d FAILED (ucode error %u)\n", f, fr.ucode_error); fails++; break; }
        if (av1) { uint8_t fhd[12] = { (uint8_t)fr.size, (uint8_t)(fr.size >> 8), (uint8_t)(fr.size >> 16), (uint8_t)(fr.size >> 24), (uint8_t)f, (uint8_t)(f >> 8) }; fwrite(fhd, 1, 12, fh); }
        fwrite(fr.data, 1, fr.size, fh);
        bits += fr.bits; ms += fr.ms; if (fr.ms > msMax) msMax = fr.ms;
        if (f >= 10) { bitsSteady += fr.bits; nSteady++; if (!fr.idr && fr.bits > pMax) pMax = fr.bits;
                       if ((int)fr.avg_qp < qpMin) qpMin = (int)fr.avg_qp; if ((int)fr.avg_qp > qpMax) qpMax = (int)fr.avg_qp; }
        if (verbose || f < 4 || f == N - 1)
            printf("frame %3d %s frame_num %3u poc %3u: %8u bits, %.3f ms, avgQP %u, intra %u inter %u MBs\n", f, fr.idr ? "IDR" : "P  ",
                   fr.frame_num, fr.poc, fr.bits, fr.ms, fr.avg_qp, fr.intra_mbs, fr.inter_mbs);
    }
    fclose(fh); fclose(fs);
    printf("%d frames %dx%d: %.1f kbit/frame, %.2f Mbit/s at 60 fps, encode %.3f ms avg %.3f ms max, %d failed\n", N, W, H,
           bits / N / 1000, bits / N * 60 / 1e6, ms / N, msMax, fails);
    if (nSteady) printf("frames 10..%d: %.2f Mbit/s at 60 fps (%.1f kbit/frame), largest P %.1f kbit, avgQP %d..%d\n", N - 1,
                        bitsSteady / nSteady * 60 / 1e6, bitsSteady / nSteady / 1000, pMax / 1000, qpMin, qpMax);
    enc_close(&e);
    return fails ? 1 : 0;
}
