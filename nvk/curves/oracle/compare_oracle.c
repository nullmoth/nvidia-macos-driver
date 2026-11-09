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
   struct curve_case cases[32]; int nc = oracle_cases(cases);
   static struct oracle_ray rs[ORACLE_RAYS]; oracle_ray_set(rs);
   char line[256]; int cur = -1, fails = 0, refused = 0;
   int agree[32] = { 0 }, total[32] = { 0 }, typeseen[32] = { 0 }; double dt[32] = { 0 }, du[32] = { 0 }; static double edt[32][ORACLE_RAYS], edu[32][ORACLE_RAYS]; int ne[32] = { 0 };
   while (fgets(line, sizeof line, f)) {
      char name[64];
      if (line[0] == '#') { printf("%s", line + 2); continue; }
      if (!strncmp(line, "case ", 5)) {
         sscanf(line + 5, "%63s", name);
         cur = -1; for (int c = 0; c < nc; c++) if (!strcmp(cases[c].name, name)) cur = c;
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
