#!/usr/bin/env python3
"""Exercise the production tiled-texture fallback without a GPU."""
import argparse
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline', nargs='?', const='HEAD', metavar='REF',
                    help='exercise source from this Git revision (default: HEAD)')
parser.add_argument('--overflow-only', action='store_true')
args = parser.parse_args()
if args.baseline:
    source = subprocess.check_output(['git','show',f'{args.baseline}:plugin/NVMTLObjects.m'],cwd=root,text=True)
else:
    source = (root/'plugin/NVMTLObjects.m').read_text()
start = source.index('- (id<MTLTexture>)newTiledTextureWithDescriptor:')
end = source.index('- (id<MTLTexture>)newTextureWithDescriptor:',start)
method = source[start:end]
fixture = r'''
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <assert.h>
#include <string.h>
#if __has_include("nvmtl_buffer_texture.h")
#include "nvmtl_buffer_texture.h"
#endif
#define nvlog(...) ((void)0)
static unsigned copies;
static const void *copiedFrom;
static unsigned char pixels[256];
static int nvmtl_pixfmt_public(MTLPixelFormat f,uint32_t *vk,uint32_t *bpp,int *a8) {
    *vk=f==MTLPixelFormatBC1_RGBA ? 131 : 37; *bpp=4; *a8=0;
    return (f==MTLPixelFormatRGBA8Unorm || f==MTLPixelFormatBC1_RGBA) ? 0 : -1;
}
static int nvmtl_vk_format_block(uint32_t f,uint32_t *bw,uint32_t *bh,uint32_t *bb) {
    *bw=*bh=f==131 ? 4 : 1; *bb=f==131 ? 8 : 0; return f==131;
}
@interface Texture : NSObject
- (void)replaceRegion:(MTLRegion)r mipmapLevel:(NSUInteger)m withBytes:(const void *)p bytesPerRow:(NSUInteger)b;
@end
@implementation Texture
- (void)replaceRegion:(MTLRegion)r mipmapLevel:(NSUInteger)m withBytes:(const void *)p bytesPerRow:(NSUInteger)b {
    copies++; copiedFrom=p;
    assert(r.size.width<=16 && r.size.height<=4);
    for (NSUInteger y=0;y<r.size.height;y++)
        memcpy(pixels+y*r.size.width*4,(const char *)p+y*b,r.size.width*4);
}
@end
@interface Device : NSObject
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d;
@end
@implementation Device
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d { return (id)[Texture new]; }
@end
@interface Buffer : NSObject { @public struct { size_t size; } _b; void *_base; }
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)o bytesPerRow:(NSUInteger)p;
- (id<MTLTexture>)newTiledTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)o bytesPerRow:(NSUInteger)p;
- (void *)contents;
- (Device *)device;
@end
@implementation Buffer
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)o bytesPerRow:(NSUInteger)p { return nil; }
- (void *)contents { return _base; }
- (Device *)device { return [Device new]; }
/*METHOD*/
@end
static void check(Buffer *b,MTLTextureDescriptor *d,size_t offset,size_t pitch,BOOL expected) {
    copies=0; copiedFrom=NULL;
    id t=[b newTiledTextureWithDescriptor:d offset:offset bytesPerRow:pitch];
    assert(!!t==expected && copies==(unsigned)expected);
    if (expected) {
        assert(copiedFrom==(char *)b->_base+offset);
        for (NSUInteger y=0;y<d.height;y++)
            assert(!memcmp(pixels+y*d.width*4,(char *)b->_base+offset+y*pitch,d.width*4));
    }
}
int main(void) { @autoreleasepool {
    unsigned char bytes[256]; for (unsigned i=0;i<sizeof bytes;i++) bytes[i]=(unsigned char)i;
    Buffer *b=[Buffer new]; b->_base=bytes;
    MTLTextureDescriptor *d=[MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:4 height:2 mipmapped:NO];
    b->_b.size=96; check(b,d,16,32,YES);  // 16-mod-32 address: exact original bytes.
#ifndef TEST_OVERFLOW_ONLY
    b->_b.size=64; check(b,d,16,32,YES);  // Last row needs texel bytes, not trailing padding.
#endif
    b->_b.size=63; check(b,d,16,32,NO);
    b->_b.size=96; check(b,d,SIZE_MAX-15,32,NO); // offset + pitch*height wraps in the old check.
    check(b,d,0,SIZE_MAX/2+1,NO);              // pitch*height wraps too.
    check(b,d,0,8,NO);                        // Short and zero rows are invalid.
    check(b,d,0,0,NO);
    d.width=SIZE_MAX; check(b,d,0,32,NO);
    d.width=4; d.height=SIZE_MAX; check(b,d,0,32,NO);
    d.height=0; check(b,d,0,32,NO);
    d.height=2; d.width=0; check(b,d,0,32,NO); d.width=4;
    d.height=2; d.pixelFormat=MTLPixelFormatInvalid; check(b,d,0,32,NO);
    d.pixelFormat=MTLPixelFormatBC1_RGBA; check(b,d,0,32,NO);
    d.pixelFormat=MTLPixelFormatRGBA8Unorm; d.arrayLength=2; check(b,d,0,32,NO);
    d.arrayLength=1; d.mipmapLevelCount=2; check(b,d,0,32,NO);
    d.mipmapLevelCount=1; d.sampleCount=4; check(b,d,0,32,NO);
    puts("PASS: 16-mod-32 starts preserve exact bytes; partial final rows work; short rows, overflows and unsupported layouts are rejected");
} }
'''.replace('/*METHOD*/',method)
with tempfile.TemporaryDirectory() as directory:
    p=pathlib.Path(directory)
    (p/'bounds.m').write_text(fixture)
    subprocess.run(['clang','-fobjc-arc','-O1','-g','-fsanitize=address,undefined','-I',str(root/'plugin'),
                    *(['-DTEST_OVERFLOW_ONLY=1'] if args.overflow_only else []),
                    str(p/'bounds.m'),'-framework','Foundation','-framework','Metal','-o',str(p/'bounds')],check=True)
    subprocess.run([str(p/'bounds')],check=True,timeout=20)
