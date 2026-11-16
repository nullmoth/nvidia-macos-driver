// GPU regression for Unknown-usage sRGB IOSurfaces with writable UNORM views.
// Build with Foundation, Metal, IOSurface, IOKit and Vulkan headers.
// Pass one private NVMTLDriver executable; this does not install it.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <vulkan/vulkan_core.h>
#include "../plugin/nvmtl_vk.h"

@interface NSObject (SRGBSurfaceProbe)
- (nvk_image *)nvi;
@end

static BOOL imported(id texture) {
    Ivar ivar=class_getInstanceVariable([texture class],"_vramOn");
    return ivar && *(BOOL *)((uint8_t *)(__bridge void *)texture+ivar_getOffset(ivar));
}
static BOOL tiled(id texture) {
    Ivar ivar=class_getInstanceVariable([texture class],"_surfaceLinear");
    return ivar && ((nvk_image *)((uint8_t *)(__bridge void *)texture+ivar_getOffset(ivar)))->img;
}
static unsigned verify(const void *base, size_t row, unsigned w, unsigned h, unsigned frame, BOOL bgra) {
    unsigned bad=0;
    const uint8_t expected[]={frame==1?64:128,128,frame==1?191:64,255};
    for (unsigned y=0;y<h;y++) for (unsigned x=0;x<w;x++) for (unsigned c=0;c<4;c++) {
        unsigned channel=bgra && c!=1 && c!=3?2-c:c;
        bad+=abs(((const uint8_t *)base)[y*row+4*x+c]-expected[channel])>1;
    }
    return bad;
}
int main(int argc,char **argv) { @autoreleasepool {
    alarm(90); setvbuf(stdout,NULL,_IONBF,0);
    if (argc!=2 || !dlopen(argv[1],RTLD_NOW|RTLD_LOCAL)) { fprintf(stderr,"plugin: %s\n",dlerror()); return 2; }
    io_service_t accelerator=IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel"));
    if (!accelerator) return 2;
    Class cls=NSClassFromString(@"NVMTLDevice"); SEL init=NSSelectorFromString(@"initWithAcceleratorPort:");
    id<MTLDevice> device=((id(*)(id,SEL,mach_port_t))objc_msgSend)([cls alloc],init,accelerator);
    // A separate device wrapper has its own surface cache: it must see the
    // published family image, rather than reusing the writer's tiled image.
    id<MTLDevice> reader=((id(*)(id,SEL,mach_port_t))objc_msgSend)([cls alloc],init,accelerator);
    IOObjectRelease(accelerator); if (!device || !reader) return 2;
    NSError *error=nil;
    id<MTLLibrary> library=[device newLibraryWithSource:@"#include <metal_stdlib>\nusing namespace metal;\n"
        "kernel void fill(texture2d<float,access::write> t [[texture(0)]], constant uint &frame [[buffer(0)]],"
        " uint2 p [[thread_position_in_grid]]) { t.write(frame==1?float4(0.25,0.5,0.75,1):float4(0.5,0.5,0.25,1),p); }"
        options:nil error:&error];
    id<MTLComputePipelineState> pipeline=library?[device newComputePipelineStateWithFunction:[library newFunctionWithName:@"fill"] error:&error]:nil;
    if (!pipeline) { fprintf(stderr,"pipeline: %s\n",error.description.UTF8String); return 2; }
    id<MTLCommandQueue> queue=[device newCommandQueue];
    unsigned failures=0,cases=0;
    for (unsigned bgra=0;bgra<2;bgra++) for (unsigned managed=0;managed<2;managed++)
    for (unsigned padded=0;padded<2;padded++) { @autoreleasepool {
        const unsigned w=padded?52:128,h=32; const size_t row=(w*4+63)&~63u;
        IOSurfaceRef surface=IOSurfaceCreate((__bridge CFDictionaryRef)@{(id)kIOSurfaceWidth:@(w),(id)kIOSurfaceHeight:@(h),
            (id)kIOSurfaceBytesPerElement:@4,(id)kIOSurfaceBytesPerRow:@(row),(id)kIOSurfaceAllocSize:@(row*h),
            (id)kIOSurfacePixelFormat:@(bgra?0x42475241:0x52474241)});
        if (!surface || IOSurfaceLock(surface,0,NULL)) return 2;
        memset(IOSurfaceGetBaseAddress(surface),0x22,row*h); IOSurfaceUnlock(surface,0,NULL);
        MTLTextureDescriptor *desc=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:bgra?MTLPixelFormatBGRA8Unorm_sRGB:MTLPixelFormatRGBA8Unorm_sRGB
            width:w height:h mipmapped:NO];
        desc.storageMode=managed?MTLStorageModeManaged:MTLStorageModeShared; desc.usage=MTLTextureUsageUnknown;
        id<MTLTexture> target=[device newTextureWithDescriptor:desc iosurface:surface plane:0];
        if (!target) return 2;
        const nvk_image *backing=[(NSObject *)target nvi];
        if (!imported(target) || !tiled(target) || backing->storage!=2) {
            fprintf(stderr,"required sRGB bridge absent: format=%u managed=%u imported=%u tiled=%u storage=%d\n",
                bgra,managed,imported(target),tiled(target),backing->storage); return 1;
        }
        unsigned bad=(target.usage&MTLTextureUsageShaderWrite)!=0;
        id<MTLTexture> writable=[target newTextureViewWithPixelFormat:bgra?MTLPixelFormatBGRA8Unorm:MTLPixelFormatRGBA8Unorm];
        if (!writable) return 2;
        desc.storageMode=MTLStorageModeShared; desc.usage=MTLTextureUsageShaderRead;
        id<MTLTexture> observer=[reader newTextureWithDescriptor:desc iosurface:surface plane:0];
        if (!observer || !imported(observer) || tiled(observer)) return 2;
        NSMutableData *pixels=[NSMutableData dataWithLength:row*h];
        for (unsigned frame=1;frame<=2;frame++) {
            id<MTLCommandBuffer> command=[queue commandBuffer];
            id<MTLComputeCommandEncoder> encoder=[command computeCommandEncoder];
            [encoder setComputePipelineState:pipeline]; [encoder setTexture:writable atIndex:0];
            [encoder setBytes:&frame length:sizeof frame atIndex:0];
            [encoder dispatchThreads:MTLSizeMake(w,h,1) threadsPerThreadgroup:MTLSizeMake(4,4,1)];
            [encoder endEncoding]; [command commit]; [command waitUntilCompleted];
            if (command.status!=MTLCommandBufferStatusCompleted) { fprintf(stderr,"command: %s\n",command.error.description.UTF8String); return 2; }
            [observer getBytes:pixels.mutableBytes bytesPerRow:row fromRegion:MTLRegionMake2D(0,0,w,h) mipmapLevel:0];
            bad+=verify(pixels.bytes,row,w,h,frame,bgra);
            if (IOSurfaceLock(surface,kIOSurfaceLockReadOnly,NULL)) return 2;
            bad+=verify(IOSurfaceGetBaseAddress(surface),row,w,h,frame,bgra);
            IOSurfaceUnlock(surface,kIOSurfaceLockReadOnly,NULL);
            // A real CPU edit must still invalidate the tiled GPU contents.
            if (IOSurfaceLock(surface,0,NULL)) return 2;
            memset(IOSurfaceGetBaseAddress(surface),0x47,row*h); IOSurfaceUnlock(surface,0,NULL);
            [target getBytes:pixels.mutableBytes bytesPerRow:row fromRegion:MTLRegionMake2D(0,0,w,h) mipmapLevel:0];
            for (unsigned y=0;y<h;y++) for (unsigned x=0;x<w*4;x++) bad+=((const uint8_t *)pixels.bytes)[y*row+x]!=0x47;
        }
        printf("format=%s managed=%u padded=%u storage=%d frames=2 errors=%u\n",bgra?"BGRA8_sRGB":"RGBA8_sRGB",managed,padded,backing->storage,bad);
        failures+=bad!=0; cases++; CFRelease(surface);
    }}
    printf("%s: %u sRGB bridge cases, %u failures\n",failures?"FAIL":"PASS",cases,failures);
    return failures?1:0;
}}
