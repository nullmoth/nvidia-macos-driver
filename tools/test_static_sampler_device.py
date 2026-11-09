#!/usr/bin/env python3
"""A built-in (static) sampler belongs to the shader's device, never to the system default GPU (a second GPU on hybrid machines)."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'plugin/NVMTLEmbedded.m').read_text()
start = source.index('static NSUInteger nvmtl_ss_enum(')
end = source.index('static const void *NVMTL_SSAMP_KEY', start)
helper = source[start:end]
has_device = 'nvmtl_static_sampler_from(NSDictionary *st, id<MTLDevice> device)' in helper
call = 'nvmtl_static_sampler_from(state, (id<MTLDevice>)owner)' if has_device else 'nvmtl_static_sampler_from(state)'

fixture = r'''
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <assert.h>
#include <stdio.h>
static void nvlog(const char *fmt, ...) {}
@interface TestDevice : NSObject
@property NSUInteger calls;
@property MTLSamplerDescriptor *descriptor;
@property id sampler;
- (id<MTLSamplerState>)newSamplerStateWithDescriptor:(MTLSamplerDescriptor *)descriptor;
@end
@implementation TestDevice
- (id<MTLSamplerState>)newSamplerStateWithDescriptor:(MTLSamplerDescriptor *)descriptor {
    self.calls++; self.descriptor = descriptor;
    return (id<MTLSamplerState>)self.sampler;
}
@end
static TestDevice *systemDevice;
static id<MTLDevice> testSystemDefaultDevice(void) { return (id<MTLDevice>)systemDevice; }
#define MTLCreateSystemDefaultDevice testSystemDefaultDevice
''' + helper + r'''
int main(void) { @autoreleasepool {
    TestDevice *owner = [TestDevice new]; owner.sampler = [NSObject new];
    systemDevice = [TestDevice new]; systemDevice.sampler = [NSObject new];
    NSDictionary *state = @{@"min_filter": @"Linear", @"mag_filter": @"Nearest",
        @"address_mode_s": @"Repeat", @"max_anisotropy": @4,
        @"lod_min_clamp": @1.0, @"lod_max_clamp": @8.0};
    id sampler = CALL;
    assert(sampler == owner.sampler);
    assert(owner.calls == 1 && systemDevice.calls == 0);
    assert(owner.descriptor.minFilter == MTLSamplerMinMagFilterLinear);
    assert(owner.descriptor.magFilter == MTLSamplerMinMagFilterNearest);
    assert(owner.descriptor.sAddressMode == MTLSamplerAddressModeRepeat);
    assert(owner.descriptor.maxAnisotropy == 4);
    assert(owner.descriptor.lodMinClamp == 1.0 && owner.descriptor.lodMaxClamp == 8.0);
    puts("PASS: static sampler uses its shader device and preserves sampler state");
} }
'''.replace('CALL', call)

with tempfile.TemporaryDirectory(prefix='nvmtl-static-sampler-') as tmp:
    path = pathlib.Path(tmp)
    (path / 'test.m').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', str(path / 'test.m'),
                    '-framework', 'Foundation', '-framework', 'Metal', '-o', str(path / 'test')],
                   check=True, timeout=60)
    subprocess.run([str(path / 'test')], check=True, timeout=60)

assert 'nvmtl_static_sampler_from(st, [fn device])' in source
