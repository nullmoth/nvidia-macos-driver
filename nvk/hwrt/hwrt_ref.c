/* Host reference for the hardware encode pass: builds the structure bytes bvh/encode_ttu.comp writes (the index arithmetic is a
 * line by line port of that shader), walks them with a plain stack traversal, and checks the walk against the model's hardware
 * primitive over random scenes. It proves the mapping (internal-node numbering, leaf offsets, child references, instance leaves,
 * masks, header words, relocation after a copy). It cannot say anything about the card: the node and leaf contents are our own
 * rule in layout generation 1, and docs/HWRT.md lists what the measurement procedure has to measure before that changes.
 *
 * cc -O2 -I. -o hwrt_ref hwrt_model.c hwrt_ref.c -lm && ./hwrt_ref */
#include "hwrt_model.h"
#include "nvk_hwrt_layout.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define IR_LEAF 64u    /* stand-ins for the intermediate node sizes: only their ratio to the output sizes matters */
#define IR_BOX  64u
#define T_TRI  0u
#define T_NODE 1u
#define T_INST 2u
#define T_BOX  3u
#define INVALID 0xFFFFFFFFu

struct ir {                       /* the shared builder's intermediate BVH, as encode.comp sees it */
   uint32_t leaf_count, internal_count;
   float (*leaf_lo)[3], (*leaf_hi)[3];
   uint32_t (*child)[2];           /* per IR box: child ref = byte offset | type */
   float (*box_lo)[3], (*box_hi)[3];
};

static void grow(float *lo, float *hi, const float *a, const float *b)
{
   for (int k = 0; k < 3; k++) { if (a[k] < lo[k]) lo[k] = a[k]; if (b[k] > hi[k]) hi[k] = b[k]; }
}

/* median split on the widest axis; boxes are created children first, so the root is the last IR box */
static uint32_t ir_build(struct ir *ir, uint32_t *ids, uint32_t n, uint32_t leaf_type, uint32_t *nbox, float lo[3], float hi[3])
{
   if (n == 1) {
      memcpy(lo, ir->leaf_lo[ids[0]], 12); memcpy(hi, ir->leaf_hi[ids[0]], 12);
      return ids[0] * IR_LEAF | leaf_type;
   }
   float mn[3] = { INFINITY, INFINITY, INFINITY }, mx[3] = { -INFINITY, -INFINITY, -INFINITY };
   for (uint32_t i = 0; i < n; i++)
      for (int k = 0; k < 3; k++) {
         float c = 0.5f * (ir->leaf_lo[ids[i]][k] + ir->leaf_hi[ids[i]][k]);
         if (c < mn[k]) mn[k] = c;
         if (c > mx[k]) mx[k] = c;
      }
   int ax = 0;
   for (int k = 1; k < 3; k++) if (mx[k] - mn[k] > mx[ax] - mn[ax]) ax = k;
   for (uint32_t i = 1; i < n; i++)         /* insertion sort by centroid: scenes are small */
      for (uint32_t j = i; j > 0; j--) {
         float a = ir->leaf_lo[ids[j]][ax] + ir->leaf_hi[ids[j]][ax], b = ir->leaf_lo[ids[j - 1]][ax] + ir->leaf_hi[ids[j - 1]][ax];
         if (a < b) { uint32_t t = ids[j]; ids[j] = ids[j - 1]; ids[j - 1] = t; } else break;
      }
   float l0[3], h0[3], l1[3], h1[3];
   uint32_t c0 = ir_build(ir, ids, n / 2, leaf_type, nbox, l0, h0);
   uint32_t c1 = ir_build(ir, ids + n / 2, n - n / 2, leaf_type, nbox, l1, h1);
   uint32_t j = (*nbox)++;
   ir->child[j][0] = c0; ir->child[j][1] = c1;
   memcpy(lo, l0, 12); memcpy(hi, h0, 12); grow(lo, hi, l1, h1);
   memcpy(ir->box_lo[j], lo, 12); memcpy(ir->box_hi[j], hi, 12);
   return (ir->leaf_count * IR_LEAF + j * IR_BOX) | T_NODE;
}

