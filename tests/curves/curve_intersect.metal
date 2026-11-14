// The driver's built-in curve intersector: a float port of curve_model.c, matched on an Apple GPU against
// reference_m1.txt, reference_caps_m1.txt and reference_oblique_m1.txt. gen_curve_ll.sh turns it into the LLVM IR the
// translator embeds.
#include <metal_stdlib>
using namespace metal;
// basis = MTLCurveBasis: 0 B-spline, 1 Catmull-Rom, 2 linear, 3 Bezier; n control points per segment (2..4)
static float4 weights(uint basis, uint n, float u) {
    float s = 1 - u;
    if (basis == 2) return float4(s, u, 0, 0);
    if (basis == 0 && n == 3) return float4(0.5f*s*s, 0.5f + u*s, 0.5f*u*u, 0);
    if (basis == 0) return float4(s*s*s/6, (3*u*u*u - 6*u*u + 4)/6, (-3*u*u*u + 3*u*u + 3*u + 1)/6, u*u*u/6);
    if (basis == 1) return float4(0.5f*(-u*u*u + 2*u*u - u), 0.5f*(3*u*u*u - 5*u*u + 2), 0.5f*(-3*u*u*u + 4*u*u + u), 0.5f*(u*u*u - u*u));
    return float4(s*s*s, 3*u*s*s, 3*u*u*s, u*u*u);
}
static float4 eval(float4 p0, float4 p1, float4 p2, float4 p3, uint basis, uint n, float u) {
    float4 w = weights(basis, n, u);
    return p0*w.x + p1*w.y + p2*w.z + p3*w.w;
}
// distance along the ray to the front of the sphere at u, 1e30 where the ray misses it
static float front(float4 p0, float4 p1, float4 p2, float4 p3, uint basis, uint n, float3 o, float3 d, float idd, float u) {
    float4 c = eval(p0, p1, p2, p3, basis, n, u);
    float tc = dot(c.xyz - o, d) * idd;
    float3 q = o + d*tc - c.xyz; float d2 = dot(q, q), r2 = c.w*c.w;
    return d2 > r2 ? 1e30f : tc - sqrt((r2 - d2) * idd);
}
// caps = MTLCurveEndCaps: 0 none, 1 disk, 2 sphere. Returns (hit, t, u, 0) for the nearest hit in [tmin, tmax].
// A round linear segment is a cone frustum (radius measured across the axis).
static float4 round_linear(float4 p0, float4 p1, uint caps, float3 o, float3 d, float tmin, float tmax) {
    float3 ax = p1.xyz - p0.xyz; float L = length(ax); float3 a = ax / L; float k = (p1.w - p0.w) / L, r0 = p0.w;
    float3 w = o - p0.xyz; float dd = dot(d, d), da = dot(d, a), s0 = dot(w, a), wd = dot(w, d);
    float A = dd - da*da - k*k*da*da, B = 2*(wd - s0*da - k*da*(r0 + k*s0)), C = dot(w, w) - s0*s0 - (r0 + k*s0)*(r0 + k*s0);
    float bt = 1e30f, bu = -1, disc = B*B - 4*A*C;
    if (disc >= 0 && fabs(A) > 1e-12f) {
        float q = sqrt(disc);
        for (int i = 0; i < 2; i++) {
            float t = (-B + (i ? q : -q)) / (2*A), sa = s0 + t*da;
            if (2*A*t + B >= 0 || r0 + k*sa < 0 || sa < 0 || sa > L || t < tmin || t > tmax) continue;
            if (t < bt) { bt = t; bu = sa / L; }
        }
    }
    for (int end = 0; end < 2 && caps != 0; end++) {
        float3 c = end ? p1.xyz : p0.xyz; float rad = end ? p1.w : p0.w;
        if (caps == 1) {
            if (fabs(da) < 1e-12f) continue;
            float th = dot(c - o, a) / da; float3 qv = o + d*th - c;
            if (dot(qv, qv) <= rad*rad && th >= tmin && th <= tmax && th < bt) { bt = th; bu = end; }
        } else {
            float3 oc = o - c; float b2 = dot(oc, d), c2 = dot(oc, oc) - rad*rad, dsc = b2*b2 - dd*c2;
            if (dsc < 0) continue;
            float th = (-b2 - sqrt(dsc)) / dd, sa = s0 + th*da;
            if ((end ? sa > L : sa < 0) && th >= tmin && th <= tmax && th < bt) { bt = th; bu = end; }
        }
    }
    return bu < 0 ? float4(0) : float4(1, bt, bu, 0);
}
// A round curve: the ray enters the tube where the front distance t(u) is at a local minimum. One between the end
// planes is a body hit; one beyond an end plane is the open end (no hit), its disk, or its sphere.
static float4 round_curve(float4 p0, float4 p1, float4 p2, float4 p3, uint basis, uint n, uint caps, float3 o, float3 d, float tmin, float tmax) {
    if (basis == 2) return round_linear(p0, p1, caps, o, d, tmin, tmax);
    float idd = 1.0f / dot(d, d);
    float4 c0 = eval(p0, p1, p2, p3, basis, n, 0), c1 = eval(p0, p1, p2, p3, basis, n, 1);
    float3 t0 = eval(p0, p1, p2, p3, basis, n, 1e-3f).xyz - c0.xyz, t1 = c1.xyz - eval(p0, p1, p2, p3, basis, n, 1 - 1e-3f).xyz;
    const int N = 32; float bt = 1e30f, bu = -1;
    float tp = 1e30f, tc = front(p0, p1, p2, p3, basis, n, o, d, idd, 0);
    for (int k = 0; k <= N; k++) {
        float tn = k < N ? front(p0, p1, p2, p3, basis, n, o, d, idd, float(k + 1) / N) : 1e30f;
        if (tc < 1e29f && (k == 0 || tp >= tc) && (k == N || tn >= tc)) {
            float a = max(float(k - 1) / N, 0.0f), b = min(float(k + 1) / N, 1.0f), mt = tc, mu = float(k) / N;
            const float g = 0.6180339887f;
            for (int it = 0; it < 24; it++) {
                float x1 = b - g * (b - a), x2 = a + g * (b - a);
                float f1 = front(p0, p1, p2, p3, basis, n, o, d, idd, x1), f2 = front(p0, p1, p2, p3, basis, n, o, d, idd, x2);
                if (f1 < mt) { mt = f1; mu = x1; }
                if (f2 < mt) { mt = f2; mu = x2; }
                if (f1 < f2) b = x2; else a = x1;
            }
            float3 hp = o + d*mt;
            bool before = dot(hp - c0.xyz, t0) < 0, after = dot(hp - c1.xyz, t1) > 0;
            float ht = mt, hu = mu; bool ok = true;
            if (before || after) {
                hu = before ? 0 : 1;
                if (caps == 0) ok = false;
                else if (caps == 1) {
                    float3 c = before ? c0.xyz : c1.xyz, t = before ? t0 : t1; float rad = before ? c0.w : c1.w, dn = dot(d, t);
                    if (fabs(dn) < 1e-12f) ok = false;
                    else { ht = dot(c - o, t) / dn; float3 q = o + d*ht - c; ok = dot(q, q) <= rad*rad; }
                }
            }
            if (ok && ht >= tmin && ht <= tmax && ht < bt) { bt = ht; bu = hu; }
        }
        tp = tc; tc = tn;
    }
    return bu < 0 ? float4(0) : float4(1, bt, bu, 0);
}
// A flat curve: a ribbon facing the ray, cut into 16 straight pieces as Apple cuts it; sphere caps add the half-disk of
// the end radius beyond each end, disk caps change nothing.
static float4 flat_curve(float4 p0, float4 p1, float4 p2, float4 p3, uint basis, uint n, uint caps, float3 o, float3 d, float tmin, float tmax) {
    const int P = 16; float idd = 1.0f / dot(d, d), bt = 1e30f, bu = -1; bool past = false;
    float4 a = eval(p0, p1, p2, p3, basis, n, 0); float ta = dot(a.xyz - o, d) * idd; float3 pa = a.xyz - o - d*ta;
    for (int i = 0; i < P; i++) {
        float u0 = float(i) / P, u1 = float(i + 1) / P;
        float4 b = eval(p0, p1, p2, p3, basis, n, u1); float tb = dot(b.xyz - o, d) * idd; float3 pb = b.xyz - o - d*tb;
        float3 seg = pb - pa; float l2 = dot(seg, seg);
        if (l2 > 1e-20f) {
            float sr = -dot(pa, seg) / l2, sp = sr < 0 && past ? 0 : sr;
            past = sr > 1;
            if (sp >= 0 && sp <= 1) {
                float3 q = pa + seg*sp; float rr = a.w + (b.w - a.w)*sp, t = ta + (tb - ta)*sp;
                if (dot(q, q) <= rr*rr && t >= tmin && t <= tmax && t < bt) { bt = t; bu = u0 + (u1 - u0)*sp; }
            }
            if (caps == 2 && ((i == 0 && sr < 0) || (i == P - 1 && sr > 1))) {
                float3 q = i == 0 ? pa : pb; float rr = i == 0 ? a.w : b.w, t = i == 0 ? ta : tb;
                if (dot(q, q) <= rr*rr && t >= tmin && t <= tmax && t < bt) { bt = t; bu = i == 0 ? 0 : 1; }
            }
        }
        a = b; pa = pb; ta = tb;
    }
    return bu < 0 ? float4(0) : float4(1, bt, bu, 0);
}
float4 nvmtl_curve_intersect(float4 p0, float4 p1, float4 p2, float4 p3, uint basis, uint n, uint flat, uint caps,
                             float3 o, float3 d, float tmin, float tmax) {
    return flat ? flat_curve(p0, p1, p2, p3, basis, n, caps, o, d, tmin, tmax)
                : round_curve(p0, p1, p2, p3, basis, n, caps, o, d, tmin, tmax);
}
