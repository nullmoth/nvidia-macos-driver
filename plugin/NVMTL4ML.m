/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Metal 4 machine learning: MTL4MachineLearningPipelineState and MTL4MachineLearningCommandEncoder.
//
// Apple's tooling (metal-package-builder -ml) turns a Core ML package into a .mtlpackage whose payload is an MPSGraph
// package. The Metal framework opens it as an _MTLMLLibrary that owns an MPSGraphExecutable, and the machine learning
// pipeline is that executable specialised for the input shapes. This file runs the same executable on this driver:
// the library keeps Apple's own object (nvmtl4ml_new_library hands the .mtlpackage to the framework's loader), the
// pipeline state records the specialised shapes, and a dispatch encodes the executable onto the replayed Metal 3 command
// buffer with the argument table's tensors as the graph's inputs and outputs. MPS and MPSGraph are reached through the
// runtime so the plugin does not link them.
//
// Tensor binding: argument table buffer indices 0..N-1 are the model inputs in order, N..N+M-1 the outputs, each bound
// with setResource: of an MTLTensor's gpuResourceID (measured on a real Mac with a one-in one-out network and a
// two-in two-out network, tests/ml). Metal tensor extents list the innermost dimension first, MPSGraph shapes the
// outermost first, so every shape is reversed on the way in. A tensor whose layout the graph cannot read in place
// (rows padded to 64 bytes, a non-zero offset) is gathered into a packed scratch buffer before the graph and scattered
// back after it, with blit copies on the same command buffer.
//
// Imported by NVMTLDevice.m after NVMTL4Core.m (the hooks) and NVMTL4Extras.m (the tensor objects).

#import <objc/message.h>
#import <objc/runtime.h>
#import <dlfcn.h>

// ---- reaching MPS without linking it ----

typedef NSInteger nvmtl4ml_dtype;   // MPSDataType

static BOOL nvmtl4ml_load_mps(void)
{
    static BOOL ok; static dispatch_once_t once;
    dispatch_once(&once, ^{
        // Metal opens the same frameworks when it loads the package; opening them here makes the classes resolvable even
        // for a process that never touched MPS before.
        dlopen("/System/Library/Frameworks/MetalPerformanceShaders.framework/MetalPerformanceShaders", RTLD_LAZY);
        dlopen("/System/Library/Frameworks/MetalPerformanceShadersGraph.framework/MetalPerformanceShadersGraph", RTLD_LAZY);
        ok = NSClassFromString(@"MPSGraphTensorData") && NSClassFromString(@"MPSCommandBuffer") && NSClassFromString(@"MPSGraphShapedType");
        if (!ok) nvlog("mtl4 ml: unsupported - MetalPerformanceShadersGraph could not be loaded");
    });
    return ok;
}

// MPSDataType values: float bit 0x10000000, signed bit 0x20000000, alternate-encoding bit 0x80000000, plus the width.
NVMTL4 static nvmtl4ml_dtype nvmtl4ml_mps_type(MTLTensorDataType t)
{
    switch (t) {
        case MTLTensorDataTypeFloat32:  return 0x10000020;
        case MTLTensorDataTypeFloat16:  return 0x10000010;
        case MTLTensorDataTypeBFloat16: return 0x90000010;
        case MTLTensorDataTypeInt32:    return 0x20000020;
        case MTLTensorDataTypeUInt32:   return 0x00000020;
        case MTLTensorDataTypeInt16:    return 0x20000010;
        case MTLTensorDataTypeUInt16:   return 0x00000010;
        case MTLTensorDataTypeInt8:     return 0x20000008;
        case MTLTensorDataTypeUInt8:    return 0x00000008;
        default: return 0;
    }
}

static NSString *nvmtl4ml_shape_str(NSArray<NSNumber *> *s)
{
    NSMutableArray *p = [NSMutableArray new];
    for (NSNumber *n in s) [p addObject:n.stringValue];
    return [NSString stringWithFormat:@"(%@)", [p componentsJoinedByString:@", "]];
}

