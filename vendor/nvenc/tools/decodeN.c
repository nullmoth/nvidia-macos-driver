// decodeN.c (M8, T8.04): decode a whole Annex B H.264 stream with src/dec.c and compare every picture with a reference
// decode (ffmpeg, I420, display order). The display position of each picture comes from its POC within its IDR period.
//   ./decodeN [-v] in.h264 ref.yuv
#include "dec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct { int poc, period, disp; } Pic;
static double gReadMs;
static Pic gPics[100000]; static int gNP;

static int by_period_poc(const void *a, const void *b)
{
    const Pic *x = *(Pic *const *)a, *y = *(Pic *const *)b;
    return x->period != y->period ? x->period - y->period : x->poc - y->poc;
}
static double psnr(const uint8_t *a, const uint8_t *b, size_t n)
{
    double se = 0; for (size_t i = 0; i < n; i++) { int d = a[i] - b[i]; se += d * d; }
    return se == 0 ? 99.0 : 10 * log10(255.0 * 255.0 / (se / n));
}

int main(int argc, char **argv)
{
    int ai = 1; bool verbose = false;
    if (ai < argc && !strcmp(argv[ai], "-v")) { verbose = true; ai++; }
    if (argc - ai < 2) { fprintf(stderr, "usage: decodeN [-v] in.h264 ref.yuv\n"); return 2; }
    FILE *f = fopen(argv[ai], "rb"); if (!f) { perror(argv[ai]); return 1; }
    fseek(f, 0, SEEK_END); size_t n = (size_t)ftell(f); fseek(f, 0, SEEK_SET);
    uint8_t *in = malloc(n); if (fread(in, 1, n, f) != n) return 1; fclose(f);
    FILE *rf = fopen(argv[ai + 1], "rb"); if (!rf) { perror(argv[ai + 1]); return 1; }

    // pass 1: display order
    static H264Params p; H264PocState st = { 0 }; int period = -1, W = 0, H = 0; H264Slice prev; bool havePrev = false;
    bool pfValid = false; int pfPar = 0, pfFn = -1;   // a first field waiting for its second
    size_t pos = 0; const uint8_t *nal; size_t len;
    while (h264_next_annexb(in, n, &pos, &nal, &len)) {
        int t = nal[0] & 31; H264Slice s;
        if (t == H264_NAL_SPS) h264_parse_sps(&p, nal, len);
        else if (t == H264_NAL_PPS) h264_parse_pps(&p, nal, len);
        else if ((t == H264_NAL_SLICE || t == H264_NAL_IDR) && !h264_parse_slice_header(&p, nal, len, &s) && !s.redundant_pic_cnt &&
                 (!havePrev || h264_new_picture(&p, &prev, &s)) && (havePrev = true, prev = s, true)) {
            const H264Sps *sps = &p.sps[p.pps[s.pps_id].sps_id];
            if (!W) { W = (sps->pic_width_in_mbs_minus1 + 1) * 16; H = (sps->pic_height_in_map_units_minus1 + 1) * 16 * (2 - sps->frame_mbs_only_flag); }
            int top, bot; h264_poc(&st, sps, &s, &top, &bot);
            // an IDR picture, or one with memory_management_control_operation 5, starts a new period: every earlier picture
            // is output first (C.4.4) and the MMCO 5 picture's POC becomes relative to itself (8.2.1)
            // the second field of a pair belongs to its first field's frame (opposite parity, same frame_num)
            const bool second = s.field_pic_flag && pfValid && pfPar != s.bottom_field_flag && pfFn == s.frame_num;
            const int poc = s.has_mmco5 ? 0 : s.field_pic_flag ? (s.bottom_field_flag ? bot : top) : (top < bot ? top : bot);
            pfValid = s.field_pic_flag && !second; pfPar = s.bottom_field_flag; pfFn = s.frame_num;
            if (second) { if (gNP && poc < gPics[gNP - 1].poc) gPics[gNP - 1].poc = poc; continue; }
            if (s.idr || s.has_mmco5) period++;
            if (gNP < 100000) gPics[gNP++] = (Pic){ poc, period, 0 };
        }
    }
    static Pic *ord[100000]; for (int i = 0; i < gNP; i++) ord[i] = &gPics[i];
    qsort(ord, (size_t)gNP, sizeof ord[0], by_period_poc);
    for (int i = 0; i < gNP; i++) ord[i]->disp = i;

    // pass 2: decode and compare
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    static H264Dec d; if (h264dec_open(&d, &rm, (unsigned)W, (unsigned)H)) return 1;
    d.verbose = verbose;
    int done = 0, exact = 0, first_bad = -1; double worst = 99, eng = 0; size_t fs = 0; uint8_t *ref = NULL, *out = NULL;
    const double t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1e6;
    pos = 0;
    for (bool end = false; !end;) {
        int r;
        if (h264_next_annexb(in, n, &pos, &nal, &len)) r = h264dec_nal(&d, nal, len);
        else { r = h264dec_flush(&d); end = true; }
        if (r < 0) { printf("decode error at picture %d\n", done); break; }
        while (h264dec_pop(&d)) {
        const unsigned w = (unsigned)d.cur_w, h = (unsigned)d.cur_h;
        if (!fs) { fs = (size_t)w * h * 3 / 2; ref = malloc(fs); out = malloc(fs); }
        eng += d.engine_ms;
        // NV12 through the SIMD untile (what a VideoToolbox decoder hands out), timed; then to I420 for the comparison
        static uint8_t *nv; static double rd = 0; if (!nv) nv = malloc(fs);
        const double r0 = clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1e6;
        h264dec_read_nv12(&d, d.cur_surf, nv, w, nv + (size_t)w * h, w, w, h);
        rd += clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1e6 - r0; gReadMs = rd;
        memcpy(out, nv, (size_t)w * h);
        for (size_t i = 0; i < (size_t)w * h / 4; i++) { out[(size_t)w * h + i] = nv[(size_t)w * h + 2 * i]; out[(size_t)w * h * 5 / 4 + i] = nv[(size_t)w * h + 2 * i + 1]; }
        const int disp = done < gNP ? gPics[done].disp : -1;
        double ps = 0;
        if (disp >= 0 && !fseeko(rf, (off_t)disp * (off_t)fs, SEEK_SET) && fread(ref, 1, fs, rf) == fs) {
            ps = psnr(out, ref, (size_t)w * h);
            double pu = psnr(out + (size_t)w * h, ref + (size_t)w * h, fs - (size_t)w * h);
            if (pu < ps) ps = pu;
        }
        if (ps >= 99) exact++; else if (first_bad < 0) first_bad = done;
        if (ps < worst) worst = ps;
        if (verbose || (ps < 99 && done - first_bad < 5)) printf("pic %4d (display %4d): %s %.2f dB, %u MBs ok, %u err, status 0x%x\n",
                                                        done, disp, ps >= 99 ? "exact" : "DIFF", ps, d.mbs_ok, d.mbs_err, d.err_status);
        h264dec_release(&d, d.cur_surf);
        done++;
        }
    }
    const double t1 = clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1e6;
    printf("RESULT %d of %d pictures decoded, %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/picture (%.0f fps), NV12 out %.2f ms/picture, total %.1f ms\n",
           done, gNP, exact, first_bad, worst, done ? eng / done : 0, done ? 1000.0 * done / eng : 0, done ? gReadMs / done : 0, t1 - t0);
    h264dec_close(&d);
    return exact == gNP && done == gNP ? 0 : 1;
}
