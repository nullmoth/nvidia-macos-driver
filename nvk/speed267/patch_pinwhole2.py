#!/usr/bin/env python3
# patch_pinwhole2.py <nak_nir_lower_non_uniform_ldcx.c>: on top of patch_pinwhole.py -
#  (1) rank bases by what pinning saves: a split access saves 4 instructions, a whole one 2, so split uses weigh 2;
#  (2) a whole-only base is pinned only with >= 2 uses (one use: pin copy == the 2 movs it saves);
#  (3) no unpin when the section's successor is the function's last block - nothing follows, and an unpin
#      there keeps the block non-empty, which costs the "if (!in_bounds) return" -> exit shortcut.
import os, shutil, sys
p = sys.argv[1]
s = open(p).read()
A1 = """         struct hash_entry *e = _mesa_hash_table_search(uses, base);
         if (e)
            e->data = (void *)((uintptr_t)e->data + 1);
         else
            _mesa_hash_table_insert(uses, base, (void *)(uintptr_t)1);"""
B1 = """         /* low 16 bits: whole-address uses, high bits: iadd-split uses */
         const uintptr_t inc = div_idx >= PIN_ADDR_KEEP ? 1 : 0x10000;
         struct hash_entry *e = _mesa_hash_table_search(uses, base);
         if (e)
            e->data = (void *)((uintptr_t)e->data + inc);
         else
            _mesa_hash_table_insert(uses, base, (void *)inc);"""
A2 = """   /* Most-used bases first, ties by SSA index so the choice is deterministic. */
   NIR_VLA(struct handle, bases, n);
   unsigned i = 0;
   hash_table_foreach(uses, e) {
      nir_def *base = (nir_def *)e->key;
      bases[i++] = (struct handle) {
         .handle = base,
         .sort_key = ((uint64_t)(UINT32_MAX - (uint32_t)(uintptr_t)e->data) << 32) | base->index,
      };
   }
   qsort(bases, n, sizeof(*bases), cmp_handles);"""
B2 = """   /* Largest saving first, ties by SSA index so the choice is deterministic.
    * A split access saves iadd3 + iadd3.x + 2 movs, a whole one only the 2
    * movs, so split uses weigh 2. A base seen only whole and only once is
    * left alone: its pin copy costs what it saves. */
   NIR_VLA(struct handle, bases, n);
   unsigned i = 0;
   hash_table_foreach(uses, e) {
      nir_def *base = (nir_def *)e->key;
      const uintptr_t d = (uintptr_t)e->data;
      const uint32_t split = d >> 16, whole = d & 0xffff;
      if (split == 0 && whole < 2)
         continue;
      bases[i++] = (struct handle) {
         .handle = base,
         .sort_key = ((uint64_t)(UINT32_MAX - (2 * split + whole)) << 32) | base->index,
      };
   }
   if (i == 0) {
      _mesa_hash_table_destroy(uses, NULL);
      return false;
   }
   qsort(bases, i, sizeof(*bases), cmp_handles);"""
A3 = """   const unsigned npin = MIN2(n, max_bases);"""
B3 = """   const unsigned npin = MIN2(i, max_bases);"""
A4 = """   b->cursor = nir_before_block_after_phis(succ);
   for (i = 0; i < npin; i++)
      nir_unpin_cx_handle_nv(b, bases[i].handle);

   _mesa_hash_table_destroy(uses, NULL);
   return true;"""
B4 = """   /* A pin with no unpin stays pinned to the end of the program, which is
    * where we already are when succ is the last block of the function. */
   const bool succ_is_end = succ->cf_node.parent->type == nir_cf_node_function &&
                            nir_cf_node_is_last(&succ->cf_node) &&
                            exec_list_is_empty(&succ->instr_list);
   if (!succ_is_end || !pin_addr_whole_enabled()) {
      b->cursor = nir_before_block_after_phis(succ);
      for (i = 0; i < npin; i++)
         nir_unpin_cx_handle_nv(b, bases[i].handle);
   }

   _mesa_hash_table_destroy(uses, NULL);
   return true;"""
for a, b in ((A1, B1), (A2, B2), (A3, B3), (A4, B4)):
    n = s.count(a)
    assert n == 1, (n, a[:70])
    s = s.replace(a, b)
bak = p + ".pre-pinwhole2"
if not os.path.exists(bak):
    shutil.copy2(p, bak)
tmp = p + ".tmp"
open(tmp, "w").write(s)
os.replace(tmp, p)
print("patched", p)