/* one primitive: the shared builder still makes a root box, with the leaf as its first child and the second child invalid */
static void ir_single(struct ir *ir, uint32_t root, uint32_t *nbox, const float *lo, const float *hi)
{
   if (*nbox != 0) return;
   ir->child[0][0] = root; ir->child[0][1] = INVALID;
   memcpy(ir->box_lo[0], lo, 12); memcpy(ir->box_hi[0], hi, 12);
   *nbox = 1;
}

static void ir_make(struct ir *ir, uint32_t n, uint32_t leaf_type)
{
   memset(ir, 0, sizeof *ir);
   ir->leaf_count = n;
   ir->leaf_lo = calloc(n, sizeof *ir->leaf_lo); ir->leaf_hi = calloc(n, sizeof *ir->leaf_hi);
   ir->child = calloc(n + 1, sizeof *ir->child);
   ir->box_lo = calloc(n + 1, sizeof *ir->box_lo); ir->box_hi = calloc(n + 1, sizeof *ir->box_hi);
   (void)leaf_type;
}
static void ir_free(struct ir *ir) { free(ir->leaf_lo); free(ir->leaf_hi); free(ir->child); free(ir->box_lo); free(ir->box_hi); }

/* ---- the encode, a port of bvh/encode_ttu.comp ---------------------------------------------------------------------- */

static void put32(uint8_t *base, uint32_t off, uint32_t v) { memcpy(base + off, &v, 4); }
static void put64(uint8_t *base, uint32_t off, uint64_t v) { memcpy(base + off, &v, 8); }
static void putf(uint8_t *base, uint32_t off, float v) { memcpy(base + off, &v, 4); }
static uint32_t get32(const uint8_t *base, uint32_t off) { uint32_t v; memcpy(&v, base + off, 4); return v; }
static uint64_t get64(const uint8_t *base, uint32_t off) { uint64_t v; memcpy(&v, base + off, 8); return v; }
static float getf(const uint8_t *base, uint32_t off) { float v; memcpy(&v, base + off, 4); return v; }

struct src_prims {
   uint32_t n, type;                       /* type T_TRI or T_INST */
   const struct m_tri *tri;
   const struct m_inst *inst;
   const uint64_t *inst_root;              /* bottom level hardware base of each instance, 0 when none */
};

static uint32_t hw_size_for(uint32_t n, uint32_t leaf_size)
{
   uint32_t nodes = n > 2 ? n - 1 : 1;
   return NVK_HWL_HEADER_SIZE + nodes * NVK_HWL_NODE_SIZE + n * leaf_size;
}

