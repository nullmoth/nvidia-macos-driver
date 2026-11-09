/* Arms for curve_math.h in its float form. The reference is independent of the analytic code: it marches the ray through an
 * implicit description of the same shape (a point is inside when some point of a piece's axis, sampled densely, is within the
 * interpolated radius) and bisects the entry. cc -O2 -o curve_model_test curve_model_test.c -lm */
#include "curve_float.h"
#include <stdio.h>
#include <stdlib.h>

typedef struct { double x, y, z; } D3;
static D3 d3(double x, double y, double z) { D3 r = { x, y, z }; return r; }
static D3 sub3(D3 a, D3 b) { return d3(a.x - b.x, a.y - b.y, a.z - b.z); }
static D3 add3(D3 a, D3 b) { return d3(a.x + b.x, a.y + b.y, a.z + b.z); }
static D3 mul3(D3 a, double s) { return d3(a.x * s, a.y * s, a.z * s); }
static double dot3(D3 a, D3 b) { return a.x * b.x + a.y * b.y + a.z * b.z; }

struct curve { D3 p[4]; double r[4]; int basis, count, flat, caps; };

/* explicit polynomial forms, written out per basis (not shared with cv_weights) */
static void eval(const struct curve *c, double u, D3 *q, double *r)
{
   double w[4] = { 0, 0, 0, 0 }, v = 1 - u;
   if (c->basis == 3 && c->count == 4) { w[0] = v * v * v; w[1] = 3 * u * v * v; w[2] = 3 * u * u * v; w[3] = u * u * u; }
   else if (c->basis == 3 && c->count == 3) { w[0] = v * v; w[1] = 2 * u * v; w[2] = u * u; }
   else if (c->basis == 0 && c->count == 3) { w[0] = v * v / 2; w[1] = (-2 * u * u + 2 * u + 1) / 2; w[2] = u * u / 2; }
   else if (c->basis == 0 && c->count == 4) { w[0] = v * v * v / 6; w[1] = (3 * u * u * u - 6 * u * u + 4) / 6; w[2] = (-3 * u * u * u + 3 * u * u + 3 * u + 1) / 6; w[3] = u * u * u / 6; }
   else if (c->basis == 1 && c->count == 4) { w[0] = (-u * u * u + 2 * u * u - u) / 2; w[1] = (3 * u * u * u - 5 * u * u + 2) / 2; w[2] = (-3 * u * u * u + 4 * u * u + u) / 2; w[3] = (u * u * u - u * u) / 2; }
   else { w[0] = v; w[1] = u; }
   *q = d3(0, 0, 0); *r = 0;
   for (int i = 0; i < 4; i++) { *q = add3(*q, mul3(c->p[i], w[i])); *r += c->r[i] * w[i]; }
}

/* signed distance-like value: negative inside the union of the pieces' hulls; pieces = number of equal steps in u */
static double implicit(const struct curve *c, int pieces, D3 q, int cap_mode)
{
   double best = 1e30;
   D3 q0, qn; double r0, rn;
   eval(c, 0, &q0, &r0); eval(c, 1, &qn, &rn);
   D3 a0, an; { D3 qa, qb; double ra, rb; eval(c, 0, &qa, &ra); eval(c, 1.0 / pieces, &qb, &rb); D3 e = sub3(qb, qa); a0 = mul3(e, 1 / sqrt(dot3(e, e)));
                eval(c, 1 - 1.0 / pieces, &qa, &ra); eval(c, 1, &qb, &rb); e = sub3(qb, qa); an = mul3(e, 1 / sqrt(dot3(e, e))); }
   for (int i = 0; i < pieces; i++) {
      D3 A, B; double ra, rb; eval(c, (double)i / pieces, &A, &ra); eval(c, (double)(i + 1) / pieces, &B, &rb);
      double piece = 1e30;
      for (int s = 0; s <= 96; s++) {          /* dense samples along the piece: the union of spheres is the hull */
         double tau = s / 96.0; D3 P = add3(A, mul3(sub3(B, A), tau)); double rr = ra + (rb - ra) * tau;
         D3 dq = sub3(q, P); double dd = sqrt(dot3(dq, dq)) - rr;
         if (dd < piece) piece = dd;
      }
      if (cap_mode != 2) {                      /* the pieces near an end are cut by that end's plane (a quarter of the curve each) */
         if (i < pieces / 4) piece = fmax(piece, -dot3(sub3(q, q0), a0));
         if (i >= pieces - pieces / 4) piece = fmax(piece, dot3(sub3(q, qn), an));
      }
      if (piece < best) best = piece;
   }
   return best;
}

