#!/usr/bin/env python3
"""Check GL uniform recognition against the AIR name schema and reflected access."""
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
start = source.index('static uint32_t nvmtl_gl_uniform_index(')
function = source[start:source.index('\nstatic void nvmtl_gl_uniform_inherit', start)]
fixture = r'''

#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <stdint.h>
#include <assert.h>
@interface NVMTLFunction:NSObject {@public NSString *_air,*_stage;NSDictionary *_names,*_reflection;}
@end
@implementation NVMTLFunction
@end
static char kNVMTLGLUniformIndex;
static NSDictionary *nvmtl_air_names(NVMTLFunction *fn){return fn->_names;}
static NSDictionary *nvmtl_function_reflection(NVMTLFunction *fn){return fn->_reflection;}
__PRODUCTION_FUNCTION__

static NSDictionary *binding(unsigned slot,unsigned space,NSString *access){return @{ @"kind":@"Buffer",@"metal_index":@(slot),@"address_space":@(space),@"access":access };}
int main(void){@autoreleasepool{
 NVMTLFunction *f=[NVMTLFunction new];f->_air=@"AIR";f->_stage=@"vertex";
 // Match the actual nvmtl_air_names schema: no access dictionary exists.
 f->_names=@{@"args":@{@"buffer:7":@"vs_uniform_buffer"},@"members":@{},@"types":@{},@"plain":@{}};
 f->_reflection=@{@"bindings":@[binding(7,2,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==7);
 f->_reflection=@{@"bindings":@[binding(7,1,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_reflection=@{@"bindings":@[binding(7,2,@"ReadWrite")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_reflection=@{@"bindings":@[binding(8,2,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_reflection=@{@"bindings":@[binding(7,2,@"ReadOnly"),binding(8,2,@"ReadOnly")]};
 f->_names=@{@"args":@{@"buffer:7":@"vs_uniform_buffer",@"buffer:8":@"vs_uniform_buffer"}};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_names=@{@"args":@{@"buffer:7":@"unrelated_buffer"}};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_stage=@"fragment";f->_names=@{@"args":@{@"buffer:7":@"fs_uniform_buffer"}};assert(nvmtl_gl_uniform_index(f)==7);
 f->_names=@{@"args":@{@"buffer:7":@"ANGLE_userUniforms"}};assert(nvmtl_gl_uniform_index(f)==7);
 f->_stage=@"vertex";assert(nvmtl_gl_uniform_index(f)==7);
 f->_reflection=@{@"bindings":@[binding(7,2,@"ReadWrite")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_reflection=@{@"bindings":@[binding(7,1,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_names=@{@"args":@{@"buffer:7":@"ANGLE_angleUniforms"}};f->_reflection=@{@"bindings":@[binding(7,2,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_names=@{@"args":@{@"buffer:7":@"ANGLE_userUniforms",@"buffer:8":@"vs_uniform_buffer"}};f->_reflection=@{@"bindings":@[binding(7,2,@"ReadOnly"),binding(8,2,@"ReadOnly")]};assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);
 f->_air=nil;assert(nvmtl_gl_uniform_index(f)==UINT32_MAX);objc_setAssociatedObject(f,&kNVMTLGLUniformIndex,@7,OBJC_ASSOCIATION_RETAIN_NONATOMIC);assert(nvmtl_gl_uniform_index(f)==7);
 assert(nvmtl_gl_uniform_index(nil)==UINT32_MAX);puts("PASS actual names schema, native GL and ANGLE vertex/fragment, readonly/address-space/slot rejection, ambiguity, unrelated names, inherited specialization");
}}
'''
with tempfile.TemporaryDirectory(prefix='nvmtl-uniform-index-') as directory:
    directory = Path(directory)
    path = directory / 'test.m'
    path.write_text(fixture.replace('__PRODUCTION_FUNCTION__', function))
    for label, flags in [('release', []), ('asan-ubsan', ['-fsanitize=address,undefined', '-fno-omit-frame-pointer'])]:
        binary = directory / label
        subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-g', *flags, str(path), '-o', str(binary), '-framework', 'Foundation'], check=True)
        subprocess.run([str(binary)], check=True)
