/*
 * NVMTLSass.m - compute kernels compiled ahead of time by NVIDIA's own compiler (AIR -> NVVM -> PTX ->
 * SASS) and dispatched directly through NVK, beside the AIR -> SPIR-V -> NAK path.
 *
 * Some kernels cannot be expressed in SPIR-V at all (MPS's fused depthwise convolution on macOS 26 is
 * refused by the translator, which stops every MPS graph that uses it), and some translate but run
 * slower or wrong. For those, the bundle carries machine code built by NVIDIA's compiler, keyed by the
 * exact program it was built from.
 *
 * A kernel runs this way only when all of these hold; otherwise it takes the existing path unchanged:
 *  - the GPU is a GeForce RTX 50 series card (the shipped machine code is sm_120, and NVK refuses it
 *    on any other architecture as a second check);
 *  - the loaded NVK exports the raw dispatch and descriptor-handle entry points;
 *  - the bundle carries an artifact for exactly this program: the library bytes or the linked AIR, the
 *    function name, the linked bodies and the function constants all match, and the machine code
 *    matches its own checksum;
 *  - NVMTL_DIRECT_SASS is not "0" (environment or nvrm610.conf).
 * Nothing here compiles at run time and nothing runs an interpreter: a miss is the existing path.
 *
 * When a kernel has both machine code and a Vulkan pipeline, the machine code runs and the Vulkan
 * pipeline stays as the fallback for a dispatch the launcher refuses (a binding it cannot resolve).
 */

static BOOL nvmtl_sass_error(NSError **error, NSInteger code, NSString *message)
{
    if (error)
        *error = [NSError errorWithDomain:@"NVMTLDomain" code:code
                                 userInfo:@{ NSLocalizedDescriptionKey: message ?: @"" }];
    return NO;
}

/* The dummy buffer's address: a table slot nothing is bound to must read 256 bytes of zeroes, never fault. */
static uint64_t nvmtl_vk_dummy_address(void) { return nvmtl_vk_buffer_address(&g_dummyBuf); }

#import "NVMTLSassLaunch.m"
#import "NVMTLSassResolve.m"
#include <math.h>
#include "nvmtl_sass_gate.h"

/* The compiler build the shipped artifacts came from: the SHA-256 over the compiler front end's own
 * files. A record built by any other compiler is refused, and the cache directory is named by it. */
#define NVMTL_SASS_COMPILER_ID @"ccc3ebcdcc4f5414d79c3341f3093115545b12d0d4b56039f45e7d55fb807e37"
#define NVMTL_SASS_TARGET      @"sm_120-ptx87-cbuf0-0x380-v1"
#define NVMTL_SASS_QUALIFIED   @"sass-launchrec-v3"
#define NVMTL_SASS_RECORD_MAX  (1u << 20)
#define NVMTL_SASS_CODE_MAX    (64u << 20)

@interface NVMTLSassKernel : NSObject {
  @public NSDictionary *_record; NSData *_code; NSString *_name;
}
@end
@implementation NVMTLSassKernel
@end

static NSString *nvmtl_sass_digest(NSData *data)
{
    NSData *hash = nvmtl_lib_sha256(data ?: [NSData data]);
    NSMutableString *text = [NSMutableString stringWithCapacity:64];
    const uint8_t *bytes = hash.bytes;
    for (NSUInteger i = 0; i < hash.length; i++) [text appendFormat:@"%02x", bytes[i]];
    return text;
}

static NSData *nvmtl_sass_read(NSString *path, NSUInteger limit)
{
    NSDictionary *attributes = [NSFileManager.defaultManager attributesOfItemAtPath:path error:NULL];
    if (!attributes || [attributes[NSFileSize] unsignedLongLongValue] > limit) return nil;
    NSData *data = [NSData dataWithContentsOfFile:path];
    return data.length <= limit ? data : nil;
}

static BOOL nvmtl_sass_hex64(id value)
{
    if (![value isKindOfClass:NSString.class] || [value length] != 64) return NO;
    NSCharacterSet *bad = [[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdef"] invertedSet];
    return [value rangeOfCharacterFromSet:bad].location == NSNotFound;
}

static BOOL nvmtl_sass_uint32(id value, uint32_t *out)
{
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) != CFNumberGetTypeID()) return NO;
    double number = [value doubleValue];
    if (!isfinite(number) || number < 0 || number > UINT32_MAX || number != (double)(uint32_t)number) return NO;
    *out = (uint32_t)number;
    return YES;
}