/* returns the structure (malloc'd, 256 aligned), its size in *size */
static uint8_t *encode(const struct ir *ir, const struct src_prims *sp, uint32_t *size)
{
   const uint32_t leaf_size = sp->type == T_INST ? NVK_HWL_INST_LEAF_SIZE : NVK_HWL_TRI_LEAF_SIZE;
   const uint32_t hw_size = (hw_size_for(ir->leaf_count, leaf_size) + 255u) & ~255u;
   uint8_t *hw = aligned_alloc(256, hw_size);
   memset(hw, 0, hw_size);
   const uint32_t leaf_count = ir->leaf_count;
   const uint32_t internal_count = ir->internal_count > 0 ? ir->internal_count : 1;
   const uint32_t leaves_base = NVK_HWL_HEADER_SIZE + internal_count * NVK_HWL_NODE_SIZE;
   const uint32_t root_offset = leaf_count * IR_LEAF;
   const uint32_t span = leaf_count > internal_count ? leaf_count : internal_count;

   for (uint32_t gid = 0; gid < span; gid++) {
      if (gid < leaf_count) {
         uint8_t *dst = hw + leaves_base + gid * leaf_size;
         if (sp->type == T_TRI) {
            for (int i = 0; i < 3; i++) for (int j = 0; j < 3; j++) putf(dst, (i * 3 + j) * 4, sp->tri[gid].v[i][j]);
            put32(dst, 36, sp->tri[gid].prim);
            put32(dst, 40, sp->tri[gid].geom);
         } else {
            const struct m_inst *in = &sp->inst[gid];
            for (int r = 0; r < 3; r++) for (int c = 0; c < 4; c++) putf(dst, NVK_HWL_INST_XFORM + (r * 4 + c) * 4, in->otw[r][c]);
            put64(dst, NVK_HWL_INST_ROOT, sp->inst_root[gid]);
            put32(dst, NVK_HWL_INST_MASK, in->mask);
            put32(dst, NVK_HWL_INST_INDEX, gid);
            put32(dst, NVK_HWL_INST_SBT, in->sbt);
            put32(dst, NVK_HWL_INST_CUSTOM, in->custom);
            put32(dst, NVK_HWL_INST_FLAGS, in->flags);
         }
      }
      if (gid >= internal_count) continue;
      const uint32_t src_index = internal_count - 1 - gid;
      uint8_t *dst = hw + NVK_HWL_HEADER_SIZE + gid * NVK_HWL_NODE_SIZE;
      const int built = ir->internal_count > 0;
      for (uint32_t c = 0; c < 2; c++) {
         uint32_t child = built ? ir->child[src_index][c] : INVALID;
         uint32_t slot = c * 56u;
         if (child == INVALID) {
            for (uint32_t k = 0; k < 6; k++) putf(dst, slot + k * 4, NAN);
            put32(dst, slot + 24, INVALID);
            continue;
         }
         uint32_t child_offset = child & ~3u;
         const float *lo, *hi;
         if (child_offset < root_offset) { lo = ir->leaf_lo[child_offset / IR_LEAF]; hi = ir->leaf_hi[child_offset / IR_LEAF]; }
         else { uint32_t j = (child_offset - root_offset) / IR_BOX; lo = ir->box_lo[j]; hi = ir->box_hi[j]; }
         for (uint32_t k = 0; k < 3; k++) {
            float a = lo[k], b = hi[k];
            a -= fmaxf(fabsf(a), 1.0f) * 1.1920929e-7f; b += fmaxf(fabsf(b), 1.0f) * 1.1920929e-7f;
            putf(dst, slot + k * 4, a); putf(dst, slot + 12 + k * 4, b);
         }
         if (child_offset < root_offset) {
            put32(dst, slot + 24, (leaves_base + (child_offset / IR_LEAF) * leaf_size) | (sp->type == T_TRI ? T_TRI : T_INST));
         } else {
            uint32_t si = (child_offset - root_offset) / IR_BOX;
            uint32_t di = internal_count - si - 1;
            put32(dst, slot + 24, (NVK_HWL_HEADER_SIZE + di * NVK_HWL_NODE_SIZE) | T_NODE);
         }
      }
      if (gid == 0) {
         put32(hw, 0x000, sp->type == T_INST ? NVK_HWL_MAGIC_TLAS : NVK_HWL_MAGIC_BLAS);
         put32(hw, NVK_HWL_OFF_FLAGS, NVK_HWL_BUILD_FLAGS);
         put32(hw, NVK_HWL_OFF_SIZE, hw_size);
         put32(hw, NVK_HWL_OFF_LEAVES, leaves_base);
         put64(hw, NVK_HWL_OFF_ROOT, NVK_HWL_HEADER_SIZE);
         put64(hw, NVK_HWL_OFF_SENTINEL, ~0ull);
         put32(hw, NVK_HWL_OFF_NODES, NVK_HWL_NODES_VALUE);
         put32(hw, NVK_HWL_OFF_GEOMS, sp->type == T_INST ? 0 : 1);
         put32(hw, NVK_HWL_OFF_PRIMS, leaf_count);
         put64(hw, NVK_HWL_OFF_SELF, (uint64_t)(uintptr_t)hw);
      }
   }
   *size = hw_size;
   return hw;
}

