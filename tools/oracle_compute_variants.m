// Explicit GPU regression; run through tools/test_compute_stage_in.py with an isolated plugin.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <unistd.h>
#include <math.h>
static id<MTLDevice> d; static id<MTLCommandQueue> q; static id<MTLLibrary> lib;
static id<MTLBuffer> vb, ib;
static id<MTLComputePipelineState> pipeline(NSString *name, int layout, BOOL icb) {
 NSError *e=nil;id<MTLFunction>fn=[lib newFunctionWithName:name];if(!fn){fprintf(stderr,"nil function %s\n",name.UTF8String);exit(2);}
 MTLComputePipelineDescriptor*p=[MTLComputePipelineDescriptor new];p.computeFunction=fn;p.supportIndirectCommandBuffers=icb;
 if(layout>=0){MTLStageInputOutputDescriptor*s=[MTLStageInputOutputDescriptor stageInputOutputDescriptor];
 s.attributes[0].format=MTLAttributeFormatFloat2;s.attributes[0].bufferIndex=1;s.attributes[0].offset=4+4*layout;
 s.layouts[1].stride=16;s.layouts[1].stepFunction=MTLStepFunctionThreadPositionInGridXIndexed;s.layouts[1].stepRate=1;s.indexBufferIndex=2;s.indexType=MTLIndexTypeUInt32;p.stageInputDescriptor=s;}
 id<MTLComputePipelineState>ps=[d newComputePipelineStateWithDescriptor:p options:0 reflection:nil error:&e];
 if(!ps){fprintf(stderr,"pipeline %s: %s\n",name.UTF8String,e.description.UTF8String);exit(3);}return ps;
}
static void finish(id<MTLCommandBuffer>cb){[cb commit];[cb waitUntilCompleted];if(cb.status!=MTLCommandBufferStatusCompleted){fprintf(stderr,"GPU: %s\n",cb.error.description.UTF8String);exit(4);}}
static void bind(id<MTLComputeCommandEncoder>ce,id<MTLComputePipelineState>ps,id<MTLBuffer>out,NSUInteger off){[ce setComputePipelineState:ps];[ce setBuffer:out offset:off atIndex:0];[ce setBuffer:vb offset:0 atIndex:1];[ce setBuffer:ib offset:0 atIndex:2];}
static void verify(id<MTLBuffer>b,NSUInteger off,int layout,int origin,int size,int n,float extra){
 const float(*v)[4]=vb.contents;const uint32_t*ix=ib.contents;const float(*a)[4]=(const void*)((char*)b.contents+off);
 for(int i=0;i<n;i++){float e[4]={v[ix[i+origin]][1+layout]+extra,v[ix[i+origin]][2+layout],size,origin};
 for(int k=0;k<4;k++)if(fabsf(a[i][k]-e[k])>0.0001f){fprintf(stderr,"FAIL row%d component%d got%g expected%g (layout%d origin%d)\n",i,k,a[i][k],e[k],layout,origin);exit(5);}}
}
int main(){alarm(90);@autoreleasepool{
 d=MTLCreateSystemDefaultDevice();q=[d newCommandQueue];NSError*e=nil;
 NSString*src=@"#include <metal_stdlib>\nusing namespace metal; struct In {float2 v [[attribute(0)]];};\n"
 "kernel void plain(In x [[stage_in]],device float4*o [[buffer(0)]],uint t [[thread_position_in_grid]],uint org [[grid_origin]],uint sz [[grid_size]]){o[t]=float4(x.v,float(sz),float(org));}\n"
 "kernel void variant(In x [[stage_in]],device float4*o [[buffer(0)]],uint t [[thread_position_in_grid]],uint lt [[thread_position_in_threadgroup]],uint org [[grid_origin]],uint sz [[grid_size]],texture2d<float> tex [[texture(0)]],sampler s [[sampler(0)]],threadgroup float*tmp [[threadgroup(0)]]){tmp[lt]=x.v.x;threadgroup_barrier(mem_flags::mem_threadgroup);o[t]=float4(tmp[lt]+tex.sample(s,float2(1.5,0.5)).x,x.v.y,float(sz),float(org));}\n"
 "kernel void region_only(device uint4*o [[buffer(0)]],uint3 org [[grid_origin]],uint3 sz [[grid_size]]){o[0]=uint4(org,1);o[1]=uint4(sz,1); }\n"
 "kernel void produce(device uint*region [[buffer(0)]],device uint*grid [[buffer(1)]],constant uint&origin [[buffer(2)]]){region[1]=origin;region[2]=0;region[3]=0;region[4]=8;region[5]=1;region[6]=1;grid[0]=1;grid[1]=1;grid[2]=1;grid[4]=0;grid[5]=2;}\n";
 lib=[d newLibraryWithSource:src options:nil error:&e];if(!lib){fprintf(stderr,"library %s\n",e.description.UTF8String);return 2;}
 const float v[8][4]={{-1,10,11,-2},{-1,20,21,-2},{-1,30,31,-2},{-1,40,41,-2},{-1,50,51,-2},{-1,60,61,-2},{-1,70,71,-2},{-1,80,81,-2}};
 const uint32_t ix[8]={5,3,0,2,1,4,6,7};vb=[d newBufferWithBytes:v length:sizeof v options:0];ib=[d newBufferWithBytes:ix length:sizeof ix options:0];
 id<MTLComputePipelineState>p0=pipeline(@"plain",0,YES),prod=pipeline(@"produce",-1,NO);
 id<MTLBuffer>out=[d newBufferWithLength:512 options:0];
 // Region built-ins are valid even when the kernel has no stage-in attribute descriptor.
 id<MTLComputePipelineState>regionOnly=pipeline(@"region_only",-1,NO);
 id<MTLCommandBuffer>regionCB=[q commandBuffer];id<MTLComputeCommandEncoder>regionCE=[regionCB computeCommandEncoder];[regionCE setComputePipelineState:regionOnly];[regionCE setBuffer:out offset:0 atIndex:0];[regionCE setStageInRegion:MTLRegionMake3D(2,3,4,8,9,10)];[regionCE dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(1,1,1)];[regionCE endEncoding];finish(regionCB);
 uint32_t expectedRegion[8]={2,3,4,1,8,9,10,1};if(memcmp(out.contents,expectedRegion,32)){fprintf(stderr,"region-only failed\n");return 7;}puts("PASS region-only kernel all 3 dimensions");
 // Different input layouts of the same AIR must never share a sampler/threadgroup variant.
 MTLTextureDescriptor*td=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR32Float width:4 height:1 mipmapped:NO];td.usage=MTLTextureUsageShaderRead;
 id<MTLTexture>tex=[d newTextureWithDescriptor:td];float texels[4]={1,2,3,4};[tex replaceRegion:MTLRegionMake2D(0,0,4,1) mipmapLevel:0 withBytes:texels bytesPerRow:16];
 id<MTLComputePipelineState>vp[2]={pipeline(@"variant",0,NO),pipeline(@"variant",1,NO)};
 for(int repeat=0;repeat<2;repeat++)for(int layout=0;layout<2;layout++)for(int pix=0;pix<2;pix++)for(int tg=16;tg<=32;tg+=16){
  MTLSamplerDescriptor*sd=[MTLSamplerDescriptor new];sd.normalizedCoordinates=!pix;sd.sAddressMode=MTLSamplerAddressModeClampToEdge;sd.tAddressMode=MTLSamplerAddressModeClampToEdge;id<MTLSamplerState>sam=[d newSamplerStateWithDescriptor:sd];
  id<MTLCommandBuffer>cb=[q commandBuffer];id<MTLComputeCommandEncoder>ce=[cb computeCommandEncoder];bind(ce,vp[layout],out,0);[ce setTexture:tex atIndex:0];[ce setSamplerState:sam atIndex:0];[ce setThreadgroupMemoryLength:tg atIndex:0];[ce setStageInRegion:MTLRegionMake3D(1,0,0,8,1,1)];[ce dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(4,1,1)];[ce endEncoding];finish(cb);verify(out,0,layout,1,8,4,pix?2:4);printf("PASS variant layout%d pixel%d tg%d repeat%d\n",layout,pix,tg,repeat);
 }
 // Private, GPU-produced region at a nonzero 4-byte offset; rewritten between two dispatches.
 id<MTLBuffer>region=[d newBufferWithLength:64 options:MTLResourceStorageModePrivate],grid=[d newBufferWithLength:32 options:MTLResourceStorageModePrivate];
 id<MTLCommandBuffer>cb=[q commandBuffer];id<MTLComputeCommandEncoder>ce=[cb computeCommandEncoder];[ce setStageInRegionWithIndirectBuffer:region indirectBufferOffset:4];
 for(uint32_t origin=1;origin<=2;origin++){
  [ce setComputePipelineState:prod];[ce setBuffer:region offset:0 atIndex:0];[ce setBuffer:grid offset:0 atIndex:1];[ce setBytes:&origin length:4 atIndex:2];[ce dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(1,1,1)];
  bind(ce,p0,out,(origin-1)*64);[ce dispatchThreadgroupsWithIndirectBuffer:grid indirectBufferOffset:0 threadsPerThreadgroup:MTLSizeMake(4,1,1)];
 }
 // Explicit direct region must replace the indirect binding; exercise a nonuniform dispatch too.
 bind(ce,p0,out,128);[ce setStageInRegion:MTLRegionMake3D(1,0,0,8,1,1)];[ce dispatchThreads:MTLSizeMake(5,1,1) threadsPerThreadgroup:MTLSizeMake(3,1,1)];[ce endEncoding];finish(cb);
 verify(out,0,0,1,8,4,0);verify(out,64,0,2,8,4,0);verify(out,128,0,1,8,5,0);puts("PASS GPU indirect region, rewrite, direct override, tail dispatch");
 // CPU-encoded indirect commands retain separate regions through copy/reset, and both execution APIs.
 MTLIndirectCommandBufferDescriptor*idc=[MTLIndirectCommandBufferDescriptor new];idc.commandTypes=MTLIndirectCommandTypeConcurrentDispatch;idc.inheritBuffers=NO;idc.inheritPipelineState=NO;idc.maxKernelBufferBindCount=3;
 id<MTLIndirectCommandBuffer>icb=[d newIndirectCommandBufferWithDescriptor:idc maxCommandCount:2 options:0];
 for(int i=0;i<2;i++){id<MTLIndirectComputeCommand>c=[icb indirectComputeCommandAtIndex:i];[c setComputePipelineState:p0];[c setKernelBuffer:out offset:i*64 atIndex:0];[c setKernelBuffer:vb offset:0 atIndex:1];[c setKernelBuffer:ib offset:0 atIndex:2];[c setStageInRegion:MTLRegionMake3D(i+1,0,0,8,1,1)];[c concurrentDispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(4,1,1)];}
 id<MTLIndirectCommandBuffer>copy=[d newIndirectCommandBufferWithDescriptor:idc maxCommandCount:2 options:0];
 cb=[q commandBuffer];id<MTLBlitCommandEncoder>blit=[cb blitCommandEncoder];[blit copyIndirectCommandBuffer:icb sourceRange:NSMakeRange(0,2) destination:copy destinationIndex:0];[blit endEncoding];ce=[cb computeCommandEncoder];[ce setStageInRegion:MTLRegionMake3D(3,0,0,8,1,1)];[ce executeCommandsInBuffer:copy withRange:NSMakeRange(0,2)];bind(ce,p0,out,256);[ce dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(4,1,1)];[ce endEncoding];finish(cb);verify(out,256,0,3,8,4,0);verify(out,0,0,1,8,4,0);verify(out,64,0,2,8,4,0);puts("PASS ICB copy and per-command regions");
 memset(out.contents,0,128);cb=[q commandBuffer];ce=[cb computeCommandEncoder];[ce executeCommandsInBuffer:copy indirectBuffer:grid indirectBufferOffset:16];[ce endEncoding];finish(cb);verify(out,0,0,1,8,4,0);verify(out,64,0,2,8,4,0);puts("PASS ICB GPU-written execution range");
 [copy resetWithRange:NSMakeRange(0,2)];memset(out.contents,0,128);cb=[q commandBuffer];ce=[cb computeCommandEncoder];[ce executeCommandsInBuffer:copy withRange:NSMakeRange(0,2)];[ce endEncoding];finish(cb);for(int i=0;i<32;i++)if(((float*)out.contents)[i]!=0)return 6;puts("PASS ICB reset");
 return 0;
}}
