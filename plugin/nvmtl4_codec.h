/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Metal 4 pipeline descriptors as data: one dictionary form shared by the pipelines script (the JSON that
// metal-tt reads), the driver's own archive container and the lookup key. Foundation and public Metal API only, so
// the same code is tested against Apple's objects on a real Mac (tests/mtl4/codec_test.m).
//
// A descriptor becomes an NSDictionary (property-list safe) in the pipelines-script schema: snake_case keys, enums
// by name, fields at their default left out. Function descriptors stay inline here; the script writer hoists them
// into its function table.

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <CommonCrypto/CommonDigest.h>
#import <objc/runtime.h>
#import <objc/message.h>
#include "nvmtl4_enum_names.h"

#define NV4_AVAIL API_AVAILABLE(macos(26.0))
// The recursive helpers below are blocks that name themselves and are cleared before the function returns.
#pragma clang diagnostic ignored "-Warc-retain-cycles"

static NSString *nv4_sha256hex(NSData *d) {
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(d.bytes, (CC_LONG)d.length, h);
    NSMutableString *s = [NSMutableString stringWithCapacity:64];
    for (int i = 0; i < 32; i++) [s appendFormat:@"%02X", h[i]];
    return s;
}

// ---- enum names ----

#define NV4_NAME_FN(T) \
static NSString *nv4_name_##T(long v) { \
    for (int i = 0; nv4_names_##T[i].n; i++) if (nv4_names_##T[i].v == v) return @(nv4_names_##T[i].n); \
    return nil; } \
static BOOL nv4_value_##T(NSString *n, long *out) { \
    for (int i = 0; nv4_names_##T[i].n; i++) if (!strcmp(nv4_names_##T[i].n, n.UTF8String)) { *out = nv4_names_##T[i].v; return YES; } \
    return NO; }
NV4_NAME_FN(MTLPixelFormat) NV4_NAME_FN(MTLVertexFormat) NV4_NAME_FN(MTLBlendFactor) NV4_NAME_FN(MTLBlendOperation)
NV4_NAME_FN(MTLVertexStepFunction) NV4_NAME_FN(MTLPrimitiveTopologyClass)

// An enum value in a dictionary is its name, or the number when the SDK has no name for it.
#define NV4_ENUM_OUT(T, v) (nv4_name_##T(v) ?: @((long)(v)))
static long nv4_enum_in(id v, BOOL (*lookup)(NSString *, long *)) {
    if ([v isKindOfClass:[NSString class]]) { long o = 0; return lookup(v, &o) ? o : 0; }
    return [v longValue];
}
#define NV4_ENUM_IN(T, v) nv4_enum_in(v, nv4_value_##T)

static NSString *nv4_two_state(long v, NSString *a, NSString *b, NSString *c) {
    return v == 0 ? a : v == 1 ? b : (c && v == 2) ? c : [@(v) stringValue];
}
static long nv4_two_state_in(id v, NSString *a, NSString *b, NSString *c) {
    if (![v isKindOfClass:[NSString class]]) return [v longValue];
    if ([v isEqual:a]) return 0;
    if ([v isEqual:b]) return 1;
    if (c && [v isEqual:c]) return 2;
    return 0;
}

// ---- function constants ----

// MTLFunctionConstantValues keeps its values private; Metal's own function creation reads them the same way.
static NSArray<NSDictionary *> *nv4_encode_constants(MTLFunctionConstantValues *cv) {
    if (!cv) return @[];
    NSMutableArray *out = [NSMutableArray array];
    SEL isel = NSSelectorFromString(@"newIndexedConstantArray"), nsel = NSSelectorFromString(@"newNamedConstantArray");
    NSArray *idx = [cv respondsToSelector:isel] ? ((id (*)(id, SEL))objc_msgSend)(cv, isel) : nil;
    NSArray *named = [cv respondsToSelector:nsel] ? ((id (*)(id, SEL))objc_msgSend)(cv, nsel) : nil;
    for (int pass = 0; pass < 2; pass++)
        for (id v in (pass ? idx : named)) {
            Ivar ii = class_getInstanceVariable([v class], pass ? "_index" : "_name");
            Ivar ti = class_getInstanceVariable([v class], "_dataType"), di = class_getInstanceVariable([v class], "_data");
            if (!ii || !ti || !di) continue;
            const uint8_t *base = (const uint8_t *)(__bridge void *)v;
            NSUInteger type = *(const NSUInteger *)(base + ivar_getOffset(ti));
            const void *data = *(const void *const *)(base + ivar_getOffset(di));
            static const uint8_t widths[] = { 0 };
            (void)widths;
            NSUInteger w = 0;
            switch (type) {
                case MTLDataTypeBool: case MTLDataTypeChar: case MTLDataTypeUChar: w = 1; break;
                case MTLDataTypeShort: case MTLDataTypeUShort: case MTLDataTypeHalf: w = 2; break;
                case MTLDataTypeInt: case MTLDataTypeUInt: case MTLDataTypeFloat: w = 4; break;
                case MTLDataTypeLong: case MTLDataTypeULong: w = 8; break;
                default: break;
            }
            if (!data || !w) continue;
            NSMutableDictionary *e = [NSMutableDictionary dictionary];
            if (pass) e[@"index"] = @(*(const NSUInteger *)(base + ivar_getOffset(ii)));
            else e[@"name"] = object_getIvar(v, ii);
            e[@"type"] = @(type);
            e[@"data"] = [NSData dataWithBytes:data length:w];
            [out addObject:e];
        }
    return out;
}
static MTLFunctionConstantValues *nv4_decode_constants(NSArray *a) {
    MTLFunctionConstantValues *cv = [MTLFunctionConstantValues new];
    for (NSDictionary *e in a) {
        NSData *d = e[@"data"]; MTLDataType t = (MTLDataType)[e[@"type"] unsignedIntegerValue];
        if (e[@"name"]) [cv setConstantValue:d.bytes type:t withName:e[@"name"]];
        else [cv setConstantValue:d.bytes type:t atIndex:[e[@"index"] unsignedIntegerValue]];
    }
    return cv;
}

// ---- context: the libraries a descriptor set refers to ----

NV4_AVAIL @interface NV4Ctx : NSObject
@property (nonatomic, copy) NSData *(^libraryBytes)(id<MTLLibrary> lib);
@property (nonatomic, copy) id<MTLLibrary> (^makeLibrary)(NSData *bytes, NSError **err);
@property (nonatomic, copy) NSString *(^libraryPath)(id<MTLLibrary> lib);
@property (nonatomic, strong) NSMutableDictionary<NSString *, NSData *> *bytesByHash;
@property (nonatomic, strong) NSMutableDictionary<NSString *, NSString *> *pathByHash;
@property (nonatomic, strong) NSMutableDictionary<NSString *, id<MTLLibrary>> *libByHash;
- (NSString *)hashForLibrary:(id<MTLLibrary>)lib error:(NSError **)err;
- (id<MTLLibrary>)libraryForHash:(NSString *)h error:(NSError **)err;
@end
NV4_AVAIL @implementation NV4Ctx
- (instancetype)init {
    if ((self = [super init])) {
        _bytesByHash = [NSMutableDictionary dictionary]; _pathByHash = [NSMutableDictionary dictionary];
        _libByHash = [NSMutableDictionary dictionary];
    }
    return self;
}
- (NSString *)hashForLibrary:(id<MTLLibrary>)lib error:(NSError **)err {
    NSData *b = self.libraryBytes ? self.libraryBytes(lib) : nil;
    if (b.length < 4) {
        if (err) *err = [NSError errorWithDomain:@"NVMTL" code:20 userInfo:@{ NSLocalizedDescriptionKey:
            @"a library in the descriptor has no retrievable metallib bytes, so it cannot be recorded" }];
        return nil;
    }
    NSString *h = nv4_sha256hex(b);
    @synchronized (self) {
        if (!_bytesByHash[h]) _bytesByHash[h] = b;
        if (!_libByHash[h]) _libByHash[h] = lib;
        NSString *p = self.libraryPath ? self.libraryPath(lib) : nil;
        if (p.length && !_pathByHash[h]) _pathByHash[h] = p;
    }
    return h;
}
- (id<MTLLibrary>)libraryForHash:(NSString *)h error:(NSError **)err {
    @synchronized (self) { if (_libByHash[h]) return _libByHash[h]; }
    NSData *b = _bytesByHash[h];
    if (!b || !self.makeLibrary) {
        if (err) *err = [NSError errorWithDomain:@"NVMTL" code:21 userInfo:@{ NSLocalizedDescriptionKey:
            [NSString stringWithFormat:@"the archive holds no library %@", h] }];
        return nil;
    }
    id<MTLLibrary> lib = self.makeLibrary(b, err);
    if (lib) @synchronized (self) { _libByHash[h] = lib; }
    return lib;
}
@end

static NSError *nv4_err(NSString *why) {
    return [NSError errorWithDomain:@"NVMTL" code:22 userInfo:@{ NSLocalizedDescriptionKey: why }];
}

// ---- function descriptors ----

NV4_AVAIL static NSDictionary *nv4_encode_fd(MTL4FunctionDescriptor *fd, NV4Ctx *cx, NSError **err);
NV4_AVAIL static MTL4FunctionDescriptor *nv4_decode_fd(NSDictionary *d, NV4Ctx *cx, NSError **err);

NV4_AVAIL static NSArray *nv4_encode_fds(NSArray<MTL4FunctionDescriptor *> *a, NV4Ctx *cx, NSError **err) {
    NSMutableArray *out = [NSMutableArray array];
    for (MTL4FunctionDescriptor *f in a) { NSDictionary *e = nv4_encode_fd(f, cx, err); if (!e) return nil; [out addObject:e]; }
    return out;
}
NV4_AVAIL static NSArray *nv4_decode_fds(NSArray *a, NV4Ctx *cx, NSError **err) {
    NSMutableArray *out = [NSMutableArray array];
    for (NSDictionary *e in a) { id f = nv4_decode_fd(e, cx, err); if (!f) return nil; [out addObject:f]; }
    return out;
}

// The stitching graph as the script prints it: one flat node table (input and function nodes, dependencies first),
// arguments and dependencies as {"id": table index}.
NV4_AVAIL static NSDictionary *nv4_encode_graph(MTLFunctionStitchingGraph *g) {
    NSMutableArray *table = [NSMutableArray array], *encoded = [NSMutableArray array];
    NSMapTable *ids = [NSMapTable strongToStrongObjectsMapTable];
    __block NSNumber *(^add)(id) = nil;
    add = ^NSNumber *(id n) {
        NSNumber *have = [ids objectForKey:n];
        if (have) return have;
        NSDictionary *node;
        if ([n isKindOfClass:[MTLFunctionStitchingInputNode class]]) {
            NSUInteger ai = [(MTLFunctionStitchingInputNode *)n argumentIndex];
            node = @{ @"node_type": @"InputNode", @"node": ai ? @{ @"argument_index": @(ai) } : @{} };
        } else {
            MTLFunctionStitchingFunctionNode *fn = n;
            NSMutableArray *args = [NSMutableArray array], *deps = [NSMutableArray array];
            for (id a in fn.arguments) [args addObject:@{ @"id": add(a) }];
            for (id a in fn.controlDependencies) [deps addObject:@{ @"id": add(a) }];
            NSMutableDictionary *body = [NSMutableDictionary dictionaryWithObject:fn.name ?: @"" forKey:@"name"];
            if (args.count) body[@"arguments"] = args;
            if (deps.count) body[@"control_dependencies"] = deps;
            node = @{ @"node_type": @"FunctionNode", @"node": body };
        }
        NSNumber *i = @(table.count);
        [table addObject:n]; [encoded addObject:node]; [ids setObject:i forKey:n];
        return i;
    };
    NSMutableArray *listed = [NSMutableArray array];
    for (id n in g.nodes) [listed addObject:@{ @"id": add(n) }];
    NSMutableDictionary *out = [NSMutableDictionary dictionary];
    out[@"function_name"] = g.functionName ?: @"";
    out[@"nodes"] = encoded;
    out[@"function_node_ids"] = listed;
    if (g.outputNode) out[@"output_node"] = @{ @"id": add(g.outputNode) };
    if (g.attributes.count) {
        NSMutableArray *at = [NSMutableArray array];
        for (id a in g.attributes) [at addObject:@((long)[a longValue])];
        out[@"attributes"] = at;
    }
    out[@"nodes"] = encoded;   // outputNode may have added entries after the first assignment
    add = nil;   // the block captures itself
    return out;
}
NV4_AVAIL static MTLFunctionStitchingGraph *nv4_decode_graph(NSDictionary *d) {
    NSArray *tbl = d[@"nodes"];
    NSMutableArray *nodes = [NSMutableArray array];
    for (NSDictionary *e in tbl) {
        NSDictionary *b = e[@"node"];
        if ([e[@"node_type"] isEqual:@"InputNode"]) {
            [nodes addObject:[[MTLFunctionStitchingInputNode alloc] initWithArgumentIndex:[b[@"argument_index"] unsignedIntegerValue]]];
        } else {
            NSMutableArray *args = [NSMutableArray array], *deps = [NSMutableArray array];
            for (NSDictionary *a in b[@"arguments"]) [args addObject:nodes[[a[@"id"] unsignedIntegerValue]]];
            for (NSDictionary *a in b[@"control_dependencies"]) [deps addObject:nodes[[a[@"id"] unsignedIntegerValue]]];
            [nodes addObject:[[MTLFunctionStitchingFunctionNode alloc] initWithName:b[@"name"] arguments:args controlDependencies:deps]];
        }
    }
    NSMutableArray *listed = [NSMutableArray array];
    for (NSDictionary *a in d[@"function_node_ids"]) [listed addObject:nodes[[a[@"id"] unsignedIntegerValue]]];
    id out = d[@"output_node"] ? nodes[[d[@"output_node"][@"id"] unsignedIntegerValue]] : nil;
    NSMutableArray *at = [NSMutableArray array];
    for (NSNumber *a in d[@"attributes"]) [at addObject:a];
    return [[MTLFunctionStitchingGraph alloc] initWithFunctionName:d[@"function_name"] nodes:listed outputNode:out attributes:@[]];
}

NV4_AVAIL static NSDictionary *nv4_encode_fd(MTL4FunctionDescriptor *fd, NV4Ctx *cx, NSError **err) {
    if ([fd isKindOfClass:[MTL4LibraryFunctionDescriptor class]]) {
        MTL4LibraryFunctionDescriptor *l = (MTL4LibraryFunctionDescriptor *)fd;
        if (!l.library || !l.name.length) { if (err) *err = nv4_err(@"library function descriptor needs a library and a name"); return nil; }
        NSString *h = [cx hashForLibrary:l.library error:err];
        if (!h) return nil;
        return @{ @"kind": @"library", @"library": h, @"name": l.name };
    }
    if ([fd isKindOfClass:[MTL4SpecializedFunctionDescriptor class]]) {
        MTL4SpecializedFunctionDescriptor *s = (MTL4SpecializedFunctionDescriptor *)fd;
        NSDictionary *inner = s.functionDescriptor ? nv4_encode_fd(s.functionDescriptor, cx, err) : nil;
        if (!inner) { if (err && !*err) *err = nv4_err(@"specialized function descriptor wraps nothing"); return nil; }
        NSMutableDictionary *o = [NSMutableDictionary dictionaryWithObjectsAndKeys:@"specialized", @"kind", inner, @"function_descriptor", nil];
        if (s.specializedName.length) o[@"specialized_name"] = s.specializedName;
        NSArray *cvs = nv4_encode_constants(s.constantValues);
        if (cvs.count) o[@"constant_values"] = cvs;
        return o;
    }
    if ([fd isKindOfClass:[MTL4StitchedFunctionDescriptor class]]) {
        MTL4StitchedFunctionDescriptor *s = (MTL4StitchedFunctionDescriptor *)fd;
        if (!s.functionGraph) { if (err) *err = nv4_err(@"stitched function descriptor has no graph"); return nil; }
        NSArray *fns = nv4_encode_fds(s.functionDescriptors ?: @[], cx, err);
        if (!fns) return nil;
        NSMutableDictionary *o = [NSMutableDictionary dictionaryWithObjectsAndKeys:@"stitched", @"kind", nv4_encode_graph(s.functionGraph), @"function_graph", nil];
        if (fns.count) o[@"function_descriptors"] = fns;
        return o;
    }
    if (err) *err = nv4_err([NSString stringWithFormat:@"function descriptor class %s", fd ? object_getClassName(fd) : "(nil)"]);
    return nil;
}
NV4_AVAIL static MTL4FunctionDescriptor *nv4_decode_fd(NSDictionary *d, NV4Ctx *cx, NSError **err) {
    NSString *k = d[@"kind"];
    if ([k isEqual:@"library"]) {
        id<MTLLibrary> lib = [cx libraryForHash:d[@"library"] error:err];
        if (!lib) return nil;
        MTL4LibraryFunctionDescriptor *l = [MTL4LibraryFunctionDescriptor new];
        l.library = lib; l.name = d[@"name"];
        return l;
    }
    if ([k isEqual:@"specialized"]) {
        MTL4FunctionDescriptor *inner = nv4_decode_fd(d[@"function_descriptor"], cx, err);
        if (!inner) return nil;
        MTL4SpecializedFunctionDescriptor *s = [MTL4SpecializedFunctionDescriptor new];
        s.functionDescriptor = inner; s.specializedName = d[@"specialized_name"];
        if ([d[@"constant_values"] count]) s.constantValues = nv4_decode_constants(d[@"constant_values"]);
        return s;
    }
    if ([k isEqual:@"stitched"]) {
        NSArray *fns = nv4_decode_fds(d[@"function_descriptors"] ?: @[], cx, err);
        if (!fns) return nil;
        MTL4StitchedFunctionDescriptor *s = [MTL4StitchedFunctionDescriptor new];
        s.functionGraph = nv4_decode_graph(d[@"function_graph"]); s.functionDescriptors = fns;
        return s;
    }
    if (err) *err = nv4_err([NSString stringWithFormat:@"archive entry has function descriptor kind %@", k]);
    return nil;
}

// ---- linking descriptors ----

NV4_AVAIL static NSDictionary *nv4_encode_static(MTL4StaticLinkingDescriptor *s, NV4Ctx *cx, NSError **err) {
    NSMutableDictionary *o = [NSMutableDictionary dictionary];
    if (s.functionDescriptors.count) { id a = nv4_encode_fds(s.functionDescriptors, cx, err); if (!a) return nil; o[@"function_descriptors"] = a; }
    if (s.privateFunctionDescriptors.count) { id a = nv4_encode_fds(s.privateFunctionDescriptors, cx, err); if (!a) return nil; o[@"private_function_descriptors"] = a; }
    if (s.groups.count) {
        NSMutableDictionary *g = [NSMutableDictionary dictionary];
        for (NSString *k in s.groups) { id a = nv4_encode_fds(s.groups[k], cx, err); if (!a) return nil; g[k] = a; }
        o[@"groups"] = g;
    }
    return o;
}
NV4_AVAIL static MTL4StaticLinkingDescriptor *nv4_decode_static(NSDictionary *d, NV4Ctx *cx, NSError **err) {
    MTL4StaticLinkingDescriptor *s = [MTL4StaticLinkingDescriptor new];
    if (d[@"function_descriptors"]) { id a = nv4_decode_fds(d[@"function_descriptors"], cx, err); if (!a) return nil; s.functionDescriptors = a; }
    if (d[@"private_function_descriptors"]) { id a = nv4_decode_fds(d[@"private_function_descriptors"], cx, err); if (!a) return nil; s.privateFunctionDescriptors = a; }
    if (d[@"groups"]) {
        NSMutableDictionary *g = [NSMutableDictionary dictionary];
        for (NSString *k in d[@"groups"]) { id a = nv4_decode_fds(d[@"groups"][k], cx, err); if (!a) return nil; g[k] = a; }
        s.groups = g;
    }
    return s;
}

// Apple's script prints MTLShaderValidationEnabled (1) as "Disabled" and MTLShaderValidationDisabled (2) as "Enabled"
// (measured on macOS 27). The same spelling is kept so a script from either side reads the same.
NV4_AVAIL static NSDictionary *nv4_encode_options(MTL4PipelineOptions *o) {
    NSMutableDictionary *d = [NSMutableDictionary dictionary];
    if (o.shaderValidation) d[@"shader_validation"] = nv4_two_state(o.shaderValidation, @"Default", @"Disabled", @"Enabled");
    if (o.shaderReflection) d[@"shader_reflection"] = @((long)o.shaderReflection);
    return d;
}
NV4_AVAIL static MTL4PipelineOptions *nv4_decode_options(NSDictionary *d) {
    MTL4PipelineOptions *o = [MTL4PipelineOptions new];
    if (d[@"shader_validation"]) o.shaderValidation = (MTLShaderValidation)nv4_two_state_in(d[@"shader_validation"], @"Default", @"Disabled", @"Enabled");
    o.shaderReflection = (MTL4ShaderReflection)[d[@"shader_reflection"] unsignedIntegerValue];
    return o;
}

// ---- pipeline descriptors ----

NV4_AVAIL static NSDictionary *nv4_encode_attachments(MTL4RenderPipelineColorAttachmentDescriptorArray *arr) {
    NSMutableArray *out = [NSMutableArray array];
    NSInteger last = -1;
    for (NSInteger i = 0; i < 8; i++) {
        MTL4RenderPipelineColorAttachmentDescriptor *a = arr[i], *z = [MTL4RenderPipelineColorAttachmentDescriptor new];
        NSMutableDictionary *o = [NSMutableDictionary dictionary];
        if (a.pixelFormat != z.pixelFormat) o[@"pixel_format"] = NV4_ENUM_OUT(MTLPixelFormat, a.pixelFormat);
        if (a.writeMask != z.writeMask) o[@"write_mask"] = @((long)a.writeMask);
        if (a.blendingState != z.blendingState) o[@"blending_state"] = nv4_two_state(a.blendingState, @"Disabled", @"Enabled", @"Unspecialized");
        if (a.rgbBlendOperation != z.rgbBlendOperation) o[@"rgb_blend_operation"] = NV4_ENUM_OUT(MTLBlendOperation, a.rgbBlendOperation);
        if (a.alphaBlendOperation != z.alphaBlendOperation) o[@"alpha_blend_operation"] = NV4_ENUM_OUT(MTLBlendOperation, a.alphaBlendOperation);
        if (a.sourceRGBBlendFactor != z.sourceRGBBlendFactor) o[@"source_rgb_blend_factor"] = NV4_ENUM_OUT(MTLBlendFactor, a.sourceRGBBlendFactor);
        if (a.destinationRGBBlendFactor != z.destinationRGBBlendFactor) o[@"destination_rgb_blend_factor"] = NV4_ENUM_OUT(MTLBlendFactor, a.destinationRGBBlendFactor);
        if (a.sourceAlphaBlendFactor != z.sourceAlphaBlendFactor) o[@"source_alpha_blend_factor"] = NV4_ENUM_OUT(MTLBlendFactor, a.sourceAlphaBlendFactor);
        if (a.destinationAlphaBlendFactor != z.destinationAlphaBlendFactor) o[@"destination_alpha_blend_factor"] = NV4_ENUM_OUT(MTLBlendFactor, a.destinationAlphaBlendFactor);
        [out addObject:o];
        if (o.count) last = i;
    }
    return last < 0 ? nil : @{ @"v": [out subarrayWithRange:NSMakeRange(0, last + 1)] };
}
NV4_AVAIL static void nv4_decode_attachments(NSArray *a, MTL4RenderPipelineColorAttachmentDescriptorArray *arr) {
    for (NSUInteger i = 0; i < a.count && i < 8; i++) {
        NSDictionary *o = a[i]; MTL4RenderPipelineColorAttachmentDescriptor *t = arr[i];
        if (o[@"pixel_format"]) t.pixelFormat = (MTLPixelFormat)NV4_ENUM_IN(MTLPixelFormat, o[@"pixel_format"]);
        if (o[@"write_mask"]) t.writeMask = (MTLColorWriteMask)[o[@"write_mask"] unsignedIntegerValue];
        if (o[@"blending_state"]) t.blendingState = (MTL4BlendState)nv4_two_state_in(o[@"blending_state"], @"Disabled", @"Enabled", @"Unspecialized");
        if (o[@"rgb_blend_operation"]) t.rgbBlendOperation = (MTLBlendOperation)NV4_ENUM_IN(MTLBlendOperation, o[@"rgb_blend_operation"]);
        if (o[@"alpha_blend_operation"]) t.alphaBlendOperation = (MTLBlendOperation)NV4_ENUM_IN(MTLBlendOperation, o[@"alpha_blend_operation"]);
        if (o[@"source_rgb_blend_factor"]) t.sourceRGBBlendFactor = (MTLBlendFactor)NV4_ENUM_IN(MTLBlendFactor, o[@"source_rgb_blend_factor"]);
        if (o[@"destination_rgb_blend_factor"]) t.destinationRGBBlendFactor = (MTLBlendFactor)NV4_ENUM_IN(MTLBlendFactor, o[@"destination_rgb_blend_factor"]);
        if (o[@"source_alpha_blend_factor"]) t.sourceAlphaBlendFactor = (MTLBlendFactor)NV4_ENUM_IN(MTLBlendFactor, o[@"source_alpha_blend_factor"]);
        if (o[@"destination_alpha_blend_factor"]) t.destinationAlphaBlendFactor = (MTLBlendFactor)NV4_ENUM_IN(MTLBlendFactor, o[@"destination_alpha_blend_factor"]);
    }
}

NV4_AVAIL static NSDictionary *nv4_encode_vertex_descriptor(MTLVertexDescriptor *vd) {
    NSMutableArray *at = [NSMutableArray array], *ly = [NSMutableArray array];
    NSInteger la = -1, ll = -1;
    for (NSInteger i = 0; i < 31; i++) {
        MTLVertexAttributeDescriptor *a = vd.attributes[i];
        NSMutableDictionary *o = [NSMutableDictionary dictionary];
        if (a.format) o[@"format"] = NV4_ENUM_OUT(MTLVertexFormat, a.format);
        if (a.offset) o[@"offset"] = @(a.offset);
        if (a.bufferIndex) o[@"buffer_index"] = @(a.bufferIndex);
        [at addObject:o]; if (o.count) la = i;
        MTLVertexBufferLayoutDescriptor *l = vd.layouts[i];
        NSMutableDictionary *p = [NSMutableDictionary dictionary];
        if (l.stepFunction != MTLVertexStepFunctionPerVertex) p[@"step_function"] = NV4_ENUM_OUT(MTLVertexStepFunction, l.stepFunction);
        if (l.stepRate != 1) p[@"step_rate"] = @(l.stepRate);
        if (l.stride) p[@"stride"] = @(l.stride);
        [ly addObject:p]; if (p.count) ll = i;
    }
    NSMutableDictionary *o = [NSMutableDictionary dictionary];
    if (la >= 0) o[@"attributes"] = [at subarrayWithRange:NSMakeRange(0, la + 1)];
    if (ll >= 0) o[@"layouts"] = [ly subarrayWithRange:NSMakeRange(0, ll + 1)];
    return o;
}
NV4_AVAIL static MTLVertexDescriptor *nv4_decode_vertex_descriptor(NSDictionary *d) {
    MTLVertexDescriptor *vd = [MTLVertexDescriptor vertexDescriptor];
    NSArray *at = d[@"attributes"], *ly = d[@"layouts"];
    for (NSUInteger i = 0; i < at.count && i < 31; i++) {
        NSDictionary *o = at[i];
        if (o[@"format"]) vd.attributes[i].format = (MTLVertexFormat)NV4_ENUM_IN(MTLVertexFormat, o[@"format"]);
        vd.attributes[i].offset = [o[@"offset"] unsignedIntegerValue];
        vd.attributes[i].bufferIndex = [o[@"buffer_index"] unsignedIntegerValue];
    }
    for (NSUInteger i = 0; i < ly.count && i < 31; i++) {
        NSDictionary *o = ly[i];
        if (o[@"step_function"]) vd.layouts[i].stepFunction = (MTLVertexStepFunction)NV4_ENUM_IN(MTLVertexStepFunction, o[@"step_function"]);
        if (o[@"step_rate"]) vd.layouts[i].stepRate = [o[@"step_rate"] unsignedIntegerValue];
        vd.layouts[i].stride = [o[@"stride"] unsignedIntegerValue];
    }
    return vd;
}

// Scalar properties of a descriptor: key in the dictionary, property name, kind. Only a value that differs from a
// fresh descriptor is written.
typedef struct { const char *key; const char *prop; char kind; } nv4_field;  // b bool, u unsigned, s state(ICB), a alpha state
static const nv4_field nv4_compute_fields[] = {
    { "threadgroup_size_is_multiple_of_thread_execution_width", "threadGroupSizeIsMultipleOfThreadExecutionWidth", 'b' },
    { "max_total_threads_per_threadgroup", "maxTotalThreadsPerThreadgroup", 'u' },
    { "support_binary_linking", "supportBinaryLinking", 'b' },
    { "support_indirect_command_buffers", "supportIndirectCommandBuffers", 'i' }, { 0, 0, 0 } };
static const nv4_field nv4_render_fields[] = {
    { "support_vertex_binary_linking", "supportVertexBinaryLinking", 'b' },
    { "support_fragment_binary_linking", "supportFragmentBinaryLinking", 'b' },
    { "max_vertex_amplification_count", "maxVertexAmplificationCount", 'u' },
    { "alpha_to_coverage_state", "alphaToCoverageState", 'c' }, { "alpha_to_one_state", "alphaToOneState", 'o' },
    { "raster_sample_count", "rasterSampleCount", 'u' }, { "rasterization_enabled", "rasterizationEnabled", 'b' },
    { "input_primitive_topology", "inputPrimitiveTopology", 't' },
    { "color_attachment_mapping_state", "colorAttachmentMappingState", 'm' },
    { "support_indirect_command_buffers", "supportIndirectCommandBuffers", 'i' }, { 0, 0, 0 } };
static const nv4_field nv4_mesh_fields[] = {
    { "max_total_threads_per_object_threadgroup", "maxTotalThreadsPerObjectThreadgroup", 'u' },
    { "max_total_threads_per_mesh_threadgroup", "maxTotalThreadsPerMeshThreadgroup", 'u' },
    { "object_threadgroup_size_is_multiple_of_thread_execution_width", "objectThreadgroupSizeIsMultipleOfThreadExecutionWidth", 'b' },
    { "mesh_threadgroup_size_is_multiple_of_thread_execution_width", "meshThreadgroupSizeIsMultipleOfThreadExecutionWidth", 'b' },
    { "payload_memory_length", "payloadMemoryLength", 'u' }, { "max_total_threadgroups_per_mesh_grid", "maxTotalThreadgroupsPerMeshGrid", 'u' },
    { "raster_sample_count", "rasterSampleCount", 'u' }, { "alpha_to_coverage_state", "alphaToCoverageState", 'c' },
    { "alpha_to_one_state", "alphaToOneState", 'o' }, { "rasterization_enabled", "rasterizationEnabled", 'b' },
    { "max_vertex_amplification_count", "maxVertexAmplificationCount", 'u' },
    { "support_object_binary_linking", "supportObjectBinaryLinking", 'b' },
    { "support_mesh_binary_linking", "supportMeshBinaryLinking", 'b' },
    { "support_fragment_binary_linking", "supportFragmentBinaryLinking", 'b' },
    { "color_attachment_mapping_state", "colorAttachmentMappingState", 'm' },
    { "support_indirect_command_buffers", "supportIndirectCommandBuffers", 'i' }, { 0, 0, 0 } };

static id nv4_field_out(char kind, id v) {
    long n = [v longValue];
    switch (kind) {
        case 'b': return @(n != 0);
        case 'u': return @(n);
        case 'i': return nv4_two_state(n, @"Disabled", @"Enabled", nil);
        case 'c': return nv4_two_state(n, @"Disabled", @"Enabled", nil);
        case 'o': return nv4_two_state(n, @"Disabled", @"Enabled", nil);
        case 'm': return nv4_two_state(n, @"Identity", @"Inherited", nil);
        case 't': return NV4_ENUM_OUT(MTLPrimitiveTopologyClass, n);
    }
    return v;
}
static id nv4_field_in(char kind, id v) {
    switch (kind) {
        case 'b': return @([v boolValue]);
        case 'u': return @([v unsignedIntegerValue]);
        case 'i': case 'c': case 'o': return @(nv4_two_state_in(v, @"Disabled", @"Enabled", nil));
        case 'm': return @(nv4_two_state_in(v, @"Identity", @"Inherited", nil));
        case 't': return @(NV4_ENUM_IN(MTLPrimitiveTopologyClass, v));
    }
    return v;
}
static void nv4_put_fields(const nv4_field *f, id desc, id fresh, NSMutableDictionary *o) {
    for (; f->key; f++) {
        id v = [desc valueForKey:@(f->prop)], z = [fresh valueForKey:@(f->prop)];
        if (![v isEqual:z]) o[@(f->key)] = nv4_field_out(f->kind, v);
    }
}
static void nv4_get_fields(const nv4_field *f, NSDictionary *d, id desc) {
    for (; f->key; f++) if (d[@(f->key)]) [desc setValue:nv4_field_in(f->kind, d[@(f->key)]) forKey:@(f->prop)];
}

NV4_AVAIL static NSArray *nv4_size3(MTLSize s) { return @[@(s.width), @(s.height), @(s.depth)]; }
NV4_AVAIL static MTLSize nv4_size3_in(NSArray *a) {
    return a.count == 3 ? MTLSizeMake([a[0] unsignedIntegerValue], [a[1] unsignedIntegerValue], [a[2] unsignedIntegerValue]) : MTLSizeMake(0, 0, 0);
}

#define NV4_FD(dst, key, fdesc) do { if (fdesc) { NSDictionary *_e = nv4_encode_fd(fdesc, cx, err); if (!_e) return nil; dst[@key] = _e; } } while (0)
#define NV4_SL(dst, key, sdesc) do { NSDictionary *_s = sdesc ? nv4_encode_static(sdesc, cx, err) : nil; \
    if (sdesc && !_s) return nil; if (_s.count) dst[@key] = _s; } while (0)