static void relocate(uint8_t *hw) { put64(hw, NVK_HWL_OFF_SELF, (uint64_t)(uintptr_t)hw); }

/* ---- the walk ------------------------------------------------------------------------------------------------------- */

static int slab(const float *lo, const float *hi, const float *org, const float *dir, float tmin, float tmax)
{
   float n = tmin, f = tmax;
   for (int i = 0; i < 3; i++) {
      float a = (lo[i] - org[i]) / dir[i], b = (hi[i] - org[i]) / dir[i];
      if (dir[i] == 0.0f) { if (org[i] < lo[i] || org[i] > hi[i]) return 0; continue; }
      if (a > b) { float x = a; a = b; b = x; }
      if (a > n) n = a;
      if (b < f) f = b;
   }
   return n <= f;
}

/* Moeller-Trumbore with the model's arithmetic order, so a bit-exact comparison is fair */
static int tri_hit(const float v[3][3], const float *org, const float *dir, float *t, float *u, float *w, int *front)
{
   float e1[3], e2[3], p[3], s[3], q[3];
   for (int i = 0; i < 3; i++) { e1[i] = v[1][i] - v[0][i]; e2[i] = v[2][i] - v[0][i]; }
   p[0] = dir[1] * e2[2] - dir[2] * e2[1]; p[1] = dir[2] * e2[0] - dir[0] * e2[2]; p[2] = dir[0] * e2[1] - dir[1] * e2[0];
   float det = e1[0] * p[0] + e1[1] * p[1] + e1[2] * p[2];
   if (det == 0.0f) return 0;
   float inv = 1.0f / det;
   for (int i = 0; i < 3; i++) s[i] = org[i] - v[0][i];
   *u = (s[0] * p[0] + s[1] * p[1] + s[2] * p[2]) * inv;
   if (*u < 0.0f || *u > 1.0f) return 0;
   q[0] = s[1] * e1[2] - s[2] * e1[1]; q[1] = s[2] * e1[0] - s[0] * e1[2]; q[2] = s[0] * e1[1] - s[1] * e1[0];
   *w = (dir[0] * q[0] + dir[1] * q[1] + dir[2] * q[2]) * inv;
   if (*w < 0.0f || *u + *w > 1.0f) return 0;
   *t = (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) * inv;
   *front = det > 0.0f;
   return 1;
}

static void walk_blas(const uint8_t *hw, const float *org, const float *dir, float tmin, float *tmax, struct m_hit *best, uint32_t inst)
{
   uint32_t stack[128]; int sp = 0, steps = 0;
   stack[sp++] = (uint32_t)get64(hw, NVK_HWL_OFF_ROOT) | T_NODE;
   while (sp) {
      if (++steps > 4096 || sp > 100) return;      /* a damaged structure can loop; a legal one visits each node once */
      uint32_t ref = stack[--sp], off = ref & ~3u;
      if ((ref & 3u) == T_TRI) {
         float v[3][3], t, u, w; int front;
         for (int i = 0; i < 3; i++) for (int j = 0; j < 3; j++) v[i][j] = getf(hw, off + (i * 3 + j) * 4);
         if (!tri_hit(v, org, dir, &t, &u, &w, &front)) continue;
         if (t < tmin || t > *tmax) continue;
         if (!best->type || t < best->t) {
            struct m_hit h = { HIT_TRIANGLE, t, u, w, front, inst, get32(hw, off + 40), get32(hw, off + 36), 1 };
            *best = h; *tmax = t;
         }
         continue;
      }
      for (int c = 0; c < 2; c++) {
         const uint8_t *slot = hw + off + c * 56;
         uint32_t child = get32(slot, 24);
         if (child == INVALID) continue;
         float lo[3], hi[3];
         for (int k = 0; k < 3; k++) { lo[k] = getf(slot, k * 4); hi[k] = getf(slot, 12 + k * 4); }
         if (slab(lo, hi, org, dir, tmin, *tmax)) stack[sp++] = child;
      }
   }
}

