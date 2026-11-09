/* The 8-wide node encoder's passes, as one text compiled both as GLSL (encode_wide.comp) and as C++ (hwrt_wide_shader.cpp), so the
 * host test executes the shader's own logic. The including file defines, before this header:
 *   ld32(addr) / ldf(addr) / st32(addr, v) / stf(addr, v) / st8(addr, v)   on 64-bit addresses
 *   W_ARGS      the parameter declaration of a wide_args (by value in GLSL, by const reference in C++)
 *   struct wide_args
 * Passes (one invocation per intermediate box, a barrier between dispatches):
 *   0  every box's depth word is unset, the root's is 0
 *   1  a box that has a depth gives each box child depth + 1 (run `rounds` times: the tree is not deeper than that)
 *   2  a box whose depth is a multiple of three becomes a wide node holding its descendants three levels down (or the leaves met
 *      sooner), up to eight, in depth-first order; slot = internal_count - 1 - box, so the root is slot 0
 * The depth word lives in the box's `flags` field, dead once the software encode has run.
 * Wide node (128 bytes): origin 3 floats at 0, step per axis 3 floats at 16, eight 12-byte children at 32: lo[3] u8, hi[3] u8, pad 2,
 * reference u32 (offset | type; 0xffffffff = empty). Bounds are widened until the walk's own arithmetic contains the child's box. */
#define W_INVALID 0xFFFFFFFFu
#define W_BOX_CHILDREN 24u
#define W_BOX_FLAGS 36u
#define W_NODE 128u

uint64_t
w_box(W_ARGS, uint32_t j)
{
   return a.ir + uint64_t(a.leaf_count) * uint64_t(a.ir_leaf_size) + uint64_t(j) * uint64_t(a.ir_box_size);
}

bool
w_is_box(W_ARGS, uint32_t ref)
{
   return ref != W_INVALID && (ref & ~3u) >= a.leaf_count * a.ir_leaf_size;
}

uint32_t
w_index(W_ARGS, uint32_t ref)
{
   return ((ref & ~3u) - a.leaf_count * a.ir_leaf_size) / a.ir_box_size;
}

uint32_t
w_child(W_ARGS, uint32_t j, uint32_t c)
{
   return ld32(w_box(a, j) + uint64_t(W_BOX_CHILDREN + 4u * c));
}

void
wide_pass_init(W_ARGS, uint32_t gid)
{
   if (gid >= a.internal_count) return;
   st32(w_box(a, gid) + uint64_t(W_BOX_FLAGS), gid == a.internal_count - 1u ? 0u : W_INVALID);
}

void
wide_pass_round(W_ARGS, uint32_t gid)
{
   if (gid >= a.internal_count) return;
   uint32_t d = ld32(w_box(a, gid) + uint64_t(W_BOX_FLAGS));
   if (d == W_INVALID) return;
   for (uint32_t c = 0u; c < 2u; c++) {
      uint32_t r = w_child(a, gid, c);
      if (w_is_box(a, r)) st32(w_box(a, w_index(a, r)) + uint64_t(W_BOX_FLAGS), d + 1u);
   }
}

void
wide_pass_write(W_ARGS, uint32_t gid)
{
   if (gid >= a.internal_count) return;
   uint32_t depth = ld32(w_box(a, gid) + uint64_t(W_BOX_FLAGS));
   if (depth == W_INVALID || depth % 3u != 0u) return;

   uint32_t kid[8];
   uint32_t n = 0u;
   for (uint32_t c0 = 0u; c0 < 2u; c0++) {
      uint32_t r0 = w_child(a, gid, c0);
      if (r0 == W_INVALID) continue;
      if (!w_is_box(a, r0)) { kid[n] = r0; n++; continue; }
      uint32_t j1 = w_index(a, r0);
      for (uint32_t c1 = 0u; c1 < 2u; c1++) {
         uint32_t r1 = w_child(a, j1, c1);
         if (r1 == W_INVALID) continue;
         if (!w_is_box(a, r1)) { kid[n] = r1; n++; continue; }
         uint32_t j2 = w_index(a, r1);
         for (uint32_t c2 = 0u; c2 < 2u; c2++) {
            uint32_t r2 = w_child(a, j2, c2);
            if (r2 == W_INVALID) continue;
            kid[n] = r2; n++;
         }
      }
   }

   float org[3];
   float mx[3];
   for (uint32_t k = 0u; k < 3u; k++) { org[k] = 1.0e38f; mx[k] = -1.0e38f; }
   for (uint32_t i = 0u; i < n; i++) {
      uint64_t bb = a.ir + uint64_t(kid[i] & ~3u);
      for (uint32_t k = 0u; k < 3u; k++) {
         float lo = ldf(bb + uint64_t(4u * k)), hi = ldf(bb + uint64_t(12u + 4u * k));
         org[k] = lo < org[k] ? lo : org[k];
         mx[k] = hi > mx[k] ? hi : mx[k];
      }
   }
   float step[3];
   for (uint32_t k = 0u; k < 3u; k++) {
      step[k] = (mx[k] - org[k]) / 255.0f;
      if (step[k] <= 0.0f) step[k] = 1.0e-30f;
   }
   uint64_t dst = a.dst + uint64_t(a.hdr) + uint64_t(a.internal_count - 1u - gid) * uint64_t(W_NODE);
   for (uint32_t b = 0u; b < W_NODE; b += 4u) st32(dst + uint64_t(b), 0u);
   for (uint32_t k = 0u; k < 3u; k++) { stf(dst + uint64_t(4u * k), org[k]); stf(dst + uint64_t(16u + 4u * k), step[k]); }
   for (uint32_t s = 0u; s < 8u; s++) st32(dst + uint64_t(32u + 12u * s + 8u), W_INVALID);
   uint32_t leaves_base = a.hdr + a.internal_count * W_NODE;
   for (uint32_t i = 0u; i < n; i++) {
      uint64_t bb = a.ir + uint64_t(kid[i] & ~3u);
      uint64_t rec = dst + uint64_t(32u + 12u * i);
      for (uint32_t k = 0u; k < 3u; k++) {
         float lo = ldf(bb + uint64_t(4u * k)), hi = ldf(bb + uint64_t(12u + 4u * k));
         int ql = int(floor((lo - org[k]) / step[k]));
         int qh = int(ceil((hi - org[k]) / step[k]));
         if (ql < 0) ql = 0;
         if (qh > 255) qh = 255;
         if (qh < ql) qh = ql;
         while (ql > 0 && org[k] + float(ql) * step[k] > lo) ql--;
         while (qh < 255 && org[k] + float(qh) * step[k] < hi) qh++;
         st8(rec + uint64_t(k), uint32_t(ql));
         st8(rec + uint64_t(3u + k), uint32_t(qh));
      }
      uint32_t ref;
      if (w_is_box(a, kid[i]))
         ref = (a.hdr + (a.internal_count - 1u - w_index(a, kid[i])) * W_NODE) | 1u;
      else
         ref = (leaves_base + ((kid[i] & ~3u) / a.ir_leaf_size) * a.out_leaf_size) | a.leaf_type;
      st32(rec + 8u, ref);
   }
}
