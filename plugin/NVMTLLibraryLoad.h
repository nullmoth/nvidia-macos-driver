// Classes and helpers of NVMTLLibraryLoad.m (function tables, intersection tables, mesh pipelines) that the primary
// implementations of the pipeline, encoder and device classes use, declared before them.
#pragma once
@interface NVMTLFunctionHandle : NSObject <MTLFunctionHandle> { @public uint64_t _id; id<MTLFunction> _fn; } @end
@interface NVMTLVisibleFunctionTable : NSObject <MTLVisibleFunctionTable> { @public id<MTLBuffer> _buf; NSUInteger _n; } @end
#define NVMTL_IFT_BUFFERS 31
#define NVMTL_IFT_OPAQUE 0x8000000000000001ull
@interface NVMTLIntersectionFunctionTable : NSObject <MTLIntersectionFunctionTable> { @public id<MTLBuffer> _buf; NSUInteger _n; } @end
@interface NVMTLMeshInfo : NSObject { @public NVMTLComputePipelineState *_k, *_o, *_ki; NVMTLRenderPipelineState *_psi;
    uint32_t _lay[9], _layi[9], _cap, _capi; NSString *_name, *_indWhy;
    NSUInteger _tgMesh, _tgObj, _gridAsk;
} @end
@interface NVMTLMeshBinds : NSObject { @public NSMutableDictionary *_obj, *_mesh; } @end
// the translator entry points the mesh pipeline calls, resolved at load in NVMTLLibraryLoad.m
typedef void (*nvmtl_free_fn)(uint8_t *p, size_t len);
typedef int (*nvmtl_lower_mesh2_fn)(const char *, const char *, const char *, const char *, uint32_t, uint32_t,
    uint8_t **, size_t *, uint8_t **, size_t *, uint8_t **, size_t *, uint32_t *, char *, size_t);
static nvmtl_lower_mesh2_fn xlate_lower_mesh2;
static nvmtl_free_fn xlate_free;
// associated-object keys
static char kNVMTLTableFns;
static char kNVMTLRenderVert, kNVMTLRenderFrag;
static char kNVMTLMeshInfo;
static char kNVMTLMeshBinds;
// defined in NVMTLLibraryLoad.m
static int nvmtl_xlate_ready(void);
static id<MTLIntersectionFunctionTable> nvmtl_ift_new(MTLIntersectionFunctionTableDescriptor *d, const char *what);
static id<MTLVisibleFunctionTable> nvmtl_vft_new(MTLVisibleFunctionTableDescriptor *d, const char *what);
static NSArray *nvmtl_lowered_stage_arguments(NVMTLComputePipelineState *kp, NVMTLFunction *stageFn);
static id nvmtl_mesh_fail(NSError **err, NSString *why);
static NSData *nvmtl_mesh_translate(NSString *ll, NSString *stage, NSString **why);
static NSData *nvmtl_mesh_translate_fc(NSString *ll, NSString *stage, NSDictionary *fc, NSString **why);
static NSString *nvmtl_mesh_text(uint8_t *p, size_t n);