// The kind of a pipeline descriptor: "compute", "render", "mesh" or "tile". The label is carried but is not part of the key.
NV4_AVAIL static NSDictionary *nv4_encode_pipeline(MTL4PipelineDescriptor *pd, NV4Ctx *cx, NSString **kind, NSError **err) {
    NSMutableDictionary *o = [NSMutableDictionary dictionary];
    if (pd.options) o[@"options"] = nv4_encode_options(pd.options);
    if (pd.label.length) o[@"label"] = pd.label;
    if ([pd isKindOfClass:[MTL4ComputePipelineDescriptor class]]) {
        MTL4ComputePipelineDescriptor *d = (MTL4ComputePipelineDescriptor *)pd;
        *kind = @"compute";
        if (!d.computeFunctionDescriptor) { if (err) *err = nv4_err(@"compute pipeline descriptor has no function"); return nil; }
        NV4_FD(o, "compute_function_descriptor", d.computeFunctionDescriptor);
        nv4_put_fields(nv4_compute_fields, d, [MTL4ComputePipelineDescriptor new], o);
        o[@"required_threads_per_threadgroup"] = nv4_size3(d.requiredThreadsPerThreadgroup);
        NV4_SL(o, "static_linking_descriptor", d.staticLinkingDescriptor);
        return o;
    }
    if ([pd isKindOfClass:[MTL4RenderPipelineDescriptor class]]) {
        MTL4RenderPipelineDescriptor *d = (MTL4RenderPipelineDescriptor *)pd;
        *kind = @"render";
        NV4_FD(o, "vertex_function_descriptor", d.vertexFunctionDescriptor);
        NV4_FD(o, "fragment_function_descriptor", d.fragmentFunctionDescriptor);
        NV4_SL(o, "vertex_static_linking_descriptor", d.vertexStaticLinkingDescriptor);
        NV4_SL(o, "fragment_static_linking_descriptor", d.fragmentStaticLinkingDescriptor);
        nv4_put_fields(nv4_render_fields, d, [MTL4RenderPipelineDescriptor new], o);
        if (d.vertexDescriptor) { NSDictionary *v = nv4_encode_vertex_descriptor(d.vertexDescriptor); if (v.count) o[@"vertex_descriptor"] = v; }
        NSDictionary *ca = nv4_encode_attachments(d.colorAttachments);
        if (ca) o[@"color_attachments"] = ca[@"v"];
        return o;
    }
    if ([pd isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]]) {
        MTL4MeshRenderPipelineDescriptor *d = (MTL4MeshRenderPipelineDescriptor *)pd;
        *kind = @"mesh";
        NV4_FD(o, "object_function_descriptor", d.objectFunctionDescriptor);
        NV4_FD(o, "mesh_function_descriptor", d.meshFunctionDescriptor);
        NV4_FD(o, "fragment_function_descriptor", d.fragmentFunctionDescriptor);
        nv4_put_fields(nv4_mesh_fields, d, [MTL4MeshRenderPipelineDescriptor new], o);
        o[@"required_threads_per_object_threadgroup"] = nv4_size3(d.requiredThreadsPerObjectThreadgroup);
        o[@"required_threads_per_mesh_threadgroup"] = nv4_size3(d.requiredThreadsPerMeshThreadgroup);
        NV4_SL(o, "object_static_linking_descriptor", d.objectStaticLinkingDescriptor);
        NV4_SL(o, "mesh_static_linking_descriptor", d.meshStaticLinkingDescriptor);
        NV4_SL(o, "fragment_static_linking_descriptor", d.fragmentStaticLinkingDescriptor);
        NSDictionary *ca = nv4_encode_attachments(d.colorAttachments);
        if (ca) o[@"color_attachments"] = ca[@"v"];
        return o;
    }
    if ([pd isKindOfClass:[MTL4TileRenderPipelineDescriptor class]]) {
        MTL4TileRenderPipelineDescriptor *d = (MTL4TileRenderPipelineDescriptor *)pd;
        *kind = @"tile";
        NV4_FD(o, "tile_function_descriptor", d.tileFunctionDescriptor);
        if (d.rasterSampleCount != 1 && d.rasterSampleCount) o[@"raster_sample_count"] = @(d.rasterSampleCount);
        if (d.threadgroupSizeMatchesTileSize) o[@"threadgroup_size_matches_tile_size"] = @YES;
        if (d.maxTotalThreadsPerThreadgroup) o[@"max_total_threads_per_threadgroup"] = @(d.maxTotalThreadsPerThreadgroup);
        o[@"required_threads_per_threadgroup"] = nv4_size3(d.requiredThreadsPerThreadgroup);
        if (d.supportBinaryLinking) o[@"support_binary_linking"] = @YES;
        NV4_SL(o, "static_linking_descriptor", d.staticLinkingDescriptor);
        NSMutableArray *pf = [NSMutableArray array];
        NSInteger last = -1;
        for (NSInteger i = 0; i < 8; i++) { MTLPixelFormat f = d.colorAttachments[i].pixelFormat; [pf addObject:NV4_ENUM_OUT(MTLPixelFormat, f)]; if (f) last = i; }
        if (last >= 0) o[@"color_attachment_pixel_formats"] = [pf subarrayWithRange:NSMakeRange(0, last + 1)];
        return o;
    }
    if (err) *err = nv4_err([NSString stringWithFormat:@"pipeline descriptor class %s", object_getClassName(pd)]);
    return nil;
}

