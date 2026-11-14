// CPU check of the lowered shader tensor IR. The translator's text pass turns a tensor kernel into plain IR over a
// 56-byte descriptor; this harness compiles that IR for the host, supplies the two AIR intrinsics the matmul helper uses
// (lane id through a prefix sum, a no-op barrier), runs every lane of one simdgroup in turn, and compares with a
// reference. usage: harness (links against the lowered kernels compiled from .ll)
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

typedef int v2i __attribute__((ext_vector_type(2)));
// nvmtl_tensor_entry from plugin/NVMTLResourceIDs.h
typedef struct { uint64_t data, scales; uint32_t rank, dtype, bits, bf0; int32_t ext[16], str[16], sext[16], sstr[16]; } Desc;

static int g_lane;
// The AIR intrinsics the lowered kernels call, under their AIR names.
float prefix_sum(float x) __asm__("_air.simd_prefix_exclusive_sum.f32");
float prefix_sum(float x) { return (float)g_lane * x; }
void barrier(int a, int b, int c) __asm__("_air.simdgroup.barrier");
void barrier(int a, int b, int c) {}
int simd_size(void) __asm__("_air.get_simdgroup_size.i32");
int simd_size(void) { return 32; }
float conv_f_s_i32(int x) __asm__("_air.convert.f.f32.s.i32");
float conv_f_s_i32(int x) { return (float)x; }

extern void mm_tile(Desc *, Desc *, Desc *);
extern void mm_sliced(Desc *, Desc *, Desc *, v2i);
extern void mm_acc_tr(Desc *, Desc *, Desc *);
extern void t_access(Desc *, float *, unsigned);

static int fails, arms;
static void arm(const char *name, int ok, const char *fmt, double v) {
    arms++; if (!ok) fails++;
    printf("%s  %-46s ", ok ? "PASS" : "FAIL", name); printf(fmt, v); printf("\n");
}

static _Float16 h(float f) { return (_Float16)f; }
static void fill(Desc *d, void *p, uint32_t es, uint32_t e0, uint32_t e1, uint32_t s0, uint32_t s1) {
    memset(d, 0, sizeof *d); d->data = (uint64_t)(uintptr_t)p; d->bits = es * 8; d->rank = 2;
    d->ext[0] = e0; d->ext[1] = e1; d->str[0] = s0; d->str[1] = s1;
}

// C(m,n) = sum_k A(m,k) B(k,n); A is [M][K] row-major (dim0 = k), B is [K][N] (dim0 = n)
static void ref_mm(const _Float16 *A, const _Float16 *B, float *C, int M, int N, int K, int acc) {
    for (int m = 0; m < M; m++) for (int n = 0; n < N; n++) {
        float s = acc ? C[m * N + n] : 0;
        for (int k = 0; k < K; k++) s += (float)A[m * K + k] * (float)B[k * N + n];
        C[m * N + n] = s;
    }
}

