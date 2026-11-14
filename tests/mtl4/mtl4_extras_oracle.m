// Oracle for the Metal 4 / macOS 26-27 device surface that is not queue, encoder or compiler work:
// counter heaps, tensors, texture view pools, sparse tiers, texture views by descriptor.
//
// Run it on an Apple GPU first (that is ground truth) and then on the NVIDIA device; both must print the same arms.
//   PASS / FAIL  an arm with the value it measured
//   CONTROL      a negative arm: the check is built to fail and the arm passes only when it does
//   SKIP         a prerequisite is missing on this device (named); exit code 2 when nothing FAILed but something skipped
//   INFO         a measured difference that is expected (reported, never asserted)
//
// usage: mtl4_extras_oracle [path/to/mtl4_extras.metallib]
#import <Metal/Metal.h>
#import <objc/message.h>
#include <string.h>
#include <stdio.h>

static int g_pass, g_fail, g_skip;
static int g_no_render;   // set by the plugin-logic harness, whose stand-in textures cannot be rendered to

#define ARM(name, cond, ...) do { if (cond) { g_pass++; printf("PASS    %s: ", name); } else { g_fail++; printf("FAIL    %s: ", name); } \
    printf(__VA_ARGS__); printf("\n"); } while (0)
#define CONTROL(name, failed, ...) do { if (failed) { g_pass++; printf("CONTROL %s: fired as built: ", name); } else { g_fail++; printf("FAIL    %s: control did NOT fire: ", name); } \
    printf(__VA_ARGS__); printf("\n"); } while (0)
#define SKIP(name, why) do { g_skip++; printf("SKIP    %s: %s\n", name, why); } while (0)
#define INFO(...) do { printf("INFO    "); printf(__VA_ARGS__); printf("\n"); } while (0)

static MTLTensorExtents *E(int n, ...)
{
    NSInteger v[16]; va_list a; va_start(a, n);
    for (int i = 0; i < n; i++) v[i] = va_arg(a, int);
    va_end(a);
    return [[MTLTensorExtents alloc] initWithRank:n values:v];
}

static BOOL wait_event(id<MTLSharedEvent> ev, uint64_t v) { return [ev waitUntilSignaledValue:v timeoutMS:20000]; }

// ----------------------------------------------------------------------------------------------------------------
static void counter_heap_host(id<MTLDevice> d)
{
    const uint64_t freq = [d queryTimestampFrequency];
    ARM("heap: timestamp frequency is a real tick rate", freq >= 1000000ull && freq <= 10000000000ull, "%llu Hz", (unsigned long long)freq);
    ARM("heap: a timestamp entry is 8 bytes", [d sizeOfCounterHeapEntry:MTL4CounterHeapTypeTimestamp] == 8, "%lu", (unsigned long)[d sizeOfCounterHeapEntry:MTL4CounterHeapTypeTimestamp]);
    ARM("heap: an invalid-type entry is 0 bytes", [d sizeOfCounterHeapEntry:MTL4CounterHeapTypeInvalid] == 0, "%lu", (unsigned long)[d sizeOfCounterHeapEntry:MTL4CounterHeapTypeInvalid]);

    MTL4CounterHeapDescriptor *hd = [MTL4CounterHeapDescriptor new];
    hd.type = MTL4CounterHeapTypeTimestamp; hd.count = 8;
    NSError *e = nil; id<MTL4CounterHeap> h = [d newCounterHeapWithDescriptor:hd error:&e];
    ARM("heap: a timestamp heap of 8 is made", h && !e, "heap %p count %lu type %ld", h, (unsigned long)h.count, (long)h.type);
    if (!h) return;
    ARM("heap: count and type read back", h.count == 8 && h.type == MTL4CounterHeapTypeTimestamp, "count %lu type %ld", (unsigned long)h.count, (long)h.type);
    h.label = @"oracle"; ARM("heap: label round trips", [h.label isEqualToString:@"oracle"], "%s", h.label.UTF8String);
    NSData *r = [h resolveCounterRange:NSMakeRange(0, 8)];
    ARM("heap: a fresh heap resolves to 8 entries of 8 bytes", r.length == 64, "%lu bytes", (unsigned long)r.length);
    ARM("heap: a range past the end resolves to nil", [h resolveCounterRange:NSMakeRange(6, 8)] == nil, "nil");
    ARM("heap: an empty range resolves to nil", [h resolveCounterRange:NSMakeRange(0, 0)] == nil, "nil");
    ARM("heap: the last entry resolves", [h resolveCounterRange:NSMakeRange(7, 1)].length == 8, "8 bytes");
    [h invalidateCounterRange:NSMakeRange(0, 8)];
    NSData *z = [h resolveCounterRange:NSMakeRange(0, 8)];
    BOOL zero = z.length == 64; for (NSUInteger i = 0; zero && i < 8; i++) zero = ((const uint64_t *)z.bytes)[i] == 0;
    ARM("heap: invalidating the heap zeroes every entry", zero, "all 8 entries zero");

    hd.count = 0; e = nil;
    ARM("heap: a heap of 0 is refused with an error", [d newCounterHeapWithDescriptor:hd error:&e] == nil && e, "%s", e.localizedDescription.UTF8String);
    hd.count = 4; hd.type = MTL4CounterHeapTypeInvalid; e = nil;
    ARM("heap: an invalid type is refused with an error", [d newCounterHeapWithDescriptor:hd error:&e] == nil && e, "%s", e.localizedDescription.UTF8String);
    hd.type = MTL4CounterHeapTypeTimestamp; hd.count = 1u << 20; e = nil;
    ARM("heap: a heap of a million entries is refused with an error", [d newCounterHeapWithDescriptor:hd error:&e] == nil && e, "%s", e.localizedDescription.UTF8String);
    {   // control: claims an invalidated heap holds a non-zero entry; that claim must be false
        NSData *f = [h resolveCounterRange:NSMakeRange(0, 8)]; BOOL nz = NO;
        for (NSUInteger i = 0; i < 8; i++) nz |= ((const uint64_t *)f.bytes)[i] != 0;
        CONTROL("heap: the claim 'an invalidated heap has a non-zero entry' is false", !nz, "all zero");
    }
}

// Writes a timestamp, runs a kernel, writes another, resolves both ways, on a Metal 4 queue.
static void counter_heap_gpu(id<MTLDevice> d, NSString *libPath)
{
    if (![d respondsToSelector:@selector(newMTL4CommandQueue)]) { SKIP("heap gpu", "device has no newMTL4CommandQueue"); return; }
    id<MTL4CommandQueue> q = [d newMTL4CommandQueue];
    id<MTL4CommandAllocator> alloc = [d newCommandAllocator];
    id<MTL4CommandBuffer> cb = [d newCommandBuffer];
    if (!q || !alloc || !cb) { SKIP("heap gpu", "Metal 4 queue, allocator or command buffer missing on this device"); return; }
    NSError *e = nil;
    id<MTLLibrary> lib = libPath ? [d newLibraryWithURL:[NSURL fileURLWithPath:libPath] error:&e] : nil;
    id<MTLFunction> fn = [lib newFunctionWithName:@"spin"];
    id<MTLComputePipelineState> pso = fn ? [d newComputePipelineStateWithFunction:fn error:&e] : nil;
    if (!pso) { SKIP("heap gpu", "the spin kernel could not be loaded (build with build_extras.sh)"); return; }

    MTL4CounterHeapDescriptor *hd = [MTL4CounterHeapDescriptor new]; hd.type = MTL4CounterHeapTypeTimestamp; hd.count = 4;
    id<MTL4CounterHeap> heap = [d newCounterHeapWithDescriptor:hd error:&e];
    const NSUInteger N = 1 << 16;
    id<MTLBuffer> out = [d newBufferWithLength:N * 4 options:MTLResourceStorageModeShared];
    id<MTLBuffer> resolved = [d newBufferWithLength:64 options:MTLResourceStorageModeShared];
    memset(resolved.contents, 0xAB, 64);
    MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new]; ad.maxBufferBindCount = 1;
    id<MTL4ArgumentTable> table = [d newArgumentTableWithDescriptor:ad error:&e];
    MTLResidencySetDescriptor *rd = [MTLResidencySetDescriptor new];
    id<MTLResidencySet> rs = [d newResidencySetWithDescriptor:rd error:&e];
    if (!heap || !table || !rs) { SKIP("heap gpu", "argument table or residency set could not be made"); return; }
    [rs addAllocation:out]; [rs addAllocation:resolved]; [rs commit]; [q addResidencySet:rs];
    [table setAddress:out.gpuAddress atIndex:0];

    [cb beginCommandBufferWithAllocator:alloc];
    [cb writeTimestampIntoHeap:heap atIndex:0];
    id<MTL4ComputeCommandEncoder> enc = [cb computeCommandEncoder];
    [enc setComputePipelineState:pso]; [enc setArgumentTable:table];
    [enc dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
    [enc writeTimestampWithGranularity:MTL4TimestampGranularityPrecise intoHeap:heap atIndex:1];
    [enc endEncoding];
    [cb writeTimestampIntoHeap:heap atIndex:2];
    [cb resolveCounterHeap:heap withRange:NSMakeRange(0, 3) intoBuffer:MTL4BufferRangeMake(resolved.gpuAddress, 24) waitFence:nil updateFence:nil];
    [cb endCommandBuffer];
    id<MTLSharedEvent> ev = [d newSharedEvent];
    id<MTL4CommandBuffer> cbs[1] = { cb };
    [q commit:cbs count:1];
    [q signalEvent:ev value:1];
    const BOOL done = wait_event(ev, 1);
    ARM("heap gpu: the command buffer completes", done, "%s", done ? "signalled" : "timeout");
    if (!done) return;

    NSData *r = [heap resolveCounterRange:NSMakeRange(0, 3)];
    const uint64_t *t = r.length == 24 ? r.bytes : NULL;
    const uint64_t freq = [d queryTimestampFrequency];
    ARM("heap gpu: three timestamps resolve non-zero", t && t[0] && t[1] && t[2], "%llu %llu %llu", t ? (unsigned long long)t[0] : 0ull, t ? (unsigned long long)t[1] : 0ull, t ? (unsigned long long)t[2] : 0ull);
    ARM("heap gpu: timestamps increase across the kernel", t && t[1] > t[0] && t[2] >= t[1], "t0 %llu < t1 %llu <= t2 %llu", t ? (unsigned long long)t[0] : 0ull, t ? (unsigned long long)t[1] : 0ull, t ? (unsigned long long)t[2] : 0ull);
    const double sec = t && freq ? (double)(t[1] - t[0]) / (double)freq : -1;
    ARM("heap gpu: the kernel interval is between 0 and 5 s at the reported frequency", sec > 0 && sec < 5.0, "%.6f s at %llu Hz", sec, (unsigned long long)freq);
    ARM("heap gpu: the entry not written stays zero", ({ NSData *u = [heap resolveCounterRange:NSMakeRange(3, 1)]; u.length == 8 && *(const uint64_t *)u.bytes == 0; }), "entry 3 zero");
    ARM("heap gpu: resolveCounterHeap into a buffer matches resolveCounterRange", t && !memcmp(resolved.contents, t, 24), "24 bytes equal");
    const uint32_t *o = out.contents;
    ARM("heap gpu: the kernel ran (out[0] != 0)", o[0] != 0 && o[N - 1] != 0, "out[0] 0x%x out[last] 0x%x", o[0], o[N - 1]);
    CONTROL("heap gpu: the claim 'the first timestamp is after the second' is false", t && !(t[0] > t[1]), "t0 %llu, t1 %llu", t ? (unsigned long long)t[0] : 0ull, t ? (unsigned long long)t[1] : 0ull);
}

