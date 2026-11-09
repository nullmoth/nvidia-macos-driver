/* See hwrt_model.h. Plain C, no dependencies: cc -O2 hwrt_model.c hwrt_model_test.c -lm */
#include "hwrt_model.h"
#include <math.h>
#include <string.h>

/* ---- small geometry ------------------------------------------------------------------------------------------------ */

static void sub3(const float *a, const float *b, float *o) { for (int i = 0; i < 3; i++) o[i] = a[i] - b[i]; }
static void cross3(const float *a, const float *b, float *o)
{
   o[0] = a[1] * b[2] - a[2] * b[1]; o[1] = a[2] * b[0] - a[0] * b[2]; o[2] = a[0] * b[1] - a[1] * b[0];
}
static float dot3(const float *a, const float *b) { return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]; }

/* Moeller-Trumbore, double sided. front = the ray arrives against the counter-clockwise normal. */
static int tri_hit(const float v[3][3], const float *org, const float *dir, float *t, float *u, float *w, int *front)
{
   float e1[3], e2[3], p[3], s[3], q[3];
   sub3(v[1], v[0], e1); sub3(v[2], v[0], e2);
   cross3(dir, e2, p);
   float det = dot3(e1, p);
   if (det == 0.0f) return 0;
   float inv = 1.0f / det;
   sub3(org, v[0], s);
   *u = dot3(s, p) * inv;
   if (*u < 0.0f || *u > 1.0f) return 0;
   cross3(s, e1, q);
   *w = dot3(dir, q) * inv;
   if (*w < 0.0f || *u + *w > 1.0f) return 0;
   *t = dot3(e2, q) * inv;
   *front = det > 0.0f;
   return 1;
}

/* slab test; entry may be negative when the origin is inside */
static int box_range(const float *lo, const float *hi, const float *org, const float *dir, float *tnear, float *tfar)
{
   float n = -INFINITY, f = INFINITY;
   for (int i = 0; i < 3; i++) {
      if (dir[i] == 0.0f) {
         if (org[i] < lo[i] || org[i] > hi[i]) return 0;
         continue;
      }
      float a = (lo[i] - org[i]) / dir[i], b = (hi[i] - org[i]) / dir[i];
      if (a > b) { float x = a; a = b; b = x; }
      if (a > n) n = a;
      if (b < f) f = b;
   }
   *tnear = n; *tfar = f;
   return n <= f;
}

static void to_object(const struct m_inst *in, const struct m_ray *r, float *o, float *d)
{
   for (int i = 0; i < 3; i++) {
      d[i] = in->wto[i][0] * r->dir[0] + in->wto[i][1] * r->dir[1] + in->wto[i][2] * r->dir[2];
      o[i] = in->wto[i][0] * r->org[0] + in->wto[i][1] * r->org[1] + in->wto[i][2] * r->org[2] + in->wto[i][3];
   }
}

void scene_finish(struct m_scene *s)
{
   for (int k = 0; k < s->ninst; k++) {
      double a[3][3], t[3], det, c[3][3];
      for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) a[i][j] = s->inst[k].otw[i][j]; t[i] = s->inst[k].otw[i][3]; }
      c[0][0] = a[1][1] * a[2][2] - a[1][2] * a[2][1]; c[0][1] = a[1][2] * a[2][0] - a[1][0] * a[2][2]; c[0][2] = a[1][0] * a[2][1] - a[1][1] * a[2][0];
      c[1][0] = a[0][2] * a[2][1] - a[0][1] * a[2][2]; c[1][1] = a[0][0] * a[2][2] - a[0][2] * a[2][0]; c[1][2] = a[0][1] * a[2][0] - a[0][0] * a[2][1];
      c[2][0] = a[0][1] * a[1][2] - a[0][2] * a[1][1]; c[2][1] = a[0][2] * a[1][0] - a[0][0] * a[1][2]; c[2][2] = a[0][0] * a[1][1] - a[0][1] * a[1][0];
      det = a[0][0] * c[0][0] + a[0][1] * c[0][1] + a[0][2] * c[0][2];
      for (int i = 0; i < 3; i++) {
         double tr = 0;
         for (int j = 0; j < 3; j++) { s->inst[k].wto[i][j] = (float)(c[j][i] / det); tr += c[j][i] / det * t[j]; }
         s->inst[k].wto[i][3] = (float)-tr;
      }
   }
}

