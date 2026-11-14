/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Metal 4 compiler side: MTL4Compiler, MTL4CompilerTask, MTL4Archive, MTL4PipelineDataSetSerializer, MTL4BinaryFunction.
//
// Every entry point converts its Metal 4 descriptor into the Metal 3 descriptor the driver already compiles
// (function descriptors become id<MTLFunction> through the library's own newFunctionWithDescriptor:, stitched ones
// through the stitched-library path), then calls the existing device path. The pipeline states returned are the
// driver's own, so the encoders treat them exactly like pipelines made by the Metal 3 calls.
//
// Archives and serializers use the data forms of nvmtl4_codec.h: the pipelines script is the JSON Apple's tools read,
// the archive file is a property list holding the libraries (as metallibs, whose AIR the translator reads) and the
// pipeline descriptors. An archive written by Apple's toolchain is read through its air64 slices.
//
// Imported by NVMTLDevice.m after NVMTLLibraryLoad.m (it uses the function-table and binary archive code there).

#import <Metal/Metal.h>
#import <objc/runtime.h>
#import "nvmtl4_codec.h"

// Machine learning pipelines are built in NVMTL4ML.m.
id nvmtl4ml_new_pipeline(id descriptor, id<MTLDevice> dev, NSError **err);

// Fields of a Metal 4 request that the Metal 3 path cannot express are reported here, once per field.
static id nvmtl4_fail(NSError **err, NSString *why) {
    return nvmtl_fail(err, [NSString stringWithFormat:@"MTL4 compiler: %@", why]);
}
static void nvmtl4_note(const char *field, NSString *detail) {
    static NSMutableSet *said; static dispatch_once_t once;
    dispatch_once(&once, ^{ said = [NSMutableSet set]; });
    NSString *key = [NSString stringWithUTF8String:field];
    @synchronized (said) { if ([said containsObject:key]) return; [said addObject:key]; }
    nvlog("MTL4 compiler: descriptor field %s: %s", field, detail.UTF8String ?: "");
}

#pragma mark - what a pipeline state remembers about the Metal 4 request that made it

static char kNV4Info;

NV4_AVAIL @interface NV4StateInfo : NSObject
@property (nonatomic, copy) MTL4PipelineDescriptor *desc;
@property (nonatomic, copy) MTL4PipelineStageDynamicLinkingDescriptor *stageLinking;
@property (nonatomic, copy) MTL4RenderPipelineDynamicLinkingDescriptor *renderLinking;
@property (nonatomic, strong) id reflection;
@property (nonatomic) BOOL unspecialized;
@property (nonatomic) BOOL inheritsColorMapping;
@end
NV4_AVAIL @implementation NV4StateInfo @end

NV4_AVAIL static NV4StateInfo *nvmtl4_info(id state) { return objc_getAssociatedObject(state, &kNV4Info); }

// A pipeline made through the Metal 4 compiler told to inherit the logical-to-physical colour attachment map: the
// Metal 4 render encoder asks this when it binds the pipeline.
BOOL nvmtl4_pipeline_inherits_color_mapping(id state) {
    if (@available(macos 26.0, *)) return nvmtl4_info(state).inheritsColorMapping;
    return NO;
}

#pragma mark - binary functions

// A binary function here is a function of a library that pipelines link by handle: the driver links functions at the
// AIR level when it builds a pipeline, so the "binary" is the translated function itself and the pipeline that links
// it gives out the handle.
NV4_AVAIL @interface NVMTL4BinaryFunction : NSObject <MTL4BinaryFunction> { @public id<MTLFunction> _fn; NSString *_name; MTL4BinaryFunctionOptions _options; } @end
NV4_AVAIL @implementation NVMTL4BinaryFunction
- (NSString *)name { return _name; }
- (MTLFunctionType)functionType { return _fn.functionType; }
@end

#pragma mark - function resolution

NV4_AVAIL static id<MTLFunction> nvmtl4_resolve_function(MTL4FunctionDescriptor *fd, NSError **err);

NV4_AVAIL static id<MTLFunction> nvmtl4_resolve_library_function(MTL4LibraryFunctionDescriptor *ld, NSString *specName,
                                                                 MTLFunctionConstantValues *cv, NSError **err) {
    if (!ld.library) return nvmtl4_fail(err, @"library function descriptor has no library");
    if (!ld.name.length) return nvmtl4_fail(err, @"function descriptor has no name");
    MTLFunctionDescriptor *md = [MTLFunctionDescriptor functionDescriptor];
    md.name = ld.name;
    md.specializedName = specName;
    md.constantValues = cv;
    NSError *e = nil;
    id<MTLFunction> f = [ld.library newFunctionWithDescriptor:md error:&e];
    if (!f) {
        nvlog("MTL4 compiler: FAIL function \"%s\" not created: %s", ld.name.UTF8String, e.localizedDescription.UTF8String ?: "no error given");
        if (err) *err = e ?: [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorFunctionNotFound
                              userInfo:@{ NSLocalizedDescriptionKey: [NSString stringWithFormat:@"no function named %@ in the library", ld.name] }];
    }
    return f;
}

// A stitched function descriptor goes through the stitched-library path, which links the graph's functions into one
// function named by the graph.
NV4_AVAIL static id<MTLFunction> nvmtl4_resolve_stitched(MTL4StitchedFunctionDescriptor *sd, NSError **err) {
    if (!sd.functionGraph) return nvmtl4_fail(err, @"stitched function descriptor has no function graph");
    NSMutableArray *fns = [NSMutableArray array];
    for (MTL4FunctionDescriptor *f in sd.functionDescriptors) {
        id<MTLFunction> r = nvmtl4_resolve_function(f, err);
        if (!r) return nil;
        [fns addObject:r];
    }
    MTLStitchedLibraryDescriptor *ld = [MTLStitchedLibraryDescriptor new];
    ld.functionGraphs = @[sd.functionGraph];
    ld.functions = fns;
    NSError *e = nil;
    id<MTLLibrary> lib = [(NVMTLDevice *)gNVMTLMainDevice newLibraryWithStitchedDescriptor:ld error:&e];
    if (!lib) {
        nvlog("MTL4 compiler: FAIL stitched function \"%s\": %s", sd.functionGraph.functionName.UTF8String ?: "?", e.localizedDescription.UTF8String ?: "no error given");
        if (err) *err = e ?: [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorCompileFailure
                              userInfo:@{ NSLocalizedDescriptionKey: @"the function graph did not stitch" }];
        return nil;
    }
    id<MTLFunction> f = [lib newFunctionWithName:sd.functionGraph.functionName];
    if (!f) return nvmtl4_fail(err, [NSString stringWithFormat:@"stitched library has no function \"%@\"", sd.functionGraph.functionName]);
    return f;
}

NV4_AVAIL static id<MTLFunction> nvmtl4_resolve_function(MTL4FunctionDescriptor *fd, NSError **err) {
    if ([fd isKindOfClass:[MTL4LibraryFunctionDescriptor class]])
        return nvmtl4_resolve_library_function((MTL4LibraryFunctionDescriptor *)fd, nil, nil, err);
    if ([fd isKindOfClass:[MTL4SpecializedFunctionDescriptor class]]) {
        MTL4SpecializedFunctionDescriptor *sd = (MTL4SpecializedFunctionDescriptor *)fd;
        if (![sd.functionDescriptor isKindOfClass:[MTL4LibraryFunctionDescriptor class]])
            return nvmtl4_fail(err, @"a specialized function descriptor must wrap a library function");
        return nvmtl4_resolve_library_function((MTL4LibraryFunctionDescriptor *)sd.functionDescriptor,
                                               sd.specializedName, sd.constantValues, err);
    }
    if ([fd isKindOfClass:[MTL4StitchedFunctionDescriptor class]])
        return nvmtl4_resolve_stitched((MTL4StitchedFunctionDescriptor *)fd, err);
    return nvmtl4_fail(err, [NSString stringWithFormat:@"function descriptor class %s", fd ? object_getClassName(fd) : "(nil)"]);
}

