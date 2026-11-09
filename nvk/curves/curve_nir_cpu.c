/* CPU checks of the NIR form of curve_math.h; nothing touches a GPU.
 *   1. fold: the intersection built with constant inputs is folded by NIR's own constant folder and compared with the float
 *      form on random curves and rays (this executes the NIR graph).
 *   2. compile: the same intersection with inputs loaded from memory goes through NAK; prints the code size.
 * usage: curve_nir_cpu <out-prefix> */
#include "nak.h"
#include "nv_device_info.h"
#include "compiler/nir/nir_builder.h"
#include "compiler/glsl_types.h"
#include "util/ralloc.h"
#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include "curve_nir.h"
#include "curve_float.h"

const struct nak_constant_offset_info nak_const_offsets_base = {0};
const struct nak_constant_offset_info nak_const_offsets_turing_graphics = {0};

static uint32_t st = 11;
static float fr(float a, float b) { st = st * 1664525u + 1013904223u; return a + (b - a) * ((st >> 8 & 0xffff) / 65535.0f); }

static int folded(nir_def *d, float *out)
{
   if (!d || nir_def_instr(d)->type != nir_instr_type_load_const) return 0;
   nir_const_value *v = nir_instr_as_load_const(nir_def_instr(d))->value;
   if (d->bit_size == 1) *out = v[0].b ? 1.0f : 0.0f; else *out = v[0].f32;
   return 1;
}