/* ---- the rules of the ray-query chapter ----------------------------------------------------------------------------- */

/* opaque = (geometry opaque or instance force-opaque) and not instance force-no-opaque; the ray flags override both */
static int hit_is_opaque(uint32_t rflags, uint32_t geom_flags, uint32_t iflags)
{
   int op = ((geom_flags & GF_OPAQUE) || (iflags & IF_FORCE_OPAQUE)) && !(iflags & IF_FORCE_NO_OPAQUE);
   if (rflags & RF_OPAQUE) op = 1;
   if (rflags & RF_NO_OPAQUE) op = 0;
   return op;
}

/* true when the triangle must be dropped before the shader sees it */
static int tri_culled(uint32_t rflags, uint32_t iflags, int back, int opaque)
{
   if (rflags & RF_SKIP_TRIANGLES) return 1;
   int front = !back;
   if (iflags & IF_FLIP) front = !front;
   if (!(iflags & IF_CULL_DISABLE)) {
      if (front && (rflags & RF_CULL_FRONT)) return 1;
      if (!front && (rflags & RF_CULL_BACK)) return 1;
   }
   if (opaque && (rflags & RF_CULL_OPAQUE)) return 1;
   if (!opaque && (rflags & RF_CULL_NO_OPAQUE)) return 1;
   return 0;
}

static int box_culled(uint32_t rflags, int opaque)
{
   if (rflags & RF_SKIP_AABBS) return 1;
   if (opaque && (rflags & RF_CULL_OPAQUE)) return 1;
   if (!opaque && (rflags & RF_CULL_NO_OPAQUE)) return 1;
   return 0;
}

static uint32_t mix(uint32_t seed, uint32_t a, uint32_t b, uint32_t c)
{
   uint32_t h = seed * 2654435761u ^ (a + 0x9e3779b9u) * 40503u ^ (b + 0x7f4a7c15u) * 2246822519u ^ (c + 0x165667b1u) * 3266489917u;
   h ^= h >> 15; h *= 2246822519u; h ^= h >> 13;
   return h;
}

int policy_confirm_tri(const struct m_policy *p, uint32_t inst, uint32_t geom, uint32_t prim)
{
   return mix(p->seed, inst, geom, prim) % 3 != 0;
}

int policy_generate_box(const struct m_policy *p, uint32_t inst, uint32_t geom, uint32_t prim, float tentry, float *tgen)
{
   if (mix(p->seed ^ 0x55u, inst, geom, prim) % 2) return 0;
   *tgen = tentry;
   return 1;
}

/* ---- the reference ------------------------------------------------------------------------------------------------- */

