// mpeg4decodeN.c (M14, T14.02): decode an MPEG-4 Part 2 elementary stream (.m4v) or an H.263 stream (.h263) with
// src/mpeg4dec.c and compare every output frame with a reference decode (ffmpeg, yuv420p, display order). Not-coded VOPs
// are not output (as ffmpeg does).
//   ./mpeg4decodeN [-v] [-n frames] in.m4v|in.h263 ref.yuv
#include "mpeg4dec.h"
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

static Mpeg4Dec d;
static FILE *rf; static int shown, exact, first_bad = -1, maxf = 1 << 30; static double worst = 99; static bool verbose, vv;
static uint8_t *out, *ref; static size_t cap;
static int held = -1, heldTref, heldW, heldH;   // the newest anchor frame (and its size), output when the next anchor arrives

static void output(int surf, int tref, unsigned w, unsigned h)
{
    const size_t ys = (size_t)w * h, fs = ys + 2 * (size_t)((w + 1) / 2) * ((h + 1) / 2);
    if (fs > cap) { cap = fs; out = realloc(out, cap); ref = realloc(ref, cap); }
    mpeg4dec_read_i420(&d, surf, out, w, h);
    // MPEG4_SKIP_SHOWN=k: frame k is not compared and uses no reference frame (ffmpeg drops the last anchor of a sequence
    // when the next one changes the size, MEI.stream17)
    if (getenv("MPEG4_SKIP_SHOWN") && shown == atoi(getenv("MPEG4_SKIP_SHOWN"))) {
        if (verbose) printf("shown %4d: tref %4d not compared\n", shown, tref);
        exact++; mpeg4dec_release(&d, surf); shown++; return;
    }
    double py = 0, pc = 0;
    if (fread(ref, 1, fs, rf) == fs) { py = psnr(out, ref, ys); pc = psnr(out + ys, ref + ys, fs - ys); }
    const double ps = py < pc ? py : pc;
    if (getenv("MPEG4_DUMP") && shown == atoi(getenv("MPEG4_DUMP"))) {
        FILE *o = fopen("/tmp/m4dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
        o = fopen("/tmp/m4dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
    }
    if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
    if (ps < worst) worst = ps;
    if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: tref %4d %s %.2f dB (Y %.2f, C %.2f)\n", shown, tref, ps >= 99 ? "exact" : "DIFF", ps, py, pc);
    mpeg4dec_release(&d, surf);
    shown++;
}

static void frame_done(void)
{
    if (!d.cur_shown) { mpeg4dec_release(&d, d.cur_surf); return; }   // not coded
    if (d.cur_type == 2) { output(d.cur_surf, (int)d.cur_time, (unsigned)d.cur_w, (unsigned)d.cur_h); return; }
    if (held >= 0) output(held, heldTref, (unsigned)heldW, (unsigned)heldH);
    held = d.cur_surf; heldTref = (int)d.cur_time; heldW = d.cur_w; heldH = d.cur_h;
}

int main(int argc, char **argv)
{
    int ai = 1;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-vv")) verbose = true, vv = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: mpeg4decodeN [-v] [-n frames] in.m4v|in.h263 ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    if (!(rf = fopen(argv[ai + 1], "rb"))) { perror(argv[ai + 1]); return 1; }
    fseek(fp, 0, SEEK_END); const size_t n = (size_t)ftell(fp); fseek(fp, 0, SEEK_SET);
    uint8_t *buf = malloc(n); if (fread(buf, 1, n, fp) != n) return 1;
    const bool h263 = strstr(argv[ai], ".h263") != NULL;
    unsigned W = 1920, H = 1088;   // the session: 1080p (MPEG4_MAXW / MPEG4_MAXH for larger)
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    if (getenv("MPEG4_MAXW")) { W = (unsigned)atoi(getenv("MPEG4_MAXW")); H = (unsigned)atoi(getenv("MPEG4_MAXH")); }
    if (mpeg4dec_open(&d, &rm, W, H)) return 1;
    d.verbose = vv ? 2 : verbose;
    int decoded = 0, errs = 0; bool failed = false; double eng = 0;
    size_t pos = 0; const uint8_t *st; size_t len;
    while (shown < maxf && (h263 ? h263_next_picture(buf, n, &pos, &st, &len) : mpeg4_next_unit(buf, n, &pos, &st, &len))) {
        const int r = h263 ? mpeg4dec_h263_picture(&d, st, len) : mpeg4dec_unit(&d, st, len);
        if (r < 0) { printf("decode error after %d frames\n", decoded); errs++; failed = true; break; }
        if (r == 1) { decoded++; eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++; frame_done(); }
    }
    (void)failed;
    if (held >= 0 && shown < maxf) output(held, heldTref, (unsigned)heldW, (unsigned)heldH);
    const long here = ftell(rf); fseek(rf, 0, SEEK_END); const long left = shown < maxf ? ftell(rf) - here : 0;
    printf("RESULT %d shown frames (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/frame%s%s\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0, errs ? " (engine errors)" : "",
           left > 0 ? " (the reference has more frames)" : left < 0 ? " (the reference has fewer frames)" : "");
    mpeg4dec_close(&d);
    return exact == shown && shown > 0 && !left ? 0 : 1;
}
