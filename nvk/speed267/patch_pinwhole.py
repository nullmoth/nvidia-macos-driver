#!/usr/bin/env python3
# patch_pinwhole.py <nak_nir_lower_non_uniform_ldcx.c>: pin_uniform_addrs_section also pins a WHOLE-uniform 64-bit
# address (no per-thread part), whether it already sits in the uniform slot or still in the offset slot.
import os, shutil, sys
p = sys.argv[1]
s = open(p).read()
A1 = """/* Returns the uniform 64-bit operand of the access's iadd address, or NULL. */
static nir_def *
pin_addr_uniform_base(nir_intrinsic_instr *intr, nir_block *pred,
                      unsigned *div_idx)
{
   nir_src *offset_src = pin_addr_offset_src(intr);
   if (offset_src == NULL || offset_src->ssa->bit_size != 64)
      return NULL;

   nir_src *uniform_src = nir_get_io_uniform_offset_src(intr);
   if (uniform_src == NULL || !nir_src_is_const(*uniform_src) ||
       nir_src_as_uint(*uniform_src) != 0)
      return NULL;
"""
B1 = """/* div_idx values for an address with no per-thread part. */
#define PIN_ADDR_KEEP 2u /* base already in the uniform slot, offset is 0 */
#define PIN_ADDR_SWAP 3u /* base in the offset slot, uniform slot is 0 */

static bool
pin_addr_whole_enabled(void)
{
   static int enabled = -1;
   if (enabled < 0) {
      const char *e = getenv("NVMTL_NAK_NO_PIN_WHOLE");
      enabled = !(e != NULL && e[0] == '1');
   }
   return enabled;
}

static nir_def *
pin_addr_check_base(nir_def *base, nir_block *pred)
{
   if (base->bit_size != 64 || base->num_components != 1 || base->divergent)
      return NULL;
   if (!can_hoist_def(base, pred))
      return NULL;
   return base;
}

/* Returns the uniform 64-bit operand of the access's address, or NULL.
 *
 * Besides iadd(per-thread, uniform), an address can be uniform as a whole:
 * a kernel-argument pointer read at a fixed offset. GB7 raw_process does 50
 * such loads inside its bounds check, and unpinned, legalize copied the
 * base into a fresh GPR pair before every one (84 of its 664 instructions
 * were those movs). NVMTL_NAK_NO_PIN_WHOLE=1 turns this case off. */
static nir_def *
pin_addr_uniform_base(nir_intrinsic_instr *intr, nir_block *pred,
                      unsigned *div_idx)
{
   nir_src *offset_src = pin_addr_offset_src(intr);
   if (offset_src == NULL)
      return NULL;

   nir_src *uniform_src = nir_get_io_uniform_offset_src(intr);
   if (uniform_src == NULL)
      return NULL;

   const bool uslot_zero = nir_src_is_const(*uniform_src) &&
                           nir_src_as_uint(*uniform_src) == 0;
   if (!uslot_zero) {
      if (!pin_addr_whole_enabled() || !nir_src_is_const(*offset_src) ||
          nir_src_as_uint(*offset_src) != 0)
         return NULL;
      *div_idx = PIN_ADDR_KEEP;
      return pin_addr_check_base(uniform_src->ssa, pred);
   }

   if (offset_src->ssa->bit_size != 64)
      return NULL;

   if (pin_addr_whole_enabled() && !nir_src_is_const(*offset_src) &&
       !nir_def_is_divergent_at_use_block(offset_src->ssa, intr->instr.block)) {
      *div_idx = PIN_ADDR_SWAP;
      return pin_addr_check_base(offset_src->ssa, pred);
   }
"""
A2 = """         nir_alu_instr *iadd = nir_def_as_alu(pin_addr_offset_src(intr)->ssa);
         b->cursor = nir_before_instr(&intr->instr);"""
B2 = """         if (div_idx == PIN_ADDR_KEEP)
            continue; /* already reads the pinned base */
         if (div_idx == PIN_ADDR_SWAP) {
            b->cursor = nir_before_instr(&intr->instr);
            nir_src_rewrite(pin_addr_offset_src(intr), nir_imm_int64(b, 0));
            nir_src_rewrite(nir_get_io_uniform_offset_src(intr), base);
            continue;
         }

         nir_alu_instr *iadd = nir_def_as_alu(pin_addr_offset_src(intr)->ssa);
         b->cursor = nir_before_instr(&intr->instr);"""
for a, b in ((A1, B1), (A2, B2)):
    n = s.count(a)
    assert n == 1, (n, a[:70])
    s = s.replace(a, b)
bak = p + ".pre-pinwhole"
if not os.path.exists(bak):
    shutil.copy2(p, bak)
tmp = p + ".tmp"
open(tmp, "w").write(s)
os.replace(tmp, p)
print("patched", p)
