// hevcdecodeN.c (M11, T11.02-T11.04): decode an Annex B HEVC stream with src/hevcdec.c and compare every output picture
// with a reference decode (ffmpeg, display order, its own pixel format: planar, 16-bit samples above 8 bits, luma only for
// monochrome; scripts/make-hevc-streams.sh and fetch-hevc-vectors.sh make both).
//   ./hevcdecodeN [-v] [-n pictures] in.hevc ref.yuv
#include "hevcdec.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int gB = 1, gBD = 8;
static double psnr(const uint8_t *a, const uint8_t *b, size_t n)   // n samples
{
    double se = 0;
    for (size_t i = 0; i < n; i++) { int d = gB == 1 ? a[i] - b[i] : (a[2 * i] | a[2 * i + 1] << 8) - (b[2 * i] | b[2 * i + 1] << 8); se += (double)d * d; }
    const double mx = (double)((1 << gBD) - 1);
    return se == 0 ? 99.0 : 10 * log10(mx * mx / (se / n));
}

static HevcDec d;
static FILE *rf; static off_t refpos;
static int shown, exact, first_bad = -1, maxp = 1 << 30; static double worst = 99; static bool verbose;
static uint8_t *out, *ref; static size_t cap;
typedef struct { int surf, poc, w, h, crop[4]; } Pending;
static Pending pend[HEVC_SURFACES]; static int npend;

static void output_one(void)
{
    int k = 0;
    for (int i = 1; i < npend; i++) if (pend[i].poc < pend[k].poc) k = i;
    const Pending p = pend[k]; pend[k] = pend[--npend];
    const unsigned w = (unsigned)p.w, h = (unsigned)p.h;
    const int sx = d.chroma_format == 3 ? 0 : 1, sy = d.chroma_format == 1 ? 1 : 0;
    const size_t cw = (w + (unsigned)sx) >> sx, ch = (h + (unsigned)sy) >> sy, ns = (size_t)w * h + 2 * cw * ch;
    const size_t fs = (d.chroma_format == 0 ? (size_t)w * h : ns) * (size_t)gB;
    if (ns * (size_t)gB > cap) { cap = ns * (size_t)gB; out = realloc(out, cap); ref = realloc(ref, cap); }
    hevcdec_read_yuv(&d, p.surf, out, p.crop, w, h);
    double ps = 0, py = 0, pc = 99;
    if (!fseeko(rf, refpos, SEEK_SET) && fread(ref, 1, fs, rf) == fs) {
        ps = py = psnr(out, ref, (size_t)w * h);
        if (d.chroma_format) { pc = psnr(out + (size_t)w * h * gB, ref + (size_t)w * h * gB, ns - (size_t)w * h); if (pc < ps) ps = pc; }
    }
    refpos += (off_t)fs;
    if (getenv("HEVC_DUMP") && shown == atoi(getenv("HEVC_DUMP"))) {
        FILE *o = fopen("/tmp/hevcdump-ours.yuv", "wb"); fwrite(out, 1, fs, o); fclose(o);
        o = fopen("/tmp/hevcdump-ref.yuv", "wb"); fwrite(ref, 1, fs, o); fclose(o);
        o = fopen("/tmp/hevcdump-chroma.raw", "wb"); fwrite(d.chroma[p.surf].cpu, 1, d.chroma[p.surf].size, o); fclose(o);
        o = fopen("/tmp/hevcdump-luma.raw", "wb"); fwrite(d.luma[p.surf].cpu, 1, d.luma[p.surf].size, o); fclose(o);
    }
    if (ps >= 99) exact++; else if (first_bad < 0) first_bad = shown;
    if (ps < worst) worst = ps;
    if (verbose || (ps < 99 && shown - first_bad < 5)) printf("shown %4d: POC %4d %s %.2f dB (Y %.2f, C %.2f)\n", shown, p.poc, ps >= 99 ? "exact" : "DIFF", ps, py, pc);
    hevcdec_release(&d, p.surf);
    shown++;
}

static int latency[HEVC_SURFACES];   // PicLatencyCount of each picture waiting for output
static void output_one_l(void) { int k = 0; for (int i = 1; i < npend; i++) if (pend[i].poc < pend[k].poc) k = i;
                                 for (int i = k; i < npend - 1; i++) latency[i] = latency[i + 1]; output_one(); }

