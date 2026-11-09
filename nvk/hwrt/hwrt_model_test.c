/* Arms for hwrt_model.c. Each arm is a named sentence; controls break the algorithm on purpose and must be caught.
 * Build and run: cc -O2 -o hwrt_model_test hwrt_model.c hwrt_model_test.c -lm && ./hwrt_model_test */
#include "hwrt_model.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint32_t rng_state = 12345;
static uint32_t rnd(void) { rng_state = rng_state * 1664525u + 1013904223u; return rng_state >> 8; }
static float frand(float a, float b) { return a + (b - a) * ((rnd() & 0xffff) / 65535.0f); }

static struct m_scene make_scene(int boxes, int tie_free)
{
   struct m_scene s = { 0 };
   s.nblas = 3; s.ninst = 2 + rnd() % 5;
   s.blas = calloc(s.nblas, sizeof *s.blas);
   s.inst = calloc(s.ninst, sizeof *s.inst);
   for (int b = 0; b < s.nblas; b++) {
      struct m_blas *bl = &s.blas[b];
      bl->ngeom = 1 + rnd() % 3;
      for (int g = 0; g < 8; g++) bl->geom_flags[g] = (rnd() & 1) ? GF_OPAQUE : 0;
      bl->ntri = 6 + rnd() % 40;
      bl->tri = calloc(bl->ntri, sizeof *bl->tri);
      for (int i = 0; i < bl->ntri; i++) {
         float c[3] = { frand(-4, 4), frand(-4, 4), frand(-4, 4) };
         for (int v = 0; v < 3; v++) for (int k = 0; k < 3; k++) bl->tri[i].v[v][k] = c[k] + frand(-1.2f, 1.2f);
         bl->tri[i].geom = i % bl->ngeom; bl->tri[i].prim = i;
      }
      bl->nbox = boxes ? rnd() % 5 : 0;
      bl->box = calloc(bl->nbox + 1, sizeof *bl->box);
      for (int i = 0; i < bl->nbox; i++) {
         for (int k = 0; k < 3; k++) { float c = frand(-4, 4), h = frand(0.2f, 1.0f); bl->box[i].lo[k] = c - h; bl->box[i].hi[k] = c + h; }
         bl->box[i].geom = rnd() % bl->ngeom; bl->box[i].prim = 1000 + i;
      }
   }
   for (int k = 0; k < s.ninst; k++) {
      struct m_inst *in = &s.inst[k];
      in->blas = rnd() % s.nblas;
      float a = frand(0, 6.28f), c = cosf(a), sn = sinf(a), sc = frand(0.6f, 1.6f);
      if (!tie_free && (rnd() & 7) == 0) sc = -sc;     /* the occasional mirrored instance */
      float m[3][3] = { { c * sc, -sn * sc, 0 }, { sn * sc, c * sc, 0 }, { 0, 0, sc } };
      for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) in->otw[i][j] = m[i][j]; in->otw[i][3] = frand(-2, 2); }
      in->mask = 1 + rnd() % 255; in->flags = rnd() & 0xf;
      if ((in->flags & IF_FORCE_OPAQUE) && (in->flags & IF_FORCE_NO_OPAQUE)) in->flags &= ~IF_FORCE_NO_OPAQUE;
   }
   scene_finish(&s);
   return s;
}

static void free_scene(struct m_scene *s)
{
   for (int b = 0; b < s->nblas; b++) { free(s->blas[b].tri); free(s->blas[b].box); }
   free(s->blas); free(s->inst);
}

static struct m_ray make_ray(void)
{
   struct m_ray r = { 0 };
   float tgt[3] = { frand(-4, 4), frand(-4, 4), frand(-4, 4) };
   for (int k = 0; k < 3; k++) { r.org[k] = frand(-9, 9); r.dir[k] = tgt[k] - r.org[k]; }
   r.tmin = (rnd() & 3) ? 0.0f : frand(0, 4); r.tmax = (rnd() & 3) ? 1e4f : frand(5, 30);
   r.mask = 1 + rnd() % 255;
   uint32_t f = 0;
   if (rnd() & 1) f |= (rnd() & 1) ? RF_OPAQUE : RF_NO_OPAQUE;
   if (!(rnd() & 3)) f |= RF_TERMINATE_FIRST;
   if (!(rnd() & 3)) f |= RF_CULL_BACK;
   if (!(rnd() & 3)) f |= RF_CULL_FRONT;
   if (!(rnd() & 5)) f |= RF_CULL_OPAQUE;
   if (!(rnd() & 5)) f |= RF_CULL_NO_OPAQUE;
   if (!(rnd() & 7)) f |= RF_SKIP_TRIANGLES;
   if (!(rnd() & 7)) f |= RF_SKIP_AABBS;
   r.flags = f;
   return r;
}

/* returns the number of rays on which the hybrid disagrees with the reference; *hits counts rays that hit something */
static int compare(int scenes, int rays_per, int boxes, int mutation, int *hits, int *types_seen)
{
   int bad = 0; *hits = 0; types_seen[0] = types_seen[1] = types_seen[2] = 0;
   for (int sc = 0; sc < scenes; sc++) {
      struct m_scene s = make_scene(boxes, 1);
      struct m_policy p = { rnd() };
      for (int i = 0; i < rays_per; i++) {
         struct m_ray r = make_ray();
         struct m_hit valid[512]; int nv;
         struct m_hit ref = spec_query(&s, &r, &p, &nv, valid, 512);
         int it;
         struct m_hit got = hybrid_query(&s, &r, &p, mutation, &it);
         int ok;
         if (it < 0) ok = 0;
         else if (r.flags & RF_TERMINATE_FIRST) {
            /* any confirmed hit is a legal answer; a miss only if there is none */
            ok = ref.type == HIT_NONE ? got.type == HIT_NONE : 0;
            for (int k = 0; k < nv && k < 512 && got.type != HIT_NONE && !ok; k++)
               ok = valid[k].type == got.type && valid[k].inst == got.inst && valid[k].geom == got.geom &&
                    valid[k].prim == got.prim && valid[k].t == got.t;
         } else {
            ok = ref.type == got.type && (ref.type == HIT_NONE ||
                 (ref.t == got.t && ref.inst == got.inst && ref.geom == got.geom && ref.prim == got.prim));
         }
         if (ref.type != HIT_NONE) { (*hits)++; types_seen[ref.type]++; }
         bad += !ok;
      }
      free_scene(&s);
   }
   return bad;
}

