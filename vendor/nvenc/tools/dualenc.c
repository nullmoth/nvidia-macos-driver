// dualenc.c: two encode sessions in one process, one per NVENC engine (M6, T6.01), each with frames in flight, one
// thread each. Reports per-session and total throughput and writes both streams for h264check.
//   ./dualenc W H N out0.h264 out1.h264 src.nv12 [ENGINE_A ENGINE_B]     (default engines 0 1; "0 0" = both on NVENC0)
#include "enc.h"
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
static int W, H, N;
static uint8_t *gY, *gUV;

typedef struct { int engine; const char *out; double ms; int fails; } Job;

static void *run(void *arg)
{
    Job *j = arg;
    Enc e; EncConfig c = { .width = W, .height = H, .qp_i = 26, .qp_p = 28, .num_refs = 1, .engine = j->engine, .depth = 2 };
    if (enc_open(&e, &c)) { j->fails = -1; return NULL; }
    FILE *f = fopen(j->out, "wb"); uint8_t hdr[256]; fwrite(hdr, 1, enc_headers(&e, hdr), f);
    NvU32 tk[64]; int head = 0, tail = 0;
    double t0 = now_ms();
    for (int i = 0; i < N; i++) {
        if (enc_inflight(&e) >= e.cfg.depth) { EncFrame fr; if (enc_wait(&e, tk[head++ % 64], &fr)) j->fails++; else fwrite(fr.data, 1, fr.size, f); }
        enc_load_nv12(&e, gY, W, gUV, W);
        if (enc_submit(&e, false, &tk[tail++ % 64])) j->fails++;
    }
    while (head < tail) { EncFrame fr; if (enc_wait(&e, tk[head++ % 64], &fr)) j->fails++; else fwrite(fr.data, 1, fr.size, f); }
    j->ms = now_ms() - t0;
    fclose(f); enc_close(&e);
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc < 7) { fprintf(stderr, "usage: dualenc W H N out0.h264 out1.h264 src.nv12 [ENGINE_A ENGINE_B]\n"); return 2; }
    W = atoi(argv[1]); H = atoi(argv[2]); N = atoi(argv[3]);
    gY = malloc((size_t)W * H); gUV = malloc((size_t)W * H / 2);
    for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) gY[r * W + c] = (uint8_t)(16 + ((c + r) & 127) + (((c >> 5) ^ (r >> 5)) & 1) * 60);
    for (size_t i = 0; i < (size_t)W * H / 2; i++) gUV[i] = (uint8_t)(128 + (i % 61) - 30);
    FILE *s = fopen(argv[6], "wb"); for (int i = 0; i < N; i++) { fwrite(gY, 1, (size_t)W * H, s); fwrite(gUV, 1, (size_t)W * H / 2, s); } fclose(s);
    Job j[2] = { { .engine = argc > 7 ? atoi(argv[7]) : 0, .out = argv[4] }, { .engine = argc > 8 ? atoi(argv[8]) : 1, .out = argv[5] } };
    // one session alone first, then both at once
    pthread_t t[2];
    pthread_create(&t[0], NULL, run, &j[0]); pthread_join(t[0], NULL);
    double alone = j[0].ms;
    double t0 = now_ms();
    for (int i = 0; i < 2; i++) pthread_create(&t[i], NULL, run, &j[i]);
    for (int i = 0; i < 2; i++) pthread_join(t[i], NULL);
    double both = now_ms() - t0;
    printf("%dx%d, %d frames per session: alone on NVENC%d %.2f ms/frame; together NVENC%d %.2f + NVENC%d %.2f ms/frame, "
           "total %.0f fps (alone %.0f fps); failures %d/%d\n", W, H, N, j[0].engine, alone / N, j[0].engine, j[0].ms / N,
           j[1].engine, j[1].ms / N, 2 * N * 1000 / both, N * 1000 / alone, j[0].fails, j[1].fails);
    return j[0].fails || j[1].fails;
}
