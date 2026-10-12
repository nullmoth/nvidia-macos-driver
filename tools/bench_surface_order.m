#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <dlfcn.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
int main(int argc,char **argv){@autoreleasepool{
 const char *mode=argc>1?argv[1]:"queued";
 id<MTLDevice>d=nil;
 const char *plugin=getenv("NM_TEST_PLUGIN");
 if(plugin){if(!dlopen(plugin,RTLD_NOW|RTLD_LOCAL)){fprintf(stderr,"%s\n",dlerror());return 3;}
  io_service_t svc=IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel"));
  if(!svc)return 4;
  d=((id(*)(id,SEL,mach_port_t))objc_msgSend)([NSClassFromString(@"NVMTLDevice") alloc],NSSelectorFromString(@"initWithAcceleratorPort:"),svc);
  IOObjectRelease(svc);
 }else d=MTLCreateSystemDefaultDevice();
 if(!d)return 5;id<MTLCommandQueue>q=[d newCommandQueue];unsigned bad=0;
 printf("device=%s\n",d.name.UTF8String);fflush(stdout);
 for(unsigned n=0;n<100;n++){@autoreleasepool{
 IOSurfaceRef s=IOSurfaceCreate((__bridge CFDictionaryRef)@{(id)kIOSurfaceWidth:@64,(id)kIOSurfaceHeight:@64,(id)kIOSurfaceBytesPerElement:@4,(id)kIOSurfacePixelFormat:@((uint32_t)'BGRA')});
 IOSurfaceLock(s,0,NULL);memset(IOSurfaceGetBaseAddress(s),0,IOSurfaceGetAllocSize(s));IOSurfaceUnlock(s,0,NULL);
 MTLTextureDescriptor *td=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:64 height:64 mipmapped:NO];td.storageMode=MTLStorageModeShared;td.usage=MTLTextureUsageRenderTarget|MTLTextureUsageShaderRead;
 id<MTLTexture>t=!strcmp(mode,"private") ? [d newTextureWithDescriptor:td] : [d newTextureWithDescriptor:td iosurface:s plane:0];if(!t)return 2;
 MTLRenderPassDescriptor*p=[MTLRenderPassDescriptor renderPassDescriptor];p.colorAttachments[0].texture=t;p.colorAttachments[0].loadAction=MTLLoadActionClear;p.colorAttachments[0].storeAction=MTLStoreActionStore;p.colorAttachments[0].clearColor=MTLClearColorMake(1,0.5,0,1);
 BOOL cross=!strcmp(mode,"cross");id<MTLSharedEvent>event=cross?[d newSharedEvent]:nil;if(cross&&!event)return 6;
 id<MTLCommandBuffer>a=[q commandBuffer];id<MTLRenderCommandEncoder>e=[a renderCommandEncoderWithDescriptor:p];[e endEncoding];if(cross)[a encodeSignalEvent:event value:1];
 BOOL same=!strcmp(mode,"same"), abandoned=!strcmp(mode,"abandoned"), cpu=!strcmp(mode,"cpu");
 if(!same&&!abandoned)[a commit];
 if(!strcmp(mode,"wait")||cpu)[a waitUntilCompleted];
 unsigned char expected[4]={0,128,255,255};
 if(abandoned)memset(expected,0,4);
 if(cpu){expected[0]=199;expected[1]=71;expected[2]=23;expected[3]=255;
  IOSurfaceLock(s,0,NULL);unsigned char *base=IOSurfaceGetBaseAddress(s);size_t row=IOSurfaceGetBytesPerRow(s);
  for(unsigned y=0;y<64;y++)for(unsigned x=0;x<64;x++)memcpy(base+y*row+x*4,expected,4);IOSurfaceUnlock(s,0,NULL);
 }
 if(!strcmp(mode,"alias_read"))td.usage=MTLTextureUsageShaderRead;
 id<MTLTexture>readTexture=(!strcmp(mode,"alias")||!strcmp(mode,"alias_read")||!strcmp(mode,"partial_alias"))?[d newTextureWithDescriptor:td iosurface:s plane:0]:t;
 id<MTLBuffer>b=[d newBufferWithLength:64*64*4 options:MTLResourceStorageModeShared];
 id<MTLCommandQueue>rq=cross?[d newCommandQueue]:q;
 id<MTLCommandBuffer>c=same?a:[rq commandBuffer];if(cross)[c encodeWaitForEvent:event value:1];id<MTLBlitCommandEncoder>bl=[c blitCommandEncoder];
 BOOL partial=!strcmp(mode,"partial")||!strcmp(mode,"partial_alias");
 if(partial){id<MTLBuffer>patch=[d newBufferWithLength:1024 options:MTLResourceStorageModeShared];unsigned char*data=patch.contents;
  for(unsigned i=0;i<256;i++){data[i*4]=255;data[i*4+1]=0;data[i*4+2]=0;data[i*4+3]=255;}
  [bl copyFromBuffer:patch sourceOffset:0 sourceBytesPerRow:64 sourceBytesPerImage:1024 sourceSize:MTLSizeMake(16,16,1) toTexture:readTexture destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(16,16,0)];
 }
 [bl copyFromTexture:readTexture sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0,0,0) sourceSize:MTLSizeMake(64,64,1) toBuffer:b destinationOffset:0 destinationBytesPerRow:256 destinationBytesPerImage:16384];[bl endEncoding];[c commit];[c waitUntilCompleted];if(!same&&!abandoned)[a waitUntilCompleted];
 unsigned char *px=b.contents;unsigned wrong=0;for(unsigned i=0;i<64*64;i++){unsigned char blue[4]={255,0,0,255};unsigned char *want=partial&&i%64>=16&&i%64<32&&i/64>=16&&i/64<32?blue:expected;
  if(abs(px[i*4]-want[0])>1||abs(px[i*4+1]-want[1])>1||abs(px[i*4+2]-want[2])>1||px[i*4+3]!=want[3])wrong++;}
 if(wrong||a.error||c.error){bad++;printf("FAIL frame=%u pixels=%u first=%u,%u,%u,%u mode=%s errors=%s/%s\n",n,wrong,px[0],px[1],px[2],px[3],mode,a.error.description.UTF8String,c.error.description.UTF8String);fflush(stdout);}
 CFRelease(s);
 }}printf("RESULT mode=%s frames=100 bad=%u\n",mode,bad);return bad?1:0;
}}
