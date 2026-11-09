#!/usr/bin/env python3
"""patch_u2fmagic.py <nak_nir.c> - u2f32 of an 8- or 16-bit value becomes fadd(as_float(0x4B000000 | x), -2^23):
two full-rate ALU ops instead of an I2F, which runs at a fraction of FFMA rate. GB7's convolve_vertical converts
four bytes per tap (4 i2f.f32.u8 beside 4 ffma). Exact for every x < 2^23, any rounding mode or denormal setting.
NVMTL_NAK_NO_U2FMAGIC=1 is the kill switch. Exact anchors, asserted counts, atomic write."""
import os, pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()
assert "nak_cg_u2f_magic" not in s, "already patched"
A = """void
nak_optimize_nir(nir_shader *nir, const struct nak_compiler *nak)
{"""
assert s.count(A) == 1, "anchor A"
s = s.replace(A, """/* NVMTL speed267 (patch_u2fmagic.py): u2f32 of an 8- or 16-bit value as fadd(as_float(0x4B000000 | x), -2^23).
 * 0x4B000000 is 2^23, whose mantissa holds any integer below 2^23 exactly, so the subtraction is exact; it replaces an
 * I2F (a fraction of FFMA rate) with two full-rate ALU ops. Image kernels convert every byte of every pixel this way.
 * NVMTL_NAK_NO_U2FMAGIC=1 is the kill switch, read once per process. */
static bool
nak_cg_u2f_magic_instr(nir_builder *b, nir_instr *instr, UNUSED void *data)
{
   if (instr->type != nir_instr_type_alu)
      return false;
   nir_alu_instr *alu = nir_instr_as_alu(instr);
   if (alu->op != nir_op_u2f32)
      return false;
   const unsigned bs = nir_src_bit_size(alu->src[0].src);
   if (bs != 8 && bs != 16)
      return false;
   b->cursor = nir_before_instr(instr);
   nir_def *x = nir_u2u32(b, nir_ssa_for_alu_src(b, alu, 0));
   nir_def *f = nir_fadd_imm(b, nir_ior_imm(b, x, 0x4B000000), -8388608.0);
   nir_def_replace(&alu->def, f);
   return true;
}

static bool
nak_cg_u2f_magic(nir_shader *nir)
{
   static int enabled = -1;
   if (enabled < 0) {
      const char *e = getenv("NVMTL_NAK_NO_U2FMAGIC");
      enabled = !(e != NULL && e[0] == '1');
   }
   if (!enabled)
      return false;
   return nir_shader_instructions_pass(nir, nak_cg_u2f_magic_instr, nir_metadata_control_flow, NULL);
}

""" + A)
B = """   OPT(nir, nir_opt_fp_math_ctrl);
   nak_optimize_nir(nir, nak);
"""
assert s.count(B) == 1, "anchor B"
s = s.replace(B, """   OPT(nir, nir_opt_fp_math_ctrl);
   OPT(nir, nak_cg_u2f_magic);
   nak_optimize_nir(nir, nak);
""")
t = p.with_name(p.name + ".tmp"); t.write_text(s); os.replace(t, p); print("patched", p)