int main(int argc, char **argv)
{
   if (argc != 2) return 2;
   glsl_type_singleton_init_or_ref();
   struct nv_device_info dev = {0};
   dev.sm = 120; dev.max_warps_per_mp = 48;
   struct nak_compiler *nak = nak_compiler_create(&dev);

   int bad = 0, total = 0, unfolded = 0, hits = 0;
   for (int n = 0; n < 160; n++) {
      nir_builder b = nir_builder_init_simple_shader(MESA_SHADER_COMPUTE, nak_nir_options(nak), "curve_fold");
      b.constant_fold_alu = true;
      struct cv_ops on = cv_nir_ops(&b);
      float pv[4][3], rv[4], o[3], d[3];
      int basis = n % 4, count = basis == 2 ? 2 : (basis == 1 ? 4 : 3 + (n / 4) % 2), flat = (n / 8) % 4 == 0, caps = n % 3;
      for (int i = 0; i < 4; i++) { for (int k = 0; k < 3; k++) pv[i][k] = fr(-1.5f, 1.5f); rv[i] = fr(0.08f, 0.35f); }
      float tgt[3] = { fr(-1, 1), fr(-1, 1), fr(-1, 1) };
      for (int k = 0; k < 3; k++) { o[k] = fr(-4, 4); d[k] = tgt[k] - o[k]; }
      o[2] = -4; d[2] = tgt[2] + 4;
      struct cv_curve cn, cf; struct cv_ray rn, rf;
      for (int i = 0; i < 4; i++) {
         cn.p[i] = v3_(R_(nir_imm_float(&b, pv[i][0])), R_(nir_imm_float(&b, pv[i][1])), R_(nir_imm_float(&b, pv[i][2])));
         cf.p[i] = v3_(vf_(pv[i][0]), vf_(pv[i][1]), vf_(pv[i][2]));
         cn.r[i] = R_(nir_imm_float(&b, rv[i])); cf.r[i] = vf_(rv[i]);
      }
      cn.basis = R_(nir_imm_float(&b, basis)); cf.basis = vf_(basis);
      cn.count = R_(nir_imm_float(&b, count)); cf.count = vf_(count);
      cn.flat = R_(nir_imm_bool(&b, flat)); cf.flat = flat;
      cn.caps = R_(nir_imm_float(&b, caps)); cf.caps = vf_(caps);
      rn.o = v3_(R_(nir_imm_float(&b, o[0])), R_(nir_imm_float(&b, o[1])), R_(nir_imm_float(&b, o[2])));
      rn.d = v3_(R_(nir_imm_float(&b, d[0])), R_(nir_imm_float(&b, d[1])), R_(nir_imm_float(&b, d[2])));
      rn.tmin = R_(nir_imm_float(&b, 0)); rn.tmax = R_(nir_imm_float(&b, 100));
      rf.o = v3_(vf_(o[0]), vf_(o[1]), vf_(o[2])); rf.d = v3_(vf_(d[0]), vf_(d[1]), vf_(d[2])); rf.tmin = vf_(0); rf.tmax = vf_(100);
      struct cv_hit hn = cv_intersect(&on, &cn, &rn), hf = cv_intersect(&cv_float_ops, &cf, &rf);
      for (int pass = 0; pass < 8; pass++) { bool p = false; NIR_PASS(p, b.shader, nir_opt_constant_folding); NIR_PASS(p, b.shader, nir_opt_algebraic); NIR_PASS(p, b.shader, nir_opt_dce); if (!p) break; }
      float ghit, gt, gu;
      total++;
      if (!folded(D_(hn.hit), &ghit) || !folded(D_(hn.t), &gt) || !folded(D_(hn.u), &gu)) { unfolded++; ralloc_free(b.shader); continue; }
      int wh = (int)hf.hit; hits += wh;
      float wt = fv_(hf.t), wu = fv_(hf.u);
      int same = (int)ghit == wh && (!wh || (fabsf(gt - wt) <= 2e-4f * fmaxf(1, fabsf(wt)) && fabsf(gu - wu) <= 2e-4f));
      if (!same) { bad++; if (bad <= 3) printf("  case %d basis %d count %d flat %d caps %d: nir hit %g t %g u %g, float hit %d t %g u %g\n", n, basis, count, flat, caps, ghit, gt, gu, wh, wt, wu); }
      ralloc_free(b.shader);
   }
   printf("CURVE_FOLD cases=%d unfolded=%d differ=%d float_hits=%d\n", total, unfolded, bad, hits);

   /* compile with runtime inputs */
   nir_builder b = nir_builder_init_simple_shader(MESA_SHADER_COMPUTE, nak_nir_options(nak), "curve_rt");
   b.shader->info.workgroup_size[0] = 32; b.shader->info.workgroup_size[1] = b.shader->info.workgroup_size[2] = 1;
   struct cv_ops on = cv_nir_ops(&b);
   nir_def *base = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x380), .align_mul = 8);
   nir_def *out = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x388), .align_mul = 8);
   nir_def *tid = nir_channel(&b, nir_load_local_invocation_id(&b), 0);
   nir_def *rec = nir_iadd(&b, base, nir_u2u64(&b, nir_imul_imm(&b, tid, 128)));
   nir_def *w[32];
   for (int i = 0; i < 8; i++) { nir_def *v = nir_build_load_global(&b, 4, 32, nir_iadd_imm(&b, rec, i * 16), .align_mul = 16); for (int k = 0; k < 4; k++) w[i * 4 + k] = nir_channel(&b, v, k); }
   struct cv_curve c; struct cv_ray r;
   for (int i = 0; i < 4; i++) { c.p[i] = v3_(R_(w[i * 3]), R_(w[i * 3 + 1]), R_(w[i * 3 + 2])); c.r[i] = R_(w[12 + i]); }
   c.basis = R_(nir_u2f32(&b, nir_iand_imm(&b, w[16], 3)));
   c.count = R_(nir_u2f32(&b, nir_iand_imm(&b, w[17], 7)));
   c.flat = R_(nir_ine_imm(&b, w[18], 0));
   c.caps = R_(nir_u2f32(&b, nir_iand_imm(&b, w[19], 3)));
   r.o = v3_(R_(w[20]), R_(w[21]), R_(w[22])); r.d = v3_(R_(w[24]), R_(w[25]), R_(w[26])); r.tmin = R_(w[23]); r.tmax = R_(w[27]);
   struct cv_hit h = cv_intersect(&on, &c, &r);
   nir_def *op = nir_iadd(&b, out, nir_u2u64(&b, nir_imul_imm(&b, tid, 16)));
   nir_build_store_global(&b, nir_vec4(&b, D_(h.t), D_(h.u), nir_b2i32(&b, D_(h.hit)), nir_imm_int(&b, 0)), op, .write_mask = 0xf, .align_mul = 16);
   nir_validate_shader(b.shader, "curve before compile");
   NIR_PASS(_, b.shader, nir_lower_returns);
   NIR_PASS(_, b.shader, nir_opt_dce);
   nir_shader_gather_info(b.shader, nir_shader_get_entrypoint(b.shader));
   nak_preprocess_nir(b.shader, nak);
   struct nak_shader_bin *bin = nak_compile_shader(b.shader, true, nak, 0, NULL, false);
   if (!bin) { printf("CURVE_COMPILE failed\n"); return 3; }
   printf("CURVE_COMPILE ok code_bytes=%u gprs=%u instrs=%u\n", (unsigned)bin->code_size, (unsigned)bin->info.num_gprs, (unsigned)(bin->code_size / 16));
   return bad != 0 || unfolded > total / 10;
}
