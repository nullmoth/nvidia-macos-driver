// Explicit GPU regression; run through tools/test_compute_stage_in.py with an isolated plugin.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <unistd.h>
int main(int argc,char **argv) { alarm(40); @autoreleasepool {
 if(argc!=3)return 1;
 id<MTLDevice>d=MTLCreateSystemDefaultDevice();NSError*e=nil;
 id fn=[NSClassFromString(@"NVMTLFunction") new];
 [fn setValue:[NSData dataWithContentsOfFile:@(argv[1])] forKey:@"spirv"];
 [fn setValue:[NSString stringWithContentsOfFile:@(argv[2]) encoding:NSUTF8StringEncoding error:&e] forKey:@"air"];
 [fn setValue:@"test" forKey:@"fname"];[fn setValue:@"kernel" forKey:@"stage"];
 id<MTLComputePipelineState>ps=[d newComputePipelineStateWithFunction:fn error:&e];if(!ps){fprintf(stderr,"pipeline %s\n",e.description.UTF8String);return 2;}
 id<MTLCommandQueue>q=[d newCommandQueue];id<MTLBuffer>out=[d newBufferWithLength:128 options:MTLResourceStorageModeShared];memset(out.contents,0,128);
 id<MTLCommandBuffer>cb=[q commandBuffer];id<MTLComputeCommandEncoder>ce=[cb computeCommandEncoder];
 [ce setComputePipelineState:ps];[ce setBuffer:out offset:0 atIndex:0];[ce dispatchThreadgroups:MTLSizeMake(1,1,1) threadsPerThreadgroup:MTLSizeMake(4,1,1)];[ce endEncoding];[cb commit];[cb waitUntilCompleted];
 if(cb.status==MTLCommandBufferStatusError)return 3;
 const uint32_t(*a)[8]=out.contents;
 for(unsigned i=0;i<4;i++) { unsigned src=(i+1)%4;printf("lane%u:",i);for(int k=0;k<8;k++)printf(" %u",a[i][k]);puts("");
 for(int k=0;k<4;k++){float expected=10.f*(k+1)+src;uint32_t bits;memcpy(&bits,&expected,4);if(a[i][k]!=bits)return 4;}
 if(a[i][4]!=100+src||a[i][5]!=77||a[i][6]!=88||a[i][7]!=99)return 5;
 }
 puts("PASS shared record values, cross-thread visibility, and padding");return 0;
}}
