/* The curve cases and the ray set shared by the Apple oracle (curve_oracle.m) and the comparison (compare_oracle.c): plain C, no
 * dependencies, deterministic. A case is one curve segment. Basis and type codes are the Metal enum values (basis: B-spline 0,
 * Catmull-Rom 1, linear 2, Bezier 3; type: round 0, flat 1; caps: none 0, disk 1, sphere 2). */
#ifndef CURVE_CASES_H
#define CURVE_CASES_H
#include <stdint.h>
#include <stdio.h>

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
#endif