/* The scene's wto matrices come from the model: this walk checks the structure, the transform arithmetic is not its subject. */
static struct m_hit walk_tlas(const uint8_t *tlas, const struct m_scene *s, const struct m_ray *r, float tmin, float tmax, uint32_t mask,
                              uint8_t *const *known)
{
   struct m_hit best = { 0 };
   uint32_t stack[128]; int sp = 0, steps = 0;
   stack[sp++] = (uint32_t)get64(tlas, NVK_HWL_OFF_ROOT) | T_NODE;
   while (sp) {
      if (++steps > 4096 || sp > 100) break;
      uint32_t ref = stack[--sp], off = ref & ~3u;
      if (off + NVK_HWL_INST_LEAF_SIZE > get32(tlas, NVK_HWL_OFF_SIZE)) break;       /* a damaged reference points outside */
      if ((ref & 3u) == T_INST) {
         uint32_t m = get32(tlas, off + NVK_HWL_INST_MASK);
         if (!(m & mask)) continue;
         uint32_t idx = get32(tlas, off + NVK_HWL_INST_INDEX);
         const uint8_t *blas = (const uint8_t *)(uintptr_t)get64(tlas, off + NVK_HWL_INST_ROOT);
         int ok = idx < (uint32_t)s->ninst;
         int found = 0;
         for (int b = 0; b < 8; b++) found |= known[b] == blas;
         if (!ok || !found) break;
         const struct m_inst *in = &s->inst[idx];
         float o[3], d[3];
         for (int i = 0; i < 3; i++) {
            d[i] = in->wto[i][0] * r->dir[0] + in->wto[i][1] * r->dir[1] + in->wto[i][2] * r->dir[2];
            o[i] = in->wto[i][0] * r->org[0] + in->wto[i][1] * r->org[1] + in->wto[i][2] * r->org[2] + in->wto[i][3];
         }
         float tm = best.type ? best.t : tmax;
         walk_blas(blas, o, d, tmin, &tm, &best, idx);
         continue;
      }
      for (int c = 0; c < 2; c++) {
         const uint8_t *slot = tlas + off + c * 56;
         uint32_t child = get32(slot, 24);
         if (child == INVALID) continue;
         /* instance boxes are in world space: the leaf boxes below were built from the world-space bounds of each instance */
         float lo[3], hi[3];
         for (int k = 0; k < 3; k++) { lo[k] = getf(slot, k * 4); hi[k] = getf(slot, 12 + k * 4); }
         if (slab(lo, hi, r->org, r->dir, tmin, best.type ? best.t : tmax)) stack[sp++] = child;
      }
   }
   return best;
}

/* ---- building from a model scene -------------------------------------------------------------------------------------- */

struct built {
   uint8_t *blas[8]; uint32_t blas_size[8];
   uint8_t *tlas; uint32_t tlas_size;
};

static void tri_box(const struct m_tri *t, float *lo, float *hi)
{
   for (int k = 0; k < 3; k++) {
      lo[k] = fminf(t->v[0][k], fminf(t->v[1][k], t->v[2][k]));
      hi[k] = fmaxf(t->v[0][k], fmaxf(t->v[1][k], t->v[2][k]));
   }
}