static NSString *nvmtl_sass_cache_root(void)
{
    const char *env = getenv("NVMTL_SASS_CACHE");
    if (env && *env) return @(env);
    NSBundle *bundle = [NSBundle bundleForClass:NVMTLSassKernel.class];
    return [bundle.resourcePath stringByAppendingPathComponent:@"sass"];
}

typedef int (*nvmtl_sass_dispatch_v3_fn)(void *, const void *, uint32_t, uint32_t, uint32_t, uint32_t,
                                         const uint64_t *, uint32_t, const uint32_t *, const uint32_t *,
                                         const void *);
static nvmtl_sass_dispatch_v3_fn g_sass_dispatch;
static NSString *g_sass_dir;

/* Decided once per process, after Vulkan is up, and said once in the log with the reason. */
static BOOL nvmtl_sass_enabled(void)
{
    static int on = -1;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *why = NULL;
        const char *knob = getenv("NVMTL_DIRECT_SASS");
        if (knob && !strcmp(knob, "0")) why = "turned off by NVMTL_DIRECT_SASS=0";
        else if (nvmtl_vk_init() || !g_lib) why = "Vulkan is not up";
        else if (!nvmtl_sass_device_name_ok(g_name)) why = "this GPU is not a GeForce RTX 50 series card";
        else {
            g_sass_dispatch = (nvmtl_sass_dispatch_v3_fn)dlsym(g_lib, "nvmtl_nvk_dispatch_sass_v3");
            if (!g_sass_dispatch || !nvmtl_sass_image_fn() || !nvmtl_sass_sampler_fn())
                why = "the loaded NVK has no direct dispatch entry points";
            else {
                NSString *dir = [nvmtl_sass_cache_root() stringByAppendingPathComponent:NVMTL_SASS_COMPILER_ID];
                BOOL isDir = NO;
                if (![NSFileManager.defaultManager fileExistsAtPath:dir isDirectory:&isDir] || !isDir)
                    why = "no compiled kernels are installed";
                else g_sass_dir = dir;
            }
        }
        on = why ? 0 : 1;
        if (why) nvlog("direct machine code: off (%s)", why);
        else nvlog("direct machine code: on for \"%s\" from %s", g_name, g_sass_dir.UTF8String);
    });
    return on == 1;
}

/* Whether the compute encoder keeps its per-index texture and sampler tables for the launcher. */
BOOL nvmtl_sass_tables_wanted(void)
{
    return nvmtl_sass_enabled();
}

/* The record must describe THIS program, built by THIS compiler for THIS target; everything it claims
 * is checked here, and the reader below refuses any layout the launcher cannot honour, so the encoder
 * never has to decide again. */
static NVMTLSassKernel *nvmtl_sass_load_record(NSDictionary *record, NSString *directory, NSString *fname,
                                               NSString *libraryHash, NSString *source)
{
    uint32_t schema = 0;
    if (![record isKindOfClass:NSDictionary.class] || ![record[@"kernel"] isEqual:fname] ||
        ![record[@"library_sha256"] isEqual:libraryHash] || ![record[@"compiler_sha256"] isEqual:NVMTL_SASS_COMPILER_ID] ||
        ![record[@"target"] isEqual:NVMTL_SASS_TARGET] || ![record[@"qualification"] isEqual:NVMTL_SASS_QUALIFIED] ||
        ![record[@"source"] isEqual:source] || !nvmtl_sass_uint32(record[@"schema"], &schema) || schema != 3) return nil;
    NSString *codeHash = record[@"sass_sha256"];
    if (!nvmtl_sass_hex64(codeHash)) return nil;
    NSData *code = nvmtl_sass_read([directory stringByAppendingPathComponent:[codeHash stringByAppendingString:@".bin"]],
                                   NVMTL_SASS_CODE_MAX);
    if (!code.length || code.length % 16 || ![nvmtl_sass_digest(code) isEqual:codeHash]) {
        nvlog("direct machine code: \"%s\" refused - its machine code does not match its checksum", fname.UTF8String);
        return nil;
    }
    NSMutableArray *keep = [NSMutableArray array];
    struct nvmtl_sass_program probe;
    NSError *error = nil;
    if (!nvmtl_sass_program_read(record, code, &probe, keep, &error)) {
        nvlog("direct machine code: \"%s\" refused - %s", fname.UTF8String, error.localizedDescription.UTF8String ?: "unreadable record");
        return nil;
    }
    NVMTLSassKernel *kernel = [NVMTLSassKernel new];
    kernel->_record = record; kernel->_code = code; kernel->_name = [fname copy];
    return kernel;
}

