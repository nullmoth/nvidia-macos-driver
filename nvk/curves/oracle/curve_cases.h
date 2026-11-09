/* The curve cases and the ray set shared by the Apple oracle (curve_oracle.m) and the comparison (compare_oracle.c): plain C, no
 * dependencies, deterministic. A case is one curve segment. Basis and type codes are the Metal enum values (basis: B-spline 0,
 * Catmull-Rom 1, linear 2, Bezier 3; type: round 0, flat 1; caps: none 0, disk 1, sphere 2). */
#ifndef CURVE_CASES_H
#define CURVE_CASES_H
#include <stdint.h>
#include <stdio.h>
#include <math.h>
#include <string.h>

struct curve_case { char name[48]; int basis, count, type, caps; float p[4][3], r[4]; };
struct oracle_ray { float o[3], d[3], tmin, tmax; };

#define ORACLE_RAYS 576

static const char *const oracle_basis_name[] = { "bspline", "catmull", "linear", "bezier" };
static const struct { int basis, count; } oracle_combos[] = { { 3, 4 }, { 3, 3 }, { 0, 4 }, { 0, 3 }, { 1, 4 }, { 2, 2 } };

static inline uint32_t oracle_rng(uint32_t *s) { *s = *s * 1664525u + 1013904223u; return *s >> 8; }
static inline float oracle_f(uint32_t *s, float a, float b) { return a + (b - a) * ((oracle_rng(s) & 0xffff) / 65535.0f); }

/* every valid combination, round with each cap mode and flat once: 6 * 3 + 6 = 24 cases */
static inline int oracle_cases(struct curve_case *out)
{
   int n = 0;
   for (int c = 0; c < 6; c++)
      for (int type = 0; type < 2; type++)
         for (int caps = 0; caps < (type ? 1 : 3); caps++) {
            struct curve_case *k = &out[n];
            uint32_t s = 1000u + (uint32_t)c * 17u;       /* the same control points for every type and cap of a basis */
            k->basis = oracle_combos[c].basis; k->count = oracle_combos[c].count; k->type = type; k->caps = type ? 2 : caps;
            snprintf(k->name, sizeof k->name, "%s%d_%s%s", oracle_basis_name[k->basis], k->count, type ? "flat" : "round",
                     type ? "" : (caps == 0 ? "_nocap" : caps == 1 ? "_disk" : "_sphere"));
            for (int i = 0; i < 4; i++) {
               k->p[i][0] = oracle_f(&s, -1.2f, 1.2f); k->p[i][1] = oracle_f(&s, -1.2f, 1.2f); k->p[i][2] = oracle_f(&s, -1.2f, 1.2f);
               k->r[i] = oracle_f(&s, 0.10f, 0.30f);
            }
            n++;
         }
   return n;
}

/* a 16 x 16 grid of rays straight down +z at the curve's neighbourhood, a 16 x 16 grid from the side, and 64 oblique rays */
static inline void oracle_ray_set(struct oracle_ray *r)
{
   int n = 0;
   for (int i = 0; i < 16; i++) for (int j = 0; j < 16; j++) {
      struct oracle_ray q = { { -1.5f + 3.0f * (i + 0.5f) / 16, -1.5f + 3.0f * (j + 0.5f) / 16, -5 }, { 0, 0, 1 }, 0, 100 };
      r[n++] = q;
   }
   for (int i = 0; i < 16; i++) for (int j = 0; j < 16; j++) {
      struct oracle_ray q = { { -5, -1.5f + 3.0f * (i + 0.5f) / 16, -1.5f + 3.0f * (j + 0.5f) / 16 }, { 1, 0, 0 }, 0, 100 };
      r[n++] = q;
   }
   uint32_t s = 77;
   while (n < ORACLE_RAYS) {
      float t[3] = { oracle_f(&s, -1, 1), oracle_f(&s, -1, 1), oracle_f(&s, -1, 1) };
      float o[3] = { oracle_f(&s, -4, 4), oracle_f(&s, -4, 4), (n & 1) ? 4.0f : -4.0f };
      struct oracle_ray q = { { o[0], o[1], o[2] }, { t[0] - o[0], t[1] - o[1], t[2] - o[2] }, 0, 100 };
      r[n++] = q;
   }
}