static void build_scene(const struct m_scene *s, struct built *out)
{
   memset(out, 0, sizeof *out);
   for (int b = 0; b < s->nblas; b++) {
      const struct m_blas *bl = &s->blas[b];
      if (bl->ntri == 0) continue;
      struct ir ir; ir_make(&ir, bl->ntri, T_TRI);
      uint32_t *ids = malloc(bl->ntri * sizeof *ids);
      for (int i = 0; i < bl->ntri; i++) { ids[i] = i; tri_box(&bl->tri[i], ir.leaf_lo[i], ir.leaf_hi[i]); }
      uint32_t nbox = 0; float lo[3], hi[3];
      uint32_t root = ir_build(&ir, ids, bl->ntri, T_TRI, &nbox, lo, hi);
      ir_single(&ir, root, &nbox, lo, hi);
      ir.internal_count = nbox;
      struct src_prims sp = { bl->ntri, T_TRI, bl->tri, NULL, NULL };
      out->blas[b] = encode(&ir, &sp, &out->blas_size[b]);
      free(ids); ir_free(&ir);
   }
   struct ir ir; ir_make(&ir, s->ninst, T_INST);
   uint64_t *roots = calloc(s->ninst, sizeof *roots);
   uint32_t *ids = malloc(s->ninst * sizeof *ids);
   for (int k = 0; k < s->ninst; k++) {
      const struct m_inst *in = &s->inst[k];
      const struct m_blas *bl = &s->blas[in->blas];
      roots[k] = (uint64_t)(uintptr_t)out->blas[in->blas];
      float lo[3] = { INFINITY, INFINITY, INFINITY }, hi[3] = { -INFINITY, -INFINITY, -INFINITY };
      for (int i = 0; i < bl->ntri; i++)
         for (int v = 0; v < 3; v++) {
            float w[3];
            for (int r = 0; r < 3; r++) w[r] = in->otw[r][0] * bl->tri[i].v[v][0] + in->otw[r][1] * bl->tri[i].v[v][1] + in->otw[r][2] * bl->tri[i].v[v][2] + in->otw[r][3];
            for (int r = 0; r < 3; r++) { if (w[r] < lo[r]) lo[r] = w[r]; if (w[r] > hi[r]) hi[r] = w[r]; }
         }
      memcpy(ir.leaf_lo[k], lo, 12); memcpy(ir.leaf_hi[k], hi, 12);
      ids[k] = k;
   }
   uint32_t nbox = 0; float lo[3], hi[3];
   uint32_t root = ir_build(&ir, ids, s->ninst, T_INST, &nbox, lo, hi);
   ir_single(&ir, root, &nbox, lo, hi);
   ir.internal_count = nbox;
   struct src_prims sp = { s->ninst, T_INST, NULL, s->inst, roots };
   out->tlas = encode(&ir, &sp, &out->tlas_size);
   free(ids); free(roots); ir_free(&ir);
}

static void free_built(struct built *b) { for (int i = 0; i < 8; i++) free(b->blas[i]); free(b->tlas); }

/* ---- arms ------------------------------------------------------------------------------------------------------------- */

static uint32_t rng_state = 99;
static uint32_t rnd(void) { rng_state = rng_state * 1664525u + 1013904223u; return rng_state >> 8; }
static float frand(float a, float b) { return a + (b - a) * ((rnd() & 0xffff) / 65535.0f); }

static struct m_scene make_scene(void)
{
   struct m_scene s = { 0 };
   s.nblas = 3; s.ninst = 1 + rnd() % 7;
   s.blas = calloc(s.nblas, sizeof *s.blas); s.inst = calloc(s.ninst, sizeof *s.inst);
   for (int b = 0; b < s.nblas; b++) {
      struct m_blas *bl = &s.blas[b];
      bl->ngeom = 1 + rnd() % 3;
      bl->ntri = b == 0 ? 1 : 1 + rnd() % 60;      /* a one-triangle bottom level is the degenerate tree */
      bl->tri = calloc(bl->ntri, sizeof *bl->tri);
      for (int i = 0; i < bl->ntri; i++) {
         float c[3] = { frand(-4, 4), frand(-4, 4), frand(-4, 4) };
         for (int v = 0; v < 3; v++) for (int k = 0; k < 3; k++) bl->tri[i].v[v][k] = c[k] + frand(-1.2f, 1.2f);
         bl->tri[i].geom = i % bl->ngeom; bl->tri[i].prim = i;
      }
   }
   for (int k = 0; k < s.ninst; k++) {
      struct m_inst *in = &s.inst[k];
      in->blas = rnd() % s.nblas;
      float a = frand(0, 6.28f), c = cosf(a), sn = sinf(a), sc = frand(0.6f, 1.6f);
      float m[3][3] = { { c * sc, -sn * sc, 0 }, { sn * sc, c * sc, 0 }, { 0, 0, sc } };
      for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) in->otw[i][j] = m[i][j]; in->otw[i][3] = frand(-2, 2); }
      in->mask = 1 + rnd() % 255; in->sbt = rnd() & 0xffff; in->custom = rnd() & 0xffff; in->flags = rnd() & 0xf;
   }
   scene_finish(&s);
   return s;
}