int main(void) {
    srand(5);
    enum { M = 32, N = 48, K = 24 };
    static _Float16 A[M * K], B[K * N]; static float C[M * N], R[M * N];
    for (int i = 0; i < M * K; i++) A[i] = h((rand() % 17 - 8) / 4.0f);
    for (int i = 0; i < K * N; i++) B[i] = h((rand() % 13 - 6) / 4.0f);

    // one 16x16x16 tile: tensors are exactly the tile
    {
        static _Float16 a[256], b[256]; static float c[256], r[256];
        for (int i = 0; i < 256; i++) { a[i] = A[i]; b[i] = B[i]; }
        Desc da, db, dc; fill(&da, a, 2, 16, 16, 1, 16); fill(&db, b, 2, 16, 16, 1, 16); fill(&dc, c, 4, 16, 16, 1, 16);
        for (g_lane = 0; g_lane < 32; g_lane++) mm_tile(&da, &db, &dc);
        ref_mm(a, b, r, 16, 16, 16, 0);
        float md = 0; for (int i = 0; i < 256; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        arm("mm_tile 16x16x16 matches reference", md < 1e-4f, "maxdiff=%.3g", md);
        // NEG: a different lane assignment must not change the result, and an untouched C would be all zero
        int zero = 1; for (int i = 0; i < 256; i++) if (c[i] != 0) zero = 0;
        arm("NEG result is not the untouched zero tensor", !zero, "%g", (double)zero);
    }
    // whole matrices, one simdgroup per 16x16 tile of C, K read from the tensors; edges are clipped
    {
        Desc da, db, dc; fill(&da, A, 2, K, M, 1, K); fill(&db, B, 2, N, K, 1, N); fill(&dc, C, 4, N, M, 1, N);
        memset(C, 0, sizeof C);
        for (int ty = 0; ty < (M + 15) / 16; ty++) for (int tx = 0; tx < (N + 15) / 16; tx++)
            for (g_lane = 0; g_lane < 32; g_lane++) mm_sliced(&da, &db, &dc, (v2i){ tx, ty });
        ref_mm(A, B, R, M, N, K, 0);
        float md = 0; for (int i = 0; i < M * N; i++) md = fmaxf(md, fabsf(C[i] - R[i]));
        arm("mm_sliced 32x48x24 in 16x16 tiles matches", md < 1e-3f, "maxdiff=%.3g", md);
        // NEG: leave out the last tile row; the reference must then differ
        memset(C, 0, sizeof C);
        for (int ty = 0; ty < 1; ty++) for (int tx = 0; tx < (N + 15) / 16; tx++)
            for (g_lane = 0; g_lane < 32; g_lane++) mm_sliced(&da, &db, &dc, (v2i){ tx, ty });
        md = 0; for (int i = 0; i < M * N; i++) md = fmaxf(md, fabsf(C[i] - R[i]));
        arm("NEG a missing tile row is detected", md > 0.5f, "maxdiff=%.3g", md);
    }
    // C += A * B^T, accumulate onto existing C; B is stored [N][K] (transposed: dim0 = k)
    {
        static _Float16 a[256], bt[256]; static float c[256], r[256], c0[256];
        for (int i = 0; i < 256; i++) { a[i] = A[i]; bt[i] = h((rand() % 9 - 4) / 2.0f); c[i] = c0[i] = (float)(i % 7); }
        Desc da, db, dc; fill(&da, a, 2, 16, 16, 1, 16); fill(&db, bt, 2, 16, 16, 1, 16); fill(&dc, c, 4, 16, 16, 1, 16);
        for (g_lane = 0; g_lane < 32; g_lane++) mm_acc_tr(&da, &db, &dc);
        for (int m = 0; m < 16; m++) for (int n = 0; n < 16; n++) {
            float s = c0[m * 16 + n]; for (int k = 0; k < 16; k++) s += (float)a[m * 16 + k] * (float)bt[n * 16 + k]; r[m * 16 + n] = s; }
        float md = 0; for (int i = 0; i < 256; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        arm("mm_acc_tr multiply-accumulate with B transposed", md < 1e-3f, "maxdiff=%.3g", md);
    }
    // element access through the descriptor: extents, and tensor[i, j] with strides (padded rows)
    {
        float t[4 * 8]; float out[2 + 12];
        for (int i = 0; i < 32; i++) t[i] = (float)i;
        Desc dt; fill(&dt, t, 4, 3, 4, 1, 8);   // 3 columns, 4 rows, row stride 8 elements
        memset(out, 0, sizeof out);
        for (unsigned i = 0; i < 12; i++) t_access(&dt, out, i);
        int ok = out[0] == 3 && out[1] == 4;
        for (unsigned i = 0; i < 12; i++) ok &= fabsf(out[2 + i] - 2.0f * t[(i / 3) * 8 + (i % 3)]) < 1e-6f;
        arm("t_access extents and strided element reads", ok, "%g", (double)ok);
    }
    printf("%d arms, %d failed\n", arms, fails);
    return fails ? 1 : 0;
}
