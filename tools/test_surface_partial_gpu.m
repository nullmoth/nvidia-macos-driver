// GPU regression for partial IOSurface wrappers and 1D texture views.
// Build with Foundation, Metal, IOSurface and IOKit; pass a private plugin executable.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static BOOL imported(id texture) {
    Ivar ivar=class_getInstanceVariable([texture class],"_vramOn");
    return ivar && *(BOOL *)((uint8_t *)(__bridge void *)texture+ivar_getOffset(ivar));
}
static unsigned check(const uint8_t *pixels,unsigned w,unsigned h,unsigned pw,unsigned ph,
                      uint8_t inside,uint8_t outside) {
    unsigned bad=0;
    for(unsigned y=0;y<h;y++) for(unsigned x=0;x<w;x++) for(unsigned c=0;c<4;c++)
        bad+=pixels[(y*w+x)*4+c]!=(x<pw && y<ph?inside:outside);
    return bad;
}
static unsigned partial(id<MTLDevice> device,BOOL shorter,MTLStorageMode mode) { @autoreleasepool {
    const unsigned w=64,h=32,pw=shorter?w:w/2,ph=shorter?h/2:h;
    IOSurfaceRef surface=IOSurfaceCreate((__bridge CFDictionaryRef)@{
        (id)kIOSurfaceWidth:@(w),(id)kIOSurfaceHeight:@(h),(id)kIOSurfaceBytesPerElement:@4,
        (id)kIOSurfaceBytesPerRow:@(w*4),(id)kIOSurfaceAllocSize:@(w*h*4),
        (id)kIOSurfacePixelFormat:@(0x42475241)});
    if(!surface || IOSurfaceLock(surface,0,NULL)) return 1;
    memset(IOSurfaceGetBaseAddress(surface),0x31,w*h*4);IOSurfaceUnlock(surface,0,NULL);
    MTLTextureDescriptor *desc=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
        width:w height:h mipmapped:NO];
    desc.usage=MTLTextureUsageShaderRead;desc.storageMode=mode;
    id<MTLTexture> whole=[device newTextureWithDescriptor:desc iosurface:surface plane:0];
    if(!whole || !imported(whole)) { CFRelease(surface);return 1; }
    uint8_t pixels[w*h*4];
    [whole getBytes:pixels bytesPerRow:w*4 fromRegion:MTLRegionMake2D(0,0,w,h) mipmapLevel:0];
    unsigned bad=check(pixels,w,h,w,h,0x31,0x31);
    if(IOSurfaceLock(surface,0,NULL)) { CFRelease(surface);return 1; }
    memset(IOSurfaceGetBaseAddress(surface),0x52,w*h*4);IOSurfaceUnlock(surface,0,NULL);
    desc.width=pw;desc.height=ph;
    id<MTLTexture> piece=[device newTextureWithDescriptor:desc iosurface:surface plane:0];
    if(!piece) { CFRelease(surface);return 1; }
    // Reading the piece must not validate the untouched rest of family VRAM.
    [piece getBytes:pixels bytesPerRow:pw*4 fromRegion:MTLRegionMake2D(0,0,pw,ph) mipmapLevel:0];
    bad+=check(pixels,pw,ph,pw,ph,0x52,0x52);
    [whole getBytes:pixels bytesPerRow:w*4 fromRegion:MTLRegionMake2D(0,0,w,h) mipmapLevel:0];
    bad+=check(pixels,w,h,w,h,0x52,0x52);
    memset(pixels,0x73,pw*ph*4);
    [piece replaceRegion:MTLRegionMake2D(0,0,pw,ph) mipmapLevel:0 withBytes:pixels bytesPerRow:pw*4];
    [whole getBytes:pixels bytesPerRow:w*4 fromRegion:MTLRegionMake2D(0,0,w,h) mipmapLevel:0];
    bad+=check(pixels,w,h,pw,ph,0x73,0x52);
    if(IOSurfaceLock(surface,kIOSurfaceLockReadOnly,NULL)) { CFRelease(surface);return 1; }
    bad+=check(IOSurfaceGetBaseAddress(surface),w,h,pw,ph,0x73,0x52);
    IOSurfaceUnlock(surface,kIOSurfaceLockReadOnly,NULL);
    printf("partial=%s managed=%u imported=%u errors=%u\n",shorter?"height":"width",
           mode==MTLStorageModeManaged,imported(piece),bad);
    CFRelease(surface);return bad;
} }
static unsigned views(id<MTLDevice> device) { @autoreleasepool {
    MTLTextureDescriptor *desc=[MTLTextureDescriptor new];
    desc.textureType=MTLTextureType1DArray;desc.width=64;desc.height=1;desc.arrayLength=3;
    desc.pixelFormat=MTLPixelFormatRGBA8Unorm;desc.storageMode=MTLStorageModeShared;
    desc.usage=MTLTextureUsageShaderRead|MTLTextureUsagePixelFormatView;
    id<MTLTexture> array=[device newTextureWithDescriptor:desc];if(!array) return 1;
    uint8_t pixels[256];
    for(unsigned slice=0;slice<3;slice++) {
        memset(pixels,0x31+slice,sizeof pixels);
        [array replaceRegion:MTLRegionMake1D(0,64) mipmapLevel:0 slice:slice withBytes:pixels
                 bytesPerRow:sizeof pixels bytesPerImage:sizeof pixels];
    }
    unsigned bad=0;
    for(unsigned slice=0;slice<3;slice++) {
        id<MTLTexture> view=[array newTextureViewWithPixelFormat:desc.pixelFormat textureType:MTLTextureType1D
            levels:NSMakeRange(0,1) slices:NSMakeRange(slice,1)];
        if(!view) { bad++;continue; }
        bad+=view.textureType!=MTLTextureType1D || view.arrayLength!=1;
        memset(pixels,0,sizeof pixels);[view getBytes:pixels bytesPerRow:sizeof pixels fromRegion:MTLRegionMake1D(0,64) mipmapLevel:0];
        for(unsigned i=0;i<sizeof pixels;i++) bad+=pixels[i]!=0x31+slice;
        id<MTLTexture> back=[view newTextureViewWithPixelFormat:desc.pixelFormat textureType:MTLTextureType1DArray
            levels:NSMakeRange(0,1) slices:NSMakeRange(0,1)];
        if(!back) { bad++;continue; }
        bad+=back.arrayLength!=1 || back.textureType!=MTLTextureType1DArray;
        memset(pixels,0,sizeof pixels);[back getBytes:pixels bytesPerRow:sizeof pixels fromRegion:MTLRegionMake1D(0,64) mipmapLevel:0];
        for(unsigned i=0;i<sizeof pixels;i++) bad+=pixels[i]!=0x31+slice;
    }
    // Invalid slices and dimensional conversions still refuse.
    bad+=[array newTextureViewWithPixelFormat:desc.pixelFormat textureType:MTLTextureType1D levels:NSMakeRange(0,1) slices:NSMakeRange(0,2)]!=nil;
    bad+=[array newTextureViewWithPixelFormat:desc.pixelFormat textureType:MTLTextureType1D levels:NSMakeRange(0,1) slices:NSMakeRange(3,1)]!=nil;
    desc.textureType=MTLTextureType2D;desc.height=2;desc.arrayLength=1;
    id<MTLTexture> two=[device newTextureWithDescriptor:desc];
    bad+=!two || [two newTextureViewWithPixelFormat:desc.pixelFormat textureType:MTLTextureType1D levels:NSMakeRange(0,1) slices:NSMakeRange(0,1)]!=nil;
    printf("1D view roundtrips and refusals errors=%u\n",bad);return bad;
} }
int main(int argc,char **argv) { @autoreleasepool {
    alarm(90);setvbuf(stdout,NULL,_IONBF,0);
    if(argc!=2 || !dlopen(argv[1],RTLD_NOW|RTLD_LOCAL)) { fprintf(stderr,"plugin: %s\n",dlerror());return 2; }
    io_service_t accelerator=IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel"));
    if(!accelerator) return 2;
    Class cls=NSClassFromString(@"NVMTLDevice");SEL init=NSSelectorFromString(@"initWithAcceleratorPort:");
    id<MTLDevice> device=((id(*)(id,SEL,mach_port_t))objc_msgSend)([cls alloc],init,accelerator);
    IOObjectRelease(accelerator);if(!device) return 2;
    unsigned bad=views(device);
    for(unsigned managed=0;managed<2;managed++) for(unsigned shorter=0;shorter<2;shorter++)
        bad+=partial(device,shorter,managed?MTLStorageModeManaged:MTLStorageModeShared);
    printf("%s: partial surface coherence and 1D views, errors=%u\n",bad?"FAIL":"PASS",bad);return bad?1:0;
} }