#define NV4_FD_IN(dst, setter, key) do { if (d[@key]) { id _f = nv4_decode_fd(d[@key], cx, err); if (!_f) return nil; [dst setter:_f]; } } while (0)
#define NV4_SL_IN(dst, setter, key) do { if (d[@key]) { id _s = nv4_decode_static(d[@key], cx, err); if (!_s) return nil; [dst setter:_s]; } } while (0)

NV4_AVAIL static MTL4PipelineDescriptor *nv4_decode_pipeline(NSDictionary *d, NSString *kind, NV4Ctx *cx, NSError **err) {
    MTL4PipelineDescriptor *base;
    if ([kind isEqual:@"compute"]) {
        MTL4ComputePipelineDescriptor *c = [MTL4ComputePipelineDescriptor new]; base = c;
        NV4_FD_IN(c, setComputeFunctionDescriptor, "compute_function_descriptor");
        nv4_get_fields(nv4_compute_fields, d, c);
        if (d[@"required_threads_per_threadgroup"]) c.requiredThreadsPerThreadgroup = nv4_size3_in(d[@"required_threads_per_threadgroup"]);
        NV4_SL_IN(c, setStaticLinkingDescriptor, "static_linking_descriptor");
    } else if ([kind isEqual:@"render"]) {
        MTL4RenderPipelineDescriptor *r = [MTL4RenderPipelineDescriptor new]; base = r;
        NV4_FD_IN(r, setVertexFunctionDescriptor, "vertex_function_descriptor");
        NV4_FD_IN(r, setFragmentFunctionDescriptor, "fragment_function_descriptor");
        NV4_SL_IN(r, setVertexStaticLinkingDescriptor, "vertex_static_linking_descriptor");
        NV4_SL_IN(r, setFragmentStaticLinkingDescriptor, "fragment_static_linking_descriptor");
        nv4_get_fields(nv4_render_fields, d, r);
        if (d[@"vertex_descriptor"]) r.vertexDescriptor = nv4_decode_vertex_descriptor(d[@"vertex_descriptor"]);
        if (d[@"color_attachments"]) nv4_decode_attachments(d[@"color_attachments"], r.colorAttachments);
    } else if ([kind isEqual:@"mesh"]) {
        MTL4MeshRenderPipelineDescriptor *m = [MTL4MeshRenderPipelineDescriptor new]; base = m;
        NV4_FD_IN(m, setObjectFunctionDescriptor, "object_function_descriptor");
        NV4_FD_IN(m, setMeshFunctionDescriptor, "mesh_function_descriptor");
        NV4_FD_IN(m, setFragmentFunctionDescriptor, "fragment_function_descriptor");
        nv4_get_fields(nv4_mesh_fields, d, m);
        if (d[@"required_threads_per_object_threadgroup"]) m.requiredThreadsPerObjectThreadgroup = nv4_size3_in(d[@"required_threads_per_object_threadgroup"]);
        if (d[@"required_threads_per_mesh_threadgroup"]) m.requiredThreadsPerMeshThreadgroup = nv4_size3_in(d[@"required_threads_per_mesh_threadgroup"]);
        NV4_SL_IN(m, setObjectStaticLinkingDescriptor, "object_static_linking_descriptor");
        NV4_SL_IN(m, setMeshStaticLinkingDescriptor, "mesh_static_linking_descriptor");
        NV4_SL_IN(m, setFragmentStaticLinkingDescriptor, "fragment_static_linking_descriptor");
        if (d[@"color_attachments"]) nv4_decode_attachments(d[@"color_attachments"], m.colorAttachments);
    } else if ([kind isEqual:@"tile"]) {
        MTL4TileRenderPipelineDescriptor *t = [MTL4TileRenderPipelineDescriptor new]; base = t;
        NV4_FD_IN(t, setTileFunctionDescriptor, "tile_function_descriptor");
        if (d[@"raster_sample_count"]) t.rasterSampleCount = [d[@"raster_sample_count"] unsignedIntegerValue];
        t.threadgroupSizeMatchesTileSize = [d[@"threadgroup_size_matches_tile_size"] boolValue];
        t.maxTotalThreadsPerThreadgroup = [d[@"max_total_threads_per_threadgroup"] unsignedIntegerValue];
        if (d[@"required_threads_per_threadgroup"]) t.requiredThreadsPerThreadgroup = nv4_size3_in(d[@"required_threads_per_threadgroup"]);
        t.supportBinaryLinking = [d[@"support_binary_linking"] boolValue];
        NV4_SL_IN(t, setStaticLinkingDescriptor, "static_linking_descriptor");
        NSArray *pf = d[@"color_attachment_pixel_formats"];
        for (NSUInteger i = 0; i < pf.count && i < 8; i++) t.colorAttachments[i].pixelFormat = (MTLPixelFormat)NV4_ENUM_IN(MTLPixelFormat, pf[i]);
    } else {
        if (err) *err = nv4_err([NSString stringWithFormat:@"archive entry of kind %@", kind]);
        return nil;
    }
    if (d[@"options"]) base.options = nv4_decode_options(d[@"options"]);
    base.label = d[@"label"];
    return base;
}

