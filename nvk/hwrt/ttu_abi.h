/* The hardware ray query's request and result words, as measured on the RTX 5060.
 *
 * One query is five 16-byte request blocks in (ttu_query_nv inputs 0..4) and sixteen result words out. The layout below
 * was read from 32-ray runs against hardware-layout acceleration structures on the card, and every field is
 * checked against recorded device output by nvk/hwrt/check_ttu_abi.py.
 *
 * Scope: one closest hit over opaque triangles through a top-level structure or a bottom-level structure. Candidate
 * reporting for non-opaque geometry, ray flags other than the instance mask, and procedural primitives are NOT decoded
 * here; a caller that needs them must not claim this path.
 */
#ifndef NVK_TTU_ABI_H
#define NVK_TTU_ABI_H
#include <stdint.h>
#include <string.h>

/* request blocks 0 and 3 carry a fixed tag in words 0..1 and the 8-bit instance mask in bits 23:16 of word 2 */
#define TTU_TAG0_W0 0xff030207u
#define TTU_TAG0_W1 0x50455400u
#define TTU_TAG3_W0 0xff030407u
#define TTU_TAG3_W1 0x50400400u
/* high word of the root pointer: bits 31:28 select the structure kind (top level / bottom level) */
#define TTU_ROOT_HI_TLAS 0x10000000u
#define TTU_ROOT_HI_BLAS 0x14000000u

struct ttu_request {
   uint32_t w[20];   /* 5 blocks x 4 words, in ttust order */
};

static inline uint32_t ttu_f2u(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }
static inline float ttu_u2f(uint32_t u) { float f; memcpy(&f, &u, 4); return f; }

/* root: device address of the root node (the structure's base plus the root offset stored at +0x20) */
static inline void ttu_pack(struct ttu_request *r, const float org[3], const float dir[3], float tmin, float tmax,
                            uint8_t mask, uint64_t root, int top_level)
{
   memset(r, 0, sizeof *r);
   r->w[0] = TTU_TAG0_W0; r->w[1] = TTU_TAG0_W1; r->w[2] = (uint32_t)mask << 16;
   for (int i = 0; i < 3; i++) { r->w[4 + i] = ttu_f2u(org[i]); r->w[8 + i] = ttu_f2u(dir[i]); }
   r->w[7] = ttu_f2u(tmin); r->w[11] = ttu_f2u(tmax);
   r->w[12] = TTU_TAG3_W0; r->w[13] = TTU_TAG3_W1; r->w[14] = (uint32_t)mask << 16;
   r->w[16] = (uint32_t)root;
   r->w[17] = (uint32_t)(root >> 32) | (top_level ? TTU_ROOT_HI_TLAS : TTU_ROOT_HI_BLAS);
}

struct ttu_hit {
   int hit;
   float t, u, v;          /* distance and the two barycentric coordinates the hardware reports */
   int front_face;         /* the sign bit of word 14 is set for a hit on the back (clockwise) side of the triangle */
   uint32_t primitive;     /* index inside its geometry */
   uint32_t geometry;      /* geometry index inside its structure */
   int32_t instance;       /* instance index; every recorded scene has one instance (0), and every recorded query started at a
                              top-level root, so a query started at a bottom-level root is not covered */
};

/* r[0..15] are result words 8..23 of the 24-word record the proof stores per lane (the 16 words ttuld returns). */
static inline struct ttu_hit ttu_decode(const uint32_t r[16])
{
   struct ttu_hit h = { 0 };
   /* a miss leaves the last word all ones; a committed opaque triangle leaves its sign bit clear */
   h.hit = (r[15] & 0x80000000u) == 0;
   if (!h.hit) return h;
   h.front_face = (r[14] >> 31) == 0;
   h.t = ttu_u2f(r[13] & 0x7fffffffu);
   h.u = ttu_u2f(r[14] & 0x7fffffffu);
   h.v = ttu_u2f(r[15] & 0x7fffffffu);
   /* word 12 = [kind:3][...][geometry:(28-4k) high][primitive:(28-4k) low] where k = word>>29 */
   const unsigned k = r[12] >> 29, pbits = 28 - 4 * k;
   h.primitive = r[12] & ((1u << pbits) - 1);
   h.geometry = (r[12] & 0x0fffffffu) >> pbits;
   h.instance = (int32_t)r[5] - 1;
   return h;
}
#endif
