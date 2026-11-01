#!/usr/bin/env python3
# patch_predload_def32.py <nak_nir.c>: NVMTL_NAK_PREDLOAD_MAX default 20 -> 32 (particle's j != i side needs > 20).
import os, sys
p = sys.argv[1]; s = open(p).read()
E = [("""      max = e && *e ? atoi(e) : 20;""", """      max = e && *e ? atoi(e) : 32;"""),
     ("""         /* a guard around a few taps, not a slow path: <= NVMTL_NAK_PREDLOAD_MAX instructions (census default 20;
          * particle's j != i side is 17) and <= 2 loads */""",
      """         /* a guard around a few taps, not a slow path: <= NVMTL_NAK_PREDLOAD_MAX instructions (default 32: particle's
          * j != i side needs more than 20; the census at 12/20/32 differs only by the unrolls 32 enables) and <= 2 loads */""")]
for a, b in E:
    assert s.count(a) == 1, a[:50]
    s = s.replace(a, b)
t = p + ".tmp"; open(t, "w").write(s); os.replace(t, p); print("patched", p)
