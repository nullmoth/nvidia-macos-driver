// nv12pgm.c: write the luma of frames from a tightly packed NV12 file as PGM, scaled down by S (box filter).
//   ./nv12pgm W H S in.nv12 outprefix frame...      writes outprefix<frame>.pgm
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv)
{
    if (argc < 7) { fprintf(stderr, "usage: nv12pgm W H S in.nv12 outprefix frame...\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]), S = atoi(argv[3]); FILE *f = fopen(argv[4], "rb"); if (!f) { perror(argv[4]); return 1; }
    size_t fs = (size_t)W * H + (size_t)W * ((H + 1) / 2); unsigned char *y = malloc((size_t)W * H);
    for (int a = 6; a < argc; a++) {
        int n = atoi(argv[a]);
        if (fseeko(f, (off_t)fs * n, SEEK_SET) || fread(y, 1, (size_t)W * H, f) != (size_t)W * H) { fprintf(stderr, "frame %d: short\n", n); continue; }
        char p[512]; snprintf(p, sizeof p, "%s%d.pgm", argv[5], n); FILE *o = fopen(p, "wb");
        int w = W / S, h = H / S; fprintf(o, "P5 %d %d 255\n", w, h);
        for (int r = 0; r < h; r++) for (int c = 0; c < w; c++) {
            unsigned sum = 0; for (int i = 0; i < S; i++) for (int j = 0; j < S; j++) sum += y[(size_t)(r * S + i) * W + c * S + j];
            fputc(sum / (S * S), o);
        }
        fclose(o);
    }
    return 0;
}
