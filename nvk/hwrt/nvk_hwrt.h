/*
 * nvk_hwrt.h - the hardware ray-traversal part of an acceleration structure (docs/HWRT.md in the driver repository).
 *
 * With NVK_HWRT_QUERY set, every acceleration structure carries a second part after the software BVH: a 32-byte appendix
 * (this header), a geometry table, and the structure the RT cores walk. The software BVH is still built first and stays
 * the fallback; a structure without the appendix (nvk_bvh_header.hw_offset == 0) is walked in software.
 *
 *   AS base +0                the software BVH (nvk_bvh.h)
 *   AS base +hw_offset        struct nvk_hwrt_appendix                      (256-byte aligned)
 *   appendix +geometry_table_offset   struct nvk_hwrt_geometry[geometry_count]  (bottom level only)
 *   appendix +NVK_HWRT_STRUCT_OFFSET  the hardware structure                (hardware-defined, nvk/hwrt/hwrt_layout.h)
 *
 * Shared between C (the driver, the NIR lowering) and GLSL (bvh/encode_ttu.comp).
 */
#ifndef NVK_HWRT_H
#define NVK_HWRT_H

#ifdef VULKAN
#include "vk_bvh_helpers.h"
#else
#include <stdint.h>
#include "bvh/vk_bvh_defines.h"
#endif

#define NVK_HWRT_APPENDIX_MAGIC 0x54525748u   /* "HWRT" */
#define NVK_HWRT_ALIGN          256u
#define NVK_HWRT_APPENDIX_SIZE  32u
#define NVK_HWRT_STRUCT_OFFSET  256u          /* the hardware structure starts here, relative to the appendix */

/* The layout generation the encoder writes. 1 is the provisional layout: every field the hardware reads that has not been
 * measured yet is written by our own rule, and the lowering refuses to use a structure of a generation it does not know.
 * The measurement procedure in docs/HWRT.md replaces a generation only after every row of its table passes. */
#define NVK_HWRT_LAYOUT_PROVISIONAL 1u

struct nvk_hwrt_appendix {
   uint32_t magic;
   uint32_t generation;              /* NVK_HWRT_LAYOUT_* */
   uint32_t structure_size;          /* bytes of the hardware structure */
   uint32_t geometry_table_offset;   /* from the appendix; 0 for a top-level structure */
   uint32_t geometry_count;
   /* top level: 1 when any instance's bottom level holds boxes, so the software box pass is needed; bottom level: 1 when it
    * holds boxes (bit 0). Top level bit 1: some instance's bottom level has no hardware part, so the hardware cannot see all
    * of the scene and the whole structure is walked in software. The bits are formed by the encoder from the bottom levels'
    * own words. */
   uint32_t has_procedural;
   uint32_t curve_table_offset;     /* from the appendix: struct nvk_curve_entry[curve_geometry_count] (nvk_curve.h); 0 = none */
   uint32_t curve_geometry_count;
};

/* One entry per geometry of a bottom-level structure, read by the lowering for every hit the hardware reports. */
struct nvk_hwrt_geometry {
   uint32_t geometry_id_and_flags;   /* exactly the word the software triangle leaves of this geometry carry */
   uint32_t first_leaf;              /* index of the geometry's first primitive among the software leaves */
};

struct nvk_hwrt_word32 { uint32_t v; };
struct nvk_hwrt_word64 { uint64_t v; };

#ifdef VULKAN
TYPE(nvk_hwrt_appendix, 4);
TYPE(nvk_hwrt_geometry, 4);
TYPE(nvk_hwrt_word32, 4);
TYPE(nvk_hwrt_word64, 8);
#endif

#endif /* NVK_HWRT_H */
