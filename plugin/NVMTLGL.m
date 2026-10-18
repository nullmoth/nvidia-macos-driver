/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <CommonCrypto/CommonDigest.h>

#define NVGL_LLVM   "/System/Library/PrivateFrameworks/GPUCompiler.framework/Versions/32023/Libraries/libLLVM.dylib"
#define NVGL_PLUGIN "/System/Library/Extensions/AppleMetalOpenGLRenderer.bundle/Contents/PlugIns/libGLDPlugin32023.dylib"

typedef void *(*nvgl_gen_fn)(void *, const void *, size_t, void *, size_t, void **, size_t *);
static struct {
    void *(*ctx_create)(void); void (*ctx_dispose)(void *); void (*mod_dispose)(void *);
    void *(*write_bc)(void *); const char *(*buf_start)(void *); size_t (*buf_size)(void *); void (*buf_dispose)(void *);
    nvgl_gen_fn gen;
} gGL;
static NSString *gGLWhy;
static char gGLRetKey, gGLReflV, gGLReflF;

static BOOL nvmtl_gl_ready(void) {
    static dispatch_once_t once; static BOOL ready;
    dispatch_once(&once, ^{
        void *llvm = dlopen(NVGL_LLVM, RTLD_NOW | RTLD_LOCAL), *plug = llvm ? dlopen(NVGL_PLUGIN, RTLD_NOW | RTLD_LOCAL) : NULL;
        if (!llvm || !plug) { const char *e = dlerror(); gGLWhy = [NSString stringWithFormat:@"GL lowering unavailable: %s", e ?: "dlopen failed"]; nvlog("GL: %s", gGLWhy.UTF8String); return; }
        gGL.ctx_create = dlsym(llvm, "LLVMContextCreate");            gGL.ctx_dispose = dlsym(llvm, "LLVMContextDispose");
        gGL.mod_dispose = dlsym(llvm, "LLVMDisposeModule");           gGL.write_bc = dlsym(llvm, "LLVMWriteBitcodeToMemoryBuffer");
        gGL.buf_start = dlsym(llvm, "LLVMGetBufferStart");            gGL.buf_size = dlsym(llvm, "LLVMGetBufferSize");
        gGL.buf_dispose = dlsym(llvm, "LLVMDisposeMemoryBuffer");
        void (*init)(void) = dlsym(plug, "oglCodeGenServiceInitialize");
        gGL.gen = (nvgl_gen_fn)dlsym(plug, "oglCodeGenServiceGenerateIRWithPluginDataAndPluginReturnData");
        if (!gGL.ctx_create || !gGL.ctx_dispose || !gGL.mod_dispose || !gGL.write_bc || !gGL.buf_start || !gGL.buf_size
            || !gGL.buf_dispose || !init || !gGL.gen) { gGLWhy = @"GL lowering unavailable: a symbol is missing from libLLVM 32023 or libGLDPlugin32023"; nvlog("GL: %s", gGLWhy.UTF8String); return; }
        init();
        ready = YES;
        nvlog("GL: in-process lowering ready (libGLDPlugin32023 + GPUCompiler 32023 libLLVM)");
    });
    return ready;
}

static NSData *nvmtl_gl_lower(NSData *ir, NSData *pd, NSData **retData, NSString **why) {
    if (!nvmtl_gl_ready()) { *why = gGLWhy; return nil; }
    static NSObject *lock; static dispatch_once_t once; dispatch_once(&once, ^{ lock = [NSObject new]; });
    @synchronized(lock) {
        void *ctx = gGL.ctx_create(), *ret = NULL; size_t retn = 0;
        void *mod = gGL.gen(ctx, ir.bytes, ir.length, (void *)pd.bytes, pd.length, &ret, &retn);
        NSData *bc = nil;
        if (mod) {
            void *mb = gGL.write_bc(mod);
            if (mb) { bc = [NSData dataWithBytes:gGL.buf_start(mb) length:gGL.buf_size(mb)]; gGL.buf_dispose(mb); }
            gGL.mod_dispose(mod);
        }
        if (ret && retn) *retData = [NSData dataWithBytes:ret length:retn];
        free(ret);
        gGL.ctx_dispose(ctx);
        if (!bc) *why = mod ? @"the GL plugin's module would not serialize" : @"the GL plugin returned no module";
        return bc;
    }
}

static id<MTLFunction> nvmtl_gl_make_function(NSData *bc, NSString *name, NSString *stage, NSData *retData, NSError **err) {
    NSString *ll = nvmtl_air_to_text(bc);
    if (!ll) return nvmtl_fail(err, @"GL: the plugin's AIR would not decode");
    NVMTLLibrary *lib = [NVMTLLibrary new];
    lib->_fns = @{name: bc}; lib->_stages = @{name: stage}; lib->_airs = @{name: ll}; lib->_externs = nvmtl_externs_of_airs(lib->_airs);
    id<MTLFunction> mine = [lib newFunctionWithName:name];
    if (!mine) return nvmtl_fail(err, [NSString stringWithFormat:@"GL: the %@ entry %@ would not translate", stage, name]);
    if (retData) objc_setAssociatedObject(mine, &gGLRetKey, retData, OBJC_ASSOCIATION_RETAIN);
    return mine;
}

