#!/usr/bin/env python3
# patch_pinwhole3.py <nak_nir_lower_non_uniform_ldcx.c>: the whole-only minimum use count becomes
# NVMTL_NAK_PIN_WHOLE_MIN (default set below after the census).
import os, shutil, sys
p = sys.argv[1]
s = open(p).read()
A1 = """      if (split == 0 && whole < 2)
         continue;"""
B1 = """      if (split == 0 && whole < pin_addr_whole_min())
         continue;"""
A2 = """static nir_def *
pin_addr_check_base(nir_def *base, nir_block *pred)"""
B2 = """/* Minimum uses before a whole-only base is pinned (census-chosen default). */
static unsigned
pin_addr_whole_min(void)
{
   static int min = -1;
   if (min < 0) {
      const char *e = getenv("NVMTL_NAK_PIN_WHOLE_MIN");
      min = e && *e ? atoi(e) : 2;
      if (min < 1)
         min = 1;
   }
   return min;
}

static nir_def *
pin_addr_check_base(nir_def *base, nir_block *pred)"""
for a, b in ((A1, B1), (A2, B2)):
    n = s.count(a)
    assert n == 1, (n, a[:70])
    s = s.replace(a, b)
bak = p + ".pre-pinwhole3"
if not os.path.exists(bak):
    shutil.copy2(p, bak)
tmp = p + ".tmp"
open(tmp, "w").write(s)
os.replace(tmp, p)
print("patched", p)
