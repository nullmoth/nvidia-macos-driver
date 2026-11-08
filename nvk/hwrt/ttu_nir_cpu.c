/* CPU check of the SM120 hardware query through the shared compiler: builds a compute
 * shader that loads five 16-byte TTU input blocks per lane, issues one ttu_query_nv and
 * stores the 16 result words, runs it through NAK, and writes the SASS + NAK listing.
 * Nothing touches a GPU. usage: ttu_nir_cpu <out-prefix> */
#include "nak.h"
#include "nv_device_info.h"
#include "compiler/nir/nir_builder.h"
#include "compiler/glsl_types.h"
#include "util/ralloc.h"
#include <stdio.h>

/* The driver normally provides these (nvk_shader.c); a compute shader with no printf never reads them. */
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
   nir_builder b = nir_builder_init_simple_shader(MESA_SHADER_COMPUTE, nak_nir_options(nak), "ttu_query_cpu");
   b.shader->info.workgroup_size[0] = 32;
   b.shader->info.workgroup_size[1] = b.shader->info.workgroup_size[2] = 1;

   /* Same buffer ABI as the GPU fixture: cb0[0x380] = 80 B/lane input, cb0[0x388] = 96 B/lane output. */
   nir_def *in = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x380), .align_mul = 8);
   nir_def *out = nir_ldc_nv(&b, 1, 64, nir_imm_int(&b, 0), nir_imm_int(&b, 0x388), .align_mul = 8);
   nir_def *tid = nir_channel(&b, nir_load_local_invocation_id(&b), 0);
   nir_def *ip = nir_iadd(&b, in, nir_u2u64(&b, nir_imul_imm(&b, tid, 80)));
   nir_def *op = nir_iadd(&b, out, nir_u2u64(&b, nir_imul_imm(&b, tid, 96)));
   nir_def *v[5];
   for (unsigned i = 0; i < 5; i++)
      v[i] = nir_build_load_global(&b, 4, 32, nir_iadd_imm(&b, ip, i * 16), .align_mul = 16);
   nir_def *r = nir_ttu_query_nv(&b, v[0], v[1], v[2], v[3], v[4], .flags = 1);
   /* Words 8..23 of the 96-byte record, so the GPU fixture's decoder reads the same slots. */
   for (unsigned i = 0; i < 4; i++)
      nir_build_store_global(&b, nir_channels(&b, r, 0xfu << (4 * i)), nir_iadd_imm(&b, op, 32 + i * 16),
                             .write_mask = 0xf, .align_mul = 16);

   nir_validate_shader(b.shader, "ttu query before compiler");
   nak_preprocess_nir(b.shader, nak);
   nak_postprocess_nir(b.shader, nak, 0, NULL, false);
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
   snprintf(path, sizeof path, "%s.nak.txt", argv[1]);
   f = fopen(path, "w");
   if (!f)
      return 5;
   fputs(bin->asm_str ? bin->asm_str : "", f);
   fclose(f);
   printf("TTU_CPU_COMPILED code_bytes=%u gprs=%u slm=%u\n", bin->code_size, bin->info.num_gprs, bin->info.slm_size);
   nak_shader_bin_destroy(bin);
   ralloc_free(b.shader);
   nak_compiler_destroy(nak);
   glsl_type_singleton_decref();
   return 0;
}