// The lookup key: the descriptor without its label, in a canonical serialization.
static NSString *nv4_key(NSString *kind, NSDictionary *enc) {
    NSMutableDictionary *m = [enc mutableCopy];
    [m removeObjectForKey:@"label"];
    // Constant payloads are NSData; the key needs a text form of them.
    NSData *j = [NSPropertyListSerialization dataWithPropertyList:@{ @"k": kind, @"d": m } format:NSPropertyListBinaryFormat_v1_0 options:0 error:NULL];
    // A binary plist does not order dictionary keys, so hash a sorted text rendering instead.
    NSMutableString *s = [NSMutableString string];
    __block void (^walk)(id) = nil;
    walk = ^(id v) {
        if ([v isKindOfClass:[NSDictionary class]]) {
            [s appendString:@"{"];
            for (NSString *k in [[v allKeys] sortedArrayUsingSelector:@selector(compare:)]) { [s appendFormat:@"%@=", k]; walk(v[k]); [s appendString:@";"]; }
            [s appendString:@"}"];
        } else if ([v isKindOfClass:[NSArray class]]) {
            [s appendString:@"["]; for (id e in v) { walk(e); [s appendString:@","]; } [s appendString:@"]"];
        } else if ([v isKindOfClass:[NSData class]]) { [s appendFormat:@"<%@>", [v base64EncodedStringWithOptions:0]]; }
        else [s appendFormat:@"%@", v];
    };
    (void)j;
    walk(@{ @"k": kind, @"d": m });
    walk = nil;   // the block captures itself
    return nv4_sha256hex([s dataUsingEncoding:NSUTF8StringEncoding]);
}