static NSArray<NSNumber *> *nvmtl4ml_reversed(NSArray<NSNumber *> *a)
{
    return a.reverseObjectEnumerator.allObjects;
}

// ---- reflection ----

// The reflection class is Apple's; a machine learning network's arguments are the tensors the caller binds by index, and a
// real Mac reports no bindings for one (measured), so the subclass answers an empty list. Made at run time because the
// class does not exist before macOS 26.
static id nvmtl4ml_make_reflection(void)
{
    static Class k; static dispatch_once_t once;
    dispatch_once(&once, ^{
        Class base = NSClassFromString(@"MTL4MachineLearningPipelineReflection");
        if (!base) return;
        k = objc_allocateClassPair(base, "NVMTL4MLReflection", 0);
        if (!k) return;
        class_addMethod(k, @selector(bindings), imp_implementationWithBlock(^NSArray *(id s) { return @[]; }), "@@:");
        objc_registerClassPair(k);
    });
    return k ? [[k alloc] init] : nil;
}

// ---- pipeline state ----

NVMTL4 @interface NVMTL4MLPipelineState : NSObject <MTL4MachineLearningPipelineState> {
@public
    id<MTLDevice> _dev; NSString *_label; id _reflection;
    id _exec;                                        // MPSGraphExecutable
    id _gdev;                                        // MPSGraphDevice
    NSArray<NSArray<NSNumber *> *> *_inShapes;       // outermost first, as MPSGraph wants them
    NSArray<NSNumber *> *_inTypes;
    NSArray<NSArray<NSNumber *> *> *_outShapes;
    NSArray<NSNumber *> *_outTypes;
}
@end

@implementation NVMTL4MLPipelineState
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { return _dev; }
- (id)reflection { return _reflection; }
// The graph runtime owns its intermediates, so the heap the app sizes from this is only a placeholder. 64 bytes is what a
// real Mac reports for a small network; the dispatch does not touch the heap.
- (NSUInteger)intermediatesHeapSize { return 64; }
- (NSUInteger)allocatedSize { return 0; }
@end

static NSError *nvmtl4ml_error(NSString *why)
{
    return [NSError errorWithDomain:@"MTL4MachineLearningDomain" code:1 userInfo:@{ NSLocalizedDescriptionKey: why }];
}

static id nvmtl4ml_fail(NSError **err, BOOL unsupported, NSString *why)
{
    nvlog("mtl4 ml: %s%s", unsupported ? "unsupported - " : "REFUSED - ", why.UTF8String);
    if (err) *err = nvmtl4ml_error(why);
    return nil;
}

// The graph executable of an _MTLMLLibrary. executableWithDeviceSelection: indexes the per-device-class executables;
// the first one that exists is the one this device runs.
static id nvmtl4ml_executable(id lib)
{
    SEL s = sel_registerName("executableWithDeviceSelection:");
    if (![lib respondsToSelector:s]) return nil;
    for (long i = 0; i < 3; i++) {
        id e = ((id (*)(id, SEL, long))objc_msgSend)(lib, s, i);
        if (e) return e;
    }
    return nil;
}