static int fails;
static void arm(const char *name, int ok, const char *detail)
{
   printf("%s %s%s%s\n", ok ? "ok  " : "FAIL", name, ok || !detail[0] ? "" : "  ", ok ? "" : detail);
   fails += !ok;
}

int main(void)
{
   char d[160];
   int hits, seen[3];

   int bad = compare(12, 800, 0, MUT_NONE, &hits, seen);
   snprintf(d, sizeof d, "%d of 9600 rays differ", bad);
   arm("hardware-only scenes (triangles, every ray flag, instance and geometry flags): hybrid equals the reference", bad == 0, d);
   snprintf(d, sizeof d, "%d of 9600 rays hit", hits);
   arm("  that run committed a hit on at least one ray in eight, so the comparison means something", hits > 1200, d);

   bad = compare(12, 800, 1, MUT_NONE, &hits, seen);
   snprintf(d, sizeof d, "%d of 9600 rays differ", bad);
   arm("scenes with procedural boxes: hybrid equals the reference", bad == 0, d);
   arm("  both triangle hits and box hits were committed in that run", seen[HIT_TRIANGLE] > 500 && seen[HIT_BOX] > 100, "one kind never won");

   int b2 = compare(8, 300, 1, MUT_NO_ADVANCE, &hits, seen);
   arm("control: asking again from the same distance never ends, and is caught", b2 > 0, "not caught");
   b2 = compare(8, 300, 1, MUT_IGNORE_CULL, &hits, seen);
   arm("control: ignoring the cull flags is caught", b2 > 100, "not caught");
   b2 = compare(8, 300, 1, MUT_NO_BOXES, &hits, seen);
   arm("control: leaving the boxes out is caught", b2 > 100, "not caught");
   b2 = compare(8, 300, 1, MUT_IGNORE_INSTANCE_FLAGS, &hits, seen);
   arm("control: ignoring the instance flags is caught", b2 > 100, "not caught");

   b2 = compare(8, 300, 0, MUT_PREVIOUS_SCOPE, &hits, seen);
   snprintf(d, sizeof d, "only %d of 2400 rays differ", b2);
   arm("control: the first lowering (one query, nearest triangle committed whatever the flags) fails on this suite", b2 > 100, d);

   /* The one documented limit: two non-opaque triangles at an identical distance. The next query starts just past the first
    * hit, so the second is never offered. The reference offers both. */
   {
      struct m_scene s = { 0 };
      s.nblas = 1; s.ninst = 1;
      s.blas = calloc(1, sizeof *s.blas); s.inst = calloc(1, sizeof *s.inst);
      s.blas[0].ntri = 2; s.blas[0].ngeom = 2; s.blas[0].tri = calloc(2, sizeof(struct m_tri));
      float tri[3][3] = { { -1, -1, 5 }, { 1, -1, 5 }, { 0, 1, 5 } };
      for (int i = 0; i < 2; i++) { memcpy(s.blas[0].tri[i].v, tri, sizeof tri); s.blas[0].tri[i].geom = i; s.blas[0].tri[i].prim = i; }
      s.inst[0].mask = 0xff; for (int i = 0; i < 3; i++) s.inst[0].otw[i][i] = 1;
      scene_finish(&s);
      struct m_ray r = { { 0, 0, 0 }, { 0, 0, 1 }, 0, 100, RF_NO_OPAQUE, 0xff };
      /* a policy that confirms only the second triangle: the reference finds it, the hybrid does not */
      struct m_policy p;
      int found = -1;
      for (p.seed = 0; p.seed < 200; p.seed++)
         if (!policy_confirm_tri(&p, 0, 0, 0) && policy_confirm_tri(&p, 0, 1, 1)) { found = 1; break; }
      struct m_hit ref = spec_query(&s, &r, &p, NULL, NULL, 0), got = hybrid_query(&s, &r, &p, MUT_NONE, NULL);
      arm("limit: two non-opaque triangles at a bit-identical distance, the second is not offered (documented)",
          found == 1 && ref.type == HIT_TRIANGLE && ref.prim == 1 && got.type == HIT_NONE, "limit changed, update docs/HWRT.md");
      free_scene(&s);
   }

   /* progress: a long run of non-opaque hits terminates in at most one query per primitive */
   {
      struct m_scene s = make_scene(0, 1);
      struct m_policy p = { 7 };
      int worst = 0;
      for (int i = 0; i < 3000; i++) {
         struct m_ray r = make_ray(); r.flags = RF_NO_OPAQUE; int it;
         hybrid_query(&s, &r, &p, MUT_NONE, &it);
         if (it > worst) worst = it;
      }
      int total = 0; for (int i = 0; i < s.ninst; i++) total += s.blas[s.inst[i].blas].ntri;
      snprintf(d, sizeof d, "worst %d candidates for %d triangles", worst, total);
      arm("every candidate is offered at most once (candidates <= triangles in the scene)", worst <= total, d);
      free_scene(&s);
   }

   printf("%d arms failed\n", fails);
   return fails != 0;
}