// ----------------------------------------------------------------------------------------------------------------
static void tensors(id<MTLDevice> d)
{
    NSError *e = nil;
    MTLTensorDescriptor *td = [MTLTensorDescriptor new];
    td.dimensions = E(3, 5, 3, 2); td.dataType = MTLTensorDataTypeFloat32; td.usage = MTLTensorUsageCompute;
    id<MTLTensor> t = [d newTensorWithDescriptor:td error:&e];
    ARM("tensor: a packed f32 5x3x2 tensor is made", t && !e, "%p", t);
    if (!t) return;
    ARM("tensor: dimensions read back", t.dimensions.rank == 3 && [t.dimensions extentAtDimensionIndex:0] == 5 && [t.dimensions extentAtDimensionIndex:2] == 2, "rank %lu", (unsigned long)t.dimensions.rank);
    ARM("tensor: data type and usage read back", t.dataType == MTLTensorDataTypeFloat32 && t.usage == MTLTensorUsageCompute, "type %ld usage %lu", (long)t.dataType, (unsigned long)t.usage);
    ARM("tensor: a device-made tensor has no buffer and no strides", t.buffer == nil && t.strides == nil && t.bufferOffset == 0, "buffer %p", t.buffer);
    ARM("tensor: allocated size is the packed 120 bytes", t.allocatedSize == 120, "%lu", (unsigned long)t.allocatedSize);
    ARM("tensor: storage shared, hazard tracked, device matches", t.storageMode == MTLStorageModeShared && t.hazardTrackingMode == MTLHazardTrackingModeTracked && t.device == d,
        "storage %ld hazard %ld", (long)t.storageMode, (long)t.hazardTrackingMode);
    ARM("tensor: resourceOptions is storage|cache|hazard", t.resourceOptions == (MTLResourceStorageModeShared | MTLResourceCPUCacheModeDefaultCache | MTLResourceHazardTrackingModeTracked), "0x%lx", (unsigned long)t.resourceOptions);
    ARM("tensor: gpuResourceID is non-zero", t.gpuResourceID._impl != 0, "%llu", (unsigned long long)t.gpuResourceID._impl);
    t.label = @"t"; ARM("tensor: label round trips", [t.label isEqualToString:@"t"], "%s", t.label.UTF8String);
    ARM("tensor: not in a heap", t.heap == nil && t.heapOffset == 0, "heap %p", t.heap);

    // dimension 0 is innermost: element (x,y) of a 5x3 slice is src[x + 5*y] with host strides (1,5)
    MTLTensorDescriptor *t2 = [MTLTensorDescriptor new]; t2.dimensions = E(2, 5, 3); t2.dataType = MTLTensorDataTypeFloat32;
    id<MTLTensor> m = [d newTensorWithDescriptor:t2 error:&e];
    float src[15]; for (int i = 0; i < 15; i++) src[i] = (float)i;
    [m replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 5, 3) withBytes:src strides:E(2, 1, 5)];
    float dst[15] = {0}; [m getBytes:dst strides:E(2, 1, 5) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 5, 3)];
    ARM("tensor: replace then get round-trips 15 floats", !memcmp(src, dst, sizeof src), "dst[7] %g dst[14] %g", dst[7], dst[14]);
    float sub[6] = {0}; [m getBytes:sub strides:E(2, 1, 2) fromSliceOrigin:E(2, 1, 1) sliceDimensions:E(2, 2, 2)];
    ARM("tensor: a 2x2 slice at (1,1) reads src[6] src[7] src[11] src[12]", sub[0] == 6 && sub[1] == 7 && sub[2] == 11 && sub[3] == 12, "%g %g %g %g", sub[0], sub[1], sub[2], sub[3]);
    float tr[15] = {0}; [m getBytes:tr strides:E(2, 3, 1) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 5, 3)];
    ARM("tensor: host strides transpose the read", tr[1] == 5 && tr[3] == 1 && tr[14] == 14, "tr[1] %g tr[3] %g tr[14] %g", tr[1], tr[3], tr[14]);
    CONTROL("tensor: the claim 'a transposed read equals the source' is false", memcmp(src, tr, sizeof src) != 0, "tr differs from src");

    // a slice that runs off the tensor is clipped, element by element
    MTLTensorDescriptor *t3 = [MTLTensorDescriptor new]; t3.dimensions = E(2, 4, 2); t3.dataType = MTLTensorDataTypeFloat32;
    id<MTLTensor> c = [d newTensorWithDescriptor:t3 error:&e];
    float ones[16]; for (int i = 0; i < 16; i++) ones[i] = (float)(i + 1);
    [c replaceSliceOrigin:E(2, 2, 1) sliceDimensions:E(2, 4, 2) withBytes:ones strides:E(2, 1, 4)];
    float all[8] = {0}; [c getBytes:all strides:E(2, 1, 4) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 4, 2)];
    ARM("tensor: a slice past the edge is clipped, not wrapped", all[0] == 0 && all[4] == 0 && all[6] == 1 && all[7] == 2, "row1 %g %g %g %g", all[4], all[5], all[6], all[7]);
    // A slice whose rank differs from the tensor rank is not asserted: Apple GPUs fault on it (measured); this device refuses it.

    // element sizes of the other plain data types
    struct { MTLTensorDataType dt; NSUInteger es; const char *n; } ty[] = {
        { MTLTensorDataTypeFloat16, 2, "f16" }, { MTLTensorDataTypeBFloat16, 2, "bf16" }, { MTLTensorDataTypeInt8, 1, "i8" },
        { MTLTensorDataTypeUInt8, 1, "u8" }, { MTLTensorDataTypeInt16, 2, "i16" }, { MTLTensorDataTypeInt32, 4, "i32" }, { MTLTensorDataTypeUInt32, 4, "u32" } };
    for (size_t i = 0; i < sizeof ty / sizeof *ty; i++) {
        MTLTensorDescriptor *x = [MTLTensorDescriptor new]; x.dimensions = E(2, 3, 3); x.dataType = ty[i].dt;
        id<MTLTensor> tx = [d newTensorWithDescriptor:x error:&e];
        char n[64]; snprintf(n, sizeof n, "tensor: %s 3x3 allocates 9 elements", ty[i].n);
        ARM(n, tx && tx.allocatedSize == 9 * ty[i].es, "%lu bytes", (unsigned long)tx.allocatedSize);
    }

    // refusals
    MTLTensorDescriptor *bad = [MTLTensorDescriptor new]; bad.dimensions = E(2, 5, 0); bad.dataType = MTLTensorDataTypeFloat32; e = nil;
    ARM("tensor: a zero dimension is refused in MTLTensorDomain", [d newTensorWithDescriptor:bad error:&e] == nil && [e.domain isEqualToString:MTLTensorDomain] && e.code == MTLTensorErrorInvalidDescriptor,
        "%s code %ld", e.domain.UTF8String, (long)e.code);
    bad.dimensions = E(2, 5, 3); bad.dataType = (MTLTensorDataType)MTLTensorDataTypeInt4; e = nil;
    ARM("tensor: a 4-bit tensor of 5x3 is refused with an error (Apple: dimension 0 must align to 32; here: packed types are not built)",
        [d newTensorWithDescriptor:bad error:&e] == nil && e, "%s", [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "].UTF8String);

    // buffer-backed
    id<MTLBuffer> b = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    float *p = b.contents; for (int i = 0; i < 1024; i++) p[i] = -1;
    MTLTensorDescriptor *bd = [MTLTensorDescriptor new];
    bd.dimensions = E(2, 8, 4); bd.strides = E(2, 1, 32); bd.dataType = MTLTensorDataTypeFloat32; bd.usage = MTLTensorUsageCompute;
    id<MTLTensor> bt = [b newTensorWithDescriptor:bd offset:0 error:&e];
    ARM("buffer tensor: made over a shared buffer", bt && !e, "%p", bt);
    if (bt) {
        ARM("buffer tensor: buffer, offset and strides read back", bt.buffer == b && bt.bufferOffset == 0 && bt.strides.rank == 2 && [bt.strides extentAtDimensionIndex:1] == 32, "buffer==b %d", bt.buffer == b);
        float v[32]; for (int i = 0; i < 32; i++) v[i] = 100 + i;
        [bt replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 8, 4) withBytes:v strides:E(2, 1, 8)];
        ARM("buffer tensor: a replace lands in the buffer by the tensor's strides (row 1 starts at element 32)", p[0] == 100 && p[7] == 107 && p[32] == 108 && p[8] == -1, "p[0] %g p[7] %g p[32] %g p[8] %g", p[0], p[7], p[32], p[8]);
        p[33] = 777;
        float back[32] = {0}; [bt getBytes:back strides:E(2, 1, 8) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 8, 4)];
        ARM("buffer tensor: a write to the buffer is visible through the tensor (aliasing)", back[9] == 777, "back[9] %g", back[9]);
        CONTROL("buffer tensor: the claim 'element (4,1) is the aliased 777' is false", back[12] != 777, "element (4,1) is %g", back[12]);
    }
    id<MTLTensor> t128 = ({ e = nil; [b newTensorWithDescriptor:bd offset:128 error:&e]; });
    ARM("buffer tensor: offset 128 is kept", t128 && t128.bufferOffset == 128, "%lu", (unsigned long)t128.bufferOffset);
    struct { const char *n; int strides[3]; int rank; NSUInteger off; } bads[] = {
        { "no strides", {0}, 0, 0 }, { "strides[0] is 2", {2, 16}, 2, 0 }, { "strides rank 1 vs dims rank 2", {1}, 1, 0 },
        { "negative stride", {1, -8}, 2, 0 }, { "offset not a multiple of the element size", {1, 32}, 2, 2 } };
    for (size_t i = 0; i < sizeof bads / sizeof *bads; i++) {
        MTLTensorDescriptor *x = [bd copy];
        x.strides = bads[i].rank ? E(bads[i].rank, bads[i].strides[0], bads[i].strides[1]) : nil;
        if (bads[i].rank == 1) x.strides = E(1, 1);
        e = nil; id<MTLTensor> tx = [b newTensorWithDescriptor:x offset:bads[i].off error:&e];
        char n[96]; snprintf(n, sizeof n, "buffer tensor: refused - %s", bads[i].n);
        ARM(n, tx == nil && e && [e.domain isEqualToString:MTLTensorDomain], "%s", [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "].UTF8String);
    }
    id<MTLBuffer> small = [d newBufferWithLength:64 options:MTLResourceStorageModeShared]; e = nil;
    ARM("buffer tensor: refused - buffer shorter than the strides address (416 bytes)", [small newTensorWithDescriptor:bd offset:0 error:&e] == nil && e, "%s", [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "].UTF8String);
    id<MTLBuffer> priv = [d newBufferWithLength:4096 options:MTLResourceStorageModePrivate]; e = nil;
    ARM("buffer tensor: refused - tensor storage shared but buffer private", [priv newTensorWithDescriptor:bd offset:0 error:&e] == nil && e, "%s", [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "].UTF8String);
    MTLTensorDescriptor *ml = [bd copy]; ml.usage = MTLTensorUsageMachineLearning; ml.strides = E(2, 1, 8); e = nil;
    ARM("buffer tensor: refused - machine learning usage needs the second stride 64-byte aligned", [b newTensorWithDescriptor:ml offset:0 error:&e] == nil && e, "%s", [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "].UTF8String);
    ml.strides = E(2, 1, 16); e = nil;
    ARM("buffer tensor: accepted - machine learning usage with a 64-byte aligned second stride", [b newTensorWithDescriptor:ml offset:0 error:&e] != nil, "%s", e.localizedDescription.UTF8String ?: "ok");

    MTLTensorDescriptor *pd = [MTLTensorDescriptor new]; pd.dimensions = E(1, 16); pd.storageMode = MTLStorageModePrivate; e = nil;
    id<MTLTensor> pt = [d newTensorWithDescriptor:pd error:&e];
    ARM("tensor: a private tensor is made", pt && pt.storageMode == MTLStorageModePrivate, "storage %ld", (long)pt.storageMode);
    float z[16] = {0}; [pt replaceSliceOrigin:E(1, 0) sliceDimensions:E(1, 16) withBytes:z strides:E(1, 1)];
    ARM("tensor: CPU access to a private tensor does not crash", YES, "survived");

    if (@available(macOS 27.0, *)) {
        if ([d respondsToSelector:@selector(newTensorWithDescriptor:attachments:error:)]) {
            MTLTensorBufferAttachments *at = [MTLTensorBufferAttachments new];
            [at setBuffer:b offset:0 forPlane:MTLTensorPlaneTypeData];
            e = nil; id<MTLTensor> a27 = [d newTensorWithDescriptor:bd attachments:at error:&e];
            ARM("tensor 27: a tensor from buffer attachments aliases the buffer", a27 && a27.buffer == b && a27.auxiliaryPlanes.count == 0, "%p aux %lu", a27, (unsigned long)a27.auxiliaryPlanes.count);
        } else SKIP("tensor 27 attachments", "device does not answer newTensorWithDescriptor:attachments:error:");
    } else SKIP("tensor 27 attachments", "needs macOS 27");
}


// Block formats: packed sub-byte elements, FP8, and the macOS 27 scales plane. Host bytes are the packed bytes, with
// slice dimensions and strides counted in elements.
static NSString *msg(NSError *e) { return [e.localizedDescription stringByReplacingOccurrencesOfString:@"\n" withString:@" | "]; }

static void packed_round(id<MTLDevice> d, const char *name, MTLTensorDataType dt, int bits, int stride1Elems)
{
    NSError *e = nil;
    id<MTLBuffer> b = [d newBufferWithLength:65536 options:MTLResourceStorageModeShared];
    memset(b.contents, 0xEE, 65536);
    MTLTensorDescriptor *td = [MTLTensorDescriptor new];
    td.dimensions = E(2, 64, 2); td.strides = E(2, 1, stride1Elems); td.dataType = dt; td.usage = MTLTensorUsageCompute;
    id<MTLTensor> t = [b newTensorWithDescriptor:td offset:0 error:&e];
    char n[160];
    snprintf(n, sizeof n, "packed %s: made over a buffer", name);
    ARM(n, t != nil, "%s", e ? msg(e).UTF8String : "ok");
    if (!t) return;
    uint8_t src[256], dst[256] = {0};
    for (int i = 0; i < 256; i++) src[i] = (uint8_t)(i * 7 + 1);
    const int rowBytes = 64 * bits / 8;
    [t replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 64, 2) withBytes:src strides:E(2, 1, 64)];
    [t getBytes:dst strides:E(2, 1, 64) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 64, 2)];
    snprintf(n, sizeof n, "packed %s: replace then get round-trips %d packed bytes", name, 2 * rowBytes);
    ARM(n, !memcmp(src, dst, 2 * rowBytes), "dst[0] %02x dst[%d] %02x", dst[0], 2 * rowBytes - 1, dst[2 * rowBytes - 1]);
    const uint8_t *m = b.contents; const int row1 = stride1Elems * bits / 8;
    snprintf(n, sizeof n, "packed %s: row 1 starts at byte %d, row 0 holds the host bytes in order", name, row1);
    ARM(n, !memcmp(m, src, rowBytes) && !memcmp(m + row1, src + rowBytes, rowBytes), "m[0] %02x m[%d] %02x", m[0], row1, m[row1]);
    snprintf(n, sizeof n, "packed %s: the buffer past the tensor is untouched", name);
    ARM(n, m[row1 + rowBytes] == 0xEE && m[rowBytes] == 0xEE, "0x%02x", m[rowBytes]);
}

static void packed_formats(id<MTLDevice> d)
{
    NSError *e = nil;
    packed_round(d, "Int4", (MTLTensorDataType)143, 4, 256);
    packed_round(d, "UInt4", (MTLTensorDataType)144, 4, 256);
    packed_round(d, "FP4 E2M1", (MTLTensorDataType)148, 4, 256);
    packed_round(d, "Int2", (MTLTensorDataType)150, 2, 512);
    packed_round(d, "UInt2", (MTLTensorDataType)149, 2, 512);
    packed_round(d, "FP8 E4M3", (MTLTensorDataType)142, 8, 128);
    packed_round(d, "FP8 E5M2", (MTLTensorDataType)141, 8, 128);

    id<MTLBuffer> b = [d newBufferWithLength:65536 options:MTLResourceStorageModeShared];
    MTLTensorDescriptor *td = [MTLTensorDescriptor new];
    td.dimensions = E(2, 32, 2); td.strides = E(2, 1, 128); td.dataType = (MTLTensorDataType)143; td.usage = MTLTensorUsageCompute;
    ARM("packed Int4: a row stride of 128 elements (64 bytes) is refused", [b newTensorWithDescriptor:td offset:0 error:&e] == nil && e, "%s", msg(e).UTF8String);
    td.strides = E(2, 1, 256); td.dimensions = E(2, 40, 2); e = nil;
    ARM("packed Int4: dimension 0 of 40 is refused (must align to 32)", [b newTensorWithDescriptor:td offset:0 error:&e] == nil && e, "%s", msg(e).UTF8String);
    td.dimensions = E(2, 32, 2); e = nil;
    ARM("packed Int4: an offset of 64 bytes is refused (format types align to 128)", [b newTensorWithDescriptor:td offset:64 error:&e] == nil && e, "%s", msg(e).UTF8String);
    td.dataType = (MTLTensorDataType)145; e = nil;
    ARM("packed: the scales type UE8M0 is not a data plane type", [b newTensorWithDescriptor:td offset:0 error:&e] == nil && e, "%s", msg(e).UTF8String);

    td.dataType = (MTLTensorDataType)143; td.strides = nil; e = nil;
    id<MTLTensor> dt = [d newTensorWithDescriptor:td error:&e];
    ARM("packed Int4: a device tensor of 32x2 allocates 32 bytes", dt && dt.allocatedSize == 32, "%lu", (unsigned long)dt.allocatedSize);
    uint8_t pat[32]; for (int i = 0; i < 32; i++) pat[i] = (uint8_t)(0x10 + i); uint8_t got[32] = {0};
    [dt replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 32, 2) withBytes:pat strides:E(2, 1, 32)];
    [dt getBytes:got strides:E(2, 1, 32) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 32, 2)];
    ARM("packed Int4: a device tensor round-trips its 32 packed bytes", !memcmp(pat, got, 32), "got[31] %02x", got[31]);
    uint8_t part[8] = {0};
    [dt getBytes:part strides:E(2, 1, 8) fromSliceOrigin:E(2, 4, 0) sliceDimensions:E(2, 8, 1)];
    ARM("packed Int4: a byte-aligned slice of 8 elements at index 4 reads bytes 2..5", !memcmp(part, pat + 2, 4), "%02x %02x %02x %02x", part[0], part[1], part[2], part[3]);
    CONTROL("packed Int4: the claim 'that slice equals bytes 4..7' is false", memcmp(part, pat + 4, 4) != 0, "%02x %02x", part[0], part[1]);

    // blit: whole rows of a packed tensor
    id<MTLBuffer> sb = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared], db = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    memset(sb.contents, 0, 4096); memset(db.contents, 0xEE, 4096);
    for (int i = 0; i < 4096; i++) ((uint8_t *)sb.contents)[i] = (uint8_t)i;
    MTLTensorDescriptor *bd = [MTLTensorDescriptor new];
    bd.dimensions = E(2, 256, 4); bd.strides = E(2, 1, 256); bd.dataType = (MTLTensorDataType)143; bd.usage = MTLTensorUsageCompute;
    id<MTLTensor> ts = [sb newTensorWithDescriptor:bd offset:0 error:&e], tdst = [db newTensorWithDescriptor:bd offset:0 error:&e];
    if (ts && tdst) {
        id<MTLCommandQueue> q = [d newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
        [bl copyFromTensor:ts sourceOrigin:E(2, 0, 1) sourceDimensions:E(2, 128, 2) toTensor:tdst destinationOrigin:E(2, 0, 0) destinationDimensions:E(2, 128, 2)];
        [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
        const uint8_t *o = db.contents, *sx = sb.contents;
        // rows are 128 bytes apart; 128 elements of 4 bits are 64 bytes; source rows 1 and 2 land in destination rows 0 and 1
        ARM("packed Int4 blit: two rows of 128 elements land in the destination", !memcmp(o, sx + 128, 64) && !memcmp(o + 128, sx + 256, 64), "o[0] %02x (source %02x)", o[0], sx[128]);
        ARM("packed Int4 blit: bytes outside the slice are untouched", o[64] == 0xEE && o[127] == 0xEE && o[192] == 0xEE, "0x%02x", o[64]);
        // Apple GPUs scale a nonzero x origin of a sub-byte blit as if it were bytes (measured), so only x = 0 is asserted.
    } else SKIP("packed Int4 blit", "buffer tensors could not be made");
}

static void scales_plane(id<MTLDevice> d)
{
    if (@available(macOS 27.0, *)) {
        NSError *e = nil;
        MTLTensorDescriptor *td = [MTLTensorDescriptor new];
        td.dimensions = E(2, 64, 4); td.dataType = (MTLTensorDataType)142; td.usage = MTLTensorUsageCompute;
        MTLTensorAuxiliaryPlaneDescriptorMap *m = [MTLTensorAuxiliaryPlaneDescriptorMap new];
        MTLTensorAuxiliaryPlaneDescriptor *ad = [MTLTensorAuxiliaryPlaneDescriptor new];
        ad.dataType = (MTLTensorDataType)145; ad.blockFactors = E(2, 32, 1);
        [m setDescriptor:ad forPlane:MTLTensorPlaneTypeScales]; td.auxiliaryPlanes = m;
        id<MTLTensor> t = [d newTensorWithDescriptor:td error:&e];
        ARM("scales: an E4M3 64x4 tensor with a block-32 scales plane is made", t != nil, "%s", e ? msg(e).UTF8String : "ok");
        if (t) {
            ARM("scales: allocated size is data 256 + scales 8", t.allocatedSize == 264, "%lu", (unsigned long)t.allocatedSize);
            id<MTLTensorAuxiliaryPlane> p = t.auxiliaryPlanes.firstObject;
            ARM("scales: one auxiliary plane of type scales, UE8M0, block factor 32", t.auxiliaryPlanes.count == 1 && p.planeType == MTLTensorPlaneTypeScales && (NSInteger)p.dataType == 145 && p.blockFactors.rank == 2 && [p.blockFactors extentAtDimensionIndex:0] == 32 && [p.blockFactors extentAtDimensionIndex:1] == 1,
                "type %ld dt %ld", (long)p.planeType, (long)p.dataType);
            ARM("scales: a device tensor's scales plane exposes no buffer", p.buffer == nil && p.bufferOffset == 0, "%p", p.buffer);
            uint8_t sc[8] = {1, 2, 3, 4, 5, 6, 7, 8}, o[8] = {0};
            [t replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 2, 4) plane:MTLTensorPlaneTypeScales withBytes:sc strides:E(2, 1, 2)];
            [t getBytes:o strides:E(2, 1, 2) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 2, 4) plane:MTLTensorPlaneTypeScales];
            ARM("scales: the 2x4 scale grid round-trips", !memcmp(sc, o, 8), "%d %d %d %d", o[0], o[1], o[6], o[7]);
            uint8_t data[256], dback[256] = {0}; for (int i = 0; i < 256; i++) data[i] = (uint8_t)(255 - i);
            [t replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 64, 4) withBytes:data strides:E(2, 1, 64)];
            [t getBytes:dback strides:E(2, 1, 64) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 64, 4)];
            [t getBytes:o strides:E(2, 1, 2) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 2, 4) plane:MTLTensorPlaneTypeScales];
            ARM("scales: writing the data plane leaves the scales alone and vice versa", !memcmp(data, dback, 256) && !memcmp(sc, o, 8), "scales[7] %d data[255] %d", o[7], dback[255]);
        }
        // attachments: scale rows start on a 4-byte boundary
        id<MTLBuffer> db = [d newBufferWithLength:65536 options:MTLResourceStorageModeShared], sb = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
        memset(sb.contents, 0, 4096);
        MTLTensorDescriptor *bd = [td copy]; bd.strides = E(2, 1, 128);
        MTLTensorBufferAttachments *at = [MTLTensorBufferAttachments new];
        [at setBuffer:db offset:0 forPlane:MTLTensorPlaneTypeData]; [at setBuffer:sb offset:128 forPlane:MTLTensorPlaneTypeScales];
        e = nil; id<MTLTensor> a = [d newTensorWithDescriptor:bd attachments:at error:&e];
        ARM("scales: a tensor over two attached buffers is made", a != nil, "%s", e ? msg(e).UTF8String : "ok");
        if (a) {
            id<MTLTensorAuxiliaryPlane> p = a.auxiliaryPlanes.firstObject;
            ARM("scales: the attached plane reports its buffer and offset", p.buffer == sb && p.bufferOffset == 128, "off %lu", (unsigned long)p.bufferOffset);
            uint8_t sc[8] = {1, 2, 3, 4, 5, 6, 7, 8};
            [a replaceSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 2, 4) plane:MTLTensorPlaneTypeScales withBytes:sc strides:E(2, 1, 2)];
            const uint8_t *q = sb.contents;
            ARM("scales: scale rows are 4 bytes apart in the attached buffer", q[128] == 1 && q[129] == 2 && q[130] == 0 && q[132] == 3 && q[133] == 4 && q[136] == 5 && q[140] == 7, "%d %d %d | %d %d", q[128], q[129], q[130], q[132], q[133]);
            // blit the scales plane to another tensor
            id<MTLBuffer> sb2 = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared], db2 = [d newBufferWithLength:65536 options:MTLResourceStorageModeShared];
            memset(sb2.contents, 0xEE, 4096);
            MTLTensorBufferAttachments *at2 = [MTLTensorBufferAttachments new];
            [at2 setBuffer:db2 offset:0 forPlane:MTLTensorPlaneTypeData]; [at2 setBuffer:sb2 offset:128 forPlane:MTLTensorPlaneTypeScales];
            id<MTLTensor> a2 = [d newTensorWithDescriptor:bd attachments:at2 error:&e];
            id<MTLCommandQueue> cq = [d newCommandQueue]; id<MTLCommandBuffer> cb = [cq commandBuffer]; id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
            [bl copyFromTensor:a sourceOrigin:E(2, 0, 0) sourceDimensions:E(2, 2, 4) sourcePlane:MTLTensorPlaneTypeScales
                      toTensor:a2 destinationOrigin:E(2, 0, 0) destinationDimensions:E(2, 2, 4) destinationPlane:MTLTensorPlaneTypeScales];
            [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
            uint8_t o[8] = {0}; [a2 getBytes:o strides:E(2, 1, 2) fromSliceOrigin:E(2, 0, 0) sliceDimensions:E(2, 2, 4) plane:MTLTensorPlaneTypeScales];
            ARM("scales blit: the scales plane copies between tensors", a2 && !memcmp(o, sc, 8), "%d %d %d %d", o[0], o[1], o[6], o[7]);
        }
        // refusals
        MTLTensorDescriptor *x = [td copy]; MTLTensorAuxiliaryPlaneDescriptorMap *m2 = [MTLTensorAuxiliaryPlaneDescriptorMap new];
        MTLTensorAuxiliaryPlaneDescriptor *a2d = [MTLTensorAuxiliaryPlaneDescriptor new]; a2d.dataType = (MTLTensorDataType)145;
        a2d.blockFactors = E(1, 32); [m2 setDescriptor:a2d forPlane:MTLTensorPlaneTypeScales]; x.auxiliaryPlanes = m2; e = nil;
        ARM("scales: refused - block factor rank differs from the tensor rank", [d newTensorWithDescriptor:x error:&e] == nil && e, "%s", msg(e).UTF8String);
        a2d.blockFactors = E(2, 32, 2); [m2 setDescriptor:a2d forPlane:MTLTensorPlaneTypeScales]; x.auxiliaryPlanes = m2; e = nil;
        ARM("scales: refused - block factor above 1 beyond dimension 0", [d newTensorWithDescriptor:x error:&e] == nil && e, "%s", msg(e).UTF8String);
        a2d.blockFactors = E(2, 32, 1); a2d.dataType = (MTLTensorDataType)142; [m2 setDescriptor:a2d forPlane:MTLTensorPlaneTypeScales]; x.auxiliaryPlanes = m2; e = nil;
        ARM("scales: refused - scales type other than UE8M0", [d newTensorWithDescriptor:x error:&e] == nil && e, "%s", msg(e).UTF8String);
        a2d.dataType = (MTLTensorDataType)145; [m2 setDescriptor:a2d forPlane:MTLTensorPlaneTypeScales]; x.auxiliaryPlanes = m2; x.dataType = MTLTensorDataTypeFloat32; e = nil;
        ARM("scales: refused - data plane is not a block format", [d newTensorWithDescriptor:x error:&e] == nil && e, "%s", msg(e).UTF8String);
    } else SKIP("scales plane", "needs macOS 27");
}