NVMTL4 id nvmtl4ml_new_pipeline(id descriptor, id<MTLDevice> dev, NSError **err)
{
    if (err) *err = nil;
    MTL4MachineLearningPipelineDescriptor *d = descriptor;
    MTL4FunctionDescriptor *fd = d.machineLearningFunctionDescriptor;
    if (!fd) return nvmtl4ml_fail(err, NO, @"Function (null) not found in library");
    if (![fd isKindOfClass:[MTL4LibraryFunctionDescriptor class]])
        return nvmtl4ml_fail(err, YES, @"a machine learning pipeline function must be a library function descriptor");
    MTL4LibraryFunctionDescriptor *lf = (MTL4LibraryFunctionDescriptor *)fd;
    if (!lf.library || !lf.name.length) return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:@"Function %@ not found in library", lf.name]);
    if (![lf.library.functionNames containsObject:lf.name])
        return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:@"Function %@ not found in library", lf.name]);
    if (!nvmtl4ml_load_mps()) return nvmtl4ml_fail(err, YES, @"MetalPerformanceShadersGraph is not available");
    id exec = nvmtl4ml_executable(lf.library);
    if (!exec) return nvmtl4ml_fail(err, YES, @"the library was not made from a machine learning package");

    SEL shapesSel = sel_registerName("getInputShapesForFunction:");
    if (![exec respondsToSelector:shapesSel]) return nvmtl4ml_fail(err, YES, @"this MPSGraph cannot report a network's input shapes");
    NSArray *modelIn = ((id (*)(id, SEL, id))objc_msgSend)(exec, shapesSel, lf.name);
    if (!modelIn.count) return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:@"Function %@ reports no inputs", lf.name]);

    Class shapedCls = NSClassFromString(@"MPSGraphShapedType");
    NSMutableArray *inShapes = [NSMutableArray new], *inTypes = [NSMutableArray new], *specTypes = [NSMutableArray new];
    for (NSUInteger i = 0; i < modelIn.count; i++) {
        NSArray<NSNumber *> *mshape = [modelIn[i] valueForKey:@"shape"];
        nvmtl4ml_dtype mt = (nvmtl4ml_dtype)[[modelIn[i] valueForKey:@"dataType"] longValue];
        MTLTensorExtents *given = [d inputDimensionsAtBufferIndex:(NSInteger)i];
        NSArray<NSNumber *> *shape = mshape;
        if (given) {
            NSMutableArray *g = [NSMutableArray new];
            for (NSUInteger k = 0; k < given.rank; k++) [g addObject:@([given extentAtDimensionIndex:k])];
            shape = nvmtl4ml_reversed(g);
            BOOL ok = shape.count == mshape.count;
            for (NSUInteger k = 0; ok && k < shape.count; k++)
                if (mshape[k].longValue > 0 && mshape[k].longValue != shape[k].longValue) ok = NO;
            if (!ok) return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:
                @"Input dimensions at index %lu are incompatible with the compiled model shape (provided %@ but model expects %@)",
                (unsigned long)i, nvmtl4ml_shape_str(shape), nvmtl4ml_shape_str(mshape)]);
        } else {
            for (NSNumber *n in mshape) if (n.longValue <= 0)
                return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:@"Input dimensions are required at index %lu: the model has a dynamic shape %@",
                                               (unsigned long)i, nvmtl4ml_shape_str(mshape)]);
        }
        [inShapes addObject:shape]; [inTypes addObject:@(mt)];
        id st = ((id (*)(id, SEL, id, long))objc_msgSend)([shapedCls alloc], sel_registerName("initWithShape:dataType:"), shape, (long)mt);
        [specTypes addObject:st];
    }

    id gdev = ((id (*)(Class, SEL, id))objc_msgSend)(NSClassFromString(@"MPSGraphDevice"), sel_registerName("deviceWithMTLDevice:"), dev);
    // Specialising here validates the shapes and yields the output types the dispatch checks its tensors against.
    NSArray *outT = ((id (*)(id, SEL, id, id, id))objc_msgSend)(exec, sel_registerName("getOutputTypesWithDevice:inputTypes:compilationDescriptor:"),
                                                                 gdev, specTypes, nil);
    if (!outT.count) return nvmtl4ml_fail(err, NO, [NSString stringWithFormat:@"Function %@ could not be specialised for the given input shapes", lf.name]);
    NSMutableArray *outShapes = [NSMutableArray new], *outTypes = [NSMutableArray new];
    for (id o in outT) { [outShapes addObject:[o valueForKey:@"shape"]]; [outTypes addObject:[o valueForKey:@"dataType"]]; }

    NVMTL4MLPipelineState *p = [NVMTL4MLPipelineState new];
    p->_dev = dev; p->_label = [d.label copy]; p->_exec = exec; p->_gdev = gdev;
    p->_inShapes = inShapes; p->_inTypes = inTypes; p->_outShapes = outShapes; p->_outTypes = outTypes;
    p->_reflection = nvmtl4ml_make_reflection();
    nvlog("mtl4 ml: pipeline for %s: %lu input(s), %lu output(s)", lf.name.UTF8String, (unsigned long)inShapes.count, (unsigned long)outShapes.count);
    return p;
}

