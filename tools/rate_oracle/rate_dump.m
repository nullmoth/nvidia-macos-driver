/* rate_dump.m - dense samples of a real rate map's x mapping, to fit the driver's model.
   Usage: rate_dump W H cellsX r0 r1 ... (vertical uniform 1.0). Prints "x phys" lines. */
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 5) return 2;
    const unsigned W = atoi(argv[1]), H = atoi(argv[2]), n = atoi(argv[3]);
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    MTLRasterizationRateMapDescriptor *d = [MTLRasterizationRateMapDescriptor rasterizationRateMapDescriptorWithScreenSize:MTLSizeMake(W, H, 0)];
    MTLRasterizationRateLayerDescriptor *l = [[MTLRasterizationRateLayerDescriptor alloc] initWithSampleCount:MTLSizeMake(n, 1, 0)];
    for (unsigned i = 0; i < n; i++) l.horizontalSampleStorage[i] = (float)atof(argv[4 + i]);
    l.verticalSampleStorage[0] = 1.0f;
    [d setLayer:l atIndex:0];
    id<MTLRasterizationRateMap> m = [dev newRasterizationRateMapWithDescriptor:d];
    if (!m) { printf("refused\n"); return 1; }
    MTLSize ps = [m physicalSizeForLayer:0];
    printf("physical %lux%lu\n", (unsigned long)ps.width, (unsigned long)ps.height);
    for (unsigned x = 0; x <= W; x += W / 40) { MTLCoordinate2D p = [m mapScreenToPhysicalCoordinates:MTLCoordinate2DMake(x, H / 2.0f) forLayer:0]; printf("%u %.4f\n", x, p.x); }
}}