NV4_AVAIL static NSArray<id<MTLFunction>> *nvmtl4_resolve_functions(NSArray<MTL4FunctionDescriptor *> *fds, NSError **err) {
    NSMutableArray *out = [NSMutableArray array];
    for (MTL4FunctionDescriptor *fd in fds) {
        id<MTLFunction> f = nvmtl4_resolve_function(fd, err);
        if (!f) return nil;
        [out addObject:f];
    }
    return out;
}

#pragma mark - linking

// Static linking plus the binary functions a dynamic linking descriptor adds. *out is nil when nothing links.
NV4_AVAIL static BOOL nvmtl4_linked_functions(MTL4StaticLinkingDescriptor *sl, MTL4PipelineStageDynamicLinkingDescriptor *dl,
                                              MTLLinkedFunctions **out, NSError **err) {
    *out = nil;
    NSMutableArray *fns = [NSMutableArray array];
    if (sl.functionDescriptors.count) { NSArray *a = nvmtl4_resolve_functions(sl.functionDescriptors, err); if (!a) return NO; [fns addObjectsFromArray:a]; }
    for (id bf in dl.binaryLinkedFunctions) {
        if (![bf isKindOfClass:[NVMTL4BinaryFunction class]]) {
            nvmtl4_fail(err, [NSString stringWithFormat:@"binary function of class %s was not made by this compiler", object_getClassName(bf)]);
            return NO;
        }
        id<MTLFunction> f = ((NVMTL4BinaryFunction *)bf)->_fn;
        if (![fns containsObject:f]) [fns addObject:f];
    }
    if (!fns.count && !sl.privateFunctionDescriptors.count && !sl.groups.count) return YES;
    MTLLinkedFunctions *lf = [MTLLinkedFunctions linkedFunctions];
    if (fns.count) lf.functions = fns;
    if (sl.privateFunctionDescriptors.count) {
        lf.privateFunctions = nvmtl4_resolve_functions(sl.privateFunctionDescriptors, err);
        if (!lf.privateFunctions) return NO;
    }
    if (sl.groups.count) {
        NSMutableDictionary *g = [NSMutableDictionary dictionary];
        for (NSString *k in sl.groups) {
            NSArray *a = nvmtl4_resolve_functions(sl.groups[k], err);
            if (!a) return NO;
            g[k] = a;
        }
        lf.groups = g;
    }
    *out = lf;
    return YES;
}

NV4_AVAIL static void nvmtl4_note_pipeline_options(MTL4PipelineDescriptor *d, MTLShaderValidation *sv) {
    if (d.options) *sv = d.options.shaderValidation;
}
NV4_AVAIL static BOOL nvmtl4_wants_reflection(MTL4PipelineDescriptor *d) {
    return d.options && d.options.shaderReflection != MTL4ShaderReflectionNone;
}

// An attachment whose blend state is left unspecialized builds as "no blending, plain factors"; the specialization
// call supplies the real state.
NV4_AVAIL static BOOL nvmtl4_copy_attachments(MTL4RenderPipelineColorAttachmentDescriptorArray *src,
                                              MTLRenderPipelineColorAttachmentDescriptorArray *dst) {
    BOOL unspecialized = NO;
    for (NSUInteger i = 0; i < 8; i++) {
        MTL4RenderPipelineColorAttachmentDescriptor *s = src[i];
        MTLRenderPipelineColorAttachmentDescriptor *t = dst[i];
        t.pixelFormat = s.pixelFormat;
        t.writeMask = (s.writeMask & MTLColorWriteMaskUnspecialized) ? MTLColorWriteMaskAll : s.writeMask;
        if (s.blendingState == MTL4BlendStateUnspecialized || (s.writeMask & MTLColorWriteMaskUnspecialized)) unspecialized = YES;
        if (s.blendingState == MTL4BlendStateUnspecialized) {
            t.blendingEnabled = NO;
            continue;
        }
        t.blendingEnabled = (s.blendingState == MTL4BlendStateEnabled);
        t.sourceRGBBlendFactor = s.sourceRGBBlendFactor;
        t.destinationRGBBlendFactor = s.destinationRGBBlendFactor;
        t.rgbBlendOperation = s.rgbBlendOperation;
        t.sourceAlphaBlendFactor = s.sourceAlphaBlendFactor;
        t.destinationAlphaBlendFactor = s.destinationAlphaBlendFactor;
        t.alphaBlendOperation = s.alphaBlendOperation;
    }
    return unspecialized;
}

#pragma mark - descriptor conversion

NV4_AVAIL static MTLComputePipelineDescriptor *nvmtl4_compute_descriptor(MTL4ComputePipelineDescriptor *d,
                                                                         MTL4PipelineStageDynamicLinkingDescriptor *dl, NSError **err) {
    if (!d) { nvmtl4_fail(err, @"nil compute pipeline descriptor"); return nil; }
    if (!d.computeFunctionDescriptor) { nvmtl4_fail(err, @"compute pipeline descriptor has no computeFunctionDescriptor"); return nil; }
    id<MTLFunction> fn = nvmtl4_resolve_function(d.computeFunctionDescriptor, err);
    if (!fn) return nil;
    MTLComputePipelineDescriptor *c = [MTLComputePipelineDescriptor new];
    if (d.label) c.label = d.label;   // macOS 27 Metal asserts on a nil label
    c.computeFunction = fn;
    c.threadGroupSizeIsMultipleOfThreadExecutionWidth = d.threadGroupSizeIsMultipleOfThreadExecutionWidth;
    c.maxTotalThreadsPerThreadgroup = d.maxTotalThreadsPerThreadgroup;
    c.requiredThreadsPerThreadgroup = d.requiredThreadsPerThreadgroup;
    c.supportAddingBinaryFunctions = d.supportBinaryLinking;
    c.supportIndirectCommandBuffers = (d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled);
    MTLShaderValidation sv = c.shaderValidation;
    nvmtl4_note_pipeline_options(d, &sv);
    c.shaderValidation = sv;
    MTLLinkedFunctions *lf = nil;
    if (!nvmtl4_linked_functions(d.staticLinkingDescriptor, dl, &lf, err)) return nil;
    c.linkedFunctions = lf;
    if (dl) {
        c.preloadedLibraries = dl.preloadedLibraries ?: @[];
        c.maxCallStackDepth = dl.maxCallStackDepth;
    }
    return c;
}

NV4_AVAIL static MTLRenderPipelineDescriptor *nvmtl4_render_descriptor(MTL4RenderPipelineDescriptor *d,
                                                                       MTL4RenderPipelineDynamicLinkingDescriptor *dl,
                                                                       BOOL *unspecialized, NSError **err) {
    if (!d.vertexFunctionDescriptor) { nvmtl4_fail(err, @"render pipeline descriptor has no vertexFunctionDescriptor"); return nil; }
    id<MTLFunction> vfn = nvmtl4_resolve_function(d.vertexFunctionDescriptor, err);
    if (!vfn) return nil;
    id<MTLFunction> ffn = nil;
    if (d.fragmentFunctionDescriptor) { ffn = nvmtl4_resolve_function(d.fragmentFunctionDescriptor, err); if (!ffn) return nil; }
    MTLRenderPipelineDescriptor *r = [MTLRenderPipelineDescriptor new];
    if (d.label) r.label = d.label;   // macOS 27 Metal asserts on a nil label
    r.vertexFunction = vfn;
    r.fragmentFunction = ffn;
    r.vertexDescriptor = d.vertexDescriptor;
    r.rasterSampleCount = d.rasterSampleCount ?: 1;
    r.alphaToCoverageEnabled = (d.alphaToCoverageState == MTL4AlphaToCoverageStateEnabled);
    r.alphaToOneEnabled = (d.alphaToOneState == MTL4AlphaToOneStateEnabled);
    r.rasterizationEnabled = d.rasterizationEnabled;
    r.maxVertexAmplificationCount = d.maxVertexAmplificationCount ?: 1;
    r.inputPrimitiveTopology = d.inputPrimitiveTopology;
    r.supportIndirectCommandBuffers = (d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled);
    r.supportAddingVertexBinaryFunctions = d.supportVertexBinaryLinking;
    r.supportAddingFragmentBinaryFunctions = d.supportFragmentBinaryLinking;
    *unspecialized = nvmtl4_copy_attachments(d.colorAttachments, r.colorAttachments);
    MTLShaderValidation sv = r.shaderValidation;
    nvmtl4_note_pipeline_options(d, &sv);
    r.shaderValidation = sv;
    MTLLinkedFunctions *lf = nil;
    if (!nvmtl4_linked_functions(d.vertexStaticLinkingDescriptor, dl.vertexLinkingDescriptor, &lf, err)) return nil;
    r.vertexLinkedFunctions = lf;
    if (!nvmtl4_linked_functions(d.fragmentStaticLinkingDescriptor, dl.fragmentLinkingDescriptor, &lf, err)) return nil;
    r.fragmentLinkedFunctions = lf;
    if (dl) {
        r.vertexPreloadedLibraries = dl.vertexLinkingDescriptor.preloadedLibraries ?: @[];
        r.fragmentPreloadedLibraries = dl.fragmentLinkingDescriptor.preloadedLibraries ?: @[];
        r.maxVertexCallStackDepth = dl.vertexLinkingDescriptor.maxCallStackDepth;
        r.maxFragmentCallStackDepth = dl.fragmentLinkingDescriptor.maxCallStackDepth;
    }
    return r;
}

