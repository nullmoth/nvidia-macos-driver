/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <metal_stdlib>
using namespace metal;

constant uint BK = 16;

template <uint BM, uint BN, uint TM, uint TN>
static inline void nvconv_body(device const float *src ,
                         device const float *wts ,
                         device float *dst , device const float *bias, device const float *res, uint epi,
                         constant uint *kp ,
                         constant uint *cp ,
                         uint3 tgp, uint lid, threadgroup float (*As)[BM + 4], threadgroup float (*Bs)[BN + 4])
{
    const uint cin = cp[32], cout = cp[33], kw = cp[34], kh = cp[35], batch = cp[37];
    const uint sw = cp[38], sh = cp[39], dw = cp[40], dh = cp[41];
    const int offx = as_type<int>(cp[44]), offy = as_type<int>(cp[45]);
    const uint strx = cp[46], stry = cp[47], dilx = cp[48], dily = cp[49];
    const uint sC = kp[8], sW = kp[9], sH = kp[10], sN = kp[11];
    const uint wCo = kp[28], wCi = kp[29], wKw = kp[30], wKh = kp[31];
    const uint ob0 = res ? 80u : bias ? 60u : 40u;
    const uint oC = kp[ob0 + 8], oW = kp[ob0 + 9], oH = kp[ob0 + 10], oN = kp[ob0 + 11];

    const uint M = batch * dh * dw, N = cout, K = kh * kw * cin;
    const uint m0 = tgp.x * BM, n0 = tgp.y * BN;
    const bool fast = (cin % BK) == 0;

    const uint ACNT = BK * BM / 256, akk = lid % BK, amm0 = lid / BK;
    uint am_n[ACNT]; int am_bx[ACNT], am_by[ACNT]; bool am_ok[ACNT];
    for (uint i = 0; i < ACNT; i++) {
        const uint am = m0 + amm0 + 16 * i;
        am_ok[i] = am < M;
        const uint aox = am % dw, at2 = am / dw, aoy = at2 % dh;
        am_n[i] = at2 / dh; am_bx[i] = int(aox * strx) + offx; am_by[i] = int(aoy * stry) + offy;
    }
    const uint BCNT = BK * BN / 256, bkk = lid / (BN / BCNT), bnn = (lid % (BN / BCNT)) * BCNT;

    const uint tx = lid % 16, ty = lid / 16;
    float acc[TM][TN] = {{0}};

    for (uint k0 = 0; k0 < K; k0 += BK) {
        uint tci0 = 0, tix = 0, tiy = 0;
        if (fast) { tci0 = k0 % cin; const uint t = k0 / cin; tix = t % kw; tiy = t / kw; }
        {
            const uint k = k0 + akk;
            uint ci = 0, ix = 0, iy = 0;
            const bool kok = k < K;
            if (kok) {
                if (fast) { ci = tci0 + akk; ix = tix; iy = tiy; }
                else { ci = k % cin; const uint t = k / cin; ix = t % kw; iy = t / kw; }
            }
            for (uint i = 0; i < ACNT; i++) {
                float v = 0.0f;
                if (kok && am_ok[i]) {
                    const int x = am_bx[i] + int(ix * dilx), y = am_by[i] + int(iy * dily);
                    if (x >= 0 && y >= 0 && uint(x) < sw && uint(y) < sh) v = src[ci * sC + uint(x) * sW + uint(y) * sH + am_n[i] * sN];
                }
                As[akk][amm0 + 16 * i] = v;
            }
        }
        {
            const uint k = k0 + bkk;
            float v[4] = {0, 0, 0, 0};
            if (k < K) {
                uint ci, ix, iy;
                if (fast) { ci = tci0 + bkk; ix = tix; iy = tiy; }
                else { ci = k % cin; const uint t = k / cin; ix = t % kw; iy = t / kw; }
                const uint base = ci * wCi + ix * wKw + iy * wKh;
                for (uint j = 0; j < BCNT; j++) { const uint co = n0 + bnn + j; v[j] = co < N ? wts[co * wCo + base] : 0.0f; }
            }
            for (uint j = 0; j < BCNT; j++) Bs[bkk][bnn + j] = v[j];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint kk = 0; kk < BK; kk++) {
            float a[TM], b[TN];
            for (uint i = 0; i < TM; i++) a[i] = As[kk][ty * TM + i];
            for (uint j = 0; j < TN; j++) b[j] = Bs[kk][tx * TN + j];
            for (uint i = 0; i < TM; i++) for (uint j = 0; j < TN; j++) acc[i][j] = fma(a[i], b[j], acc[i][j]);
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    for (uint i = 0; i < TM; i++) {
        const uint m = m0 + ty * TM + i;
        if (m >= M) continue;
        const uint ox = m % dw, t2 = m / dw, oy = t2 % dh, n = t2 / dh;
        const uint ob = ox * oW + oy * oH + n * oN;
        for (uint j = 0; j < TN; j++) {
            const uint co = n0 + tx * TN + j;
            if (co >= N) continue;
            float v = acc[i][j];
            if (epi == 4) { v = v > 0.0f ? v : v * (bias ? bias[co] : 0.0f); }
            else {
                v += bias ? bias[co] : 0.0f;
                if (epi == 1 || epi == 5) v = max(v, 0.0f); else if (epi == 2) v = clamp(v, 0.0f, 6.0f); else if (epi == 3) v = 1.0f / (1.0f + exp(-v));
            }
            // 5 and 6: a residual block that adds its own input back (input + ReLU(conv + bias), input + conv + bias).
            // Solved from a dumped Super Resolution layer, where MPS matched 5 to 4.5e-7 on all 147,456 outputs.
            // Only an output the same size and channel count as its input can be one; verification decides the rest.
            if ((epi == 5 || epi == 6) && co < cin && ox < sw && oy < sh) v += src[co * sC + ox * sW + oy * sH + n * sN];
            if (res) v += res[co * oC + ob];
            dst[co * oC + ob] = v;
        }
    }
}

kernel void nvconv3_b(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device float *dst [[buffer(2)]],
                      constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][128 + 4]; threadgroup float Bs[BK][64 + 4]; nvconv_body<128, 64, 8, 4>(src, wts, dst, nullptr, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv3_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device float *dst [[buffer(2)]],
                      constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][64 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<64, 32, 4, 2>(src, wts, dst, nullptr, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv4_b(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device float *dst [[buffer(3)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][128 + 4]; threadgroup float Bs[BK][64 + 4]; nvconv_body<128, 64, 8, 4>(src, wts, dst, bias, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv4_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device float *dst [[buffer(3)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][64 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<64, 32, 4, 2>(src, wts, dst, bias, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv5_b(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device const float *res [[buffer(3)]], device float *dst [[buffer(4)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]],
                      constant uint &epi [[buffer(30)]], uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][128 + 4]; threadgroup float Bs[BK][64 + 4]; nvconv_body<128, 64, 8, 4>(src, wts, dst, bias, res, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv5_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device const float *res [[buffer(3)]], device float *dst [[buffer(4)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]],
                      constant uint &epi [[buffer(30)]], uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][64 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<64, 32, 4, 2>(src, wts, dst, bias, res, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv_cmp(device const float *a [[buffer(5)]], device const float *b [[buffer(6)]], device atomic_uint *bad [[buffer(7)]],
                       constant uint &n [[buffer(8)]], device float *sample [[buffer(9)]], uint gid [[thread_position_in_grid]],
                       uint lane [[thread_index_in_simdgroup]])
{
    // A variant that mismatches everywhere had every thread add to the same counter: 295k serialized atomics took
    // ~100 ms per compare on an RTX 5060 (14.6 s of GPU across one Geekbench run), and one add per SIMD group still
    // took 109 ms on a 9.8M-element output. The verdict only needs "any mismatch", so the count stops near 4096
    // (the log then reads it as a floor): one add per SIMD group, skipped once the counter is past the cap.
    uint m = 0;
    if (gid < n) {
        const float x = a[gid], y = b[gid];
        if (gid < 6) { sample[gid * 2] = x; sample[gid * 2 + 1] = y; }
        m = (fabs(x - y) <= 1e-3f * fabs(x) + 1e-4f) ? 0u : 1u;
    }
    const uint s = simd_sum(m);
    if (lane == 0 && s && atomic_load_explicit(bad, memory_order_relaxed) < 4096u)
        atomic_fetch_add_explicit(bad, s, memory_order_relaxed);
}
// Copies n floats from buffer 5 to buffer 6. Used only by the NVMTL_NVCONV_DUMP diagnostic, which moves a refused
// layer's input, weights and both outputs into CPU-readable buffers so its rule can be solved offline.
kernel void nvconv_copy(device const float *a [[buffer(5)]], device float *b [[buffer(6)]], constant uint &n [[buffer(8)]],
                        uint gid [[thread_position_in_grid]])
{
    if (gid < n) b[gid] = a[gid];
}
// 32x32 tiles: for small layers (e.g. a 48x48x64 -> 64 3x3 residual block) the 64x32 tiles leave only 72 threadgroups
// for a 30-SM GPU; these give 4x as many output tiles per layer.
kernel void nvconv3_t(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device float *dst [[buffer(2)]],
                      constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][32 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<32, 32, 2, 2>(src, wts, dst, nullptr, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv4_t(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device float *dst [[buffer(3)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][32 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<32, 32, 2, 2>(src, wts, dst, bias, nullptr, epi, kp, cp, tgp, lid, As, Bs); }
kernel void nvconv5_t(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device const float *res [[buffer(3)]], device float *dst [[buffer(4)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]],
                      constant uint &epi [[buffer(30)]], uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][32 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<32, 32, 2, 2>(src, wts, dst, bias, res, epi, kp, cp, tgp, lid, As, Bs); }
