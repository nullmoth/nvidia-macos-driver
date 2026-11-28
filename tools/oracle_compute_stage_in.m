// Explicit GPU regression; run through tools/test_compute_stage_in.py with an isolated plugin.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <unistd.h>
int main(void) { alarm(40); @autoreleasepool {
 id<MTLDevice>d=MTLCreateSystemDefaultDevice();NSError*e=nil;
 NSString*s=@"#include <metal_stdlib>\nusing namespace metal;\nstruct In { float2 v [[attribute(0)]]; }; kernel void test(In x [[stage_in]], device float4 *o [[buffer(0)]], uint2 t [[thread_position_in_grid]], uint2 org [[grid_origin]], uint2 sz [[grid_size]]) { o[t.y*4+t.x]=float4(x.v,float(sz.x),float(org.x)); }";
 id<MTLLibrary>l=[d newLibraryWithSource:s options:nil error:&e]; if(!l){fprintf(stderr,"library: %s\n",e.description.UTF8String);return 2;}
 const float v[6][4]={{-1,10,11,-2},{-1,20,21,-2},{-1,30,31,-2},{-1,40,41,-2},{-1,50,51,-2},{-1,60,61,-2}};
 const uint32_t ix32[8]={5,3,0,2,1,4,0,0};const uint16_t ix16[8]={5,3,0,2,1,4,0,0};
 id<MTLBuffer>vb=[d newBufferWithBytes:v length:sizeof v options:MTLResourceStorageModeShared];
 id<MTLCommandQueue>q=[d newCommandQueue];
 for(NSUInteger axis=0;axis<2;axis++) for(NSUInteger bits=0;bits<2;bits++) {
  id<MTLFunction>fn=[l newFunctionWithName:@"test"];
  if(!fn){fprintf(stderr,"function translation refused\n");return 3;}
  MTLComputePipelineDescriptor*p=[MTLComputePipelineDescriptor new];p.computeFunction=fn;
  MTLStageInputOutputDescriptor*sd=[MTLStageInputOutputDescriptor stageInputOutputDescriptor];
  sd.attributes[0].format=MTLAttributeFormatFloat2;sd.attributes[0].bufferIndex=1;sd.attributes[0].offset=4;
  sd.layouts[1].stride=16;sd.layouts[1].stepFunction=axis?MTLStepFunctionThreadPositionInGridYIndexed:MTLStepFunctionThreadPositionInGridXIndexed;sd.layouts[1].stepRate=1;
  sd.indexBufferIndex=2;sd.indexType=bits?MTLIndexTypeUInt32:MTLIndexTypeUInt16;p.stageInputDescriptor=sd;
  id<MTLComputePipelineState>ps=[d newComputePipelineStateWithDescriptor:p options:0 reflection:nil error:&e];if(!ps){fprintf(stderr,"pipeline: %s\n",e.description.UTF8String);return 3;}
  id<MTLBuffer>ib=[d newBufferWithBytes:bits?(const void*)ix32:(const void*)ix16 length:bits?sizeof ix32:sizeof ix16 options:MTLResourceStorageModeShared];
  id<MTLBuffer>out=[d newBufferWithLength:64 options:MTLResourceStorageModeShared];memset(out.contents,0xFF,64);
  id<MTLCommandBuffer>cb=[q commandBuffer];id<MTLComputeCommandEncoder>ce=[cb computeCommandEncoder];
  [ce setComputePipelineState:ps];[ce setBuffer:out offset:0 atIndex:0];[ce setBuffer:vb offset:0 atIndex:1];[ce setBuffer:ib offset:0 atIndex:2];
  [ce setStageInRegion:MTLRegionMake3D(axis?0:1,axis?1:0,0,8,8,1)];[ce dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(4,1,1)];[ce endEncoding];[cb commit];[cb waitUntilCompleted];
  if(cb.status==MTLCommandBufferStatusError){fprintf(stderr,"command: %s\n",cb.error.description.UTF8String);return 4;}
  const float(*a)[4]=out.contents;BOOL ok=YES;for(int i=0;i<4;i++){printf("index%lu[%d] = %g %g %g %g\n",bits?32ul:16ul,i,a[i][0],a[i][1],a[i][2],a[i][3]);ok &= a[i][0]==v[ix32[axis?1:i+1]][1]&&a[i][1]==v[ix32[axis?1:i+1]][2]&&a[i][2]==8&&a[i][3]==(axis?0:1);}
  if(!ok)return 5;puts("PASS");
 }
 return 0;
}}