// A mesh pipeline's object and mesh stages are lowered whole by the mesh path, which does not read linked functions
// or preloaded libraries. Link them first (static functions, binary functions, dynamic libraries), then hand the
// linked stage over.
NV4_AVAIL static id<MTLFunction> nvmtl4_link_mesh_stage(id<MTLFunction> fn, MTL4StaticLinkingDescriptor *sl,
                                                        MTL4PipelineStageDynamicLinkingDescriptor *dl, BOOL binaryLinking,
                                                        const char *stage, NSError **err) {
    if (!fn) return nil;
    MTLLinkedFunctions *lf = nil;
    if (!nvmtl4_linked_functions(sl, dl, &lf, err)) return nil;
    NSArray *libs = dl.preloadedLibraries;
    if (!lf && !libs.count) return fn;
    NSError *le = nil;
    id<MTLFunction> linked = nvmtl_link_stage(fn, lf, libs, &le);
    if (!linked) {
        nvlog("MTL4 compiler: FAIL mesh pipeline %s stage did not link: %s", stage, le.localizedDescription.UTF8String ?: "no reason given");
        if (err) *err = le ?: [NSError errorWithDomain:@"NVMTL" code:23 userInfo:@{ NSLocalizedDescriptionKey: @"the mesh stage did not link" }];
        return nil;
    }
    return linked;
}

NV4_AVAIL static MTLMeshRenderPipelineDescriptor *nvmtl4_mesh_descriptor(MTL4MeshRenderPipelineDescriptor *d,
                                                                         MTL4RenderPipelineDynamicLinkingDescriptor *dl,
                                                                         BOOL *unspecialized, NSError **err) {
    if (!d.meshFunctionDescriptor) { nvmtl4_fail(err, @"mesh pipeline descriptor has no meshFunctionDescriptor"); return nil; }
    id<MTLFunction> ofn = nil, ffn = nil;
    if (d.objectFunctionDescriptor) { ofn = nvmtl4_resolve_function(d.objectFunctionDescriptor, err); if (!ofn) return nil; }
    id<MTLFunction> mfn = nvmtl4_resolve_function(d.meshFunctionDescriptor, err);
    if (!mfn) return nil;
    if (d.fragmentFunctionDescriptor) { ffn = nvmtl4_resolve_function(d.fragmentFunctionDescriptor, err); if (!ffn) return nil; }
    if (ofn) { ofn = nvmtl4_link_mesh_stage(ofn, d.objectStaticLinkingDescriptor, dl.objectLinkingDescriptor, d.supportObjectBinaryLinking, "object", err); if (!ofn) return nil; }
    mfn = nvmtl4_link_mesh_stage(mfn, d.meshStaticLinkingDescriptor, dl.meshLinkingDescriptor, d.supportMeshBinaryLinking, "mesh", err);
    if (!mfn) return nil;
    MTLMeshRenderPipelineDescriptor *m = [MTLMeshRenderPipelineDescriptor new];
    if (d.label) m.label = d.label;   // macOS 27 Metal asserts on a nil label
    m.objectFunction = ofn;
    m.meshFunction = mfn;
    m.fragmentFunction = ffn;
    m.maxTotalThreadsPerObjectThreadgroup = d.maxTotalThreadsPerObjectThreadgroup;
    m.maxTotalThreadsPerMeshThreadgroup = d.maxTotalThreadsPerMeshThreadgroup;
    m.requiredThreadsPerObjectThreadgroup = d.requiredThreadsPerObjectThreadgroup;
    m.requiredThreadsPerMeshThreadgroup = d.requiredThreadsPerMeshThreadgroup;
    m.objectThreadgroupSizeIsMultipleOfThreadExecutionWidth = d.objectThreadgroupSizeIsMultipleOfThreadExecutionWidth;
    m.meshThreadgroupSizeIsMultipleOfThreadExecutionWidth = d.meshThreadgroupSizeIsMultipleOfThreadExecutionWidth;
    m.payloadMemoryLength = d.payloadMemoryLength;
    m.maxTotalThreadgroupsPerMeshGrid = d.maxTotalThreadgroupsPerMeshGrid;
    m.rasterSampleCount = d.rasterSampleCount ?: 1;
    m.alphaToCoverageEnabled = (d.alphaToCoverageState == MTL4AlphaToCoverageStateEnabled);
    m.alphaToOneEnabled = (d.alphaToOneState == MTL4AlphaToOneStateEnabled);
    m.rasterizationEnabled = d.rasterizationEnabled;
    m.maxVertexAmplificationCount = d.maxVertexAmplificationCount ?: 1;
    m.supportIndirectCommandBuffers = (d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled);
    *unspecialized = nvmtl4_copy_attachments(d.colorAttachments, m.colorAttachments);
    MTLShaderValidation sv = m.shaderValidation;
    nvmtl4_note_pipeline_options(d, &sv);
    m.shaderValidation = sv;
    MTLLinkedFunctions *lf = nil;
    if (!nvmtl4_linked_functions(d.fragmentStaticLinkingDescriptor, dl.fragmentLinkingDescriptor, &lf, err)) return nil;
    m.fragmentLinkedFunctions = lf;
    return m;
}

// The tile pipeline creation is owned by the tile work; define NVMTL4_TILE_CREATE(device, descriptor, error) to its
// entry point (nvmtl_tile_pipeline_create) when that lands. Until then the request is refused, not faked.
NV4_AVAIL static MTLTileRenderPipelineDescriptor *nvmtl4_tile_descriptor(MTL4TileRenderPipelineDescriptor *d, NSError **err) {
    if (!d.tileFunctionDescriptor) { nvmtl4_fail(err, @"tile pipeline descriptor has no tileFunctionDescriptor"); return nil; }
    id<MTLFunction> fn = nvmtl4_resolve_function(d.tileFunctionDescriptor, err);
    if (!fn) return nil;
    MTLTileRenderPipelineDescriptor *t = [MTLTileRenderPipelineDescriptor new];
    if (d.label) t.label = d.label;   // macOS 27 Metal asserts on a nil label
    t.tileFunction = fn;
    t.rasterSampleCount = d.rasterSampleCount ?: 1;
    t.threadgroupSizeMatchesTileSize = d.threadgroupSizeMatchesTileSize;
    t.maxTotalThreadsPerThreadgroup = d.maxTotalThreadsPerThreadgroup;
    t.requiredThreadsPerThreadgroup = d.requiredThreadsPerThreadgroup;
    t.supportAddingBinaryFunctions = d.supportBinaryLinking;
    for (NSUInteger i = 0; i < 8; i++) t.colorAttachments[i].pixelFormat = d.colorAttachments[i].pixelFormat;
    MTLLinkedFunctions *lf = nil;
    if (!nvmtl4_linked_functions(d.staticLinkingDescriptor, nil, &lf, err)) return nil;
    t.linkedFunctions = lf;
    return t;
}

#pragma mark - compiler task