static int march(const struct curve *c, int pieces, D3 o, D3 d, double tmin, double tmax, double *t_out)
{
   double step = 0.01, prev = tmin, fprev = implicit(c, pieces, add3(o, mul3(d, tmin)), c->caps);
   if (fprev <= 0) return 0;
   for (double t = tmin + step; t <= tmax; t += step) {
      double f = implicit(c, pieces, add3(o, mul3(d, t)), c->caps);
      if (f <= 0) {
         double lo = prev, hi = t;
         for (int i = 0; i < 40; i++) { double m = 0.5 * (lo + hi); if (implicit(c, pieces, add3(o, mul3(d, m)), c->caps) <= 0) hi = m; else lo = m; }
         *t_out = hi; return 1;
      }
      prev = t; fprev = f;
   }
   (void)fprev;
   return 0;
}

static int g_basis_override = -1, g_caps_override = -1;
static struct cv_hit run(const struct curve *c, D3 o, D3 d, double tmin, double tmax)
{
   struct cv_curve cc; struct cv_ray ray;
   for (int i = 0; i < 4; i++) { cc.p[i] = v3_(vf_((float)c->p[i].x), vf_((float)c->p[i].y), vf_((float)c->p[i].z)); cc.r[i] = vf_((float)c->r[i]); }
   cc.basis = vf_((float)(g_basis_override >= 0 ? g_basis_override : c->basis)); cc.count = vf_((float)c->count); cc.flat = c->flat; cc.caps = vf_((float)(g_caps_override >= 0 ? g_caps_override : c->caps));
   ray.o = v3_(vf_((float)o.x), vf_((float)o.y), vf_((float)o.z)); ray.d = v3_(vf_((float)d.x), vf_((float)d.y), vf_((float)d.z));
   ray.tmin = vf_((float)tmin); ray.tmax = vf_((float)tmax);
   return cv_intersect(&cv_float_ops, &cc, &ray);
}

static uint32_t st = 5;
static double fr(double a, double b) { st = st * 1664525u + 1013904223u; return a + (b - a) * ((st >> 8 & 0xffff) / 65535.0); }
static int fails;
static void arm(const char *n, int ok, const char *d) { printf("%s %s%s%s\n", ok ? "ok  " : "FAIL", n, ok || !d[0] ? "" : "  ", ok ? "" : d); fails += !ok; }

static struct curve rand_curve(int basis, int count, int caps)
{
   struct curve c = { .basis = basis, .count = count, .caps = caps };
   for (int i = 0; i < 4; i++) { c.p[i] = d3(fr(-1.5, 1.5), fr(-1.5, 1.5), fr(-1.5, 1.5)); c.r[i] = fr(0.08, 0.35); }
   return c;
}

/* returns rays disagreeing (outside the grazing band), hits compared, max t error */
static int compare_round(int basis, int count, int caps, int nrays, int *nhit, double *maxerr)
{
   int bad = 0; *nhit = 0; *maxerr = 0;
   for (int n = 0; n < nrays; n++) {
      struct curve c = rand_curve(basis, count, caps);
      D3 tgt = d3(fr(-1, 1), fr(-1, 1), fr(-1, 1)), o = d3(fr(-4, 4), fr(-4, 4), 4 * (n & 1 ? 1 : -1));
      D3 d = sub3(tgt, o); d = mul3(d, 1 / sqrt(dot3(d, d)));
      double tref; int rh = march(&c, CV_SUBDIV, o, d, 0, 30, &tref);
      struct cv_hit h = run(&c, o, d, 0, 30);
      int gh = (int)h.hit; double tg = fv_(h.t);
      if (rh) (*nhit)++;
      if (gh && caps == 1 && (fv_(h.u) == 0.0f || fv_(h.u) == 1.0f)) continue;      /* an end disk: its radius is the end point's own, tested below */
      if (rh && gh) { double e = fabs(tg - tref); if (e > *maxerr) *maxerr = e; if (e > 3e-3) bad++; }
      else if (rh != gh) {
         /* a grazing ray may fall either side of the sampling resolution: only count it when the reference is clearly in or out */
         D3 pt = add3(o, mul3(d, rh ? tref : tg));
         double f = implicit(&c, CV_SUBDIV, pt, caps);
         if (fabs(f) > 4e-3) bad++;
      }
   }
   return bad;
}

