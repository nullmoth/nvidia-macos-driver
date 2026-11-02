// vp8decodeN.c (M14, T14.04): decode a VP8 IVF file with src/vp8dec.c and compare every shown frame with a reference
// decode (ffmpeg, yuv420p; VP8's reconstruction is exact, so the frames must match bit for bit).
//   ./vp8decodeN [-v] [-n frames] in.ivf ref.yuv
#include "vp8dec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static double psnr(const uint8_t *a, const uint8_t *b, size_t n)
{
    double se = 0;
    for (size_t i = 0; i < n; i++) { int d = a[i] - b[i]; se += (double)d * d; }
    return se == 0 ? 99.0 : 10 * log10(255.0 * 255.0 / (se / n));
}

int main(int argc, char **argv)
{
    int ai = 1, maxf = 1 << 30; bool verbose = false, vv = false;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-vv")) verbose = true, vv = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: vp8decodeN [-v] [-n frames] in.ivf ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"), *rf = fopen(argv[ai + 1], "rb");
    if (!fp || !rf) { perror("open"); return 1; }
    uint8_t ih[32];
    if (fread(ih, 1, 32, fp) != 32 || memcmp(ih, "DKIF", 4)) { fprintf(stderr, "not an IVF file\n"); return 1; }
    const unsigned hl = ih[6] | ih[7] << 8, W = ih[12] | ih[13] << 8, H = ih[14] | ih[15] << 8;
    fseek(fp, (long)hl, SEEK_SET);
    unsigned mw = W, mh = H;   // frames may be larger than the IVF header says (frame_size_change): scan the key frames
    { long here = ftell(fp); uint8_t fh[12], kb[10];
      while (fread(fh, 1, 12, fp) == 12) {
          const unsigned sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (unsigned)fh[3] << 24;
          if (sz >= 10 && fread(kb, 1, 10, fp) == 10) {
              if (!(kb[0] & 1)) { const unsigned w = (kb[6] | kb[7] << 8) & 0x3fff, h = (kb[8] | kb[9] << 8) & 0x3fff; if (w > mw) mw = w; if (h > mh) mh = h; }
              fseek(fp, (long)sz - 10, SEEK_CUR);
          } else fseek(fp, (long)sz, SEEK_CUR);
      }
      fseek(fp, here, SEEK_SET); }
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    static Vp8Dec d; if (vp8dec_open(&d, &rm, mw, mh)) return 1;
    d.verbose = vv ? 2 : verbose;
    uint8_t *buf = NULL, *out = NULL, *ref = NULL; size_t cap = 0, fcap = 0;
    int decoded = 0, shown = 0, exact = 0, first_bad = -1, errs = 0; double worst = 99, eng = 0;
    uint8_t fh[12];
    while (shown < maxf && fread(fh, 1, 12, fp) == 12) {
        const size_t sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
        if (sz > cap) { cap = sz; buf = realloc(buf, cap); }
        if (fread(buf, 1, sz, fp) != sz) break;
        if (!sz) continue;   // a dropped frame
        const int r = vp8dec_frame(&d, buf, sz);
        if (r < 0) { printf("decode error after %d frames\n", decoded); errs++; break; }
        if (r == 0) continue;
        decoded++; eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++;
        if (d.h.show) {
            const unsigned w = (unsigned)d.width, h = (unsigned)d.height;
            const size_t ys = (size_t)w * h, fs = ys + 2 * (size_t)((w + 1) / 2) * ((h + 1) / 2);
            if (fs > fcap) { fcap = fs; out = realloc(out, fcap); ref = realloc(ref, fcap); }
            vp8dec_read_i420(&d, d.cur_surf, out, w, h);
            double py = 0, pc = 0;
            if (fread(ref, 1, fs, rf) == fs) { py = psnr(out, ref, ys); pc = psnr(out + ys, ref + ys, fs - ys); }
            const double ps = py < pc ? py : pc;
            if (getenv("VP8_DUMP") && shown == atoi(getenv("VP8_DUMP"))) {
                FILE *o = fopen("/tmp/vp8dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
                o = fopen("/tmp/vp8dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
            }
            if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
            if (ps < worst) worst = ps;
            if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: %s %.2f dB (Y %.2f, C %.2f)\n", shown, ps >= 99 ? "exact" : "DIFF", ps, py, pc);
            shown++;
        }
        vp8dec_release(&d, d.cur_surf);
    }
    const long here = ftell(rf); fseek(rf, 0, SEEK_END); const long left = shown < maxf ? ftell(rf) - here : 0;
    printf("RESULT %d shown frames (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/frame%s%s\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0, errs ? " (engine errors)" : "",
           left > 0 ? " (the reference has more frames)" : left < 0 ? " (the reference has fewer frames)" : "");
    vp8dec_close(&d);
    return exact == shown && shown > 0 && !left ? 0 : 1;
}