// ---- library ----

// A .mtlpackage is opened by the Metal framework's own loader, which builds the _MTLMLLibrary around an MPSGraphExecutable.
// The device class does not override that loader, so the base implementation is called directly.
id nvmtl4ml_new_library(id device, NSURL *url, NSError **err)
{
    Class base = NSClassFromString(@"_MTLDevice");
    Method m = base ? class_getInstanceMethod(base, @selector(newLibraryWithURL:error:)) : NULL;
    if (!m) return nvmtl4ml_fail(err, YES, @"the Metal framework has no loader for machine learning packages");
    if (!nvmtl4ml_load_mps()) return nvmtl4ml_fail(err, YES, @"MetalPerformanceShadersGraph is not available");
    id (*imp)(id, SEL, NSURL *, NSError **) = (id (*)(id, SEL, NSURL *, NSError **))method_getImplementation(m);
    return imp(device, @selector(newLibraryWithURL:error:), url, err);
}


// ---- encoder ----

// One tensor bound to the dispatch, resolved when the dispatch is encoded.
typedef struct {
    NVMTLTensor *t;
    BOOL direct;               // the graph reads the tensor's own buffer
    id<MTLBuffer> scratch;     // packed copy when not direct
    size_t bytes;              // packed size
} nvmtl4ml_bind;

// Rows of a tensor in element order: one blit per run along dimension 0. `toPacked` gathers the tensor into the packed
// buffer, else scatters the packed buffer back.
NVMTL4 static void nvmtl4ml_copy_rows(id<MTLBlitCommandEncoder> b, NVMTLTensor *t, id<MTLBuffer> packed, BOOL toPacked)
{
    const nvmtl4_tensor_layout *L = &t->_L;
    const size_t row = (size_t)(L->rank ? L->d[0] : 1) * L->es;
    NSInteger idx[MTL_TENSOR_MAX_RANK] = { 0 };
    size_t packedOff = 0;
    for (;;) {
        size_t el = 0;
        for (NSUInteger k = 1; k < L->rank; k++) el += (size_t)idx[k] * (size_t)L->s[k];
        const NSUInteger tOff = t->_off + el * L->es;
        if (toPacked) [b copyFromBuffer:t->_buf sourceOffset:tOff toBuffer:packed destinationOffset:packedOff size:row];
        else [b copyFromBuffer:packed sourceOffset:packedOff toBuffer:t->_buf destinationOffset:tOff size:row];
        packedOff += row;
        NSUInteger k = 1;
        for (; k < L->rank; k++) { if (++idx[k] < L->d[k]) break; idx[k] = 0; }
        if (k >= L->rank) break;
    }
}

NVMTL4 static BOOL nvmtl4ml_is_packed(NVMTLTensor *t)
{
    const nvmtl4_tensor_layout *L = &t->_L;
    if (t->_off) return NO;
    NSInteger want = 1;
    for (NSUInteger k = 0; k < L->rank; k++) { if (L->s[k] != want && L->d[k] > 1) return NO; want *= L->d[k]; }
    return YES;
}

