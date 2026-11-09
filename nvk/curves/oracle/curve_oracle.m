// The Apple oracle for curve geometry: builds one curve acceleration structure per case of curve_cases.h on the machine's own GPU,
// intersects the fixed ray set, and writes distance, curve_parameter and primitive id for every ray as hex float bits.
//   usage: curve_oracle <curve_oracle.metallib> <out.txt>          (run it through run_oracle.sh, which checks the machine first)
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include "curve_cases.h"

typedef struct { float o[3]; float tmin; float d[3]; float tmax; } RayIn;
typedef struct { float t; float u; uint32_t prim; uint32_t hit; } HitOut;

static uint32_t bits(float f) { uint32_t b; memcpy(&b, &f, 4); return b; }

int main(int argc, char **argv)
{
   @autoreleasepool {
      if (argc != 3) { fprintf(stderr, "usage: curve_oracle <metallib> <out.txt>\n"); return 2; }
      id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
      if (!dev) { fprintf(stderr, "REFUSED: no Metal device\n"); return 3; }
      NSError *err = nil;
      id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&err];
      id<MTLFunction> fn = lib ? [lib newFunctionWithName:@"curve_oracle"] : nil;
      id<MTLComputePipelineState> pso = fn ? [dev newComputePipelineStateWithFunction:fn error:&err] : nil;
      if (!pso) { fprintf(stderr, "REFUSED: pipeline: %s\n", err.localizedDescription.UTF8String); return 3; }
      id<MTLCommandQueue> q = [dev newCommandQueue];

      int probe = getenv("ORACLE_PROBE") != NULL;
      struct curve_case cases[32]; int nc = probe ? oracle_probe_cases(cases) : oracle_cases(cases);
      static struct oracle_ray rs[ORACLE_RAYS]; if (!probe) oracle_ray_set(rs);
      RayIn rin[ORACLE_RAYS];
      for (int i = 0; i < ORACLE_RAYS; i++) { memcpy(rin[i].o, rs[i].o, 12); memcpy(rin[i].d, rs[i].d, 12); rin[i].tmin = rs[i].tmin; rin[i].tmax = rs[i].tmax; }
      id<MTLBuffer> rb = [dev newBufferWithBytes:rin length:sizeof rin options:MTLResourceStorageModeShared];
      id<MTLBuffer> ob = [dev newBufferWithLength:sizeof(HitOut) * ORACLE_RAYS options:MTLResourceStorageModeShared];
      FILE *f = fopen(argv[2], "w");
      fprintf(f, "# device %s\n", dev.name.UTF8String);
      int refused = 0;
      for (int c = 0; c < nc; c++) {
         struct curve_case *k = &cases[c];
         if (probe) { oracle_probe_rays(k, rs); for (int i = 0; i < ORACLE_RAYS; i++) { memcpy(rin[i].o, rs[i].o, 12); memcpy(rin[i].d, rs[i].d, 12); rin[i].tmin = rs[i].tmin; rin[i].tmax = rs[i].tmax; } memcpy([rb contents], rin, sizeof rin); }
         float cp[4][3]; float rad[4]; uint32_t idx[1] = { 0 };
         for (int i = 0; i < 4; i++) { memcpy(cp[i], k->p[i], 12); rad[i] = k->r[i]; }
         MTLAccelerationStructureCurveGeometryDescriptor *g = [MTLAccelerationStructureCurveGeometryDescriptor descriptor];
         g.controlPointBuffer = [dev newBufferWithBytes:cp length:sizeof cp options:MTLResourceStorageModeShared];
         g.controlPointCount = 4; g.controlPointStride = 12; g.controlPointFormat = MTLAttributeFormatFloat3;
         g.radiusBuffer = [dev newBufferWithBytes:rad length:sizeof rad options:MTLResourceStorageModeShared];
         g.radiusFormat = MTLAttributeFormatFloat; g.radiusStride = 4;
         g.indexBuffer = [dev newBufferWithBytes:idx length:sizeof idx options:MTLResourceStorageModeShared];
         g.indexType = MTLIndexTypeUInt32;
         g.segmentCount = 1; g.segmentControlPointCount = k->count;
         g.curveType = k->type ? MTLCurveTypeFlat : MTLCurveTypeRound;
         g.curveBasis = (MTLCurveBasis)k->basis;
         g.curveEndCaps = (MTLCurveEndCaps)k->caps;
         g.opaque = YES;
         MTLPrimitiveAccelerationStructureDescriptor *d = [MTLPrimitiveAccelerationStructureDescriptor descriptor];
         d.geometryDescriptors = @[ g ];
         MTLAccelerationStructureSizes sz = [dev accelerationStructureSizesWithDescriptor:d];
         id<MTLAccelerationStructure> as = [dev newAccelerationStructureWithSize:sz.accelerationStructureSize];
         id<MTLBuffer> scratch = [dev newBufferWithLength:sz.buildScratchBufferSize options:MTLResourceStorageModePrivate];
         if (!as || !scratch) { fprintf(f, "case %s REFUSED size\n", k->name); refused++; continue; }
         id<MTLCommandBuffer> cb = [q commandBuffer];
         id<MTLAccelerationStructureCommandEncoder> ae = [cb accelerationStructureCommandEncoder];
         [ae buildAccelerationStructure:as descriptor:d scratchBuffer:scratch scratchBufferOffset:0];
         [ae endEncoding];
         id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
         [ce setComputePipelineState:pso];
         [ce setAccelerationStructure:as atBufferIndex:0];
         [ce setBuffer:rb offset:0 atIndex:1];
         [ce setBuffer:ob offset:0 atIndex:2];
         [ce useResource:as usage:MTLResourceUsageRead];
         [ce dispatchThreads:MTLSizeMake(ORACLE_RAYS, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
         [ce endEncoding];
         [cb commit]; [cb waitUntilCompleted];
         if (cb.error) { fprintf(f, "case %s REFUSED %s\n", k->name, cb.error.localizedDescription.UTF8String); refused++; continue; }
         HitOut *h = (HitOut *)ob.contents;
         fprintf(f, "case %s basis %d count %d type %d caps %d\n", k->name, k->basis, k->count, k->type, k->caps);
         for (int i = 0; i < ORACLE_RAYS; i++)
            fprintf(f, "ray %d hit %u t %08x u %08x prim %u\n", i, h[i].hit, bits(h[i].t), bits(h[i].u), h[i].prim);
      }
      fclose(f);
      return refused ? 4 : 0;
   }
}