NV4_AVAIL @interface NVMTL4CompilerTask : NSObject <MTL4CompilerTask>
- (instancetype)initWithCompiler:(id<MTL4Compiler>)compiler work:(void (^)(void))work;
@end

NV4_AVAIL @implementation NVMTL4CompilerTask {
    id<MTL4Compiler> _compiler;
    dispatch_group_t _done;
    _Atomic(NSInteger) _status;
}
- (instancetype)initWithCompiler:(id<MTL4Compiler>)compiler work:(void (^)(void))work {
    if ((self = [super init])) {
        _compiler = compiler;
        _done = dispatch_group_create();
        atomic_store(&_status, MTL4CompilerTaskStatusScheduled);
        dispatch_group_enter(_done);
        // The completion handler runs inside the work block, so waitUntilCompleted returns only after it has run.
        [nvmtl_pipeline_compile_queue() addOperationWithBlock:^{ @autoreleasepool {
            atomic_store(&self->_status, MTL4CompilerTaskStatusCompiling);
            work();
            atomic_store(&self->_status, MTL4CompilerTaskStatusFinished);
            dispatch_group_leave(self->_done);
        }}];
    }
    return self;
}
- (id<MTL4Compiler>)compiler { return _compiler; }
- (MTL4CompilerTaskStatus)status { return (MTL4CompilerTaskStatus)atomic_load(&_status); }
- (void)waitUntilCompleted { dispatch_group_wait(_done, DISPATCH_TIME_FOREVER); }
@end

#pragma mark - libraries as data

NV4_AVAIL static NV4Ctx *nvmtl4_new_ctx(NVMTLDevice *device) {
    NV4Ctx *cx = [NV4Ctx new];
    cx.libraryBytes = ^NSData *(id<MTLLibrary> l) {
        if (![(id)l isKindOfClass:[NVMTLLibrary class]]) return nil;
        NVMTLLibrary *n = (NVMTLLibrary *)l;
        if (n->_raw.length) return n->_raw;
        return n->_rawPath ? [NSData dataWithContentsOfFile:n->_rawPath] : nil;
    };
    cx.libraryPath = ^NSString *(id<MTLLibrary> l) {
        if (![(id)l isKindOfClass:[NVMTLLibrary class]]) return nil;
        NVMTLLibrary *n = (NVMTLLibrary *)l;
        if (n->_rawPath.length) return n->_rawPath;
        NSArray *f = objc_getAssociatedObject(l, &gNVMTLLibFileAssoc);
        return [f.firstObject isKindOfClass:[NSString class]] ? f.firstObject : nil;
    };
    cx.makeLibrary = ^id<MTLLibrary>(NSData *b, NSError **e) {
        dispatch_data_t dd = dispatch_data_create(b.bytes, b.length, dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), DISPATCH_DATA_DESTRUCTOR_DEFAULT);
        return [device newLibraryWithData:dd error:e];
    };
    return cx;
}

#pragma mark - serializer

NV4_AVAIL @interface NVMTL4PipelineDataSetSerializer : NSObject <MTL4PipelineDataSetSerializer>
- (instancetype)initWithDevice:(NVMTLDevice *)device configuration:(MTL4PipelineDataSetSerializerConfiguration)cfg;
- (void)capture:(MTL4PipelineDescriptor *)d;
@end

NV4_AVAIL @implementation NVMTL4PipelineDataSetSerializer {
    MTL4PipelineDataSetSerializerConfiguration _cfg;
    NV4Ctx *_cx;
    NSMutableArray<NSDictionary *> *_entries;
    NSMutableSet<NSString *> *_keys;
    NSString *_firstFailure;
}
- (instancetype)initWithDevice:(NVMTLDevice *)device configuration:(MTL4PipelineDataSetSerializerConfiguration)cfg {
    if ((self = [super init])) {
        _cfg = cfg; _cx = nvmtl4_new_ctx(device); _entries = [NSMutableArray array]; _keys = [NSMutableSet set];
    }
    return self;
}
// Called for every pipeline request, whether or not it compiles: a failed compile is still part of the data set.
- (void)capture:(MTL4PipelineDescriptor *)d {
    if (!_cfg || !d) return;
    NSString *kind = nil; NSError *e = nil;
    NSDictionary *enc = nv4_encode_pipeline(d, _cx, &kind, &e);
    @synchronized (self) {
        if (!enc) { if (!_firstFailure) _firstFailure = e.localizedDescription ?: @"a descriptor could not be recorded"; return; }
        NSString *key = nv4_key(kind, enc);
        if ([_keys containsObject:key]) return;
        [_keys addObject:key];
        [_entries addObject:@{ @"kind": kind, @"key": key, @"descriptor": enc }];
    }
}
- (NSArray *)snapshotError:(NSError **)error {
    @synchronized (self) {
        if (_firstFailure) { nvmtl4_fail(error, [NSString stringWithFormat:@"the data set is incomplete: %@", _firstFailure]); return nil; }
        return [_entries copy];
    }
}
- (NSData *)serializeAsPipelinesScriptWithError:(NSError **)error {
    if (!(_cfg & MTL4PipelineDataSetSerializerConfigurationCaptureDescriptors)) {
        nvmtl4_fail(error, @"this serializer does not capture descriptors (MTL4PipelineDataSetSerializerConfigurationCaptureDescriptors)");
        return nil;
    }
    NSArray *entries = [self snapshotError:error];
    return entries ? nv4_pipelines_script(entries, _cx, error) : nil;
}
- (BOOL)serializeAsArchiveAndFlushToURL:(NSURL *)url error:(NSError **)error {
    if (!(_cfg & MTL4PipelineDataSetSerializerConfigurationCaptureBinaries)) {
        nvmtl4_fail(error, @"this serializer does not capture binaries (MTL4PipelineDataSetSerializerConfigurationCaptureBinaries)");
        return NO;
    }
    NSArray *entries = [self snapshotError:error];
    if (!entries) return NO;
    NSData *blob = nv4_write_container(entries, _cx, error);
    if (!blob) return NO;
    NSError *we = nil;
    if (![blob writeToURL:url options:NSDataWritingAtomic error:&we]) {
        nvmtl4_fail(error, [NSString stringWithFormat:@"cannot write %@: %@", url.path, we.localizedDescription ?: @"?"]);
        if (error && we) *error = we;
        return NO;
    }
    nvlog("MTL4 serializer: wrote %lu pipeline(s), %lu B to %s", (unsigned long)entries.count, (unsigned long)blob.length, url.path.UTF8String);
    return YES;
}
@end

#pragma mark - compiler

NV4_AVAIL @interface NVMTL4Compiler : NSObject <MTL4Compiler>
- (instancetype)initWithDevice:(NVMTLDevice *)device descriptor:(MTL4CompilerDescriptor *)desc;
- (id<MTLComputePipelineState>)buildCompute:(MTL4ComputePipelineDescriptor *)d dynamic:(MTL4PipelineStageDynamicLinkingDescriptor *)dl
                                    capture:(BOOL)capture error:(NSError **)error;
- (id<MTLRenderPipelineState>)buildRender:(MTL4PipelineDescriptor *)d dynamic:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl
                                  capture:(BOOL)capture error:(NSError **)error;
@end

NV4_AVAIL @interface NVMTL4Archive : NSObject <MTL4Archive>
- (instancetype)initWithData:(NSData *)data device:(NVMTLDevice *)device error:(NSError **)error;
// A hit builds the pipeline from the archive's own copy of the data; a miss returns nil with *miss describing it.
- (id)lookup:(MTL4PipelineDescriptor *)d computeLinking:(MTL4PipelineStageDynamicLinkingDescriptor *)cdl
renderLinking:(MTL4RenderPipelineDynamicLinkingDescriptor *)rdl error:(NSError **)error;
@end

NV4_AVAIL @implementation NVMTL4Compiler {
    NVMTLDevice *_device;
    NSString *_label;
    id<MTL4PipelineDataSetSerializer> _serializer;
}
- (instancetype)initWithDevice:(NVMTLDevice *)device descriptor:(MTL4CompilerDescriptor *)desc {
    if ((self = [super init])) {
        _device = device;
        _label = [desc.label copy];
        _serializer = desc.pipelineDataSetSerializer;
    }
    return self;
}
- (id<MTLDevice>)device { return _device; }
- (NSString *)label { return _label; }
- (id<MTL4PipelineDataSetSerializer>)pipelineDataSetSerializer { return _serializer; }