static void picture(void)   // a picture was decoded: output and removal as the DPB does (C.5.2.2, C.5.2.3)
{
    if (d.cur_irap_nro) {                                      // all prior pictures leave: output or discarded
        if (d.cur_no_output_prior) while (npend) hevcdec_release(&d, pend[--npend].surf);
        else while (npend) output_one_l();
    } else for (;;) {                                          // "bumping" before the current picture
        int dpb = npend;
        for (int i = 0; i < HEVC_SURFACES; i++) if (d.ref[i] && i != d.cur_surf) {
            bool p = false; for (int k = 0; k < npend; k++) p |= pend[k].surf == i;
            dpb += !p;
        }
        bool lat = false; for (int k = 0; k < npend; k++) lat |= d.max_latency && latency[k] >= d.max_latency;
        if (npend && (npend > d.max_reorder || lat || dpb >= d.max_dec_pic)) output_one_l(); else break;
    }
    for (int k = 0; k < npend; k++) latency[k]++;
    if (d.cur_output) {
        latency[npend] = 0;
        pend[npend++] = (Pending){ d.cur_surf, d.cur_poc, d.cur_w, d.cur_h, { d.cur_crop[0], d.cur_crop[1], d.cur_crop[2], d.cur_crop[3] } };
        for (;;) {                                             // C.5.2.3 "additional bumping"
            bool lat = false; for (int k = 0; k < npend; k++) lat |= d.max_latency && latency[k] >= d.max_latency;
            if (npend > d.max_reorder || lat) output_one_l(); else break;
        }
    } else hevcdec_release(&d, d.cur_surf);
    while (npend > HEVC_SURFACES / 2) output_one_l();
}

int main(int argc, char **argv)
{
    int ai = 1;
    for (; ai < argc && argv[ai][0] == '-'; ai++) {
        if (!strcmp(argv[ai], "-v")) verbose = true;
        else if (!strcmp(argv[ai], "-n") && ai + 1 < argc) maxp = atoi(argv[++ai]);
    }
    if (argc - ai < 2) { fprintf(stderr, "usage: hevcdecodeN [-v] [-n pictures] in.hevc ref.yuv\n"); return 2; }
    FILE *fp = fopen(argv[ai], "rb"); if (!fp) { perror(argv[ai]); return 1; }
    if (!(rf = fopen(argv[ai + 1], "rb"))) { perror(argv[ai + 1]); return 1; }
    fseek(fp, 0, SEEK_END); const size_t n = (size_t)ftell(fp); fseek(fp, 0, SEEK_SET);
    uint8_t *buf = malloc(n); if (fread(buf, 1, n, fp) != n) return 1;
    // the session: the largest SPS, its bit depth and chroma format
    unsigned W = 0, H = 0; int bd = 0, cf = -1;
    { static HevcParams p; const uint8_t *nal; size_t len;
      for (size_t pos = 0; hevc_next_annexb(buf, n, &pos, &nal, &len);) {
          if (((nal[0] >> 1) & 63) != HEVC_NAL_SPS || (nal[0] & 1) || (nal[1] >> 3)) continue;
          const int id = hevc_parse_sps(&p, nal, len); if (id < 0) continue;
          const HevcSps *s = &p.sps[id];
          if ((unsigned)s->width > W) W = (unsigned)s->width;
          if ((unsigned)s->height > H) H = (unsigned)s->height;
          if (!bd) { bd = s->bit_depth_luma; cf = s->chroma_format_idc; }
      } }
    if (!bd) { fprintf(stderr, "no SPS\n"); return 1; }
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    if (hevcdec_open(&d, &rm, W, H, bd, cf)) return 1;
    gB = d.bps; gBD = bd; d.verbose = verbose;
    int decoded = 0, errs = 0; bool failed = false; double eng = 0;
    const uint8_t *nal; size_t len;
    for (size_t pos = 0; shown < maxp && hevc_next_annexb(buf, n, &pos, &nal, &len);) {
        const int r = hevcdec_nal(&d, nal, len);
        if (r < 0) { printf("decode error after %d pictures\n", decoded); errs++; failed = true; break; }
        if (r == 1) { decoded++; eng += d.engine_ms; if (d.err_status) errs++; picture(); }
    }
    if (!failed && hevcdec_flush(&d) == 1) { decoded++; eng += d.engine_ms; if (d.err_status) errs++; picture(); }
    while (npend && shown < maxp) output_one();
    fseeko(rf, 0, SEEK_END); const off_t left = shown < maxp ? ftello(rf) - refpos : 0;   // reference pictures we did not output
    printf("RESULT %d shown pictures (%d decoded), %d bit-exact, first difference at %d, worst %.2f dB; engine %.2f ms/picture%s%s\n",
           shown, decoded, exact, first_bad, worst, decoded ? eng / decoded : 0, errs ? " (engine errors)" : "",
           left > 0 ? " (the reference has more pictures)" : left < 0 ? " (the reference has fewer pictures)" : "");
    hevcdec_close(&d);
    return exact == shown && shown > 0 && !left ? 0 : 1;
}
