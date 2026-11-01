// mpeg2decodeN.c (M14, T14.01): decode an MPEG-1 / MPEG-2 video elementary stream with src/mpeg2dec.c and compare every
// output frame with a reference decode (ffmpeg, yuv420p, display order).
//   ./mpeg2decodeN [-v] [-n frames] in.m2v ref.yuv
#include "mpeg2dec.h"
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

static Mpeg2Dec d;
static FILE *rf; static int shown, exact, first_bad = -1, maxf = 1 << 30; static double worst = 99; static bool verbose;
static uint8_t *out, *ref; static size_t cap;
static int held = -1, heldTref, heldW, heldH;   // the newest anchor frame (and its size), output when the next anchor arrives

static void output(int surf, int tref, unsigned w, unsigned h)
{
    const size_t ys = (size_t)w * h, fs = ys + 2 * (size_t)((w + 1) / 2) * ((h + 1) / 2);
    if (fs > cap) { cap = fs; out = realloc(out, cap); ref = realloc(ref, cap); }
    mpeg2dec_read_i420(&d, surf, out, w, h);
    // MPEG2_SKIP_SHOWN=k: frame k is not compared and uses no reference frame (ffmpeg drops the last anchor of a sequence
    // when the next one changes the size, MEI.stream17)
    if (getenv("MPEG2_SKIP_SHOWN") && shown == atoi(getenv("MPEG2_SKIP_SHOWN"))) {
        if (verbose) printf("shown %4d: tref %4d not compared\n", shown, tref);
        exact++; mpeg2dec_release(&d, surf); shown++; return;
    }
    double py = 0, pc = 0;
    if (fread(ref, 1, fs, rf) == fs) { py = psnr(out, ref, ys); pc = psnr(out + ys, ref + ys, fs - ys); }
    const double ps = py < pc ? py : pc;
    if (getenv("MPEG2_DUMP") && shown == atoi(getenv("MPEG2_DUMP"))) {
        FILE *o = fopen("/tmp/m2dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
        o = fopen("/tmp/m2dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
    }
    if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
    if (ps < worst) worst = ps;
    if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: tref %4d %s %.2f dB (Y %.2f, C %.2f)\n", shown, tref, ps >= 99 ? "exact" : "DIFF", ps, py, pc);
    mpeg2dec_release(&d, surf);
    shown++;
}

static int dropped;
static void drop_skipped(void)   // frames the decoder left out (broken links): ffmpeg shows something for them, skip those
{
    const unsigned w = (unsigned)(d.cur_w ? d.cur_w : d.seq.width), h = (unsigned)(d.cur_h ? d.cur_h : d.seq.height);
    const long fs = (long)w * h + 2L * ((w + 1) / 2) * ((h + 1) / 2);
    for (; dropped < d.skipped; dropped++) { fseek(rf, fs, SEEK_CUR); if (verbose) printf("skipped a frame without references\n"); }
}

static void frame_done(void)
{
    if (d.cur_type == 3) { output(d.cur_surf, d.cur_tref, (unsigned)d.cur_w, (unsigned)d.cur_h); return; }
    if (held >= 0) output(held, heldTref, (unsigned)heldW, (unsigned)heldH);
    held = d.cur_surf; heldTref = d.cur_tref; heldW = d.cur_w; heldH = d.cur_h;
}

int main(int argc, char **argv)
{
    int ai = 1;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: mpeg2decodeN [-v] [-n frames] in.m2v ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    if (!(rf = fopen(argv[ai + 1], "rb"))) { perror(argv[ai + 1]); return 1; }
    fseek(fp, 0, SEEK_END); const size_t n = (size_t)ftell(fp); fseek(fp, 0, SEEK_SET);
    uint8_t *buf = malloc(n); if (fread(buf, 1, n, fp) != n) return 1;
    unsigned W = 0, H = 0;   // the largest sequence header (with its extension)
    { size_t pos = 0; int code; const uint8_t *data; size_t len; unsigned w = 0, h = 0;
      while (mpeg2_next_unit(buf, n, &pos, &code, &data, &len)) {
          if (code == 0xb3 && len >= 3) { w = (unsigned)(data[0] << 4 | data[1] >> 4); h = (unsigned)((data[1] & 15) << 8 | data[2]); }
          else if (code == 0xb5 && len >= 3 && (data[0] >> 4) == 1) { w |= (unsigned)((data[1] & 1) << 1 | data[2] >> 7) << 12; h |= (unsigned)(data[2] >> 5 & 3) << 12; }
          else continue;
          if (w > W) W = w;
          if (h > H) H = h;
      } }
    if (!W) { fprintf(stderr, "no sequence header\n"); return 1; }
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    if (mpeg2dec_open(&d, &rm, W, H)) return 1;
    d.verbose = verbose;
    int decoded = 0, errs = 0; bool failed = false; double eng = 0;
    size_t pos = 0; int code; const uint8_t *data; size_t len;
    while (shown < maxf && mpeg2_next_unit(buf, n, &pos, &code, &data, &len)) {
        const int r = mpeg2dec_unit(&d, code, data, len);
        if (r < 0) { printf("decode error after %d frames\n", decoded); errs++; failed = true; break; }
        if (r == 1) { decoded++; eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++; frame_done(); }
        if (d.skipped > dropped && !d.skipField2) drop_skipped();
    }
    if (!failed && mpeg2dec_flush(&d) == 1) { decoded++; eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++; frame_done(); }
    if (held >= 0 && shown < maxf) output(held, heldTref, (unsigned)heldW, (unsigned)heldH);
    const long here = ftell(rf); fseek(rf, 0, SEEK_END); const long left = shown < maxf ? ftell(rf) - here : 0;
    printf("RESULT %d shown frames (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/frame%s%s\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0, errs ? " (engine errors)" : "",
           left > 0 ? " (the reference has more frames)" : left < 0 ? " (the reference has fewer frames)" : "");
    mpeg2dec_close(&d);
    return exact == shown && shown > 0 && !left ? 0 : 1;
}
