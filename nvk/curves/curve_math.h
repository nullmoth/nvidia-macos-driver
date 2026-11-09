/* Ray / curve intersection for Metal curve geometry, written once over an abstract set of scalar operations so the same text
 * runs as plain C floats (the model test) and as NIR (the ray-query lowering). It contains no control flow: every case is
 * computed and selected, so what the test executes is what the shader executes.
 *
 * Shape. A curve primitive is one segment of control_count (2, 3 or 4) control points with a radius per control point,
 * evaluated with the descriptor's basis (B-spline 0, Catmull-Rom 1, linear 2, Bezier 3). It is cut into CV_SUBDIV equal
 * steps of the parameter u, and each step is a straight piece with linearly varying radius:
 *   round curve  the convex hull of the two end spheres (a round cone): a cone frustum tangent to both spheres plus the
 *                two spheres. Interior joints are spheres, so there are no cracks on the outside of a bend. At the two
 *                ends of the segment: end caps sphere = the end sphere as is; disk = the hull cut by the plane through the
 *                end point perpendicular to the end piece, closed with a flat disk; none = the same cut, left open.
 *   flat curve   a ribbon: per piece a quad of half-width = radius at each end, its width direction perpendicular to both
 *                the piece and the ray, so it always faces the ray.
 * curve_parameter is u at the hit (the piece's start plus the hit's position along the piece).
 * The approximation is documented in docs/CURVES.md with its measured error against a much finer cut. */
#ifndef NVK_CURVE_MATH_H
#define NVK_CURVE_MATH_H
#include <stdint.h>

#ifndef CV_SUBDIV
#define CV_SUBDIV 16     /* measured against an Apple GPU: docs/CURVES.md */
#endif

typedef uintptr_t V;
typedef struct { V x, y, z; } V3;

struct cv_ops {
   void *ctx;
   V (*c)(void *, float);
   V (*add)(void *, V, V);
   V (*sub)(void *, V, V);
   V (*mul)(void *, V, V);
   V (*div)(void *, V, V);
   V (*sqrt)(void *, V);
   V (*min)(void *, V, V);
   V (*max)(void *, V, V);
   V (*abs)(void *, V);
   V (*lt)(void *, V, V);
   V (*le)(void *, V, V);
   V (*band)(void *, V, V);
   V (*bor)(void *, V, V);
   V (*bnot)(void *, V);
   V (*sel)(void *, V cond, V a, V b);
};

struct cv_curve {
   V3 p[4];            /* control points; unused entries are ignored */
   V r[4];             /* radius per control point */
   V basis, count;     /* float codes: basis 0..3, count 2..4 */
   V flat;             /* bool */
   V caps;             /* float code: 0 none, 1 disk, 2 sphere */
};
struct cv_ray { V3 o, d; V tmin, tmax; };
struct cv_hit { V hit; V t; V u; };

#define OP(f) o->f(o->ctx
static inline V k_(const struct cv_ops *o, float f) { return o->c(o->ctx, f); }
static inline V add_(const struct cv_ops *o, V a, V b) { return o->add(o->ctx, a, b); }
static inline V sub_(const struct cv_ops *o, V a, V b) { return o->sub(o->ctx, a, b); }
static inline V mul_(const struct cv_ops *o, V a, V b) { return o->mul(o->ctx, a, b); }
static inline V sel_(const struct cv_ops *o, V c, V a, V b) { return o->sel(o->ctx, c, a, b); }
static inline V and_(const struct cv_ops *o, V a, V b) { return o->band(o->ctx, a, b); }
static inline V or_(const struct cv_ops *o, V a, V b) { return o->bor(o->ctx, a, b); }
static inline V lt_(const struct cv_ops *o, V a, V b) { return o->lt(o->ctx, a, b); }
static inline V le_(const struct cv_ops *o, V a, V b) { return o->le(o->ctx, a, b); }
static inline V min_(const struct cv_ops *o, V a, V b) { return o->min(o->ctx, a, b); }
static inline V max_(const struct cv_ops *o, V a, V b) { return o->max(o->ctx, a, b); }
static inline V abs_(const struct cv_ops *o, V a) { return o->abs(o->ctx, a); }
static inline V sqrt_(const struct cv_ops *o, V a) { return o->sqrt(o->ctx, a); }
/* division by a value whose size the caller has already tested: the divisor is replaced by 1 where the test fails */
static inline V sdiv_(const struct cv_ops *o, V a, V b, V ok) { return o->div(o->ctx, a, sel_(o, ok, b, k_(o, 1.0f))); }
static inline V eq_(const struct cv_ops *o, V a, V b) { return and_(o, le_(o, a, b), le_(o, b, a)); }

