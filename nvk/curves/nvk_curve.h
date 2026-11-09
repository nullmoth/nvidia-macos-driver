/*
 * nvk_curve.h - curve geometry in an acceleration structure (docs/CURVES.md). Shared by C and GLSL.
 *
 * A curve geometry is built as a box geometry: bvh/curve_aabb.comp writes one box per curve segment into scratch, the shared
 * builder builds the tree over them, and the ray-query lowering intersects the curve itself when a box belongs to a geometry
 * with a table entry (nvk_curve_entry). The table sits in the structure's appendix (nvk_hwrt.h); the appendix exists in
 * the curve-only case too (generation 0, no hardware structure).
 */
#ifndef NVK_CURVE_H
#define NVK_CURVE_H

#ifdef VULKAN
#include "vk_bvh_helpers.h"
#else
#include <stdint.h>
#include "bvh/vk_bvh_defines.h"
#endif

#define NVK_CURVE_ENTRY_MAGIC 0x56524355u   /* "UCRV" */

/* flags word */
#define NVK_CURVE_BASIS_SHIFT   0u    /* 2 bits: 0 B-spline, 1 Catmull-Rom, 2 linear, 3 Bezier (MTLCurveBasis) */
#define NVK_CURVE_COUNT_SHIFT   2u    /* 3 bits: control points per segment, 2..4 */
#define NVK_CURVE_FLAT_SHIFT    5u    /* 1 bit: 1 flat, 0 round (MTLCurveType: round 0, flat 1) */
#define NVK_CURVE_CAPS_SHIFT    6u    /* 2 bits: 0 none, 1 disk, 2 sphere (MTLCurveEndCaps) */
#define NVK_CURVE_CPFMT_SHIFT   8u    /* 1 bit: 0 float3, 1 half3 */
#define NVK_CURVE_RADFMT_SHIFT  9u    /* 1 bit: 0 float, 1 half */
#define NVK_CURVE_IDXTYPE_SHIFT 10u   /* 1 bit: 0 uint16, 1 uint32 */

struct nvk_curve_entry {
   uint64_t control_points;
   uint64_t radii;
   uint64_t indices;
   uint32_t control_point_stride;
   uint32_t radius_stride;
   uint32_t flags;
   uint32_t magic;               /* NVK_CURVE_ENTRY_MAGIC when the geometry is a curve geometry */
   uint32_t segment_count;
   uint32_t padding;
};

/* The build-time description a client chains to VkAccelerationStructureGeometryAabbsDataKHR (the aabbs data address and stride
 * are then ignored: the driver supplies the boxes). */
#define VK_STRUCTURE_TYPE_NVK_CURVE_GEOMETRY_MESA ((VkStructureType)1000999101)
#ifndef VULKAN
#include <vulkan/vulkan_core.h>
typedef struct VkNvkCurveGeometryMESA {
   VkStructureType sType;
   const void *pNext;
   VkDeviceAddress controlPoints;
   VkDeviceAddress radii;
   VkDeviceAddress indices;
   uint32_t controlPointStride;
   uint32_t radiusStride;
   uint32_t flags;               /* the flags word above */
   uint32_t segmentCount;
} VkNvkCurveGeometryMESA;
#endif

/* box bytes the driver needs in scratch for one curve geometry */
#define NVK_CURVE_BOX_BYTES 24u

#ifdef VULKAN
TYPE(nvk_curve_entry, 8);
#endif
#endif
