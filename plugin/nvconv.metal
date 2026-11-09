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
                         uint3 tgp, uint lid, threadgroup float (*As)[BM + 4], threadgroup float (*Bs)[BN + 4],
                         uint nsplit = 1, device float *part = nullptr)
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
    // epi's high byte is a fused depth-to-space factor r (1 = none). MPS writes Super Resolution's 48x48x64 -> 1024
    // conv straight into a 192x192x64 image: channel co = (dy*r + dx)*(cout/r^2) + c lands at (oy*r + dy, ox*r + dx, c).
    // Solved from its dump (nvcdump4 shuf), max abs error 8.9e-7 against MPS; verification still decides at run time.
    const uint shuf = max(epi >> 8, 1u), shC = cout / (shuf * shuf);
    epi &= 0xffu;
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

    // split-K: grid z picks a BK-aligned slice of the reduction; each slice writes raw sums to part[z][m][n] and
    // nvconv_reduce adds the slices and applies the epilogue. nsplit 1 = the whole reduction, epilogue here.
    const uint kChunk = ((K + nsplit - 1) / nsplit + BK - 1) / BK * BK;
    const uint kBeg = tgp.z * kChunk, kEnd = min(K, kBeg + kChunk);
    for (uint k0 = kBeg; k0 < kEnd; k0 += BK) {
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
        // A runtime trip count (exact: rows past K were zero-filled) keeps NIR from unrolling all 16 steps; unrolled,
        // the scheduler hoisted all 96 shared loads ahead of 256 FMAs and the kernel needed 152 registers (12 warps/SM);
        // as a loop it needs 64 (32 warps/SM).
        const uint kkN = min(BK, kEnd - k0);
        for (uint kk = 0; kk < kkN; kk++) {
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
            if (part) { part[(tgp.z * M + m) * N + co] = acc[i][j]; continue; }
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
            // the residual has its OWN strides (array 3, words 68..71): a concat-branch output is strided (Super
            // Resolution's 48x48 layer writes 64 of 256 channels per pixel) while its residual input stays dense, and
            // reading it with the output's strides was the whole of that layer's refusal (dump 43204-68, numpy exact)
            if (res) v += res[co * kp[68] + ox * kp[69] + oy * kp[70] + n * kp[71]];
            if (shuf > 1) {
                const uint q = co / shC, c = co % shC;
                dst[c * oC + (ox * shuf + q % shuf) * oW + (oy * shuf + q / shuf) * oH + n * oN] = v;
            } else {
                dst[co * oC + ob] = v;
            }
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

// split-K kernels: 32x32 tiles, grid z = nsplit slices of the reduction, raw sums to part (buffer 7).
kernel void nvconv4_k(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device float *dst [[buffer(3)]], device float *part [[buffer(7)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]],
                      constant uint &epi [[buffer(30)]], constant uint &nsplit [[buffer(28)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][32 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<32, 32, 2, 2>(src, wts, dst, bias, nullptr, epi, kp, cp, tgp, lid, As, Bs, nsplit, part); }
kernel void nvconv5_k(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                      device const float *res [[buffer(3)]], device float *dst [[buffer(4)]], device float *part [[buffer(7)]], constant uint *kp [[buffer(23)]],
                      constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]], constant uint &nsplit [[buffer(28)]],
                      uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]])
{ threadgroup float As[BK][32 + 4]; threadgroup float Bs[BK][32 + 4]; nvconv_body<32, 32, 2, 2>(src, wts, dst, bias, res, epi, kp, cp, tgp, lid, As, Bs, nsplit, part); }
// Adds the split-K slices for one output and applies the same epilogue as nvconv_body (bias, activation, residual).
// form = number of arrays (4: src, wts, bias, dst; 5: + res). One thread per output element, gid = m * N + co.
kernel void nvconv_reduce(device const float *src [[buffer(0)]], device const float *bias [[buffer(2)]], device const float *res [[buffer(3)]],
                          device float *dst4 [[buffer(4)]], device float *dst3 [[buffer(5)]], device const float *part [[buffer(7)]],
                          constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                          constant uint &nsplit [[buffer(28)]], constant uint &form [[buffer(27)]], uint gid [[thread_position_in_grid]])
{
    const uint cin = cp[32], cout = cp[33], batch = cp[37], sw = cp[38], sh = cp[39], dw = cp[40], dh = cp[41];
    const uint sC = kp[8], sW = kp[9], sH = kp[10], sN = kp[11];
    const uint ob0 = form == 5 ? 80u : 60u;
    const uint oC = kp[ob0 + 8], oW = kp[ob0 + 9], oH = kp[ob0 + 10], oN = kp[ob0 + 11];
    const uint M = batch * dh * dw, N = cout;
    if (gid >= M * N) return;
    const uint m = gid / N, co = gid % N;
    const uint ep = epi & 0xffu;   // the driver sends the depth-to-space factor in the byte above (split-K never runs with one)
    float v = 0.0f;
    for (uint z = 0; z < nsplit; z++) v += part[(z * M + m) * N + co];
    const uint ox = m % dw, t2 = m / dw, oy = t2 % dh, n = t2 / dh;
    const uint ob = ox * oW + oy * oH + n * oN;
    if (ep == 4) { v = v > 0.0f ? v : v * bias[co]; }
    else {
        v += bias[co];
        if (ep == 1 || ep == 5) v = max(v, 0.0f); else if (ep == 2) v = clamp(v, 0.0f, 6.0f); else if (ep == 3) v = 1.0f / (1.0f + exp(-v));
    }
    if ((ep == 5 || ep == 6) && co < cin && ox < sw && oy < sh) v += src[co * sC + ox * sW + oy * sH + n * sN];
    if (form == 5) v += res[co * kp[68] + ox * kp[69] + oy * kp[70] + n * kp[71]];   // residual's own strides, as above
    device float *dst = form == 5 ? dst4 : dst3;
    dst[co * oC + ob] = v;
}

