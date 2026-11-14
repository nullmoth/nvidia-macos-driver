// Oracle for the Metal 4 pipeline data codec (plugin/nvmtl4_codec.h) against Apple's own objects:
//  - descriptors survive encode -> decode -> encode,
//  - the pipelines script matches the one Apple's serializer writes for the same pipelines,
//  - our container round-trips and replays a pipeline that runs,
//  - the air64 slices of an archive Apple wrote can be read and hold the pipeline functions.
// Usage: codec_test lib.metallib
#import "../../plugin/nvmtl4_codec.h"

static int g_fail;
static void arm(const char *name, BOOL ok, NSString *measured) {
    printf("%s %s: %s\n", ok ? "PASS" : "FAIL", name, measured.UTF8String);
    if (!ok) g_fail++;
}

// A script with labels, hashes, paths and the generator removed and function references expanded, so two scripts
// can be compared on content.
static id expand(id v, NSDictionary *table) {
    if ([v isKindOfClass:[NSString class]] && [v hasPrefix:@"fnd:"]) return expand(table[[v substringFromIndex:4]], table);
    if ([v isKindOfClass:[NSDictionary class]]) {
        NSMutableDictionary *o = [NSMutableDictionary dictionary];
        for (NSString *k in v) {
            if ([k isEqual:@"label"] || [k isEqual:@"path"] || [k isEqual:@"generator"]) continue;
            if ([k isEqual:@"library"]) { o[k] = @"LIB"; continue; }
            o[k] = expand(v[k], table);
        }
        return o;
    }
    if ([v isKindOfClass:[NSArray class]]) { NSMutableArray *a = [NSMutableArray array]; for (id e in v) [a addObject:expand(e, table)]; return a; }
    return v;
}
static NSDictionary *script_content(NSData *json) {
    NSDictionary *d = [NSJSONSerialization JSONObjectWithData:json options:0 error:NULL];
    NSMutableDictionary *table = [NSMutableDictionary dictionary];
    for (NSString *group in d[@"function_descriptors"]) for (NSDictionary *f in d[@"function_descriptors"][group]) table[f[@"label"]] = f;
    return expand(d[@"pipeline_descriptors"], table);
}
static NSString *diff(id a, id b, NSString *path) {
    if ([a isEqual:b]) return nil;
    if ([a isKindOfClass:[NSDictionary class]] && [b isKindOfClass:[NSDictionary class]]) {
        NSMutableSet *ks = [NSMutableSet setWithArray:[a allKeys]]; [ks addObjectsFromArray:[b allKeys]];
        for (NSString *k in [ks.allObjects sortedArrayUsingSelector:@selector(compare:)]) {
            NSString *r = diff(a[k], b[k], [path stringByAppendingFormat:@"/%@", k]); if (r) return r;
        }
    }
    if ([a isKindOfClass:[NSArray class]] && [b isKindOfClass:[NSArray class]] && [a count] == [b count]) {
        for (NSUInteger i = 0; i < [a count]; i++) { NSString *r = diff(a[i], b[i], [path stringByAppendingFormat:@"[%lu]", (unsigned long)i]); if (r) return r; }
    }
    return [NSString stringWithFormat:@"%@: ours %@ vs apple %@", path, a, b];
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) return 2;
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        NSError *err = nil;
        NSData *libBytes = [NSData dataWithContentsOfFile:@(argv[1])];
        id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&err];
        NV4Ctx *cx = [NV4Ctx new];
        cx.libraryBytes = ^NSData *(id<MTLLibrary> l) { return libBytes; };
        cx.libraryPath = ^NSString *(id<MTLLibrary> l) { return @(argv[1]); };
        cx.makeLibrary = ^id<MTLLibrary>(NSData *b, NSError **e) {
            return [dev newLibraryWithData:dispatch_data_create(b.bytes, b.length, NULL, DISPATCH_DATA_DESTRUCTOR_DEFAULT) error:e]; };

        #define LF(var, nm) MTL4LibraryFunctionDescriptor *var = [MTL4LibraryFunctionDescriptor new]; var.library = lib; var.name = @nm;
        LF(vf, "v_main") LF(ff, "f_main") LF(k, "k_plain") LF(ks, "k_scale")
        MTL4RenderPipelineDescriptor *r = [MTL4RenderPipelineDescriptor new];
        r.vertexFunctionDescriptor = vf; r.fragmentFunctionDescriptor = ff;
        r.rasterSampleCount = 4; r.alphaToCoverageState = MTL4AlphaToCoverageStateEnabled; r.alphaToOneState = MTL4AlphaToOneStateEnabled;
        r.maxVertexAmplificationCount = 2; r.inputPrimitiveTopology = MTLPrimitiveTopologyClassTriangle;
        r.supportIndirectCommandBuffers = MTL4IndirectCommandBufferSupportStateEnabled; r.supportVertexBinaryLinking = YES;
        r.colorAttachmentMappingState = MTL4LogicalToPhysicalColorAttachmentMappingStateInherited; r.rasterizationEnabled = NO;
        r.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA16Float; r.colorAttachments[0].blendingState = MTL4BlendStateEnabled;
        r.colorAttachments[0].sourceRGBBlendFactor = MTLBlendFactorSourceAlpha; r.colorAttachments[0].destinationRGBBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
        r.colorAttachments[0].rgbBlendOperation = MTLBlendOperationMax; r.colorAttachments[0].writeMask = MTLColorWriteMaskRed | MTLColorWriteMaskAlpha;
        r.colorAttachments[1].pixelFormat = MTLPixelFormatBGRA8Unorm; r.colorAttachments[1].blendingState = MTL4BlendStateUnspecialized;
        MTLVertexDescriptor *vd = [MTLVertexDescriptor vertexDescriptor];
        vd.attributes[0].format = MTLVertexFormatFloat3; vd.attributes[0].offset = 4; vd.attributes[0].bufferIndex = 1;
        vd.layouts[1].stride = 16; vd.layouts[1].stepFunction = MTLVertexStepFunctionPerInstance; vd.layouts[1].stepRate = 2;
        r.vertexDescriptor = vd;
        MTL4StaticLinkingDescriptor *sl = [MTL4StaticLinkingDescriptor new];
        sl.functionDescriptors = @[ks]; sl.privateFunctionDescriptors = @[k]; sl.groups = @{ @"g": @[k] };
        r.vertexStaticLinkingDescriptor = sl;

        MTLFunctionConstantValues *cv = [MTLFunctionConstantValues new];
        uint seven = 7; float f = 2.5f; bool b = true;
        [cv setConstantValue:&seven type:MTLDataTypeUInt atIndex:0]; [cv setConstantValue:&f type:MTLDataTypeFloat atIndex:3];
        [cv setConstantValue:&b type:MTLDataTypeBool withName:@"named"];
        MTL4SpecializedFunctionDescriptor *sp = [MTL4SpecializedFunctionDescriptor new];
        sp.functionDescriptor = ks; sp.constantValues = cv; sp.specializedName = @"spn";
        MTL4ComputePipelineDescriptor *cpd = [MTL4ComputePipelineDescriptor new];
        cpd.computeFunctionDescriptor = sp; cpd.label = @"specialized";
        MTL4ComputePipelineDescriptor *cpl = [MTL4ComputePipelineDescriptor new];
        cpl.computeFunctionDescriptor = k; cpl.threadGroupSizeIsMultipleOfThreadExecutionWidth = YES; cpl.maxTotalThreadsPerThreadgroup = 128;
        cpl.requiredThreadsPerThreadgroup = MTLSizeMake(64, 1, 1); cpl.supportBinaryLinking = YES;
        cpl.supportIndirectCommandBuffers = MTL4IndirectCommandBufferSupportStateEnabled;
        MTL4PipelineOptions *po = [MTL4PipelineOptions new]; po.shaderReflection = MTL4ShaderReflectionBindingInfo | MTL4ShaderReflectionBufferTypeInfo;
        cpl.options = po; cpl.staticLinkingDescriptor = sl;

        NSArray<MTL4PipelineDescriptor *> *all = @[cpl, cpd, r];
        NSMutableArray *entries = [NSMutableArray array];
        for (MTL4PipelineDescriptor *pd in all) {
            NSString *kind = nil; err = nil;
            NSDictionary *e = nv4_encode_pipeline(pd, cx, &kind, &err);
            if (!e) { printf("FAIL encode %s: %s\n", object_getClassName(pd), err.localizedDescription.UTF8String); return 1; }
            [entries addObject:@{ @"kind": kind, @"key": nv4_key(kind, e), @"descriptor": e }];
        }

        // 1. encode -> decode -> encode
        BOOL same = YES; NSString *why = @"";
        for (NSDictionary *en in entries) {
            err = nil;
            MTL4PipelineDescriptor *back = nv4_decode_pipeline(en[@"descriptor"], en[@"kind"], cx, &err);
            NSString *kind = nil;
            NSDictionary *again = back ? nv4_encode_pipeline(back, cx, &kind, &err) : nil;
            NSString *d = again ? diff(again, en[@"descriptor"], @"") : @"decode failed";
            if (d) { same = NO; why = [why stringByAppendingFormat:@"%@ %@; ", en[@"kind"], d]; }
        }
        arm("1 descriptors round-trip through the dictionary form", same, same ? [NSString stringWithFormat:@"%lu pipelines identical", (unsigned long)entries.count] : why);
        NSString *k1 = entries[0][@"key"], *k2 = nil;
        { MTL4ComputePipelineDescriptor *c2 = [cpl copy]; c2.maxTotalThreadsPerThreadgroup = 17; NSString *kd; k2 = nv4_key(@"compute", nv4_encode_pipeline(c2, cx, &kd, &err)); }
        arm("1n NEGATIVE control: one changed field changes the key", ![k1 isEqual:k2], [NSString stringWithFormat:@"%@ vs %@", [k1 substringToIndex:8], [k2 substringToIndex:8]]);
        { MTL4ComputePipelineDescriptor *c3 = [cpl copy]; c3.label = @"another"; NSString *kd; NSString *k3 = nv4_key(@"compute", nv4_encode_pipeline(c3, cx, &kd, &err));
          arm("1l a label does not change the key", [k1 isEqual:k3], [k3 substringToIndex:8]); }

        // 2. our script vs Apple's script for the same pipelines
        MTL4PipelineDataSetSerializerDescriptor *sd = [MTL4PipelineDataSetSerializerDescriptor new];
        sd.configuration = MTL4PipelineDataSetSerializerConfigurationCaptureDescriptors;
        id<MTL4PipelineDataSetSerializer> ser = [dev newPipelineDataSetSerializerWithDescriptor:sd];
        MTL4CompilerDescriptor *cd = [MTL4CompilerDescriptor new]; cd.pipelineDataSetSerializer = ser;
        id<MTL4Compiler> comp = [dev newCompilerWithDescriptor:cd error:&err];
        for (MTL4PipelineDescriptor *pd in all) {
            err = nil;
            if ([pd isKindOfClass:[MTL4ComputePipelineDescriptor class]]) [comp newComputePipelineStateWithDescriptor:(id)pd compilerTaskOptions:nil error:&err];
            else [comp newRenderPipelineStateWithDescriptor:pd compilerTaskOptions:nil error:&err];
        }
        NSData *apple = [ser serializeAsPipelinesScriptWithError:&err];
        NSData *ours = nv4_pipelines_script(entries, cx, &err);
        NSDictionary *ac = script_content(apple), *oc = script_content(ours);
        // Apple lists compute pipelines in compile order too; compare per bucket.
        NSString *d = diff(oc, ac, @"");
        arm("2 pipelines script content equals Apple's for the same pipelines", d == nil, d ?: [NSString stringWithFormat:@"%lu bytes, identical content", (unsigned long)ours.length]);

        // 3. container round trip and replay
        NSData *blob = nv4_write_container(entries, cx, &err);
        NSDictionary *doc = blob ? nv4_read_container(blob) : nil;
        NV4Ctx *cx2 = [NV4Ctx new]; cx2.makeLibrary = cx.makeLibrary;
        [cx2.bytesByHash addEntriesFromDictionary:doc[@"libraries"]];
        NSDictionary *spEntry = doc[@"entries"][1];
        MTL4PipelineDescriptor *replay = nv4_decode_pipeline(spEntry[@"descriptor"], spEntry[@"kind"], cx2, &err);
        id<MTLComputePipelineState> ps = replay ? [comp newComputePipelineStateWithDescriptor:(id)replay compilerTaskOptions:nil error:&err] : nil;
        BOOL ran = NO; NSString *m = @"no pipeline";
        if (ps) {
            id<MTLBuffer> buf = [dev newBufferWithLength:256 options:MTLResourceStorageModeShared];
            id<MTLCommandBuffer> cb = [[dev newCommandQueue] commandBuffer]; id<MTLComputeCommandEncoder> e = [cb computeCommandEncoder];
            [e setComputePipelineState:ps]; [e setBuffer:buf offset:0 atIndex:0];
            [e dispatchThreads:MTLSizeMake(64, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)]; [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            uint *o = buf.contents; ran = (o[5] == 35 && o[63] == 441);
            m = [NSString stringWithFormat:@"out[5]=%u out[63]=%u", o[5], o[63]];
        }
        arm("3 container read back from bytes, specialized pipeline rebuilt from its own library copy and run", doc && ran, m);

        // 4. an archive Apple wrote: air64 slices hold the pipeline functions
        MTL4PipelineDataSetSerializerDescriptor *sd2 = [MTL4PipelineDataSetSerializerDescriptor new];
        sd2.configuration = MTL4PipelineDataSetSerializerConfigurationCaptureBinaries;
        id<MTL4PipelineDataSetSerializer> ser2 = [dev newPipelineDataSetSerializerWithDescriptor:sd2];
        MTL4CompilerDescriptor *cd2 = [MTL4CompilerDescriptor new]; cd2.pipelineDataSetSerializer = ser2;
        id<MTL4Compiler> comp2 = [dev newCompilerWithDescriptor:cd2 error:&err];
        MTL4ComputePipelineDescriptor *kp = [MTL4ComputePipelineDescriptor new]; kp.computeFunctionDescriptor = k;
        [comp2 newComputePipelineStateWithDescriptor:kp compilerTaskOptions:nil error:&err];
        [comp2 newComputePipelineStateWithDescriptor:cpd compilerTaskOptions:nil error:&err];
        NSString *tmp = [NSTemporaryDirectory() stringByAppendingPathComponent:@"codec_test.mtl4archive"];
        [[NSFileManager defaultManager] removeItemAtPath:tmp error:NULL];
        BOOL wrote = [ser2 serializeAsArchiveAndFlushToURL:[NSURL fileURLWithPath:tmp] error:&err];
        NSData *arc = [NSData dataWithContentsOfFile:tmp];
        NSArray<NSData *> *slices = nv4_apple_air_slices(arc);
        NSMutableArray *names = [NSMutableArray array]; BOOL loaded = NO;
        for (NSData *s in slices) {
            id<MTLLibrary> sl2 = cx.makeLibrary(s, &err);
            if (sl2) { loaded = YES; [names addObjectsFromArray:[sl2.functionNames sortedArrayUsingSelector:@selector(compare:)]]; }
        }
        arm("4 Apple's archive yields air64 slices that load as libraries holding the pipeline functions",
            wrote && slices.count >= 1 && loaded && [names containsObject:@"k_plain"] && [names containsObject:@"k_scale"],
            [NSString stringWithFormat:@"wrote=%d airSlices=%lu functions=%@", wrote, (unsigned long)slices.count, names]);
        arm("4n NEGATIVE control: a file that is not an archive yields no slices", nv4_apple_air_slices(libBytes) == nil,
            [NSString stringWithFormat:@"slices=%@", nv4_apple_air_slices(libBytes)]);
        [[NSFileManager defaultManager] removeItemAtPath:tmp error:NULL];
    }
    printf("%s (%d failed)\n", g_fail ? "RESULT FAIL" : "RESULT PASS", g_fail);
    return g_fail ? 1 : 0;
}
