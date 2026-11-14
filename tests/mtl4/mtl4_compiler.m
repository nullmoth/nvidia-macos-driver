// Oracle for the Metal 4 compiler API: MTL4Compiler pipelines built from a metallib, checked by running them.
// Build and run: see build_compiler.sh. Arguments: path to mtl4_compiler.metallib [--missing-name]. Exit status is non-zero on any FAIL.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

static int g_fail;
static void arm(const char *name, BOOL ok, NSString *measured) {
    printf("%s %s: %s\n", ok ? "PASS" : "FAIL", name, measured.UTF8String);
    if (!ok) g_fail++;
}

static NSArray<NSNumber *> *run_scale(id<MTLDevice> dev, id<MTLComputePipelineState> ps, uint n) {
    id<MTLBuffer> buf = [dev newBufferWithLength:n * 4 options:MTLResourceStorageModeShared];
    memset(buf.contents, 0xEE, n * 4);
    id<MTLCommandQueue> q = [dev newCommandQueue];
    id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLComputeCommandEncoder> e = [cb computeCommandEncoder];
    [e setComputePipelineState:ps];
    [e setBuffer:buf offset:0 atIndex:0];
    [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(MIN(n, ps.maxTotalThreadsPerThreadgroup), 1, 1)];
    [e endEncoding];
    [cb commit];
    [cb waitUntilCompleted];
    NSMutableArray *r = [NSMutableArray array];
    for (uint i = 0; i < n; i++) [r addObject:@(((uint *)buf.contents)[i])];
    return r;
}

static BOOL matches(NSArray<NSNumber *> *got, uint mul) {
    for (uint i = 0; i < got.count; i++) if (got[i].unsignedIntValue != i * mul) return NO;
    return YES;
}