static inline V3 v3_(V x, V y, V z) { V3 r = { x, y, z }; return r; }
static inline V3 v3add(const struct cv_ops *o, V3 a, V3 b) { return v3_(add_(o, a.x, b.x), add_(o, a.y, b.y), add_(o, a.z, b.z)); }
static inline V3 v3sub(const struct cv_ops *o, V3 a, V3 b) { return v3_(sub_(o, a.x, b.x), sub_(o, a.y, b.y), sub_(o, a.z, b.z)); }
static inline V3 v3mul(const struct cv_ops *o, V3 a, V s) { return v3_(mul_(o, a.x, s), mul_(o, a.y, s), mul_(o, a.z, s)); }
static inline V v3dot(const struct cv_ops *o, V3 a, V3 b) { return add_(o, add_(o, mul_(o, a.x, b.x), mul_(o, a.y, b.y)), mul_(o, a.z, b.z)); }
static inline V3 v3cross(const struct cv_ops *o, V3 a, V3 b)
{
   return v3_(sub_(o, mul_(o, a.y, b.z), mul_(o, a.z, b.y)), sub_(o, mul_(o, a.z, b.x), mul_(o, a.x, b.z)),
              sub_(o, mul_(o, a.x, b.y), mul_(o, a.y, b.x)));
}

/* basis weights at u for every (basis, count) the descriptor allows; unsupported pairs fall back to linear on the first two */
static inline void cv_weights(const struct cv_ops *o, V basis, V count, V u, V w[4])
{
   const V one = k_(o, 1.0f), zero = k_(o, 0.0f), half = k_(o, 0.5f), two = k_(o, 2.0f), three = k_(o, 3.0f);
   V v = sub_(o, one, u), u2 = mul_(o, u, u), u3 = mul_(o, u2, u), v2 = mul_(o, v, v), v3 = mul_(o, v2, v);
   V lin[4] = { v, u, zero, zero };
   V bz3[4] = { v2, mul_(o, two, mul_(o, u, v)), u2, zero };
   V bz4[4] = { v3, mul_(o, three, mul_(o, u, v2)), mul_(o, three, mul_(o, u2, v)), u3 };
   V sixth = k_(o, 1.0f / 6.0f);
   V bs3[4] = { mul_(o, half, v2), mul_(o, half, sub_(o, add_(o, one, mul_(o, two, u)), mul_(o, two, u2))), mul_(o, half, u2), zero };
   V bs4[4] = { mul_(o, sixth, v3),
                mul_(o, sixth, add_(o, sub_(o, mul_(o, three, u3), mul_(o, k_(o, 6.0f), u2)), k_(o, 4.0f))),
                mul_(o, sixth, add_(o, add_(o, sub_(o, k_(o, 0.0f), mul_(o, three, u3)), mul_(o, three, u2)), add_(o, mul_(o, three, u), one))),
                mul_(o, sixth, u3) };
   V cr[4] = { mul_(o, half, add_(o, sub_(o, k_(o, 0.0f), u3), sub_(o, mul_(o, two, u2), u))),
               mul_(o, half, add_(o, sub_(o, mul_(o, three, u3), mul_(o, k_(o, 5.0f), u2)), two)),
               mul_(o, half, add_(o, add_(o, sub_(o, k_(o, 0.0f), mul_(o, three, u3)), mul_(o, k_(o, 4.0f), u2)), u)),
               mul_(o, half, sub_(o, u3, u2)) };
   V is_bz = eq_(o, basis, k_(o, 3.0f)), is_bs = eq_(o, basis, zero), is_cr = eq_(o, basis, one);
   V n3 = eq_(o, count, three), n4 = eq_(o, count, k_(o, 4.0f));
   for (int i = 0; i < 4; i++) {
      V x = lin[i];
      x = sel_(o, and_(o, is_bz, n3), bz3[i], x);
      x = sel_(o, and_(o, is_bz, n4), bz4[i], x);
      x = sel_(o, and_(o, is_bs, n3), bs3[i], x);
      x = sel_(o, and_(o, is_bs, n4), bs4[i], x);
      x = sel_(o, and_(o, is_cr, n4), cr[i], x);
      w[i] = x;
   }
}