// Checks a bound tensor against what the graph expects at that slot. Returns nil when it fits, else the reason.
NVMTL4 static NSString *nvmtl4ml_check(NVMTLTensor *t, NSArray<NSNumber *> *shape, nvmtl4ml_dtype type, NSUInteger slot)
{
    if (!t) return [NSString stringWithFormat:@"argument table index %lu does not hold a tensor", (unsigned long)slot];
    if (nvmtl4ml_mps_type(t->_dt) != type)
        return [NSString stringWithFormat:@"tensor at index %lu has data type %ld but the network expects MPSDataType 0x%lx", (unsigned long)slot, (long)t->_dt, (long)type];
    if (t->_L.rank != shape.count) return [NSString stringWithFormat:@"tensor at index %lu has rank %lu but the network expects %@", (unsigned long)slot, (unsigned long)t->_L.rank, nvmtl4ml_shape_str(shape)];
    for (NSUInteger k = 0; k < shape.count; k++)
        if (shape[k].longValue > 0 && shape[k].longValue != t->_L.d[shape.count - 1 - k])
            return [NSString stringWithFormat:@"tensor at index %lu has dimensions that do not match the network's %@", (unsigned long)slot, nvmtl4ml_shape_str(shape)];
    if (!t->_buf) return [NSString stringWithFormat:@"tensor at index %lu has no backing buffer", (unsigned long)slot];
    return nil;
}

NVMTL4 static void nvmtl4ml_run(id<MTLCommandBuffer> m3, NVMTL4MLPipelineState *pso, NSArray<NVMTLTensor *> *ts, NSString *label);

NVMTL4 @interface NVMTL4MLEncoder : NSObject <MTL4MachineLearningCommandEncoder> {
    id<MTL4CommandBuffer> _cb; void (^_add)(void (^)(id<MTLCommandBuffer>));
    NVMTL4MLPipelineState *_pso; NVMTL4ArgumentTable *_table; NSString *_label; BOOL _ended;
}
- (instancetype)initWithCommandBuffer:(id<MTL4CommandBuffer>)cb add:(void (^)(void (^)(id<MTLCommandBuffer>)))add;
@end

@implementation NVMTL4MLEncoder
- (instancetype)initWithCommandBuffer:(id<MTL4CommandBuffer>)cb add:(void (^)(void (^)(id<MTLCommandBuffer>)))add {
    if ((self = [super init])) { _cb = cb; _add = [add copy]; }
    return self;
}
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (id<MTL4CommandBuffer>)commandBuffer { return _cb; }
- (BOOL)live:(const char *)what {
    if (!_ended) return YES;
    nvmtl4_note_once([NSString stringWithFormat:@"machine learning encoder: %s after endEncoding - ignored", what]);
    return NO;
}
- (void)setPipelineState:(id<MTL4MachineLearningPipelineState>)p {
    if (![self live:"setPipelineState:"]) return;
    if (p && ![p isKindOfClass:[NVMTL4MLPipelineState class]]) { nvlog("mtl4 ml: REFUSED - setPipelineState: with a pipeline this driver did not make"); return; }
    _pso = (NVMTL4MLPipelineState *)p;
}
- (void)setArgumentTable:(id<MTL4ArgumentTable>)t {
    if (![self live:"setArgumentTable:"]) return;
    if (t && ![t isKindOfClass:[NVMTL4ArgumentTable class]]) { nvlog("mtl4 ml: REFUSED - setArgumentTable: with a table this driver did not make"); return; }
    _table = (NVMTL4ArgumentTable *)t;
}

// Barriers: the dispatch is one more block on the replayed Metal 3 command buffer, which already orders every encoder
// against the one before it, so a barrier adds nothing. A fence has no Metal 3 counterpart outside an encoder and is
// reported rather than dropped.
- (void)barrierAfterQueueStages:(MTLStages)a beforeStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)barrierAfterStages:(MTLStages)a beforeQueueStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)barrierAfterEncoderStages:(MTLStages)a beforeEncoderStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)updateFence:(id<MTLFence>)f afterEncoderStages:(MTLStages)s {
    nvmtl4_note_once(@"unsupported: updateFence on a machine learning encoder - the fence is not signalled");
}
- (void)waitForFence:(id<MTLFence>)f beforeEncoderStages:(MTLStages)s {
    nvmtl4_note_once(@"unsupported: waitForFence on a machine learning encoder - the wait is not made");
}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s { if ([self live:"pushDebugGroup:"]) _add(^(id<MTLCommandBuffer> m3) { [m3 pushDebugGroup:s]; }); }
- (void)popDebugGroup { if ([self live:"popDebugGroup"]) _add(^(id<MTLCommandBuffer> m3) { [m3 popDebugGroup]; }); }
- (void)endEncoding { _ended = YES; _pso = nil; _table = nil; }