static NSArray<NSNumber *> *run_call(id<MTLDevice> dev, id<MTLComputePipelineState> ps, id<MTLFunctionHandle> h, uint n) {
    MTLVisibleFunctionTableDescriptor *td = [MTLVisibleFunctionTableDescriptor new]; td.functionCount = 1;
    id<MTLVisibleFunctionTable> t = [ps newVisibleFunctionTableWithDescriptor:td];
    [t setFunction:h atIndex:0];
    id<MTLBuffer> buf = [dev newBufferWithLength:n * 4 options:MTLResourceStorageModeShared];
    memset(buf.contents, 0xEE, n * 4);
    id<MTLCommandBuffer> cb = [[dev newCommandQueue] commandBuffer];
    id<MTLComputeCommandEncoder> e = [cb computeCommandEncoder];
    [e setComputePipelineState:ps]; [e setBuffer:buf offset:0 atIndex:0]; [e setVisibleFunctionTable:t atBufferIndex:1];
    [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(n, 1, 1)];
    [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
    NSMutableArray *r = [NSMutableArray array];
    for (uint i = 0; i < n; i++) [r addObject:@(((int *)buf.contents)[i])];
    return r;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: %s lib.metallib\n", argv[0]); return 2; }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        if (!dev) { puts("FAIL setup: no Metal device"); return 1; }
        printf("device: %s\n", dev.name.UTF8String);
        NSError *err = nil;
        id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&err];
        if (!lib) { printf("FAIL setup: library: %s\n", err.localizedDescription.UTF8String); return 1; }

        MTL4CompilerDescriptor *cd = [MTL4CompilerDescriptor new];
        cd.label = @"oracle";
        id<MTL4Compiler> comp = [dev newCompilerWithDescriptor:cd error:&err];
        // Apple answers a nil label here even when the descriptor had one, so the label is printed, not asserted.
        arm("compiler created", comp != nil && comp.device == dev,
            [NSString stringWithFormat:@"compiler=%p label=%@ deviceMatches=%d", comp, comp.label, comp.device == dev]);
        if (!comp) return 1;

        // (1) compute pipeline by name -> out[i] = i
        MTL4LibraryFunctionDescriptor *lfd = [MTL4LibraryFunctionDescriptor new];
        lfd.library = lib; lfd.name = @"k_scale";
        MTL4LibraryFunctionDescriptor *plainFd = [MTL4LibraryFunctionDescriptor new];
        plainFd.library = lib; plainFd.name = @"k_plain";
        MTL4ComputePipelineDescriptor *cpd = [MTL4ComputePipelineDescriptor new];
        cpd.computeFunctionDescriptor = plainFd;
        cpd.label = @"k_plain";
        err = nil;
        id<MTLComputePipelineState> plain = [comp newComputePipelineStateWithDescriptor:cpd compilerTaskOptions:nil error:&err];
        NSArray *got = plain ? run_scale(dev, plain, 64) : nil;
        arm("1 compute pipeline by name runs", plain && matches(got, 1),
            [NSString stringWithFormat:@"pipeline=%p out[0..3]=%@ err=%@", plain, got ? [got subarrayWithRange:NSMakeRange(0, 4)] : @"-", err]);

        // (2) specialized: scale = 7 -> out[i] = 7*i
        MTLFunctionConstantValues *cv = [MTLFunctionConstantValues new];
        uint seven = 7;
        [cv setConstantValue:&seven type:MTLDataTypeUInt atIndex:0];
        MTL4SpecializedFunctionDescriptor *sfd = [MTL4SpecializedFunctionDescriptor new];
        sfd.functionDescriptor = lfd; sfd.constantValues = cv;
        MTL4ComputePipelineDescriptor *cps = [MTL4ComputePipelineDescriptor new];
        cps.computeFunctionDescriptor = sfd;
        err = nil;
        id<MTLComputePipelineState> spec = [comp newComputePipelineStateWithDescriptor:cps compilerTaskOptions:nil error:&err];
        NSArray *got7 = spec ? run_scale(dev, spec, 64) : nil;
        arm("2 function constant took effect (out[i] = 7*i)", spec && matches(got7, 7),
            [NSString stringWithFormat:@"out[0..3]=%@ out[63]=%@ err=%@", got7 ? [got7 subarrayWithRange:NSMakeRange(0, 4)] : @"-",
             got7 ? got7[63] : @"-", err]);
        arm("2n NEGATIVE control: the 7x output must NOT equal 1x (proves the check can fire)", got7 && !matches(got7, 1),
            [NSString stringWithFormat:@"matches1x=%d", got7 ? matches(got7, 1) : -1]);

        // (3) render pipeline
        MTL4LibraryFunctionDescriptor *vfd = [MTL4LibraryFunctionDescriptor new];
        vfd.library = lib; vfd.name = @"v_main";
        MTL4LibraryFunctionDescriptor *ffd = [MTL4LibraryFunctionDescriptor new];
        ffd.library = lib; ffd.name = @"f_main";
        MTL4RenderPipelineDescriptor *rpd = [MTL4RenderPipelineDescriptor new];
        rpd.vertexFunctionDescriptor = vfd; rpd.fragmentFunctionDescriptor = ffd;
        rpd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
        rpd.colorAttachments[0].blendingState = MTL4BlendStateEnabled;
        rpd.colorAttachments[0].sourceRGBBlendFactor = MTLBlendFactorSourceAlpha;
        rpd.colorAttachments[0].destinationRGBBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
        rpd.label = @"render";
        err = nil;
        id<MTLRenderPipelineState> rps = [comp newRenderPipelineStateWithDescriptor:rpd compilerTaskOptions:nil error:&err];
        arm("3 render pipeline created", rps != nil, [NSString stringWithFormat:@"pipeline=%p err=%@", rps, err]);

        // (4) async completion fires, and waitUntilCompleted returns after the handler ran
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        __block id<MTLComputePipelineState> asyncPs = nil; __block NSError *asyncErr = nil;
        __block BOOL handlerRan = NO;
        id<MTL4CompilerTask> task = [comp newComputePipelineStateWithDescriptor:cps compilerTaskOptions:nil
            completionHandler:^(id<MTLComputePipelineState> p, NSError *e) { asyncPs = p; asyncErr = e; handlerRan = YES; dispatch_semaphore_signal(sem); }];
        [task waitUntilCompleted];
        long to = dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC));
        NSArray *gotA = asyncPs ? run_scale(dev, asyncPs, 64) : nil;
        arm("4 async compile completion fires", to == 0 && handlerRan && asyncPs && matches(gotA, 7),
            [NSString stringWithFormat:@"timedOut=%ld pipeline=%p status=%ld err=%@", to, asyncPs, (long)task.status, asyncErr]);
        arm("4b task finished and reports its compiler", task.status == MTL4CompilerTaskStatusFinished && task.compiler == comp,
            [NSString stringWithFormat:@"status=%ld", (long)task.status]);

        // (5) negative: source that does not compile -> nil + error. Measured on Apple (M1, macOS 27): a missing function
        // name aborts inside Apple's descriptor code and a vertex function used as a compute function crashed the
        // compiler once, so neither can be a shared negative; the missing-name case runs only with --missing-name.
        MTL4LibraryDescriptor *badSrc = [MTL4LibraryDescriptor new];
        badSrc.source = @"this is not metal source";
        err = nil;
        id<MTLLibrary> badLib = [comp newLibraryWithDescriptor:badSrc error:&err];
        arm("5 uncompilable source returns nil + error", badLib == nil && err != nil,
            [NSString stringWithFormat:@"library=%p err=%@", badLib, err ? @"(error set)" : @"(nil)"]);

        // (6) the same failure reported through the async handler
        __block NSError *asyncBadErr = nil; __block id asyncBadLib = (id)@1;
        id<MTL4CompilerTask> t2 = [comp newLibraryWithDescriptor:badSrc
            completionHandler:^(id<MTLLibrary> l, NSError *e) { asyncBadLib = l; asyncBadErr = e; }];
        [t2 waitUntilCompleted];
        arm("6 async uncompilable source reports nil + error to the handler", asyncBadLib == nil && asyncBadErr != nil,
            [NSString stringWithFormat:@"library=%p err=%@", asyncBadLib, asyncBadErr ? @"(error set)" : @"(nil)"]);

        // (5c) Apple rejects a pipeline with rasterization off whose vertex function returns a position, with nil + error
        // (measured 10 of 10 runs, sync and async). A render-pipeline negative that Apple handles without aborting.
        MTL4RenderPipelineDescriptor *noRast = [MTL4RenderPipelineDescriptor new];
        noRast.vertexFunctionDescriptor = vfd; noRast.rasterizationEnabled = NO;
        err = nil;
        id<MTLRenderPipelineState> nrp = [comp newRenderPipelineStateWithDescriptor:noRast compilerTaskOptions:nil error:&err];
        arm("5c rasterization off with a position-returning vertex function returns nil + error", nrp == nil && err != nil,
            [NSString stringWithFormat:@"pipeline=%p err=%@", nrp, err.localizedDescription]);
        __block id nrpA = (id)@1; __block NSError *nrpE = nil;
        id<MTL4CompilerTask> t3 = [comp newRenderPipelineStateWithDescriptor:noRast compilerTaskOptions:nil
            completionHandler:^(id<MTLRenderPipelineState> q, NSError *e) { nrpA = q; nrpE = e; }];
        [t3 waitUntilCompleted];
        arm("6c same, async: handler gets nil + error", nrpA == nil && nrpE != nil,
            [NSString stringWithFormat:@"pipeline=%p err=%@", nrpA, nrpE.localizedDescription]);

        if (argc > 2 && !strcmp(argv[2], "--missing-name")) {
            MTL4LibraryFunctionDescriptor *gone = [MTL4LibraryFunctionDescriptor new];
            gone.library = lib; gone.name = @"no_such_function";
            MTL4ComputePipelineDescriptor *cpg = [MTL4ComputePipelineDescriptor new];
            cpg.computeFunctionDescriptor = gone;
            err = nil;
            id<MTLComputePipelineState> gps = [comp newComputePipelineStateWithDescriptor:cpg compilerTaskOptions:nil error:&err];
            arm("5b missing function name returns nil + error (driver only)", gps == nil && err != nil,
                [NSString stringWithFormat:@"pipeline=%p err=%@", gps, err.localizedDescription]);
        }

        // (A) stitched function: out[i] = (i + 1) * i, linked statically and called through a visible function table
        {
            MTLFunctionStitchingInputNode *in0 = [[MTLFunctionStitchingInputNode alloc] initWithArgumentIndex:0];
            MTLFunctionStitchingFunctionNode *fa = [[MTLFunctionStitchingFunctionNode alloc] initWithName:@"st_add" arguments:@[in0] controlDependencies:@[]];
            MTLFunctionStitchingFunctionNode *fm = [[MTLFunctionStitchingFunctionNode alloc] initWithName:@"st_mul" arguments:@[fa, in0] controlDependencies:@[]];
            MTLFunctionStitchingGraph *g = [[MTLFunctionStitchingGraph alloc] initWithFunctionName:@"st_graph" nodes:@[fa] outputNode:fm attributes:@[]];
            MTL4LibraryFunctionDescriptor *da = [MTL4LibraryFunctionDescriptor new]; da.library = lib; da.name = @"st_add";
            MTL4LibraryFunctionDescriptor *dm = [MTL4LibraryFunctionDescriptor new]; dm.library = lib; dm.name = @"st_mul";
            MTL4StitchedFunctionDescriptor *st = [MTL4StitchedFunctionDescriptor new]; st.functionGraph = g; st.functionDescriptors = @[da, dm];
            MTL4StaticLinkingDescriptor *sl = [MTL4StaticLinkingDescriptor new]; sl.functionDescriptors = @[st];
            MTL4LibraryFunctionDescriptor *kc = [MTL4LibraryFunctionDescriptor new]; kc.library = lib; kc.name = @"k_call";
            MTL4ComputePipelineDescriptor *cp = [MTL4ComputePipelineDescriptor new]; cp.computeFunctionDescriptor = kc; cp.staticLinkingDescriptor = sl;
            err = nil;
            id<MTLComputePipelineState> sp = [comp newComputePipelineStateWithDescriptor:cp compilerTaskOptions:nil error:&err];
            id<MTLFunctionHandle> h = [sp functionHandleWithName:@"st_graph"];
            NSArray *o = (sp && h) ? run_call(dev, sp, h, 8) : nil;
            BOOL ok = o && [o[0] intValue] == 0 && [o[1] intValue] == 2 && [o[7] intValue] == 56;
            arm("A stitched function runs: out[i] = (i+1)*i", ok, [NSString stringWithFormat:@"pipeline=%p handle=%p out=%@ err=%@", sp, h, o, err.localizedDescription]);
            arm("An NEGATIVE control: a name that is not linked has no handle", sp && [sp functionHandleWithName:@"not_linked"] == nil, @"nil expected");
        }

        // (B) binary function: out[i] = i + 100, handle from the pipeline that links it
        {
            MTL4LibraryFunctionDescriptor *vis = [MTL4LibraryFunctionDescriptor new]; vis.library = lib; vis.name = @"vis_add";
            MTL4BinaryFunctionDescriptor *bd = [MTL4BinaryFunctionDescriptor new]; bd.name = @"binvis"; bd.functionDescriptor = vis;
            err = nil;
            id<MTL4BinaryFunction> bf = [comp newBinaryFunctionWithDescriptor:bd compilerTaskOptions:nil error:&err];
            MTL4LibraryFunctionDescriptor *kc = [MTL4LibraryFunctionDescriptor new]; kc.library = lib; kc.name = @"k_call";
            MTL4ComputePipelineDescriptor *cp = [MTL4ComputePipelineDescriptor new]; cp.computeFunctionDescriptor = kc; cp.supportBinaryLinking = YES;
            MTL4PipelineStageDynamicLinkingDescriptor *dl = [MTL4PipelineStageDynamicLinkingDescriptor new];
            dl.binaryLinkedFunctions = bf ? @[bf] : @[]; dl.maxCallStackDepth = 2;
            id<MTLComputePipelineState> bp = bf ? [comp newComputePipelineStateWithDescriptor:cp dynamicLinkingDescriptor:dl compilerTaskOptions:nil error:&err] : nil;
            id<MTLFunctionHandle> h = [bp functionHandleWithBinaryFunction:bf];
            NSArray *o = (bp && h) ? run_call(dev, bp, h, 8) : nil;
            arm("B binary function linked into a pipeline runs: out[i] = i+100", o && [o[0] intValue] == 100 && [o[7] intValue] == 107,
                [NSString stringWithFormat:@"binary=%p pipeline=%p handle=%p out=%@ err=%@", bf, bp, h, o, err.localizedDescription]);
            arm("Bn NEGATIVE control: a device-level handle for a binary function is nil, as on Apple", bf && [dev functionHandleWithBinaryFunction:bf] == nil, @"nil expected");
            // a pipeline made without the function, then extended with it
            MTL4ComputePipelineDescriptor *cp2 = [cp copy];
            id<MTLComputePipelineState> bare = [comp newComputePipelineStateWithDescriptor:cp2 compilerTaskOptions:nil error:&err];
            id<MTLComputePipelineState> more = bf ? [bare newComputePipelineStateWithBinaryFunctions:@[bf] error:&err] : nil;
            id<MTLFunctionHandle> h2 = [more functionHandleWithBinaryFunction:bf];
            NSArray *o2 = (more && h2) ? run_call(dev, more, h2, 8) : nil;
            arm("Bx a pipeline extended with a binary function runs it", o2 && [o2[7] intValue] == 107,
                [NSString stringWithFormat:@"bare=%p more=%p out=%@ err=%@", bare, more, o2, err.localizedDescription]);
        }

        // (C) reflection only when the request asked for it
        {
            MTL4ComputePipelineDescriptor *cr = [MTL4ComputePipelineDescriptor new]; cr.computeFunctionDescriptor = plainFd;
            MTL4PipelineOptions *po = [MTL4PipelineOptions new]; po.shaderReflection = MTL4ShaderReflectionBindingInfo;
            cr.options = po;
            id<MTLComputePipelineState> with = [comp newComputePipelineStateWithDescriptor:cr compilerTaskOptions:nil error:&err];
            id<MTLComputePipelineState> without = [comp newComputePipelineStateWithDescriptor:cpd compilerTaskOptions:nil error:&err];
            arm("C reflection present when requested", with.reflection != nil, [NSString stringWithFormat:@"reflection=%p", with.reflection]);
            arm("Cn NEGATIVE control: no reflection when not requested", without.reflection == nil, [NSString stringWithFormat:@"reflection=%p", without.reflection]);
        }

        // (D) unspecialized blend state, then specialization
        {
            MTL4RenderPipelineDescriptor *ur = [MTL4RenderPipelineDescriptor new];
            ur.vertexFunctionDescriptor = vfd; ur.fragmentFunctionDescriptor = ffd;
            ur.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm; ur.colorAttachments[0].blendingState = MTL4BlendStateUnspecialized;
            err = nil;
            id<MTLRenderPipelineState> un = [comp newRenderPipelineStateWithDescriptor:ur compilerTaskOptions:nil error:&err];
            MTL4PipelineDescriptor *sd = [un newRenderPipelineDescriptorForSpecialization];
            BOOL isRender = [sd isKindOfClass:[MTL4RenderPipelineDescriptor class]];
            MTL4RenderPipelineDescriptor *q = isRender ? (MTL4RenderPipelineDescriptor *)sd : nil;
            arm("D unspecialized pipeline hands out a specialization descriptor with the state unspecialized", un && q && q.colorAttachments[0].blendingState == MTL4BlendStateUnspecialized,
                [NSString stringWithFormat:@"pipeline=%p descriptor=%p state=%ld", un, sd, (long)q.colorAttachments[0].blendingState]);
            q.colorAttachments[0].blendingState = MTL4BlendStateEnabled;
            q.colorAttachments[0].sourceRGBBlendFactor = MTLBlendFactorOne; q.colorAttachments[0].destinationRGBBlendFactor = MTLBlendFactorOne;
            id<MTLRenderPipelineState> specd = q ? [comp newRenderPipelineStateBySpecializationWithDescriptor:q pipeline:un error:&err] : nil;
            arm("Dx specialization builds the pipeline with the blend state fixed", specd != nil && specd != un, [NSString stringWithFormat:@"specialized=%p err=%@", specd, err.localizedDescription]);
        }

        // (E) the data set serializer, an archive file, lookup, and lookup archives
        {
            MTL4PipelineDataSetSerializerDescriptor *sd = [MTL4PipelineDataSetSerializerDescriptor new];
            sd.configuration = MTL4PipelineDataSetSerializerConfigurationCaptureBinaries;
            id<MTL4PipelineDataSetSerializer> ser = [dev newPipelineDataSetSerializerWithDescriptor:sd];
            MTL4CompilerDescriptor *scd = [MTL4CompilerDescriptor new]; scd.pipelineDataSetSerializer = ser;
            id<MTL4Compiler> sc = [dev newCompilerWithDescriptor:scd error:&err];
            [sc newComputePipelineStateWithDescriptor:cpd compilerTaskOptions:nil error:&err];
            MTL4SpecializedFunctionDescriptor *spf = [MTL4SpecializedFunctionDescriptor new]; spf.functionDescriptor = lfd; spf.constantValues = cv;
            MTL4ComputePipelineDescriptor *cps2 = [MTL4ComputePipelineDescriptor new]; cps2.computeFunctionDescriptor = spf;
            [sc newComputePipelineStateWithDescriptor:cps2 compilerTaskOptions:nil error:&err];
            NSString *path = [NSTemporaryDirectory() stringByAppendingPathComponent:@"mtl4_oracle.mtl4archive"];
            [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
            err = nil;
            BOOL wrote = [ser serializeAsArchiveAndFlushToURL:[NSURL fileURLWithPath:path] error:&err];
            id<MTL4Archive> ar = wrote ? [dev newArchiveWithURL:[NSURL fileURLWithPath:path] error:&err] : nil;
            arm("E archive written by the serializer opens", ar != nil, [NSString stringWithFormat:@"wrote=%d archive=%p err=%@", wrote, ar, err.localizedDescription]);
            err = nil;
            id<MTLComputePipelineState> hit = [ar newComputePipelineStateWithDescriptor:cpd error:&err];
            NSArray *oh = hit ? run_scale(dev, hit, 8) : nil;
            arm("Ea lookup of a recorded pipeline builds it from the archive and it runs", hit && matches(oh, 1), [NSString stringWithFormat:@"pipeline=%p out=%@ err=%@", hit, oh, err.localizedDescription]);
            MTL4ComputePipelineDescriptor *other = [MTL4ComputePipelineDescriptor new];
            MTL4LibraryFunctionDescriptor *unrec = [MTL4LibraryFunctionDescriptor new]; unrec.library = lib; unrec.name = @"st_add";
            other.computeFunctionDescriptor = unrec; err = nil;
            id<MTLComputePipelineState> miss = [ar newComputePipelineStateWithDescriptor:other error:&err];
            arm("En NEGATIVE control: a pipeline that was never recorded is a miss with an error", miss == nil && err != nil, [NSString stringWithFormat:@"pipeline=%p err=%@", miss, err.localizedDescription]);
            MTL4CompilerTaskOptions *lo = [MTL4CompilerTaskOptions new]; lo.lookupArchives = ar ? @[ar] : @[];
            id<MTLComputePipelineState> viaOpt = [comp newComputePipelineStateWithDescriptor:cpd compilerTaskOptions:lo error:&err];
            NSArray *ov = viaOpt ? run_scale(dev, viaOpt, 8) : nil;
            arm("Eb compile with lookupArchives gives a working pipeline", viaOpt && matches(ov, 1), [NSString stringWithFormat:@"pipeline=%p out=%@", viaOpt, ov]);
            // the pipelines script
            MTL4PipelineDataSetSerializerDescriptor *dd = [MTL4PipelineDataSetSerializerDescriptor new];
            dd.configuration = MTL4PipelineDataSetSerializerConfigurationCaptureDescriptors;
            id<MTL4PipelineDataSetSerializer> ds = [dev newPipelineDataSetSerializerWithDescriptor:dd];
            MTL4CompilerDescriptor *dcd = [MTL4CompilerDescriptor new]; dcd.pipelineDataSetSerializer = ds;
            id<MTL4Compiler> dc = [dev newCompilerWithDescriptor:dcd error:&err];
            [dc newComputePipelineStateWithDescriptor:cps2 compilerTaskOptions:nil error:&err];
            err = nil;
            NSData *script = [ds serializeAsPipelinesScriptWithError:&err];
            NSDictionary *js = script ? [NSJSONSerialization JSONObjectWithData:script options:0 error:NULL] : nil;
            NSArray *specs = js[@"function_descriptors"][@"specialized_function_descriptors"];
            BOOL scriptOk = [js[@"pipeline_descriptors"][@"compute_pipeline_descriptors"] count] == 1 && specs.count == 1
                         && [specs[0][@"constant_values"][0][@"value_type"] isEqual:@"ConstantUInt"] && [specs[0][@"constant_values"][0][@"value"][@"data"] intValue] == 7;
            arm("Es pipelines script holds the specialized pipeline with its constant", scriptOk, [NSString stringWithFormat:@"%lu bytes err=%@", (unsigned long)script.length, err.localizedDescription]);
            [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
        }

        // (F) Metal 3 binary archive: add, save, reopen, and fail-on-miss against what was saved
        {
            MTLBinaryArchiveDescriptor *bad = [MTLBinaryArchiveDescriptor new];
            err = nil;
            id<MTLBinaryArchive> ba = [dev newBinaryArchiveWithDescriptor:bad error:&err];
            MTLComputePipelineDescriptor *cd3 = [MTLComputePipelineDescriptor new];
            cd3.computeFunction = [lib newFunctionWithName:@"k_plain"];
            BOOL added = [ba addComputePipelineFunctionsWithDescriptor:cd3 error:&err];
            NSString *path = [NSTemporaryDirectory() stringByAppendingPathComponent:@"mtl3_oracle.metallib-archive"];
            [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
            BOOL saved = added && [ba serializeToURL:[NSURL fileURLWithPath:path] error:&err];
            MTLBinaryArchiveDescriptor *od = [MTLBinaryArchiveDescriptor new]; od.url = [NSURL fileURLWithPath:path];
            id<MTLBinaryArchive> re = saved ? [dev newBinaryArchiveWithDescriptor:od error:&err] : nil;
            cd3.binaryArchives = re ? @[re] : @[];
            id<MTLComputePipelineState> ok3 = re ? [dev newComputePipelineStateWithDescriptor:cd3 options:MTLPipelineOptionFailOnBinaryArchiveMiss reflection:nil error:&err] : nil;
            arm("F recorded pipeline found again in the reopened Metal 3 archive (fail on miss)", ok3 != nil, [NSString stringWithFormat:@"added=%d saved=%d reopened=%p pipeline=%p err=%@", added, saved, re, ok3, err.localizedDescription]);
            MTLComputePipelineDescriptor *cd4 = [MTLComputePipelineDescriptor new];
            cd4.computeFunction = [lib newFunctionWithName:@"st_add"];   // not a kernel; use another kernel name below
            MTLComputePipelineDescriptor *cd5 = [MTLComputePipelineDescriptor new];
            id<MTLFunction> ks5 = nil;
            { MTLFunctionConstantValues *c5 = [MTLFunctionConstantValues new]; uint three = 3; [c5 setConstantValue:&three type:MTLDataTypeUInt atIndex:0];
              ks5 = [lib newFunctionWithName:@"k_scale" constantValues:c5 error:&err]; }
            cd5.computeFunction = ks5; cd5.binaryArchives = re ? @[re] : @[];
            err = nil;
            id<MTLComputePipelineState> miss5 = re ? [dev newComputePipelineStateWithDescriptor:cd5 options:MTLPipelineOptionFailOnBinaryArchiveMiss reflection:nil error:&err] : nil;
            arm("Fn NEGATIVE control: a pipeline not in the archive fails when misses are fatal", re && miss5 == nil && err != nil, [NSString stringWithFormat:@"pipeline=%p err=%@", miss5, err.localizedDescription]);
            [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
        }

        // (7) library from source through the compiler
        MTL4LibraryDescriptor *ld = [MTL4LibraryDescriptor new];
        ld.source = @"#include <metal_stdlib>\nkernel void k_src(device uint *o [[buffer(0)]], uint t [[thread_position_in_grid]]) { o[t] = t + 1; }\n";
        err = nil;
        id<MTLLibrary> srcLib = [comp newLibraryWithDescriptor:ld error:&err];
        id<MTLFunction> srcFn = [srcLib newFunctionWithName:@"k_src"];
        arm("7 library from source via the compiler", srcLib != nil && srcFn != nil,
            [NSString stringWithFormat:@"library=%p function=%p err=%@", srcLib, srcFn, err.localizedDescription]);

        // (8) library descriptor with no source: Apple aborts, so driver only (--missing-name)
        if (argc > 2 && !strcmp(argv[2], "--missing-name")) {
            err = nil;
            id<MTLLibrary> noSrc = [comp newLibraryWithDescriptor:[MTL4LibraryDescriptor new] error:&err];
            arm("8 library descriptor without source returns nil + error (driver only)", noSrc == nil && err != nil,
                [NSString stringWithFormat:@"err=%@", err.localizedDescription]);
        }
        err = nil;
        id<MTL4Archive> ar = [dev newArchiveWithURL:[NSURL fileURLWithPath:@"/nonexistent/none.mtl4archive"] error:&err];
        arm("9 archive that does not exist returns nil + error", ar == nil && err != nil, [NSString stringWithFormat:@"err=%@", err.localizedDescription]);
    }
    printf("%s (%d failed)\n", g_fail ? "RESULT FAIL" : "RESULT PASS", g_fail);
    return g_fail ? 1 : 0;
}
