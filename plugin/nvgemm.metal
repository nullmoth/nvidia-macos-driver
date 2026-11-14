/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <metal_stdlib>
using namespace metal;

// MPSMatrix's MatrixMultiplyTN, tiled. The MPS kernel (one 4x4 block of D per thread, a scalar K loop, no threadgroup
// memory) computes, with both operands K-contiguous:
//   D[n][m] = alpha * sum_k A[m][k] * B[n][k]  +  (beta != 0 ? beta * C[n][m] : 0)
// for m < M, n < N; row r of A starts at A + r * ARowBytes, likewise B, C and D. C is not read when beta is 0.
// Read from the AIR of MatrixMultiplyTN (MPSMatrix.metallib, macOS 27.2).
struct nvgemm_params { uint M, N, K, ARowBytes, BRowBytes, CRowBytes, DRowBytes; float alpha, beta; };

constant uint GK = 16;

// One threadgroup = a BM (m) x BN (n) tile of D, 256 threads, each TM x TN outputs.
template <uint BM, uint BN, uint TM, uint TN>
static inline void nvgemm_body(device const float *A, device const float *B, device const float *C, device float *D,
                               constant nvgemm_params &p, uint2 tgp, uint lid,
                               threadgroup float (*As)[BM + 4], threadgroup float (*Bs)[BN + 4])
{
    const uint m0 = tgp.x * BM, n0 = tgp.y * BN;
    const uint tm = (lid % (BM / TM)) * TM, tn = (lid / (BM / TM)) * TN;
    const ulong lda = p.ARowBytes / 4, ldb = p.BRowBytes / 4, ldc = p.CRowBytes / 4, ldd = p.DRowBytes / 4;
    float acc[TM][TN];
    for (uint i = 0; i < TM; i++) for (uint j = 0; j < TN; j++) acc[i][j] = 0.0f;
    for (uint k0 = 0; k0 < p.K; k0 += GK) {
        for (uint i = lid; i < BM * GK; i += 256) {
            const uint r = i / GK, c = i % GK, m = m0 + r, k = k0 + c;
            As[c][r] = (m < p.M && k < p.K) ? A[m * lda + k] : 0.0f;
        }
        for (uint i = lid; i < BN * GK; i += 256) {
            const uint r = i / GK, c = i % GK, n = n0 + r, k = k0 + c;
            Bs[c][r] = (n < p.N && k < p.K) ? B[n * ldb + k] : 0.0f;
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint kk = 0; kk < GK; kk++) {
            float a[TM], b[TN];
            for (uint i = 0; i < TM; i++) a[i] = As[kk][tm + i];
            for (uint j = 0; j < TN; j++) b[j] = Bs[kk][tn + j];
            for (uint i = 0; i < TM; i++) for (uint j = 0; j < TN; j++) acc[i][j] = fma(a[i], b[j], acc[i][j]);
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    for (uint j = 0; j < TN; j++) {
        const uint n = n0 + tn + j;
        if (n >= p.N) continue;
        for (uint i = 0; i < TM; i++) {
            const uint m = m0 + tm + i;
            if (m >= p.M) continue;
            float v = acc[i][j] * p.alpha;
            // read before the write: when C is D (in place), each element is read and written by this thread only
            if (p.beta != 0.0f) v += C[n * ldc + m] * p.beta;
            D[n * ldd + m] = v;
        }
    }
}

// 64x64 tiles for large products; 32x32 tiles when 64x64 leaves too few threadgroups to fill the GPU.
kernel void nvgemm_tn64(device const float *A [[buffer(0)]], device const float *B [[buffer(1)]], device const float *C [[buffer(2)]],
                        device float *D [[buffer(3)]], constant nvgemm_params &p [[buffer(4)]],
                        uint2 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[GK][64 + 4]; threadgroup float Bs[GK][64 + 4]; nvgemm_body<64, 64, 4, 4>(A, B, C, D, p, tgp, lid, As, Bs); }

kernel void nvgemm_tn32(device const float *A [[buffer(0)]], device const float *B [[buffer(1)]], device const float *C [[buffer(2)]],
                        device float *D [[buffer(3)]], constant nvgemm_params &p [[buffer(4)]],
                        uint2 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[GK][32 + 4]; threadgroup float Bs[GK][32 + 4]; nvgemm_body<32, 32, 2, 2>(A, B, C, D, p, tgp, lid, As, Bs); }

// D[n][m] for m < M, n < N, compared between MPS's output (a) and ours (b), both with row stride ld floats: any element
// off by more than the tolerance counts. One atomic add per SIMD group, capped (as nvconv_cmp).
kernel void nvgemm_cmp(device const float *a [[buffer(5)]], device const float *b [[buffer(6)]], device atomic_uint *bad [[buffer(7)]],
                       constant uint4 &mnld [[buffer(8)]], uint gid [[thread_position_in_grid]], uint lane [[thread_index_in_simdgroup]])
{
    const uint M = mnld.x, N = mnld.y, ld = mnld.z;
    uint miss = 0;
    if (gid < M * N) {
        const uint m = gid % M, n = gid / M;
        const float x = a[(ulong)n * ld + m], y = b[(ulong)n * ld + m];
        miss = (fabs(x - y) <= 1e-3f * fabs(x) + 1e-4f) ? 0u : 1u;
    }
    const uint s = simd_sum(miss);
    if (lane == 0 && s && atomic_load_explicit(bad, memory_order_relaxed) < 4096u)
        atomic_fetch_add_explicit(bad, s, memory_order_relaxed);
}

kernel void nvgemm_copy(device const float *a [[buffer(5)]], device float *b [[buffer(6)]], constant uint &n [[buffer(8)]],
                        uint gid [[thread_position_in_grid]])
{
    if (gid < n) b[gid] = a[gid];
}
