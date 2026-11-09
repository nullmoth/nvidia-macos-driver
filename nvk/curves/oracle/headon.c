/* head-on residual: for a case and batch, list rays where Apple and the model both hit with different distances.  usage: headon dump case batch */
#include "../curve_float.h"
#include "sweep.h"
#include <stdlib.h>
static float bf(uint32_t b) { float f; memcpy(&f, &b, 4); return f; }
int main(int argc, char **argv)
{
   FILE *f = fopen(argv[1], "r"); struct curve_case cs[8]; int nc = sw_cases(cs); int want = -1; for (int c = 0; c < nc; c++) if (!strcmp(cs[c].name, argv[2])) want = c;
   int wb = atoi(argv[3]); static struct oracle_ray rs[SW_BATCH]; sw_rays(&cs[want], wb, rs);
   char line[200]; int on = 0; int shown = 0; int na = 0, nm = 0, nboth = 0, nsame = 0;
   while (fgets(line, sizeof line, f)) {
      char name[64]; int b;
      if (sscanf(line, "case %63s batch %d", name, &b) == 2) { on = !strcmp(name, argv[2]) && b == wb; continue; }
      int i; unsigned hit, tb, ub; if (!on || sscanf(line, "ray %d hit %u t %x u %x", &i, &hit, &tb, &ub) != 4) continue;
      const struct curve_case *k = &cs[want]; struct cv_curve c; struct cv_ray r;
      for (int a = 0; a < 4; a++) { c.p[a] = v3_(vf_(k->p[a][0]), vf_(k->p[a][1]), vf_(k->p[a][2])); c.r[a] = vf_(k->r[a]); }
      c.basis = vf_((float)k->basis); c.count = vf_((float)k->count); c.flat = k->type; c.caps = vf_((float)k->caps);
      r.o = v3_(vf_(rs[i].o[0]), vf_(rs[i].o[1]), vf_(rs[i].o[2])); r.d = v3_(vf_(rs[i].d[0]), vf_(rs[i].d[1]), vf_(rs[i].d[2])); r.tmin = vf_(0); r.tmax = vf_(100);
      struct cv_hit h = cv_intersect(&cv_float_ops, &c, &r); int ah = hit != 0, mh = (int)h.hit;
      double a_ = -SW_HALF + 2 * SW_HALF * ((i / SW_N) + 0.5) / SW_N, b_ = -SW_HALF + 2 * SW_HALF * ((i % SW_N) + 0.5) / SW_N;
      if (ah && mh) { if (fabs(fv_(h.t) - bf(tb)) < 0.02) nsame++; else { nboth++; if (shown++ < 14) printf("  (%.3f,%.3f) rho %.3f: apple t %.4f u %.3f | model t %.4f u %.3f\n", a_, b_, sqrt(a_*a_+b_*b_), bf(tb), bf(ub), fv_(h.t), fv_(h.u)); } }
      else if (ah) na++; else if (mh) nm++;
   }
   printf("%s batch %d: agree %d, both-hit-different %d, apple-only %d, model-only %d; radius0 %.3f radius1 %.3f\n", argv[2], wb, nsame, nboth, na, nm, cs[want].r[0], cs[want].r[cs[want].count-1]);
   return 0; }