// ---- the pipelines script (the JSON metal-tt reads) ----

// Hoists every inline function descriptor into the script's function table; entries reference it as "fnd:<label>".
NV4_AVAIL static NSData *nv4_pipelines_script(NSArray<NSDictionary *> *entries, NV4Ctx *cx, NSError **err) {
    NSMutableArray *libs = [NSMutableArray array], *libFns = [NSMutableArray array], *specFns = [NSMutableArray array], *stitchFns = [NSMutableArray array];
    NSMutableDictionary *labelOf = [NSMutableDictionary dictionary];
    NSMutableSet *seenLibs = [NSMutableSet set];
    __block NSString *(^hoist)(NSDictionary *) = nil;
    hoist = ^NSString *(NSDictionary *fd) {
        NSString *kind = fd[@"kind"];
        NSMutableDictionary *t = [NSMutableDictionary dictionary];
        if ([kind isEqual:@"library"]) {
            t[@"name"] = fd[@"name"]; t[@"library"] = fd[@"library"];
        } else if ([kind isEqual:@"specialized"]) {
            t[@"function_descriptor"] = hoist(fd[@"function_descriptor"]);
            if (fd[@"specialized_name"]) t[@"specialized_name"] = fd[@"specialized_name"];
            NSMutableArray *cvs = [NSMutableArray array];
            for (NSDictionary *c in fd[@"constant_values"]) {
                NSData *dt = c[@"data"]; long ty = [c[@"type"] longValue];
                id val; NSString *vt;
                switch (ty) {
                    case MTLDataTypeBool: val = @(((const uint8_t *)dt.bytes)[0] != 0); vt = @"ConstantBool"; break;
                    case MTLDataTypeFloat: { float f; memcpy(&f, dt.bytes, 4); val = @(f); vt = @"ConstantFloat"; break; }
                    case MTLDataTypeUInt: { uint32_t u; memcpy(&u, dt.bytes, 4); val = @(u); vt = @"ConstantUInt"; break; }
                    case MTLDataTypeInt: { int32_t u; memcpy(&u, dt.bytes, 4); val = @(u); vt = @"ConstantInt"; break; }
                    default: { uint64_t u = 0; memcpy(&u, dt.bytes, MIN(dt.length, 8)); val = @(u); vt = [NSString stringWithFormat:@"ConstantType%ld", ty]; }
                }
                NSMutableDictionary *e = [NSMutableDictionary dictionary];
                if (c[@"name"]) { e[@"id_type"] = @"FunctionConstantName"; e[@"id"] = c[@"name"]; }
                else { e[@"id_type"] = @"FunctionConstantIndex"; e[@"id"] = @{ @"data": c[@"index"] }; }
                e[@"value_type"] = vt; e[@"value"] = @{ @"data": val };
                [cvs addObject:e];
            }
            if (cvs.count) t[@"constant_values"] = cvs;
        } else {
            NSMutableArray *fns = [NSMutableArray array];
            for (NSDictionary *f in fd[@"function_descriptors"]) [fns addObject:hoist(f)];
            if (fns.count) t[@"function_descriptors"] = fns;
            t[@"function_graph"] = fd[@"function_graph"];
        }
        NSData *id_ = [NSPropertyListSerialization dataWithPropertyList:t format:NSPropertyListXMLFormat_v1_0 options:0 error:NULL];
        NSString *label = nv4_sha256hex(id_ ?: [NSData data]);
        NSString *ref = [@"fnd:" stringByAppendingString:label];
        if (!labelOf[label]) {
            labelOf[label] = @YES;
            NSMutableDictionary *row = [t mutableCopy]; row[@"label"] = label;
            [([kind isEqual:@"library"] ? libFns : [kind isEqual:@"specialized"] ? specFns : stitchFns) addObject:row];
        }
        return ref;
    };
    NSMutableDictionary *pipes = [NSMutableDictionary dictionary];
    for (NSDictionary *en in entries) {
        NSMutableDictionary *p = [en[@"descriptor"] mutableCopy];
        [p removeObjectForKey:@"label"];
        for (NSString *k in p.allKeys) {
            id v = p[k];
            if ([k hasSuffix:@"_function_descriptor"] && [v isKindOfClass:[NSDictionary class]]) p[k] = hoist(v);
            else if ([k hasSuffix:@"static_linking_descriptor"]) {
                NSMutableDictionary *s = [NSMutableDictionary dictionary];
                for (NSString *sk in v) {
                    // Apple's script leaves the groups of a static linking descriptor out (measured on macOS 27); the
                    // archive container keeps them.
                    if ([sk isEqual:@"groups"]) continue;
                    { NSMutableArray *a = [NSMutableArray array]; for (id f in v[sk]) [a addObject:hoist(f)]; s[sk] = a; }
                }
                p[k] = s;
            }
        }
        NSString *bucket = [en[@"kind"] stringByAppendingString:@"_pipeline_descriptors"];
        if (!pipes[bucket]) pipes[bucket] = [NSMutableArray array];
        [pipes[bucket] addObject:p];
    }
    hoist = nil;   // the block captures itself
    for (NSString *h in cx.bytesByHash) {
        if ([seenLibs containsObject:h]) continue;
        [seenLibs addObject:h];
        NSMutableDictionary *l = [NSMutableDictionary dictionaryWithObject:h forKey:@"label"];
        if (cx.pathByHash[h]) l[@"path"] = cx.pathByHash[h];
        [libs addObject:l];
    }
    NSMutableDictionary *fnTable = [NSMutableDictionary dictionary];
    if (libFns.count) fnTable[@"library_function_descriptors"] = libFns;
    if (specFns.count) fnTable[@"specialized_function_descriptors"] = specFns;
    if (stitchFns.count) fnTable[@"stitched_function_descriptors"] = stitchFns;
    NSDictionary *doc = @{ @"version": @{ @"major": @0, @"minor": @1, @"sub_minor": @0 }, @"generator": @"NullMothMetalDriver",
                           @"libraries": libs, @"function_descriptors": fnTable, @"pipeline_descriptors": pipes };
    NSData *json = [NSJSONSerialization dataWithJSONObject:doc options:NSJSONWritingPrettyPrinted | NSJSONWritingSortedKeys error:err];
    return json;
}

