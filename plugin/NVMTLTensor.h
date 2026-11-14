/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// The tensor object other parts of the driver (machine learning encoder, Metal 4 tensor copies, argument tables) read.
// Dimension 0 is the innermost; strides are in elements. The data lives in `_buf` at byte `_off`; a tensor made by the
// device owns a shared buffer (`_wrapped` NO, `_off` 0, packed strides), one made over a caller's buffer shares it.

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

// These types exist on macOS 26 and later; only code holding a macOS 26 object reaches them (see NVMTL4Extras.m).
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"

// Layout of one plane of a tensor. Dimension 0 is the innermost; strides are in elements.
typedef struct {
    NSUInteger rank;
    NSInteger d[MTL_TENSOR_MAX_RANK], s[MTL_TENSOR_MAX_RANK];   // dimensions and strides in elements
    size_t es;                                                  // bytes per element, 0 for a sub-byte element
    unsigned ebits;                                             // bits per element: 2, 4, 8, 16 or 32 (sub-byte types are packed low bits first)
    size_t packedBytes;                                         // all elements, densely packed
    size_t needBytes;                                           // bytes from the first element to past the last one addressed
} nvmtl4_tensor_layout;

// The data plane's layout is `_L`. A tensor with a scales plane (macOS 27, block-scaled formats) also has `_S`: one
// 8-bit scale per `_bf0` elements along dimension 0, held in `_sbuf` at `_soff` (a device-made tensor keeps both
// planes in `_buf`, scales after the data, and `_sbuf` is that same buffer).
@interface NVMTLTensor : NSObject <MTLTensor> {
    @public id<MTLDevice> _dev; id<MTLBuffer> _buf; NSUInteger _off; BOOL _wrapped;
    nvmtl4_tensor_layout _L; MTLTensorDataType _dt; MTLTensorUsage _usage;
    MTLStorageMode _storage; MTLCPUCacheMode _cache; MTLHazardTrackingMode _hazard;
    NSString *_label; uint64_t _rid;
    BOOL _hasScales; nvmtl4_tensor_layout _S; NSInteger _bf0; MTLTensorDataType _sdt; id<MTLBuffer> _sbuf; NSUInteger _soff;
}
@end

// Tensor arguments in function reflection (MTLTensorBinding). The translator's reflection JSON lists one entry of kind
// "Tensor" per tensor parameter, next to the "Buffer", "Texture" and "Sampler" entries:
//   { "kind": "Tensor", "param_index": N, "metal_index": I, "access": "read"|"write"|"read_write",
//     "data_type": "float"|"half"|"bfloat"|"char"|"uchar"|"short"|"ushort"|"int"|"uint",
//     "index_type": "int"|"uint"|"short"|"ushort"|"long"|"ulong",
//     "rank": R, "dimensions": [e0, e1, ...] }
// "dimensions" is optional (a dynamic shape); an entry of -1 is a dynamic extent. The Metal name comes from the AIR names
// as for other arguments ("tensor:<index>").
// The scales plane as macOS 27's MTLTensorAuxiliaryPlane.
@interface NVMTLTensorAuxPlane : NSObject { @public NVMTLTensor *_t; } @end

#pragma clang diagnostic pop