BOOL nvmtl_gl_is_apple_gl_function(id f) {
    if (!f || [f isKindOfClass:[NVMTLFunction class]]) return NO;
    SEL bt = sel_registerName("bitcodeType"), bd = sel_registerName("bitcodeDataInternal");
    if (![f respondsToSelector:bt] || ![f respondsToSelector:bd]) return NO;
    return ((unsigned char (*)(id, SEL))objc_msgSend)(f, bt) == 1;
}

id<MTLFunction> nvmtl_gl_function(id f, NSError **err) {
    if (!nvmtl_gl_is_apple_gl_function(f)) return f;
    if (getenv("NVMTL_NO_GL_BRIDGE")) return nvmtl_fail(err, @"GL: bridge OFF (NVMTL_NO_GL_BRIDGE) - Apple's GL function refused");
    NSData *ir = [NSData dataWithData:(NSData *)((id (*)(id, SEL))objc_msgSend)(f, sel_registerName("bitcodeDataInternal"))];
    id pdo = [f respondsToSelector:sel_registerName("pluginData")] ? ((id (*)(id, SEL))objc_msgSend)(f, sel_registerName("pluginData")) : nil;
    NSData *pd = [pdo isKindOfClass:[NSData class]] ? pdo : nil;
    NSUInteger ft = [(id<MTLFunction>)f functionType];
    NSString *stage = ft == MTLFunctionTypeVertex ? @"vertex" : ft == MTLFunctionTypeFragment ? @"fragment" : nil;
    if (!ir.length || !pd || !stage)
        return nvmtl_fail(err, [NSString stringWithFormat:@"GL: function not bridgeable (GL IR %lu B, pluginData %s, functionType %lu)",
                                (unsigned long)ir.length, pd ? "present" : "MISSING - never specialized", (unsigned long)ft]);
    unsigned char h[CC_SHA256_DIGEST_LENGTH]; CC_SHA256_CTX c; CC_SHA256_Init(&c);
    CC_SHA256_Update(&c, stage.UTF8String, (CC_LONG)stage.length); CC_SHA256_Update(&c, ir.bytes, (CC_LONG)ir.length);
    CC_SHA256_Update(&c, pd.bytes, (CC_LONG)pd.length); CC_SHA256_Final(h, &c);
    NSData *key = [NSData dataWithBytes:h length:sizeof h];
    static NSMutableDictionary *cache; static NSObject *lock; static dispatch_once_t once;
    dispatch_once(&once, ^{ cache = [NSMutableDictionary new]; lock = [NSObject new]; });
    @synchronized(lock) { id hit = cache[key]; if (hit) return hit; }
    NSString *why = nil;
    NSData *retData = nil;
    NSData *bc = nvmtl_gl_lower(ir, pd, &retData, &why);
    if (!bc) return nvmtl_fail(err, [NSString stringWithFormat:@"GL: lowering failed - %@", why]);
    id<MTLFunction> mine = nvmtl_gl_make_function(bc, @"__main", stage, retData, err);
    if (!mine) return nil;
    static int said;
    if (said++ < 16) nvlog("GL: %s lowered in process (GL IR %lu B + pluginData %lu B -> %lu B AIR) -> ours",
                           stage.UTF8String, (unsigned long)ir.length, (unsigned long)pd.length, (unsigned long)bc.length);
    @synchronized(lock) { if (cache.count >= 4096) [cache removeAllObjects]; cache[key] = mine; }
    return mine;
}

MTLRenderPipelineDescriptor *nvmtl_gl_rewrite_descriptor(MTLRenderPipelineDescriptor *d, NSError **err) {
    id v = d.vertexFunction, f = d.fragmentFunction;
    if (!nvmtl_gl_is_apple_gl_function(v) && !nvmtl_gl_is_apple_gl_function(f)) return d;
    id<MTLFunction> v2 = nvmtl_gl_function(v, err); if (v && !v2) return nil;
    id<MTLFunction> f2 = nvmtl_gl_function(f, err); if (f && !f2) return nil;
    MTLRenderPipelineDescriptor *c = [d copy]; c.vertexFunction = v2; c.fragmentFunction = f2;
    return c;
}

void nvmtl_gl_tag_reflection(id refl, MTLRenderPipelineDescriptor *d) {
    if (!refl) return;
    id v = d.vertexFunction ? objc_getAssociatedObject(d.vertexFunction, &gGLRetKey) : nil;
    id f = d.fragmentFunction ? objc_getAssociatedObject(d.fragmentFunction, &gGLRetKey) : nil;
    if (v) objc_setAssociatedObject(refl, &gGLReflV, v, OBJC_ASSOCIATION_RETAIN);
    if (f) objc_setAssociatedObject(refl, &gGLReflF, f, OBJC_ASSOCIATION_RETAIN);
}
@implementation NVMTLRenderPipelineReflection (NVMTLGL)
- (NSData *)vertexPluginReturnData { return objc_getAssociatedObject(self, &gGLReflV); }
- (NSData *)fragmentPluginReturnData { return objc_getAssociatedObject(self, &gGLReflF); }
- (uint64_t)usageFlags {
    static int said; if (!said++) nvlog("GL: reflection usageFlags answered 0 (bits unmapped; AMD answers 0 for the probe)");
    return 0;
}
- (NSDictionary *)performanceStatistics { return @{}; }
@end