// Color attachment mapping on a Metal 3 encoder. Measured on an Apple GPU: the map is accepted and changes nothing,
// because a Metal 3 pipeline is built with the identity mapping. The arms hold both devices to that. The Metal 4 mapping
// itself needs a Metal 4 pipeline and encoder and is checked where those exist.
static void color_map(id<MTLDevice> d, NSString *libPath)
{
    if (g_no_render) { SKIP("color map", "stand-in textures cannot be rendered to"); return; }
    if (@available(macOS 26.0, *)) {
        NSError *e = nil;
        id<MTLLibrary> lib = libPath ? [d newLibraryWithURL:[NSURL fileURLWithPath:libPath] error:&e] : nil;
        id<MTLFunction> vf = [lib newFunctionWithName:@"vs_tri"];
        id<MTLFunction> ff = [lib newFunctionWithName:@"fs_two"];
        if (!vf || !ff) { SKIP("color map", "vs_tri/fs_two not in the metallib (build with build_extras.sh)"); return; }
        MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
        pd.vertexFunction = vf; pd.fragmentFunction = ff;
        pd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm; pd.colorAttachments[1].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> pso = [d newRenderPipelineStateWithDescriptor:pd error:&e];
        if (!pso) { SKIP("color map", "the two-output pipeline could not be built"); return; }
        id<MTLCommandQueue> q = [d newCommandQueue];
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:8 height:8 mipmapped:NO];
        td.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; td.storageMode = MTLStorageModeShared;
        for (int scenario = 0; scenario < 3; scenario++) {
            id<MTLTexture> t0 = [d newTextureWithDescriptor:td], t1 = [d newTextureWithDescriptor:td];
            MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor new];
            rp.supportColorAttachmentMapping = YES;
            for (int i = 0; i < 2; i++) {
                rp.colorAttachments[i].texture = i ? t1 : t0; rp.colorAttachments[i].loadAction = MTLLoadActionClear;
                rp.colorAttachments[i].storeAction = MTLStoreActionStore; rp.colorAttachments[i].clearColor = MTLClearColorMake(0, 0, 1, 1);
            }
            MTLLogicalToPhysicalColorAttachmentMap *swap = [MTLLogicalToPhysicalColorAttachmentMap new];
            [swap setPhysicalIndex:1 forLogicalIndex:0]; [swap setPhysicalIndex:0 forLogicalIndex:1];
            id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLRenderCommandEncoder> enc = [cb renderCommandEncoderWithDescriptor:rp];
            if (scenario >= 1) [enc setColorAttachmentMap:swap];
            [enc setRenderPipelineState:pso];
            if (scenario == 2) [enc setColorAttachmentMap:swap];
            [enc drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
            [enc endEncoding]; [cb commit]; [cb waitUntilCompleted];
            uint32_t a[64], b[64];
            [t0 getBytes:a bytesPerRow:32 fromRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0];
            [t1 getBytes:b bytesPerRow:32 fromRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0];
            const uint32_t RED = 0xFF0000FFu, GREEN = 0xFF00FF00u;   // RGBA8 little-endian words
            char n[160];
            snprintf(n, sizeof n, "color map: Metal 3, %s - output 0 stays on attachment 0, output 1 on attachment 1",
                     scenario == 0 ? "no map" : scenario == 1 ? "swap set before the pipeline" : "swap set after the pipeline");
            ARM(n, a[27] == RED && b[27] == GREEN && cb.status == MTLCommandBufferStatusCompleted, "a %08x b %08x", a[27], b[27]);
            if (scenario == 1) CONTROL("color map: the claim 'a Metal 3 swap puts green on attachment 0' is false", a[27] != GREEN, "a %08x", a[27]);
        }
    } else SKIP("color map", "needs macOS 26");
}