struct cv_best { V hit, t, u; };

static inline void cv_consider(const struct cv_ops *o, struct cv_best *b, const struct cv_ray *r, V valid, V t, V u)
{
   V take = and_(o, valid, and_(o, le_(o, r->tmin, t), le_(o, t, b->t)));
   b->hit = or_(o, b->hit, take);
   b->t = sel_(o, take, t, b->t);
   b->u = sel_(o, take, u, b->u);
}

/* both roots of a t^2 + 2 b t + c = 0, each with its own validity (the caller adds the range tests) */
static inline void cv_roots(const struct cv_ops *o, V a, V b, V c, V *t0, V *t1, V *ok)
{
   V disc = sub_(o, mul_(o, b, b), mul_(o, a, c));
   V good = and_(o, le_(o, k_(o, 0.0f), disc), lt_(o, k_(o, 1e-12f), abs_(o, a)));
   V s = sqrt_(o, max_(o, disc, k_(o, 0.0f)));
   V nb = sub_(o, k_(o, 0.0f), b);
   *t0 = sdiv_(o, add_(o, nb, s), a, good);
   *t1 = sdiv_(o, sub_(o, nb, s), a, good);
   *ok = good;
}

static inline V cv_clamp01(const struct cv_ops *o, V x) { return max_(o, k_(o, 0.0f), min_(o, k_(o, 1.0f), x)); }

/* With disk or no end caps the whole curve is cut by the plane through its first point perpendicular to the first piece and the plane
 * through its last point perpendicular to the last piece: nothing of any piece lies behind either plane. */
static inline V cv_end_clip(const struct cv_ops *o, const struct cv_ray *ray, V3 q0, V3 a0, V3 qn, V3 an, V notsphere, V t,
                            int near_start, int near_end)
{
   V3 p = v3add(o, ray->o, v3mul(o, ray->d, t));
   V ok = lt_(o, k_(o, 0.0f), k_(o, 1.0f));                                   /* true */
   if (near_start) ok = and_(o, ok, le_(o, k_(o, 0.0f), v3dot(o, v3sub(o, p, q0), a0)));
   if (near_end) ok = and_(o, ok, le_(o, v3dot(o, v3sub(o, p, qn), an), k_(o, 0.0f)));
   return o->bor(o->ctx, o->bnot(o->ctx, notsphere), ok);
}