struct m_hit spec_query(const struct m_scene *s, const struct m_ray *r, const struct m_policy *p, int *nvalid,
                        struct m_hit *valid, int max_valid)
{
   struct m_hit best = { 0 };
   int n = 0;
   for (int k = 0; k < s->ninst; k++) {
      const struct m_inst *in = &s->inst[k];
      if (!(in->mask & r->mask)) continue;
      const struct m_blas *bl = &s->blas[in->blas];
      float o[3], d[3];
      to_object(in, r, o, d);
      for (int i = 0; i < bl->ntri; i++) {
         float t, u, w; int front;
         if (!tri_hit(bl->tri[i].v, o, d, &t, &u, &w, &front)) continue;
         if (t < r->tmin || t > r->tmax) continue;
         int opaque = hit_is_opaque(r->flags, bl->geom_flags[bl->tri[i].geom], in->flags);
         if (tri_culled(r->flags, in->flags, !front, opaque)) continue;
         if (!opaque && !policy_confirm_tri(p, k, bl->tri[i].geom, bl->tri[i].prim)) continue;
         struct m_hit h = { HIT_TRIANGLE, t, u, w, front, (uint32_t)k, bl->tri[i].geom, bl->tri[i].prim, opaque };
         if (n < max_valid && valid) valid[n] = h;
         n++;
         if (!best.type || t < best.t) best = h;
      }
      for (int i = 0; i < bl->nbox; i++) {
         float tn, tf;
         if (!box_range(bl->box[i].lo, bl->box[i].hi, o, d, &tn, &tf)) continue;
         if (tf < r->tmin || tn > r->tmax) continue;
         int opaque = hit_is_opaque(r->flags, bl->geom_flags[bl->box[i].geom], in->flags);
         if (box_culled(r->flags, opaque)) continue;
         float tg;
         if (!policy_generate_box(p, k, bl->box[i].geom, bl->box[i].prim, tn < r->tmin ? r->tmin : tn, &tg)) continue;
         if (tg < r->tmin || tg > r->tmax) continue;
         struct m_hit h = { HIT_BOX, tg, 0, 0, 0, (uint32_t)k, bl->box[i].geom, bl->box[i].prim, opaque };
         if (n < max_valid && valid) valid[n] = h;
         n++;
         if (!best.type || tg < best.t) best = h;
      }
   }
   if (nvalid) *nvalid = n;
   return best;
}

/* ---- the hardware primitive ----------------------------------------------------------------------------------------- */

struct m_hit hw_closest(const struct m_scene *s, const struct m_ray *r, float tmin, float tmax, uint32_t mask)
{
   struct m_hit best = { 0 };
   for (int k = 0; k < s->ninst; k++) {
      const struct m_inst *in = &s->inst[k];
      if (!(in->mask & mask)) continue;
      const struct m_blas *bl = &s->blas[in->blas];
      float o[3], d[3];
      to_object(in, r, o, d);
      for (int i = 0; i < bl->ntri; i++) {
         float t, u, w; int front;
         if (!tri_hit(bl->tri[i].v, o, d, &t, &u, &w, &front)) continue;
         if (t < tmin || t > tmax) continue;
         if (!best.type || t < best.t) {
            struct m_hit h = { HIT_TRIANGLE, t, u, w, front, (uint32_t)k, bl->tri[i].geom, bl->tri[i].prim, 1 };
            best = h;
         }
      }
   }
   return best;
}

/* ---- the hybrid query: the algorithm the NIR lowering transcribes ---------------------------------------------------- */

struct hq {
   const struct m_scene *s;
   const struct m_ray *r;
   int mut;
   float tmin_cur;            /* lower bound of the next hardware query */
   struct m_hit closest;      /* committed; closest.t starts at the ray's tmax */
   float closest_t;
   struct m_hit cand;
   int phase;                 /* 0 triangles on the hardware, 1 boxes in software, 2 done */
   int box_inst, box_idx;
   int incomplete;
};

static float next_up(float t)
{
   /* the lowering works on the integer bits of the distance with the sign bit cleared (the hardware reports the distance that
    * way), so a distance of -0.0 steps to the smallest positive value instead of back to itself */
   uint32_t b; memcpy(&b, &t, 4);
   b = (b & 0x7fffffffu) + 1;
   memcpy(&t, &b, 4);
   return t;
}