// Placement sparse buffer: map one 64 KB page from a placement heap, run a kernel over two pages, read both back.
static void sparse_buffer(id<MTLDevice> d, NSString *libPath)
{
    if (@available(macOS 26.4, *)) {
        if (!d.supportsPlacementSparse) { SKIP("sparse buffer", "supportsPlacementSparse is NO on this device"); return; }
        if (![d respondsToSelector:@selector(newMTL4CommandQueue)]) { SKIP("sparse buffer", "device has no newMTL4CommandQueue"); return; }
        NSError *e = nil;
        id<MTLLibrary> lib = libPath ? [d newLibraryWithURL:[NSURL fileURLWithPath:libPath] error:&e] : nil;
        id<MTLFunction> fn = [lib newFunctionWithName:@"spin"];
        id<MTLComputePipelineState> pso = fn ? [d newComputePipelineStateWithFunction:fn error:&e] : nil;
        if (!pso) { SKIP("sparse buffer", "the spin kernel could not be loaded"); return; }
        id<MTLBuffer> sp = [d newBufferWithLength:4 * 65536 options:MTLResourceStorageModePrivate placementSparsePageSize:MTLSparsePageSize64];
        ARM("sparse buffer: made with 64 KB pages, tier 1, private", sp && sp.sparseBufferTier == MTLBufferSparseTier1 && sp.storageMode == MTLStorageModePrivate && sp.length == 4 * 65536, "%p tier %ld", sp, (long)sp.sparseBufferTier);
        if (!sp) return;
        MTLHeapDescriptor *hd = [MTLHeapDescriptor new];
        hd.type = MTLHeapTypePlacement; hd.size = 1 << 20; hd.storageMode = MTLStorageModePrivate; hd.maxCompatiblePlacementSparsePageSize = MTLSparsePageSize64;
        id<MTLHeap> heap = [d newHeapWithDescriptor:hd];
        id<MTL4CommandQueue> q = [d newMTL4CommandQueue];
        id<MTL4CommandAllocator> alloc = [d newCommandAllocator];
        id<MTL4CommandBuffer> cb = [d newCommandBuffer];
        id<MTLBuffer> readback = [d newBufferWithLength:2 * 65536 options:MTLResourceStorageModeShared];
        memset(readback.contents, 0xAB, 2 * 65536);
        MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new]; ad.maxBufferBindCount = 1;
        id<MTL4ArgumentTable> table = [d newArgumentTableWithDescriptor:ad error:&e];
        MTLResidencySetDescriptor *rd = [MTLResidencySetDescriptor new];
        id<MTLResidencySet> rs = [d newResidencySetWithDescriptor:rd error:&e];
        if (!heap || !q || !alloc || !cb || !table || !rs) { SKIP("sparse buffer", "heap, queue, argument table or residency set missing"); return; }
        [rs addAllocation:sp]; [rs addAllocation:heap]; [rs addAllocation:readback]; [rs commit]; [q addResidencySet:rs];
        [table setAddress:sp.gpuAddress atIndex:0];
        MTL4UpdateSparseBufferMappingOperation op = { MTLSparseTextureMappingModeMap, NSMakeRange(0, 1), 3 };
        [q updateBufferMappings:sp heap:heap operations:&op count:1];
        [cb beginCommandBufferWithAllocator:alloc];
        id<MTL4ComputeCommandEncoder> enc = [cb computeCommandEncoder];
        [enc setComputePipelineState:pso]; [enc setArgumentTable:table];
        [enc dispatchThreads:MTLSizeMake(32768, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
        [enc barrierAfterEncoderStages:MTLStageDispatch beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
        [enc copyFromBuffer:sp sourceOffset:0 toBuffer:readback destinationOffset:0 size:2 * 65536];
        [enc endEncoding]; [cb endCommandBuffer];
        id<MTLSharedEvent> ev = [d newSharedEvent]; id<MTL4CommandBuffer> cbs[1] = { cb };
        [q commit:cbs count:1]; [q signalEvent:ev value:1];
        const BOOL done = wait_event(ev, 1);
        ARM("sparse buffer: the command buffer over a half-mapped buffer completes", done, "%s", done ? "signalled" : "timeout");
        if (done) {
            const uint32_t *r = readback.contents;
            ARM("sparse buffer: the mapped page holds what the kernel wrote", r[0] != 0 && r[16383] != 0, "r[0] 0x%x r[16383] 0x%x", r[0], r[16383]);
            ARM("sparse buffer: the unmapped page reads zeros and the kernel's writes to it are dropped", r[16384] == 0 && r[32767] == 0, "r[16384] 0x%x r[32767] 0x%x", r[16384], r[32767]);
            CONTROL("sparse buffer: the claim 'the unmapped page holds the kernel value' is false", r[16384] == 0, "r[16384] 0x%x", r[16384]);
        }
    } else SKIP("sparse buffer", "needs macOS 26.4");
}


// Sparse textures. Everything uses the 64 KB page size, the GPU's native tile on NVIDIA. A tile is mapped, filled from a
// buffer by a blit, and read back with the neighbouring unmapped tile: the mapped tile holds the data, the unmapped one reads zeros.
static void sparse_texture_fill_read(id<MTLDevice> d, id<MTLTexture> tex, MTLSize tile, BOOL viaMetal4Queue, id<MTL4CommandQueue> q4, id<MTLResidencySet> rset, const char *label)
{
    // The texture is 4x4 tiles. Tile (0,0) is mapped and written; tile (3,3) is never mapped. (Apple GPUs were measured to lose
    // writes to a tile mapped on its own when it is not the first, so only tile (0,0) is asserted as the mapped one.)
    const NSUInteger bpr = tile.width * 4, bytes = bpr * tile.height;
    id<MTLBuffer> src = [d newBufferWithLength:bytes options:MTLResourceStorageModeShared];
    id<MTLBuffer> outA = [d newBufferWithLength:bytes options:MTLResourceStorageModeShared], outB = [d newBufferWithLength:bytes options:MTLResourceStorageModeShared];
    uint32_t *sp = src.contents; for (NSUInteger i = 0; i < tile.width * tile.height; i++) sp[i] = 0xC0000000u | (uint32_t)i;
    memset(outA.contents, 0xAB, bytes); memset(outB.contents, 0xAB, bytes);
    char n[200];
    const MTLOrigin far = MTLOriginMake(tile.width * 3, tile.height * 3, 0);
    if (!viaMetal4Queue) {
        id<MTLCommandQueue> q = [d newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
        [bl copyFromBuffer:src sourceOffset:0 sourceBytesPerRow:bpr sourceBytesPerImage:0 sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                 toTexture:tex destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
        [bl copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                   toBuffer:outA destinationOffset:0 destinationBytesPerRow:bpr destinationBytesPerImage:0];
        [bl copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:far sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                   toBuffer:outB destinationOffset:0 destinationBytesPerRow:bpr destinationBytesPerImage:0];
        [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
        snprintf(n, sizeof n, "%s: the command buffer completes", label);
        ARM(n, cb.status == MTLCommandBufferStatusCompleted, "status %ld", (long)cb.status);
    } else {
        [rset addAllocation:src]; [rset addAllocation:outA]; [rset addAllocation:outB]; [rset commit];
        id<MTL4CommandAllocator> al = [d newCommandAllocator]; id<MTL4CommandBuffer> cb = [d newCommandBuffer];
        [cb beginCommandBufferWithAllocator:al];
        id<MTL4ComputeCommandEncoder> en = [cb computeCommandEncoder];
        [en copyFromBuffer:src sourceOffset:0 sourceBytesPerRow:bpr sourceBytesPerImage:0 sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                 toTexture:tex destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
        [en barrierAfterEncoderStages:MTLStageBlit beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
        [en copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                   toBuffer:outA destinationOffset:0 destinationBytesPerRow:bpr destinationBytesPerImage:0];
        [en copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:far sourceSize:MTLSizeMake(tile.width, tile.height, 1)
                   toBuffer:outB destinationOffset:0 destinationBytesPerRow:bpr destinationBytesPerImage:0];
        [en endEncoding]; [cb endCommandBuffer];
        id<MTLSharedEvent> ev = [d newSharedEvent]; id<MTL4CommandBuffer> cbs[1] = { cb };
        [q4 commit:cbs count:1]; [q4 signalEvent:ev value:1];
        const BOOL done = wait_event(ev, 1);
        snprintf(n, sizeof n, "%s: the command buffer completes", label);
        ARM(n, done, "%s", done ? "signalled" : "timeout");
    }
    const uint32_t *a = outA.contents, *b = outB.contents;
    const uint32_t last = 0xC0000000u | (uint32_t)(tile.width * tile.height - 1);
    snprintf(n, sizeof n, "%s: the mapped tile holds what was written", label);
    ARM(n, a[0] == 0xC0000000u && a[tile.width * tile.height - 1] == last && a[tile.width + 5] == (0xC0000000u | (uint32_t)(tile.width + 5)), "first 0x%08x last 0x%08x", a[0], a[tile.width * tile.height - 1]);
    snprintf(n, sizeof n, "%s: a tile that was never mapped reads zeros", label);
    ARM(n, b[0] == 0 && b[tile.width * tile.height - 1] == 0, "0x%08x 0x%08x", b[0], b[tile.width * tile.height - 1]);
    snprintf(n, sizeof n, "%s: the claim 'the unmapped tile holds the data' is false", label);
    CONTROL(n, b[0] != 0xC0000000u, "0x%08x", b[0]);
}

static void sparse_textures(id<MTLDevice> d)
{
    if (g_no_render) { SKIP("sparse textures", "stand-in textures cannot be rendered to"); return; }
    if (@available(macOS 26.4, *)) {
        MTLSize tile = [d sparseTileSizeWithTextureType:MTLTextureType2D pixelFormat:MTLPixelFormatRGBA8Unorm sampleCount:1 sparsePageSize:MTLSparsePageSize64];
        const NSUInteger tbytes = [d sparseTileSizeInBytesForSparsePageSize:MTLSparsePageSize64];
        if (!tile.width || !tile.height) { SKIP("sparse textures", "the device reports no sparse tile size for RGBA8"); return; }
        ARM("sparse texture: a tile is 64 KB and the reported tile size matches it", tbytes == 65536 && tile.width * tile.height * tile.depth * 4 == tbytes, "%lux%lux%lu = %lu bytes, device says %lu",
            (unsigned long)tile.width, (unsigned long)tile.height, (unsigned long)tile.depth, (unsigned long)(tile.width * tile.height * tile.depth * 4), (unsigned long)tbytes);
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:tile.width * 4 height:tile.height * 4 mipmapped:NO];
        td.storageMode = MTLStorageModePrivate; td.usage = MTLTextureUsageShaderRead;

        // Metal 3: a sparse heap and the resource state encoder
        MTLHeapDescriptor *hd = [MTLHeapDescriptor new];
        hd.type = MTLHeapTypeSparse; hd.size = 8 * 65536; hd.storageMode = MTLStorageModePrivate; hd.sparsePageSize = MTLSparsePageSize64;
        id<MTLHeap> sh = [d newHeapWithDescriptor:hd];
        id<MTLTexture> st = [sh newTextureWithDescriptor:td];
        ARM("sparse heap: a sparse texture is made from a sparse heap", sh && st && st.isSparse, "heap %p texture %p sparse %d", sh, st, st.isSparse);
        if (st) {
            ARM("sparse heap: the texture reports sparse tier 1 or 2 and a mip tail", st.sparseTextureTier >= MTLTextureSparseTier1 && st.firstMipmapInTail <= st.mipmapLevelCount, "tier %ld first tail %lu",
                (long)st.sparseTextureTier, (unsigned long)st.firstMipmapInTail);
            id<MTLCommandQueue> q = [d newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLResourceStateCommandEncoder> rs = [cb resourceStateCommandEncoder];
            ARM("sparse heap: a resource state encoder is made", rs != nil, "%p", rs);
            if (rs) {
                [rs updateTextureMapping:st mode:MTLSparseTextureMappingModeMap region:MTLRegionMake2D(0, 0, 1, 1) mipLevel:0 slice:0];   // in tiles
                [rs endEncoding]; [cb commit]; [cb waitUntilCompleted];
                sparse_texture_fill_read(d, st, tile, NO, nil, nil, "sparse heap (Metal 3)");
            }
        }

        // Metal 4: a placement sparse texture and queue operations
        if (!d.supportsPlacementSparse || ![d respondsToSelector:@selector(newMTL4CommandQueue)]) { SKIP("placement sparse texture", "placement sparse or the Metal 4 queue is missing"); return; }
        MTLTextureDescriptor *pd = [td copy]; pd.placementSparsePageSize = MTLSparsePageSize64;
        id<MTLTexture> pt = [d newTextureWithDescriptor:pd];
        ARM("placement sparse texture: made, sparse, private", pt && pt.isSparse && pt.storageMode == MTLStorageModePrivate, "%p", pt);
        if (!pt) return;
        MTLHeapDescriptor *ph = [MTLHeapDescriptor new];
        ph.type = MTLHeapTypePlacement; ph.size = 1 << 20; ph.storageMode = MTLStorageModePrivate; ph.maxCompatiblePlacementSparsePageSize = MTLSparsePageSize64;
        id<MTLHeap> heap = [d newHeapWithDescriptor:ph];
        id<MTL4CommandQueue> q4 = [d newMTL4CommandQueue];
        MTLResidencySetDescriptor *rd = [MTLResidencySetDescriptor new]; NSError *e = nil;
        id<MTLResidencySet> rset = [d newResidencySetWithDescriptor:rd error:&e];
        [rset addAllocation:pt]; [rset addAllocation:heap]; [rset commit]; [q4 addResidencySet:rset];
        MTL4UpdateSparseTextureMappingOperation op = { MTLSparseTextureMappingModeMap, MTLRegionMake2D(0, 0, 1, 1), 0, 0, 2 };   // in tiles
        [q4 updateTextureMappings:pt heap:heap operations:&op count:1];
        sparse_texture_fill_read(d, pt, tile, YES, q4, rset, "placement sparse texture (Metal 4)");
    } else SKIP("sparse textures", "needs macOS 26.4");
}

// MTLCaptureScope.mtl4CommandQueue is Apple's own object: the driver only has to be a valid queue owner.
static void capture_scope(id<MTLDevice> d)
{
    if (![d respondsToSelector:@selector(newMTL4CommandQueue)]) { SKIP("capture scope", "device has no newMTL4CommandQueue"); return; }
    id<MTL4CommandQueue> q = [d newMTL4CommandQueue];
    id<MTLCaptureScope> sc = [[MTLCaptureManager sharedCaptureManager] newCaptureScopeWithMTL4CommandQueue:q];
    ARM("capture scope: a scope made for a Metal 4 queue reports that queue", sc && sc.mtl4CommandQueue == q && sc.commandQueue == nil, "%p", sc.mtl4CommandQueue);
}

// Function reflection from a library, before any pipeline exists. Apple GPUs return nil for a library loaded from a
// prebuilt metallib (measured) and a reflection for one compiled from source, so this device may answer for both.
static void library_reflection(id<MTLDevice> d, NSString *libPath)
{
    NSError *e = nil;
    id<MTLLibrary> lib = libPath ? [d newLibraryWithURL:[NSURL fileURLWithPath:libPath] error:&e] : nil;
    if (!lib) { SKIP("library reflection", "metallib not loaded"); return; }
    MTLFunctionReflection *r = [lib reflectionForFunctionWithName:@"spin"];
    INFO("library reflection of a prebuilt-metallib kernel: %s", r ? "present" : "nil (Apple GPUs: nil)");
    if (r) {
        ARM("library reflection: spin has a buffer binding named out at index 0", r.bindings.count >= 1 && r.bindings.firstObject.index == 0 && r.bindings.firstObject.type == MTLBindingTypeBuffer && [r.bindings.firstObject.name isEqualToString:@"out"],
            "%lu bindings, first '%s' index %lu", (unsigned long)r.bindings.count, r.bindings.firstObject.name.UTF8String, (unsigned long)r.bindings.firstObject.index);
    }
    ARM("library reflection: an unknown function has none", [lib reflectionForFunctionWithName:@"no_such_function"] == nil, "nil");
    NSString *src = @"#include <metal_stdlib>\nusing namespace metal;\nkernel void k(device uint *o [[buffer(0)]], constant float &c [[buffer(1)]], uint t [[thread_position_in_grid]]) { o[t] = t + c; }";
    id<MTLLibrary> sl = [d newLibraryWithSource:src options:nil error:&e];
    if (!sl) { SKIP("library reflection from source", "this device does not compile Metal source"); return; }
    MTLFunctionReflection *sr = [sl reflectionForFunctionWithName:@"k"];
    ARM("library reflection (source library): kernel k lists buffers o and c at 0 and 1", sr && sr.bindings.count == 2 && sr.bindings[0].index == 0 && sr.bindings[1].index == 1 && [sr.bindings[1].name isEqualToString:@"c"],
        "%lu bindings", (unsigned long)sr.bindings.count);
}

// copyFromTensor on a Metal 3 blit encoder: a 4x2 slice out of an 8x4 tensor into a 4x2 tensor
static void tensor_blit(id<MTLDevice> d)
{
    NSError *e = nil;
    id<MTLBuffer> sb = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    id<MTLBuffer> db = [d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    float *s = sb.contents, *o = db.contents;
    for (int i = 0; i < 1024; i++) { s[i] = 1000 + i; o[i] = -5; }
    MTLTensorDescriptor *sd = [MTLTensorDescriptor new]; sd.dimensions = E(2, 8, 4); sd.strides = E(2, 1, 8); sd.dataType = MTLTensorDataTypeFloat32;
    MTLTensorDescriptor *dd = [MTLTensorDescriptor new]; dd.dimensions = E(2, 4, 2); dd.strides = E(2, 1, 4); dd.dataType = MTLTensorDataTypeFloat32;
    id<MTLTensor> st = [sb newTensorWithDescriptor:sd offset:0 error:&e], dt = [db newTensorWithDescriptor:dd offset:0 error:&e];
    if (!st || !dt) { SKIP("tensor blit", "buffer tensors could not be made"); return; }
    id<MTLCommandQueue> q = [d newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
    if (![bl respondsToSelector:@selector(copyFromTensor:sourceOrigin:sourceDimensions:toTensor:destinationOrigin:destinationDimensions:)]) { SKIP("tensor blit", "blit encoder has no copyFromTensor"); [bl endEncoding]; return; }
    [bl copyFromTensor:st sourceOrigin:E(2, 2, 1) sourceDimensions:E(2, 4, 2) toTensor:dt destinationOrigin:E(2, 0, 0) destinationDimensions:E(2, 4, 2)];
    [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
    // source (x,y) = 1000 + x + 8y ; slice origin (2,1) -> dst[0..3] = 1010..1013, dst[4..7] = 1018..1021
    ARM("tensor blit: a 4x2 slice at (2,1) lands in the destination", o[0] == 1010 && o[3] == 1013 && o[4] == 1018 && o[7] == 1021, "%g %g | %g %g", o[0], o[3], o[4], o[7]);
    ARM("tensor blit: nothing outside the destination slice is touched", o[8] == -5 && o[31] == -5, "o[8] %g", o[8]);
    CONTROL("tensor blit: the claim 'the copy is the top-left corner' is false", o[0] != 1000, "o[0] is %g", o[0]);
}

// ----------------------------------------------------------------------------------------------------------------
static void views_and_pools(id<MTLDevice> d)
{
    NSError *e = nil;
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:8 height:8 mipmapped:NO];
    td.usage = MTLTextureUsageShaderRead; td.storageMode = MTLStorageModeShared;
    id<MTLTexture> t = [d newTextureWithDescriptor:td];
    ARM("sparse: a texture reports sparse tier none", t.sparseTextureTier == MTLTextureSparseTierNone, "%ld", (long)t.sparseTextureTier);
    id<MTLBuffer> b = [d newBufferWithLength:1024 options:MTLResourceStorageModeShared];
    ARM("sparse: a buffer reports sparse tier none", b.sparseBufferTier == MTLBufferSparseTierNone, "%ld", (long)b.sparseBufferTier);
    INFO("supportsPlacementSparse = %d (Apple GPUs: 1; this device may answer 0 because placement sparse is not built)", d.supportsPlacementSparse);

    uint32_t px[64]; for (int i = 0; i < 64; i++) px[i] = 0xFF000000u | (uint32_t)i;
    [t replaceRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0 withBytes:px bytesPerRow:32];
    MTLTextureViewDescriptor *vd = [MTLTextureViewDescriptor new];
    vd.pixelFormat = MTLPixelFormatRGBA8Unorm_sRGB; vd.textureType = MTLTextureType2D; vd.levelRange = NSMakeRange(0, 1); vd.sliceRange = NSMakeRange(0, 1);
    vd.swizzle = (MTLTextureSwizzleChannels){ MTLTextureSwizzleBlue, MTLTextureSwizzleGreen, MTLTextureSwizzleRed, MTLTextureSwizzleAlpha };
    id<MTLTexture> v = [t newTextureViewWithDescriptor:vd];
    ARM("view: newTextureViewWithDescriptor makes a view with the descriptor's format", v && v.pixelFormat == MTLPixelFormatRGBA8Unorm_sRGB && v.width == 8, "format %lu", (unsigned long)v.pixelFormat);
    ARM("view: the swizzle is carried", v && v.swizzle.red == MTLTextureSwizzleBlue && v.swizzle.blue == MTLTextureSwizzleRed, "red<-%ld blue<-%ld", (long)v.swizzle.red, (long)v.swizzle.blue);
    uint32_t rd[64] = {0}; [v getBytes:rd bytesPerRow:32 fromRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0];
    ARM("view: shares the parent's storage", !memcmp(px, rd, sizeof px), "px[5] 0x%x read 0x%x", px[5], rd[5]);

    MTLResourceViewPoolDescriptor *pd = [MTLResourceViewPoolDescriptor new]; pd.resourceViewCount = 4;
    id<MTLTextureViewPool> p = [d newTextureViewPoolWithDescriptor:pd error:&e];
    ARM("pool: a texture view pool of 4 is made", p && !e, "%p", p);
    if (p) {
        ARM("pool: count and device read back", p.resourceViewCount == 4 && p.device == d, "count %lu", (unsigned long)p.resourceViewCount);
        MTLResourceID r0 = [p setTextureView:t atIndex:0], r1 = [p setTextureView:t atIndex:1];
        ARM("pool: setTextureView returns non-zero resource ids", r0._impl && r1._impl, "%llu %llu", (unsigned long long)r0._impl, (unsigned long long)r1._impl);
        MTLResourceID r2 = [p setTextureView:t descriptor:vd atIndex:2];
        ARM("pool: setTextureView with a descriptor returns a resource id", r2._impl != 0, "%llu", (unsigned long long)r2._impl);
        MTLTextureDescriptor *bt = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR32Float width:4 height:4 mipmapped:NO];
        bt.storageMode = MTLStorageModeShared; bt.usage = MTLTextureUsageShaderRead;
        MTLResourceID r3 = [p setTextureViewFromBuffer:b descriptor:bt offset:0 bytesPerRow:256 atIndex:3];
        ARM("pool: setTextureViewFromBuffer returns a resource id", r3._impl != 0, "%llu", (unsigned long long)r3._impl);
        id<MTLTextureViewPool> p2 = [d newTextureViewPoolWithDescriptor:pd error:&e];
        MTLResourceID rc = [p2 copyResourceViewsFromPool:p sourceRange:NSMakeRange(0, 2) destinationIndex:1];
        ARM("pool: copying views between pools returns the first copied id", rc._impl != 0, "%llu", (unsigned long long)rc._impl);
        ARM("pool: the id of view i is baseResourceID + i", r0._impl == p.baseResourceID._impl && r1._impl == p.baseResourceID._impl + 1 && r2._impl == p.baseResourceID._impl + 2 && r3._impl == p.baseResourceID._impl + 3,
            "base %llu ids %llu %llu %llu %llu", (unsigned long long)p.baseResourceID._impl, (unsigned long long)r0._impl, (unsigned long long)r1._impl, (unsigned long long)r2._impl, (unsigned long long)r3._impl);
        ARM("pool: a copy returns the id of its first destination slot", rc._impl == p2.baseResourceID._impl + 1, "%llu vs base %llu", (unsigned long long)rc._impl, (unsigned long long)p2.baseResourceID._impl);
        ARM("pool: two pools have disjoint id ranges", p2.baseResourceID._impl != p.baseResourceID._impl && (p2.baseResourceID._impl > p.baseResourceID._impl + 3 || p2.baseResourceID._impl + 3 < p.baseResourceID._impl), "%llu and %llu", (unsigned long long)p.baseResourceID._impl, (unsigned long long)p2.baseResourceID._impl);
        CONTROL("pool: the claim 'slot 0 and slot 2 hold the same id' is false", r0._impl != r2._impl, "%llu vs %llu", (unsigned long long)r0._impl, (unsigned long long)r2._impl);
    }
    MTLDepthStencilDescriptor *dsd = [MTLDepthStencilDescriptor new]; dsd.depthCompareFunction = MTLCompareFunctionLess; dsd.depthWriteEnabled = YES;
    id<MTLDepthStencilState> ds1 = [d newDepthStencilStateWithDescriptor:dsd], ds2 = [d newDepthStencilStateWithDescriptor:dsd];
    ARM("depth stencil state: gpuResourceID is stable across reads", ds1 && ds1.gpuResourceID._impl == ds1.gpuResourceID._impl, "%llu", (unsigned long long)ds1.gpuResourceID._impl);
    INFO("depth stencil state ids: %llu and %llu (Apple GPUs answer 0 for a state outside an argument buffer; this device hands out registry ids)",
         (unsigned long long)ds1.gpuResourceID._impl, (unsigned long long)ds2.gpuResourceID._impl);
    pd.resourceViewCount = 0; e = nil;
    ARM("pool: a pool of 0 views is refused", [d newTextureViewPoolWithDescriptor:pd error:&e] == nil, "%s", e.localizedDescription.UTF8String);
}

// ----------------------------------------------------------------------------------------------------------------
// The arms run on whatever device `d` is; plugin_logic_harness.m builds this file without main to run them over the
// plugin's own tensor, counter heap and view pool code on an Apple GPU.
int oracle_run(id<MTLDevice> d, NSString *lib)
{
    printf("device: %s  Metal4 family: %d\n", d.name.UTF8String, [d supportsFamily:MTLGPUFamilyMetal4]);
    counter_heap_host(d);
    counter_heap_gpu(d, lib);
    tensors(d);
    tensor_blit(d);
    packed_formats(d);
    scales_plane(d);
    views_and_pools(d);
    color_map(d, lib);
    capture_scope(d);
    sparse_buffer(d, lib);
    sparse_textures(d);
    library_reflection(d, lib);
    printf("\n%d passed, %d failed, %d skipped\n", g_pass, g_fail, g_skip);
    return g_fail ? 1 : g_skip ? 2 : 0;
}

#ifndef ORACLE_NO_MAIN
int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    @autoreleasepool {
        id<MTLDevice> d = MTLCreateSystemDefaultDevice();
        if (!d) { printf("FAIL    no Metal device\n"); return 1; }
        return oracle_run(d, argc > 1 ? [NSString stringWithUTF8String:argv[1]] : @"build/mtl4_extras.metallib");
    }
}
#endif
