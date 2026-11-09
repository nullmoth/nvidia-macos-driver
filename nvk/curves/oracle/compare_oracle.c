/* Compares an oracle dump (curve_oracle.m) with curve_math.h on the same cases and rays.
 *   cc -O2 -I.. -o compare_oracle compare_oracle.c -lm && ./compare_oracle fixture.txt
 * Per case: rays where hit/miss agree, and over common hits the worst distance and curve_parameter error. Exit 0 only when every case
 * agrees within the stated tolerance (hit/miss on 98 percent of rays, the 95th percentile of the distance and parameter errors within 0.03: the model cuts a segment into straight pieces). */
#include "../curve_float.h"
#include "curve_cases.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static float bf(uint32_t b) { float f; memcpy(&f, &b, 4); return f; }

int main(int argc, char **argv)
{
   if (argc < 2) return 2;
   FILE *f = fopen(argv[1], "r"); if (!f) { perror("open"); return 2; }
   int probe = getenv("ORACLE_PROBE") != NULL;
   struct curve_case cases[32]; int nc = probe ? oracle_probe_cases(cases) : oracle_cases(cases);
   static struct oracle_ray rs[ORACLE_RAYS]; oracle_ray_set(rs);
   char line[256]; int cur = -1, fails = 0, refused = 0;
   int agree[32] = { 0 }, total[32] = { 0 }, typeseen[32] = { 0 }; double dt[32] = { 0 }, du[32] = { 0 }; static double edt[32][ORACLE_RAYS], edu[32][ORACLE_RAYS]; int ne[32] = { 0 };
   while (fgets(line, sizeof line, f)) {
      char name[64];
      if (line[0] == '#') { printf("%s", line + 2); continue; }
      if (!strncmp(line, "case ", 5)) {
         sscanf(line + 5, "%63s", name);
         cur = -1; for (int c = 0; c < nc; c++) if (!strcmp(cases[c].name, name)) cur = c;
         if (probe && cur >= 0) oracle_probe_rays(&cases[cur], rs);
         if (strstr(line, "REFUSED")) { printf("REFUSED  %s: %s", name, line); refused++; cur = -1; }
         continue;
      }
      int i; unsigned hit, tb, ub, prim;
      if (cur < 0 || sscanf(line, "ray %d hit %u t %x u %x prim %u", &i, &hit, &tb, &ub, &prim) != 5) continue;
      const struct curve_case *k = &cases[cur];
      struct cv_curve c; struct cv_ray r;
      for (int a = 0; a < 4; a++) { c.p[a] = v3_(vf_(k->p[a][0]), vf_(k->p[a][1]), vf_(k->p[a][2])); c.r[a] = vf_(k->r[a]); }
      c.basis = vf_((float)k->basis); c.count = vf_((float)k->count); c.flat = k->type; c.caps = vf_((float)k->caps);
      r.o = v3_(vf_(rs[i].o[0]), vf_(rs[i].o[1]), vf_(rs[i].o[2])); r.d = v3_(vf_(rs[i].d[0]), vf_(rs[i].d[1]), vf_(rs[i].d[2]));
      r.tmin = vf_(rs[i].tmin); r.tmax = vf_(rs[i].tmax);
      struct cv_hit h = cv_intersect(&cv_float_ops, &c, &r);
      total[cur]++;
      if (getenv("ORACLE_RESID") && hit) {
         const struct curve_case *kk = &cases[cur]; struct curve_case rc = *kk;
         for (int a = 0; a < 4; a++) { rc.p[a][0] = kk->r[a]; rc.p[a][1] = rc.p[a][2] = 0; }
         double pt[3]; for (int a = 0; a < 3; a++) pt[a] = rs[i].o[a] + bf(tb) * rs[i].d[a];
         double best = 1e9; for (int s = 0; s <= 2000; s++) { double u = s / 2000.0, q[3], r3[3]; oracle_eval(kk, u, q); oracle_eval(&rc, u, r3);
            double dd = sqrt((pt[0]-q[0])*(pt[0]-q[0]) + (pt[1]-q[1])*(pt[1]-q[1]) + (pt[2]-q[2])*(pt[2]-q[2])) - r3[0]; if (dd < best) best = dd; }
         if (kk->type == 0) { static double res[32][ORACLE_RAYS]; static int nr[32]; res[cur][nr[cur]++] = fabs(best);
            if (fabs(best) > 0.004) printf("  RESID %s ray %3d: apple u %.4f t %.4f, field at its hit point %.4f\n", kk->name, i, bf(ub), bf(tb), best);
            (void)res; }
      }
      if (getenv("ORACLE_AXIAL") && probe && hit && (bf(ub) < 0.07f || bf(ub) > 0.93f)) {
         int endc = bf(ub) > 0.5f; double E[3], E2[3], T[3];
         oracle_eval(&cases[cur], endc ? 1.0 : 0.0, E); oracle_eval(&cases[cur], endc ? 0.999 : 0.001, E2);
         double tl = 0; for (int a = 0; a < 3; a++) { T[a] = E2[a] - E[a]; tl += T[a] * T[a]; } tl = sqrt(tl); for (int a = 0; a < 3; a++) T[a] /= tl;
         double p[3], x = 0, rho = 0; for (int a = 0; a < 3; a++) { p[a] = rs[i].o[a] + bf(tb) * rs[i].d[a] - E[a]; x += p[a] * T[a]; }
         for (int a = 0; a < 3; a++) { double q = p[a] - x * T[a]; rho += q * q; } rho = sqrt(rho);
         double Ep[3], Tp[3], xp = 0, tpl = 0; oracle_eval(&cases[cur], endc ? 1.0 - 1.0 / 16 : 1.0 / 16, Ep);
         for (int a = 0; a < 3; a++) { Tp[a] = Ep[a] - E[a]; tpl += Tp[a] * Tp[a]; } tpl = sqrt(tpl); for (int a = 0; a < 3; a++) xp += p[a] * Tp[a] / tpl;
         struct cv_curve cc; struct cv_ray rr; const struct curve_case *k = &cases[cur];
         for (int a = 0; a < 4; a++) { cc.p[a] = v3_(vf_(k->p[a][0]), vf_(k->p[a][1]), vf_(k->p[a][2])); cc.r[a] = vf_(k->r[a]); }
         cc.basis = vf_((float)k->basis); cc.count = vf_((float)k->count); cc.flat = k->type; cc.caps = vf_((float)k->caps);
         rr.o = v3_(vf_(rs[i].o[0]), vf_(rs[i].o[1]), vf_(rs[i].o[2])); rr.d = v3_(vf_(rs[i].d[0]), vf_(rs[i].d[1]), vf_(rs[i].d[2])); rr.tmin = vf_(0); rr.tmax = vf_(100);
         struct cv_hit mh = cv_intersect(&cv_float_ops, &cc, &rr);
         printf("  AXIAL %s end %d ray %3d: x_tangent %.4f x_piece %.4f rho %.4f (end radius %.3f) u %.4f t %.4f | model %s t %.4f\n", cases[cur].name, endc, i, x, xp, rho, cases[cur].r[endc ? cases[cur].count - 1 : 0], bf(ub), bf(tb), mh.hit ? "hit" : "miss", mh.hit ? fv_(mh.t) : 0.0);
      }
      if (getenv("ORACLE_DIFF") && (probe ? i < 3 * PROBE_GROUP : !strcmp(cases[cur].name, getenv("ORACLE_DIFF")))) {
         int mh = (int)h.hit, ah = hit != 0;
         double dd = (mh && ah) ? fabs(fv_(h.t) - bf(tb)) : 0;
         if (mh && !ah) {
            double E0[3], E1[3], F0[3], F1[3], p[3], x0 = 0, x1 = 0, tl0 = 0, tl1 = 0;
            oracle_eval(&cases[cur], 0.0, E0); oracle_eval(&cases[cur], 0.002, F0); oracle_eval(&cases[cur], 1.0, E1); oracle_eval(&cases[cur], 0.998, F1);
            for (int a = 0; a < 3; a++) { p[a] = rs[i].o[a] + fv_(h.t) * rs[i].d[a]; tl0 += (F0[a]-E0[a])*(F0[a]-E0[a]); tl1 += (E1[a]-F1[a])*(E1[a]-F1[a]); }
            tl0 = sqrt(tl0); tl1 = sqrt(tl1);
            for (int a = 0; a < 3; a++) { x0 += (p[a]-E0[a]) * (F0[a]-E0[a]) / tl0; x1 += (p[a]-E1[a]) * (E1[a]-F1[a]) / tl1; }
            printf("  EXTRA %s ray %3d: model-only hit at u %.4f, axial from start %.4f, from end %.4f\n", cases[cur].name, i, fv_(h.u), x0, x1);
         }
         if (mh != ah || dd > 0.02) printf("  %s group %d ray %3d: apple %s t %.4f u %.4f | model %s t %.4f u %.4f\n", cases[cur].name, i / PROBE_GROUP, i, ah ? "hit" : "miss", bf(tb), bf(ub), mh ? "hit" : "miss", mh ? fv_(h.t) : 0.0, mh ? fv_(h.u) : 0.0);
      }
      if (hit && hit != 3) typeseen[cur] = (int)hit;
      hit = hit != 0;
      if ((int)h.hit == (int)hit) {
         agree[cur]++;
         if (hit) { double e = fabs(fv_(h.t) - bf(tb)); if (e > dt[cur]) dt[cur] = e; edt[cur][ne[cur]] = e; e = fabs(fv_(h.u) - bf(ub)); if (e > du[cur]) du[cur] = e; edu[cur][ne[cur]++] = e; }
      }
   }
   for (int c = 0; c < nc; c++) {
      if (!total[c]) continue;
      int n = ne[c]; double p95t = 0, p95u = 0;
      for (int a = 0; a < n; a++) for (int b = a + 1; b < n; b++) {
         if (edt[c][b] < edt[c][a]) { double x = edt[c][a]; edt[c][a] = edt[c][b]; edt[c][b] = x; }
         if (edu[c][b] < edu[c][a]) { double x = edu[c][a]; edu[c][a] = edu[c][b]; edu[c][b] = x; }
      }
      if (n) { p95t = edt[c][n * 95 / 100]; p95u = edu[c][n * 95 / 100]; }
      int ok = agree[c] * 100 >= total[c] * 98 && p95t <= 0.03 && p95u <= 0.03;
      printf("%s %-22s hit/miss agree %d of %d, p95 dt %.4f du %.4f, worst dt %.4f du %.4f (%d common hits)%s\n", ok ? "ok  " : "FAIL", cases[c].name,
             agree[c], total[c], p95t, p95u, dt[c], du[c], n, typeseen[c] ? " (reported a type other than curve)" : "");
      fails += !ok;
   }
   printf("%d cases differ, %d refused\n", fails, refused);
   return fails || refused;
}
