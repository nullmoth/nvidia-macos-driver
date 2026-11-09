/* Reads a sweep dump and prints, per case and batch, a map of the grid: '.' both miss, '#' both hit and agree, 'A' Apple only, 'M' model only,
 * 'd' both hit with different distance.   cc -O2 -I.. -o sweep_analyze sweep_analyze.c -lm && ./sweep_analyze dump.txt */
#include "../curve_float.h"
#include "sweep.h"
#include <stdlib.h>
static float bf(uint32_t b) { float f; memcpy(&f, &b, 4); return f; }
int main(int argc, char **argv)
{
   FILE *f = fopen(argv[1], "r"); if (!f) return 2;
   struct curve_case cases[8]; int nc = sw_cases(cases); static struct oracle_ray rs[SW_BATCH];
   char line[200]; int cur = -1, batch = -1; static char map[SW_N][SW_N + 1]; int cnt[5] = { 0 };
   int fail_total = 0;
   while (fgets(line, sizeof line, f)) {
      char name[64]; int b;
      if (line[0] == '#') { printf("%s", line + 2); continue; }
      if (sscanf(line, "case %63s batch %d", name, &b) == 2) {
         if (cur >= 0 && batch >= 0) { printf("== %s end %d view %d   (rows: axial -> , columns: lateral)  both-miss %d agree %d apple-only %d model-only %d differ %d\n", cases[cur].name, batch / 3, batch % 3, cnt[0], cnt[1], cnt[2], cnt[3], cnt[4]); for (int i = 0; i < SW_N; i += 2) printf("%.*s\n", SW_N, map[i]); fail_total += cnt[2] + cnt[3] + cnt[4]; }
         cur = -1; for (int c = 0; c < nc; c++) if (!strcmp(cases[c].name, name)) cur = c;
         batch = b; memset(cnt, 0, sizeof cnt); if (cur >= 0) sw_rays(&cases[cur], b, rs); continue;
      }
      int i; unsigned hit, tb, ub;
      if (cur < 0 || sscanf(line, "ray %d hit %u t %x u %x", &i, &hit, &tb, &ub) != 4) continue;
      const struct curve_case *k = &cases[cur]; struct cv_curve c; struct cv_ray r;
      for (int a = 0; a < 4; a++) { c.p[a] = v3_(vf_(k->p[a][0]), vf_(k->p[a][1]), vf_(k->p[a][2])); c.r[a] = vf_(k->r[a]); }
      c.basis = vf_((float)k->basis); c.count = vf_((float)k->count); c.flat = k->type; c.caps = vf_((float)k->caps);
      r.o = v3_(vf_(rs[i].o[0]), vf_(rs[i].o[1]), vf_(rs[i].o[2])); r.d = v3_(vf_(rs[i].d[0]), vf_(rs[i].d[1]), vf_(rs[i].d[2])); r.tmin = vf_(0); r.tmax = vf_(100);
      struct cv_hit h = cv_intersect(&cv_float_ops, &c, &r); int ah = hit != 0, mh = (int)h.hit; char ch; int cls;
      if (!ah && !mh) { ch = '.'; cls = 0; } else if (ah && mh) { if (fabs(fv_(h.t) - bf(tb)) > 0.02) { ch = 'd'; cls = 4; } else { ch = '#'; cls = 1; } }
      else if (ah) { ch = 'A'; cls = 2; } else { ch = 'M'; cls = 3; }
      map[i / SW_N][i % SW_N] = ch; map[i / SW_N][SW_N] = 0; cnt[cls]++;
   }
   if (cur >= 0 && batch >= 0) { printf("== %s end %d view %d  both-miss %d agree %d apple-only %d model-only %d differ %d\n", cases[cur].name, batch / 3, batch % 3, cnt[0], cnt[1], cnt[2], cnt[3], cnt[4]); for (int i = 0; i < SW_N; i += 2) printf("%.*s\n", SW_N, map[i]); }
   return 0;
}
