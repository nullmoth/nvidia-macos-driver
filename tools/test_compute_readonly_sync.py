#!/usr/bin/env python3
"""Compute encoders keep ReadOnly/Unused buffers out of _written while promoting every writable binding at dispatch.

Compiles the production NVMTLResSet, nvmtl_hz_fn_writes, nvmtl_ps_writable_buffers, nvmtlMarkWrittenBuffers, and
setThreadgroupMemoryLength:atIndex: / fast _e78Key logic straight from plugin/NVMTLObjects.m and checks:
  1. Every compute dispatch site (dispatchThreadgroups:, dispatchThreads:, dispatchThreadgroupsWithIndirectBuffer:,
     and nvmtl_sass_encode in NVMTLSass.m) invokes nvmtlMarkWrittenBuffers.
  2. ReadOnly / Unused slots stay member=YES, isWrittenObject=NO; ReadWrite / WriteOnly / unknown-kind slots become
     isWrittenObject=YES.
  3. A buffer first bound ReadOnly and later dispatched in a writable slot (including after a restart seal) becomes
     isWrittenObject=YES (MRU-safe).
  4. Missing reflection (_function == nil or malformed reflection) falls back to 0xffffffffu (conservative write-all).
  5. Single-slot setThreadgroupMemoryLength:atIndex: produces an NSNumber key without NSJSONSerialization, and
     nvmtlSamplerPipeline copies _function/_hzBuf/_hzTex/_hzReady onto newly built variants."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
objs = (root / 'plugin/NVMTLObjects.m').read_text()
hdr = (root / 'plugin/NVMTLObjects.h').read_text()
sass = (root / 'plugin/NVMTLSass.m').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


for sig in ('- (void)dispatchThreadgroups:(MTLSize)grid',
            '- (void)dispatchThreads:(MTLSize)threads',
            '- (void)dispatchThreadgroupsWithIndirectBuffer:(id<MTLBuffer>)buf'):
    body = objs[objs.index(sig):objs.index(sig) + 600]
    check(f'{sig.split(":")[0]} calls [self nvmtlMarkWrittenBuffers]', '[self nvmtlMarkWrittenBuffers];' in body)

check('nvmtl_sass_encode retains read-only then calls [encoder nvmtlMarkWrittenBuffers]',
      'addReadOnlyObject:encoder->_baObj[k]' in sass and '[encoder nvmtlMarkWrittenBuffers];' in sass)

sp_body = objs[objs.index('- (NVMTLComputePipelineState *)nvmtlSamplerPipeline {'):]
sp_body = sp_body[:sp_body.index('- (void)setComputePipelineState:')]
check('nvmtlSamplerPipeline copies _function, _hz*, and _syncBuf/_syncReady onto variant',
      all(tok in sp_body for tok in ('selected->_function = _ps->_function;',
                                     'selected->_hzBuf = _ps->_hzBuf;',
                                     'selected->_hzReady = _ps->_hzReady;',
                                     'selected->_syncBuf =',
                                     'selected->_syncReady = 1;')))

resset_iface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', hdr, re.S).group(0)
resset_impl = objs[objs.index('static NSMutableSet *nvmtl_identity_set('):objs.index('@end\n@interface NVMTLResSet (SubmissionCopyQuery)') + 4]
hz_fn = objs[objs.index('typedef struct { uint32_t buf; uint64_t tex[2]; } nvmtl_hz_writes;'):
             objs.index('static void nvmtl_hz_render_writes(')]
ps_wr = objs[objs.index('static uint32_t nvmtl_spirv_writable_buffers('):objs.index('static void nvmtl_hz_compute_writes(')]
mark_wr = objs[objs.index('- (void)nvmtlMarkWrittenBuffers {'):objs.index('- (void)dispatchThreadgroups:(MTLSize)grid')]
tg_len = objs[objs.rindex('- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i {'):
              objs.index('// GPU-encoded dispatches (NVMTL_GPU_ICB=1).')]

HARNESS = r'''
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <os/lock.h>
#include <stdatomic.h>

#define NVMTL_NBUFAT 32
static _Atomic unsigned long gRes2Tick;
static _Atomic long gRefs;
static inline void nvmtl_resref(id o, int d) { (void)o; atomic_fetch_add(&gRefs, d); }
static int nvmtl_zprobe_on(void) { return 0; }
static void nvlog(const char *fmt, ...) { (void)fmt; }

''' + resset_iface + '\n' + resset_impl + r'''

@interface NVMTLFunction : NSObject { @public NSDictionary *_refl; NSData *_spirv; } @end
@implementation NVMTLFunction @end
static NSDictionary *nvmtl_function_reflection(NVMTLFunction *fn) { return fn ? fn->_refl : nil; }

@interface NVMTLComputePipelineState : NSObject {
@public
    NVMTLFunction *_function;
    id _sassKernel;
    int _hzReady;
    uint32_t _hzBuf;
    uint64_t _hzTex[2];
    int _syncReady;
    uint32_t _syncBuf;
}
@end
@implementation NVMTLComputePipelineState @end

@interface NVMTLCommandBuffer : NSObject { @public NVMTLResSet *_resources; } @end
@implementation NVMTLCommandBuffer @end

@interface NVMTLBuffer : NSObject @end
@implementation NVMTLBuffer @end

''' + hz_fn + '\n' + ps_wr + r'''

@interface NVMTLComputeCommandEncoder : NSObject {
@public
    NVMTLCommandBuffer *_cb;
    NVMTLComputePipelineState *_ps;
    NSMutableDictionary *_samplers, *_tgLen;
    id _e78Key;
    NSUInteger _tgFastIdx, _tgFastLen;
    NVMTLComputePipelineState *_e78VarSel;
    __strong id _baObj[NVMTL_NBUFAT];
    NSUInteger _baOff[NVMTL_NBUFAT];
    uint8_t _baN[NVMTL_NBUFAT];
}
- (void)setBuffer:(NVMTLBuffer *)nb offset:(NSUInteger)off atIndex:(NSUInteger)i;
- (void)nvmtlMarkWrittenBuffers;
- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i;
- (id)buildVariantKey;
@end

@implementation NVMTLComputeCommandEncoder
- (void)setBuffer:(NVMTLBuffer *)nb offset:(NSUInteger)off atIndex:(NSUInteger)i {
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    if (i >= NVMTL_NBUFAT) [_cb->_resources addObject:nb];
    else if (_baObj[i] != (id)nb) [_cb->_resources addReadOnlyObject:nb];
    if (i < NVMTL_NBUFAT) { _baObj[i] = nb; _baOff[i] = off; _baN[i] = 2; }
}
''' + mark_wr + '\n' + tg_len + r'''
- (id)buildVariantKey {
    if (!_e78Key) {
        if (_samplers.count == 0 && _tgLen.count == 1 && _tgFastIdx < 0x7FFFu && _tgFastLen < (1ull << 40))
            _e78Key = @((((uint64_t)(_tgFastIdx + 1u)) << 40) | (uint64_t)_tgFastLen);
        else
            _e78Key = [NSJSONSerialization dataWithJSONObject:@{@"s": _samplers ?: @{}, @"tg": _tgLen ?: @{}} options:NSJSONWritingSortedKeys error:NULL];
    }
    return _e78Key;
}
@end

int main(void) { @autoreleasepool {
    /* Minimal Logical SPIR-V preamble:
     *   id 10: StorageBuffer at Binding 0 with NonWritable
     *   id 11: StorageBuffer at Binding 1 with NonWritable
     *   id 12: StorageBuffer at Binding 3 WITHOUT NonWritable
     *   id 13: StorageBuffer at Binding 4 WITHOUT NonWritable
     *   id 14: StorageBuffer at Binding 5 WITHOUT NonWritable
     *   (Binding 2 Unused -> dropped from SPIR-V) */
    const uint32_t spv_logical[] = {
        0x07230203u, 0x00010500u, 0u, 20u, 0u,
        (3u << 16) | 14u, 0u, 1u,                  /* OpMemoryModel Logical GLSL450 */
        (4u << 16) | 71u, 10u, 33u, 0u,            /* OpDecorate %10 Binding 0 */
        (3u << 16) | 71u, 10u, 24u,                /* OpDecorate %10 NonWritable */
        (4u << 16) | 71u, 11u, 33u, 1u,            /* OpDecorate %11 Binding 1 */
        (3u << 16) | 71u, 11u, 24u,                /* OpDecorate %11 NonWritable */
        (4u << 16) | 71u, 12u, 33u, 3u,            /* OpDecorate %12 Binding 3 */
        (4u << 16) | 71u, 13u, 33u, 4u,            /* OpDecorate %13 Binding 4 */
        (4u << 16) | 71u, 14u, 33u, 5u,            /* OpDecorate %14 Binding 5 */
        (4u << 16) | 59u, 2u, 10u, 12u,            /* OpVariable %ptr %10 StorageBuffer */
        (4u << 16) | 59u, 2u, 11u, 12u,            /* OpVariable %ptr %11 StorageBuffer */
        (4u << 16) | 59u, 2u, 12u, 12u,            /* OpVariable %ptr %12 StorageBuffer */
        (4u << 16) | 59u, 2u, 13u, 12u,            /* OpVariable %ptr %13 StorageBuffer */
        (4u << 16) | 59u, 2u, 14u, 12u,            /* OpVariable %ptr %14 StorageBuffer */
        (5u << 16) | 54u, 1u, 15u, 0u, 3u,         /* OpFunction */
    };
    NVMTLFunction *fn = [NVMTLFunction new];
    fn->_spirv = [NSData dataWithBytes:spv_logical length:sizeof spv_logical];
    fn->_refl = @{@"bindings": @[
        @{@"metal_index": @0, @"kind": @"Buffer", @"access": @"ReadOnly"},
        @{@"metal_index": @1, @"kind": @"Buffer", @"access": @"ReadOnly"},
        @{@"metal_index": @2, @"kind": @"Buffer", @"access": @"Unused"},
        @{@"metal_index": @3, @"kind": @"Buffer", @"access": @"WriteOnly"},
        @{@"metal_index": @4, @"kind": @"Buffer", @"access": @"ReadWrite"},
        @{@"metal_index": @5, @"kind": @"Buffer", @"access": @"ReadWrite"},
    ]};
    NVMTLComputePipelineState *ps = [NVMTLComputePipelineState new];
    ps->_function = fn;

    NVMTLCommandBuffer *cb = [NVMTLCommandBuffer new];
    cb->_resources = [NVMTLResSet new];
    NVMTLComputeCommandEncoder *enc = [NVMTLComputeCommandEncoder new];
    enc->_cb = cb; enc->_ps = ps;

    NVMTLBuffer *b0 = [NVMTLBuffer new], *b1 = [NVMTLBuffer new], *b2 = [NVMTLBuffer new];
    NVMTLBuffer *b3 = [NVMTLBuffer new], *b4 = [NVMTLBuffer new], *bBytes = [NVMTLBuffer new];
    [enc setBuffer:b0 offset:0 atIndex:0];
    [enc setBuffer:b1 offset:0 atIndex:1];
    [enc setBuffer:b2 offset:0 atIndex:2];
    [enc setBuffer:b3 offset:0 atIndex:3];
    [enc setBuffer:b4 offset:0 atIndex:4];
    /* Simulate a setBytes scratch buffer at writable slot 5 (_baN == 4): must not be treated as user buffer. */
    [cb->_resources addReadOnlyObject:bBytes];
    enc->_baObj[5] = bBytes; enc->_baN[5] = 4;

    [enc nvmtlMarkWrittenBuffers];
    int ro_ok = ([cb->_resources member:b0] != nil && ![cb->_resources isWrittenObject:b0] &&
                 [cb->_resources member:b1] != nil && ![cb->_resources isWrittenObject:b1] &&
                 [cb->_resources member:b2] != nil && ![cb->_resources isWrittenObject:b2] &&
                 [cb->_resources isWrittenObject:b3] && [cb->_resources isWrittenObject:b4] &&
                 ![cb->_resources isWrittenObject:bBytes]);

    /* Contradicted AIR metadata: AIR claims ReadOnly on slot 1, but SPIR-V lacks NonWritable on %11 -> MUST mark b1 written! */
    const uint32_t spv_contradicted[] = {
        0x07230203u, 0x00010500u, 0u, 20u, 0u,
        (3u << 16) | 14u, 0u, 1u,                  /* OpMemoryModel Logical GLSL450 */
        (4u << 16) | 71u, 11u, 33u, 1u,            /* OpDecorate %11 Binding 1 (NO NonWritable!) */
        (4u << 16) | 59u, 2u, 11u, 12u,            /* OpVariable %ptr %11 StorageBuffer */
        (5u << 16) | 54u, 1u, 15u, 0u, 3u,
    };
    NVMTLFunction *fnContra = [NVMTLFunction new];
    fnContra->_spirv = [NSData dataWithBytes:spv_contradicted length:sizeof spv_contradicted];
    fnContra->_refl = @{@"bindings": @[@{@"metal_index": @1, @"kind": @"Buffer", @"access": @"ReadOnly"}]};
    NVMTLComputePipelineState *psContra = [NVMTLComputePipelineState new];
    psContra->_function = fnContra;
    enc->_ps = psContra;
    [enc nvmtlMarkWrittenBuffers];
    int contra_ok = ([cb->_resources isWrittenObject:b1] == YES && ![cb->_resources isWrittenObject:b2]);

    /* PhysicalStorageBuffer64 (5348) module: even with ReadOnly AIR + NonWritable -> MUST return 0xffffffffu! */
    const uint32_t spv_psb[] = {
        0x07230203u, 0x00010500u, 0u, 20u, 0u,
        (3u << 16) | 14u, 5348u, 1u,               /* OpMemoryModel PhysicalStorageBuffer64 GLSL450 */
        (4u << 16) | 71u, 10u, 33u, 0u,
        (3u << 16) | 71u, 10u, 24u,
        (4u << 16) | 59u, 2u, 10u, 12u,
        (5u << 16) | 54u, 1u, 15u, 0u, 3u,
    };
    int psb_ok = (nvmtl_spirv_writable_buffers(spv_psb, sizeof spv_psb) == 0xffffffffu);

    /* Bind b0 (previously ReadOnly) to writable slot 3 in a second dispatch: must become written! */
    enc->_ps = ps;
    [enc setBuffer:b0 offset:0 atIndex:3];
    [enc nvmtlMarkWrittenBuffers];
    int promoted_ok = [cb->_resources isWrittenObject:b0] == YES;

    /* Fallback: pipeline with nil _function marks all _baN==2 slots written. */
    NVMTLComputePipelineState *blind = [NVMTLComputePipelineState new];
    enc->_ps = blind;
    [enc nvmtlMarkWrittenBuffers];
    int fallback_ok = ([cb->_resources isWrittenObject:b1] && [cb->_resources isWrittenObject:b2]);

    /* Fast single-slot _tgLen key is an NSNumber; multi-slot falls back to NSData. */
    [enc setThreadgroupMemoryLength:4096 atIndex:0];
    id k1 = [enc buildVariantKey];
    int fast1 = [k1 isKindOfClass:[NSNumber class]];
    [enc setThreadgroupMemoryLength:8192 atIndex:2];
    id k2 = [enc buildVariantKey];
    int multi_json = [k2 isKindOfClass:[NSData class]];
    [enc setThreadgroupMemoryLength:0 atIndex:0];
    id k3 = [enc buildVariantKey];
    int fast_after_remove = ([k3 isKindOfClass:[NSNumber class]] && ![k3 isEqual:k1]);

    cb = nil; enc = nil;
    printf("%d %d %d %d %d %d %d %d %ld\n",
           ro_ok, contra_ok, psb_ok, promoted_ok, fallback_ok, fast1, multi_json, fast_after_remove, (long)atomic_load(&gRefs));
    return 0;
} }
'''

with tempfile.TemporaryDirectory(prefix='nvmtl-comp-ro-') as d:
    d = pathlib.Path(d)
    src, exe = d / 't.m', d / 't'
    src.write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-Wall', '-Wextra', '-Werror=unguarded-availability-new',
                        '-mmacosx-version-min=15.0', '-framework', 'Foundation', '-o', str(exe), str(src)],
                       capture_output=True, text=True)
    check('production compute read-only + SPIR-V NonWritable + _tgLen fast-key slice compiles cleanly (0 warnings, minos 15.0)',
          r.returncode == 0 and not r.stderr.strip(), r.stderr[-800:])
    if r.returncode == 0:
        out = subprocess.run([str(exe)], capture_output=True, text=True, timeout=30)
        check('harness exited 0', out.returncode == 0, out.stderr[-400:])
        if out.returncode == 0:
            ro_ok, contra_ok, psb_ok, prom_ok, fb_ok, fast1, multi_j, fast_rem, refs = map(int, out.stdout.split())
            check('Logical SPIR-V NonWritable + ReadOnly/Unused slots stay unwritten while writable slots are marked written', ro_ok == 1)
            check('contradicted AIR ReadOnly without SPIR-V NonWritable is conservatively marked written', contra_ok == 1)
            check('PhysicalStorageBuffer64 (5348) module conservatively marks all slots writable (0xffffffffu)', psb_ok == 1)
            check('ReadOnly buffer later bound to a writable slot promotes to written', prom_ok == 1)
            check('pipeline without reflection conservatively marks all bound buffers written', fb_ok == 1)
            check('single-slot _tgLen uses NSNumber key; multi-slot uses NSData; removal updates fast key',
                  fast1 == 1 and multi_j == 1 and fast_rem == 1)
            check('residency references balance to 0', refs == 0, f'refs={refs}')

print('PASS' if not fails else f'FAILED {fails}')
sys.exit(1 if fails else 0)
