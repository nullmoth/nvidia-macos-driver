#!/usr/bin/env python3
# patch_predload_max.py <nak_nir.c>: the side-size cap of nak_cg_predload becomes NVMTL_NAK_PREDLOAD_MAX (default 20).
import os, sys
p = sys.argv[1]; s = open(p).read()
E = [("""         /* 12 instructions / 2 loads: a guard around a few taps, not a slow path */
         if (n < 0 || n > 12 || loads > 2)
            continue;""", """         /* a guard around a few taps, not a slow path: <= NVMTL_NAK_PREDLOAD_MAX instructions (census default 20;
          * particle's j != i side is 17) and <= 2 loads */
         if (n < 0 || n > (int)nak_cg_predload_max() || loads > 2)
            continue;"""),
("""static void
nak_cg_predload_hoist(""", """static unsigned
nak_cg_predload_max(void)
{
   static int max = -1;
   if (max < 0) {
      const char *e = getenv("NVMTL_NAK_PREDLOAD_MAX");
      max = e && *e ? atoi(e) : 20;
      if (max < 0)
         max = 0;
   }
   return max;
}

static void
nak_cg_predload_hoist(""")]
for a, b in E:
    assert s.count(a) == 1, a[:60]
    s = s.replace(a, b)
t = p + ".tmp"; open(t, "w").write(s); os.replace(t, p); print("patched", p)
