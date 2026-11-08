/* CPU check of the ray-query lowering: builds a compute shader with one ray query (initialize, a proceed loop, committed
 * distance and instance id), runs nvk_nir_lower_ray_queries and then NAK, and writes the machine code. Nothing touches a
 * GPU. With NVK_HWRT_QUERY=1 the code must contain exactly one hardware query region; with it unset it must contain none.
 * usage: ttu_rq_cpu <out-prefix> */
#include "nak.h"
#include "nv_device_info.h"
#include "compiler/nir/nir_builder.h"
#include "compiler/glsl_types.h"
#include "util/ralloc.h"
#include <stdio.h>
#include <stdlib.h>

extern bool nvk_nir_lower_ray_queries(struct nir_shader *shader);

const struct nak_constant_offset_info nak_const_offsets_base = {0};
const struct nak_constant_offset_info nak_const_offsets_turing_graphics = {0};

int main(int argc, char **argv)
{
   if (argc != 2)
      return 2;
   glsl_type_singleton_init_or_ref();
   struct nv_device_info dev = {0};
   dev.sm = 120;
   dev.max_warps_per_mp = 48;
   struct nak_compiler *nak = nak_compiler_create(&dev);
   nir_builder b = nir_builder_init_simple_shader(MESA_SHADER_COMPUTE, nak_nir_options(nak), "rq_cpu");
   b.shader->info.workgroup_size[0] = 32;
   b.shader->info.workgroup_size[1] = b.shader->info.workgroup_size[2] = 1;

   nir_def *accel = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x380), .align_mul = 8);
   nir_def *out = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x388), .align_mul = 8);
   nir_def *tid = nir_channel(&b, nir_load_local_invocation_id(&b), 0);
   nir_def *rays = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x390), .align_mul = 8);
   nir_def *rp = nir_iadd(&b, rays, nir_u2u64(&b, nir_imul_imm(&b, tid, 32)));
   nir_def *o = nir_build_load_global(&b, 4, 32, rp, .align_mul = 16);
   nir_def *d = nir_build_load_global(&b, 4, 32, nir_iadd_imm(&b, rp, 16), .align_mul = 16);

   nir_variable *rq = nir_variable_create(b.shader, nir_var_shader_temp, glsl_uint64_t_type()  /* the type SPIR-V translation gives a ray query */, "rq");
   rq->data.ray_query = true;
   nir_deref_instr *q = nir_build_deref_var(&b, rq);
   nir_rq_initialize(&b, &q->def, accel, nir_imm_int(&b, 0), nir_imm_int(&b, 0xff), nir_trim_vector(&b, o, 3),
                     nir_channel(&b, o, 3), nir_trim_vector(&b, d, 3), nir_channel(&b, d, 3));
   if (getenv("RQ_NO_LOOP")) {
      (void)nir_rq_proceed(&b, 1, &q->def);
   } else {
      nir_push_loop(&b);
      {
         nir_def *more = nir_rq_proceed(&b, 1, &q->def);
         nir_break_if(&b, nir_inot(&b, more));
      }
      nir_pop_loop(&b, NULL);
   }
   nir_def *t = nir_rq_load(&b, 1, 32, &q->def, .ray_query_value = nir_ray_query_value_intersection_t, .committed = true);
   nir_def *inst = nir_rq_load(&b, 1, 32, &q->def, .ray_query_value = nir_ray_query_value_intersection_instance_id,
                               .committed = true);
   nir_def *op = nir_iadd(&b, out, nir_u2u64(&b, nir_imul_imm(&b, tid, 8)));
   nir_build_store_global(&b, nir_vec2(&b, t, inst), nir_iadd_imm(&b, op, 0), .write_mask = 0x3, .align_mul = 8);

   nir_validate_shader(b.shader, "ray query before the lowering");
   if (!nvk_nir_lower_ray_queries(b.shader)) {
      fprintf(stderr, "the lowering reported no progress\n");
      return 5;
   }
   nir_validate_shader(b.shader, "ray query after the lowering");
   NIR_PASS(_, b.shader, nir_lower_returns);
   NIR_PASS(_, b.shader, nir_lower_global_vars_to_local);
   NIR_PASS(_, b.shader, nir_lower_vars_to_ssa);
   NIR_PASS(_, b.shader, nir_opt_dce);
   nir_shader_gather_info(b.shader, nir_shader_get_entrypoint(b.shader));
   nak_preprocess_nir(b.shader, nak);
   /* nak_compile_shader runs the post-processing (and the control-flow lowering) itself; doing it here too lowers twice */
   struct nak_shader_bin *bin = nak_compile_shader(b.shader, true, nak, 0, NULL, false);
   if (!bin)
      return 3;

   char path[4096];
   snprintf(path, sizeof path, "%s.sass.bin", argv[1]);
   FILE *f = fopen(path, "wb");
   if (!f)
      return 4;
   fwrite(bin->code, 1, bin->code_size, f);
   fclose(f);
   printf("compiled %u bytes of code, %u registers\n", (unsigned)bin->code_size, (unsigned)bin->info.num_gprs);
   return 0;
}