- (void)dispatchNetworkWithIntermediatesHeap:(id<MTLHeap>)heap {
    if (![self live:"dispatchNetworkWithIntermediatesHeap:"]) return;
    NVMTL4MLPipelineState *pso = _pso; NVMTL4ArgumentTable *tb = _table;
    if (!pso || !tb) { nvlog("mtl4 ml: REFUSED - dispatchNetwork without a %s", !pso ? "pipeline state" : "argument table"); return; }
    const NSUInteger nin = pso->_inShapes.count, nout = pso->_outShapes.count;
    if (tb->_nb < nin + nout) {
        nvlog("mtl4 ml: REFUSED - the argument table has %lu buffer slots, the network needs %lu", (unsigned long)tb->_nb, (unsigned long)(nin + nout));
        return;
    }
    // The table is read when the dispatch is encoded, as for every Metal 4 encoder.
    uint64_t rid[64]; uint8_t kind[64];
    if (nin + nout > 64) { nvlog("mtl4 ml: REFUSED - %lu tensors exceed the 64 this encoder binds", (unsigned long)(nin + nout)); return; }
    os_unfair_lock_lock(&tb->_lk);
    for (NSUInteger i = 0; i < nin + nout; i++) { rid[i] = tb->_baddr[i]; kind[i] = tb->_bk[i]; }
    os_unfair_lock_unlock(&tb->_lk);

    NSMutableArray<NVMTLTensor *> *ts = [NSMutableArray new];
    for (NSUInteger i = 0; i < nin + nout; i++) {
        NVMTLTensor *t = kind[i] == 2 ? nvmtl4_tensor_for_resource_id(rid[i]) : nil;
        NSString *why = nvmtl4ml_check(t, i < nin ? pso->_inShapes[i] : pso->_outShapes[i - nin],
                                       (nvmtl4ml_dtype)(i < nin ? pso->_inTypes[i] : pso->_outTypes[i - nin]).longValue, i);
        if (why) { nvlog("mtl4 ml: REFUSED - %s", why.UTF8String); return; }
        [ts addObject:t];
    }
    NSString *label = _label;
    _add(^(id<MTLCommandBuffer> m3) { nvmtl4ml_run(m3, pso, ts, label); });
}
@end

