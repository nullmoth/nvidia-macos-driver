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
#define HDR NVK_HWL_HEADER_SIZE
struct wide { uint8_t *bytes; uint32_t nodes, internal, leaf_count; int conservative; };

extern void shader_wide_encode(uint8_t *ir, uint8_t *out, uint32_t leaf_count, uint32_t internal_count, uint32_t leaf_type,
                               uint32_t out_leaf_size, uint32_t hdr, uint32_t ir_leaf_size, uint32_t ir_box_size, uint32_t rounds);

static void box_of(const struct ir *ir, uint32_t ref, uint32_t root_offset, const float **lo, const float **hi)
{
   uint32_t off = ref & ~3u;
   if (off < root_offset) { *lo = ir->leaf_lo[off / IR_LEAF]; *hi = ir->leaf_hi[off / IR_LEAF]; }
   else { uint32_t j = (off - root_offset) / IR_BOX; *lo = ir->box_lo[j]; *hi = ir->box_hi[j]; }
}

/* The descendants of box j three levels down, or the leaves met sooner, depth first: the definition the shader's three nested loops
 * implement, written here as a recursion. */
static void collect(const struct ir *ir, uint32_t root_offset, uint32_t j, int level, uint32_t *kids, int *n)
{
   for (int c = 0; c < 2; c++) {
      uint32_t r = ir->child[j][c];
      if (r == INVALID) continue;
      int is_box = (r & ~3u) >= root_offset;
      if (!is_box || level == 3) kids[(*n)++] = r;
      else collect(ir, root_offset, (((r & ~3u) - root_offset) / IR_BOX), level + 1, kids, n);
   }
}

static void wide_node(struct wide *w, const struct ir *ir, uint32_t root_offset, uint32_t j, uint32_t leaves_base)
{
   uint32_t kids[8]; int n = 0;
   collect(ir, root_offset, j, 1, kids, &n);
   uint32_t slot = w->internal - 1 - j;
   w->nodes++;
   float org[3] = { 1e38f, 1e38f, 1e38f }, mx[3] = { -1e38f, -1e38f, -1e38f };
   for (int i = 0; i < n; i++) {
      const float *lo, *hi; box_of(ir, kids[i], root_offset, &lo, &hi);
      for (int k = 0; k < 3; k++) { org[k] = lo[k] < org[k] ? lo[k] : org[k]; mx[k] = hi[k] > mx[k] ? hi[k] : mx[k]; }
   }
   float step[3];
   for (int k = 0; k < 3; k++) { step[k] = (mx[k] - org[k]) / 255.0f; if (step[k] <= 0.0f) step[k] = 1e-30f; }
   uint8_t *nd = w->bytes + HDR + (size_t)slot * WIDE_NODE;
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
         if (w->conservative) {
            while (ql > 0 && org[k] + (float)ql * step[k] > lo[k]) ql--;
            while (qh < 255 && org[k] + (float)qh * step[k] < hi[k]) qh++;
         }
         rec[k] = (uint8_t)ql; rec[3 + k] = (uint8_t)qh;
      }
      uint32_t r;
      if ((kids[i] & ~3u) >= root_offset) {
         uint32_t cj = ((kids[i] & ~3u) - root_offset) / IR_BOX;
         r = (HDR + (w->internal - 1 - cj) * WIDE_NODE) | T_NODE;
         wide_node(w, ir, root_offset, cj, leaves_base);
      } else r = (leaves_base + ((kids[i] & ~3u) / IR_LEAF) * 64) | T_TRI;
      put32(rec, 8, r);
   }
}