static NSDictionary *nvmtl_sass_manifest(NSString *directory, NSString *nameHash)
{
    NSData *data = nvmtl_sass_read([directory stringByAppendingPathComponent:[nameHash stringByAppendingString:@".json"]],
                                   NVMTL_SASS_RECORD_MAX);
    id record = data ? [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL] : nil;
    return [record isKindOfClass:NSDictionary.class] ? record : nil;
}

/* Library route: a kernel taken from a metallib as it is, with no linked functions and no function
 * constants. Keyed by the library's bytes and the function name. The library digest is kept on the
 * library object, because an app creates many pipelines from one library. */
static char gNVMTLSassLibDigest;
id nvmtl_sass_kernel_for_function(id function)
{
    if (!nvmtl_sass_enabled() || ![function isKindOfClass:NVMTLFunction.class]) return nil;
    NVMTLFunction *f = function;
    if (f->_fcv || f->_fc.count || !f->_fname.length || f.functionType != MTLFunctionTypeKernel) return nil;
    NVMTLLibrary *library = f->_lib;
    if (![library isKindOfClass:NVMTLLibrary.class] || !library->_raw.length) return nil;
    NSString *libraryHash = objc_getAssociatedObject(library, &gNVMTLSassLibDigest);
    if (!libraryHash) {
        libraryHash = nvmtl_sass_digest(library->_raw);
        objc_setAssociatedObject(library, &gNVMTLSassLibDigest, libraryHash, OBJC_ASSOCIATION_RETAIN);
    }
    NSString *directory = [g_sass_dir stringByAppendingPathComponent:libraryHash];
    NSString *nameHash = nvmtl_sass_digest([f->_fname dataUsingEncoding:NSUTF8StringEncoding]);
    NSDictionary *record = nvmtl_sass_manifest(directory, nameHash);
    if (!record) return nil;
    id linked = record[@"linked_functions"];
    if (linked && (![linked isKindOfClass:NSArray.class] || [linked count])) return nil;
    NVMTLSassKernel *kernel = nvmtl_sass_load_record(record, directory, f->_fname, libraryHash, @"metallib");
    if (kernel) nvlog("direct machine code: \"%s\" has a compiled kernel", f->_fname.UTF8String);
    return kernel;
}

/* Linked route: the kernel's own AIR module, the module the linker chose for each body it calls, and
 * the function-constant bytes the app set. Every byte that decides what the program is goes into the
 * key, so an artifact can only answer for the program it was compiled from. */