// Runs one dispatch on the Metal 3 command buffer: gather, encode the graph, scatter.
NVMTL4 static void nvmtl4ml_run(id<MTLCommandBuffer> m3, NVMTL4MLPipelineState *pso, NSArray<NVMTLTensor *> *ts, NSString *label)
{
    const NSUInteger nin = pso->_inShapes.count, n = ts.count;
    id<MTLDevice> dev = m3.device;
    nvmtl4ml_bind B[64];
    BOOL any = NO;
    for (NSUInteger i = 0; i < n; i++) {
        NVMTLTensor *t = ts[i];
        B[i].t = t; B[i].direct = nvmtl4ml_is_packed(t); B[i].scratch = nil;
        B[i].bytes = t->_L.packedBytes;
        if (!B[i].direct) {
            B[i].scratch = [dev newBufferWithLength:MAX(B[i].bytes, (size_t)16) options:MTLResourceStorageModePrivate];
            if (!B[i].scratch) { nvlog("mtl4 ml: FAILED - the %zu-byte scratch buffer for tensor %lu could not be allocated", B[i].bytes, (unsigned long)i); return; }
            any = YES;
        }
    }
    if (any) {   // inputs that need a packed copy
        id<MTLBlitCommandEncoder> b = [m3 blitCommandEncoder];
        for (NSUInteger i = 0; i < nin; i++) if (!B[i].direct) nvmtl4ml_copy_rows(b, B[i].t, B[i].scratch, YES);
        [b endEncoding];
    }
    Class dataCls = NSClassFromString(@"MPSGraphTensorData");
    NSMutableArray *inputs = [NSMutableArray new], *results = [NSMutableArray new];
    for (NSUInteger i = 0; i < n; i++) {
        NSArray<NSNumber *> *shape = i < nin ? pso->_inShapes[i] : pso->_outShapes[i - nin];
        nvmtl4ml_dtype ty = (nvmtl4ml_dtype)(i < nin ? pso->_inTypes[i] : pso->_outTypes[i - nin]).longValue;
        // Dynamic dimensions (<= 0) take the bound tensor's size.
        NSMutableArray *shp = [NSMutableArray new];
        for (NSUInteger k = 0; k < shape.count; k++) [shp addObject:@(B[i].t->_L.d[shape.count - 1 - k])];
        id<MTLBuffer> buf = B[i].direct ? B[i].t->_buf : B[i].scratch;
        id td = ((id (*)(id, SEL, id, id, long))objc_msgSend)([dataCls alloc], sel_registerName("initWithMTLBuffer:shape:dataType:"), buf, shp, (long)ty);
        if (!td) { nvlog("mtl4 ml: FAILED - MPSGraph refused the buffer for tensor %lu", (unsigned long)i); return; }
        [i < nin ? inputs : results addObject:td];
    }
    Class cbCls = NSClassFromString(@"MPSCommandBuffer");
    id mcb = ((id (*)(Class, SEL, id))objc_msgSend)(cbCls, sel_registerName("commandBufferWithCommandBuffer:"), m3);
    if (label) nvlog("mtl4 ml: dispatch %s", label.UTF8String);
    NSArray *r = ((id (*)(id, SEL, id, id, id, id))objc_msgSend)(pso->_exec,
        sel_registerName("encodeToCommandBuffer:inputsArray:resultsArray:executionDescriptor:"), mcb, inputs, results, nil);
    if (!r) { nvlog("mtl4 ml: FAILED - the graph executable encoded nothing"); return; }
    // MPSGraph may commit and continue on a fresh buffer; the replay owns the commit, so a switch is not survivable.
    id root = ((id (*)(id, SEL))objc_msgSend)(mcb, sel_registerName("rootCommandBuffer"));
    if (root && root != m3) nvlog("mtl4 ml: FAILED - MPSGraph moved the work to another command buffer; results are not ordered with the rest of the frame");
    if (any) {   // outputs that were computed into a packed copy
        BOOL need = NO;
        for (NSUInteger i = nin; i < n; i++) if (!B[i].direct) need = YES;
        if (need) {
            id<MTLBlitCommandEncoder> b = [m3 blitCommandEncoder];
            for (NSUInteger i = nin; i < n; i++) if (!B[i].direct) nvmtl4ml_copy_rows(b, B[i].t, B[i].scratch, NO);
            [b endEncoding];
        }
    }
}

NVMTL4 static id<MTL4MachineLearningCommandEncoder> nvmtl4ml_make_encoder(id<MTL4CommandBuffer> cb, void (^add)(void (^)(id<MTLCommandBuffer>)))
{
    if (!nvmtl4ml_load_mps()) return nil;
    return [[NVMTL4MLEncoder alloc] initWithCommandBuffer:cb add:add];
}

__attribute__((constructor)) static void nvmtl4ml_attach(void)
{
    if (@available(macos 26.0, *)) nvmtl4_hook_ml_encoder = nvmtl4ml_make_encoder;
}