static int wide_walk(const uint8_t *nodes, const uint8_t *leaves_base_ptr, const float *org, const float *dir, float tmin, float *tmax, struct m_hit *best, uint32_t inst)
{
   uint32_t stack[128]; int sp = 0, steps = 0;
   stack[sp++] = HDR | T_NODE;
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

/* the intermediate tree as the shader reads it: leaves then boxes, aabb first, children at +24, depth word at +36 */
static uint8_t *flat_ir(const struct ir *ir, uint32_t root_offset, uint32_t nbox)
{
   uint8_t *b = calloc(1, root_offset + (size_t)nbox * IR_BOX + 64);
   for (uint32_t i = 0; i < ir->leaf_count; i++) for (int k = 0; k < 3; k++) { putf(b, i * IR_LEAF + k * 4, ir->leaf_lo[i][k]); putf(b, i * IR_LEAF + 12 + k * 4, ir->leaf_hi[i][k]); }
   for (uint32_t j = 0; j < nbox; j++) {
      uint8_t *p = b + root_offset + (size_t)j * IR_BOX;
      for (int k = 0; k < 3; k++) { putf(p, k * 4, ir->box_lo[j][k]); putf(p, 12 + k * 4, ir->box_hi[j][k]); }
      put32(p, 24, ir->child[j][0]); put32(p, 28, ir->child[j][1]);
   }
   return b;
}

/* returns rays that differ; *shader_bytes_differ counts scenes whose shader-built node area differs from the reference's */
static int run_wide(int conservative, int scenes, int rays, int *nodes_wide, int *nodes_bin, int *hits, int *shader_differ)
{
   int bad = 0; *nodes_wide = *nodes_bin = *hits = *shader_differ = 0;
   for (int sc = 0; sc < scenes; sc++) {
      struct m_scene s = { 0 };
      s.nblas = 1; s.ninst = 1; s.blas = calloc(1, sizeof *s.blas); s.inst = calloc(1, sizeof *s.inst);
      struct m_blas *bl = &s.blas[0]; bl->ngeom = 1; bl->ntri = 1 + (sc % 3 == 0 ? rnd() % 4 : rnd() % 150); bl->tri = calloc(bl->ntri, sizeof *bl->tri);
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
      uint32_t root_offset = ir.leaf_count * IR_LEAF, leaves_base = HDR + nbox * WIDE_NODE;
      struct wide w = { .internal = nbox, .leaf_count = bl->ntri, .conservative = conservative };
      size_t total = leaves_base + (size_t)bl->ntri * 64;
      w.bytes = calloc(1, total);
      wide_node(&w, &ir, root_offset, nbox - 1, leaves_base);
      for (int i = 0; i < bl->ntri; i++) {
         uint8_t *lf = w.bytes + leaves_base + (size_t)i * 64;
         for (int a = 0; a < 3; a++) for (int b2 = 0; b2 < 3; b2++) putf(lf, (a * 3 + b2) * 4, bl->tri[i].v[a][b2]);
         put32(lf, 36, i); put32(lf, 40, 0);
      }
      if (conservative) {                /* the shader's own text, run on the same intermediate tree */
         uint8_t *fir = flat_ir(&ir, root_offset, nbox), *out = calloc(1, total);
         shader_wide_encode(fir, out, bl->ntri, nbox, T_TRI, 64, HDR, IR_LEAF, IR_BOX, 64);
         if (memcmp(out + HDR, w.bytes + HDR, (size_t)nbox * WIDE_NODE)) (*shader_differ)++;
         free(fir); free(out);
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
   char d[160]; int nw, nb, hits, sd;
   int bad = run_wide(1, 40, 250, &nw, &nb, &hits, &sd);
   snprintf(d, sizeof d, "%d of 10000 rays differ", bad);
   arm("8-wide nodes with 8-bit bounds: the walk equals the model's hardware primitive bit for bit", bad == 0, d);
   snprintf(d, sizeof d, "%d of 40 scenes differ", sd);
   arm("the GLSL passes (encode_wide_body.h, run as C++) write the node area byte for byte as the reference does", sd == 0, d);
   snprintf(d, sizeof d, "%d wide nodes against %d binary nodes", nw, nb);
   arm("the wide tree has at most half of the binary tree's nodes", nw * 2 <= nb, d);
   snprintf(d, sizeof d, "%d hits", hits);
   arm("  that run hit something on at least 300 of 10000 rays", hits > 300, d);
   int bad2 = run_wide(0, 40, 250, &nw, &nb, &hits, &sd);
   snprintf(d, sizeof d, "%d rays differ", bad2);
   arm("control: boxes that do not contain their children lose hits, and the suite sees it", bad2 > 0, d);
   printf("%d arms failed\n", fails);
   return fails != 0;
}
