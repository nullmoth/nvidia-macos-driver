/* The end sweep: for one curve, fine grids of parallel rays around each end point, from three sides. Shared by curve_sweep.m and sweep_analyze.c. */
#ifndef CURVE_SWEEP_H
#define CURVE_SWEEP_H
#include "curve_cases.h"
#define SW_N 64                    /* grid is SW_N x SW_N */
#define SW_BATCH (SW_N * SW_N)
#define SW_BATCHES 6               /* 2 ends x 3 views */
#define SW_HALF 0.6                /* half-width of the grid around the end point */

static inline void sw_unit(double *v) { double l = sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]); for (int k = 0; k < 3; k++) v[k] /= l; }
static inline void sw_cross(const double *a, const double *b, double *c) { c[0] = a[1]*b[2]-a[2]*b[1]; c[1] = a[2]*b[0]-a[0]*b[2]; c[2] = a[0]*b[1]-a[1]*b[0]; }

/* frame at an end: point E, tangent T pointing into the curve from that end's start side (along increasing u at the start, decreasing at the end:
 * T always points from the end point into the curve), normals N1, N2 */
static inline void sw_frame(const struct curve_case *c, int end, double *E, double *T, double *N1, double *N2)
{
   double F[3]; oracle_eval(c, end ? 1.0 : 0.0, E); oracle_eval(c, end ? 0.998 : 0.002, F);
   for (int k = 0; k < 3; k++) T[k] = F[k] - E[k];
   sw_unit(T);
   double a[3] = { 0, 0, 1 }; if (fabs(T[2]) > 0.9) { a[0] = 1; a[2] = 0; }
   sw_cross(T, a, N1); sw_unit(N1); sw_cross(T, N1, N2); sw_unit(N2);
}

/* batch b = end * 3 + view; view 0: rays travel along -N1 (grid: axial, N2), view 1: along -N2 (grid: axial, N1), view 2: head-on along -(outward) */
static inline void sw_rays(const struct curve_case *c, int batch, struct oracle_ray *r)
{
   int end = batch / 3, view = batch % 3; double E[3], T[3], N1[3], N2[3];
   sw_frame(c, end, E, T, N1, N2);
   for (int i = 0; i < SW_N; i++) for (int j = 0; j < SW_N; j++) {
      double a = -SW_HALF + 2 * SW_HALF * (i + 0.5) / SW_N, b = -SW_HALF + 2 * SW_HALF * (j + 0.5) / SW_N;
      double o[3], d[3];
      for (int k = 0; k < 3; k++) {
         if (view == 0) { o[k] = E[k] + 2 * N1[k] + a * T[k] + b * N2[k]; d[k] = -N1[k]; }
         else if (view == 1) { o[k] = E[k] + 2 * N2[k] + a * T[k] + b * N1[k]; d[k] = -N2[k]; }
         else { o[k] = E[k] - 2 * T[k] + a * N1[k] + b * N2[k]; d[k] = T[k]; }
      }
      struct oracle_ray *q = &r[i * SW_N + j];
      for (int k = 0; k < 3; k++) { q->o[k] = (float)o[k]; q->d[k] = (float)d[k]; }
      q->tmin = 0; q->tmax = 100;
   }
}

static inline int sw_cases(struct curve_case *out)
{
   struct curve_case all[32]; int n = oracle_cases(all), m = 0;
   const char *want[] = { "catmull4_round_nocap", "catmull4_flat", "bspline4_flat", "bezier4_flat", "linear2_flat", "bspline3_flat", "bezier3_flat" };
   for (int w = 0; w < 7; w++) for (int i = 0; i < n; i++) if (!strcmp(all[i].name, want[w])) out[m++] = all[i];
   return m;
}
#endif
