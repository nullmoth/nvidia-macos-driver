// Real-GPU copy-back regression: pass the exact driver image and a fresh output directory.
// clang -fobjc-arc -O1 -framework Foundation -framework Metal -framework IOKit test_readonly_copyback_gpu.m -o probe
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <mach-o/dyld.h>
#include <stdatomic.h>
#include <unistd.h>

@protocol ProbeDevice <MTLDevice>
- (instancetype)initWithAcceleratorPort:(mach_port_t)port;
@end
static void require(BOOL ok, NSString *why) {
    if (!ok) { fprintf(stderr,"FAIL: %s\n",why.UTF8String); exit(1); }
}
static void landed(id<MTLCommandBuffer> cb) {
    [cb commit]; [cb waitUntilCompleted];
    require(cb.status==MTLCommandBufferStatusCompleted && !cb.error,cb.error.description ?: @"command failed");
}
static void bytesAre(const void *memory, size_t length, uint8_t expected) {
    const uint8_t *p=memory;
    for(size_t i=0;i<length;i++) if(p[i]!=expected)
        require(NO,[NSString stringWithFormat:@"byte %zu: expected %u got %u",i,expected,p[i]]);
}
static _Atomic unsigned callbacks;
static int runProbe(int argc,char **argv) { @autoreleasepool {
    setbuf(stdout,NULL);setbuf(stderr,NULL);alarm(120);
    require(argc==3 || argc==4,@"usage: probe driver cache-dir [split-events]");
    NSString *dir=@(argv[2]);
    [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:NULL];
    setenv("NVMTL_PROCESS_ENV","1",1);setenv("NVMTL_PERF_LOG","1",1);
    setenv("NVMTL_LOG_PATH",[dir stringByAppendingPathComponent:@"driver.log"].fileSystemRepresentation,1);
    setenv("NVMTL_AIRCACHE",[dir stringByAppendingPathComponent:@"aircache"].fileSystemRepresentation,1);
    const BOOL split=argc==4;
    if(split) setenv("NVMTL_G79_OFF","ev",1);
    require(objc_lookUpClass("NVMTLDevice")==Nil,@"driver already loaded");
    void *library=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL);require(library!=NULL,@"dlopen selected driver");
    Class cls=objc_lookUpClass("NVMTLDevice");
    io_service_t svc=IOServiceGetMatchingService(kIOMainPortDefault,IOServiceMatching("NVAccel"));
    require(svc!=IO_OBJECT_NULL,@"NVAccel service");
    id<ProbeDevice> dev=[[cls alloc] initWithAcceleratorPort:svc];IOObjectRelease(svc);
    require(dev!=nil,@"device");
    unsigned drivers=0;
    for(uint32_t i=0;i<_dyld_image_count();i++) if(strstr(_dyld_get_image_name(i),"/Contents/MacOS/NVMTLDriver")) drivers++;
    require(drivers==1,@"one selected driver image");
    const size_t n=4u<<20;uint8_t *allocation=malloc(n+64);
    require(allocation!=NULL,@"caller allocation");uint8_t *caller=allocation+1;
    id<MTLBuffer> source=[dev newBufferWithBytesNoCopy:caller length:n options:MTLResourceStorageModeShared
        deallocator:^(void *pointer,NSUInteger length){
            require(pointer==caller && length==n,@"original NoCopy callback arguments");
            atomic_fetch_add(&callbacks,1);free(allocation);
        }];
    require(source!=nil && source.contents==caller,@"NoCopy contents must be caller pages");
    Ivar host=class_getInstanceVariable([source class],"_hostPtr");
    require(host!=NULL && *(void **)((char *)(__bridge void *)source+ivar_getOffset(host))==caller,
            @"must exercise the unimportable caller-page fallback, not a zero-copy import");
    id<MTLCommandQueue> q=[dev newCommandQueue];
    id<MTLBuffer> vram=[dev newBufferWithLength:n options:MTLResourceStorageModePrivate];
    id<MTLBuffer> readback=[dev newBufferWithLength:n options:MTLResourceStorageModeShared];
    require(q && vram && readback,@"working buffers");
    id<MTLEvent> event=split?[dev newEvent]:nil;
    NSMutableArray *times=[NSMutableArray new];
    fprintf(stderr,"PHASE readonly-buffer\n");
    for(unsigned round=0;round<64;round++) { @autoreleasepool {
        uint8_t value=(uint8_t)(round*3+1);memset(caller,value,n);
        CFAbsoluteTime start=CFAbsoluteTimeGetCurrent();
        id<MTLCommandBuffer> cb=[q commandBuffer];
        id<MTLBlitCommandEncoder> blit=[cb blitCommandEncoder];
        [blit copyFromBuffer:source sourceOffset:0 toBuffer:vram destinationOffset:0 size:n];
        if(split) {
            [blit endEncoding];[cb encodeSignalEvent:event value:round+1];[cb encodeWaitForEvent:event value:round+1];
            blit=[cb blitCommandEncoder];
        }
        [blit copyFromBuffer:vram sourceOffset:0 toBuffer:readback destinationOffset:0 size:n];
        [blit endEncoding];landed(cb);
        [times addObject:@((CFAbsoluteTimeGetCurrent()-start)*1000)];
        bytesAre(readback.contents,n,value);bytesAre(caller,n,value);
    } }
    fprintf(stderr,"PHASE readonly-texture\n");
    MTLTextureDescriptor *td=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:1024 height:1024 mipmapped:NO];
    td.storageMode=MTLStorageModeManaged;td.usage=MTLTextureUsageShaderRead;
    id<MTLTexture> texture=[dev newTextureWithDescriptor:td];require(texture!=nil,@"texture");
    uint8_t *pixels=malloc(n);require(pixels!=NULL,@"pixel allocation");
    for(unsigned round=0;round<8;round++) { @autoreleasepool {
        uint8_t value=(uint8_t)(129+round);memset(caller,value,n);
        id<MTLCommandBuffer> cb=[q commandBuffer];id<MTLBlitCommandEncoder> blit=[cb blitCommandEncoder];
        [blit copyFromBuffer:source sourceOffset:0 sourceBytesPerRow:4096 sourceBytesPerImage:n sourceSize:MTLSizeMake(1024,1024,1)
                  toTexture:texture destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0,0,0)];
        [blit synchronizeResource:texture];[blit endEncoding];landed(cb);
        [texture getBytes:pixels bytesPerRow:4096 fromRegion:MTLRegionMake2D(0,0,1024,1024) mipmapLevel:0];
        bytesAre(pixels,n,value);
    } }
    free(pixels);
    fprintf(stderr,"PHASE write-upgrades\n");
    for(unsigned round=0;round<8;round++) { @autoreleasepool {
        uint8_t first=(uint8_t)(11+round),second=(uint8_t)(201+round);memset(caller,first,n);
        id<MTLCommandBuffer> cb=[q commandBuffer];id<MTLBlitCommandEncoder> blit=[cb blitCommandEncoder];
        [blit copyFromBuffer:source sourceOffset:0 toBuffer:vram destinationOffset:0 size:n];[blit endEncoding];
        if(split) { [cb encodeSignalEvent:event value:128+round];[cb encodeWaitForEvent:event value:128+round]; }
        blit=[cb blitCommandEncoder];[blit fillBuffer:source range:NSMakeRange(0,n) value:second];
        [blit copyFromBuffer:vram sourceOffset:0 toBuffer:readback destinationOffset:0 size:n];[blit endEncoding];
        landed(cb);bytesAre(caller,n,second);bytesAre(readback.contents,n,first);
    } }
    // Flush the final accounting record without adding another caller-backed copy.
    for(unsigned i=0;i<2;i++) { @autoreleasepool {
        id<MTLCommandBuffer> cb=[q commandBuffer];id<MTLBlitCommandEncoder> blit=[cb blitCommandEncoder];
        [blit fillBuffer:readback range:NSMakeRange(0,4) value:0];[blit endEncoding];landed(cb);
    } }
    source=nil;
    NSArray *sorted=[times sortedArrayUsingSelector:@selector(compare:)];
    NSDictionary *result=@{@"readonlyBufferSubmissions":@64,@"textureSubmissions":@8,@"writeUpgradeSubmissions":@8,
        @"bytesChecked":@(n*(128+8+16)),@"medianSubmitMs":sorted[sorted.count/2],@"splitEvents":@(split)};
    NSData *json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted error:NULL];
    require([json writeToFile:[dir stringByAppendingPathComponent:@"results.json"] atomically:YES],@"write result");
    printf("PASS: %s\n",[[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding].UTF8String);
    return 0;
} }

int main(int argc,char **argv) {
    int result=runProbe(argc,argv);
    // Completed command buffers may themselves be autoreleased; check after the explicit owner pool drains.
    require(atomic_load(&callbacks)==1,@"caller storage released exactly once after completed GPU work and owner-pool drain");
    @autoreleasepool {
        NSString *path=[@(argv[2]) stringByAppendingPathComponent:@"results.json"];
        NSMutableDictionary *data=[[NSJSONSerialization JSONObjectWithData:[NSData dataWithContentsOfFile:path] options:0 error:NULL] mutableCopy];
        data[@"callbacks"]=@(atomic_load(&callbacks));
        [[NSJSONSerialization dataWithJSONObject:data options:NSJSONWritingPrettyPrinted error:NULL] writeToFile:path atomically:YES];
        puts("PASS: caller storage released once after completed work and owner-pool drain");
    }
    return result;
}
