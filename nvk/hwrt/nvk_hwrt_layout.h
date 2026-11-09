/*
 * nvk_hwrt_layout.h - the hardware structure the RT cores walk, layout generation NVK_HWRT_LAYOUT_PROVISIONAL.
 *
 * Every row of docs/HWRT.md "Layout table" says whether a field here was MEASURED on the card or is PROVISIONAL (written by
 * our own rule until the measurement procedure in docs/HWRT.md measures it). Shared between C and GLSL.
 */
#ifndef NVK_HWRT_LAYOUT_H
#define NVK_HWRT_LAYOUT_H

/* header words (offsets from the hardware structure base) */
#define NVK_HWL_MAGIC_BLAS     0x00100003u   /* MEASURED: word 0 of a bottom-level structure */
#define NVK_HWL_MAGIC_TLAS     0x00100004u   /* PROVISIONAL */
#define NVK_HWL_BUILD_FLAGS    0x00000007u   /* MEASURED for a prefer-fast-trace build */
#define NVK_HWL_OFF_FLAGS      0x004u        /* MEASURED */
#define NVK_HWL_OFF_SIZE       0x008u        /* MEASURED: total size in bytes */
#define NVK_HWL_OFF_LEAVES     0x010u        /* MEASURED: offset of the geometry section */
#define NVK_HWL_OFF_ROOT       0x020u        /* MEASURED: u64, offset of the root node from the base */
#define NVK_HWL_OFF_SENTINEL   0x040u        /* MEASURED: u64 all ones */
#define NVK_HWL_OFF_NODES      0x048u        /* MEASURED: u32, 0x180 in every recorded build */
#define NVK_HWL_OFF_GEOMS      0x058u        /* MEASURED: u32, geometry count */
#define NVK_HWL_OFF_PRIMS      0x060u        /* MEASURED: u32, primitive count (1, 2, 4, 8, 16, 32, 64 in the recorded builds) */
#define NVK_HWL_OFF_SELF       0x0d0u        /* MEASURED: u64, the structure's own device address */
#define NVK_HWL_NODES_VALUE    0x180u

#define NVK_HWL_HEADER_SIZE    0x380u        /* MEASURED: the root node is at 0x380 in every recorded build; header and frame precede it */
#define NVK_HWL_NODE_SIZE      128u          /* MEASURED: node stride */
#define NVK_HWL_TRI_LEAF_SIZE  64u           /* PROVISIONAL */
#define NVK_HWL_BOX_LEAF_SIZE  64u           /* PROVISIONAL */
#define NVK_HWL_INST_LEAF_SIZE 128u          /* PROVISIONAL */

/* child reference: byte offset from the structure base, node type in the low two bits (0 triangle, 1 node, 2 instance, 3 box) */

/* instance leaf: 12 floats of object-to-world rows, the bottom-level root address, mask, instance index */
#define NVK_HWL_INST_XFORM     0u
#define NVK_HWL_INST_ROOT      48u
#define NVK_HWL_INST_MASK      56u
#define NVK_HWL_INST_INDEX     60u
#define NVK_HWL_INST_SBT       64u
#define NVK_HWL_INST_CUSTOM    68u
#define NVK_HWL_INST_FLAGS     72u

/* upper bound on the hardware structure for a build, in bytes (before alignment), used for the structure's size */
#define NVK_HWL_NODES_FOR(leaves)  ((leaves) > 2u ? (leaves) - 1u : 1u)

#endif