// Tensor-core variant of nvconv_body<64, 32, 4, 2>: same operands, tiling, split-K and epilogue. Each fp32 operand is
// split into a half pair (hi = half(v), lo = half(v - hi)) and C += Ahi*Bhi + Ahi*Blo + Alo*Bhi in fp32, which keeps
// ~21 bits of each product where a single half product keeps 11; the per-layer verification against MPS still decides.
static inline void nvconv_mma_body(device const float *src, device const float *wts, device float *dst,
                                   device const float *bias, device const float *res, uint epi,
                                   constant uint *kp, constant uint *cp, uint3 tgp, uint lid, uint sg,
                                   threadgroup half *Ah, threadgroup half *Al,
                                   threadgroup half *Bh, threadgroup half *Bl,
                                   threadgroup float *Cs, uint nsplit = 1, device float *part = nullptr)
{
    constexpr uint BM = 64, BN = 32, TM = 4, TN = 2;
    const uint cin = cp[32], cout = cp[33], kw = cp[34], kh = cp[35], batch = cp[37];
    const uint sw = cp[38], sh = cp[39], dw = cp[40], dh = cp[41];
    const int offx = as_type<int>(cp[44]), offy = as_type<int>(cp[45]);
    const uint strx = cp[46], stry = cp[47], dilx = cp[48], dily = cp[49];
    const uint sC = kp[8], sW = kp[9], sH = kp[10], sN = kp[11];
    const uint wCo = kp[28], wCi = kp[29], wKw = kp[30], wKh = kp[31];
    const uint ob0 = res ? 80u : bias ? 60u : 40u;
    const uint oC = kp[ob0 + 8], oW = kp[ob0 + 9], oH = kp[ob0 + 10], oN = kp[ob0 + 11];
    const uint M = batch * dh * dw, N = cout, K = kh * kw * cin;
    const uint shuf = max(epi >> 8, 1u), shC = cout / (shuf * shuf);
    epi &= 0xffu;
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
    // simdgroup sg owns output rows sg*8..sg*8+7 and all four 8-column tiles
    simdgroup_float8x8 acc[4];
    for (uint t = 0; t < 4; t++) acc[t] = simdgroup_float8x8(0.0f);
    const uint kChunk = ((K + nsplit - 1) / nsplit + BK - 1) / BK * BK;
    const uint kBeg = tgp.z * kChunk, kEnd = min(K, kBeg + kChunk);
    for (uint k0 = kBeg; k0 < kEnd; k0 += BK) {
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
                const half h = half(v);
                Ah[(amm0 + 16 * i) * (BK + 8) + akk] = h; Al[(amm0 + 16 * i) * (BK + 8) + akk] = half(v - float(h));
            }
        }
        {
            const uint k = k0 + bkk;
            float v[BCNT];
            for (uint j = 0; j < BCNT; j++) v[j] = 0.0f;
            if (k < K) {
                uint ci, ix, iy;
                if (fast) { ci = tci0 + bkk; ix = tix; iy = tiy; }
                else { ci = k % cin; const uint t = k / cin; ix = t % kw; iy = t / kw; }
                const uint base = ci * wCi + ix * wKw + iy * wKh;
                for (uint j = 0; j < BCNT; j++) { const uint co = n0 + bnn + j; v[j] = co < N ? wts[co * wCo + base] : 0.0f; }
            }
            for (uint j = 0; j < BCNT; j++) { const half h = half(v[j]); Bh[bkk * 40 + bnn + j] = h; Bl[bkk * 40 + bnn + j] = half(v[j] - float(h)); }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // each pair of 8-deep products is one split term over k 0..7 and 8..15, so the two fuse into one k16 tensor op
        simdgroup_half8x8 ah0, ah1, al0, al1;
        simdgroup_load(ah0, Ah + sg * 8 * (BK + 8), BK + 8); simdgroup_load(ah1, Ah + sg * 8 * (BK + 8) + 8, BK + 8);
        simdgroup_load(al0, Al + sg * 8 * (BK + 8), BK + 8); simdgroup_load(al1, Al + sg * 8 * (BK + 8) + 8, BK + 8);
        for (uint t = 0; t < 4; t++) {
            simdgroup_half8x8 bh0, bh1, bl0, bl1;
            simdgroup_load(bh0, Bh + t * 8, 40); simdgroup_load(bh1, Bh + 8 * 40 + t * 8, 40);
            simdgroup_load(bl0, Bl + t * 8, 40); simdgroup_load(bl1, Bl + 8 * 40 + t * 8, 40);
            simdgroup_multiply_accumulate(acc[t], al0, bh0, acc[t]); simdgroup_multiply_accumulate(acc[t], al1, bh1, acc[t]);
            simdgroup_multiply_accumulate(acc[t], ah0, bl0, acc[t]); simdgroup_multiply_accumulate(acc[t], ah1, bl1, acc[t]);
            simdgroup_multiply_accumulate(acc[t], ah0, bh0, acc[t]); simdgroup_multiply_accumulate(acc[t], ah1, bh1, acc[t]);
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    for (uint t = 0; t < 4; t++) simdgroup_store(acc[t], Cs + sg * 8 * 36 + t * 8, 36);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    const uint tx = lid % 16, ty = lid / 16;
    for (uint i = 0; i < TM; i++) {
        const uint m = m0 + ty * TM + i;
        if (m >= M) continue;
        const uint ox = m % dw, t2 = m / dw, oy = t2 % dh, n = t2 / dh;
        const uint ob = ox * oW + oy * oH + n * oN;
        for (uint j = 0; j < TN; j++) {
            const uint co = n0 + tx * TN + j;
            if (co >= N) continue;
            float v = Cs[(ty * TM + i) * 36 + tx * TN + j];
            if (part) { part[(tgp.z * M + m) * N + co] = v; continue; }
            if (epi == 4) { v = v > 0.0f ? v : v * (bias ? bias[co] : 0.0f); }
            else {
                v += bias ? bias[co] : 0.0f;
                if (epi == 1 || epi == 5) v = max(v, 0.0f); else if (epi == 2) v = clamp(v, 0.0f, 6.0f); else if (epi == 3) v = 1.0f / (1.0f + exp(-v));
            }
            if ((epi == 5 || epi == 6) && co < cin && ox < sw && oy < sh) v += src[co * sC + ox * sW + oy * sH + n * sN];
            if (res) v += res[co * kp[68] + ox * kp[69] + oy * kp[70] + n * kp[71]];
            if (shuf > 1) {
                const uint q = co / shC, c = co % shC;
                dst[c * oC + (ox * shuf + q % shuf) * oW + (oy * shuf + q / shuf) * oH + n * oN] = v;
            } else dst[co * oC + ob] = v;
        }
    }
}
kernel void nvconvm3_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device float *dst [[buffer(2)]],
                       constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                       uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]], uint sg [[simdgroup_index_in_threadgroup]])
{ threadgroup half Ah[64 * (BK + 8)], Al[64 * (BK + 8)], Bh[BK * 40], Bl[BK * 40]; threadgroup float Cs[64 * 36];
  nvconv_mma_body(src, wts, dst, nullptr, nullptr, epi, kp, cp, tgp, lid, sg, Ah, Al, Bh, Bl, Cs); }
