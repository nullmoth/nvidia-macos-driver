/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// What a MTLResourceID means for the resources built in NVMTL4Extras.m, and the GPU-visible tables behind them.
// A shader receives a resource id as a 64-bit value in an argument buffer or an argument table.
//
//  texture view pool   id of view i = baseResourceID + i = (1 << 62) | (first bindless slot + i). This is the same form
//                      as any texture's gpuResourceID: the low 32 bits index the bindless texture heap, so a shader
//                      resolves it exactly as it resolves a texture id. Nothing extra is needed on the shader side.
//  tensor              id = NVMTL_RESID_TENSOR | 1-based index into the tensor table (below). Entry (id - 1) holds the tensor's addresses,
//                      shape and strides. Written once, when the id is first asked for.
//  depth stencil state id = 1-based index into the depth stencil table (below), written when the id is first asked for.
//
// Both tables are host-visible buffers that live as long as the process. Their GPU addresses come from
// nvmtl4_tensor_table_address() and nvmtl4_ds_table_address() (0 when the table could not be made). An id past the
// table capacity is still valid on the host side (nvmtl4_tensor_for_resource_id) but has no entry: it logs once.

#import <Foundation/Foundation.h>

#define NVMTL_TENSOR_TABLE_CAPACITY 16384u
// Tensor ids carry this bit so they never equal a texture, sampler, heap or pipeline id (bits 63..61 and the plain
// small ids). The table index is the id with the bit cleared.
#define NVMTL_RESID_TENSOR (1ull << 60)
#define NVMTL_DS_TABLE_CAPACITY 4096u

// One tensor. Dimension 0 is the innermost; dims and strides are in elements. Element i of dimension k sits at
// bit offset (sum over k of index[k] * strides[k]) * element_bits from data_addr, packed low bits first for sub-byte
// types. The scales plane (rank >= 1, block formats on macOS 27) is the same shape over scale_dims / scale_strides with
// 8-bit elements, and one scale covers block_factor0 data elements along dimension 0.
typedef struct {
    uint64_t data_addr;        // GPU address of element (0, ..., 0) of the data plane
    uint64_t scales_addr;      // GPU address of scale (0, ..., 0), 0 when there is no scales plane
    uint32_t rank;             // 0..16
    uint32_t dtype;            // MTLTensorDataType of the data plane
    uint32_t element_bits;     // 2, 4, 8, 16 or 32
    uint32_t block_factor0;    // elements per scale along dimension 0, 0 when there is no scales plane
    int32_t dims[16];
    int32_t strides[16];
    int32_t scale_dims[16];
    int32_t scale_strides[16];
} nvmtl_tensor_entry;

// One depth stencil state. compare is an MTLCompareFunction value. front[] and back[] hold, in order: stencil compare
// function, stencil fail operation, depth fail operation, depth pass operation, read mask, write mask (MTLStencilOperation
// values for the three operations).
typedef struct {
    uint32_t depth_write;
    uint32_t depth_compare;
    uint32_t stencil_enabled;
    uint32_t reserved;
    uint32_t front[6];
    uint32_t back[6];
} nvmtl_ds_entry;

#ifdef __cplusplus
extern "C" {
#endif
uint64_t nvmtl4_tensor_table_address(void);
uint64_t nvmtl4_ds_table_address(void);
#ifdef __cplusplus
}
#endif
