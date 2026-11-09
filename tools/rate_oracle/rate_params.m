/* rate_params.m - the parameter buffer of a real rate map, as raw 32-bit words, plus its physical size.
   Usage: rate_params W H cellsX cellsY out.bin  rx0 rx1 ... ry0 ry1 ...   (rates: horizontal cells then vertical cells) */
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 7) return 2;
    const unsigned W = atoi(argv[1]), H = atoi(argv[2]), nx = atoi(argv[3]), ny = atoi(argv[4]);
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    MTLRasterizationRateMapDescriptor *d = [MTLRasterizationRateMapDescriptor rasterizationRateMapDescriptorWithScreenSize:MTLSizeMake(W, H, 0)];
    MTLRasterizationRateLayerDescriptor *l = [[MTLRasterizationRateLayerDescriptor alloc] initWithSampleCount:MTLSizeMake(nx, ny, 0)];
    for (unsigned i = 0; i < nx; i++) l.horizontalSampleStorage[i] = (float)atof(argv[6 + i]);
    for (unsigned i = 0; i < ny; i++) l.verticalSampleStorage[i] = (float)atof(argv[6 + nx + i]);
    [d setLayer:l atIndex:0];
    id<MTLRasterizationRateMap> m = [dev newRasterizationRateMapWithDescriptor:d];
    if (!m) { printf("refused\n"); return 1; }
    MTLSize ps = [m physicalSizeForLayer:0]; MTLSizeAndAlign sa = m.parameterBufferSizeAndAlign;
    id<MTLBuffer> b = [dev newBufferWithLength:sa.size options:MTLResourceStorageModeShared];
    memset(b.contents, 0xEE, sa.size);
    [m copyParameterDataToBuffer:b offset:0];
    [[NSData dataWithBytes:b.contents length:sa.size] writeToFile:[NSString stringWithUTF8String:argv[5]] atomically:YES];
    printf("physical %lux%lu; buffer %lu bytes\n", (unsigned long)ps.width, (unsigned long)ps.height, (unsigned long)sa.size);
}}