@implementation NVMTLCommandBuffer (NVMTLGL)
- (void)commitAndHold { [self commit]; }
@end
@implementation NVMTLTexture (NVMTLGL)
- (void)waitUntilComplete {
    const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    NVMTLTexture *own = _parent ? _parent : self;
    while (__atomic_load_n(&own->_resRefs, __ATOMIC_ACQUIRE) > 0) {
        if (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0 > 10000000000ull) {
            nvlog("GL: waitUntilComplete gave up after 10 s - %d command buffer(s) still name this texture", __atomic_load_n(&own->_resRefs, __ATOMIC_ACQUIRE));
            return;
        }
        usleep(50);
    }
}
@end

static BOOL nvmtl_sel1_idle(int *r) { return __atomic_load_n(r, __ATOMIC_ACQUIRE) == 0; }
@implementation NVMTLTexture (NVMTLSel1)
- (BOOL)isComplete { NVMTLTexture *o = _parent ? _parent : self; return nvmtl_sel1_idle(&o->_resRefs); }
- (BOOL)isWriteComplete { NVMTLTexture *o = _parent ? _parent : self; return nvmtl_sel1_idle(&o->_resRefs); }
@end
@implementation NVMTLBuffer (NVMTLSel1)
- (BOOL)isComplete { return nvmtl_sel1_idle(&_resRefs); }
- (BOOL)isWriteComplete { return nvmtl_sel1_idle(&_resRefs); }
- (void)waitUntilComplete {
    const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    while (!nvmtl_sel1_idle(&_resRefs)) {
        if (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0 > 10000000000ull) {
            nvlog("waitUntilComplete (buffer) gave up after 10 s - %d command buffer(s) still name it", __atomic_load_n(&_resRefs, __ATOMIC_ACQUIRE));
            return;
        }
        usleep(50);
    }
}
@end
@implementation NVMTLRenderCommandEncoder (NVMTLSel1)
- (void)setLineWidth:(float)w { nvmtl_vk_cmd_set_line_width(&_cb->_c, w); }
- (void)setDepthCleared {}
- (void)setStencilCleared {}
// Apple's GL renderer turns primitive restart off (GL's default) through this call, at the start of each render pass.
// Restart is per-draw dynamic state here already (strips only; Vulkan list topologies never restart), so this records
// GL's choice for the strips. Vulkan restarts only at the all-ones index of the bound index type.
- (void)setPrimitiveRestartEnabled:(BOOL)enabled index:(NSUInteger)index {
    _cb->_c.restart_off = !enabled;
    if (enabled && index != 0xffffu && index != 0xffffffffu) {
        static int said; if (!said++) nvlog("GL: primitive restart index 0x%lx requested - Vulkan restarts only at the all-ones index", (unsigned long)index);
    }
}
// Apple's GL renderer sends this (1 = GL's last-vertex convention, 0 = first) at the start of each render pass, once
// its device profile says the encoder takes it; see nvmtl_gl_caps_enable below.
- (void)setProvokingVertexMode:(NSUInteger)mode {
    if (mode > 1) { static int said; if (!said++) nvlog("GL: setProvokingVertexMode:%lu is not first (0) or last (1) - first used", (unsigned long)mode); }
    nvmtl_vk_cmd_set_provoking_last(&_cb->_c, mode == 1);
}
@end

// Apple's GL renderer (GLDDeviceRec::initWithDisplayMask) takes a device profile by vendor name; "NVIDIA" gets the
// table written for the Kepler Macs, whose first byte lacks 0x8: "the render encoder takes setProvokingVertexMode:".
// Without that bit the renderer emulates GL's last-vertex convention itself, rewriting the index buffer of every
// indexed draw with its fixIndexBuffer compute kernel: a compute dispatch (and its descriptor sets) per draw call.
// Once the Vulkan device can make the last vertex provoke, the bit is set in that table, in this process only. The
// renderer copies the table after it has created its Metal device (this plugin's device init); a renderer loaded
// later is caught by the dyld callback. A table that does not hold the bytes it held on macOS 15.8.1 is left alone.
#include <dlfcn.h>
#include <mach-o/dyld.h>
#include <mach-o/loader.h>
#include <mach-o/nlist.h>
#include <mach/mach.h>
static const char kGLRendererSuffix[] = "/AppleMetalOpenGLRenderer.bundle/Contents/MacOS/AppleMetalOpenGLRenderer";
static const char kGLNvidiaCaps[] = "__ZZN12GLDDeviceRec19initWithDisplayMaskEjE6nvidia";
static const uint8_t kGLNvidiaCaps1581[20] = { 0x40, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x42, 0, 0, 0x80, 0x41, 0, 0, 0, 0 };
enum { NVMTL_GL_CAP_PROVOKING_VERTEX = 0x08 };