kernel void nvconvm4_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                       device float *dst [[buffer(3)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]], constant uint &epi [[buffer(30)]],
                       uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]], uint sg [[simdgroup_index_in_threadgroup]])
{ threadgroup half Ah[64 * (BK + 8)], Al[64 * (BK + 8)], Bh[BK * 40], Bl[BK * 40]; threadgroup float Cs[64 * 36];
  nvconv_mma_body(src, wts, dst, bias, nullptr, epi, kp, cp, tgp, lid, sg, Ah, Al, Bh, Bl, Cs); }
kernel void nvconvm5_s(device const float *src [[buffer(0)]], device const float *wts [[buffer(1)]], device const float *bias [[buffer(2)]],
                       device const float *res [[buffer(3)]], device float *dst [[buffer(4)]], constant uint *kp [[buffer(23)]], constant uint *cp [[buffer(29)]],
                       constant uint &epi [[buffer(30)]], uint3 tgp [[threadgroup_position_in_grid]], uint lid [[thread_index_in_threadgroup]], uint sg [[simdgroup_index_in_threadgroup]])
{ threadgroup half Ah[64 * (BK + 8)], Al[64 * (BK + 8)], Bh[BK * 40], Bl[BK * 40]; threadgroup float Cs[64 * 36];
  nvconv_mma_body(src, wts, dst, bias, res, epi, kp, cp, tgp, lid, sg, Ah, Al, Bh, Bl, Cs); }
