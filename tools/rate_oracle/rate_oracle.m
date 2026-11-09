/* rate_oracle.m - what a real MTLRasterizationRateMap reports: physical size, granularity and the screen <-> physical mapping.
   xcrun clang -fobjc-arc tools/rate_oracle/rate_oracle.m -framework Metal -framework Foundation -o /tmp/rate_oracle && /tmp/rate_oracle
   Prints one line per fact so the driver's model can be diffed against an Apple GPU. */
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
static void run(id<MTLDevice> dev, const char *name, MTLSize screen, MTLSize cells, const float *h, const float *v)
{
    MTLRasterizationRateMapDescriptor *d = [MTLRasterizationRateMapDescriptor rasterizationRateMapDescriptorWithScreenSize:screen];
    MTLRasterizationRateLayerDescriptor *l = [[MTLRasterizationRateLayerDescriptor alloc] initWithSampleCount:cells];
    for (NSUInteger i = 0; i < cells.width; i++) l.horizontalSampleStorage[i] = h[i];
    for (NSUInteger i = 0; i < cells.height; i++) l.verticalSampleStorage[i] = v[i];
    [d setLayer:l atIndex:0];
    id<MTLRasterizationRateMap> m = [dev newRasterizationRateMapWithDescriptor:d];
    if (!m) { printf("%s: map refused\n", name); return; }
    MTLSize ps = [m physicalSizeForLayer:0], gr = m.physicalGranularity, ss = m.screenSize;
    printf("%s: screen %lux%lu physical %lux%lu granularity %lux%lu layers %lu\n", name, (unsigned long)ss.width, (unsigned long)ss.height,
           (unsigned long)ps.width, (unsigned long)ps.height, (unsigned long)gr.width, (unsigned long)gr.height, (unsigned long)m.layerCount);
    MTLSizeAndAlign pb = m.parameterBufferSizeAndAlign;
    printf("%s: parameter buffer %lu bytes, align %lu\n", name, (unsigned long)pb.size, (unsigned long)pb.align);
    MTLCoordinate2D pts[] = { {0, 0}, {1, 1}, {(float)screen.width * 0.25f, (float)screen.height * 0.25f}, {(float)screen.width * 0.5f, (float)screen.height * 0.5f},
                              {(float)screen.width * 0.75f, (float)screen.height * 0.9f}, {(float)screen.width - 1, (float)screen.height - 1} };
    for (unsigned k = 0; k < sizeof pts / sizeof *pts; k++) {
        MTLCoordinate2D p = [m mapScreenToPhysicalCoordinates:pts[k] forLayer:0];
        MTLCoordinate2D b = [m mapPhysicalToScreenCoordinates:p forLayer:0];
        printf("%s: screen (%.3f, %.3f) -> physical (%.4f, %.4f) -> screen (%.3f, %.3f)\n", name, pts[k].x, pts[k].y, p.x, p.y, b.x, b.y);
    }
}
int main(void) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    printf("device %s; supportsRasterizationRateMapWithLayerCount:1 = %d\n", dev.name.UTF8String, (int)[dev supportsRasterizationRateMapWithLayerCount:1]);
    const float uh[1] = { 1.0f }, uv[1] = { 1.0f };
    run(dev, "uniform", MTLSizeMake(1920, 1080, 0), MTLSizeMake(1, 1, 0), uh, uv);
    const float fh[5] = { 0.25f, 0.5f, 1.0f, 0.5f, 0.25f }, fv[3] = { 0.5f, 1.0f, 0.5f };
    run(dev, "foveated", MTLSizeMake(1920, 1080, 0), MTLSizeMake(5, 3, 0), fh, fv);
    const float hh[2] = { 1.0f, 0.5f }, hv[1] = { 1.0f };
    run(dev, "half-right", MTLSizeMake(1000, 500, 0), MTLSizeMake(2, 1, 0), hh, hv);
}}
