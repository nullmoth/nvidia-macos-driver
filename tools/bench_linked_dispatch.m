// Exercise repeated dispatches of a linked, specialized function, checking
// integer output and timing both AIR-key lookup and command recording.
// No installed driver files are changed.
// clang -fobjc-arc -O2 tools/bench_linked_dispatch.m -framework Foundation -framework Metal -framework IOKit -o bench_linked_dispatch
// bench_linked_dispatch path/to/NVMTLDriver [dispatches=1000] [operations=128] [mode=0]
// Modes: 0 dispatchThreads, 1 dispatchThreadgroups, 2 indirect threadgroups.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>

@interface NSObject (NVMTLProbe)
- (instancetype)initWithAcceleratorPort:(mach_port_t)port;
@end

static double cpu_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &t);
    return t.tv_sec * 1000.0 + t.tv_nsec / 1e6;
}
static uint32_t expected(uint32_t x, unsigned operations) {
    for (unsigned i = 0; i < operations; i++)
        x = (x ^ (x >> (i % 15 + 1))) * (0x9e3779b1u + 2 * i) + (i * 17 + 3);
    return x;
}
static double median(double *times, int n) {
    for (int i = 1; i < n; i++)
        for (int j = i; j > 0 && times[j] < times[j - 1]; j--) {
            double v = times[j]; times[j] = times[j - 1]; times[j - 1] = v;
        }
    return times[n / 2];
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 2) return 2;
    int draws = argc > 2 ? atoi(argv[2]) : 1000;
    unsigned operations = argc > 3 ? (unsigned)atoi(argv[3]) : 128;
    int mode = argc > 4 ? atoi(argv[4]) : 0;
    if (draws < 1 || draws > 10000 || operations > 512 || mode < 0 || mode > 2) return 2;
    void *handle = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!handle) { fprintf(stderr, "load: %s\n", dlerror()); return 2; }
    io_service_t accel = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    id<MTLDevice> dev = [[NSClassFromString(@"NVMTLDevice") alloc] initWithAcceleratorPort:accel];
    if (accel) IOObjectRelease(accel);
    if (!dev) return 2;
    printf("device: %s\nplugin: %s\n", dev.name.UTF8String, argv[1]); fflush(stdout);
    NSError *error = nil;
    NSString *ks = @"#include <metal_stdlib>\nusing namespace metal;\n"
        "constant bool enabled [[function_constant(0)]];\n"
        "[[visible]] uint transform(uint x);\n"
        "kernel void linked(device uint *out [[buffer(0)]], uint i [[thread_position_in_grid]]) {\n"
        "out[i] = enabled ? transform(i) : i; }\n";
    NSMutableString *vs = [NSMutableString stringWithString:
        @"#include <metal_stdlib>\nusing namespace metal;\n[[visible]] uint transform(uint x) {\n"];
    for (unsigned i = 0; i < operations; i++)
        [vs appendFormat:@"x = (x ^ (x >> %uu)) * %uu + %uu;\n", i % 15 + 1, 0x9e3779b1u + 2 * i, i * 17 + 3];
    [vs appendString:@"return x; }\n"];
    id<MTLLibrary> kl = [dev newLibraryWithSource:ks options:nil error:&error];
    id<MTLLibrary> vl = [dev newLibraryWithSource:vs options:nil error:&error];
    if (!kl || !vl) { fprintf(stderr, "library: %s\n", error.description.UTF8String); return 2; }
    MTLFunctionConstantValues *fc = [MTLFunctionConstantValues new];
    BOOL enabled = YES;
    [fc setConstantValue:&enabled type:MTLDataTypeBool atIndex:0];
    id<MTLFunction> kernel = [kl newFunctionWithName:@"linked" constantValues:fc error:&error];
    id<MTLFunction> visible = [vl newFunctionWithName:@"transform"];
    if (!kernel || !visible) { fprintf(stderr, "functions: %s\n", error.description.UTF8String); return 2; }
    MTLComputePipelineDescriptor *pd = [MTLComputePipelineDescriptor new];
    pd.computeFunction = kernel;
    pd.linkedFunctions = [MTLLinkedFunctions linkedFunctions];
    pd.linkedFunctions.functions = @[visible];
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithDescriptor:pd options:0 reflection:nil error:&error];
    if (!ps) { fprintf(stderr, "pipeline: %s\n", error.description.UTF8String); return 2; }
    Ivar functionIvar = class_getInstanceVariable([ps class], "_function");
    id fn = functionIvar ? object_getIvar(ps, functionIvar) : nil;
    Ivar airIvar = fn ? class_getInstanceVariable([fn class], "_air") : NULL;
    NSString *air = airIvar ? object_getIvar(fn, airIvar) : nil;
    uint64_t (*key)(id) = dlsym(handle, "nvmtl_function_air_key");
    if (!key || !air.length) return 2;
    uint64_t digest = key(fn);
    volatile uint64_t combined = 0;
    double h0 = cpu_ms();
    for (int i = 0; i < draws; i++) combined ^= key(fn);
    printf("linked_air_bytes=%lu key=%016llx key_lookups=%d key_lookup_cpu_ms=%.6f combined=%llx\n",
        (unsigned long)[air lengthOfBytesUsingEncoding:NSUTF8StringEncoding],
        (unsigned long long)digest, draws, cpu_ms() - h0, (unsigned long long)combined); fflush(stdout);
    id<MTLCommandQueue> q = [dev newCommandQueue];
    id<MTLBuffer> out = [dev newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    id<MTLBuffer> indirect = [dev newBufferWithLength:256 options:MTLResourceStorageModeShared];
    if (!q || !out || !indirect) return 2;
    uint32_t *groups = indirect.contents; groups[0] = 2; groups[1] = groups[2] = 1;
    double times[9];
    for (int frame = -2; frame < 9; frame++) { @autoreleasepool {
        memset(out.contents, 0xff, 4096);
        id<MTLCommandBuffer> cb = [q commandBuffer];
        id<MTLComputeCommandEncoder> enc = [cb computeCommandEncoder];
        [enc setComputePipelineState:ps];
        [enc setBuffer:out offset:0 atIndex:0];
        double start = cpu_ms();
        for (int i = 0; i < draws; i++) {
            if (mode == 0) [enc dispatchThreads:MTLSizeMake(64, 1, 1) threadsPerThreadgroup:MTLSizeMake(32, 1, 1)];
            else if (mode == 1) [enc dispatchThreadgroups:MTLSizeMake(2, 1, 1) threadsPerThreadgroup:MTLSizeMake(32, 1, 1)];
            else [enc dispatchThreadgroupsWithIndirectBuffer:indirect indirectBufferOffset:0 threadsPerThreadgroup:MTLSizeMake(32, 1, 1)];
        }
        double elapsed = cpu_ms() - start;
        [enc endEncoding]; [cb commit]; [cb waitUntilCompleted];
        if (cb.status != MTLCommandBufferStatusCompleted) { fprintf(stderr, "command: %s\n", cb.error.description.UTF8String); return 1; }
        const uint32_t *values = out.contents;
        for (uint32_t i = 0; i < 64; i++) if (values[i] != expected(i, operations)) {
            fprintf(stderr, "output %u got %u expected %u\n", i, values[i], expected(i, operations)); return 1;
        }
        if (frame >= 0) times[frame] = elapsed;
    }}
    printf("PASS mode=%s dispatches=%d operations=%u frames=9 median_record_cpu_ms=%.6f outputs=64/64\n",
        mode == 0 ? "threads" : mode == 1 ? "threadgroups" : "indirect", draws, operations, median(times, 9));
    return 0;
}}