// ---- containers ----

// Our own archive: a binary property list.
//   format "NVMTLArchive", version 2
//   libraries: sha256 -> the complete metallib (its AIR is what the translator reads)
//   entries: [{ kind, key, descriptor, label }]
// An archive written by Apple's toolchain is a fat container (magic 0xCBFEBABE, big endian) with air64 MetalLib
// slices; those carry the functions of every pipeline in the archive as AIR.
NV4_AVAIL static NSData *nv4_write_container(NSArray<NSDictionary *> *entries, NV4Ctx *cx, NSError **err) {
    NSMutableDictionary *libs = [NSMutableDictionary dictionary];
    @synchronized (cx) { [libs addEntriesFromDictionary:cx.bytesByHash]; }
    NSDictionary *doc = @{ @"format": @"NVMTLArchive", @"version": @2, @"libraries": libs, @"entries": entries };
    return [NSPropertyListSerialization dataWithPropertyList:doc format:NSPropertyListBinaryFormat_v1_0 options:0 error:err];
}
static NSDictionary *nv4_read_container(NSData *data) {
    id pl = [NSPropertyListSerialization propertyListWithData:data options:NSPropertyListImmutable format:NULL error:NULL];
    if ([pl isKindOfClass:[NSDictionary class]] && [pl[@"format"] isEqual:@"NVMTLArchive"] && [pl[@"version"] integerValue] == 2) return pl;
    return nil;
}

