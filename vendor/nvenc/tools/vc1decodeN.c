// vc1decodeN.c (M14, T14.03): decode a VC-1 stream with src/vc1dec.c - Simple / Main as RCV (SMPTE 421M Annex L, ffmpeg's
// vc1test format), Advanced as an elementary stream (.vc1) - and compare every output frame with a reference decode
// (ffmpeg, yuv420p, display order; VC-1's inverse transform is exact, so frames must match bit for bit).
//   ./vc1decodeN [-v] [-n frames] in.rcv|in.vc1 ref.yuv
#include "vc1dec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static double psnr(const uint8_t *a, const uint8_t *b, size_t n)
{
    double se = 0;
    for (size_t i = 0; i < n; i++) { int e = a[i] - b[i]; se += (double)e * e; }
    return se == 0 ? 99.0 : 10 * log10(255.0 * 255.0 / (se / n));
}

static Vc1Dec d;
static FILE *rf; static int shown, exact, first_bad = -1, maxf = 1 << 30; static double worst = 99; static bool verbose;
static uint8_t *out, *ref; static size_t cap;
static int held = -1, heldW, heldH;

static void output(int surf, unsigned w, unsigned h)
{
    const size_t ys = (size_t)w * h, fs = ys + 2 * (size_t)((w + 1) / 2) * ((h + 1) / 2);
    if (fs > cap) { cap = fs; out = realloc(out, cap); ref = realloc(ref, cap); }
    vc1dec_read_i420(&d, surf, out, w, h);
    double py = 0, pc = 0;
    if (fread(ref, 1, fs, rf) == fs) { py = psnr(out, ref, ys); pc = psnr(out + ys, ref + ys, fs - ys); }
    const double ps = py < pc ? py : pc;
    if (getenv("VC1_DUMP") && shown == atoi(getenv("VC1_DUMP"))) {
        FILE *o = fopen("/tmp/vc1dump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
        o = fopen("/tmp/vc1dump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
    }
    if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
    if (ps < worst) worst = ps;
    if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: %s %.2f dB (Y %.2f, C %.2f)\n", shown, ps >= 99 ? "exact" : "DIFF", ps, py, pc);
    shown++;
}

static void frame_done(void)
{
    if (d.cur_type == 2 || d.cur_type == 3) { output(d.cur_surf, (unsigned)d.cur_w, (unsigned)d.cur_h); vc1dec_release(&d, d.cur_surf); return; }
    if (held >= 0) { output(held, (unsigned)heldW, (unsigned)heldH); if (held != d.cur_surf) vc1dec_release(&d, held); }
    held = d.cur_surf; heldW = d.cur_w; heldH = d.cur_h;   // a skipped picture holds the same anchor again
}

int main(int argc, char **argv)
{
    int ai = 1; bool vv = false;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-vv")) verbose = vv = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxf = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: vc1decodeN [-v] [-n frames] in.rcv|in.vc1 ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    if (!(rf = fopen(argv[ai + 1], "rb"))) { perror(argv[ai + 1]); return 1; }
    fseek(fp, 0, SEEK_END); const size_t n = (size_t)ftell(fp); fseek(fp, 0, SEEK_SET);
    uint8_t *buf = malloc(n); if (fread(buf, 1, n, fp) != n) return 1;
    // RCV: frame count (24 bits), 0xC5, STRUCT_C size (>= 4) and STRUCT_C, STRUCT_A (height, width), 0x0C, STRUCT_B (12)
    const size_t cs = n > 8 ? (size_t)(buf[4] | buf[5] << 8 | buf[6] << 16 | (size_t)buf[7] << 24) : 0;
    const bool rcv = n > 36 && buf[3] == 0xc5 && cs >= 4 && cs < 64;
    unsigned W = 1920, H = 1088;
    if (rcv) { H = buf[8 + cs] | buf[9 + cs] << 8 | (unsigned)buf[10 + cs] << 16; W = buf[12 + cs] | buf[13 + cs] << 8 | (unsigned)buf[14 + cs] << 16; }
    else for (size_t i = 0; i + 8 < n; i++) if (!buf[i] && !buf[i + 1] && buf[i + 2] == 1 && buf[i + 3] == 0x0f) {   // max_coded size
        const unsigned v = (unsigned)buf[i + 6] << 16 | buf[i + 7] << 8 | buf[i + 8];   // after profile/level/colordiff/frmrtq/bitrtq/postproc (16 bits)
        W = ((v >> 12) + 1) * 2; H = ((v & 0xfff) + 1) * 2; break;
    }
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    if (vc1dec_open(&d, &rm, W, H)) return 1;
    d.verbose = vv ? 2 : verbose;
    int decoded = 0, errs = 0; double eng = 0;
    if (rcv) {
        if (vc1dec_simple_main_header(&d, buf + 8, (int)W, (int)H)) return 1;
        size_t pos = 32 + cs;
        while (shown < maxf && pos + 8 <= n) {
            const size_t sz = (buf[pos] | buf[pos + 1] << 8 | (size_t)buf[pos + 2] << 16); pos += 8;
            if (pos + sz > n) break;
            const int r = vc1dec_simple_main_frame(&d, buf + pos, sz); pos += sz;
            if (r < 0) { printf("decode error after %d frames\n", decoded); errs++; break; }
            if (r == 1) { decoded++; if (d.cur_shown) { eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++; } frame_done(); }
        }
    } else {
        size_t pos = 0;
        for (;;) {
            size_t i = pos; while (i + 3 < n && !(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)) i++;
            if (i + 3 >= n || shown >= maxf) break;
            size_t e = i + 4; while (e + 2 < n && !(buf[e] == 0 && buf[e + 1] == 0 && buf[e + 2] == 1)) e++;
            if (e + 2 >= n) e = n;
            const int r = vc1dec_advanced_unit(&d, buf + i, e - i); pos = e;
            if (r < 0) { printf("decode error after %d frames\n", decoded); errs++; break; }
            if (r == 1) { decoded++; if (d.cur_shown) { eng += d.engine_ms; if (d.err_status || d.mbs_err) errs++; } frame_done(); }
        }
        if (shown < maxf && vc1dec_flush(&d) == 1) { decoded++; frame_done(); }
    }
    if (held >= 0 && shown < maxf) output(held, (unsigned)heldW, (unsigned)heldH);
    const long here = ftell(rf); fseek(rf, 0, SEEK_END); const long left = shown < maxf ? ftell(rf) - here : 0;
    printf("RESULT %d shown frames (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/frame%s%s\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0, errs ? " (engine errors)" : "",
           left > 0 ? " (the reference has more frames)" : left < 0 ? " (the reference has fewer frames)" : "");
    vc1dec_close(&d);
    return exact == shown && shown > 0 && !left ? 0 : 1;
}
