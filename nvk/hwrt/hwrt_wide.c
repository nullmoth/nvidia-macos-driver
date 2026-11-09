/* The 8-wide node option of the hardware encoder, as a host reference: the binary intermediate tree is collapsed so each node holds up
 * to eight children with bounds quantised to 8 bits against the node's own frame (origin and per-axis step). Bounds are rounded outward
 * in the arithmetic the walk uses, so a quantised box always contains the real one. Node layout (128 bytes):
 *   0..11 origin, 12..15 pad, 16..27 step per axis, 28..31 pad, 32..127 eight child records of 12 bytes
 *   child: lo[3] u8, hi[3] u8, pad[2], ref u32 (byte offset | type, as in nvk_hwrt_layout.h; 0xffffffff = empty)
 * This is a model of the shape the measured structure suggests (rows R21/R22 of docs/HWRT.md, unmeasured): P3 builds either variant
 * and the card decides. The GLSL encoder writes the 2-wide variant only.
 *   cc -O2 -I. -o hwrt_wide hwrt_model.c hwrt_wide.c -lm */
#define main hwrt_ref_main
#include "hwrt_ref.c"
#undef main

#define WIDE_NODE 128u
struct wide { uint8_t *bytes; uint32_t nodes, cap, leaves_base, leaf_size; int conservative; };

static uint32_t wide_node(struct wide *w, const struct ir *ir, uint32_t ref, uint32_t root_offset, uint32_t leaf_type, uint32_t *depth_max);

static void box_of(const struct ir *ir, uint32_t ref, uint32_t root_offset, const float **lo, const float **hi)
{
   uint32_t off = ref & ~3u;
   if (off < root_offset) { *lo = ir->leaf_lo[off / IR_LEAF]; *hi = ir->leaf_hi[off / IR_LEAF]; }
   else { uint32_t j = (off - root_offset) / IR_BOX; *lo = ir->box_lo[j]; *hi = ir->box_hi[j]; }
}

/* returns the byte offset (from the node area) of the wide node built for the subtree at `ref`, which must be a box */
static uint32_t wide_node(struct wide *w, const struct ir *ir, uint32_t ref, uint32_t root_offset, uint32_t leaf_type, uint32_t *dm)
{
   uint32_t kids[8]; int n = 0;
   uint32_t j = ((ref & ~3u) - root_offset) / IR_BOX;
   kids[n++] = ir->child[j][0]; if (ir->child[j][1] != INVALID) kids[n++] = ir->child[j][1];
   while (n < 8) {                      /* open the internal child with the largest box */
      int best = -1; double ba = -1;
      for (int i = 0; i < n; i++) {
         if ((kids[i] & 3u) != T_NODE) continue;
         const float *lo, *hi; box_of(ir, kids[i], root_offset, &lo, &hi);
         double a = (double)(hi[0] - lo[0]) * (hi[1] - lo[1]) + (double)(hi[1] - lo[1]) * (hi[2] - lo[2]) + (double)(hi[0] - lo[0]) * (hi[2] - lo[2]);
         if (a > ba) { ba = a; best = i; }
      }
      if (best < 0) break;
      uint32_t jj = ((kids[best] & ~3u) - root_offset) / IR_BOX;
      uint32_t c0 = ir->child[jj][0], c1 = ir->child[jj][1];
      if (c1 == INVALID) { kids[best] = c0; continue; }
      if (n + 1 > 8) break;
      kids[best] = c0; kids[n++] = c1;
   }
   uint32_t me = w->nodes++;
   if (me * WIDE_NODE + WIDE_NODE > w->cap) { fprintf(stderr, "wide node area too small\n"); exit(2); }
   float org[3] = { INFINITY, INFINITY, INFINITY }, mx[3] = { -INFINITY, -INFINITY, -INFINITY };
   for (int i = 0; i < n; i++) {
      const float *lo, *hi; box_of(ir, kids[i], root_offset, &lo, &hi);
      for (int k = 0; k < 3; k++) { if (lo[k] < org[k]) org[k] = lo[k]; if (hi[k] > mx[k]) mx[k] = hi[k]; }
   }
   float step[3];
   for (int k = 0; k < 3; k++) { step[k] = (mx[k] - org[k]) / 255.0f; if (step[k] <= 0) step[k] = 1e-30f; }
   uint8_t *nd = w->bytes + (size_t)me * WIDE_NODE;
   memset(nd, 0, WIDE_NODE);
   for (int k = 0; k < 3; k++) { putf(nd, k * 4, org[k]); putf(nd, 16 + k * 4, step[k]); }
   for (int s = 0; s < 8; s++) put32(nd, 32 + s * 12 + 8, INVALID);
   for (int i = 0; i < n; i++) {
      const float *lo, *hi; box_of(ir, kids[i], root_offset, &lo, &hi);
      uint8_t *rec = nd + 32 + i * 12;
      for (int k = 0; k < 3; k++) {
         int ql = w->conservative ? (int)floorf((lo[k] - org[k]) / step[k]) : (int)ceilf((lo[k] - org[k]) / step[k]);
         int qh = w->conservative ? (int)ceilf((hi[k] - org[k]) / step[k]) : (int)floorf((hi[k] - org[k]) / step[k]);
         if (ql < 0) ql = 0; if (qh > 255) qh = 255; if (qh < ql) qh = ql;
         if (!w->conservative) { int sh = (qh - ql) / 8; ql += sh; qh -= sh; }   /* the control: boxes that do not contain what they hold */
         if (w->conservative) {         /* widen until the walk's own arithmetic contains the real box */
            while (ql > 0 && org[k] + (float)ql * step[k] > lo[k]) ql--;
            while (qh < 255 && org[k] + (float)qh * step[k] < hi[k]) qh++;
         }
         rec[k] = (uint8_t)ql; rec[3 + k] = (uint8_t)qh;
      }
      uint32_t r;
      if ((kids[i] & 3u) == T_NODE) { uint32_t child = wide_node(w, ir, kids[i], root_offset, leaf_type, dm); r = (child * WIDE_NODE) | T_NODE; }
      else r = kids[i];                 /* a leaf: patched to its final offset by the caller */
      put32(rec, 8, r);
   }
   return me;
}

