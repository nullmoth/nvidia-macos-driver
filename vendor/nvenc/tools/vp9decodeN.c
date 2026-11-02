// vp9decodeN.c (M10, T10.02/T10.03): decode a VP9 IVF stream with src/vp9dec.c and compare every shown frame with a
// reference decode (ffmpeg, I420, display order; scripts/make-vp9-streams.sh makes both).
//   ./vp9decodeN [-v] [-n frames] [-s WxH (at least this session size)] in.ivf ref.yuv
#include "vp9dec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static int gB = 1, gBD = 8;   // bytes per sample, bit depth
static double psnr(const uint8_t *a, const uint8_t *b, size_t n)   // n samples
{
    double se = 0;
    for (size_t i = 0; i < n; i++) { int d = gB == 1 ? a[i] - b[i] : (a[2 * i] | a[2 * i + 1] << 8) - (b[2 * i] | b[2 * i + 1] << 8); se += (double)d * d; }
    const double mx = (double)((1 << gBD) - 1);
    return se == 0 ? 99.0 : 10 * log10(mx * mx / (se / n));
}

int main(int argc, char **argv)
{
    int ai = 1, maxf = 1 << 30; bool verbose = false; unsigned minW = 0, minH = 0;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
        else if (!strcmp(argv[ai], "-s") && ai + 1 < argc) { ai++; minW = (unsigned)atoi(argv[ai]); minH = (unsigned)atoi(strchr(argv[ai], 'x') + 1); }
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: vp9decodeN [-v] [-n frames] in.ivf ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    FILE *rf = fopen(argv[ai + 1], "rb"); if (!rf) { perror(argv[ai + 1]); return 1; }
    uint8_t ih[32]; if (fread(ih, 1, 32, fp) != 32 || memcmp(ih, "DKIF", 4)) { fprintf(stderr, "not IVF\n"); return 1; }
    unsigned W = ih[12] | ih[13] << 8, H = ih[14] | ih[15] << 8; int bd = 0;
    {   // the largest frame (streams that resize grow beyond the IVF header's size)
        static Vp9State ps; vp9_init(&ps); uint8_t fh[12];
        while (fread(fh, 1, 12, fp) == 12) {
            const size_t sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
            uint8_t *buf = malloc(sz); if (fread(buf, 1, sz, fp) != sz) { free(buf); break; }
            size_t off[8], len[8]; const int nf = vp9_superframe(buf, sz, off, len);
            for (int i = 0; i < nf; i++) {
                Vp9Frame f; if (vp9_parse_frame(&ps, buf + off[i], len[i], &f)) continue;
                if (!f.show_existing_frame && !bd) bd = f.bit_depth;
                if (!f.show_existing_frame) { if ((unsigned)f.width > W) W = (unsigned)f.width; if ((unsigned)f.height > H) H = (unsigned)f.height; }
                vp9_end_frame(&ps, &f, NULL);
            }
            free(buf);
        }
        fseek(fp, 32, SEEK_SET);
        if (W < minW) W = minW;
        if (H < minH) H = minH;
    }
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    static Vp9Dec d; if (vp9dec_open(&d, &rm, W, H, bd ? bd : 8)) return 1;
    const int B = d.bps;   // bytes per sample in the reference file too
    gB = B; gBD = d.bit_depth;
    d.verbose = verbose;
    int shown = 0, decoded = 0, exact = 0, first_bad = -1; double worst = 99, eng = 0; off_t refpos = 0;
    uint8_t *out = NULL, *ref = NULL; size_t cap = 0;
    while (shown < maxf) {
        uint8_t fh[12]; if (fread(fh, 1, 12, fp) != 12) break;
        const size_t sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
        uint8_t *buf = malloc(sz); if (fread(buf, 1, sz, fp) != sz) break;
        size_t off[8], len[8]; const int nf = vp9_superframe(buf, sz, off, len);
        for (int i = 0; i < nf && shown < maxf; i++) {
            const int r = vp9dec_frame(&d, buf + off[i], len[i]);
            if (r < 0) { printf("decode error at frame %d (shown %d)\n", decoded, shown); goto done; }
            if (!d.f.show_existing_frame) { decoded++; eng += d.engine_ms; }
            if (r == 0) continue;
            const unsigned w = (unsigned)d.cur_w, h = (unsigned)d.cur_h, cw = (w + 1) / 2, ch = (h + 1) / 2;
            const size_t ns = (size_t)w * h + 2 * (size_t)cw * ch, fs = ns * (size_t)B;
            if (fs > cap) { cap = fs; out = realloc(out, cap); ref = realloc(ref, cap); }
            vp9dec_read_i420(&d, d.cur_surf, out, w, h);
            double ps = 0;
            if (!fseeko(rf, refpos, SEEK_SET) && fread(ref, 1, fs, rf) == fs) {
                ps = psnr(out, ref, (size_t)w * h);
                const double pu = psnr(out + (size_t)w * h * B, ref + (size_t)w * h * B, ns - (size_t)w * h);
                if (pu < ps) ps = pu;
            }
            refpos += (off_t)fs;
            if (getenv("VP9_DUMP") && shown == atoi(getenv("VP9_DUMP"))) {
                FILE *o = fopen("/tmp/vp9dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
                o = fopen("/tmp/vp9dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
            }
            if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
            if (ps < worst) worst = ps;
            if (verbose || (ps < 99 && shown - first_bad < 5))
                printf("shown %4d: %s %.2f dB (%s, status 0x%x)\n", shown, ps >= 99 ? "exact" : "DIFF", ps,
                       d.f.show_existing_frame ? "show_existing" : d.f.frame_type == VP9_KEY_FRAME ? "key" : "inter", d.err_status);
            vp9dec_release(&d, d.cur_surf);
            shown++;
        }
        free(buf);
    }
done:
    printf("RESULT %d shown frames (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/frame\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0);
    vp9dec_close(&d);
    return exact == shown && shown > 0 ? 0 : 1;
}
