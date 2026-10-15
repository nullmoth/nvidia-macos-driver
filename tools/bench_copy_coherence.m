#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <string.h>
int main(int argc,char **argv) { @autoreleasepool {
 if(argc!=2)return 2;
 if(!dlopen(argv[1],RTLD_NOW|RTLD_LOCAL)){fprintf(stderr,"LOAD: %s\n",dlerror());return 3;}
 io_service_t svc=IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel")); if(!svc)return 4;
 id<MTLDevice>d=((id(*)(id,SEL,mach_port_t))objc_msgSend)([NSClassFromString(@"NVMTLDevice") alloc],NSSelectorFromString(@"initWithAcceleratorPort:"),svc);IOObjectRelease(svc);if(!d)return 5;
 printf("DEVICE %s\n",d.name.UTF8String);fflush(stdout);
 // Intentionally unaligned: exercise NullMoth's documented copied-buffer fallback.
 unsigned char *allocation=calloc(1,4097),*cpu=allocation+1;
 id<MTLBuffer>b=[d newBufferWithBytesNoCopy:cpu length:4096 options:MTLResourceStorageModeShared deallocator:nil];if(!b)return 6;
 Ivar host=class_getInstanceVariable(object_getClass(b),"_hostPtr");void *ptr=NULL;
 if(host)memcpy(&ptr,(char *)(__bridge void *)b+ivar_getOffset(host),sizeof(ptr));
 if(ptr!=cpu){printf("SKIP not the copied-buffer path\n");return 7;}
 MTLTextureDescriptor*td=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:16 height:16 mipmapped:NO];td.storageMode=MTLStorageModeShared;td.usage=MTLTextureUsageShaderRead|MTLTextureUsageShaderWrite;
 id<MTLTexture>t=[b newTextureWithDescriptor:td offset:0 bytesPerRow:256];if(!t)return 8;
 id<MTLTexture>v=[t newTextureViewWithPixelFormat:MTLPixelFormatRGBA8Unorm];if(!v)return 9;
 id<MTLCommandQueue>q=[d newCommandQueue];id<MTLBuffer>out=[d newBufferWithLength:4096 options:MTLResourceStorageModeShared];id<MTLBuffer>input=[d newBufferWithLength:4096 options:MTLResourceStorageModeShared];
 if(!q||!out||!input)return 10;unsigned readBad=0,writeBad=0;
 for(unsigned n=0;n<16;n++) {@autoreleasepool {
 unsigned char value=31+n;memset(cpu,value,4096);
 id<MTLCommandBuffer>c=[q commandBuffer];id<MTLBlitCommandEncoder>bl=[c blitCommandEncoder];
 [bl copyFromTexture:(n&1)?v:t sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0,0,0) sourceSize:MTLSizeMake(16,16,1) toBuffer:out destinationOffset:0 destinationBytesPerRow:256 destinationBytesPerImage:4096];[bl endEncoding];[c commit];[c waitUntilCompleted];
 if(c.error){printf("GPU ERROR %s\n",c.error.description.UTF8String);return 11;}
 unsigned char*got=out.contents;BOOL bad=NO;for(unsigned y=0;y<16;y++)for(unsigned x=0;x<64;x++)if(got[y*256+x]!=value)bad=YES;readBad+=bad;
 value=101+n;memset(input.contents,value,4096);
 c=[q commandBuffer];bl=[c blitCommandEncoder];
 [bl copyFromBuffer:input sourceOffset:0 sourceBytesPerRow:256 sourceBytesPerImage:4096 sourceSize:MTLSizeMake(16,16,1) toTexture:(n&1)?v:t destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0,0,0)];[bl endEncoding];[c commit];[c waitUntilCompleted];
 if(c.error){printf("GPU ERROR %s\n",c.error.description.UTF8String);return 11;}
 bad=NO;for(unsigned y=0;y<16;y++)for(unsigned x=0;x<64;x++)if(cpu[y*256+x]!=value)bad=YES;writeBad+=bad;
 }}
 printf("RESULT frames=16 host_to_texture_bad=%u texture_to_host_bad=%u\n",readBad,writeBad);fflush(stdout);
 // The buffer owns the host pointer for the lifetime of this short process.
 return (readBad||writeBad)?1:0;
}}