typedef struct { uint32_t cputype, subtype, offset, size; } nv4_slice;
static uint32_t nv4_be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }
// The air64 slices of an Apple archive, newest AIR version first.
static NSArray<NSData *> *nv4_apple_air_slices(NSData *data) {
    const uint8_t *b = data.bytes; NSUInteger n = data.length;
    if (n < 8 || nv4_be32(b) != 0xCBFEBABEu) return nil;
    uint32_t cnt = nv4_be32(b + 4);
    if (cnt > 64 || 8 + (NSUInteger)cnt * 20 > n) return nil;
    NSMutableArray<NSDictionary *> *air = [NSMutableArray array];
    for (uint32_t i = 0; i < cnt; i++) {
        const uint8_t *e = b + 8 + i * 20;
        nv4_slice s = { nv4_be32(e), nv4_be32(e + 4), nv4_be32(e + 8), nv4_be32(e + 12) };
        if (s.cputype != 0x01000017u) continue;
        if ((NSUInteger)s.offset > n || (NSUInteger)s.size > n - s.offset || s.size < 4 || memcmp(b + s.offset, "MTLB", 4) != 0) continue;
        [air addObject:@{ @"sub": @(s.subtype), @"data": [data subdataWithRange:NSMakeRange(s.offset, s.size)] }];
    }
    [air sortUsingComparator:^NSComparisonResult(NSDictionary *a, NSDictionary *c) { return [c[@"sub"] compare:a[@"sub"]]; }];
    NSMutableArray *out = [NSMutableArray array];
    for (NSDictionary *a in air) [out addObject:a[@"data"]];
    return out;
}
