#!/usr/bin/env python3
"""patch_u2fmagic2.py <nak_nir.c> - on top of patch_u2fmagic.py: a byte that comes straight from unpack_32_4x8 builds
0x4B0000xx with ONE prmt (byte c of the word, then bytes 1..3 of 0x4B000000: selector 0x765c) instead of
extract + or. Exact anchors, asserted counts, atomic write."""
import os, pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
A = """   b->cursor = nir_before_instr(instr);
   nir_def *x = nir_u2u32(b, nir_ssa_for_alu_src(b, alu, 0));
   nir_def *f = nir_fadd_imm(b, nir_ior_imm(b, x, 0x4B000000), -8388608.0);
   nir_def_replace(&alu->def, f);
   return true;"""
assert s.count(A) == 1, "anchor A"
s = s.replace(A, """   b->cursor = nir_before_instr(instr);
   nir_alu_instr *up = nir_src_as_alu_instr(alu->src[0].src);
   nir_def *f;
   if (bs == 8 && up && up->op == nir_op_unpack_32_4x8) {
      /* one PRMT per byte: result bytes = { word.byte[c], 0x00, 0x00, 0x4B } */
      nir_def *word = nir_ssa_for_alu_src(b, up, 0);
      nir_def *comps[NIR_MAX_VEC_COMPONENTS];
      for (unsigned i = 0; i < alu->def.num_components; i++) {
         nir_def *bits = nir_prmt_nv(b, nir_imm_int(b, 0x7650 | alu->src[0].swizzle[i]), word,
                                     nir_imm_int(b, 0x4B000000));
         comps[i] = nir_fadd_imm(b, bits, -8388608.0);
      }
      f = nir_vec(b, comps, alu->def.num_components);
   } else {
      nir_def *x = nir_u2u32(b, nir_ssa_for_alu_src(b, alu, 0));
      f = nir_fadd_imm(b, nir_ior_imm(b, x, 0x4B000000), -8388608.0);
   }
   nir_def_replace(&alu->def, f);
   return true;""")
t = p.with_name(p.name + ".tmp"); t.write_text(s); os.replace(t, p); print("patched", p)
