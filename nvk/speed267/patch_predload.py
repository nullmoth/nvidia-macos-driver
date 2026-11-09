#!/usr/bin/env python3
# patch_predload.py <nak_nir.c>: (1) nak_cg_predload - a small divergent if whose one side is ALU + plain global loads
# is flattened by moving that side before the if, each load becoming load_global_nv predicated on the branch
# condition (no access, returns 0 when false); peephole_select then turns the phis into selects.
# (2) nak_cg_punroll accepts a counter that steps by any constant 2..4096, not only 1.
# (3) after the vectorizer: predload, then punroll again on the loops it straightened.
import os, shutil, sys
p = sys.argv[1]
s = open(p).read()
E = []
E.append(("""   nir_op exit_op;           /* nir_op_ieq, nir_op_ige or nir_op_uge: the loop exits when exit_op(t, n) */
   unsigned f;
};""", """   nir_op exit_op;           /* nir_op_ieq, nir_op_ige or nir_op_uge: the loop exits when exit_op(t, n) */
   unsigned f;
   unsigned step;            /* the counter's constant increment (speed267 predload: was always 1) */
};"""))
E.append(("""   case nir_intrinsic_load_ubo:
   case nir_intrinsic_load_ssbo:
      return true;
   default:
      return false;
   }
}""", """   case nir_intrinsic_load_ubo:
   case nir_intrinsic_load_ssbo:
   case nir_intrinsic_load_global_nv:   /* speed267: punroll also runs after nak_cg_predload */
      return true;
   default:
      return false;
   }
}"""))
E.append(("""nak_cg_punroll_counter(nir_block *a, nir_block *z, nir_def *def, unsigned *d)
{""", """nak_cg_punroll_counter(nir_block *a, nir_block *z, nir_def *def, unsigned *d, unsigned *step)
{"""))
E.append(("""      bool ok = false;
      for (unsigned s = 0; s < 2; s++) {
         if (add->src[s].src.ssa == &phi->def && add->src[s].swizzle[0] == 0 &&
             nir_src_is_const(add->src[1 - s].src) &&
             nir_alu_src_as_uint(add->src[1 - s]) == 1)
            ok = true;
      }
      if (!ok)
         continue;
      if (def == &phi->def) {
         *d = 0;
         return phi;
      }
      if (def == &add->def) {
         *d = 1;
         return phi;
      }""", """      bool ok = false;
      uint64_t st = 0;
      for (unsigned s = 0; s < 2; s++) {
         if (add->src[s].src.ssa == &phi->def && add->src[s].swizzle[0] == 0 &&
             nir_src_is_const(add->src[1 - s].src)) {
            st = nir_alu_src_as_uint(add->src[1 - s]);
            /* speed267: a grid-stride loop (i += threads per group) is as countable as i++; particle steps 64 */
            if (st == 1 || (nak_cg_predload_enabled() && st >= 2 && st <= 4096))
               ok = true;
         }
      }
      if (!ok)
         continue;
      if (def == &phi->def) {
         *d = 0;
         *step = st;
         return phi;
      }
      if (def == &add->def) {
         *d = 1;
         *step = st;
         return phi;
      }"""))
E.append(("""   unsigned d = 0;
   nir_phi_instr *iv = NULL;""", """   unsigned d = 0, step = 1;
   nir_phi_instr *iv = NULL;"""))
E.append(("""      iv = nak_cg_punroll_counter(a, z, cmp->src[s].src.ssa, &d);""",
          """      iv = nak_cg_punroll_counter(a, z, cmp->src[s].src.ssa, &d, &step);"""))
E.append(("""   if (iv == NULL || n->bit_size != 32 || n->num_components != 1)
      return false;""", """   if (iv == NULL || n->bit_size != 32 || n->num_components != 1)
      return false;
   if (step != 1 && op == nir_op_ieq)
      return false;   /* t == n can be stepped over by a stride > 1 */"""))
