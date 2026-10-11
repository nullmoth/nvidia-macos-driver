#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreImage/CoreImage.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <time.h>
#include <sys/resource.h>

static double now(void) { return (double)clock_gettime_nsec_np(CLOCK_MONOTONIC)/1e9; }
static double cpu_now(void) { struct rusage r; getrusage(RUSAGE_SELF,&r);return r.ru_utime.tv_sec+r.ru_stime.tv_sec+(r.ru_utime.tv_usec+r.ru_stime.tv_usec)/1e6; }
static void fail(NSString *s) { fprintf(stderr,"PROBE_ERROR: %s\n",s.UTF8String); exit(2); }
static void done(id<MTLCommandBuffer> c) {
    if (!c) fail(@"nil command buffer");
    [c commit]; [c waitUntilCompleted];
    if (c.status != MTLCommandBufferStatusCompleted) fail(c.error.description ?: @"command did not complete");
}
static double median(NSArray<NSNumber *> *a) {
    NSArray *s=[a sortedArrayUsingSelector:@selector(compare:)]; return [s[s.count/2] doubleValue];
}
static NSDictionary *transfer(id<MTLDevice> d,id<MTLCommandQueue> q,NSString *name,MTLResourceOptions aopt,MTLResourceOptions bopt) {
    const NSUInteger size=32*1024*1024;
    id<MTLBuffer> seed=[d newBufferWithLength:size options:MTLResourceStorageModeShared];
    id<MTLBuffer> a=[d newBufferWithLength:size options:aopt],b=[d newBufferWithLength:size options:bopt];
    id<MTLBuffer> readback=[d newBufferWithLength:size options:MTLResourceStorageModeShared];
    if(!seed||!a||!b||!readback) fail(@"buffer allocation failed");
    uint32_t *p=seed.contents;
    for(NSUInteger i=0;i<size/4;i++)p[i]=(uint32_t)i*1664525u+1013904223u;
    id<MTLCommandBuffer> c=[q commandBuffer];id<MTLBlitCommandEncoder> e=[c blitCommandEncoder];
    [e copyFromBuffer:seed sourceOffset:0 toBuffer:a destinationOffset:0 size:size];[e endEncoding];done(c);
    NSMutableArray *times=[NSMutableArray array];
    for(int i=0;i<9;i++){@autoreleasepool{
        double start=now();c=[q commandBuffer];e=[c blitCommandEncoder];
        [e copyFromBuffer:a sourceOffset:0 toBuffer:b destinationOffset:0 size:size];[e endEncoding];done(c);
        if(i>=2)[times addObject:@((now()-start)*1000)];
    }}
    c=[q commandBuffer];e=[c blitCommandEncoder];
    [e copyFromBuffer:b sourceOffset:0 toBuffer:readback destinationOffset:0 size:size];[e endEncoding];done(c);
    if(memcmp(seed.contents,readback.contents,size))fail([name stringByAppendingString:@" incorrect bytes"]);
    double ms=median(times);
    return @{@"test":name,@"bytes":@(size),@"median_ms":@(ms),@"GB_per_s":@((double)size/ms/1e6),@"samples_ms":times,@"verified":@YES};
}
int main(int argc,char **argv){@autoreleasepool{
    NSString *mode=argc>1?@(argv[1]):@"inventory";
    // Load the installed plugin first: release builds clear inherited tuning variables.
    // Overrides below exist only in this disposable probe, before device initialization.
    void *plugin=dlopen("/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver",RTLD_NOW|RTLD_GLOBAL);
    if(!plugin)fail(@(dlerror()));
    NSMutableDictionary *overrides=[NSMutableDictionary dictionary];
    for(int i=2;i<argc;i++){
        char *eq=strchr(argv[i],'=');if(!eq)fail(@"expected NAME=value");
        NSString *k=[[NSString alloc]initWithBytes:argv[i] length:(NSUInteger)(eq-argv[i]) encoding:NSUTF8StringEncoding];
        if(![k isEqualToString:@"NVRM_EVENT_WAIT"] || (strcmp(eq+1,"0") && strcmp(eq+1,"1")))fail(@"expected NVRM_EVENT_WAIT=0 or NVRM_EVENT_WAIT=1");
        setenv(k.UTF8String,eq+1,1);overrides[k]=@(eq+1);
    }
    double init=now();id<MTLDevice> d=MTLCreateSystemDefaultDevice();
    if(!d)fail(@"no Metal device");
    NSMutableDictionary *result=[@{@"device":d.name,@"initialization_ms":@((now()-init)*1000),@"mode":mode,@"overrides":overrides,@"recommended_working_set_bytes":@(d.recommendedMaxWorkingSetSize)} mutableCopy];
    result[@"plugin_image"]=@(class_getImageName(object_getClass(d)));
    if(![result[@"plugin_image"] isEqualToString:@"/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver"])
        fail(@"the default Metal device is not the installed NVMTL device");
    result[@"probe_version"]=@2;
    if([mode isEqual:@"inventory"]){
        CGDirectDisplayID display=CGMainDisplayID();CGDisplayModeRef current=CGDisplayCopyDisplayMode(display);
        result[@"display"]=@{@"width":@(CGDisplayModeGetWidth(current)),@"height":@(CGDisplayModeGetHeight(current)),@"pixel_width":@(CGDisplayModeGetPixelWidth(current)),@"pixel_height":@(CGDisplayModeGetPixelHeight(current)),@"hz":@(CGDisplayModeGetRefreshRate(current))};CGDisplayModeRelease(current);
        NSArray *modes=CFBridgingRelease(CGDisplayCopyAllDisplayModes(display,(__bridge CFDictionaryRef)@{(__bridge NSString*)kCGDisplayShowDuplicateLowResolutionModes:@YES}));
        NSMutableArray *hidpi=[NSMutableArray array];for(id obj in modes){CGDisplayModeRef m=(__bridge CGDisplayModeRef)obj;
            if(CGDisplayModeGetPixelWidth(m)>CGDisplayModeGetWidth(m))[hidpi addObject:@{@"width":@(CGDisplayModeGetWidth(m)),@"height":@(CGDisplayModeGetHeight(m)),@"pixel_width":@(CGDisplayModeGetPixelWidth(m)),@"pixel_height":@(CGDisplayModeGetPixelHeight(m)),@"hz":@(CGDisplayModeGetRefreshRate(m))}];}
        result[@"hidpi_modes"]=hidpi;result[@"display_mode_count"]=@(modes.count);
    }else if([mode isEqual:@"transfer"]){
        id<MTLCommandQueue> q=[d newCommandQueue];if(!q)fail(@"no queue");
        result[@"tests"]=@[
            transfer(d,q,@"shared_to_private",MTLResourceStorageModeShared,MTLResourceStorageModePrivate),
            transfer(d,q,@"private_to_shared",MTLResourceStorageModePrivate,MTLResourceStorageModeShared),
            transfer(d,q,@"private_to_private",MTLResourceStorageModePrivate,MTLResourceStorageModePrivate)];
    }else if([mode isEqual:@"events"]){
        const NSUInteger size=4*1024*1024;
        id<MTLCommandQueue> producer=[d newCommandQueue],consumer=[d newCommandQueue];
        id<MTLEvent> event=[d newEvent];
        id<MTLBuffer> a=[d newBufferWithLength:size options:MTLResourceStorageModePrivate];
        id<MTLBuffer> b=[d newBufferWithLength:size options:MTLResourceStorageModeShared];
        if(!producer||!consumer||!event||!a||!b)fail(@"event test allocation failed");
        for(unsigned i=1;i<=32;i++){@autoreleasepool{
            id<MTLCommandBuffer> read=[consumer commandBuffer];[read encodeWaitForEvent:event value:i];
            id<MTLBlitCommandEncoder> e=[read blitCommandEncoder];
            [e copyFromBuffer:a sourceOffset:0 toBuffer:b destinationOffset:0 size:size];[e endEncoding];
            [read commit];
            id<MTLCommandBuffer> write=[producer commandBuffer];e=[write blitCommandEncoder];
            [e fillBuffer:a range:NSMakeRange(0,size) value:(uint8_t)i];[e endEncoding];
            [write encodeSignalEvent:event value:i];done(write);[read waitUntilCompleted];
            if(read.status!=MTLCommandBufferStatusCompleted)fail(read.error.description?:@"cross-queue wait failed");
            const uint8_t *p=b.contents;for(NSUInteger j=0;j<size;j++)if(p[j]!=(uint8_t)i)fail(@"cross-queue data mismatch");
        }}
        result[@"iterations"]=@32;result[@"verified_bytes"]=@(32*size);result[@"verified"]=@YES;
    }else if([mode isEqual:@"compute"]){
        NSError *err=nil;double start=now();
        NSString *source=@"#include <metal_stdlib>\nusing namespace metal;\nkernel void mix(device const uint *a [[buffer(0)]], device uint *b [[buffer(1)]], uint i [[thread_position_in_grid]]) { uint v=a[i]; for(uint j=0;j<64;j++){v^=v>>13;v=v*1664525u+1013904223u;} b[i]=v;}";
        id<MTLLibrary> l=[d newLibraryWithSource:source options:nil error:&err];if(!l)fail(err.description?:@"no library");
        id<MTLComputePipelineState> pipeline=[d newComputePipelineStateWithFunction:[l newFunctionWithName:@"mix"] error:&err];if(!pipeline)fail(err.description?:@"no pipeline");
        result[@"compile_ms"]=@((now()-start)*1000);
        const NSUInteger n=1024*1024;id<MTLBuffer>a=[d newBufferWithLength:n*4 options:MTLResourceStorageModeShared],b=[d newBufferWithLength:n*4 options:MTLResourceStorageModeShared];
        if(!a||!b)fail(@"compute allocation failed");uint32_t *p=a.contents;for(NSUInteger i=0;i<n;i++)p[i]=(uint32_t)i;
        id<MTLCommandQueue>q=[d newCommandQueue];NSMutableArray *times=[NSMutableArray array],*cpuTimes=[NSMutableArray array];
        for(int k=0;k<27;k++){@autoreleasepool{start=now();double cpuStart=cpu_now();id<MTLCommandBuffer>c=[q commandBuffer];id<MTLComputeCommandEncoder>e=[c computeCommandEncoder];
            [e setComputePipelineState:pipeline];[e setBuffer:a offset:0 atIndex:0];[e setBuffer:b offset:0 atIndex:1];
            NSUInteger group=MIN((NSUInteger)256,pipeline.maxTotalThreadsPerThreadgroup);
            if(!group)fail(@"invalid thread group size");
            [e dispatchThreads:MTLSizeMake(n,1,1) threadsPerThreadgroup:MTLSizeMake(group,1,1)];[e endEncoding];done(c);
            if(k>=2){[times addObject:@((now()-start)*1000)];[cpuTimes addObject:@((cpu_now()-cpuStart)*1000)];}}}
        uint32_t *out=b.contents;for(NSUInteger i=0;i<n;i++){uint32_t v=(uint32_t)i;for(int j=0;j<64;j++){v^=v>>13;v=v*1664525u+1013904223u;}if(out[i]!=v)fail([NSString stringWithFormat:@"compute mismatch at %lu",(unsigned long)i]);}
        result[@"median_ms"]=@(median(times));result[@"samples_ms"]=times;result[@"verified"]=@YES;
        result[@"median_cpu_ms"]=@(median(cpuTimes));result[@"samples_cpu_ms"]=cpuTimes;
    }else if([mode isEqual:@"image"]){
        const NSUInteger w=1024,h=1024;NSMutableData *input=[NSMutableData dataWithLength:w*h*4],*output=[NSMutableData dataWithLength:w*h*4];
        uint8_t*p=input.mutableBytes;for(NSUInteger y=0;y<h;y++)for(NSUInteger x=0;x<w;x++){NSUInteger o=(y*w+x)*4;p[o]=x%256;p[o+1]=y%256;p[o+2]=(x+y)%256;p[o+3]=255;}
        CGColorSpaceRef cs=CGColorSpaceCreateDeviceRGB();
        CIImage *im=[CIImage imageWithBitmapData:input bytesPerRow:w*4 size:CGSizeMake(w,h) format:kCIFormatRGBA8 colorSpace:cs];
        im=[[[im imageByClampingToExtent] imageByApplyingFilter:@"CIGaussianBlur" withInputParameters:@{kCIInputRadiusKey:@10}] imageByCroppingToRect:CGRectMake(0,0,w,h)];
        CIContext *ctx=[CIContext contextWithMTLDevice:d options:@{kCIContextCacheIntermediates:@NO}];
        NSMutableArray *times=[NSMutableArray array],*cpuTimes=[NSMutableArray array];for(int k=0;k<27;k++){@autoreleasepool{double start=now(),cpuStart=cpu_now();[ctx render:im toBitmap:output.mutableBytes rowBytes:w*4 bounds:CGRectMake(0,0,w,h) format:kCIFormatRGBA8 colorSpace:cs];if(k>=2){[times addObject:@((now()-start)*1000)];[cpuTimes addObject:@((cpu_now()-cpuStart)*1000)];}}}
        uint8_t*out=output.mutableBytes;uint64_t hash=1469598103934665603ull;for(NSUInteger i=0;i<output.length;i++){hash^=out[i];hash*=1099511628211ull;}
        NSUInteger alphaErrors=0;for(NSUInteger i=3;i<output.length;i+=4)if(out[i]!=255)alphaErrors++;
        if(alphaErrors)fail(@"image alpha mismatch");
        result[@"median_ms"]=@(median(times));result[@"samples_ms"]=times;result[@"pixel_hash"]=@(hash);result[@"alpha_verified"]=@YES;
        result[@"median_cpu_ms"]=@(median(cpuTimes));result[@"samples_cpu_ms"]=cpuTimes;
        CGColorSpaceRelease(cs);
    }else fail(@"unknown probe mode");
    NSError *jsonError=nil;NSData*json=[NSJSONSerialization dataWithJSONObject:result options:NSJSONWritingPrettyPrinted|NSJSONWritingSortedKeys error:&jsonError];
    if(!json)fail(jsonError.description);fwrite(json.bytes,1,json.length,stdout);fputc('\n',stdout);
}}
