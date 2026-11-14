/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Curve geometry for the acceleration structure build (NVMTLCurves.m): one thread per segment reads the segment's
// control points and radii from the app's buffers, wherever they live, and writes the segment's box (min xyz, max xyz)
// and its record (control points as (x, y, z, radius), basis, control points per segment, type, end caps) for the
// translator's curve intersector. Built with: xcrun metal -target air64-apple-macos15.7 -c nvcurve.metal; xcrun
// metallib; xxd -i into nvcurve_lib.h with this header.
#include <metal_stdlib>
using namespace metal;

struct nvcurve_params {
    uint nseg, n, ncp, basis, type, caps;
    uint cstride, rstride, cfmt, rfmt, index16, pad;   // cfmt: 0 float3 1 float4 2 float2 3 half3 4 half4 5 half2; rfmt: 0 float 1 half
};

static float3 read_point(device const uchar *p, uint fmt) {
    switch (fmt) {
    case 0: case 1: return float3(((device const packed_float3 *)p)[0]);
    case 2: return float3(((device const packed_float2 *)p)[0], 0);
    case 5: return float3(float2(((device const packed_half2 *)p)[0]), 0);
    default: return float3(((device const packed_half3 *)p)[0]);
    }
}

kernel void nvmtl_curve_build(device const uchar *cp [[buffer(0)]], device const uchar *rad [[buffer(1)]],
                              device const uchar *idx [[buffer(2)]], device float *boxes [[buffer(3)]],
                              device uint *records [[buffer(4)]], constant nvcurve_params &pr [[buffer(5)]],
                              uint s [[thread_position_in_grid]]) {
    if (s >= pr.nseg) return;
    const uint first = pr.index16 ? uint(((device const ushort *)idx)[s]) : ((device const uint *)idx)[s];
    device uint *rec = records + s * 20;
    float4 pt[4] = { float4(0), float4(0), float4(0), float4(0) };
    if (first + pr.n > pr.ncp) {
        // an out-of-range segment: an empty box no ray enters and a zero record
        for (uint k = 0; k < 20; k++) rec[k] = 0;
        for (uint c = 0; c < 6; c++) boxes[s * 6 + c] = c < 3 ? 1.0f : -1.0f;
        return;
    }
    for (uint k = 0; k < pr.n; k++) {
        const uint i = first + k;
        const float r = pr.rfmt ? float(((device const half *)(rad + i * pr.rstride))[0]) : ((device const float *)(rad + i * pr.rstride))[0];
        pt[k] = float4(read_point(cp + i * pr.cstride, pr.cfmt), r);
    }
    for (uint k = 0; k < 4; k++) for (uint c = 0; c < 4; c++) rec[k * 4 + c] = as_type<uint>(pt[k][c]);
    rec[16] = pr.basis; rec[17] = pr.n; rec[18] = pr.type; rec[19] = pr.caps;
    // the box holds the whole tube: the control points' hull grown by the largest radius (a Catmull-Rom segment can
    // leave its control points' hull and is bounded by its Bezier points instead)
    float4 h[4] = { pt[0], pt[1], pt[2], pt[3] };
    if (pr.basis == 1 && pr.n == 4) {
        h[0] = pt[1]; h[1] = pt[1] + (pt[2] - pt[0]) / 6; h[2] = pt[2] - (pt[3] - pt[1]) / 6; h[3] = pt[2];
    }
    float3 lo = h[0].xyz, hi = h[0].xyz; float rmax = fabs(h[0].w);
    for (uint k = 1; k < pr.n; k++) { lo = min(lo, h[k].xyz); hi = max(hi, h[k].xyz); rmax = max(rmax, fabs(h[k].w)); }
    for (uint c = 0; c < 3; c++) { boxes[s * 6 + c] = lo[c] - rmax; boxes[s * 6 + 3 + c] = hi[c] + rmax; }
}