static inline struct cv_hit cv_intersect(const struct cv_ops *o, const struct cv_curve *c, const struct cv_ray *ray)
{
   const V zero = k_(o, 0.0f), one = k_(o, 1.0f);
   V3 Q[CV_SUBDIV + 1]; V R[CV_SUBDIV + 1];
   for (int k = 0; k <= CV_SUBDIV; k++) {
      V u = k_(o, (float)k / CV_SUBDIV), w[4];
      cv_weights(o, c->basis, c->count, u, w);
      V3 q = v3_(zero, zero, zero); V rr = zero;
      for (int i = 0; i < 4; i++) { q = v3add(o, q, v3mul(o, c->p[i], w[i])); rr = add_(o, rr, mul_(o, c->r[i], w[i])); }
      Q[k] = q; R[k] = rr;
   }
   /* axes of the first and last piece, for the end planes */
   V3 axis0 = v3_(zero, zero, zero), axisN = axis0;
   {
#ifdef CV_END_PIECE_AXIS
      V3 e0 = v3sub(o, Q[1], Q[0]), eN = v3sub(o, Q[CV_SUBDIV], Q[CV_SUBDIV - 1]);
#else
      /* the curve's own tangent at the end points, from two close evaluations */
      V3 q0e, qne; V w0[4], wn[4];
      cv_weights(o, c->basis, c->count, k_(o, 0.002f), w0);
      cv_weights(o, c->basis, c->count, k_(o, 0.998f), wn);
      q0e = v3_(zero, zero, zero); qne = q0e;
      for (int i = 0; i < 4; i++) { q0e = v3add(o, q0e, v3mul(o, c->p[i], w0[i])); qne = v3add(o, qne, v3mul(o, c->p[i], wn[i])); }
      V3 e0 = v3sub(o, q0e, Q[0]), eN = v3sub(o, Q[CV_SUBDIV], qne);
#endif
      V l0 = sqrt_(o, v3dot(o, e0, e0)), lN = sqrt_(o, v3dot(o, eN, eN));
      V ok0 = lt_(o, k_(o, 1e-6f), l0), okN = lt_(o, k_(o, 1e-6f), lN);
      axis0 = v3mul(o, e0, sdiv_(o, one, l0, ok0));
      axisN = v3mul(o, eN, sdiv_(o, one, lN, okN));
   }
   V notsphere = o->bnot(o->ctx, eq_(o, c->caps, k_(o, 2.0f)));
   /* An open end (no cap) has no surface across it: a ray that passes through the end plane inside the end radius, heading in, is inside the
    * solid from there on, and every surface it meets next is one it leaves through. Hits beyond that crossing do not exist. */
   V tblock = ray->tmax;
   {
      V isnone = eq_(o, c->caps, zero);
      for (int end = 0; end < 2; end++) {
         V3 P = end ? Q[CV_SUBDIV] : Q[0]; V3 a = end ? axisN : axis0; V rP = end ? R[CV_SUBDIV] : R[0];
         V3 rp = v3sub(o, ray->o, P);
         V dxa = v3dot(o, ray->d, a);
         V okp = lt_(o, k_(o, 1e-9f), abs_(o, dxa));
         V tp = sdiv_(o, sub_(o, zero, v3dot(o, rp, a)), dxa, okp);
         V3 q = v3add(o, rp, v3mul(o, ray->d, tp));
         V inward = end ? lt_(o, dxa, zero) : lt_(o, zero, dxa);
         V blocks = and_(o, and_(o, isnone, okp), and_(o, inward, and_(o, le_(o, v3dot(o, q, q), mul_(o, rP, rP)), le_(o, ray->tmin, tp))));
         tblock = sel_(o, blocks, min_(o, tblock, tp), tblock);
      }
   }
   V isdisk = eq_(o, c->caps, one);
   struct cv_best round = { 0, ray->tmax, zero }, flat = { 0, ray->tmax, zero };
   round.hit = lt_(o, one, zero); flat.hit = round.hit;       /* false */
   V okc0 = round.hit, Ln = zero, okcn = round.hit, xan = zero, xbn = zero, rhoAn = zero, mn = zero;

   for (int k = 0; k < CV_SUBDIV; k++) {
      V3 A = Q[k], B = Q[k + 1]; V ra = R[k], rb = R[k + 1];
      V3 e = v3sub(o, B, A);
      V L = sqrt_(o, v3dot(o, e, e));
      V okL = lt_(o, k_(o, 1e-6f), L);
      V invL = sdiv_(o, one, L, okL);
      V3 a = v3mul(o, e, invL);
      V s = mul_(o, sub_(o, ra, rb), invL);
      V okc = and_(o, okL, lt_(o, abs_(o, s), k_(o, 0.999f)));
      V cs = sqrt_(o, max_(o, sub_(o, one, mul_(o, s, s)), zero));
      V xa = mul_(o, ra, s), xb = add_(o, L, mul_(o, rb, s));
      V rhoA = mul_(o, ra, cs), rhoB = mul_(o, rb, cs);
      V m = sdiv_(o, sub_(o, rhoB, rhoA), sub_(o, xb, xa), okc);
      V3 ro = v3sub(o, ray->o, A);
      V ox = v3dot(o, ro, a), dx = v3dot(o, ray->d, a);
      V3 ow = v3sub(o, ro, v3mul(o, a, ox)), dw = v3sub(o, ray->d, v3mul(o, a, dx));
      V c0 = add_(o, rhoA, mul_(o, m, sub_(o, ox, xa))), c1 = mul_(o, m, dx);
      V qa = sub_(o, v3dot(o, dw, dw), mul_(o, c1, c1));
      V qb = sub_(o, v3dot(o, ow, dw), mul_(o, c0, c1));
      V qc = sub_(o, v3dot(o, ow, ow), mul_(o, c0, c0));
      V t0, t1, okq;
      cv_roots(o, qa, qb, qc, &t0, &t1, &okq);
      V lo_x = xa, hi_x = xb;
      for (int rt = 0; rt < 2; rt++) {
         V t = rt ? t1 : t0;
         V x = add_(o, ox, mul_(o, t, dx));
         V rad = add_(o, c0, mul_(o, c1, t));
         V valid = and_(o, and_(o, okc, okq), and_(o, and_(o, le_(o, lo_x, x), le_(o, x, hi_x)), lt_(o, zero, rad)));
         valid = and_(o, valid, cv_end_clip(o, ray, Q[0], axis0, Q[CV_SUBDIV], axisN, notsphere, t, k < CV_SUBDIV / 4, k >= CV_SUBDIV - CV_SUBDIV / 4));
         V u = mul_(o, add_(o, k_(o, (float)k), cv_clamp01(o, sdiv_(o, x, L, okL))), k_(o, 1.0f / CV_SUBDIV));
         /* only the surface a ray enters through: a ray inside an open-ended tube does not hit its inner wall */
         valid = and_(o, valid, lt_(o, add_(o, mul_(o, qa, t), qb), zero));
         valid = and_(o, valid, lt_(o, t, tblock));
         cv_consider(o, &round, ray, valid, t, u);
      }
      /* sphere at the start of this piece (k = 0..7); the last piece also owns the sphere at the end */
      for (int end = 0; end < (k == CV_SUBDIV - 1 ? 2 : 1); end++) {
         V3 P = end ? B : A; V rr = end ? rb : ra;
         V3 rp = v3sub(o, ray->o, P);
         V sa = v3dot(o, ray->d, ray->d), sb = v3dot(o, rp, ray->d), sc = sub_(o, v3dot(o, rp, rp), mul_(o, rr, rr));
         V s0, s1, oks;
         cv_roots(o, sa, sb, sc, &s0, &s1, &oks);
         for (int rt = 0; rt < 2; rt++) {
            V t = rt ? s1 : s0;
            V valid = and_(o, oks, cv_end_clip(o, ray, Q[0], axis0, Q[CV_SUBDIV], axisN, notsphere, t, (k + end) <= CV_SUBDIV / 4, (k + end) >= CV_SUBDIV - CV_SUBDIV / 4));
            valid = and_(o, valid, lt_(o, add_(o, mul_(o, sa, t), sb), zero));
            valid = and_(o, valid, lt_(o, t, tblock));
            cv_consider(o, &round, ray, valid, t, k_(o, (float)(k + end) / CV_SUBDIV));
         }
      }
      if (k == 0) { okc0 = okL; }
      if (k == CV_SUBDIV - 1) { Ln = L; okcn = okc; xan = xa; xbn = xb; rhoAn = rhoA; mn = m; }

      /* ribbon */
      {
         V3 w = v3cross(o, a, ray->d);
         V wl = sqrt_(o, v3dot(o, w, w));
         V okw = and_(o, okL, lt_(o, k_(o, 1e-6f), wl));
         V3 wn = v3mul(o, w, sdiv_(o, one, wl, okw));
         V3 ap = v3add(o, A, v3mul(o, wn, ra)), am = v3sub(o, A, v3mul(o, wn, ra));
         V3 bp = v3add(o, B, v3mul(o, wn, rb)), bm = v3sub(o, B, v3mul(o, wn, rb));
         V3 tri[2][3] = { { ap, am, bp }, { bp, am, bm } };
         for (int ti = 0; ti < 2; ti++) {
            V3 e1 = v3sub(o, tri[ti][1], tri[ti][0]), e2 = v3sub(o, tri[ti][2], tri[ti][0]);
            V3 pv = v3cross(o, ray->d, e2);
            V det = v3dot(o, e1, pv);
            V okd = and_(o, okw, lt_(o, k_(o, 1e-12f), abs_(o, det)));
            V inv = sdiv_(o, one, det, okd);
            V3 sv = v3sub(o, ray->o, tri[ti][0]);
            V uu = mul_(o, v3dot(o, sv, pv), inv);
            V3 qv = v3cross(o, sv, e1);
            V vv = mul_(o, v3dot(o, ray->d, qv), inv);
            V t = mul_(o, v3dot(o, e2, qv), inv);
            V valid = and_(o, okd, and_(o, and_(o, le_(o, zero, uu), le_(o, zero, vv)), le_(o, add_(o, uu, vv), one)));
            V3 q = v3add(o, ray->o, v3mul(o, ray->d, t));
            V frac = cv_clamp01(o, sdiv_(o, v3dot(o, v3sub(o, q, A), a), L, okL));
            cv_consider(o, &flat, ray, valid, t, mul_(o, add_(o, k_(o, (float)k), frac), k_(o, 1.0f / CV_SUBDIV)));
         }
      }
   }

   /* flat end disks when the cap is a disk: the plane through the end point, a disk of the end point's own radius */
   for (int end = 0; end < 2; end++) {
      V3 P = end ? Q[CV_SUBDIV] : Q[0]; V3 a = end ? axisN : axis0; V rP = end ? R[CV_SUBDIV] : R[0];
      V3 rp = v3sub(o, ray->o, P);
      V dxa = v3dot(o, ray->d, a);
      V okp = lt_(o, k_(o, 1e-9f), abs_(o, dxa));
      V t = sdiv_(o, sub_(o, zero, v3dot(o, rp, a)), dxa, okp);
      V3 q = v3add(o, rp, v3mul(o, ray->d, t));          /* relative to P, lies in the plane */
      V rho2 = v3dot(o, q, q);
      V inside = le_(o, rho2, mul_(o, rP, rP));
      V valid = and_(o, and_(o, okp, isdisk), inside);
      /* entering only: the start disk faces against the first piece's direction, the end disk along the last piece's */
      valid = and_(o, valid, end ? lt_(o, dxa, zero) : lt_(o, zero, dxa));
      (void)okc0;
      cv_consider(o, &round, ray, valid, t, end ? one : zero);
   }

   /* A flat curve with sphere end caps continues past each end point as a half disc of the end point's radius that faces the ray (a billboard:
    * its plane is perpendicular to the ray through the end point). Measured on Apple GPUs: 43 of 43 cap hits at curve_parameter 0 or 1 lie in
    * that plane, and no other plane (the ribbon's, perpendicular to the tangent) fits. Only the half beyond the end point exists; the ribbon
    * itself is there in front of it. A ray along the tangent sees the whole disc, hence the small tolerance on "beyond". */
   for (int end = 0; end < 2; end++) {
      V3 P = end ? Q[CV_SUBDIV] : Q[0]; V3 a = end ? axisN : axis0; V rP = end ? R[CV_SUBDIV] : R[0];
      V dd = v3dot(o, ray->d, ray->d);
      V okn = lt_(o, k_(o, 1e-20f), dd);
      V t = sdiv_(o, v3dot(o, v3sub(o, P, ray->o), ray->d), dd, okn);
      V3 q = v3sub(o, v3add(o, ray->o, v3mul(o, ray->d, t)), P);
      V tol = mul_(o, rP, k_(o, 1e-3f));
      V qa = v3dot(o, q, a);
      V beyond = end ? le_(o, sub_(o, zero, tol), qa) : le_(o, qa, tol);
      V valid = and_(o, and_(o, okn, beyond), and_(o, eq_(o, c->caps, k_(o, 2.0f)), le_(o, v3dot(o, q, q), mul_(o, rP, rP))));
      cv_consider(o, &flat, ray, valid, t, end ? one : zero);
   }

   struct cv_hit h;
   h.hit = sel_(o, c->flat, flat.hit, round.hit);
   h.t = sel_(o, c->flat, flat.t, round.t);
   h.u = sel_(o, c->flat, flat.u, round.u);
   return h;
}
#endif