static void free_scene(struct m_scene *s)
{
   for (int b = 0; b < s->nblas; b++) { free(s->blas[b].tri); free(s->blas[b].box); }
   free(s->blas); free(s->inst);
}

static struct m_ray make_ray(void)
{
   struct m_ray r = { 0 };
   float tgt[3] = { frand(-4, 4), frand(-4, 4), frand(-4, 4) };
   for (int k = 0; k < 3; k++) { r.org[k] = frand(-9, 9); r.dir[k] = tgt[k] - r.org[k]; }
   r.tmin = (rnd() & 3) ? 0.0f : frand(0, 4); r.tmax = (rnd() & 3) ? 1e4f : frand(5, 30);
   r.mask = 1 + rnd() % 255;
   return r;
}

static int same(const struct m_hit *a, const struct m_hit *b)
{
   if (a->type != b->type) return 0;
   if (!a->type) return 1;
   return a->t == b->t && a->u == b->u && a->v == b->v && a->front == b->front && a->inst == b->inst && a->geom == b->geom && a->prim == b->prim;
}

static int fails;
static void arm(const char *name, int ok, const char *detail)
{
   printf("%s %s%s%s\n", ok ? "ok  " : "FAIL", name, ok || !detail[0] ? "" : "  ", ok ? "" : detail);
   fails += !ok;
}

/* damage kinds for the controls */
enum { DMG_NONE, DMG_CHILD_REF, DMG_MASK, DMG_LEAF_OFFSET, DMG_NODE_ORDER };

