// The end sweep on an Apple GPU: for each sweep case, SW_BATCHES batches of SW_BATCH rays, one dump line per ray.
//   usage: curve_sweep <curve_oracle.metallib> <out.txt>
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include "sweep.h"
typedef struct { float o[3]; float tmin; float d[3]; float tmax; } RayIn;
typedef struct { float t; float u; uint32_t prim; uint32_t hit; } HitOut;
static uint32_t bits(float f) { uint32_t b; memcpy(&b, &f, 4); return b; }
int main(int argc, char **argv)
{
   @autoreleasepool {
      if (argc != 3) return 2;
      id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *err = nil;
      id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&err];
      id<MTLComputePipelineState> pso = lib ? [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"curve_oracle"] error:&err] : nil;
      if (!pso) { fprintf(stderr, "REFUSED: %s\n", err.localizedDescription.UTF8String); return 3; }
      id<MTLCommandQueue> q = [dev newCommandQueue];
      struct curve_case cases[8]; int nc = sw_cases(cases);
      static struct oracle_ray rs[SW_BATCH]; static RayIn rin[SW_BATCH];
      id<MTLBuffer> rb = [dev newBufferWithLength:sizeof rin options:MTLResourceStorageModeShared];
      id<MTLBuffer> ob = [dev newBufferWithLength:sizeof(HitOut) * SW_BATCH options:MTLResourceStorageModeShared];
      FILE *f = fopen(argv[2], "w"); fprintf(f, "# device %s\n", dev.name.UTF8String);
      for (int c = 0; c < nc; c++) {
         struct curve_case *k = &cases[c];
         float cp[4][3], rad[4]; uint32_t idx[1] = { 0 };
         for (int i = 0; i < 4; i++) { memcpy(cp[i], k->p[i], 12); rad[i] = k->r[i]; }
         MTLAccelerationStructureCurveGeometryDescriptor *g = [MTLAccelerationStructureCurveGeometryDescriptor descriptor];
         g.controlPointBuffer = [dev newBufferWithBytes:cp length:sizeof cp options:MTLResourceStorageModeShared];
         g.controlPointCount = 4; g.controlPointStride = 12; g.controlPointFormat = MTLAttributeFormatFloat3;
         g.radiusBuffer = [dev newBufferWithBytes:rad length:sizeof rad options:MTLResourceStorageModeShared]; g.radiusFormat = MTLAttributeFormatFloat; g.radiusStride = 4;
         g.indexBuffer = [dev newBufferWithBytes:idx length:sizeof idx options:MTLResourceStorageModeShared]; g.indexType = MTLIndexTypeUInt32;
         g.segmentCount = 1; g.segmentControlPointCount = k->count;
         g.curveType = k->type ? MTLCurveTypeFlat : MTLCurveTypeRound; g.curveBasis = (MTLCurveBasis)k->basis; g.curveEndCaps = (MTLCurveEndCaps)k->caps; g.opaque = YES;
         MTLPrimitiveAccelerationStructureDescriptor *d = [MTLPrimitiveAccelerationStructureDescriptor descriptor]; d.geometryDescriptors = @[ g ];
         MTLAccelerationStructureSizes sz = [dev accelerationStructureSizesWithDescriptor:d];
         id<MTLAccelerationStructure> as = [dev newAccelerationStructureWithSize:sz.accelerationStructureSize];
         id<MTLBuffer> sc = [dev newBufferWithLength:sz.buildScratchBufferSize options:MTLResourceStorageModePrivate];
         id<MTLCommandBuffer> cb0 = [q commandBuffer]; id<MTLAccelerationStructureCommandEncoder> ae = [cb0 accelerationStructureCommandEncoder];
         [ae buildAccelerationStructure:as descriptor:d scratchBuffer:sc scratchBufferOffset:0]; [ae endEncoding]; [cb0 commit]; [cb0 waitUntilCompleted];
         for (int b = 0; b < SW_BATCHES; b++) {
            sw_rays(k, b, rs);
            for (int i = 0; i < SW_BATCH; i++) { memcpy(rin[i].o, rs[i].o, 12); memcpy(rin[i].d, rs[i].d, 12); rin[i].tmin = rs[i].tmin; rin[i].tmax = rs[i].tmax; }
            memcpy([rb contents], rin, sizeof rin);
            id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:pso]; [ce setAccelerationStructure:as atBufferIndex:0]; [ce setBuffer:rb offset:0 atIndex:1]; [ce setBuffer:ob offset:0 atIndex:2];
            [ce useResource:as usage:MTLResourceUsageRead];
            [ce dispatchThreads:MTLSizeMake(SW_BATCH, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)]; [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];
            if (cb.error) { fprintf(f, "case %s batch %d REFUSED %s\n", k->name, b, cb.error.localizedDescription.UTF8String); continue; }
            HitOut *h = (HitOut *)ob.contents;
            fprintf(f, "case %s batch %d\n", k->name, b);
            for (int i = 0; i < SW_BATCH; i++) fprintf(f, "ray %d hit %u t %08x u %08x\n", i, h[i].hit, bits(h[i].t), bits(h[i].u));
         }
      }
      fclose(f);
   }
   return 0;
}