static void patch_leaf_refs(struct wide *w, uint32_t nodes, uint32_t root_offset, uint32_t leaves_base, uint32_t leaf_size)
{
   for (uint32_t i = 0; i < nodes; i++)
      for (int s = 0; s < 8; s++) {
         uint8_t *rec = w->bytes + (size_t)i * WIDE_NODE + 32 + s * 12;
         uint32_t r = get32(rec, 8);
         if (r == INVALID || (r & 3u) == T_NODE) continue;
         (void)root_offset;
         put32(rec, 8, (leaves_base + (r / IR_LEAF) * leaf_size) | T_TRI);
      }
}

static int wide_walk(const uint8_t *nodes, const uint8_t *leaves_base_ptr, const float *org, const float *dir, float tmin, float *tmax, struct m_hit *best, uint32_t inst)
{
   uint32_t stack[128]; int sp = 0, steps = 0;
   stack[sp++] = 0 | T_NODE;
   while (sp) {
      if (++steps > 8192 || sp > 100) return 0;
      uint32_t ref = stack[--sp], off = ref & ~3u;
      if ((ref & 3u) == T_TRI) {
         const uint8_t *lf = leaves_base_ptr + off;
         float v[3][3], t, u, w; int front;
         for (int i = 0; i < 3; i++) for (int j = 0; j < 3; j++) v[i][j] = getf(lf, (i * 3 + j) * 4);
         if (!tri_hit(v, org, dir, &t, &u, &w, &front)) continue;
         if (t < tmin || t > *tmax) continue;
         if (!best->type || t < best->t) { struct m_hit h = { HIT_TRIANGLE, t, u, w, front, inst, get32(lf, 40), get32(lf, 36), 1 }; *best = h; *tmax = t; }
         continue;
      }
      const uint8_t *nd = nodes + off;
      float o3[3], st[3];
      for (int k = 0; k < 3; k++) { o3[k] = getf(nd, k * 4); st[k] = getf(nd, 16 + k * 4); }
      for (int s = 0; s < 8; s++) {
         const uint8_t *rec = nd + 32 + s * 12;
         uint32_t child = get32(rec, 8);
         if (child == INVALID) continue;
         float lo[3], hi[3];
         for (int k = 0; k < 3; k++) { lo[k] = o3[k] + (float)rec[k] * st[k]; hi[k] = o3[k] + (float)rec[3 + k] * st[k]; }
         if (slab(lo, hi, org, dir, tmin, *tmax)) stack[sp++] = child;
      }
   }
   return 1;
}