- (void)captureIfWanted:(MTL4PipelineDescriptor *)d {
    if ([(id)_serializer isKindOfClass:[NVMTL4PipelineDataSetSerializer class]]) [(NVMTL4PipelineDataSetSerializer *)_serializer capture:d];
}

// Lookup archives are tried first; a hit is the pipeline built from the archive's data, a miss compiles normally.
- (id)fromArchives:(MTL4CompilerTaskOptions *)o descriptor:(MTL4PipelineDescriptor *)d computeLinking:(MTL4PipelineStageDynamicLinkingDescriptor *)cdl
     renderLinking:(MTL4RenderPipelineDynamicLinkingDescriptor *)rdl {
    for (id a in o.lookupArchives) {
        if (![a isKindOfClass:[NVMTL4Archive class]]) { nvmtl4_note("compilerTaskOptions.lookupArchives", @"an archive not made by this driver is skipped"); continue; }
        NSError *miss = nil;
        id hit = [(NVMTL4Archive *)a lookup:d computeLinking:cdl renderLinking:rdl error:&miss];
        if (hit) { nvlog("MTL4 compiler: lookup archive hit for \"%s\"", d.label.UTF8String ?: "(unlabelled)"); return hit; }
    }
    return nil;
}

- (id<MTLLibrary>)newLibraryWithDescriptor:(MTL4LibraryDescriptor *)d error:(NSError **)error {
    if (!d.source.length) return nvmtl4_fail(error, @"library descriptor has no source");
    id<MTLLibrary> lib = [_device newLibraryWithSource:d.source options:d.options error:error];
    if (lib && d.name.length) lib.label = d.name;
    return lib;
}
- (id<MTLDynamicLibrary>)newDynamicLibrary:(id<MTLLibrary>)library error:(NSError **)error {
    return [_device newDynamicLibrary:library error:error];
}
- (id<MTLDynamicLibrary>)newDynamicLibraryWithURL:(NSURL *)url error:(NSError **)error {
    return [_device newDynamicLibraryWithURL:url error:error];
}

- (id<MTLComputePipelineState>)buildCompute:(MTL4ComputePipelineDescriptor *)d dynamic:(MTL4PipelineStageDynamicLinkingDescriptor *)dl
                                    capture:(BOOL)capture error:(NSError **)error {
    if (capture) [self captureIfWanted:d];
    MTLComputePipelineDescriptor *c = nvmtl4_compute_descriptor(d, dl, error);
    if (!c) return nil;
    MTLComputePipelineReflection *rf = nil;
    id<MTLComputePipelineState> ps = [_device newComputePipelineStateWithDescriptor:c options:MTLPipelineOptionNone reflection:&rf error:error];
    if (ps) {
        NV4StateInfo *info = [NV4StateInfo new];
        info.desc = d; info.stageLinking = dl;
        if (nvmtl4_wants_reflection(d)) info.reflection = rf;
        objc_setAssociatedObject(ps, &kNV4Info, info, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    }
    return ps;
}

- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d
                                                 compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)error {
    return [self newComputePipelineStateWithDescriptor:d dynamicLinkingDescriptor:nil compilerTaskOptions:o error:error];
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d
                                            dynamicLinkingDescriptor:(MTL4PipelineStageDynamicLinkingDescriptor *)dl
                                                 compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)error {
    [self captureIfWanted:d];
    id hit = [self fromArchives:o descriptor:d computeLinking:dl renderLinking:nil];
    if (hit) return hit;
    return [self buildCompute:d dynamic:dl capture:NO error:error];
}