id nvmtl_sass_kernel_for_linked(id function, NSDictionary *mods, NSDictionary *fc)
{
    if (!nvmtl_sass_enabled() || ![function isKindOfClass:NVMTLFunction.class]) return nil;
    NVMTLFunction *fn = function;
    if (fn.functionType != MTLFunctionTypeKernel || !fn->_air.length || !fn->_fname.length) return nil;
    NSMutableDictionary *bodies = [NSMutableDictionary dictionary];
    for (NSString *name in mods) if ([mods[name] isKindOfClass:NSString.class]) bodies[name] = mods[name];
    NSMutableDictionary *constants = [NSMutableDictionary dictionary];
    for (NSString *index in fc) {
        NSArray *bytes = fc[index];
        if (![bytes isKindOfClass:NSArray.class]) continue;
        NSMutableString *hex = [NSMutableString string];
        for (NSNumber *b in bytes) [hex appendFormat:@"%02x", (unsigned)[b unsignedCharValue]];
        constants[index] = hex;
    }
    NSMutableString *key = [NSMutableString stringWithFormat:@"air\n%@\n%@\n", fn->_fname,
                            nvmtl_sass_digest([fn->_air dataUsingEncoding:NSUTF8StringEncoding])];
    for (NSString *name in [bodies.allKeys sortedArrayUsingSelector:@selector(compare:)])
        [key appendFormat:@"body %@ %@\n", name, nvmtl_sass_digest([bodies[name] dataUsingEncoding:NSUTF8StringEncoding])];
    for (NSString *index in [constants.allKeys sortedArrayUsingSelector:@selector(compare:)])
        [key appendFormat:@"fc %@ %@\n", index, constants[index]];
    NSString *airHash = nvmtl_sass_digest([fn->_air dataUsingEncoding:NSUTF8StringEncoding]);
    NSString *directory = [g_sass_dir stringByAppendingPathComponent:airHash];
    NSDictionary *record = nvmtl_sass_manifest(directory,
                                               nvmtl_sass_digest([key dataUsingEncoding:NSUTF8StringEncoding]));
    if (!record) return nil;
    /* every body the record names must be one this link chose; one it did not use is fine (constants
     * can make a call unreachable), one it invented is not */
    NSMutableSet *named = [NSMutableSet set];
    id entries = record[@"linked_functions"];
    for (id entry in [entries isKindOfClass:NSArray.class] ? entries : @[])
        if ([entry isKindOfClass:NSDictionary.class] && [entry[@"function"] isKindOfClass:NSString.class])
            [named addObject:entry[@"function"]];
    if (![named isSubsetOfSet:[NSSet setWithArray:bodies.allKeys]]) return nil;
    NVMTLSassKernel *kernel = nvmtl_sass_load_record(record, directory, fn->_fname, airHash, @"linked-air");
    if (kernel) nvlog("direct machine code: linked kernel \"%s\" has a compiled kernel (%lu bodies, %lu constants)",
                      fn->_fname.UTF8String, (unsigned long)bodies.count, (unsigned long)constants.count);
    return kernel;
}

/* Records one dispatch. Returns NO with a reason when the launcher refuses it; nothing has been
 * recorded into the command buffer in that case. */
