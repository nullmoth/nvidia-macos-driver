// Ruler for ray-traced curves: builds one curve geometry per (basis, type) and traces a fan of rays at it, printing
// distance and curve parameter per ray. Run on Apple hardware for the reference; on the NVIDIA driver to compare.
// run: curves curves.metallib [caps|oblique|mid] [query|plain] [private]   (query: trace through intersection_query<curve_data>)   (reference_m1.txt / reference_caps_m1.txt are the Apple GPU's answers)
// build: xcrun metal -c curves.metal -o curves.air && xcrun metallib curves.air -o curves.metallib
//        clang -arch arm64 -arch x86_64 -fobjc-arc -mmacosx-version-min=14.0 curves.m -framework Metal -framework Foundation -o curves
#import <Metal/Metal.h>
#include <math.h>
#include <string.h>
#import <Foundation/Foundation.h>
#include <stdio.h>
int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: curves curves.metallib\n"); return 2; }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        NSError *e = nil;
        id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
        id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:(argc > 3 && !strcmp(argv[3], "query")) ? @"trace_curves_query" : @"trace_curves"] error:&e];
        if (!ps) { printf("FAIL pipeline: %s\n", e.localizedDescription.UTF8String); return 1; }
        id<MTLCommandQueue> q = [dev newCommandQueue];
        // control points of a gently bent curve in the z = 0 plane, x from -1 to 1
        const float cp[5][3] = { {-1.5f, 0.2f, 0}, {-1.0f, -0.1f, 0}, {0.0f, 0.3f, 0}, {1.0f, -0.1f, 0}, {1.5f, 0.2f, 0} };
        const float rad[5] = { 0.10f, 0.12f, 0.15f, 0.12f, 0.10f };
        // "private" anywhere in the arguments puts the control points, radii and indices in GPU-only memory
        int priv = 0; for (int a = 2; a < argc; a++) if (!strcmp(argv[a], "private")) priv = 1;
        id<MTLBuffer> (^gpu)(const void *, NSUInteger) = ^id<MTLBuffer>(const void *bytes, NSUInteger len) {
            id<MTLBuffer> shared = [dev newBufferWithBytes:bytes length:len options:MTLResourceStorageModeShared];
            if (!priv) return shared;
            id<MTLBuffer> p = [dev newBufferWithLength:len options:MTLResourceStorageModePrivate];
            id<MTLCommandBuffer> c = [q commandBuffer]; id<MTLBlitCommandEncoder> b = [c blitCommandEncoder];
            [b copyFromBuffer:shared sourceOffset:0 toBuffer:p destinationOffset:0 size:len]; [b endEncoding]; [c commit]; [c waitUntilCompleted];
            return p;
        };
        id<MTLBuffer> cpb = gpu(cp, sizeof cp);
        id<MTLBuffer> rb = gpu(rad, sizeof rad);
        // "caps" mode traces each basis and type with every end cap style, with rays aimed along the curve at its two
        // ends (set per geometry below); the default traces the middle with rays across the curve
        // "oblique" traces the same cap styles with rays at 45 degrees to the end tangent, swept across each end point
        const int oblique = argc > 2 && !strcmp(argv[2], "oblique"), caps = oblique || (argc > 2 && !strcmp(argv[2], "caps"));
        const int NR = 41; float rays[NR][6];
        for (int k = 0; k < NR; k++) { float x = -1.0f + 2.0f * k / (NR - 1); rays[k][0] = x; rays[k][1] = 0.05f; rays[k][2] = -5; rays[k][3] = 0; rays[k][4] = 0; rays[k][5] = 1; }
        id<MTLBuffer> rayb = [dev newBufferWithBytes:rays length:sizeof rays options:MTLResourceStorageModeShared];
        id<MTLBuffer> out = [dev newBufferWithLength:NR * 16 options:MTLResourceStorageModeShared];
        struct { MTLCurveBasis b; NSUInteger cpps; const char *name; } bases[] = {
            { MTLCurveBasisLinear, 2, "linear" }, { MTLCurveBasisBSpline, 3, "bspline2" }, { MTLCurveBasisBSpline, 4, "bspline3" },
            { MTLCurveBasisCatmullRom, 4, "catmullrom" }, { MTLCurveBasisBezier, 4, "bezier3" } };
        for (int cap = 0; cap < (caps ? 3 : 1); cap++) for (int bi = 0; bi < 5; bi++) for (int ty = 0; ty < 2; ty++) {
            if (caps) {
                // the curve's end points and end tangents, from the basis weights at u = 0 and u = 1 of the first and
                // last segments; 20 rays come in along each end's tangent, offset across it, plus one along the axis
                const int n = (int)bases[bi].cpps, last = 5 - n;
                float e[2][3], t[2][3];
                for (int end = 0; end < 2; end++) {
                    const int s0 = end ? last : 0; const float u = end ? 1.0f : 0.0f, h = end ? -1e-3f : 1e-3f;
                    float w[2][4];
                    for (int j = 0; j < 2; j++) {
                        const float uu = u + (j ? h : 0), v = 1 - uu;
                        if (bases[bi].b == MTLCurveBasisLinear) { w[j][0] = v; w[j][1] = uu; w[j][2] = w[j][3] = 0; }
                        else if (bases[bi].b == MTLCurveBasisBSpline && n == 3) { w[j][0] = 0.5f*v*v; w[j][1] = 0.5f + uu*v; w[j][2] = 0.5f*uu*uu; w[j][3] = 0; }
                        else if (bases[bi].b == MTLCurveBasisBSpline) { w[j][0] = v*v*v/6; w[j][1] = (3*uu*uu*uu - 6*uu*uu + 4)/6; w[j][2] = (-3*uu*uu*uu + 3*uu*uu + 3*uu + 1)/6; w[j][3] = uu*uu*uu/6; }
                        else if (bases[bi].b == MTLCurveBasisCatmullRom) { w[j][0] = 0.5f*(-uu*uu*uu + 2*uu*uu - uu); w[j][1] = 0.5f*(3*uu*uu*uu - 5*uu*uu + 2); w[j][2] = 0.5f*(-3*uu*uu*uu + 4*uu*uu + uu); w[j][3] = 0.5f*(uu*uu*uu - uu*uu); }
                        else { w[j][0] = v*v*v; w[j][1] = 3*uu*v*v; w[j][2] = 3*uu*uu*v; w[j][3] = uu*uu*uu; }
                    }
                    for (int c = 0; c < 3; c++) {
                        float p0 = 0, p1 = 0;
                        for (int i = 0; i < n; i++) { p0 += w[0][i] * cp[s0 + i][c]; p1 += w[1][i] * cp[s0 + i][c]; }
                        e[end][c] = p0; t[end][c] = (p1 - p0) / h * (end ? 1 : -1);   // pointing out of the curve
                    }
                    const float l = sqrtf(t[end][0]*t[end][0] + t[end][1]*t[end][1] + t[end][2]*t[end][2]);
                    for (int c = 0; c < 3; c++) t[end][c] /= l;
                }
                for (int k = 0; k < NR && oblique; k++) {
                    // aim at axis points from 0.3 inside to 0.3 beyond the end, coming down from +z tilted outward
                    const int end = k < 20 ? 0 : 1, j = k < 20 ? k : k - 20;
                    const float sd = j == 20 ? -0.02f : -0.3f + 0.6f * (j % 20) / 19.0f, h2 = 0.70710678f;
                    for (int c = 0; c < 3; c++) {
                        const float dir = h2 * (c == 2 ? 1.0f : 0.0f) + h2 * t[end][c];
                        rays[k][c] = e[end][c] + sd * t[end][c] + 2.0f * dir; rays[k][3 + c] = -dir;
                    }
                }
                for (int k = 0; k < NR && !oblique; k++) {
                    const int end = k < 20 ? 0 : 1, j = k < 20 ? k : k - 20;
                    // offsets across the end: a cross in the plane normal to the tangent, out to 1.5x the end radius
                    const float rr = 0.10f, off = j == 20 ? 0 : (j % 10 - 4.5f) / 3.0f * rr;
                    const float ax[3] = { -t[end][1], t[end][0], 0 }, az[3] = { 0, 0, 1 };
                    const float *dir = j < 10 ? ax : az;
                    for (int c = 0; c < 3; c++) { rays[k][c] = e[end][c] + 2.0f * t[end][c] + off * dir[c]; rays[k][3 + c] = -t[end][c]; }
                }
                memcpy(rayb.contents, rays, sizeof rays);
            }
            MTLAccelerationStructureCurveGeometryDescriptor *g = [MTLAccelerationStructureCurveGeometryDescriptor descriptor];
            g.controlPointBuffer = cpb; g.controlPointCount = 5; g.controlPointStride = 12; g.controlPointFormat = MTLAttributeFormatFloat3;
            g.radiusBuffer = rb; g.radiusStride = 4; g.radiusFormat = MTLAttributeFormatFloat;
            g.curveBasis = bases[bi].b; g.segmentControlPointCount = bases[bi].cpps;
            g.curveType = ty ? MTLCurveTypeFlat : MTLCurveTypeRound; g.curveEndCaps = (MTLCurveEndCaps)cap;
            NSUInteger nseg = 5 - bases[bi].cpps + 1; uint32_t idx[4] = { 0, 1, 2, 3 };
            g.indexBuffer = gpu(idx, sizeof idx); g.indexType = MTLIndexTypeUInt32;
            g.segmentCount = nseg;
            MTLPrimitiveAccelerationStructureDescriptor *pd = [MTLPrimitiveAccelerationStructureDescriptor descriptor]; pd.geometryDescriptors = @[g];
            MTLAccelerationStructureSizes sz = [dev accelerationStructureSizesWithDescriptor:pd];
            id<MTLAccelerationStructure> as = [dev newAccelerationStructureWithSize:sz.accelerationStructureSize];
            id<MTLBuffer> scratch = [dev newBufferWithLength:sz.buildScratchBufferSize options:MTLResourceStorageModePrivate];
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLAccelerationStructureCommandEncoder> ae = [cb accelerationStructureCommandEncoder];
            [ae buildAccelerationStructure:as descriptor:pd scratchBuffer:scratch scratchBufferOffset:0]; [ae endEncoding];
            id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:ps]; [ce setAccelerationStructure:as atBufferIndex:0]; [ce setBuffer:rayb offset:0 atIndex:1]; [ce setBuffer:out offset:0 atIndex:2];
            [ce useResource:as usage:MTLResourceUsageRead];
            [ce dispatchThreads:MTLSizeMake(NR, 1, 1) threadsPerThreadgroup:MTLSizeMake(NR, 1, 1)]; [ce endEncoding];
            [cb commit]; [cb waitUntilCompleted];
            const float *o = out.contents;
            static const char *capname[3] = { "none", "disk", "sphere" };
            if (caps) printf("%s %s %s%s:", bases[bi].name, ty ? "flat" : "round", capname[cap], oblique ? " oblique" : "");
            else printf("%s %s:", bases[bi].name, ty ? "flat" : "round");
            for (int k = 0; k < NR; k++) printf(" %.4f/%.4f/%u", o[k * 4], o[k * 4 + 1], ((const uint32_t *)o)[k * 4 + 3]);
            printf("\n");
        }
    }
    return 0;
}