/* returns 1 with hq->cand set when the shader must decide, 0 when the traversal is complete */
static int hybrid_proceed(struct hq *q)
{
   const struct m_ray *r = q->r;
   uint32_t rf = r->flags;
   int guard = 0;
   while (q->incomplete) {
      if (++guard > 5000) { q->incomplete = 0; q->closest.type = HIT_NONE; q->closest_t = -1.0f; break; }   /* only a broken variant gets here */
      if (q->phase == 0) {
         if ((rf & RF_SKIP_TRIANGLES) || q->tmin_cur > q->closest_t) { q->phase = 1; continue; }
         struct m_hit h = hw_closest(q->s, r, q->tmin_cur, q->closest_t, r->mask);
         if (!h.type) { q->phase = 1; continue; }
         if (q->mut == MUT_PREVIOUS_SCOPE) { q->closest = h; q->closest_t = h.t; q->incomplete = 0; break; }
         q->tmin_cur = q->mut == MUT_NO_ADVANCE ? h.t : next_up(h.t);
         const struct m_inst *in = &q->s->inst[h.inst];
         uint32_t iflags = q->mut == MUT_IGNORE_INSTANCE_FLAGS ? 0 : in->flags;
         int opaque = hit_is_opaque(rf, q->s->blas[in->blas].geom_flags[h.geom], iflags);
         uint32_t cf = q->mut == MUT_IGNORE_CULL ? (rf & ~(RF_CULL_BACK | RF_CULL_FRONT | RF_CULL_OPAQUE | RF_CULL_NO_OPAQUE)) : rf;
         if (tri_culled(cf, iflags, !h.front, opaque)) continue;
         h.opaque = opaque;
         if (opaque) {
            q->closest = h; q->closest_t = h.t;
            if (rf & RF_TERMINATE_FIRST) { q->incomplete = 0; break; }
            q->phase = 1;           /* everything nearer was already enumerated */
            continue;
         }
         q->cand = h;
         return 1;
      }
      if (q->phase == 1) {
         if (q->mut == MUT_NO_BOXES) { q->phase = 2; continue; }
         while (q->box_inst < q->s->ninst) {
            const struct m_inst *in = &q->s->inst[q->box_inst];
            const struct m_blas *bl = &q->s->blas[in->blas];
            if (!(in->mask & r->mask) || q->box_idx >= bl->nbox) { q->box_inst++; q->box_idx = 0; continue; }
            int i = q->box_idx++;
            float o[3], d[3], tn, tf;
            to_object(in, r, o, d);
            if (!box_range(bl->box[i].lo, bl->box[i].hi, o, d, &tn, &tf)) continue;
            if (tf < r->tmin || tn > q->closest_t) continue;
            int opaque = hit_is_opaque(rf, bl->geom_flags[bl->box[i].geom], in->flags);
            if (box_culled(rf, opaque)) continue;
            struct m_hit h = { HIT_BOX, tn < r->tmin ? r->tmin : tn, 0, 0, 0, (uint32_t)q->box_inst, bl->box[i].geom, bl->box[i].prim, opaque };
            q->cand = h;
            return 1;
         }
         q->phase = 2;
      }
      if (q->phase == 2) q->incomplete = 0;
   }
   return 0;
}

struct m_hit hybrid_query(const struct m_scene *s, const struct m_ray *r, const struct m_policy *p, int mutation,
                          int *iterations)
{
   struct hq q = { 0 };
   q.s = s; q.r = r; q.mut = mutation; q.tmin_cur = r->tmin; q.closest_t = r->tmax; q.incomplete = 1;
   int it = 0;
   while (hybrid_proceed(&q)) {
      if (++it > 600)   /* a legal run offers each triangle and box at most once: < 500 here */ { if (iterations) *iterations = -1; struct m_hit none = { 0 }; return none; }
      if (q.cand.type == HIT_TRIANGLE) {
         if (policy_confirm_tri(p, q.cand.inst, q.cand.geom, q.cand.prim)) {      /* rayQueryConfirmIntersection */
            q.closest = q.cand; q.closest_t = q.cand.t;
            if (r->flags & RF_TERMINATE_FIRST) q.incomplete = 0;
         }
      } else {
         float tg;
         if (policy_generate_box(p, q.cand.inst, q.cand.geom, q.cand.prim, q.cand.t, &tg) &&
             q.closest_t >= tg && tg >= r->tmin) {                                  /* rayQueryGenerateIntersection */
            q.cand.t = tg;
            q.closest = q.cand; q.closest_t = tg;
            if (r->flags & RF_TERMINATE_FIRST) q.incomplete = 0;
         }
      }
   }
   if (iterations) *iterations = it;
   return q.closest;
}