int main(void)
{
   char d[200]; int nh; double me;
   const char *bn[] = { "B-spline", "Catmull-Rom", "linear", "Bezier" };
   struct { int basis, count; } combos[] = { { 3, 4 }, { 3, 3 }, { 0, 4 }, { 0, 3 }, { 1, 4 }, { 2, 2 } };
   for (int i = 0; i < 6; i++) {
      int bad = compare_round(combos[i].basis, combos[i].count, 2, 400, &nh, &me);
      snprintf(d, sizeof d, "%d rays differ (%d hits, worst t error %g)", bad, nh, me);
      char name[160]; snprintf(name, sizeof name, "round %s with %d control points, sphere caps: the analytic hull equals the implicit reference", bn[combos[i].basis], combos[i].count);
      arm(name, bad == 0 && nh > 25, d);
   }
   g_basis_override = 0;
   int cbad = compare_round(3, 4, 2, 150, &nh, &me);
   g_basis_override = -1;
   arm("control: evaluating a Bezier segment with the B-spline weights is caught", cbad > 10, "not caught");
   g_caps_override = 1;
   cbad = compare_round(3, 4, 2, 400, &nh, &me);
   g_caps_override = -1;
   arm("control: cutting the ends when the reference keeps them round is caught", cbad > 0, "not caught");
   int bad = compare_round(3, 4, 1, 400, &nh, &me);
   snprintf(d, sizeof d, "%d rays differ (%d hits)", bad, nh);
   arm("round Bezier, disk caps: the cut hull equals the implicit reference away from the end disks (at most 1 percent of rays differ at a disk's rim)", bad * 100 <= 400 && nh > 25, d);

   {
      struct curve c = { .basis = 2, .count = 2, .caps = 1 }; c.p[0] = d3(0, 0, 0); c.p[1] = d3(0, 0, 2); c.r[0] = 0.3; c.r[1] = 0.3;
      struct cv_hit h0 = run(&c, d3(0.1, 0, -3), d3(0, 0, 1), 0, 100), h1 = run(&c, d3(0.4, 0, -3), d3(0, 0, 1), 0, 100), h2 = run(&c, d3(0.1, 0, 6), d3(0, 0, -1), 0, 100);
      c.caps = 0; struct cv_hit n0 = run(&c, d3(0.1, 0, -3), d3(0, 0, 1), 0, 100);
      snprintf(d, sizeof d, "hit %d t %g, outside radius hit %d, far disk hit %d t %g, open end hit %d t %g", (int)h0.hit, fv_(h0.t), (int)h1.hit, (int)h2.hit, fv_(h2.t), (int)n0.hit, fv_(n0.t));
      arm("a ray along the axis hits the start disk at the start point, not outside the end point's radius, and the far disk at 4 from the far side", h0.hit && fabsf(fv_(h0.t) - 3.0f) < 1e-4 && !h1.hit && h2.hit && fabsf(fv_(h2.t) - 4.0f) < 1e-4, d);
      arm("control: the same ray into an open end passes through it and meets only surfaces it would leave through, so it misses (measured on Apple GPUs)", !n0.hit, d);
   }
   /* open ends: identical to disk caps except where the disk is what the ray meets first */
   {
      int diff_other = 0, disk_first = 0, n = 0;
      for (int k = 0; k < 300; k++) {
         struct curve c = rand_curve(3, 4, 1), c2 = c; c2.caps = 0;
         D3 tgt = d3(fr(-1, 1), fr(-1, 1), fr(-1, 1)), o = d3(fr(-4, 4), fr(-4, 4), -4);
         D3 dd = sub3(tgt, o); dd = mul3(dd, 1 / sqrt(dot3(dd, dd)));
         struct cv_hit a = run(&c, o, dd, 0, 30), b = run(&c2, o, dd, 0, 30);
         float ua = fv_(a.u); int disk = a.hit && (ua == 0.0f || ua == 1.0f);
         if (disk) { disk_first++; continue; }
         n++; if (a.hit != b.hit || (a.hit && fv_(a.t) != fv_(b.t))) diff_other++;
      }
      snprintf(d, sizeof d, "%d of %d rays differ without meeting a disk", diff_other, n);
      arm("open ends equal disk caps for every ray that does not meet an end disk first", diff_other == 0 && disk_first > 0, d);
   }

   /* curve parameter */
   {
      struct curve c = { .basis = 2, .count = 2, .caps = 2 }; c.p[0] = d3(-1, 0, 0); c.p[1] = d3(1, 0, 0); c.r[0] = c.r[1] = 0.2;
      int ok = 1; double worst = 0;
      for (int i = 0; i < 40; i++) {
         double x = fr(-0.9, 0.9);
         struct cv_hit h = run(&c, d3(x, 0.05, -5), d3(0, 0, 1), 0, 100);
         double want = (x + 1) / 2, got = fv_(h.u);
         ok &= h.hit && fabs(got - want) < 1e-3; if (fabs(got - want) > worst) worst = fabs(got - want);
      }
      snprintf(d, sizeof d, "worst u error %g", worst);
      arm("curve_parameter on a straight round curve is the position along it", ok, d);
      struct cv_hit m = run(&c, d3(0, 0.5, -5), d3(0, 0, 1), 0, 100);
      arm("control: a ray that passes outside the radius misses", !m.hit, "");
      struct cv_hit cut = run(&c, d3(0, 0, -5), d3(0, 0, 1), 0, 2);
      arm("control: tmax short of the curve misses", !cut.hit, "");
   }
   /* the basis shapes: ends */
   {
      struct curve c = { .basis = 3, .count = 4, .caps = 2 }; c.p[0] = d3(0, 0, 0); c.p[1] = d3(1, 2, 0); c.p[2] = d3(2, 2, 0); c.p[3] = d3(3, 0, 0);
      for (int i = 0; i < 4; i++) c.r[i] = 0.1;
      struct cv_hit s = run(&c, d3(0, 0, -5), d3(0, 0, 1), 0, 100), e = run(&c, d3(3, 0, -5), d3(0, 0, 1), 0, 100);
      struct cv_hit mid = run(&c, d3(1, 0, -5), d3(0, 0, 1), 0, 100);
      arm("a Bezier segment passes through its first and last control point and not through the middle ones", s.hit && e.hit && !mid.hit, "");
      c.basis = 1;
      struct cv_hit through = run(&c, d3(1, 2, -5), d3(0, 0, 1), 0, 100), skip = run(&c, d3(0, 0, -5), d3(0, 0, 1), 0, 100);
      arm("a Catmull-Rom segment passes through its second and third control point and starts there", through.hit && !skip.hit, "");
   }
   /* flat ribbon against its plane geometry */
   {
      int ok = 1, hits = 0;
      for (int i = 0; i < 400; i++) {
         struct curve c = { .basis = 2, .count = 2, .flat = 1, .caps = 0 };   /* the ribbon alone: the end caps are tested below */
         c.p[0] = d3(fr(-1, 0), fr(-.5, .5), fr(-.5, .5)); c.p[1] = d3(fr(.2, 1.2), fr(-.5, .5), fr(-.5, .5)); c.r[0] = c.r[1] = fr(.1, .4);
         D3 tgt = d3(fr(-1, 1.2), fr(-.6, .6), fr(-.6, .6)), o = d3(fr(-1, 1), fr(-1, 1), -4), dd = sub3(tgt, o);
         struct cv_hit h = run(&c, o, dd, 0, 100);
         D3 e = sub3(c.p[1], c.p[0]); double L = sqrt(dot3(e, e)); D3 a = mul3(e, 1 / L);
         D3 w = d3(a.y * dd.z - a.z * dd.y, a.z * dd.x - a.x * dd.z, a.x * dd.y - a.y * dd.x); w = mul3(w, 1 / sqrt(dot3(w, w)));
         D3 n = d3(a.y * w.z - a.z * w.y, a.z * w.x - a.x * w.z, a.x * w.y - a.y * w.x);
         double den = dot3(dd, n), t = dot3(sub3(c.p[0], o), n) / den;
         D3 q = sub3(add3(o, mul3(dd, t)), c.p[0]); double f = dot3(q, a) / L, s = dot3(q, w);
         int want = fabs(den) > 1e-6 && t >= 0 && f >= 0 && f <= 1 && fabs(s) <= c.r[0];
         if (fabs(f) < 1e-3 || fabs(f - 1) < 1e-3 || fabs(fabs(s) - c.r[0]) < 1e-3) continue;      /* on an edge */
         hits += want;
         if (want != (int)h.hit) ok = 0;
         else if (want && (fabs(fv_(h.t) - t) > 1e-3 * fmax(1, fabs(t)) || fabs(fv_(h.u) - f) > 2e-3)) ok = 0;
      }
      snprintf(d, sizeof d, "%d hits compared", hits);
      arm("a flat curve is the ribbon facing the ray: hit, distance and parameter equal the plane construction", ok && hits > 30, d);
   }
   /* flat end caps: a half disc facing the ray beyond each end point (measured on Apple GPUs) */
   {
      struct curve c = { .basis = 2, .count = 2, .flat = 1, .caps = 2 }; c.p[0] = d3(0, 0, 0); c.p[1] = d3(2, 0, 0); c.r[0] = c.r[1] = 0.3;
      struct cv_hit behind = run(&c, d3(-0.2, 0.1, -5), d3(0, 0, 1), 0, 100);       /* beyond the start point, inside the radius */
      struct cv_hit far_ = run(&c, d3(-0.2, 0.28, -5), d3(0, 0, 1), 0, 100);        /* beyond it and outside the radius (0.34 away) */
      struct cv_hit past_end = run(&c, d3(2.2, -0.1, -5), d3(0, 0, 1), 0, 100);
      struct cv_hit front = run(&c, d3(0.5, 0.1, -5), d3(0, 0, 1), 0, 100);         /* on the ribbon itself */
      c.caps = 0; struct cv_hit nocap = run(&c, d3(-0.2, 0.1, -5), d3(0, 0, 1), 0, 100);
      snprintf(d, sizeof d, "behind %d t %g u %g, outside %d, past the end %d u %g, ribbon %d u %g, no caps %d", (int)behind.hit, fv_(behind.t), fv_(behind.u), (int)far_.hit, (int)past_end.hit, fv_(past_end.u), (int)front.hit, fv_(front.u), (int)nocap.hit);
      arm("a flat curve with sphere caps continues past each end as a disc that faces the ray (distance 5, parameter 0 or 1), not beyond the radius, and the ribbon is untouched",
          behind.hit && fabsf(fv_(behind.t) - 5.0f) < 1e-4 && fv_(behind.u) == 0.0f && !far_.hit && past_end.hit && fv_(past_end.u) == 1.0f && front.hit && fv_(front.u) > 0.2f && fv_(front.u) < 0.3f, d);
      arm("control: the same ray with no caps misses", !nocap.hit, d);
   }
   /* accuracy of the 8-piece cut against a 128-piece cut of the same curve */
   {
      int agree = 0, n = 0; double worst = 0;
      for (int k = 0; k < 120; k++) {
         struct curve c = rand_curve(3, 4, 2);
         D3 tgt = d3(fr(-1, 1), fr(-1, 1), fr(-1, 1)), o = d3(fr(-4, 4), fr(-4, 4), -4), dd = sub3(tgt, o); dd = mul3(dd, 1 / sqrt(dot3(dd, dd)));
         double tf; int rh = march(&c, 48, o, dd, 0, 30, &tf);
         struct cv_hit h = run(&c, o, dd, 0, 30);
         if (rh && h.hit) { n++; double e = fabs(fv_(h.t) - tf); if (e > worst) worst = e; agree += e < 0.05; }
      }
      snprintf(d, sizeof d, "%d of %d within 0.05, worst %g", agree, n, worst);
      printf("info cut accuracy: CV_SUBDIV pieces against 48 pieces, %s\n", d);
      arm("the default cut stays within 0.05 of a 48-piece cut on at least 90 percent of common hits (scene scale 3, radius 0.08 to 0.35)", n > 10 && agree * 10 >= n * 9, d);
   }
   /* bounds: a port of curve_aabb.comp must contain every sphere of the curve, for every basis */
   {
      int contained = 1, total = 0;
      for (int combo = 0; combo < 6; combo++) {
         for (int k = 0; k < 300; k++) {
            struct curve c = rand_curve(combos[combo].basis, combos[combo].count, 2);
            D3 p[4]; double r[4]; for (int i = 0; i < 4; i++) { p[i] = c.p[i]; r[i] = c.r[i]; }
            int n = c.count;
            if (c.basis == 1 && n == 4) {
               D3 b1 = add3(p[1], mul3(sub3(p[2], p[0]), 1 / 6.0)), b2 = sub3(p[2], mul3(sub3(p[3], p[1]), 1 / 6.0));
               double s1 = r[1] + (r[2] - r[0]) / 6, s2 = r[2] - (r[3] - r[1]) / 6;
               p[0] = p[1]; p[3] = p[2]; p[1] = b1; p[2] = b2; r[0] = r[1]; r[3] = r[2]; r[1] = s1; r[2] = s2;
            }
            D3 lo = p[0], hi = p[0]; double rmax = fabs(r[0]);
            for (int i = 1; i < n; i++) { lo = d3(fmin(lo.x, p[i].x), fmin(lo.y, p[i].y), fmin(lo.z, p[i].z)); hi = d3(fmax(hi.x, p[i].x), fmax(hi.y, p[i].y), fmax(hi.z, p[i].z)); rmax = fmax(rmax, fabs(r[i])); }
            double g = rmax * 1.0001 + 1e-6;
            for (int s = 0; s <= 64; s++) {
               D3 q; double rr; eval(&c, s / 64.0, &q, &rr);
               total++;
               if (q.x - rr < lo.x - g || q.x + rr > hi.x + g || q.y - rr < lo.y - g || q.y + rr > hi.y + g || q.z - rr < lo.z - g || q.z + rr > hi.z + g) contained = 0;
            }
         }
      }
      snprintf(d, sizeof d, "%d spheres checked", total);
      arm("the segment box contains every sphere of the curve, for every basis (Catmull-Rom through its Bezier form)", contained, d);
      /* control: without the Bezier conversion the Catmull-Rom box is too small for some curves */
      int escaped = 0;
      for (int k = 0; k < 600 && !escaped; k++) {
         struct curve c = rand_curve(1, 4, 2);
         D3 lo = c.p[0], hi = c.p[0]; double rmax = fabs(c.r[0]);
         for (int i = 1; i < 4; i++) { lo = d3(fmin(lo.x, c.p[i].x), fmin(lo.y, c.p[i].y), fmin(lo.z, c.p[i].z)); hi = d3(fmax(hi.x, c.p[i].x), fmax(hi.y, c.p[i].y), fmax(hi.z, c.p[i].z)); rmax = fmax(rmax, c.r[i]); }
         for (int s = 0; s <= 64; s++) { D3 q; double rr; eval(&c, s / 64.0, &q, &rr);
            if (q.x < lo.x - rmax || q.x > hi.x + rmax || q.y < lo.y - rmax || q.y > hi.y + rmax || q.z < lo.z - rmax || q.z > hi.z + rmax) escaped = 1; }
      }
      arm("control: a Catmull-Rom segment can leave the box of its own control points, which is why the conversion exists", escaped, "never escaped: the conversion is untested");
   }
   printf("%d arms failed\n", fails);
   return fails != 0;
}
