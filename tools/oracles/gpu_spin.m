// gpu_spin: keep the GPU busy with one long compute dispatch and report whether the desktop survived it.
//
// The compositor's own frames queue behind whatever else the GPU is doing. A fence that is late is not a dead
// device, and the Metal driver must not treat it as one: a desktop that waited is fine, a desktop whose driver
// disabled itself is a session that restarts. This runs a kernel whose length is chosen in seconds, then checks
// that WindowServer kept its process id and that the device still answers.
//
//   clang -fobjc-arc -framework Metal -framework Foundation gpu_spin.m -o gpu_spin
//   ./gpu_spin <seconds>        prints PASS/FAIL on the last line; PASS needs WindowServer to be the same process
//                               before and after and the device to answer afterwards; the long dispatch may run to
//                               the end or be stopped by the driver (Apple's stops it: "Impacting Interactivity")
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <unistd.h>

static const char *kSource =
    "#include <metal_stdlib>\n"
    "using namespace metal;\n"
    "kernel void spin(device atomic_uint *out [[buffer(0)]], constant uint &iters [[buffer(1)]],\n"
    "                 uint id [[thread_position_in_grid]]) {\n"
    "    uint x = id * 2654435761u + 1u;\n"
    "    for (uint i = 0; i < iters; i++) x = x * 1664525u + 1013904223u;\n"
    "    if (x == 0x9E3779B9u) atomic_fetch_add_explicit(out, 1u, memory_order_relaxed);\n"
    "}\n";

static pid_t windowServerPid(void)
{
    FILE *p = popen("/usr/bin/pgrep -x WindowServer", "r");
    if (!p) return 0;
    char line[64] = {0};
    pid_t pid = fgets(line, sizeof line, p) ? (pid_t)atoi(line) : 0;
    pclose(p);
    return pid;
}

// One dispatch of `threads` x `iters` steps; returns the GPU time in seconds (wall time of the command buffer).
static double runOnce(id<MTLDevice> dev, id<MTLComputePipelineState> pso, id<MTLBuffer> out, NSUInteger threads, uint32_t iters)
{
    id<MTLCommandQueue> q = [dev newCommandQueue];
    id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLComputeCommandEncoder> enc = [cb computeCommandEncoder];
    [enc setComputePipelineState:pso];
    [enc setBuffer:out offset:0 atIndex:0];
    [enc setBytes:&iters length:sizeof iters atIndex:1];
    NSUInteger w = pso.threadExecutionWidth;
    [enc dispatchThreads:MTLSizeMake(threads, 1, 1) threadsPerThreadgroup:MTLSizeMake(w, 1, 1)];
    [enc endEncoding];
    NSDate *t0 = [NSDate date];
    [cb commit];
    [cb waitUntilCompleted];
    if (cb.status != MTLCommandBufferStatusCompleted) {
        // Apple's driver stops a dispatch that blocks the compositor ("Impacting Interactivity") and fails only that
        // command buffer; the device stays up. That is a pass for the desktop, reported as a negative time.
        printf("command buffer status %ld error %s after %.2f s\n", (long)cb.status,
               cb.error.localizedDescription.UTF8String ?: "none", -[t0 timeIntervalSinceNow]);
        return -[t0 timeIntervalSinceNow] > 0.5 ? -2 : -1;
    }
    return -[t0 timeIntervalSinceNow];
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        double want = argc > 1 ? atof(argv[1]) : 8.0;
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        if (!dev) { puts("FAIL no Metal device"); return 1; }
        printf("device %s\n", dev.name.UTF8String);
        NSError *err = nil;
        id<MTLLibrary> lib = [dev newLibraryWithSource:[NSString stringWithUTF8String:kSource] options:nil error:&err];
        if (!lib) { printf("FAIL library: %s\n", err.localizedDescription.UTF8String); return 1; }
        id<MTLComputePipelineState> pso = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"spin"] error:&err];
        if (!pso) { printf("FAIL pipeline: %s\n", err.localizedDescription.UTF8String); return 1; }
        id<MTLBuffer> out = [dev newBufferWithLength:4 options:MTLResourceStorageModeShared];
        memset(out.contents, 0, 4);
        const pid_t ws0 = windowServerPid();
        // calibrate: a short dispatch, then scale the iteration count to the wanted length (the loop is linear in iters)
        const NSUInteger threads = 1 << 20;
        double t = runOnce(dev, pso, out, threads, 1u << 12);
        if (t <= 0) { puts("FAIL calibration dispatch"); return 1; }
        double perIter = t / (double)(1u << 12);
        // dispatches of about two seconds each, back to back, until `want` seconds have passed: the GPU never idles
        uint32_t iters = (uint32_t)(2.0 / perIter);
        if (iters < (1u << 12)) iters = 1u << 12;
        printf("calibration: %.3f s for 4096 steps; dispatching %u-step kernels back to back for %.1f s\n", t, iters, want);
        NSDate *T0 = [NSDate date];
        int runs = 0, stopped = 0, failed = 0; double longest = 0;
        while (-[T0 timeIntervalSinceNow] < want) {
            double r = runOnce(dev, pso, out, threads, iters);
            runs++;
            if (r == -2) stopped++;
            else if (r < 0) { failed++; break; }
            else if (r > longest) longest = r;
        }
        printf("hog: %d dispatches in %.1f s, longest %.2f s, %d stopped by the driver, %d failed\n",
               runs, -[T0 timeIntervalSinceNow], longest, stopped, failed);
        // the desktop must still be the same process, and the device must still answer a short dispatch
        sleep(2);
        const pid_t ws1 = windowServerPid();
        double after = runOnce(dev, pso, out, threads, 1u << 12);
        printf("WindowServer pid before %d after %d; device answers after: %s (%.3f s)\n", ws0, ws1, after > 0 ? "yes" : "no", after);
        // the desktop is the subject: same WindowServer, a device that still answers; the hog may finish or be stopped
        const int ok = !failed && ws0 && ws0 == ws1 && after > 0;
        puts(ok ? "PASS" : "FAIL");
        return ok ? 0 : 2;
    }
}
