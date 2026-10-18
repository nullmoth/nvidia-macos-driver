#!/usr/bin/env python3
"""Exercise the production render sampler key and invalidation helper without a GPU."""
import os
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'plugin/NVMTLObjects.m').read_text()
start = source.index('static NSDictionary *nvmtl_pixel_sampler_norm(')
end = source.index('- (NVMTLRenderPipelineState *)nvmtlSamplerPipeline', start)
helpers = source[start:end]
start = source.index('        NSDictionary *vS=nil,*fS=nil;', end)
end = source.index('        @synchronized(_ps)', start)
key_path = source[start:end]
assert source.count('if (nvmtl_pixel_sampler_update(') == 4
assert '_fragmentSamplers[key]=state' not in source
assert '_vertexSamplers[key]=state' not in source

fixture = r'''
#import <Foundation/Foundation.h>
#include <assert.h>
''' + helpers + r'''
@interface Cache : NSObject {
@public NSMutableDictionary *_vertexSamplers, *_fragmentSamplers;
    NSData *_pixelSamplerKey; NSMutableArray *_pixelSamplerKeys;
}
- (NSData *)key;
- (void)put:(NSDictionary *)state vertex:(BOOL)vertex;
@end
@implementation Cache
- (instancetype)init { if ((self=[super init])) { _vertexSamplers=[NSMutableDictionary new]; _fragmentSamplers=[NSMutableDictionary new]; } return self; }
- (void)put:(NSDictionary *)state vertex:(BOOL)vertex {
    if (nvmtl_pixel_sampler_update(vertex ? _vertexSamplers : _fragmentSamplers,@"0",state)) _pixelSamplerKey=nil;
}
- (NSData *)key {
    if (_vertexSamplers.count || _fragmentSamplers.count) {
''' + key_path + r'''
        return key;
    }
    return nil;
}
@end
int main(void) { @autoreleasepool {
    Cache *cache=[Cache new]; assert(!cache.key);
    NSDictionary *nearest=@{@"min_filter":@"Nearest",@"lod_min_clamp":@0,@"lod_max_clamp":@100};
    NSDictionary *linear=@{@"min_filter":@"Linear",@"lod_min_clamp":@0,@"lod_max_clamp":@100};
    [cache put:nearest vertex:NO]; NSData *a=cache.key; assert(a && cache->_pixelSamplerKeys.count==1);
    for (unsigned i=0;i<10000;i++) { [cache put:[nearest copy] vertex:NO]; assert(cache.key==a); }
    [cache put:linear vertex:NO]; NSData *b=cache.key; assert(b && ![b isEqual:a]);
    [cache put:nearest vertex:NO]; assert(cache.key==a && cache->_pixelSamplerKeys.count==2);
    NSMutableDictionary *clamp=[nearest mutableCopy]; clamp[@"lod_max_clamp"]=@12;
    [cache put:clamp vertex:NO]; NSData *maxKey=cache.key;
    if (getenv("NVMTL_NO_SAMPKEY")) assert(![maxKey isEqual:a]); else assert([maxKey isEqual:a]);
    // The slot's mutable map may change after its shallow immutable snapshot is saved.
    [cache put:linear vertex:NO]; assert(cache.key==b);
    clamp=[nearest mutableCopy]; clamp[@"lod_min_clamp"]=@2;
    [cache put:clamp vertex:NO]; NSData *minKey=cache.key; assert(![minKey isEqual:a]);
    [cache put:nearest vertex:YES]; assert(![cache.key isEqual:minKey]);
    [cache put:nil vertex:YES]; assert(cache.key==minKey);
    [cache put:nil vertex:NO]; assert(!cache.key);
    [cache put:nearest vertex:NO]; assert(cache.key==a);
    // More than eight states evicts old entries while retaining correct serialized values.
    for (unsigned i=0;i<20;i++) {
        NSDictionary *state=@{@"min_filter":@"Nearest",@"lod_min_clamp":@(10+i),@"lod_max_clamp":@100};
        [cache put:state vertex:NO]; assert(cache.key && cache->_pixelSamplerKeys.count<=8);
    }
    [cache put:nearest vertex:NO]; assert([cache.key isEqual:a] && cache->_pixelSamplerKeys.count==8);
    Cache *other=[Cache new]; [other put:linear vertex:NO]; assert([other.key isEqual:b] && cache.key!=other.key);
    puts("PASS: unchanged/equal rebinds, alternate states, vertex/fragment separation, LOD clamps, unbind, bounded eviction, encoder isolation");
} }
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-sampler-key-') as tmp:
    path = pathlib.Path(tmp)
    (path / 'test.m').write_text(fixture)
    subprocess.run(['xcrun', 'clang', '-O2', '-fobjc-arc', str(path/'test.m'),
                    '-framework', 'Foundation', '-o', str(path/'test')], check=True)
    for no_norm in (False, True):
        env = os.environ.copy()
        env.pop('NVMTL_NO_SAMPKEY', None)
        if no_norm:
            env['NVMTL_NO_SAMPKEY'] = '1'
        subprocess.run([str(path/'test')], env=env, check=True)