static BOOL nvmtl_sass_encode(NVMTLSassKernel *kernel, NVMTLComputeCommandEncoder *encoder,
                              MTLSize grid, MTLSize group, uint32_t kind, NSError **error)
{
    if (kind > 1) return nvmtl_sass_error(error, 1, @"Unknown dispatch kind");
    uint32_t local[3] = { (uint32_t)group.width, (uint32_t)group.height, (uint32_t)group.depth };
    uint64_t input[3] = { grid.width, grid.height, grid.depth };
    uint32_t threads[3], groups[3];
    uint64_t total = 1;
    for (uint32_t axis = 0; axis < 3; axis++) {
        if (!local[axis] || local[axis] > g_lim.maxComputeWorkGroupSize[axis])
            return nvmtl_sass_error(error, 1, @"Threadgroup size exceeds the device limits");
        total *= local[axis];
        uint64_t extent = kind == 0 ? input[axis] * local[axis] : input[axis];
        if (!extent || extent > UINT32_MAX) return nvmtl_sass_error(error, 1, @"Grid exceeds the parameter ABI");
        threads[axis] = (uint32_t)extent;
        groups[axis] = (uint32_t)((extent + local[axis] - 1) / local[axis]);
        if (groups[axis] > g_lim.maxComputeWorkGroupCount[axis])
            return nvmtl_sass_error(error, 1, @"Group count exceeds the device limits");
    }
    if (total > g_lim.maxComputeWorkGroupInvocations) return nvmtl_sass_error(error, 1, @"Threadgroup exceeds the device limits");

    NSMutableArray *keep = [NSMutableArray array];
    struct nvmtl_sass_program program;
    if (!nvmtl_sass_program_read(kernel->_record, kernel->_code, &program, keep, error)) return NO;
    uint8_t block[NVMTL_SASS_MAX_PARAM_BYTES];
    uint32_t blockBytes = 0, sharedBytes = 0;
    if (!nvmtl_sass_resolve_block(encoder, &program, threads, block, sizeof block, &blockBytes, &sharedBytes, error))
        return NO;
    struct nvmtl_sass_extra {
        uint32_t        size;
        const void     *bank2;       uint32_t bank2_len;
        const void     *bank4;       uint32_t bank4_len;
        const void     *global_init; uint32_t global_init_len;
        const uint32_t *bank4_relocs; uint32_t bank4_reloc_count;
        uint32_t        slm_size;    uint32_t crs_size;
    } extra = {
        .size = sizeof extra,
        .bank2 = nvmtl_sass_program_bank2(&program), .bank2_len = nvmtl_sass_program_bank2_len(&program),
        .bank4 = nvmtl_sass_program_bank4(&program), .bank4_len = nvmtl_sass_program_bank4_len(&program),
        .global_init = nvmtl_sass_program_global_init(&program),
        .global_init_len = nvmtl_sass_program_global_init_len(&program),
        .bank4_relocs = nvmtl_sass_program_bank4_relocs(&program),
        .bank4_reloc_count = nvmtl_sass_program_bank4_reloc_count(&program),
        .slm_size = nvmtl_sass_program_scratch(&program),
        .crs_size = nvmtl_sass_program_call_stack(&program),
    };
    nvmtl_retain_resource(encoder->_cb, encoder->_ps);
    for (NSArray *entry in encoder->_bufAt.allValues) if (entry.count) nvmtl_retain_resource(encoder->_cb, entry[0]);
    nvk_cmdbuf *commands = &encoder->_cb->_c;
    if (commands->in_rp) nvmtl_vk_cmd_end_render(commands);
    VkMemoryBarrier before = { .sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_HOST_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_SHADER_READ_BIT | VK_ACCESS_SHADER_WRITE_BIT };
    pvkCmdPipelineBarrier(commands->cb, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT | VK_PIPELINE_STAGE_HOST_BIT,
                          VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, 0, 1, &before, 0, NULL, 0, NULL);
    int rc = g_sass_dispatch(commands->cb, nvmtl_sass_program_code(&program), nvmtl_sass_program_code_len(&program),
                             nvmtl_sass_program_registers(&program), sharedBytes, nvmtl_sass_program_barriers(&program),
                             (const uint64_t *)block, (blockBytes + 7) / 8, local, groups, &extra);
    if (rc) {
        /* NVK refused before recording anything (host side): the barrier above is harmless on its own */
        nvlog("direct machine code: NVK refused the dispatch of \"%s\" (%d)", kernel->_name.UTF8String, rc);
        return nvmtl_sass_error(error, 5, @"NVK refused the direct dispatch");
    }
    /* The program, its constant bank and its launch descriptor live in this pool's upload memory, which
     * the reuse witness does not cover: a command buffer that ran machine code is never reused. */
    commands->direct_sass = 1;
    VkMemoryBarrier after = { .sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT,
        .dstAccessMask = VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT | VK_ACCESS_HOST_READ_BIT };
    pvkCmdPipelineBarrier(commands->cb, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,
                          VK_PIPELINE_STAGE_ALL_COMMANDS_BIT | VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &after, 0, NULL, 0, NULL);
    return YES;
}

/* Called first by both dispatch methods when the bound pipeline carries machine code. Returns 1 when
 * the dispatch was recorded. Returns 0 when it was refused and the pipeline has a Vulkan pipeline to
 * fall back to. Returns -1 when it was refused and there is no fallback: the command buffer then
 * carries the error, as Metal reports an encoding failure. */
int nvmtl_sass_dispatch(id encoderObject, MTLSize grid, MTLSize group, uint32_t kind, BOOL hasFallback)
{
    NVMTLComputeCommandEncoder *encoder = encoderObject;
    NVMTLSassKernel *kernel = encoder->_ps ? encoder->_ps->_sassKernel : nil;
    if (![kernel isKindOfClass:NVMTLSassKernel.class] || !g_sass_dispatch) return hasFallback ? 0 : -1;
    NSError *error = nil;
    if (nvmtl_sass_encode(kernel, encoder, grid, group, kind, &error)) return 1;
    nvlog("direct machine code: \"%s\" dispatch refused: %s%s", kernel->_name.UTF8String,
          error.localizedDescription.UTF8String ?: "no reason given",
          hasFallback ? " - running its Vulkan pipeline instead" : "");
    if (hasFallback) return 0;
    [encoder->_cb nvmtlRecordEncodingError:error ?: [NSError errorWithDomain:MTLCommandBufferErrorDomain
        code:MTLCommandBufferErrorInternal userInfo:@{ NSLocalizedDescriptionKey: @"Compute dispatch failed" }]];
    return -1;
}
