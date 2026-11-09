/* CPU model of Vulkan / Metal ray-query semantics and of the hybrid query the NVK lowering emits.
 *
 * The hardware primitive (ttu_abi.h) answers one question: the nearest triangle hit in [tmin, tmax] for an instance mask,
 * every triangle treated as opaque, no culling, no flags. Everything else a ray query must do (ray flags, non-opaque
 * candidates, procedural boxes, instance and geometry flags) is built around that one primitive:
 *
 *   phase 0  triangles: ask the primitive for the nearest hit, classify it with the ray/instance/geometry flags, then either
 *            commit it, hand it to the shader as a candidate, or discard it; the next ask starts just past it.
 *   phase 1  boxes: a software walk with SkipTriangles forced on, reporting every box as a candidate.
 *
 * This file is the executable statement of that algorithm. nvk_nir_lower_ray_queries.c (ttu-rq-full.patch) is a line by line
 * translation of hybrid_proceed(), and hwrt_model_test.c checks it against spec_query(), a brute-force enumeration of every
 * primitive written straight from the rules in the Vulkan ray-query chapter, over random scenes and random ray flags.
 */
#ifndef HWRT_MODEL_H
#define HWRT_MODEL_H
#include <stdint.h>

enum {
   RF_OPAQUE = 0x1, RF_NO_OPAQUE = 0x2, RF_TERMINATE_FIRST = 0x4, RF_CULL_BACK = 0x10, RF_CULL_FRONT = 0x20,
   RF_CULL_OPAQUE = 0x40, RF_CULL_NO_OPAQUE = 0x80, RF_SKIP_TRIANGLES = 0x100, RF_SKIP_AABBS = 0x200,
};
enum { GF_OPAQUE = 0x1 };                                               /* VkGeometryFlagBitsKHR */
enum { IF_CULL_DISABLE = 0x1, IF_FLIP = 0x2, IF_FORCE_OPAQUE = 0x4, IF_FORCE_NO_OPAQUE = 0x8 }; /* VkGeometryInstanceFlagBitsKHR */

struct m_tri { float v[3][3]; uint32_t geom, prim; };
struct m_box { float lo[3], hi[3]; uint32_t geom, prim; };
struct m_blas {
   int ntri, nbox, ngeom;
   struct m_tri *tri;
   struct m_box *box;
   uint32_t geom_flags[8];
};
struct m_inst {
   int blas;
   float otw[3][4];     /* object to world, rows of (a b c t) */
   float wto[3][4];     /* world to object */
   uint32_t mask, flags, sbt, custom;
};
struct m_scene {
   int nblas, ninst;
   struct m_blas *blas;
   struct m_inst *inst;
};

struct m_ray { float org[3], dir[3], tmin, tmax; uint32_t flags, mask; };

enum { HIT_NONE = 0, HIT_TRIANGLE = 1, HIT_BOX = 2 };
struct m_hit {
   int type;
   float t, u, v;
   int front;
   uint32_t inst, geom, prim;
   int opaque;
};

/* shader policy: whether the shader confirms a non-opaque triangle, and whether (and where) it generates a box hit */
struct m_policy { uint32_t seed; };
int policy_confirm_tri(const struct m_policy *p, uint32_t inst, uint32_t geom, uint32_t prim);
int policy_generate_box(const struct m_policy *p, uint32_t inst, uint32_t geom, uint32_t prim, float tentry, float *tgen);

void scene_finish(struct m_scene *s);   /* fills wto from otw */

/* The reference: every primitive tested, every rule applied. Returns the committed hit (type NONE for a miss); *nvisited
 * counts candidates a shader would see in a legal traversal that confirms nothing (an upper bound on what is visited). */
struct m_hit spec_query(const struct m_scene *s, const struct m_ray *r, const struct m_policy *p, int *nvalid,
                        struct m_hit *valid, int max_valid);

/* The hardware primitive over the same scene. */
struct m_hit hw_closest(const struct m_scene *s, const struct m_ray *r, float tmin, float tmax, uint32_t mask);

/* The hybrid query. mutation selects a deliberately broken variant for the negative controls (0 = the real algorithm). */
enum { MUT_NONE = 0, MUT_NO_ADVANCE = 1, MUT_IGNORE_CULL = 2, MUT_NO_BOXES = 3, MUT_IGNORE_INSTANCE_FLAGS = 4, MUT_PREVIOUS_SCOPE = 5 };
/* MUT_PREVIOUS_SCOPE is the first lowering (hwrt-28): one query, the nearest triangle committed whatever its flags, no boxes. */
struct m_hit hybrid_query(const struct m_scene *s, const struct m_ray *r, const struct m_policy *p, int mutation,
                          int *iterations);

#endif
