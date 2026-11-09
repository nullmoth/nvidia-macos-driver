// GPU regression: an unspecified-usage linear IOSurface still accepts shader writes.
// Build with Foundation, Metal and IOSurface; run with the driver under test.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <stdint.h>
#include <unistd.h>

int main(void) {
    alarm(45);
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 2;
        NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
            "kernel void fill(texture2d<float, access::write> target [[texture(0)]],"
            " uint2 position [[thread_position_in_grid]]) {"
            " target.write(float4(0.25, 0.5, 0.75, 1.0), position); }";
        NSError *error = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        id<MTLComputePipelineState> pipeline = library ? [device newComputePipelineStateWithFunction:
            [library newFunctionWithName:@"fill"] error:&error] : nil;
        if (!pipeline) { fprintf(stderr, "%s\n", error.description.UTF8String); return 3; }
        id<MTLCommandQueue> queue = [device newCommandQueue];
        NSMutableArray *cases = [NSMutableArray array];
        NSUInteger failures = 0;
        for (NSUInteger usageIndex = 0; usageIndex < 2; usageIndex++) {
            for (NSUInteger sizeIndex = 0; sizeIndex < 2; sizeIndex++) {
                @autoreleasepool {
                    NSUInteger width = sizeIndex ? 64 : 16, height = 32;
                    size_t row = width * 8;
                    IOSurfaceRef surface = IOSurfaceCreate((__bridge CFDictionaryRef)@{
                        (id)kIOSurfaceWidth: @(width), (id)kIOSurfaceHeight: @(height),
                        (id)kIOSurfaceBytesPerElement: @8, (id)kIOSurfaceBytesPerRow: @(row),
                        (id)kIOSurfaceAllocSize: @(row * height),
                        (id)kIOSurfacePixelFormat: @(0x6c363472), // 64RGBALE / 'l64r'
                    });
                    if (!surface) return 4;
                    IOSurfaceLock(surface, 0, NULL);
                    memset(IOSurfaceGetBaseAddress(surface), 0x22, row * height);
                    IOSurfaceUnlock(surface, 0, NULL);
                    MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
                        texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA16Unorm
                        width:width height:height mipmapped:NO];
                    descriptor.storageMode = MTLStorageModeShared;
                    descriptor.usage = usageIndex ? MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite
                                                 : MTLTextureUsageUnknown;
                    id<MTLTexture> texture = [device newTextureWithDescriptor:descriptor iosurface:surface plane:0];
                    if (!texture) { CFRelease(surface); return 5; }
                    id<MTLCommandBuffer> command = [queue commandBuffer];
                    id<MTLComputeCommandEncoder> encoder = [command computeCommandEncoder];
                    [encoder setComputePipelineState:pipeline];
                    [encoder setTexture:texture atIndex:0];
                    [encoder dispatchThreads:MTLSizeMake(width, height, 1)
                        threadsPerThreadgroup:MTLSizeMake(8, 8, 1)];
                    [encoder endEncoding]; [command commit]; [command waitUntilCompleted];
                    if (command.status == MTLCommandBufferStatusError) {
                        fprintf(stderr, "%s\n", command.error.description.UTF8String);
                        CFRelease(surface); return 6;
                    }
                    NSMutableData *readback = [NSMutableData dataWithLength:row * height];
                    [texture getBytes:readback.mutableBytes bytesPerRow:row
                        fromRegion:MTLRegionMake2D(0, 0, width, height) mipmapLevel:0];
                    const uint16_t *pixels = readback.bytes;
                    const int expected[4] = {16384, 32768, 49151, 65535};
                    NSUInteger badPixels = 0;
                    for (NSUInteger pixel = 0; pixel < width * height; pixel++) {
                        BOOL bad = NO;
                        for (int channel = 0; channel < 4; channel++)
                            if (abs((int)pixels[pixel * 4 + channel] - expected[channel]) > 2) bad = YES;
                        if (bad) badPixels++;
                    }
                    if (badPixels) failures++;
                    [cases addObject:@{@"requestedUsage": @(descriptor.usage),
                        @"reportedUsage": @(texture.usage), @"width": @(width),
                        @"badPixels": @(badPixels),
                        @"firstPixel": @[@(pixels[0]), @(pixels[1]), @(pixels[2]), @(pixels[3])]}];
                    CFRelease(surface);
                }
            }
        }
        NSData *json = [NSJSONSerialization dataWithJSONObject:@{@"failures": @(failures), @"cases": cases}
            options:NSJSONWritingPrettyPrinted error:NULL];
        fwrite(json.bytes, 1, json.length, stdout); putchar('\n');
        return failures ? 1 : 0;
    }
}