E.append(("""   *p = (struct nak_cg_punroll_plan){ loop, a, z, iv, d, n, op, f };""",
          """   *p = (struct nak_cg_punroll_plan){ loop, a, z, iv, d, n, op, f, step };"""))
E.append(("""   nir_def *t0 = p->d ? nir_iadd_imm(&b, &mphi[ivk]->def, 1) : &mphi[ivk]->def;
   nir_def *g = nir_uge(&b, nir_isub(&b, p->n, t0), nir_imm_int(&b, p->f));""",
          """   /* a chunk of f bodies at iv, iv+step, ... needs iv + (f-1)*step < n; (n - t0) >= f*step is that, conservatively */
   nir_def *t0 = p->d ? nir_iadd_imm(&b, &mphi[ivk]->def, p->step) : &mphi[ivk]->def;
   nir_def *g = nir_uge(&b, nir_isub(&b, p->n, t0), nir_imm_int(&b, p->f * p->step));"""))
E.append(("""static bool
nak_cg_punroll(nir_shader *nir)
{""", """/* speed267 (patch_predload.py): NVMTL_NAK_NO_PREDLOAD=1 turns off nak_cg_predload and the stride-n punroll. */
static bool
nak_cg_predload_enabled(void)
{
   static int enabled = -1;
   if (enabled < 0) {
      const char *e = getenv("NVMTL_NAK_NO_PREDLOAD");
      enabled = !(e != NULL && e[0] == '1');
   }
   return enabled;
}

/* Instructions of one if side that nak_cg_predload may move above the if: pure ALU, constants, and global loads
 * that are neither volatile nor coherent (those become predicated). -1 for anything else. */
static int
nak_cg_predload_side(nir_block *blk, unsigned *loads)
{
   int n = 0;
   nir_foreach_instr(instr, blk) {
      switch (instr->type) {
      case nir_instr_type_alu:
         n++;
         break;
      case nir_instr_type_load_const:
      case nir_instr_type_undef:
         break;
      case nir_instr_type_intrinsic: {
         nir_intrinsic_instr *intr = nir_instr_as_intrinsic(instr);
         if (intr->intrinsic != nir_intrinsic_load_global &&
             intr->intrinsic != nir_intrinsic_load_global_constant)
            return -1;
         if (nir_intrinsic_access(intr) & (ACCESS_VOLATILE | ACCESS_COHERENT))
            return -1;
         (*loads)++;
         n++;
         break;
      }
      default:
         return -1;
      }
   }
   return n;
}

static void
nak_cg_predload_hoist(nir_shader *nir, nir_block *side, nir_block *dest, nir_def *pred)
{
   nir_builder b = nir_builder_create(nir_cf_node_get_function(&dest->cf_node));
   nir_foreach_instr_safe(instr, side) {
      if (instr->type != nir_instr_type_intrinsic) {
         nir_instr_move(nir_after_block(dest), instr);
         continue;
      }
      nir_intrinsic_instr *intr = nir_instr_as_intrinsic(instr);
      b.cursor = nir_after_block(dest);
      nir_def *addr = intr->src[0].ssa;
      nir_def *res = nir_load_global_nv(&b, intr->def.num_components, intr->def.bit_size, addr,
                                        nir_imm_zero(&b, 1, addr->bit_size), pred);
      nir_intrinsic_instr *nv = nir_def_as_intrinsic(res);
      enum gl_access_qualifier access = nir_intrinsic_access(intr);
      if (intr->intrinsic == nir_intrinsic_load_global_constant)
         access |= ACCESS_CAN_REORDER;
      nir_intrinsic_set_access(nv, access);
      nir_intrinsic_set_align_mul(nv, nir_intrinsic_align_mul(intr));
      nir_intrinsic_set_align_offset(nv, nir_intrinsic_align_offset(intr));
      if (nir_intrinsic_has_base(intr))
         nir_intrinsic_set_base(nv, nir_intrinsic_base(intr));
      nir_def_rewrite_uses(&intr->def, res);
      nir_instr_remove(instr);
   }
}

/* speed267: a divergent if costs bclear + bssy + bsync + a branch, every trip of a loop, and it keeps the loop
 * out of nak_cg_punroll (which only takes straight-line bodies). GB7 particle's inner loop is
 * `if (j != i) { load p[j]; if (0 < d2 < r2) { f += m*rsq(d2)*d; } }` for 64 trips per thread: one load in flight,
 * two branch regions per trip. NVIDIA's compiler runs it in 47 us; we took 69 us.
 * A small side with only ALU and plain global loads runs unconditionally instead, each load predicated on the
 * branch condition, which LDG does in hardware: a false predicate makes no access and returns 0, so nothing
 * is read that the program would not have read. The phis become selects in peephole_select. */
static bool
nak_cg_predload_once(nir_shader *nir)
{
   bool progress = false;
   nir_divergence_analysis(nir);
   nir_foreach_function_impl(impl, nir) {
      bool p = false;
      nir_foreach_block(block, impl) {
         nir_if *nif = nir_block_get_following_if(block);
         if (nif == NULL || !nir_src_is_divergent(&nif->condition))
            continue;
         nir_block *tb = nir_if_first_then_block(nif), *eb = nir_if_first_else_block(nif);
         if (tb != nir_if_last_then_block(nif) || eb != nir_if_last_else_block(nif))
            continue;
         const bool t_empty = exec_list_is_empty(&tb->instr_list);
         const bool e_empty = exec_list_is_empty(&eb->instr_list);
         if (t_empty == e_empty)
            continue;   /* both empty: peephole_select's job; both full: not a guard */
         nir_block *side = t_empty ? eb : tb;
         unsigned loads = 0;
         const int n = nak_cg_predload_side(side, &loads);
         /* 12 instructions / 2 loads: a guard around a few taps, not a slow path */
         if (n < 0 || n > 12 || loads > 2)
            continue;
         nir_def *pred = nif->condition.ssa;
         if (t_empty) {
            nir_builder b = nir_builder_at(nir_after_block(block));
            pred = nir_inot(&b, pred);
         }
         nak_cg_predload_hoist(nir, side, block, pred);
         p = true;
      }
      progress |= nir_progress(p, impl, nir_metadata_control_flow);
   }
   return progress;
}

static bool
nak_cg_predload(nir_shader *nir)
{
   if (!nak_cg_predload_enabled() ||
       (nir->info.stage != MESA_SHADER_COMPUTE && nir->info.stage != MESA_SHADER_KERNEL))
      return false;
   const nir_opt_peephole_select_options sel = { .limit = 0 };
   bool progress = false;
   /* inner ifs first: once an inner side is hoisted and its if folded, the outer side is one block again */
   for (unsigned it = 0; it < 4; it++) {
      if (!nak_cg_predload_once(nir))
         break;
      progress = true;
      nir_opt_peephole_select(nir, &sel);
      nir_opt_dce(nir);
   }
   return progress;
}

static bool
nak_cg_punroll(nir_shader *nir)
{"""))
E.append(("""   OPT(nir, nir_opt_load_store_vectorize, &vectorize_opts);
""", """   OPT(nir, nir_opt_load_store_vectorize, &vectorize_opts);

   /* speed267: after the vectorizer, so a hoisted b32 pair is already one b64 when it becomes predicated */
   if (!raw_pressure && OPT(nir, nak_cg_predload)) {
      nak_optimize_nir(nir, nak);
      if (OPT(nir, nak_cg_punroll))
         nak_optimize_nir(nir, nak);
   }
"""))
for a, b in E:
    n = s.count(a)
    assert n == 1, (n, a[:80])
    s = s.replace(a, b)
bak = p + ".pre-predload"
if not os.path.exists(bak):
    shutil.copy2(p, bak)
tmp = p + ".tmp"
open(tmp, "w").write(s)
os.replace(tmp, p)
print("patched", p)