static uint8_t *nvmtl_gl_local_symbol(const struct mach_header_64 *mh, const char *name) {
    const struct load_command *lc = (const struct load_command *)(mh + 1);
    const struct segment_command_64 *text = NULL, *linkedit = NULL; const struct symtab_command *st = NULL;
    for (uint32_t i = 0; i < mh->ncmds; i++, lc = (const struct load_command *)((const char *)lc + lc->cmdsize)) {
        if (lc->cmd == LC_SEGMENT_64) {
            const struct segment_command_64 *sg = (const struct segment_command_64 *)lc;
            if (!strcmp(sg->segname, SEG_TEXT)) text = sg;
            else if (!strcmp(sg->segname, SEG_LINKEDIT)) linkedit = sg;
        } else if (lc->cmd == LC_SYMTAB) st = (const struct symtab_command *)lc;
    }
    if (!text || !linkedit || !st) return NULL;
    const intptr_t slide = (intptr_t)mh - (intptr_t)text->vmaddr;
    const char *base = (const char *)(slide + linkedit->vmaddr - linkedit->fileoff);
    const struct nlist_64 *sym = (const struct nlist_64 *)(base + st->symoff);
    const char *strs = base + st->stroff;
    for (uint32_t i = 0; i < st->nsyms; i++)
        if (!(sym[i].n_type & N_STAB) && sym[i].n_value && !strcmp(strs + sym[i].n_un.n_strx, name))
            return (uint8_t *)(sym[i].n_value + slide);
    return NULL;
}
static void nvmtl_gl_caps_patch(const struct mach_header *mh) {
    Dl_info info; const char *path = dladdr(mh, &info) ? info.dli_fname : NULL;
    size_t n = path ? strlen(path) : 0, k = sizeof kGLRendererSuffix - 1;
    if (n < k || strcmp(path + n - k, kGLRendererSuffix) || mh->magic != MH_MAGIC_64) return;
    uint8_t *caps = nvmtl_gl_local_symbol((const struct mach_header_64 *)mh, kGLNvidiaCaps);
    if (!caps) { nvlog("GL: renderer has no %s - provoking vertex left to its index rewrite", kGLNvidiaCaps); return; }
    if (caps[0] & NVMTL_GL_CAP_PROVOKING_VERTEX) return;
    if (memcmp(caps, kGLNvidiaCaps1581, sizeof kGLNvidiaCaps1581)) {
        nvlog("GL: the renderer's NVIDIA profile is not the macOS 15.8.1 one - provoking vertex left to its index rewrite"); return;
    }
    const vm_address_t page = (vm_address_t)caps & ~(vm_address_t)(vm_page_size - 1);
    kern_return_t kr = vm_protect(mach_task_self(), page, vm_page_size, FALSE, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY);
    if (kr != KERN_SUCCESS) { nvlog("GL: profile page not writable (%d) - provoking vertex left to its index rewrite", kr); return; }
    caps[0] |= NVMTL_GL_CAP_PROVOKING_VERTEX;
    vm_protect(mach_task_self(), page, vm_page_size, FALSE, VM_PROT_READ);   // data only on this page (__TEXT,__const)
}
static void nvmtl_gl_image_added(const struct mach_header *mh, intptr_t slide) { (void)slide; nvmtl_gl_caps_patch(mh); }
void nvmtl_gl_caps_enable(void) {
    if (!nvmtl_vk_have_provoking_last()) return;
    static dispatch_once_t once;   // the callback also runs at once for every image already loaded
    dispatch_once(&once, ^{ _dyld_register_func_for_add_image(nvmtl_gl_image_added); });
}
@implementation NVMTLFunction (NVMTLSel1)
- (NSArray *)importedLibraries { return nil; }
@end

@implementation NVMTLRenderCommandEncoder (NVMTLSel2)
- (void)setTileBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i {}
- (void)setTileBufferOffset:(NSUInteger)o atIndex:(NSUInteger)i {}
- (void)setTileBytes:(const void *)p length:(NSUInteger)n atIndex:(NSUInteger)i {}
- (void)setTileTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {}
- (void)setTileSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i {}
- (void)setTileSamplerState:(id<MTLSamplerState>)s lodMinClamp:(float)a lodMaxClamp:(float)b atIndex:(NSUInteger)i {}
- (void)setTileAccelerationStructure:(id)a atBufferIndex:(NSUInteger)i {}
- (void)setTileVisibleFunctionTable:(id)t atBufferIndex:(NSUInteger)i {}
- (void)setTileIntersectionFunctionTable:(id)t atBufferIndex:(NSUInteger)i {}
- (void)dispatchThreadsPerTile:(MTLSize)s {}
- (void)setThreadgroupMemoryLength:(NSUInteger)l offset:(NSUInteger)o atIndex:(NSUInteger)i {}
- (void)setDepthTestMinBound:(float)lo maxBound:(float)hi {}
@end
@implementation NVMTLComputeCommandEncoder (NVMTLSel2)
- (void)setImageblockWidth:(NSUInteger)w height:(NSUInteger)h {}
@end

