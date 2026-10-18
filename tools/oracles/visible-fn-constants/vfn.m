// Does a function that calls a linked visible function survive specialization with function constants?
//
// This is the path the system's icon and Liquid Glass shaders take on macOS 26: newFunctionWithName:constantValues:
// on a function that calls an extern [[visible]] function, which the pipeline supplies through linkedFunctions.
// The driver used to decide whether to defer such a function to pipeline creation by reading the translator's
// error text, and that text changed, so the function came back nil.
//
// usage: vfn <vfn.metallib>     every arm prints the value it checked
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

static int fails;
static void arm(const char *name, BOOL ok, NSString *detail)
{
    fails += !ok;
    printf("%s %-66s %s\n", ok ? "ok  " : "FAIL", name, detail.UTF8String ?: "");
}

// Run the kernel with use_custom set as given and return how many of 64 outputs match.
static int run(id<MTLDevice> dev, id<MTLLibrary> lib, BOOL useCustom, BOOL link, NSString **why)
{
    MTLFunctionConstantValues *cv = [MTLFunctionConstantValues new];
    [cv setConstantValue:&useCustom type:MTLDataTypeBool atIndex:0];
    NSError *e = nil;
    id<MTLFunction> k = [lib newFunctionWithName:@"k" constantValues:cv error:&e];
    if (!k) { *why = [NSString stringWithFormat:@"newFunctionWithName:constantValues: returned nil (%@)", e.localizedDescription]; return -1; }
    MTLComputePipelineDescriptor *d = [MTLComputePipelineDescriptor new];
    d.computeFunction = k;
    if (link) {
        id<MTLFunction> v = [lib newFunctionWithName:@"custom_fn"];
        if (!v) { *why = @"the visible function itself did not load"; return -1; }
        MTLLinkedFunctions *lf = [MTLLinkedFunctions linkedFunctions];
        lf.functions = @[ v ];
        d.linkedFunctions = lf;
    }
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithDescriptor:d options:0 reflection:nil error:&e];
    if (!ps) { *why = [NSString stringWithFormat:@"pipeline refused (%@)", e.localizedDescription]; return -1; }
    id<MTLBuffer> b = [dev newBufferWithLength:64 * sizeof(float) options:MTLResourceStorageModeShared];
    memset(b.contents, 0xff, 64 * sizeof(float));
    id<MTLCommandQueue> q = [dev newCommandQueue];
    id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLComputeCommandEncoder> en = [cb computeCommandEncoder];
    [en setComputePipelineState:ps];
    [en setBuffer:b offset:0 atIndex:0];
    [en dispatchThreads:MTLSizeMake(64, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
    [en endEncoding];
    [cb commit];
    [cb waitUntilCompleted];
    if (cb.status != MTLCommandBufferStatusCompleted) { *why = [NSString stringWithFormat:@"command buffer status %ld", (long)cb.status]; return -1; }
    const float *o = b.contents;
    int good = 0;
    for (int i = 0; i < 64; i++) good += o[i] == (useCustom ? i * 3.0f + 1.0f : (float)i);
    *why = [NSString stringWithFormat:@"%d/64 correct, o[5]=%g (want %g)", good, o[5], useCustom ? 16.0 : 5.0];
    return good;
}

int main(int argc, const char **argv)
{
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: vfn <vfn.metallib>\n"); return 2; }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        if (!dev) { fprintf(stderr, "no Metal device\n"); return 3; }
        NSError *e = nil;
        id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
        if (!lib) { fprintf(stderr, "library: %s\n", e.localizedDescription.UTF8String); return 3; }
        printf("device: %s\n", dev.name.UTF8String);
        NSString *why = nil; int g;

        // The failing case on 2.3.6: the call is live, so specialization alone cannot finish it.
        g = run(dev, lib, YES, YES, &why);
        arm("a live call to a linked visible function specializes and runs", g == 64, why);
        // Control: the constant removes the call, so it never needed linking and must work either way.
        g = run(dev, lib, NO, YES, &why);
        arm("control: with the call compiled out, the same function runs", g == 64, why);
        g = run(dev, lib, NO, NO, &why);
        arm("control: and runs with no linked functions at all", g == 64, why);
        // A live call with nothing linked has no answer; it must be refused, not run with garbage.
        g = run(dev, lib, YES, NO, &why);
        arm("a live call with nothing linked is refused, never run", g == -1, why);

        printf("%s\n", fails ? "FAILED" : "PASS");
        return fails != 0;
    }
}