static int run_wide(int conservative, int scenes, int rays, int *nodes_wide, int *nodes_bin, int *hits)
{
   int bad = 0; *nodes_wide = *nodes_bin = *hits = 0;
   for (int sc = 0; sc < scenes; sc++) {
      struct m_scene s = { 0 };
      s.nblas = 1; s.ninst = 1; s.blas = calloc(1, sizeof *s.blas); s.inst = calloc(1, sizeof *s.inst);
      struct m_blas *bl = &s.blas[0]; bl->ngeom = 1; bl->ntri = 2 + rnd() % 120; bl->tri = calloc(bl->ntri, sizeof *bl->tri);
      for (int i = 0; i < bl->ntri; i++) {
         float c[3] = { frand(-6, 6), frand(-6, 6), frand(-6, 6) };
         for (int v = 0; v < 3; v++) for (int k = 0; k < 3; k++) bl->tri[i].v[v][k] = c[k] + frand(-0.8f, 0.8f);
         bl->tri[i].prim = i;
      }
      s.inst[0].mask = 0xff; for (int i = 0; i < 3; i++) s.inst[0].otw[i][i] = 1;
      scene_finish(&s);
      struct ir ir; ir_make(&ir, bl->ntri, T_TRI);
      uint32_t *ids = malloc(bl->ntri * sizeof *ids);
      for (int i = 0; i < bl->ntri; i++) { ids[i] = i; tri_box(&bl->tri[i], ir.leaf_lo[i], ir.leaf_hi[i]); }
      uint32_t nbox = 0; float lo[3], hi[3];
      uint32_t root = ir_build(&ir, ids, bl->ntri, T_TRI, &nbox, lo, hi);
      ir_single(&ir, root, &nbox, lo, hi);
      ir.internal_count = nbox;
      struct wide w = { .cap = (nbox + 1) * WIDE_NODE, .conservative = conservative };
      w.bytes = calloc(1, w.cap + (size_t)bl->ntri * 64);
      uint32_t dm = 0;
      uint32_t top = (nbox == 1 && (ir.child[0][1] == INVALID)) ? ((ir.leaf_count * IR_LEAF + 0 * IR_BOX) | T_NODE) : ((ir.leaf_count * IR_LEAF + (nbox - 1) * IR_BOX) | T_NODE);
      wide_node(&w, &ir, top, ir.leaf_count * IR_LEAF, T_TRI, &dm);
      uint32_t leaves_base = w.nodes * WIDE_NODE;
      patch_leaf_refs(&w, w.nodes, ir.leaf_count * IR_LEAF, leaves_base, 64);
      for (int i = 0; i < bl->ntri; i++) {
         uint8_t *lf = w.bytes + leaves_base + (size_t)i * 64;
         for (int a = 0; a < 3; a++) for (int b2 = 0; b2 < 3; b2++) putf(lf, (a * 3 + b2) * 4, bl->tri[i].v[a][b2]);
         put32(lf, 36, i); put32(lf, 40, 0);
      }
      *nodes_wide += w.nodes; *nodes_bin += nbox;
      for (int i = 0; i < rays; i++) {
         struct m_ray r = make_ray(); r.mask = 0xff;
         struct m_hit ref = hw_closest(&s, &r, r.tmin, r.tmax, r.mask), got = { 0 };
         float tm = r.tmax; float o[3], d[3];
         for (int k = 0; k < 3; k++) { o[k] = r.org[k]; d[k] = r.dir[k]; }
         wide_walk(w.bytes, w.bytes, o, d, r.tmin, &tm, &got, 0);
         if (ref.type) (*hits)++;
         bad += !same(&ref, &got);
      }
      free(w.bytes); free(ids); ir_free(&ir); free_scene(&s);
   }
   return bad;
}

int main(void)
{
   setvbuf(stdout, NULL, _IOLBF, 0);
   char d[160]; int nw, nb, hits;
   int bad = run_wide(1, 30, 300, &nw, &nb, &hits);
   snprintf(d, sizeof d, "%d of 9000 rays differ", bad);
   arm("8-wide nodes with 8-bit bounds: the walk equals the model's hardware primitive bit for bit", bad == 0, d);
   snprintf(d, sizeof d, "%d wide nodes against %d binary nodes, %d hits", nw, nb, hits);
   arm("the wide tree has at most two fifths of the binary tree's nodes", nw * 5 <= nb * 2, d);
   snprintf(d, sizeof d, "%d hits", hits);
   arm("  that run hit something on at least 300 of 9000 rays", hits > 300, d);
   int bad2 = run_wide(0, 30, 300, &nw, &nb, &hits);
   snprintf(d, sizeof d, "%d rays differ", bad2);
   arm("control: boxes that do not contain their children lose hits, and the suite sees it", bad2 > 0, d);
   printf("%d arms failed\n", fails);
   return fails != 0;
}