@implementation NVMTLFunction (NVMTLGL)
- (id<MTLFunction>)newFunctionWithPluginData:(NSData *)pd bitcodeType:(unsigned char)bt {
    NSError *err = nil;
    NSData *air = [_lib isKindOfClass:[NVMTLLibrary class]] ? ((NVMTLLibrary *)_lib)->_fns[_fname] : nil;
    if (getenv("NVMTL_NO_GL_BRIDGE")) { nvlog("GL: bridge OFF - newFunctionWithPluginData refused (%s)", _fname.UTF8String); return nil; }
    if (bt != 2 || ![pd isKindOfClass:[NSData class]] || air.length < 20 || *(const uint32_t *)air.bytes == 0x07230203u) {
        nvlog("GL: newFunctionWithPluginData: %s not bridgeable (bitcodeType %u, pluginData %lu B, AIR %lu B) - refused",
              _fname.UTF8String, bt, (unsigned long)[pd length], (unsigned long)air.length);
        return nil;
    }
    unsigned char h[CC_SHA256_DIGEST_LENGTH]; CC_SHA256_CTX c; CC_SHA256_Init(&c);
    CC_SHA256_Update(&c, "cl", 2); CC_SHA256_Update(&c, air.bytes, (CC_LONG)air.length); CC_SHA256_Update(&c, pd.bytes, (CC_LONG)pd.length); CC_SHA256_Final(h, &c);
    NSData *key = [NSData dataWithBytes:h length:sizeof h];
    static NSMutableDictionary *cache; static NSObject *lock; static dispatch_once_t once;
    dispatch_once(&once, ^{ cache = [NSMutableDictionary new]; lock = [NSObject new]; });
    @synchronized(lock) { id hit = cache[key]; if (hit) return hit; }
    NSString *why = nil; NSData *retData = nil;
    NSData *bc = nvmtl_gl_lower(air, pd, &retData, &why);
    if (!bc) { nvlog("GL: OpenCL kernel %s lowering failed - %s", _fname.UTF8String, why.UTF8String); return nil; }
    id<MTLFunction> mine = nvmtl_gl_make_function(bc, _fname, @"kernel", retData, &err);
    if (!mine) { nvlog("GL: OpenCL kernel %s: %s", _fname.UTF8String, err.localizedDescription.UTF8String); return nil; }
    static int said; if (said++ < 16) nvlog("GL: OpenCL kernel %s lowered in process (AIR %lu B + pluginData %lu B -> %lu B AIR) -> ours",
                                            _fname.UTF8String, (unsigned long)air.length, (unsigned long)pd.length, (unsigned long)bc.length);
    @synchronized(lock) { if (cache.count >= 4096) [cache removeAllObjects]; cache[key] = mine; }
    return mine;
}
@end

