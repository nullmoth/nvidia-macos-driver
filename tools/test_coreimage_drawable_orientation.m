// GPU regression: Core Image chooses screen coordinates for real drawables.
// Build with Foundation, CoreImage, CoreVideo, CoreGraphics, Metal, IOSurface and QuartzCore.
// The four-color test image contains no camera data.
#import <Foundation/Foundation.h>
#import <CoreImage/CoreImage.h>
#import <CoreVideo/CoreVideo.h>
#import <QuartzCore/CAMetalLayer.h>
#import <Metal/Metal.h>
#import <objc/message.h>
#include <unistd.h>

static BOOL drawableFlag(id texture) {
    SEL selector = sel_registerName("isDrawable");
    return [texture respondsToSelector:selector] && ((BOOL (*)(id, SEL))objc_msgSend)(texture, selector);
}

int main(void) {
    alarm(90);
    @autoreleasepool {
        const NSUInteger width = 80, height = 48;
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 2;
        id<MTLCommandQueue> queue = [device newCommandQueue];
        NSDictionary *options = @{kCIContextWorkingColorSpace: (id)kCFNull,
            kCIContextOutputColorSpace: (id)kCFNull, kCIContextCacheIntermediates: @NO};
        CIContext *gpu = [CIContext contextWithMTLCommandQueue:queue options:options];
        NSMutableDictionary *softwareOptions = [options mutableCopy];
        softwareOptions[kCIContextUseSoftwareRenderer] = @YES;
        CIContext *cpu = [CIContext contextWithOptions:softwareOptions];
        CGColorSpaceRef colorSpace = CGColorSpaceCreateDeviceRGB();
        const float colors[4][3] = {{0.75,0.125,0.0625},{0.0625,0.75,0.125},
                                   {0.125,0.0625,0.75},{0.75,0.625,0.0625}};
        CIImage *image = [CIImage emptyImage];
        for (NSUInteger tile = 0; tile < 4; tile++) {
            CIColor *color = [CIColor colorWithRed:colors[tile][0] green:colors[tile][1] blue:colors[tile][2] alpha:1];
            CIImage *piece = [[CIImage imageWithColor:color] imageByCroppingToRect:
                CGRectMake(tile % 2 * width / 2, tile / 2 * height / 2, width / 2, height / 2)];
            image = [piece imageByCompositingOverImage:image];
        }
        NSMutableData *reference = [NSMutableData dataWithLength:width * height * 4];
        CIRenderDestination *cpuDestination = [[CIRenderDestination alloc] initWithBitmapData:reference.mutableBytes
            width:width height:height bytesPerRow:width * 4 format:kCIFormatRGBA8];
        cpuDestination.flipped = NO;
        cpuDestination.colorSpace = colorSpace;
        NSError *error = nil;
        CIRenderTask *cpuTask = [cpu startTaskToRender:image toDestination:cpuDestination error:&error];
        if (![cpuTask waitUntilCompletedAndReturnError:&error]) return 3;
        NSMutableArray *cases = [NSMutableArray array];
        NSUInteger failures = 0;
        for (NSUInteger backing = 0; backing < 3; backing++) {
            for (NSInteger mode = -1; mode <= 2; mode++) { @autoreleasepool {
                CVPixelBufferRef buffer = NULL;
                CAMetalLayer *layer = nil;
                id<CAMetalDrawable> drawable = nil;
                id<MTLTexture> texture = nil;
                if (backing == 2) {
                    layer = [CAMetalLayer layer]; layer.device = device;
                    layer.pixelFormat = MTLPixelFormatBGRA8Unorm; layer.framebufferOnly = NO;
                    layer.drawableSize = CGSizeMake(width,height); layer.bounds = CGRectMake(0,0,width,height);
                    drawable = [layer nextDrawable]; texture = drawable.texture;
                } else {
                    MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:
                        MTLPixelFormatBGRA8Unorm width:width height:height mipmapped:NO];
                    descriptor.usage = MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite | MTLTextureUsageRenderTarget;
                    descriptor.storageMode = MTLStorageModeShared;
                    if (backing == 1) {
                        if (CVPixelBufferCreate(kCFAllocatorDefault,width,height,kCVPixelFormatType_32BGRA,
                            (__bridge CFDictionaryRef)@{(id)kCVPixelBufferIOSurfacePropertiesKey: @{},
                                (id)kCVPixelBufferMetalCompatibilityKey: @YES},&buffer)) return 4;
                        texture = [device newTextureWithDescriptor:descriptor iosurface:CVPixelBufferGetIOSurface(buffer) plane:0];
                    } else texture = [device newTextureWithDescriptor:descriptor];
                }
                if (!texture) return 5;
                BOOL isDrawable = drawableFlag(texture), expectedDrawable = backing == 2;
                id<MTLTexture> view = [texture newTextureViewWithPixelFormat:texture.pixelFormat];
                BOOL viewFlag = view && drawableFlag(view);
                id<MTLCommandBuffer> command = [queue commandBuffer];
                CIRenderDestination *destination = [[CIRenderDestination alloc] initWithMTLTexture:texture commandBuffer:command];
                if (!destination) return 6;
                if (mode == 0 || mode == 1) destination.flipped = mode == 1;
                BOOL expectedFlipped = (mode == 0 || mode == 1) ? mode == 1 : expectedDrawable;
                destination.colorSpace = colorSpace;
                CIRenderTask *task = nil;
                if (mode == 2) [gpu render:image toMTLTexture:texture commandBuffer:command
                    bounds:CGRectMake(0,0,width,height) colorSpace:colorSpace];
                else task = [gpu startTaskToRender:image toDestination:destination error:&error];
                [command commit]; [command waitUntilCompleted];
                BOOL completed = command.status == MTLCommandBufferStatusCompleted &&
                    (mode == 2 || (task && [task waitUntilCompletedAndReturnError:&error] != nil));
                NSMutableData *pixels = [NSMutableData dataWithLength:width * height * 4];
                [texture getBytes:pixels.mutableBytes bytesPerRow:width * 4
                    fromRegion:MTLRegionMake2D(0,0,width,height) mipmapLevel:0];
                const uint8_t *actual = pixels.bytes, *expected = reference.bytes;
                NSUInteger wrong = 0;
                for (NSUInteger y = 0; y < height; y++) for (NSUInteger x = 0; x < width; x++) {
                    NSUInteger target = (y * width + x) * 4;
                    NSUInteger source = ((expectedFlipped ? height - 1 - y : y) * width + x) * 4;
                    BOOL bad = NO;
                    for (NSUInteger channel = 0; channel < 4; channel++)
                        bad |= abs((int)actual[target + (channel < 3 ? 2 - channel : 3)] - expected[source + channel]) > 3;
                    wrong += bad;
                }
                BOOL valid = completed && !wrong && isDrawable == expectedDrawable &&
                    viewFlag == expectedDrawable && destination.isFlipped == expectedFlipped;
                if (!valid) failures++;
                [cases addObject:@{@"backing": @[@"plain",@"IOSurface",@"CAMetalLayer"][backing],
                    @"mode": @(mode), @"isDrawable": @(isDrawable), @"viewIsDrawable": @(viewFlag),
                    @"flipped": @(destination.isFlipped), @"expectedFlipped": @(expectedFlipped),
                    @"completed": @(completed), @"wrongPixels": @(wrong), @"valid": @(valid)}];
                if (buffer) CVPixelBufferRelease(buffer);
            }}
        }
        // Repeated wrappers may share a private image, but retain the descriptor's identity.
        CVPixelBufferRef sharedBuffer = NULL;
        if (CVPixelBufferCreate(kCFAllocatorDefault,width,height,kCVPixelFormatType_32BGRA,
            (__bridge CFDictionaryRef)@{(id)kCVPixelBufferIOSurfacePropertiesKey: @{},
                (id)kCVPixelBufferMetalCompatibilityKey: @YES},&sharedBuffer)) return 7;
        NSMutableArray *wrappers = [NSMutableArray array], *cacheCases = [NSMutableArray array];
        for (NSUInteger index = 0; index < 3; index++) {
            BOOL expectedFlag = index == 1;
            MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:
                MTLPixelFormatBGRA8Unorm width:width height:height mipmapped:NO];
            descriptor.usage = MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite;
            descriptor.storageMode = MTLStorageModeShared;
            SEL setDrawable = sel_registerName("setIsDrawable:");
            if (![descriptor respondsToSelector:setDrawable]) return 8;
            ((void (*)(id,SEL,BOOL))objc_msgSend)(descriptor,setDrawable,expectedFlag);
            id<MTLTexture> wrapper = [device newTextureWithDescriptor:descriptor
                iosurface:CVPixelBufferGetIOSurface(sharedBuffer) plane:0];
            if (!wrapper) return 9;
            [wrappers addObject:wrapper];
            BOOL actualFlag = drawableFlag(wrapper);
            if (actualFlag != expectedFlag) failures++;
            [cacheCases addObject:@{@"expectedDrawable": @(expectedFlag), @"actualDrawable": @(actualFlag)}];
        }
        CVPixelBufferRelease(sharedBuffer);
        CGColorSpaceRelease(colorSpace);
        NSData *json = [NSJSONSerialization dataWithJSONObject:@{@"cases": cases,@"cacheCases": cacheCases,@"failures": @(failures)}
            options:NSJSONWritingPrettyPrinted error:NULL];
        fwrite(json.bytes,1,json.length,stdout); putchar('\n');
        return failures ? 1 : 0;
    }
}