static int run(int scenes, int rays, int damage, int reloc, int *hits, int *header_ok)
{
   int bad = 0; *hits = 0; *header_ok = 1;
   for (int sc = 0; sc < scenes; sc++) {
      struct m_scene s = make_scene();
      struct built bt; build_scene(&s, &bt);
      for (int b = 0; b < s.nblas; b++) {
         if (!bt.blas[b]) continue;
         uint8_t *h = bt.blas[b];
         uint32_t leaves = get32(h, NVK_HWL_OFF_LEAVES);
         *header_ok &= get32(h, 0) == NVK_HWL_MAGIC_BLAS && get32(h, NVK_HWL_OFF_SIZE) == bt.blas_size[b] &&
                       get64(h, NVK_HWL_OFF_ROOT) == 0x380 && get32(h, NVK_HWL_OFF_PRIMS) == (uint32_t)s.blas[b].ntri &&
                       get64(h, NVK_HWL_OFF_SELF) == (uint64_t)(uintptr_t)h && leaves > 0x380 && leaves < bt.blas_size[b];
      }
      if (damage == DMG_CHILD_REF) { put32(bt.tlas, NVK_HWL_HEADER_SIZE + 24, get32(bt.tlas, NVK_HWL_HEADER_SIZE + 24) ^ 0x80u); }
      if (damage == DMG_MASK) {
         uint32_t lb = get32(bt.tlas, NVK_HWL_OFF_LEAVES);
         for (int k = 0; k < s.ninst; k++) put32(bt.tlas, lb + k * NVK_HWL_INST_LEAF_SIZE + NVK_HWL_INST_MASK, 0);
      }
      if (damage == DMG_LEAF_OFFSET) {
         for (int b = 0; b < s.nblas; b++)
            if (bt.blas[b]) put32(bt.blas[b], get32(bt.blas[b], NVK_HWL_OFF_LEAVES) + 36, 777);   /* first leaf's primitive id */
      }
      if (damage == DMG_NODE_ORDER) {
         for (int b = 0; b < s.nblas; b++)
            if (bt.blas[b] && s.blas[b].ntri > 2) {               /* swap the two children of the root node: the walk still finds everything */
               uint8_t tmp[56]; memcpy(tmp, bt.blas[b] + 0x380, 56); memcpy(bt.blas[b] + 0x380, bt.blas[b] + 0x380 + 56, 56); memcpy(bt.blas[b] + 0x380 + 56, tmp, 56);
            }
      }
      if (reloc) {
         /* copy every structure to a new address; the bottom-level pointers inside the top level are rewritten the way the
          * driver does it: a top-level copy keeps its instances' absolute pointers, so only a bottom-level copy matters here */
         for (int b = 0; b < s.nblas; b++) {
            if (!bt.blas[b]) continue;
            uint8_t *n = aligned_alloc(256, bt.blas_size[b]);
            memcpy(n, bt.blas[b], bt.blas_size[b]);
            if (reloc == 2) relocate(n);
            *header_ok &= get64(n, NVK_HWL_OFF_SELF) == (uint64_t)(uintptr_t)n;
            /* point every instance of this bottom level at the copy */
            uint32_t lb = get32(bt.tlas, NVK_HWL_OFF_LEAVES);
            for (int k = 0; k < s.ninst; k++)
               if (s.inst[k].blas == b) put64(bt.tlas, lb + k * NVK_HWL_INST_LEAF_SIZE + NVK_HWL_INST_ROOT, (uint64_t)(uintptr_t)n);
            free(bt.blas[b]); bt.blas[b] = n;
         }
      }
      for (int i = 0; i < rays; i++) {
         struct m_ray r = make_ray();
         float tmin = r.tmin, tmax = r.tmax;
         struct m_hit ref = hw_closest(&s, &r, tmin, tmax, r.mask);
         struct m_hit got = walk_tlas(bt.tlas, &s, &r, tmin, tmax, r.mask, bt.blas);
         if (ref.type) (*hits)++;
         bad += !same(&ref, &got);
      }
      free_built(&bt); free_scene(&s);
   }
   return bad;
}

int main(void)
{
   char d[160];
   setvbuf(stdout, NULL, _IOLBF, 0);
   int hits, hok;
   int bad = run(40, 400, DMG_NONE, 0, &hits, &hok);
   snprintf(d, sizeof d, "%d of 16000 rays differ", bad);
   arm("the walk over the encoded structures equals the model's hardware primitive: distance, barycentrics, facing, ids, bit for bit", bad == 0, d);
   snprintf(d, sizeof d, "%d hits", hits);
   arm("  that run hit something on at least a tenth of the rays", hits > 1600, d);
   arm("the headers of the encoded bottom levels hold magic, size, root at 0x380, primitive count, own address", hok, "a header word is wrong");

   bad = run(40, 400, DMG_NONE, 2, &hits, &hok);
   snprintf(d, sizeof d, "%d of 16000 rays differ", bad);
   arm("a copied bottom level, relocated, walks the same", bad == 0 && hok, d);
   run(4, 10, DMG_NONE, 1, &hits, &hok);
   arm("control: a copy that is not relocated still carries the old address, and the header check says so", !hok, "not detected");

   bad = run(40, 400, DMG_CHILD_REF, 0, &hits, &hok);
   arm("control: one damaged child reference in the top level is caught", bad > 0, "not caught");
   bad = run(40, 400, DMG_MASK, 0, &hits, &hok);
   arm("control: instance masks written as zero are caught", bad > 0, "not caught");
   bad = run(40, 400, DMG_LEAF_OFFSET, 0, &hits, &hok);
   arm("control: a wrong primitive id in a leaf is caught", bad > 0, "not caught");
   bad = run(40, 400, DMG_NODE_ORDER, 0, &hits, &hok);
   snprintf(d, sizeof d, "%d rays differ", bad);
   arm("order of the two children of a node does not matter (the walk visits both)", bad == 0, d);

   printf("%d arms failed\n", fails);
   return fails != 0;
}
