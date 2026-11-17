// GPU regression: Unknown usage must permit array writes, including through a mip/slice view.
// Build: clang -fobjc-arc tools/test_unknown_array_storage.m -framework Foundation -framework Metal -o <test>
// Run explicitly with the isolated driver under test. No installed-driver changes are needed.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <math.h>
#include <unistd.h>

int main(void) {
    alarm(60);
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 2;
        NSError *error = nil;
        NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
            "kernel void fill(texture2d_array<float, access::write> dst [[texture(0)]],"
            " uint3 p [[thread_position_in_grid]]) {"
            " dst.write(float4(0.25 * (p.z + 1), 0.5, 0.75, 1.0), p.xy, p.z); }";
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        id<MTLComputePipelineState> pipeline = library ? [device newComputePipelineStateWithFunction:
            [library newFunctionWithName:@"fill"] error:&error] : nil;
        if (!pipeline) { fprintf(stderr, "%s\n", error.description.UTF8String); return 3; }
        id<MTLCommandQueue> queue = [device newCommandQueue];
        NSUInteger failures = 0;
        for (NSUInteger allocation = 0; allocation < 3; allocation++)
        for (NSUInteger explicitUsage = 0; explicitUsage < 2; explicitUsage++)
        for (NSUInteger wide = 0; wide < 2; wide++) {
            @autoreleasepool {
                MTLTextureDescriptor *d = [MTLTextureDescriptor new];
                d.textureType = MTLTextureType2DArray;
                d.pixelFormat = wide ? MTLPixelFormatRGBA32Float : MTLPixelFormatRGBA16Float;
                d.width = 16; d.height = 8; d.arrayLength = 4; d.mipmapLevelCount = 2;
                d.storageMode = MTLStorageModePrivate;
                d.usage = explicitUsage ? MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite |
                                         MTLTextureUsagePixelFormatView : MTLTextureUsageUnknown;
                id<MTLHeap> heap = nil;
                id<MTLTexture> texture = nil;
                if (allocation) {
                    MTLSizeAndAlign sa = [device heapTextureSizeAndAlignWithDescriptor:d];
                    MTLHeapDescriptor *hd = [MTLHeapDescriptor new];
                    hd.storageMode = MTLStorageModePrivate;
                    hd.size = sa.size + sa.align;
                    if (allocation == 2) hd.type = MTLHeapTypePlacement;
                    heap = [device newHeapWithDescriptor:hd];
                    texture = allocation == 2 ? [heap newTextureWithDescriptor:d offset:0] :
                                                [heap newTextureWithDescriptor:d];
                } else texture = [device newTextureWithDescriptor:d];
                id<MTLTexture> view = [texture newTextureViewWithPixelFormat:d.pixelFormat
                    textureType:MTLTextureType2DArray levels:NSMakeRange(1, 1) slices:NSMakeRange(1, 2)];
                if (!texture || !view) { fprintf(stderr, "texture/view allocation failed\n"); return 4; }
                const NSUInteger row = 256, sliceBytes = row * 8, levelBytes = sliceBytes * 4;
                id<MTLBuffer> initial = [device newBufferWithLength:levelBytes * 2 options:MTLResourceStorageModeShared];
                id<MTLBuffer> result = [device newBufferWithLength:levelBytes * 2 options:MTLResourceStorageModeShared];
                if (!initial || !result) return 5;
                memset(initial.contents, 0, initial.length);
                memset(result.contents, 0xCC, result.length);
                id<MTLCommandBuffer> command = [queue commandBuffer];
                id<MTLBlitCommandEncoder> blit = [command blitCommandEncoder];
                for (NSUInteger level = 0; level < 2; level++)
                for (NSUInteger slice = 0; slice < 4; slice++)
                    [blit copyFromBuffer:initial sourceOffset:level * levelBytes + slice * sliceBytes
                        sourceBytesPerRow:row sourceBytesPerImage:sliceBytes
                        sourceSize:MTLSizeMake(16 >> level, 8 >> level, 1) toTexture:texture
                        destinationSlice:slice destinationLevel:level destinationOrigin:MTLOriginMake(0, 0, 0)];
                [blit endEncoding];
                id<MTLComputeCommandEncoder> compute = [command computeCommandEncoder];
                [compute setComputePipelineState:pipeline];
                [compute setTexture:texture atIndex:0];
                [compute dispatchThreads:MTLSizeMake(16, 8, 4) threadsPerThreadgroup:MTLSizeMake(4, 4, 1)];
                [compute setTexture:view atIndex:0];
                [compute dispatchThreads:MTLSizeMake(8, 4, 2) threadsPerThreadgroup:MTLSizeMake(4, 4, 1)];
                [compute endEncoding];
                blit = [command blitCommandEncoder];
                for (NSUInteger level = 0; level < 2; level++)
                for (NSUInteger slice = 0; slice < 4; slice++)
                    [blit copyFromTexture:texture sourceSlice:slice sourceLevel:level
                        sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(16 >> level, 8 >> level, 1)
                        toBuffer:result destinationOffset:level * levelBytes + slice * sliceBytes
                        destinationBytesPerRow:row destinationBytesPerImage:sliceBytes];
                [blit endEncoding]; [command commit]; [command waitUntilCompleted];
                if (command.status == MTLCommandBufferStatusError) {
                    fprintf(stderr, "%s\n", command.error.description.UTF8String); return 6;
                }
                NSUInteger bad = 0;
                for (NSUInteger level = 0; level < 2; level++)
                for (NSUInteger slice = 0; slice < 4; slice++)
                for (NSUInteger y = 0; y < (8 >> level); y++)
                for (NSUInteger x = 0; x < (16 >> level); x++) {
                    BOOL written = !level || (slice >= 1 && slice <= 2);
                    float expected[4] = {0.25f * (float)(level ? slice : slice + 1), 0.5f, 0.75f, 1.0f};
                    const char *pixel = (char *)result.contents + level * levelBytes + slice * sliceBytes +
                                        y * row + x * (wide ? 16 : 8);
                    for (NSUInteger c = 0; c < 4; c++) {
                        float actual = wide ? ((const float *)pixel)[c] : (float)((const _Float16 *)pixel)[c];
                        float wanted = written ? expected[c] : 0;
                        if (!isfinite(actual) || fabsf(actual - wanted) > 0.001f) bad++;
                    }
                }
                printf("allocation=%lu usage=%s format=RGBA%luFloat badChannels=%lu\n",
                    (unsigned long)allocation, explicitUsage ? "explicit" : "unknown",
                    wide ? 32ul : 16ul, (unsigned long)bad);
                if (bad) failures++;
            }
        }
        printf("failures=%lu\n", (unsigned long)failures);
        return failures ? 1 : 0;
    }
}
