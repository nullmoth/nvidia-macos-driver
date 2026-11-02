// av1decodeN.c (M9, T9.02/T9.03): decode an AV1 IVF stream with src/av1dec.c and compare every shown frame with a
// reference decode (dav1d, I420 / 16-bit LE, display order; scripts/make-av1-streams.sh makes both).
//   ./av1decodeN [-v] [-n frames] in.ivf ref.yuv
#include "av1dec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int gB = 1, gBD = 8;
static double psnr(const uint8_t *a, const uint8_t *b, size_t n)
{
    double se = 0;
    for (size_t i = 0; i < n; i++) { int d = gB == 1 ? a[i] - b[i] : (a[2 * i] | a[2 * i + 1] << 8) - (b[2 * i] | b[2 * i + 1] << 8); se += (double)d * d; }
    const double mx = (double)((1 << gBD) - 1);
    return se == 0 ? 99.0 : 10 * log10(mx * mx / (se / n));
}

static uint8_t *read_tu(FILE *fp, size_t *sz)
{
    uint8_t fh[12]; if (fread(fh, 1, 12, fp) != 12) return NULL;
    *sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
    uint8_t *b = malloc(*sz); if (fread(b, 1, *sz, fp) != *sz) { free(b); return NULL; }
    return b;
}

int main(int argc, char **argv)
{
    int ai = 1, maxf = 1 << 30; bool verbose = false;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: av1decodeN [-v] [-n frames] in.ivf ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    FILE *rf = fopen(argv[ai + 1], "rb"); if (!rf) { perror(argv[ai + 1]); return 1; }
    uint8_t ih[32]; if (fread(ih, 1, 32, fp) != 32 || memcmp(ih, "DKIF", 4)) { fprintf(stderr, "not IVF\n"); return 1; }
    // session size and bit depth from the sequence header
    unsigned W = 0, H = 0; int bd = 8; size_t sz; uint8_t *buf;
    while ((buf = read_tu(fp, &sz))) {
        for (size_t pos = 0; pos < sz;) {
            Av1Obu o; if (av1_obu(buf + pos, sz - pos, &o)) break;
            Av1Seq s;
            if (o.type == AV1_OBU_SEQUENCE_HEADER && !av1_parse_sequence_header(&s, buf + pos + o.header_len, o.payload_len)) {
                if ((unsigned)s.max_frame_width_minus_1 + 1 > W) W = (unsigned)s.max_frame_width_minus_1 + 1;
                if ((unsigned)s.max_frame_height_minus_1 + 1 > H) H = (unsigned)s.max_frame_height_minus_1 + 1;
                bd = s.bit_depth;
            }
            pos += o.header_len + o.payload_len;
        }
        free(buf);
    }
    fseek(fp, 32, SEEK_SET);
    if (getenv("AV1_SMALL")) { W = ih[12] | ih[13] << 8; H = ih[14] | ih[15] << 8; }   // start at the first size (as VideoToolbox does)
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    static Av1Dec d; if (av1dec_open(&d, &rm, W, H, bd)) return 1;
    d.verbose = verbose; gB = d.bps; gBD = bd;
    int shown = 0, exact = 0, first_bad = -1; double worst = 99, eng = 0; int decoded = 0; off_t refpos = 0;
    uint8_t *out = NULL, *ref = NULL; size_t cap = 0;
    while (shown < maxf && (buf = read_tu(fp, &sz))) {
        const int r = av1dec_tu(&d, buf, sz); free(buf);
        if (r < 0) { printf("decode error at shown frame %d\n", shown); break; }
        eng += d.engine_ms; decoded++;
        if (r == 0) continue;
        const unsigned w = (unsigned)d.cur_w, h = (unsigned)d.cur_h, cw = (w + 1) / 2, ch = (h + 1) / 2;
        const bool mono = d.st.seq.mono_chrome;   // the reference has luma only
        const size_t ns = (size_t)w * h + (mono ? 0 : 2 * (size_t)cw * ch), fs = ns * (size_t)gB;
        const size_t full = ((size_t)w * h + 2 * (size_t)cw * ch) * (size_t)gB;
        if (full > cap) { cap = full; out = realloc(out, cap); ref = realloc(ref, cap); }
        av1dec_read_i420(&d, d.cur_surf, out, w, h);
        if (verbose) {   // what the engine wrote into the shown surface
            size_t ln = 0, cn = 0; const uint8_t *L = d.luma[d.cur_surf].cpu, *C = d.chroma[d.cur_surf].cpu;
            for (size_t i = 0; i < d.luma[d.cur_surf].size; i++) ln += L[i] != 0;
            for (size_t i = 0; i < d.chroma[d.cur_surf].size; i++) cn += C[i] != 0;
            printf("    surface %d: %zu nonzero luma bytes, %zu nonzero chroma bytes\n", d.cur_surf, ln, cn);
        }
        double ps = 0;
        if (!fseeko(rf, refpos, SEEK_SET) && fread(ref, 1, fs, rf) == fs) {
            ps = psnr(out, ref, (size_t)w * h);
            if (!mono) { const double pu = psnr(out + (size_t)w * h * gB, ref + (size_t)w * h * gB, ns - (size_t)w * h); if (pu < ps) ps = pu; }
        }
        refpos += (off_t)fs;
        if (getenv("AV1_DUMP") && shown == atoi(getenv("AV1_DUMP"))) {
            FILE *o = fopen("/tmp/av1dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
            o = fopen("/tmp/av1dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
        }
        if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
        if (ps < worst) worst = ps;
        if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: %s %.2f dB (status 0x%x)\n", shown, ps >= 99 ? "exact" : "DIFF", ps, d.err_status);
        av1dec_release(&d, d.cur_surf);
        shown++;
    }
    fseeko(rf, 0, SEEK_END); const off_t left = ftello(rf) - refpos;
    if (left) printf("reference has %lld bytes more than the frames decoded\n", (long long)left);
    printf("RESULT %d shown frames, %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms per temporal unit\n",
           shown, exact, first_bad, worst, decoded ? eng / decoded : 0);
    av1dec_close(&d);
    return exact == shown && shown > 0 ? 0 : 1;
}