/* ---- the targeted probe: one curve, dense rays at both ends and along the middle -------------------------------------------------- */
#define PROBE_RAYS ORACLE_RAYS
#define PROBE_GROUP (ORACLE_RAYS / 3)

static inline void oracle_eval(const struct curve_case *c, double u, double q[3])
{
   double w[4] = { 0, 0, 0, 0 }, v = 1 - u;
   if (c->basis == 3 && c->count == 4) { w[0] = v * v * v; w[1] = 3 * u * v * v; w[2] = 3 * u * u * v; w[3] = u * u * u; }
   else if (c->basis == 3 && c->count == 3) { w[0] = v * v; w[1] = 2 * u * v; w[2] = u * u; }
   else if (c->basis == 0 && c->count == 3) { w[0] = v * v / 2; w[1] = (-2 * u * u + 2 * u + 1) / 2; w[2] = u * u / 2; }
   else if (c->basis == 0 && c->count == 4) { w[0] = v * v * v / 6; w[1] = (3 * u * u * u - 6 * u * u + 4) / 6; w[2] = (-3 * u * u * u + 3 * u * u + 3 * u + 1) / 6; w[3] = u * u * u / 6; }
   else if (c->basis == 1 && c->count == 4) { w[0] = (-u * u * u + 2 * u * u - u) / 2; w[1] = (3 * u * u * u - 5 * u * u + 2) / 2; w[2] = (-3 * u * u * u + 4 * u * u + u) / 2; w[3] = (u * u * u - u * u) / 2; }
   else { w[0] = v; w[1] = u; }
   for (int k = 0; k < 3; k++) { q[k] = 0; for (int i = 0; i < 4; i++) q[k] += w[i] * c->p[i][k]; }
}

/* three groups of PROBE_GROUP rays: aimed at the curve's start point, its end point, and its middle. Each ray comes from a direction of a
 * Fibonacci sphere, 3 away, shifted sideways by up to 0.45 in two directions, and flies parallel to that direction. */
static inline void oracle_probe_rays(const struct curve_case *c, struct oracle_ray *r)
{
   double tgt[3][3];
   oracle_eval(c, 0.0, tgt[0]); oracle_eval(c, 1.0, tgt[1]); oracle_eval(c, 0.5, tgt[2]);
   uint32_t s = 4242;
   for (int g = 0; g < 3; g++)
      for (int i = 0; i < PROBE_GROUP; i++) {
         double z = 1 - 2 * (i + 0.5) / PROBE_GROUP, rad = sqrt(1 - z * z), phi = i * 2.399963229728653;
         double n[3] = { rad * cos(phi), rad * sin(phi), z };
         double a[3] = { -n[1], n[0], 0 }; double al = sqrt(a[0] * a[0] + a[1] * a[1]);
         if (al < 1e-6) { a[0] = 1; a[1] = 0; al = 1; }
         for (int k = 0; k < 3; k++) a[k] /= al;
         double b[3] = { n[1] * a[2] - n[2] * a[1], n[2] * a[0] - n[0] * a[2], n[0] * a[1] - n[1] * a[0] };
         double j1 = oracle_f(&s, -0.45f, 0.45f), j2 = oracle_f(&s, -0.45f, 0.45f);
         struct oracle_ray *q = &r[g * PROBE_GROUP + i];
         for (int k = 0; k < 3; k++) { q->o[k] = (float)(tgt[g][k] + 3 * n[k] + j1 * a[k] + j2 * b[k]); q->d[k] = (float)-n[k]; }
         q->tmin = 0; q->tmax = 100;
      }
   for (int i = 3 * PROBE_GROUP; i < ORACLE_RAYS; i++) { memset(&r[i], 0, sizeof r[i]); r[i].d[2] = 1; r[i].tmax = 100; r[i].o[2] = -50; }
}

/* the three cases that disagree with Apple's curves, by name */
static inline int oracle_probe_cases(struct curve_case *out)
{
   struct curve_case all[32]; int n = oracle_cases(all), m = 0;
   const char *want[] = { "bezier4_round_disk", "catmull4_round_nocap", "catmull4_flat" };
   for (int w = 0; w < 3; w++) for (int i = 0; i < n; i++) if (!strcmp(all[i].name, want[w])) out[m++] = all[i];
   return m;
}
#endif
