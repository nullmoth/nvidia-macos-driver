/* Writes a dump in the oracle's format from curve_math.h itself (or from a deliberately different curve with --perturb), to prove
 * the comparison tool passes what it should and fails what it should, without a GPU.   cc -O2 -I.. -o gen_dump gen_dump.c -lm */
#include "../curve_float.h"
#include "curve_cases.h"
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv)
{
   int perturb = argc > 1 && !strcmp(argv[1], "--perturb");
   struct curve_case cases[32]; int nc = oracle_cases(cases);
   static struct oracle_ray rs[ORACLE_RAYS]; oracle_ray_set(rs);
   printf("# device synthetic\n");
   for (int ci = 0; ci < nc; ci++) {
      const struct curve_case *k = &cases[ci];
      printf("case %s basis %d count %d type %d caps %d\n", k->name, k->basis, k->count, k->type, k->caps);
      for (int i = 0; i < ORACLE_RAYS; i++) {
         struct cv_curve c; struct cv_ray r;
         for (int a = 0; a < 4; a++) { c.p[a] = v3_(vf_(k->p[a][0]), vf_(k->p[a][1]), vf_(k->p[a][2])); c.r[a] = vf_(k->r[a] * (perturb ? 1.5f : 1.0f)); }
         c.basis = vf_((float)k->basis); c.count = vf_((float)k->count); c.flat = k->type; c.caps = vf_((float)k->caps);
         r.o = v3_(vf_(rs[i].o[0]), vf_(rs[i].o[1]), vf_(rs[i].o[2])); r.d = v3_(vf_(rs[i].d[0]), vf_(rs[i].d[1]), vf_(rs[i].d[2]));
         r.tmin = vf_(rs[i].tmin); r.tmax = vf_(rs[i].tmax);
         struct cv_hit h = cv_intersect(&cv_float_ops, &c, &r);
         printf("ray %d hit %u t %08x u %08x prim %u\n", i, (unsigned)h.hit, h.hit ? (unsigned)h.t : 0u, h.hit ? (unsigned)h.u : 0u, 0u);
      }
   }
   return 0;
}