- (id<MTLRenderPipelineState>)buildRender:(MTL4PipelineDescriptor *)d dynamic:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl
                                  capture:(BOOL)capture error:(NSError **)error {
    if (capture) [self captureIfWanted:d];
    BOOL unspecialized = NO;
    id<MTLRenderPipelineState> ps = nil;
    if ([d isKindOfClass:[MTL4RenderPipelineDescriptor class]]) {
        MTL4RenderPipelineDescriptor *rd = (MTL4RenderPipelineDescriptor *)d;
        MTLRenderPipelineDescriptor *r = nvmtl4_render_descriptor(rd, dl, &unspecialized, error);
        if (!r) return nil;
        MTLRenderPipelineReflection *rf = nil;
        ps = [_device newRenderPipelineStateWithDescriptor:r options:MTLPipelineOptionNone reflection:&rf error:error];
        if (ps) {
            NV4StateInfo *info = [NV4StateInfo new];
            info.desc = d; info.renderLinking = dl; info.unspecialized = unspecialized;
            info.inheritsColorMapping = (rd.colorAttachmentMappingState == MTL4LogicalToPhysicalColorAttachmentMappingStateInherited);
            if (nvmtl4_wants_reflection(d)) info.reflection = rf;
            objc_setAssociatedObject(ps, &kNV4Info, info, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        return ps;
    }
    if ([d isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]]) {
        MTL4MeshRenderPipelineDescriptor *md = (MTL4MeshRenderPipelineDescriptor *)d;
        MTLMeshRenderPipelineDescriptor *m = nvmtl4_mesh_descriptor(md, dl, &unspecialized, error);
        if (!m) return nil;
        MTLRenderPipelineReflection *rf = nil;
        ps = [_device newRenderPipelineStateWithMeshDescriptor:m options:MTLPipelineOptionNone reflection:&rf error:error];
        if (ps) {
            NV4StateInfo *info = [NV4StateInfo new];
            info.desc = d; info.renderLinking = dl; info.unspecialized = unspecialized;
            info.inheritsColorMapping = (md.colorAttachmentMappingState == MTL4LogicalToPhysicalColorAttachmentMappingStateInherited);
            if (nvmtl4_wants_reflection(d)) info.reflection = rf;
            objc_setAssociatedObject(ps, &kNV4Info, info, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        return ps;
    }
    if ([d isKindOfClass:[MTL4TileRenderPipelineDescriptor class]]) {
#ifdef NVMTL4_TILE_CREATE
        MTLTileRenderPipelineDescriptor *t = nvmtl4_tile_descriptor((MTL4TileRenderPipelineDescriptor *)d, error);
        if (!t) return nil;
        ps = NVMTL4_TILE_CREATE(_device, t, error);
        if (ps) {
            NV4StateInfo *info = [NV4StateInfo new];
            info.desc = d; info.renderLinking = dl;
            objc_setAssociatedObject(ps, &kNV4Info, info, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        return ps;
#else
        (void)nvmtl4_tile_descriptor;
        return nvmtl4_fail(error, @"unsupported - tile render pipelines wait on the tile pipeline creation (nvmtl_tile_pipeline_create)");
#endif
    }
    return nvmtl4_fail(error, [NSString stringWithFormat:@"unsupported pipeline descriptor class %s", d ? object_getClassName(d) : "(nil)"]);
}

- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d
                                               compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)error {
    return [self newRenderPipelineStateWithDescriptor:d dynamicLinkingDescriptor:nil compilerTaskOptions:o error:error];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d
                                          dynamicLinkingDescriptor:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl
                                               compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)error {
    [self captureIfWanted:d];
    id hit = [self fromArchives:o descriptor:d computeLinking:nil renderLinking:dl];
    if (hit) return hit;
    return [self buildRender:d dynamic:dl capture:NO error:error];
}

// The source pipeline supplies everything but the attachment state the descriptor now fixes: Apple answers with a
// pipeline in every case it was probed with, and the functions of a specialization descriptor are not taken from it
// (newRenderPipelineDescriptorForSpecialization hands them out empty).
- (id<MTLRenderPipelineState>)newRenderPipelineStateBySpecializationWithDescriptor:(MTL4PipelineDescriptor *)d
                                                                          pipeline:(id<MTLRenderPipelineState>)pipeline
                                                                             error:(NSError **)error {
    if (!pipeline) return nvmtl4_fail(error, @"specialization needs the unspecialized pipeline");
    NV4StateInfo *src = nvmtl4_info(pipeline);
    if (!src) return nvmtl4_fail(error, @"the pipeline given for specialization was not made by this compiler");
    MTL4PipelineDescriptor *base = [src.desc copy];
    MTL4RenderPipelineColorAttachmentDescriptorArray *dst = nil, *given = nil;
    if ([base isKindOfClass:[MTL4RenderPipelineDescriptor class]] && [d isKindOfClass:[MTL4RenderPipelineDescriptor class]]) {
        dst = ((MTL4RenderPipelineDescriptor *)base).colorAttachments; given = ((MTL4RenderPipelineDescriptor *)d).colorAttachments;
    } else if ([base isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]] && [d isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]]) {
        dst = ((MTL4MeshRenderPipelineDescriptor *)base).colorAttachments; given = ((MTL4MeshRenderPipelineDescriptor *)d).colorAttachments;
    } else return nvmtl4_fail(error, @"the specialization descriptor is not the kind of descriptor the pipeline was made from");
    for (NSUInteger i = 0; i < 8; i++) {
        MTL4RenderPipelineColorAttachmentDescriptor *s = dst[i], *g = given[i];
        if (s.blendingState != MTL4BlendStateUnspecialized && !(s.writeMask & MTLColorWriteMaskUnspecialized)) continue;
        s.blendingState = g.blendingState; s.writeMask = g.writeMask;
        s.sourceRGBBlendFactor = g.sourceRGBBlendFactor; s.destinationRGBBlendFactor = g.destinationRGBBlendFactor;
        s.rgbBlendOperation = g.rgbBlendOperation; s.sourceAlphaBlendFactor = g.sourceAlphaBlendFactor;
        s.destinationAlphaBlendFactor = g.destinationAlphaBlendFactor; s.alphaBlendOperation = g.alphaBlendOperation;
    }
    return [self buildRender:base dynamic:src.renderLinking capture:YES error:error];
}

- (id<MTL4BinaryFunction>)newBinaryFunctionWithDescriptor:(MTL4BinaryFunctionDescriptor *)d
                                      compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)error {
    if (!d.functionDescriptor) return nvmtl4_fail(error, @"binary function descriptor has no function descriptor");
    id<MTLFunction> fn = nvmtl4_resolve_function(d.functionDescriptor, error);
    if (!fn) return nil;
    NVMTL4BinaryFunction *bf = [NVMTL4BinaryFunction new];
    bf->_fn = fn; bf->_name = [d.name copy] ?: fn.name; bf->_options = d.options;
    return bf;
}
- (id<MTL4MachineLearningPipelineState>)newMachineLearningPipelineStateWithDescriptor:(MTL4MachineLearningPipelineDescriptor *)d
                                                                                error:(NSError **)error {
    return nvmtl4ml_new_pipeline(d, _device, error);
}

#pragma mark asynchronous variants: the same work on the pipeline compile queue

- (id<MTL4CompilerTask>)newLibraryWithDescriptor:(MTL4LibraryDescriptor *)d completionHandler:(MTLNewLibraryCompletionHandler)h {
    MTL4LibraryDescriptor *snap = [d copy];
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil; id<MTLLibrary> lib = [self newLibraryWithDescriptor:snap error:&e];
        if (h) h(lib, e);
    }];
}
- (id<MTL4CompilerTask>)newDynamicLibrary:(id<MTLLibrary>)library completionHandler:(MTLNewDynamicLibraryCompletionHandler)h {
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil; id<MTLDynamicLibrary> dl = [self newDynamicLibrary:library error:&e];
        if (h) h(dl, e);
    }];
}
- (id<MTL4CompilerTask>)newDynamicLibraryWithURL:(NSURL *)url completionHandler:(MTLNewDynamicLibraryCompletionHandler)h {
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil; id<MTLDynamicLibrary> dl = [self newDynamicLibraryWithURL:url error:&e];
        if (h) h(dl, e);
    }];
}
- (id<MTL4CompilerTask>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d
                                          compilerTaskOptions:(MTL4CompilerTaskOptions *)o
                                            completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    return [self newComputePipelineStateWithDescriptor:d dynamicLinkingDescriptor:nil compilerTaskOptions:o completionHandler:h];
}
- (id<MTL4CompilerTask>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d
                                     dynamicLinkingDescriptor:(MTL4PipelineStageDynamicLinkingDescriptor *)dl
                                          compilerTaskOptions:(MTL4CompilerTaskOptions *)o
                                            completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    MTL4ComputePipelineDescriptor *snap = [d copy]; MTL4PipelineStageDynamicLinkingDescriptor *dls = [dl copy];
    MTL4CompilerTaskOptions *os = [o copy];
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil;
        id<MTLComputePipelineState> ps = [self newComputePipelineStateWithDescriptor:snap dynamicLinkingDescriptor:dls
                                                                  compilerTaskOptions:os error:&e];
        if (h) h(ps, e);
    }];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d
                                         compilerTaskOptions:(MTL4CompilerTaskOptions *)o
                                           completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    return [self newRenderPipelineStateWithDescriptor:d dynamicLinkingDescriptor:nil compilerTaskOptions:o completionHandler:h];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d
                                    dynamicLinkingDescriptor:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl
                                         compilerTaskOptions:(MTL4CompilerTaskOptions *)o
                                           completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    MTL4PipelineDescriptor *snap = [d copy]; MTL4RenderPipelineDynamicLinkingDescriptor *dls = [dl copy];
    MTL4CompilerTaskOptions *os = [o copy];
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil;
        id<MTLRenderPipelineState> ps = [self newRenderPipelineStateWithDescriptor:snap dynamicLinkingDescriptor:dls
                                                               compilerTaskOptions:os error:&e];
        if (h) h(ps, e);
    }];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateBySpecializationWithDescriptor:(MTL4PipelineDescriptor *)d
                                                                    pipeline:(id<MTLRenderPipelineState>)pipeline
                                                           completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    MTL4PipelineDescriptor *snap = [d copy];
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil;
        id<MTLRenderPipelineState> ps = [self newRenderPipelineStateBySpecializationWithDescriptor:snap pipeline:pipeline error:&e];
        if (h) h(ps, e);
    }];
}
- (id<MTL4CompilerTask>)newBinaryFunctionWithDescriptor:(MTL4BinaryFunctionDescriptor *)d
                                    compilerTaskOptions:(MTL4CompilerTaskOptions *)o
                                      completionHandler:(MTL4NewBinaryFunctionCompletionHandler)h {
    MTL4BinaryFunctionDescriptor *snap = [d copy];
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil;
        id<MTL4BinaryFunction> bf = [self newBinaryFunctionWithDescriptor:snap compilerTaskOptions:o error:&e];
        if (h) h(bf, e);
    }];
}
- (id<MTL4CompilerTask>)newMachineLearningPipelineStateWithDescriptor:(MTL4MachineLearningPipelineDescriptor *)d
                                                    completionHandler:(MTL4NewMachineLearningPipelineStateCompletionHandler)h {
    return [[NVMTL4CompilerTask alloc] initWithCompiler:self work:^{
        NSError *e = nil;
        id<MTL4MachineLearningPipelineState> ps = [self newMachineLearningPipelineStateWithDescriptor:d error:&e];
        if (h) h(ps, e);
    }];
}
@end

#pragma mark - archive

// Two kinds of file open here: this driver's own container (descriptors + libraries) and the fat container Apple's
// toolchain writes (air64 slices holding the functions of the archived pipelines; its GPU binary is not usable).
NV4_AVAIL @implementation NVMTL4Archive {
    NVMTLDevice *_device;
    NV4Ctx *_cx;
    NSMutableDictionary<NSString *, NSDictionary *> *_byKey;   // our container: key -> entry
    NSMutableArray<id<MTLLibrary>> *_appleLibs;                // Apple's container: libraries of the air64 slices
    NSMutableSet<NSString *> *_appleNames;
    NVMTL4Compiler *_compiler;
    NSString *_label;
}
@synthesize label = _label;

