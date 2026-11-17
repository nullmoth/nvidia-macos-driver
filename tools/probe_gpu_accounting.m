/* Isolated Metal accounting check. Never installs or changes a driver.
 * Build with Foundation, Metal and IOKit; pass a private plugin executable,
 * optionally followed by --require-reporting after booting the paired NVAccel.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>
#include "../kexts/NVRM/nvrm_gpu_accounting_abi.h"

@interface NSObject (GPUAccountingProbe)
- (instancetype)initWithAcceleratorPort:(mach_port_t)port;
@end

static double cpu_seconds(void) {
    struct rusage usage; getrusage(RUSAGE_SELF, &usage);
    return usage.ru_utime.tv_sec + usage.ru_stime.tv_sec +
        (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1e6;
}

static NSDictionary *counters(unsigned *clients) {
    io_iterator_t iterator = 0; NSDictionary *result = nil; *clients = 0;
    // User clients are attached registry children, not registered services.
    // IOServiceGetMatchingServices would silently omit these live counters.
    io_service_t accelerator = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    if (!accelerator) return nil;
    const kern_return_t status = IORegistryEntryGetChildIterator(accelerator, kIOServicePlane, &iterator);
    IOObjectRelease(accelerator);
    if (status != KERN_SUCCESS) return nil;
    for (io_service_t service; (service = IOIteratorNext(iterator)); IOObjectRelease(service)) {
        if (!IOObjectConformsTo(service, "NVGPUAccountingClient")) continue;
        CFMutableDictionaryRef raw = NULL;
        if (IORegistryEntryCreateCFProperties(service, &raw, kCFAllocatorDefault, 0)) continue;
        NSDictionary *properties = CFBridgingRelease(raw);
        NSString *creator = properties[@"IOUserClientCreator"]; int pid = 0;
        if ([creator isKindOfClass:NSString.class] && sscanf(creator.UTF8String, "pid %d,", &pid) == 1 && pid == getpid()) {
            ++*clients; result = properties;
        }
    }
    IOObjectRelease(iterator); return result;
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 2 || argc > 3) return 2;
    const BOOL required = argc == 3 && !strcmp(argv[2], "--require-reporting");
    if (!dlopen(argv[1], RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr, "%s\n", dlerror()); return 2; }
    io_service_t accelerator = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    if (!accelerator) return 2;
    CFTypeRef rawVersion = IORegistryEntryCreateCFProperty(accelerator,
        CFSTR(NV_GPU_ACCOUNTING_PROPERTY), kCFAllocatorDefault, 0);
    NSNumber *version = CFBridgingRelease(rawVersion);
    const BOOL supported = [version isKindOfClass:NSNumber.class] && version.unsignedIntValue == NV_GPU_ACCOUNTING_VERSION;
    if (required && !supported) {
        fprintf(stderr, "Paired NVAccel is not active; a reboot may be required\n");
        IOObjectRelease(accelerator); return 2;
    }
    id<MTLDevice> device = [[NSClassFromString(@"NVMTLDevice") alloc] initWithAcceleratorPort:accelerator];
    IOObjectRelease(accelerator);
    if (!device) return 2;
    NSError *error = nil;
    NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
        "kernel void work(device uint *out [[buffer(0)]], uint i [[thread_position_in_grid]]) {\n"
        "uint v=i; for(uint j=0;j<128;j++) v=(v^(v>>3))*1664525u+1013904223u; out[i]=v; }\n";
    id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
    id<MTLComputePipelineState> pipeline = library ? [device newComputePipelineStateWithFunction:
        [library newFunctionWithName:@"work"] error:&error] : nil;
    if (!pipeline) { fprintf(stderr, "%s\n", error.description.UTF8String); return 2; }
    id<MTLCommandQueue> queues[2] = {[device newCommandQueue], [device newCommandQueue]};
    id<MTLBuffer> buffers[2] = {
        [device newBufferWithLength:16384 * 4 options:MTLResourceStorageModeShared],
        [device newBufferWithLength:16384 * 4 options:MTLResourceStorageModeShared]};
    if (!queues[0] || !queues[1] || !buffers[0] || !buffers[1]) return 2;
    const double cpuStart = cpu_seconds();
    const uint64_t hostStart = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    unsigned completions = 0; double timestampSeconds = 0;
    for (unsigned frame = 0; frame < 180; frame++) { @autoreleasepool {
        id<MTLCommandBuffer> commands[2];
        for (unsigned i = 0; i < 2; i++) {
            commands[i] = [queues[i] commandBuffer];
            id<MTLComputeCommandEncoder> encoder = [commands[i] computeCommandEncoder];
            [encoder setComputePipelineState:pipeline]; [encoder setBuffer:buffers[i] offset:0 atIndex:0];
            [encoder dispatchThreads:MTLSizeMake(16384, 1, 1) threadsPerThreadgroup:MTLSizeMake(128, 1, 1)];
            [encoder endEncoding]; [commands[i] commit];
        }
        // Reverse collection order exercises independent Metal completion queues.
        for (int i = 1; i >= 0; i--) {
            [commands[i] waitUntilCompleted];
            if (commands[i].status != MTLCommandBufferStatusCompleted) return 1;
            double duration = commands[i].GPUEndTime - commands[i].GPUStartTime;
            if (isfinite(duration) && duration > 0) timestampSeconds += duration;
            completions++;
        }
        usleep(12000);
    } }
    for (unsigned i = 0; i < 2; i++) {
        const uint32_t *values = buffers[i].contents;
        for (unsigned j = 0; j < 16384; j++) {
            uint32_t expected = j;
            for (unsigned k = 0; k < 128; k++) expected = (expected ^ (expected >> 3)) * 1664525u + 1013904223u;
            if (values[j] != expected) { fprintf(stderr, "GPU readback mismatch\n"); return 1; }
        }
    }
    const double elapsed = (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) - hostStart) / 1e9;
    const double cpu = cpu_seconds() - cpuStart;
    usleep(600000); // allow the final report to run after the app goes idle
    unsigned clients = 0; NSDictionary *first = counters(&clients);
    const uint64_t busy = [first[@"accumulatedGPUTime"] unsignedLongLongValue];
    const uint64_t submitted = [first[@"lastSubmittedTime"] unsignedLongLongValue];
    printf("completed=%u wall_s=%.6f cpu_s=%.6f Metal_timestamp_s=%.6f clients=%u reported_gpu_s=%.6f\n",
        completions, elapsed, cpu, timestampSeconds, clients, busy / 1e9);
    if (required && (clients != 1 || !busy || !submitted || submitted > mach_absolute_time() || busy / 1e9 > elapsed))
        return 1;
    usleep(600000);
    NSDictionary *idle = counters(&clients);
    if (busy != [idle[@"accumulatedGPUTime"] unsignedLongLongValue]) {
        fprintf(stderr, "GPU time changed while idle\n"); return 1;
    }
    puts(required ? "PASS: GPU readback, owner attribution, completed time and idle accounting"
        : "PASS: GPU readback and optional accounting compatibility");
    return 0;
} }
