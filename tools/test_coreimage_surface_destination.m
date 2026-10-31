// GPU regression: Core Image accepts writable IOSurfaces, including linear storage-twin targets.
// Build with Foundation, CoreImage, CoreVideo, Metal and IOSurface. No camera is used.
#import <Foundation/Foundation.h>
#import <CoreImage/CoreImage.h>
#import <CoreVideo/CoreVideo.h>
#import <Metal/Metal.h>
#include <math.h>
#include <unistd.h>

int main(void) {
    alarm(60);
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 2;
        id<MTLCommandQueue> queue = [device newCommandQueue];
        CIContext *context = [CIContext contextWithMTLCommandQueue:queue options:@{
            kCIContextWorkingColorSpace: (id)kCFNull, kCIContextOutputColorSpace: (id)kCFNull,
            kCIContextCacheIntermediates: @NO}];
        if (!context) return 5;
        NSMutableArray *cases = [NSMutableArray array];
        NSUInteger failures = 0;
        const MTLPixelFormat formats[] = {MTLPixelFormatBGRA8Unorm, MTLPixelFormatRGBA16Float, MTLPixelFormatRGBA16Unorm};
        const OSType cvformats[] = {kCVPixelFormatType_32BGRA, kCVPixelFormatType_64RGBAHalf, 0x6c363472};
        const MTLTextureUsage usages[] = {MTLTextureUsageUnknown, MTLTextureUsageShaderRead, MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite};
        for (unsigned backing = 0; backing < 2; backing++) {
            for (unsigned f = 0; f < 3; f++) {
                for (unsigned u = 0; u < 3; u++) {
                    @autoreleasepool {
                        CVPixelBufferRef pixelBuffer = NULL;
                        MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:formats[f]
                            width:64 height:64 mipmapped:NO];
                        descriptor.usage = usages[u];
                        descriptor.storageMode = MTLStorageModeShared;
                        id<MTLTexture> texture;
                        if (backing) {
                            CVReturn status = CVPixelBufferCreate(kCFAllocatorDefault, 64, 64, cvformats[f],
                                (__bridge CFDictionaryRef)@{(id)kCVPixelBufferIOSurfacePropertiesKey: @{},
                                    (id)kCVPixelBufferMetalCompatibilityKey: @YES}, &pixelBuffer);
                            if (status || !pixelBuffer) return 3;
                            texture = [device newTextureWithDescriptor:descriptor iosurface:CVPixelBufferGetIOSurface(pixelBuffer) plane:0];
                        } else {
                            texture = [device newTextureWithDescriptor:descriptor];
                        }
                        if (!texture) return 4;
                        id<MTLCommandBuffer> command = [queue commandBuffer];
                        CIRenderDestination *destination = [[CIRenderDestination alloc] initWithMTLTexture:texture commandBuffer:command];
                        NSMutableDictionary *result = [@{@"IOSurface": @(backing), @"format": @(formats[f]),
                            @"requestedUsage": @(usages[u]), @"reportedUsage": @(texture.usage),
                            @"CoreImageAccepted": @(destination != nil)} mutableCopy];
                        if (destination) {
                            destination.colorSpace = NULL;
                            CIImage *image = [[CIImage imageWithColor:[CIColor colorWithRed:0.25 green:0.5 blue:0.75 alpha:1]]
                                imageByCroppingToRect:CGRectMake(0, 0, 64, 64)];
                            NSError *error = nil;
                            CIRenderTask *task = [context startTaskToRender:image toDestination:destination error:&error];
                            [command commit]; [command waitUntilCompleted];
                            BOOL complete = task && [task waitUntilCompletedAndReturnError:&error] != nil
                                && command.status == MTLCommandBufferStatusCompleted;
                            result[@"completed"] = @(complete);
                            if (error) result[@"error"] = error.description;
                            NSUInteger row = 64 * (f ? 8 : 4), bytes = row * 64;
                            NSMutableData *readback = [NSMutableData dataWithLength:bytes];
                            [texture getBytes:readback.mutableBytes bytesPerRow:row
                                fromRegion:MTLRegionMake2D(0, 0, 64, 64) mipmapLevel:0];
                            const float expected[] = {0.25, 0.5, 0.75, 1};
                            NSUInteger bad = 0;
                            for (unsigned pixel = 0; pixel < 64 * 64; pixel++) {
                                BOOL wrong = NO;
                                for (unsigned channel = 0; channel < 4; channel++) {
                                    float value;
                                    if (!f) value = ((const uint8_t *)readback.bytes)[pixel * 4 + (channel < 3 ? 2 - channel : 3)] / 255.f;
                                    else if (f == 1) value = ((const _Float16 *)readback.bytes)[pixel * 4 + channel];
                                    else value = ((const uint16_t *)readback.bytes)[pixel * 4 + channel] / 65535.f;
                                    if (fabsf(value - expected[channel]) > 0.015f) wrong = YES;
                                }
                                if (wrong) bad++;
                            }
                            result[@"badPixels"] = @(bad);
                        }
                        BOOL expected = backing || (usages[u] & MTLTextureUsageShaderWrite);
                        if ((expected && !destination) || (!backing && usages[u] == MTLTextureUsageShaderRead && destination)
                            || (destination && (![result[@"completed"] boolValue] || [result[@"badPixels"] unsignedIntegerValue]))) failures++;
                        [cases addObject:result];
                        if (pixelBuffer) CVPixelBufferRelease(pixelBuffer);
                    }
                }
            }
        }
        NSData *data = [NSJSONSerialization dataWithJSONObject:@{@"cases": cases, @"failures": @(failures)} options:NSJSONWritingPrettyPrinted error:NULL];
        fwrite(data.bytes, 1, data.length, stdout); putchar('\n');
        return failures ? 1 : 0;
    }
    return 0;
}