- (instancetype)initWithData:(NSData *)data device:(NVMTLDevice *)device error:(NSError **)error {
    if ((self = [super init])) {
        _device = device; _cx = nvmtl4_new_ctx(device); _byKey = [NSMutableDictionary dictionary];
        NSDictionary *doc = nv4_read_container(data);
        if (doc) {
            [_cx.bytesByHash addEntriesFromDictionary:doc[@"libraries"]];
            for (NSDictionary *e in doc[@"entries"]) _byKey[e[@"key"]] = e;
        } else {
            NSArray<NSData *> *slices = nv4_apple_air_slices(data);
            if (!slices.count) { nvmtl4_fail(error, @"the file is neither a driver archive nor an archive with AIR slices"); return nil; }
            _appleLibs = [NSMutableArray array]; _appleNames = [NSMutableSet set];
            NSError *le = nil;
            for (NSData *s in slices) {
                id<MTLLibrary> l = _cx.makeLibrary(s, &le);
                if (!l) { nvlog("MTL4 archive: an air64 slice did not load: %s", le.localizedDescription.UTF8String ?: "?"); continue; }
                [_appleLibs addObject:l];
                [_appleNames addObjectsFromArray:l.functionNames];
            }
            if (!_appleLibs.count) { nvmtl4_fail(error, @"no air64 slice of the archive could be loaded"); return nil; }
        }
        _compiler = [[NVMTL4Compiler alloc] initWithDevice:device descriptor:nil];
    }
    return self;
}

NV4_AVAIL static void nvmtl4_collect_names(NSDictionary *fd, NSMutableArray *out) {
    NSString *k = fd[@"kind"];
    if ([k isEqual:@"library"]) [out addObject:fd[@"name"]];
    else if ([k isEqual:@"specialized"]) nvmtl4_collect_names(fd[@"function_descriptor"], out);
}
- (BOOL)appleHolds:(NSDictionary *)enc {
    NSMutableArray *names = [NSMutableArray array];
    for (NSString *key in enc) {
        id v = enc[key];
        if ([key hasSuffix:@"_function_descriptor"] && [v isKindOfClass:[NSDictionary class]]) nvmtl4_collect_names(v, names);
    }
    if (!names.count) return NO;
    for (NSString *n in names) if (![_appleNames containsObject:n]) return NO;
    return YES;
}

- (id)lookup:(MTL4PipelineDescriptor *)d computeLinking:(MTL4PipelineStageDynamicLinkingDescriptor *)cdl
renderLinking:(MTL4RenderPipelineDynamicLinkingDescriptor *)rdl error:(NSError **)error {
    NV4Ctx *asked = nvmtl4_new_ctx(_device);   // the request's own libraries, hashed the same way
    NSString *kind = nil; NSError *e = nil;
    NSDictionary *enc = nv4_encode_pipeline(d, asked, &kind, &e);
    if (!enc) { if (error) *error = e; return nil; }
    NSString *key = nv4_key(kind, enc);
    MTL4PipelineDescriptor *use = nil;
    if (_appleLibs) {
        // Apple's archive carries no descriptors: it holds when every function the request names is in its slices.
        if ([self appleHolds:enc]) use = d;
    } else if (_byKey[key]) {
        use = nv4_decode_pipeline(_byKey[key][@"descriptor"], kind, _cx, &e);
        if (!use) { if (error) *error = e; return nil; }
        if (d.label) use.label = d.label;
    }
    if (!use) {
        nvmtl4_fail(error, [NSString stringWithFormat:@"Failed to find %@ pipeline in the archive (key %@)", kind, [key substringToIndex:8]]);
        return nil;
    }
    if ([use isKindOfClass:[MTL4ComputePipelineDescriptor class]])
        return [_compiler buildCompute:(MTL4ComputePipelineDescriptor *)use dynamic:cdl capture:NO error:error];
    return [_compiler buildRender:use dynamic:rdl capture:NO error:error];
}

- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d error:(NSError **)error {
    return [self lookup:d computeLinking:nil renderLinking:nil error:error];
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d
    dynamicLinkingDescriptor:(MTL4PipelineStageDynamicLinkingDescriptor *)dl error:(NSError **)error {
    return [self lookup:d computeLinking:dl renderLinking:nil error:error];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d error:(NSError **)error {
    return [self lookup:d computeLinking:nil renderLinking:nil error:error];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d
    dynamicLinkingDescriptor:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl error:(NSError **)error {
    return [self lookup:d computeLinking:nil renderLinking:dl error:error];
}
// Binary functions are not stored in an archive: the function itself is rebuilt from its descriptor.
- (id<MTL4BinaryFunction>)newBinaryFunctionWithDescriptor:(MTL4BinaryFunctionDescriptor *)d error:(NSError **)error {
    NSMutableArray *names = [NSMutableArray array];
    NSDictionary *enc = d.functionDescriptor ? nv4_encode_fd(d.functionDescriptor, nvmtl4_new_ctx(_device), error) : nil;
    if (!enc) return nil;
    nvmtl4_collect_names(enc, names);
    BOOL held = _appleLibs ? [_appleNames containsObject:names.firstObject] : NO;
    if (!held) for (NSDictionary *e in _byKey.allValues) {
        NSMutableArray *have = [NSMutableArray array];
        for (NSString *k in e[@"descriptor"]) if ([k hasSuffix:@"function_descriptor"]) nvmtl4_collect_names(e[@"descriptor"][k], have);
        if ([have containsObject:names.firstObject]) { held = YES; break; }
    }
    if (!held) return nvmtl4_fail(error, [NSString stringWithFormat:@"Failed to find binary function %@ in the archive", names.firstObject]);
    return [_compiler newBinaryFunctionWithDescriptor:d compilerTaskOptions:nil error:error];
}
@end

#pragma mark - pipeline state methods of Metal 4

// These categories add methods the pipeline state protocols already declare.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wobjc-protocol-method-implementation"

// Reflection exists on a state only when the Metal 4 request asked for it.
NV4_AVAIL @implementation NVMTLComputePipelineState (MTL4Compiler)
- (MTLComputePipelineReflection *)reflection { return nvmtl4_info(self).reflection; }
- (id<MTLFunctionHandle>)nvmtl4HandleForFunction:(id<MTLFunction>)f name:(NSString *)name {
    NSArray *c = _function ? objc_getAssociatedObject(_function, &kNVMTLTableFns) : nil;
    for (NSUInteger k = 0; k < c.count; k++) {
        NVMTLFunction *cf = c[k];
        BOOL same = f ? (cf == (id)f || [cf.name isEqualToString:f.name]) : ([cf.name isEqualToString:name] || [cf->_fname isEqualToString:name] || [cf->_specName isEqualToString:name]);
        if (same) { NVMTLFunctionHandle *h = [NVMTLFunctionHandle new]; h->_id = k + 1; h->_fn = cf; return h; }
    }
    return nil;
}
- (id<MTLFunctionHandle>)functionHandleWithName:(NSString *)name {
    id<MTLFunctionHandle> h = [self nvmtl4HandleForFunction:nil name:name];
    if (!h) nvlog("functionHandleWithName: \"%s\" is not linked into this pipeline -> nil", name.UTF8String);
    return h;
}
- (id<MTLFunctionHandle>)functionHandleWithBinaryFunction:(id<MTL4BinaryFunction>)function {
    if (![(id)function isKindOfClass:[NVMTL4BinaryFunction class]]) { nvlog("functionHandleWithBinaryFunction: a binary function not made by this driver -> nil"); return nil; }
    id<MTLFunctionHandle> h = [self nvmtl4HandleForFunction:((NVMTL4BinaryFunction *)function)->_fn name:nil];
    if (!h) nvlog("functionHandleWithBinaryFunction: \"%s\" is not linked into this pipeline -> nil", function.name.UTF8String);
    return h;
}
// A pipeline plus more binary functions: the same request again with those functions linked in.
- (id<MTLComputePipelineState>)newComputePipelineStateWithBinaryFunctions:(NSArray<id<MTL4BinaryFunction>> *)more error:(NSError **)error {
    NV4StateInfo *info = nvmtl4_info(self);
    if (!info || ![info.desc isKindOfClass:[MTL4ComputePipelineDescriptor class]])
        return nvmtl4_fail(error, @"this pipeline was not made by a Metal 4 compiler, so it cannot be extended");
    MTL4PipelineStageDynamicLinkingDescriptor *dl = [info.stageLinking copy] ?: [MTL4PipelineStageDynamicLinkingDescriptor new];
    NSMutableArray *all = [NSMutableArray arrayWithArray:dl.binaryLinkedFunctions ?: @[]];
    [all addObjectsFromArray:more];
    dl.binaryLinkedFunctions = all;
    MTL4ComputePipelineDescriptor *d = [(MTL4ComputePipelineDescriptor *)info.desc copy];
    d.supportBinaryLinking = YES;
    NVMTL4Compiler *c = [[NVMTL4Compiler alloc] initWithDevice:(NVMTLDevice *)gNVMTLMainDevice descriptor:nil];
    return [c buildCompute:d dynamic:dl capture:NO error:error];
}
@end

NV4_AVAIL @implementation NVMTLRenderPipelineState (MTL4Compiler)
- (MTLRenderPipelineReflection *)reflection { return nvmtl4_info(self).reflection; }
- (id<MTLFunctionHandle>)functionHandleWithName:(NSString *)name stage:(MTLRenderStages)st {
    const void *key = (st & MTLRenderStageFragment) ? &kNVMTLRenderFrag : (st & MTLRenderStageVertex) ? &kNVMTLRenderVert : NULL;
    id lf = key ? objc_getAssociatedObject(self, key) : nil;
    NSArray *c = lf ? objc_getAssociatedObject(lf, &kNVMTLTableFns) : nil;
    for (NSUInteger k = 0; k < c.count; k++) {
        NVMTLFunction *cf = c[k];
        if ([cf.name isEqualToString:name] || [cf->_fname isEqualToString:name] || [cf->_specName isEqualToString:name]) {
            NVMTLFunctionHandle *h = [NVMTLFunctionHandle new]; h->_id = k + 1; h->_fn = cf; return h;
        }
    }
    nvlog("render functionHandleWithName: \"%s\" stage %lu is not linked into this pipeline -> nil", name.UTF8String, (unsigned long)st);
    return nil;
}
- (id<MTLFunctionHandle>)functionHandleWithBinaryFunction:(id<MTL4BinaryFunction>)function stage:(MTLRenderStages)st {
    if (![(id)function isKindOfClass:[NVMTL4BinaryFunction class]]) { nvlog("render functionHandleWithBinaryFunction: not made by this driver -> nil"); return nil; }
    return [self functionHandleWithFunction:((NVMTL4BinaryFunction *)function)->_fn stage:st];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithBinaryFunctions:(MTL4RenderPipelineBinaryFunctionsDescriptor *)more error:(NSError **)error {
    NV4StateInfo *info = nvmtl4_info(self);
    if (!info || ![info.desc isKindOfClass:[MTL4RenderPipelineDescriptor class]])
        return nvmtl4_fail(error, @"this pipeline was not made by a Metal 4 compiler, so it cannot be extended");
    MTL4RenderPipelineDynamicLinkingDescriptor *dl = [info.renderLinking copy];
    if (!dl) dl = [MTL4RenderPipelineDynamicLinkingDescriptor new];
    void (^add)(MTL4PipelineStageDynamicLinkingDescriptor *, NSArray *) = ^(MTL4PipelineStageDynamicLinkingDescriptor *s, NSArray *extra) {
        if (!extra.count) return;
        NSMutableArray *all = [NSMutableArray arrayWithArray:s.binaryLinkedFunctions ?: @[]];
        [all addObjectsFromArray:extra];
        s.binaryLinkedFunctions = all;
    };
    add(dl.vertexLinkingDescriptor, more.vertexAdditionalBinaryFunctions);
    add(dl.fragmentLinkingDescriptor, more.fragmentAdditionalBinaryFunctions);
    if (more.tileAdditionalBinaryFunctions.count || more.objectAdditionalBinaryFunctions.count || more.meshAdditionalBinaryFunctions.count)
        return nvmtl4_fail(error, @"unsupported - extra binary functions for the tile, object and mesh stages of a vertex/fragment pipeline");
    MTL4RenderPipelineDescriptor *d = [(MTL4RenderPipelineDescriptor *)info.desc copy];
    d.supportVertexBinaryLinking = YES; d.supportFragmentBinaryLinking = YES;
    NVMTL4Compiler *c = [[NVMTL4Compiler alloc] initWithDevice:(NVMTLDevice *)gNVMTLMainDevice descriptor:nil];
    return [c buildRender:d dynamic:dl capture:NO error:error];
}
// The descriptor a specialization call starts from: the request without its functions, so only the state the
// specialization may set remains (Apple hands the functions out empty).
- (MTL4PipelineDescriptor *)newRenderPipelineDescriptorForSpecialization {
    NV4StateInfo *info = nvmtl4_info(self);
    if (!info) { nvlog("newRenderPipelineDescriptorForSpecialization: this pipeline was not made by a Metal 4 compiler -> nil"); return nil; }
    MTL4PipelineDescriptor *d = [info.desc copy];
    if ([d isKindOfClass:[MTL4RenderPipelineDescriptor class]]) {
        MTL4RenderPipelineDescriptor *r = (MTL4RenderPipelineDescriptor *)d;
        r.vertexFunctionDescriptor = nil; r.fragmentFunctionDescriptor = nil;
        r.vertexStaticLinkingDescriptor = nil; r.fragmentStaticLinkingDescriptor = nil;
    } else if ([d isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]]) {
        MTL4MeshRenderPipelineDescriptor *m = (MTL4MeshRenderPipelineDescriptor *)d;
        m.objectFunctionDescriptor = nil; m.meshFunctionDescriptor = nil; m.fragmentFunctionDescriptor = nil;
        m.objectStaticLinkingDescriptor = nil; m.meshStaticLinkingDescriptor = nil; m.fragmentStaticLinkingDescriptor = nil;
    }
    return d;
}
@end

#pragma clang diagnostic pop

#pragma mark - device entry points
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wobjc-protocol-method-implementation"

NV4_AVAIL @interface NVMTLDevice (MTL4Compiler)
@end

NV4_AVAIL @implementation NVMTLDevice (MTL4Compiler)
- (id<MTL4Compiler>)newCompilerWithDescriptor:(MTL4CompilerDescriptor *)descriptor error:(NSError **)error {
    if (error) *error = nil;
    return [[NVMTL4Compiler alloc] initWithDevice:self descriptor:descriptor];
}
- (id<MTL4Archive>)newArchiveWithURL:(NSURL *)url error:(NSError **)error {
    if (error) *error = nil;
    NSData *data = url.isFileURL ? [NSData dataWithContentsOfURL:url] : nil;
    if (!data) {
        nvlog("MTL4 compiler: newArchiveWithURL: cannot read an archive at %s", url.absoluteString.UTF8String ?: "(nil)");
        if (error) *error = [NSError errorWithDomain:NSCocoaErrorDomain code:NSFileReadNoSuchFileError
                             userInfo:@{ NSLocalizedDescriptionKey: @"the archive file cannot be read", NSURLErrorKey: url ?: [NSNull null] }];
        return nil;
    }
    return [[NVMTL4Archive alloc] initWithData:data device:self error:error];
}
- (id<MTL4PipelineDataSetSerializer>)newPipelineDataSetSerializerWithDescriptor:(MTL4PipelineDataSetSerializerDescriptor *)descriptor {
    return [[NVMTL4PipelineDataSetSerializer alloc] initWithDevice:self configuration:descriptor.configuration];
}
// Apple answers nil for a binary function that is not tied to a pipeline (measured on macOS 27): a function handle
// is an index into the function table of the pipeline that links the function, so it comes from that pipeline.
- (id<MTLFunctionHandle>)functionHandleWithBinaryFunction:(id<MTL4BinaryFunction>)function {
    nvlog("functionHandleWithBinaryFunction: a handle comes from the pipeline that links the function -> nil");
    return nil;
}
@end
#pragma clang diagnostic pop
