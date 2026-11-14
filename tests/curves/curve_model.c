// CPU model of Metal ray-traced curve intersection, matched against Apple's results in reference_m1.txt.
// Rules measured on an Apple GPU: a round curve is the swept sphere of radius r(u) along C(u) and the hit is its front
// surface; a flat curve is a ribbon facing the ray and the hit is the curve point the ray passes closest to; with no
// end caps each segment is cut by the planes through its end points normal to the curve. End caps (reference_caps_m1.txt):
// a round curve's disk cap is the disk of radius r in that end plane, its sphere cap the sphere of radius r at the end
// point (the swept sphere left uncut); a flat curve ignores disk caps, and its sphere cap is a disk of radius r facing
// the ray at the end point, hit at the ray's closest approach. A cap hit reports u = 0 or 1. Between segments hit at the
// same distance the choice is the traversal's. The driver's GPU curve intersector is a port of intersect() below.
// build: clang -O2 curve_model.c -o curve_model -lm; ./curve_model prints in the reference format.
#include <stdio.h>
#include <math.h>
#include <string.h>
#include <stdlib.h>
typedef struct { double x, y, z; } v3;
static v3 add(v3 a, v3 b){return (v3){a.x+b.x,a.y+b.y,a.z+b.z};}
static v3 sub(v3 a, v3 b){return (v3){a.x-b.x,a.y-b.y,a.z-b.z};}
static v3 mul(v3 a, double s){return (v3){a.x*s,a.y*s,a.z*s};}
static double dot(v3 a, v3 b){return a.x*b.x+a.y*b.y+a.z*b.z;}
enum { LINEAR, BSPLINE, CATMULL, BEZIER };
// Bezier/B-spline/Catmull-Rom/linear basis weights for parameter u, n control points (2,3,4)
static void weights(int basis, int n, double u, double *w) {
    double s = 1 - u;
    if (basis == LINEAR) { w[0] = s; w[1] = u; return; }
    if (basis == BSPLINE && n == 3) { w[0] = 0.5*s*s; w[1] = 0.5 + u*s; w[2] = 0.5*u*u; return; }
    if (basis == BSPLINE && n == 4) { w[0] = s*s*s/6; w[1] = (3*u*u*u - 6*u*u + 4)/6; w[2] = (-3*u*u*u + 3*u*u + 3*u + 1)/6; w[3] = u*u*u/6; return; }
    if (basis == CATMULL) { w[0] = 0.5*(-u*u*u + 2*u*u - u); w[1] = 0.5*(3*u*u*u - 5*u*u + 2); w[2] = 0.5*(-3*u*u*u + 4*u*u + u); w[3] = 0.5*(u*u*u - u*u); return; }
    /* bezier cubic */ w[0] = s*s*s; w[1] = 3*u*s*s; w[2] = 3*u*u*s; w[3] = u*u*u;
}
static void eval(int basis, int n, const v3 *p, const double *r, double u, v3 *c, double *rad) {
    double w[4]; weights(basis, n, u, w); *c = (v3){0,0,0}; *rad = 0;
    for (int i = 0; i < n; i++) { *c = add(*c, mul(p[i], w[i])); *rad += r[i]*w[i]; }
}
// Hit = smallest t over u in [0,1]: flat -> t of the ray's closest approach to C(u) where that distance <= r(u)
// round -> t where |o + t d - C(u)| = r(u) (front surface of the swept sphere), minimised over u.
// Fast form for the GPU: score(u) is the quantity minimised (round: hit t, flat: squared distance to the ray), +inf
// where the ray misses the cross-section at u or the hit falls outside the end planes. 32 coarse samples, then a
// golden-section search on the bracket around the best sample.
enum { CAP_NONE, CAP_DISK, CAP_SPHERE };
static double score(int basis, int n, const v3 *p, const double *r, v3 o, v3 d, int flat, int clip, double u,
                    v3 c0, v3 t0, v3 c1, v3 t1, double *tout) {
    v3 c; double rad; eval(basis, n, p, r, u, &c, &rad);
    v3 oc = sub(c, o); double tc = dot(oc, d); v3 q = sub(add(o, mul(d, tc)), c); double d2 = dot(q, q);
    if (d2 > rad*rad) return 1e30;
    double t = flat ? tc : tc - sqrt(rad*rad - d2);
    v3 hp = add(o, mul(d, t));
    if (clip && (dot(sub(hp, c0), t0) < 0 || dot(sub(hp, c1), t1) > 0)) return 1e30;
    *tout = t; return flat ? d2 : t;
}
// A round curve: t(u) is the distance to the front of the sphere at u (uncut). The tube's surface is where t(u) is at
// a local minimum: the ray enters the tube there. The first interior minimum between the end planes is the hit. A
// minimum at u = 0 or 1 whose point lies beyond its end plane is the ray coming in by an open end: no caps, no hit there
// (the ray may still enter the tube's wall further on); disk caps, the end disk; sphere caps, the end sphere.
static int intersect_round(int basis, int n, const v3 *p, const double *r, v3 o, v3 d, int caps,
                           v3 c0, v3 t0, double r0, v3 c1, v3 t1, double r1, double *T, double *U, double *K) {
    const int N = 32; double ts[N + 1], tmp;
    for (int k = 0; k <= N; k++) { double t = 1e30; ts[k] = score(basis, n, p, r, o, d, 0, 0, (double)k / N, c0, t0, c1, t1, &t) < 1e29 ? t : 1e30; }
    // a thin tube can fall between samples: if every sample misses, sub-sample each interval
    double bt = 1e30, bu = -1;
    for (int k = 0; k <= N; k++) {
        if (ts[k] >= 1e29) continue;
        const int lo = k > 0 ? ts[k - 1] >= ts[k] : 1, hi = k < N ? ts[k + 1] >= ts[k] : 1;
        if (!lo || !hi) continue;
        double a = k > 0 ? (k - 1.0) / N : 0, b = k < N ? (k + 1.0) / N : 1, mt = ts[k], mu = (double)k / N;
        const double g = 0.6180339887498949;
        for (int it = 0; it < 40; it++) {
            double x1 = b - g * (b - a), x2 = a + g * (b - a), ta = 1e30, tb = 1e30;
            if (score(basis, n, p, r, o, d, 0, 0, x1, c0, t0, c1, t1, &ta) >= 1e29) ta = 1e30;
            if (score(basis, n, p, r, o, d, 0, 0, x2, c0, t0, c1, t1, &tb) >= 1e29) tb = 1e30;
            if (ta < mt) { mt = ta; mu = x1; }
            if (tb < mt) { mt = tb; mu = x2; }
            if (ta < tb) b = x2; else a = x1;
        }
        const v3 hp = add(o, mul(d, mt));
        const int before = dot(sub(hp, c0), t0) < 0, after = dot(sub(hp, c1), t1) > 0;
        double ht = mt, hu = mu;
        if (before || after) {
            if (caps == CAP_NONE) continue;
            hu = before ? 0 : 1;
            if (caps == CAP_DISK) {
                const v3 c = before ? c0 : c1, t = before ? t0 : t1; const double rad = before ? r0 : r1, dn = dot(d, t);
                if (fabs(dn) < 1e-12) continue;
                ht = dot(sub(c, o), t) / dn; const v3 q = sub(add(o, mul(d, ht)), c);
                if (dot(q, q) > rad * rad) continue;
            }
        }
        if (ht < bt) { bt = ht; bu = hu; }
    }
    (void)tmp;
    if (bu < 0) return 0;
    *T = bt; *U = bu; *K = bt; return 1;
}
// A round linear segment is a cone frustum: radius r0 + (r1 - r0) s / L at axial distance s, measured across the axis
// (measured: offset rays report u = (offset - r0) / (r1 - r0), where a swept sphere would give a different u and t).
// The hit is where the ray enters the cone between the end planes; the open ends and caps as for other round curves.
static int intersect_cone(v3 c0, double r0, v3 c1, double r1, v3 o, v3 d, int caps, double *T, double *U) {
    const v3 ax = sub(c1, c0); const double L = sqrt(dot(ax, ax)); const v3 a = mul(ax, 1 / L); const double k = (r1 - r0) / L;
    const v3 w = sub(o, c0); const double dd = dot(d, d), da = dot(d, a), s0 = dot(w, a), wd = dot(w, d);
    const double A = dd - da * da - k * k * da * da, B = 2 * (wd - s0 * da - k * da * (r0 + k * s0)), C = dot(w, w) - s0 * s0 - (r0 + k * s0) * (r0 + k * s0);
    double bt = 1e30, bu = -1;
    const double disc = B * B - 4 * A * C;
    if (disc >= 0 && fabs(A) > 1e-15) {
        const double q = sqrt(disc), ts[2] = { (-B - q) / (2 * A), (-B + q) / (2 * A) };
        for (int i = 0; i < 2; i++) {
            const double t = ts[i], sa = s0 + t * da;
            if (2 * A * t + B >= 0) continue;                 // leaving the cone: an inner wall
            if (r0 + k * sa < 0) continue;                    // the mirrored sheet of the double cone
            if (sa >= 0 && sa <= L) { if (t < bt) { bt = t; bu = sa / L; } }
        }
    }
    // the ends: where the ray enters the infinite cone beyond an end plane it comes in by that end
    for (int end = 0; end < 2 && caps != CAP_NONE; end++) {
        const v3 c = end ? c1 : c0; const double rad = end ? r1 : r0;
        if (caps == CAP_DISK) {
            if (fabs(da) < 1e-12) continue;
            const double th = dot(sub(c, o), a) / da; const v3 qv = sub(add(o, mul(d, th)), c);
            if (dot(qv, qv) <= rad * rad && th < bt) { bt = th; bu = end; }
        } else {
            const v3 oc = sub(o, c); const double b2 = dot(oc, d), c2 = dot(oc, oc) - rad * rad, dsc = b2 * b2 - dd * c2;
            if (dsc < 0) continue;
            const double th = (-b2 - sqrt(dsc)) / dd; const double sa = s0 + th * da;
            if ((end ? sa > L : sa < 0) && th < bt) { bt = th; bu = end; }
        }
    }
    if (bu < 0) return 0;
    *T = bt; *U = bu; return 1;
}
// A flat curve is a ribbon facing the ray, built as Apple builds it: the segment cut into FLAT_PIECES straight pieces
// (measured: 16 reproduces Apple's oblique hits to 1e-4, 8 and 32 do not), each seen along the ray as a 2D line with
// the radius interpolated along it. The ray hits a piece where its closest point on the piece lies inside the piece and
// within the radius there; t and u are interpolated along the piece, and the nearest hit wins. Sphere caps add the
// half-disk of the end radius beyond each end; disk caps change nothing for a flat curve.
#define FLAT_PIECES 16
static int intersect_flat(int basis, int n, const v3 *p, const double *r, v3 o, v3 d, int caps, double *T, double *U) {
    const double dd = dot(d, d); double bt = 1e30, bu = -1; int past = 0;
    v3 a, pa; double ra, ta;
    eval(basis, n, p, r, 0, &a, &ra); ta = dot(sub(a, o), d) / dd; pa = sub(sub(a, o), mul(d, ta));
    for (int i = 0; i < FLAT_PIECES; i++) {
        v3 b, pb; double rb, tb; const double u0 = (double)i / FLAT_PIECES, u1 = (i + 1.0) / FLAT_PIECES;
        eval(basis, n, p, r, u1, &b, &rb); tb = dot(sub(b, o), d) / dd; pb = sub(sub(b, o), mul(d, tb));
        const v3 seg = sub(pb, pa); const double l2 = dot(seg, seg);
        if (l2 > 1e-24) {
            // a closest point past the end of one piece and before the start of the next is their joint (the wedge on
            // the outside of a bend); the ribbon's own two ends cut it off
            const double sr = -dot(pa, seg) / l2, sp = sr < 0 && past ? 0 : sr;
            past = sr > 1;
            if (sp >= 0 && sp <= 1) {
                const v3 q = add(pa, mul(seg, sp)); const double rr = ra + (rb - ra) * sp, t = ta + (tb - ta) * sp;
                if (dot(q, q) <= rr * rr && t < bt) { bt = t; bu = u0 + (u1 - u0) * sp; }
            }
            if (caps == CAP_SPHERE && ((i == 0 && sr < 0) || (i == FLAT_PIECES - 1 && sr > 1))) {
                const v3 q = i == 0 ? pa : pb; const double rr = i == 0 ? ra : rb, t = i == 0 ? ta : tb;
                if (dot(q, q) <= rr * rr && t < bt) { bt = t; bu = i == 0 ? 0 : 1; }
            }
        }
        a = b; pa = pb; ra = rb; ta = tb;
    }
    if (bu < 0) return 0;
    *T = bt; *U = bu; return 1;
}
static int intersect(int basis, int n, const v3 *p, const double *r, v3 o, v3 d, int flat, int caps, double *T, double *U, double *K) {
    if (flat) { if (!intersect_flat(basis, n, p, r, o, d, caps, T, U)) return 0; *K = *T; return 1; }
    if (!flat && basis == LINEAR) { if (!intersect_cone(p[0], r[0], p[1], r[1], o, d, caps, T, U)) return 0; *K = *T; return 1; }
    v3 c0, c1, e0, e1; double r0, r1, rr;
    eval(basis, n, p, r, 0, &c0, &r0); eval(basis, n, p, r, 1e-4, &e0, &rr);
    eval(basis, n, p, r, 1, &c1, &r1); eval(basis, n, p, r, 1 - 1e-4, &e1, &rr);
    const v3 t0 = sub(e0, c0), t1 = sub(c1, e1);
    // a round curve is searched uncut: where the ray first enters the swept tube decides the hit. An entry between the
    // end planes is the body; one beyond them is the open end (no caps: Apple reports no hit, the ray sees only inner
    // wall), the disk in the end plane (disk caps), or the end sphere itself (sphere caps)
    const int clip = flat;
    // the caps, as candidates beside the body
    double cbest = 1e30, cu = 0, ct = 0;
    for (int end = 0; end < 2 && caps != CAP_NONE; end++) {
        const v3 c = end ? c1 : c0, t = end ? t1 : t0; const double rad = end ? r1 : r0;
        if (!flat && caps == CAP_DISK && 0) {
            const double dn = dot(d, t); if (fabs(dn) < 1e-12) continue;
            const double th = dot(sub(c, o), t) / dn; const v3 q = sub(add(o, mul(d, th)), c);
            if (dot(q, q) <= rad * rad && th < cbest) { cbest = th; ct = th; cu = end; }
        } else if (flat && caps == CAP_SPHERE) {
            const double tc = dot(sub(c, o), d) / dot(d, d); const v3 q = sub(add(o, mul(d, tc)), c); const double d2 = dot(q, q);
            if (d2 <= rad * rad && d2 < cbest) { cbest = d2; ct = tc; cu = end; }
        }
    }
    if (!flat) return intersect_round(basis, n, p, r, o, d, caps, c0, t0, r0, c1, t1, r1, T, U, K);
    const int N = 32; double best = 1e30, bu = 0, bt = 0; int bk = -1;
    for (int k = 0; k <= N; k++) { double t, sc = score(basis, n, p, r, o, d, flat, clip, (double)k / N, c0, t0, c1, t1, &t);
        if (sc < best) { best = sc; bu = (double)k / N; bt = t; bk = k; } }
    if (bk < 0) {   // nothing at the samples: a thin hit can sit between two; look at each interval's midpoint pairs
        for (int k = 0; k < N && bk < 0; k++) for (int m = 1; m < 8; m++) {
            double u = (k + m / 8.0) / N, t, sc = score(basis, n, p, r, o, d, flat, clip, u, c0, t0, c1, t1, &t);
            if (sc < best) { best = sc; bu = u; bt = t; bk = k; } }
        if (bk < 0) {
            if (cbest >= 1e30) return 0;
            *T = ct; *U = cu; *K = cbest; return 1;
        }
    }
    double a = bu - 1.0 / N, b = bu + 1.0 / N; if (a < 0) a = 0; if (b > 1) b = 1;
    const double g = 0.6180339887498949;
    for (int it = 0; it < 40; it++) {
        double x1 = b - g * (b - a), x2 = a + g * (b - a), ta, tb;
        double f1 = score(basis, n, p, r, o, d, flat, clip, x1, c0, t0, c1, t1, &ta), f2 = score(basis, n, p, r, o, d, flat, clip, x2, c0, t0, c1, t1, &tb);
        if (f1 < best) { best = f1; bu = x1; bt = ta; }
        if (f2 < best) { best = f2; bu = x2; bt = tb; }
        if (f1 < f2) b = x2; else a = x1;
    }
    if (!flat) {
        const v3 hp = add(o, mul(d, bt));
        const int before = dot(sub(hp, c0), t0) < 0, after = dot(sub(hp, c1), t1) > 0;
        if (before || after) {
            if (caps == CAP_NONE) return 0;
            if (caps == CAP_SPHERE) { *T = bt; *U = before ? 0 : 1; *K = bt; return 1; }
            // disk caps: the disk of the end the ray came in by
            const v3 c = before ? c0 : c1, t = before ? t0 : t1; const double rad = before ? r0 : r1, dn = dot(d, t);
            if (fabs(dn) < 1e-12) return 0;
            const double th = dot(sub(c, o), t) / dn; const v3 q = sub(add(o, mul(d, th)), c);
            if (dot(q, q) > rad * rad) return 0;
            *T = th; *U = before ? 0 : 1; *K = th; return 1;
        }
    }
    if (cbest < best) { bt = ct; bu = cu; best = cbest; }
    *T = bt; *U = bu; *K = best; return 1;
}
static double basis_w(int basis, int n, double u, int i) { double w[4]; weights(basis, n, u, w); return w[i]; }
int main(int argc, char **argv) {
    const int oblique = argc > 1 && !strcmp(argv[1], "oblique"), capsmode = oblique || (argc > 1 && !strcmp(argv[1], "caps"));
    const double cp[5][3] = { {-1.5,0.2,0}, {-1.0,-0.1,0}, {0.0,0.3,0}, {1.0,-0.1,0}, {1.5,0.2,0} };
    const double rad[5] = { 0.10, 0.12, 0.15, 0.12, 0.10 };
    struct { int b, n; const char *name; } B[] = { {LINEAR,2,"linear"}, {BSPLINE,3,"bspline2"}, {BSPLINE,4,"bspline3"}, {CATMULL,4,"catmullrom"}, {BEZIER,4,"bezier3"} };
    static const char *capname[3] = { "none", "disk", "sphere" };
    for (int cap = 0; cap < (capsmode ? 3 : 1); cap++) for (int bi = 0; bi < 5; bi++) for (int fl = 0; fl < 2; fl++) {
        int n = B[bi].n, nseg = 5 - n + 1;
        v3 ro[41], rd[41];
        for (int k = 0; k < 41; k++) { ro[k] = (v3){ -1.0 + 2.0*k/40, 0.05, -5 }; rd[k] = (v3){ 0, 0, 1 }; }
        if (capsmode) {   // the rays of curves.m caps mode: along each end's tangent, offset across it
            v3 e[2], t[2];
            for (int end = 0; end < 2; end++) {
                const int s0 = end ? 5 - n : 0; const double u = end ? 1 : 0, h = end ? -1e-3 : 1e-3;
                v3 p0 = {0,0,0}, p1 = {0,0,0};
                for (int i = 0; i < n; i++) { v3 c = { cp[s0+i][0], cp[s0+i][1], cp[s0+i][2] };
                    p0 = add(p0, mul(c, basis_w(B[bi].b, n, u, i))); p1 = add(p1, mul(c, basis_w(B[bi].b, n, u + h, i))); }
                e[end] = p0; t[end] = mul(sub(p1, p0), (end ? 1 : -1) / h); t[end] = mul(t[end], 1 / sqrt(dot(t[end], t[end])));
            }
            for (int k = 0; k < 41 && oblique; k++) {
                const int end = k < 20 ? 0 : 1, j = k < 20 ? k : k - 20;
                const double sd = j == 20 ? -0.02 : -0.3 + 0.6 * (j % 20) / 19.0, h2 = 0.70710678;
                const v3 dir = add(mul((v3){0, 0, 1}, h2), mul(t[end], h2));
                ro[k] = add(add(e[end], mul(t[end], sd)), mul(dir, 2.0)); rd[k] = mul(dir, -1);
            }
            for (int k = 0; k < 41 && !oblique; k++) {
                const int end = k < 20 ? 0 : 1, j = k < 20 ? k : k - 20;
                const double off = j == 20 ? 0 : (j % 10 - 4.5) / 3.0 * 0.10;
                const v3 ax = { -t[end].y, t[end].x, 0 }, az = { 0, 0, 1 };
                ro[k] = add(add(e[end], mul(t[end], 2.0)), mul(j < 10 ? ax : az, off)); rd[k] = mul(t[end], -1);
            }
        }
        if (capsmode) printf("%s %s %s%s:", B[bi].name, fl ? "flat" : "round", capname[cap], oblique ? " oblique" : "");
        else printf("%s %s:", B[bi].name, fl ? "flat" : "round");
        for (int k = 0; k < 41; k++) {
            double bt = 1e30, bu = -1; int any = 0, bs = -1;
            for (int s = 0; s < nseg; s++) {
                v3 p[4]; double r[4];
                for (int i = 0; i < n; i++) { p[i] = (v3){cp[s+i][0], cp[s+i][1], cp[s+i][2]}; r[i] = rad[s+i]; }
                double t, u, kk; if (intersect(B[bi].b, n, p, r, ro[k], rd[k], fl, cap, &t, &u, &kk) && t < bt) { bt = t; bu = u; any = 1; bs = s; }
            }
            if (any) printf(" %.4f/%.4f/%d", bt, bu, bs); else printf(" -1.0000/-1.0000/0");
        }
        printf("\n");
    }
}