// Apple's GL/OpenCL-on-Metal layer reads reflection objects through MTLArgument/MTLType getters that change between
// macOS builds. Geekbench's OpenCL build aborted on an unimplemented one three times (10-07 twice, then a 1.2.0 RTX 3060
// report on 10-09), each time after the previous getters were added. The build that worked had a forwarding net that
// answered ZERO to every getter it did not implement; this is that net. respondsToSelector: still says NO, so code
// that asks first is unchanged; a direct send gets 0 / nil / NO and the selector is logged, so it can be implemented.
static NSMethodSignature *nvgl_zero_sig(SEL s) {
    char types[48] = "Q@:";  // integer-class return: 0 in rax answers NSUInteger, BOOL, enums and nil alike
    int args = 0;
    for (const char *c = sel_getName(s); *c; c++) args += *c == ':';
    for (int i = 0; i < args && i < 16; i++) strcat(types, "Q");
    return [NSMethodSignature signatureWithObjCTypes:types];
}
static void nvgl_zero(NSInvocation *inv, const char *cls) {
    nvlog("GL: %s asked -%s (answered 0)", cls, sel_getName(inv.selector));
    char zero[64] = {0};
    NSUInteger len = inv.methodSignature.methodReturnLength;
    if (len && len <= sizeof zero) [inv setReturnValue:zero];
}
#define NVGL_ZERO_NET(cls) \
- (NSMethodSignature *)methodSignatureForSelector:(SEL)s { return [super methodSignatureForSelector:s] ?: nvgl_zero_sig(s); } \
- (void)forwardInvocation:(NSInvocation *)inv { nvgl_zero(inv, #cls); }

@interface NVMTLGLPointerType : MTLPointerType {
  @public MTLDataType _elem; MTLBindingAccess _access; NSUInteger _size, _align; } @end
@implementation NVMTLGLPointerType
NVGL_ZERO_NET(NVMTLGLPointerType)
- (MTLDataType)dataType { return MTLDataTypePointer; }
- (MTLDataType)elementType { return _elem; }
- (MTLBindingAccess)access { return _access; }
- (NSUInteger)alignment { return _align; }
- (NSUInteger)dataSize { return _size; }
- (BOOL)elementIsArgumentBuffer { return NO; }
- (BOOL)elementIsIndirectArgumentBuffer { return NO; }
- (BOOL)isConstantBuffer { return NO; }
- (id)elementStructType { return nil; }
- (id)elementArrayType { return nil; }
- (id)elementTypeDescription { return nil; }
- (NSArray *)members { return nil; }
- (NSUInteger)arrayLength { return 0; }
@end
@interface NVMTLGLBinding : NSObject <MTLBufferBinding> { @public NSString *_name; MTLBindingType _type; MTLBindingAccess _access;
  NSUInteger _index, _size, _align; MTLDataType _dtype; } @end
@implementation NVMTLGLBinding
- (NSString *)name { return _name; }
- (MTLBindingType)type { return _type; }
- (MTLBindingAccess)access { return _access; }
- (NSUInteger)index { return _index; }
- (BOOL)isUsed { return YES; }
- (BOOL)used { return YES; }
- (BOOL)isArgument { return YES; }
- (BOOL)argument { return YES; }
- (NSUInteger)bufferAlignment { return _align; }
- (NSUInteger)bufferDataSize { return _size; }
- (MTLDataType)bufferDataType { return _dtype; }
- (MTLStructType *)bufferStructType { return nil; }
- (MTLPointerType *)bufferPointerType {
    NVMTLGLPointerType *p = [NVMTLGLPointerType new]; p->_elem = _dtype; p->_access = _access; p->_size = _size; p->_align = _align;
    return (MTLPointerType *)p;
}
- (id)dataTypeDescription { return _type == MTLBindingTypeBuffer ? (id)[self bufferPointerType] : nil; }
// (Screen Sharing "Both Displays" = tiled garbage): these getters answered real-looking values (2D, length 1,
// stride = size, Pointer/Texture, the binding index) and Apple's GL/OpenCL layer laid out its clImage*Scale kernel arguments
// from them. The working build never implemented them: its forwarding net answered ZERO. They stay implemented (no abort)
// and answer exactly that zero.
// 10-07 (a user's crash report): Geekbench's OpenCL build aborted in -[NVMTLGLBinding doesNotRecognizeSelector:]
// under GLDComputeProgramRec::buildComputeProgram. Apple's GL-on-Metal layer reads reflection through the older MTLArgument
// / MTLType getters; measured on its binary (selector refs): arrayLength, textureType, alignment, dataSize, members,
// elementTypeDescription. A buffer binding answers each with what MTLArgument gives for a buffer.
- (NSUInteger)arrayLength { return 0; }
- (MTLTextureType)textureType { return (MTLTextureType)0; }
- (MTLDataType)textureDataType { return MTLDataTypeNone; }
- (BOOL)isDepthTexture { return NO; }
- (NSUInteger)alignment { return 0; }
- (NSUInteger)dataSize { return 0; }
- (NSArray *)members { return nil; }
- (id)elementTypeDescription { return nil; }
- (BOOL)isActive { return NO; }
- (BOOL)active { return NO; }
- (NSUInteger)threadgroupMemoryAlignment { return 0; }
- (NSUInteger)threadgroupMemoryDataSize { return 0; }
// 10-07 (RTX 3050 on driver 1.0.3): Geekbench's OpenCL build still aborted here. The renderer's selector
// list (AppleMetalOpenGLRenderer, macOS 15.8.1) also names dataType, offset, stride and argumentIndex, which a buffer
// binding did not answer. A buffer reads as a pointer at offset 0, one element of its own size, at its own index.
- (MTLDataType)dataType { return MTLDataTypeNone; }
- (NSUInteger)offset { return 0; }
- (NSUInteger)stride { return 0; }
- (NSUInteger)argumentIndex { return 0; }
- (MTLDataType)elementType { return MTLDataTypeNone; }
- (id)elementStructType { return nil; }
- (id)elementArrayType { return nil; }
- (id)elementPointerType { return nil; }
NVGL_ZERO_NET(NVMTLGLBinding)
- (NSString *)description { return [NSString stringWithFormat:@"<NVMTLGLBinding %@ type %ld access %ld index %lu size %lu>",
                                     _name, (long)_type, (long)_access, (unsigned long)_index, (unsigned long)_size]; }
@end
@interface NVMTLGLTexBinding : NSObject <MTLTextureBinding> { @public NSString *_name; MTLBindingAccess _access; NSUInteger _index;
  MTLTextureType _ttype; } @end
@implementation NVMTLGLTexBinding
- (NSString *)name { return _name; }
- (MTLBindingType)type { return MTLBindingTypeTexture; }
- (MTLBindingAccess)access { return _access; }
- (NSUInteger)index { return _index; }
- (BOOL)isUsed { return YES; }
- (BOOL)used { return YES; }
- (BOOL)isArgument { return YES; }
- (BOOL)argument { return YES; }
- (MTLTextureType)textureType { return _ttype; }
- (MTLDataType)textureDataType { return MTLDataTypeNone; }
- (BOOL)isDepthTexture { return NO; }
- (BOOL)depthTexture { return NO; }
- (NSUInteger)arrayLength { return 1; }
// same older getters as NVMTLGLBinding, with what MTLArgument gives for a texture
- (NSUInteger)alignment { return 0; }
- (NSUInteger)dataSize { return 0; }
- (NSArray *)members { return nil; }
- (id)elementTypeDescription { return nil; }
- (id)dataTypeDescription { return nil; }
- (BOOL)isActive { return NO; }
- (BOOL)active { return NO; }
- (NSUInteger)bufferAlignment { return 0; }
- (NSUInteger)bufferDataSize { return 0; }
- (MTLDataType)bufferDataType { return MTLDataTypeNone; }
- (NSUInteger)threadgroupMemoryAlignment { return 0; }
- (NSUInteger)threadgroupMemoryDataSize { return 0; }
// same four getters as NVMTLGLBinding: a texture is MTLDataTypeTexture at offset 0, at its own index
- (MTLDataType)dataType { return MTLDataTypeNone; }
- (NSUInteger)offset { return 0; }
- (NSUInteger)stride { return 0; }
- (NSUInteger)argumentIndex { return 0; }
NVGL_ZERO_NET(NVMTLGLTexBinding)
- (NSString *)description { return [NSString stringWithFormat:@"<NVMTLGLTexBinding %@ access %ld index %lu textureType %lu>", _name, (long)_access, (unsigned long)_index, (unsigned long)_ttype]; }
@end
@interface NVMTLGLFunctionReflection : NSObject { @public NSArray *_args; } @end
@implementation NVMTLGLFunctionReflection
- (NSArray *)arguments { return _args; }
- (NSArray *)bindings { return _args; }
- (NSArray *)builtInArguments { return @[]; }
- (NSArray *)tags { return @[]; }
- (NSData *)pluginReturnData { return nil; }
- (NSUInteger)primitiveKind { return 0; }
NVGL_ZERO_NET(NVMTLGLFunctionReflection)
@end

static NSArray *nvmtl_gl_cl_bindings(NSString *ll) {
    NSMutableDictionary<NSString *, NSString *> *md = [NSMutableDictionary new]; NSString *kernel = nil;
    for (NSString *l in [ll componentsSeparatedByString:@"\n"]) {
        if ([l hasPrefix:@"!air.kernel = !{!"]) { kernel = [[l substringFromIndex:17] componentsSeparatedByCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@",}"]][0]; continue; }
        NSRange eq = [l rangeOfString:@" = "];
        if (![l hasPrefix:@"!"] || eq.location == NSNotFound) continue;
        NSString *k = [l substringWithRange:NSMakeRange(1, eq.location - 1)], *body = [l substringFromIndex:eq.location + 3];
        if ([body hasPrefix:@"distinct "]) body = [body substringFromIndex:9];
        if ([body hasPrefix:@"!{"] && [body hasSuffix:@"}"]) md[k] = [body substringWithRange:NSMakeRange(2, body.length - 3)];
    }
    if (!kernel || !md[kernel]) return nil;
    NSRegularExpression *ref = [NSRegularExpression regularExpressionWithPattern:@"^!(\\d+)$" options:0 error:nil];
    NSString *argList = nil;
    for (NSString *op in [md[kernel] componentsSeparatedByString:@", "]) {
        if (![ref firstMatchInString:op options:0 range:NSMakeRange(0, op.length)]) continue;
        NSString *b = md[[op substringFromIndex:1]]; BOOL allRefs = b.length > 0;
        for (NSString *x in [b componentsSeparatedByString:@", "]) if (![ref firstMatchInString:x options:0 range:NSMakeRange(0, x.length)]) { allRefs = NO; break; }
        if (allRefs) argList = b;
    }
    NSMutableArray *out = [NSMutableArray new];
    NSInteger (^num)(NSString *, NSString *) = ^NSInteger(NSString *b, NSString *key) {
        NSRange r = [b rangeOfString:[NSString stringWithFormat:@"!\"%@\", i32 ", key]]; if (r.location == NSNotFound) return -1;
        return [[b substringFromIndex:NSMaxRange(r)] integerValue]; };
    NSString *(^str)(NSString *, NSString *) = ^NSString *(NSString *b, NSString *key) {
        NSRange r = [b rangeOfString:[NSString stringWithFormat:@"!\"%@\", !\"", key]]; if (r.location == NSNotFound) return nil;
        NSString *t = [b substringFromIndex:NSMaxRange(r)]; NSRange q = [t rangeOfString:@"\""]; return q.location == NSNotFound ? nil : [t substringToIndex:q.location]; };
    for (NSString *a in [argList componentsSeparatedByString:@", "]) {
        NSString *b = md[[a substringFromIndex:1]]; if (!b) continue;
        if (getenv("NVMTL_CL_REFL_TRACE")) nvlog("GL: OpenCL argument metadata: %s", b.UTF8String);
        BOOL buf = [b rangeOfString:@"!\"air.buffer\""].location != NSNotFound, cst = [b rangeOfString:@"!\"air.constant\""].location != NSNotFound;
        if ([b rangeOfString:@"!\"air.texture\""].location != NSNotFound) {
            NVMTLGLTexBinding *t = [NVMTLGLTexBinding new]; t->_name = str(b, @"air.arg_name") ?: @"";
            NSInteger li = num(b, @"air.location_index"); t->_index = li >= 0 ? (NSUInteger)li : 0;
            t->_access = [b rangeOfString:@"!\"air.read_write\""].location != NSNotFound ? MTLBindingAccessReadWrite
                       : [b rangeOfString:@"!\"air.write\""].location != NSNotFound ? MTLBindingAccessWriteOnly : MTLBindingAccessReadOnly;
            NSString *tn = str(b, @"air.arg_type_name") ?: @"";
            t->_ttype = [tn hasPrefix:@"texture1d_array"] ? MTLTextureType1DArray : [tn hasPrefix:@"texture1d"] ? MTLTextureType1D
                      : [tn hasPrefix:@"texture2d_array"] ? MTLTextureType2DArray : [tn hasPrefix:@"texture3d"] ? MTLTextureType3D
                      : [tn hasPrefix:@"texture_buffer"] ? MTLTextureTypeTextureBuffer : MTLTextureType2D;
            [out addObject:t]; continue;
        }
        if (!buf && !cst) { nvlog("GL: OpenCL argument not reflected: %s", b.UTF8String); continue; }
        NVMTLGLBinding *x = [NVMTLGLBinding new];
        // an OpenCL __local pointer is an air.buffer in address space 3: OpenCL sets it with a size and no cl_mem, and
        // refused it as a buffer (CL_INVALID_ARG_SIZE) until it was reflected as threadgroup memory
        x->_name = str(b, @"air.arg_name") ?: @"";
        x->_type = cst ? (MTLBindingType)22 : num(b, @"air.address_space") == 3 ? MTLBindingTypeThreadgroupMemory : MTLBindingTypeBuffer;
        x->_access = [b rangeOfString:@"!\"air.read_write\""].location != NSNotFound ? MTLBindingAccessReadWrite
                   : [b rangeOfString:@"!\"air.write\""].location != NSNotFound ? MTLBindingAccessWriteOnly : MTLBindingAccessReadOnly;
        NSInteger li = num(b, @"air.location_index"); x->_index = li >= 0 ? (NSUInteger)li : 0;
        NSInteger sz = num(b, @"air.arg_type_size"), al = num(b, @"air.arg_type_align_size");
        x->_size = sz > 0 ? (NSUInteger)sz : 0; x->_align = al > 0 ? (NSUInteger)al : 0;
        x->_dtype = nvmtl_datatype_for_air_name(str(b, @"air.arg_type_name"));
        [out addObject:x];
    }
    return out;
}
@implementation NVMTLFunction (NVMTLGLRefl)
- (id)reflectionWithOptions:(NSUInteger)o {
    // Apple's OpenCL builds its argument table from this reflection (GLDComputeProgramRec::buildComputeProgram reads
    // reflectionWithOptions: -> arguments). It answered nil whenever the function already had SPIR-V - which is always,
    // since library entries translate when they are made - or had no air.constant argument, so OpenCL saw 0 arguments:
    // every clSetKernelArg returned -49 and Geekbench's OpenCL run stopped at its first workload.
    if (getenv("NVMTL_CL_REFL_TRACE")) nvlog("GL: reflectionWithOptions: %s options 0x%lx spirv %lu B air %lu B kernel-list %d", _fname.UTF8String, (unsigned long)o, (unsigned long)_spirv.length, (unsigned long)_air.length, [_air containsString:@"!air.kernel"]);
    // A kernel is translated when its library entry is made, so _spirv is always set by now; only the AIR decides.
    if (!_air.length || ![_air containsString:@"!air.kernel"]) return nil;
    NSArray *args = nvmtl_gl_cl_bindings(_air);
    if (!args) { nvlog("GL: reflectionWithOptions: %s - no air.kernel argument list in its AIR", _fname.UTF8String); return nil; }
    NVMTLGLFunctionReflection *r = [NVMTLGLFunctionReflection new]; r->_args = args;
    static int said; if (said++ < 8) nvlog("GL: OpenCL kernel %s reflection: %s", _fname.UTF8String, [[args description] stringByReplacingOccurrencesOfString:@"\n" withString:@" "].UTF8String);
    return r;
}
@end
static char gGLReflC;
void nvmtl_gl_tag_compute_reflection(id r, id fn) {
    id d = fn ? objc_getAssociatedObject(fn, &gGLRetKey) : nil;
    if (r && d) objc_setAssociatedObject(r, &gGLReflC, d, OBJC_ASSOCIATION_RETAIN);
}
@implementation NVMTLComputePipelineReflection (NVMTLGL)
- (NSData *)pluginReturnData { return objc_getAssociatedObject(self, &gGLReflC); }
@end
