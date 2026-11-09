#!/usr/bin/env python3
# patch_predload_fwd.py <nak_nir.c>: forward-declare nak_cg_predload_enabled above the punroll counter that reads it.
import os, sys
p = sys.argv[1]; s = open(p).read()
a = """static nir_phi_instr *
nak_cg_punroll_counter(nir_block *a, nir_block *z, nir_def *def, unsigned *d, unsigned *step)"""
b = """static bool nak_cg_predload_enabled(void);

static nir_phi_instr *
nak_cg_punroll_counter(nir_block *a, nir_block *z, nir_def *def, unsigned *d, unsigned *step)"""
assert s.count(a) == 1
s = s.replace(a, b); t = p + ".tmp"; open(t, "w").write(s); os.replace(t, p); print("patched", p)
