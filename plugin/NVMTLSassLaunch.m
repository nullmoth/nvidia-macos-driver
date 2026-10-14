/*
 * NVMTLSassLaunch.m — turn a compiled kernel's launch record into an actual dispatch.
 *
 * The compiler (`sass_worker.py`) writes `artifact.bin`, the machine code NVIDIA's own compiler
 * produced, and `artifact.json`, the record saying what each word of the kernel's constant bank is.
 * This reads that record, resolves each word against what the Metal encoder has bound — buffer
 * addresses, texture and sampler descriptor indices, the threadgroup window, the constant tables —
 * packs the block, and hands it to NVK's raw-SASS dispatch.
 *
 * IT REFUSES RATHER THAN GUESSES. A record naming a texture the encoder never bound, a handle NVK
 *    will not give an index for, a window larger than the hardware's shared memory, two words
 *    claiming the same byte: each one is a refusal that sends the function to the existing engine.
 *    A kernel launched on a half-filled bank reads another resource's descriptor, which the hardware
 *    cannot report and the output does not reveal — so every one of these is checked before the
 *    dispatch is recorded, never after.
 *
 * Over 200 real Metal functions, 198 compile with a record this launcher can honour;
 * 88% of them need a texture handle and 53% a sampler handle, which is why those two are the whole
 * point of this file.
 */
#if !defined(NVMTL_SASS_LAUNCH_TEST)
#import "NVMTLObjects.h"
#import "nvmtl_vk.h"
#endif
#import "nvmtl_sass_params.h"

#import <Foundation/Foundation.h>
#include <dlfcn.h>

/* The plugin's own refusal helper lives in NVMTLV2Worker.m, which is part of the same translation
 * unit; this stands in for it only when this file is built alone for its test. */
#if defined(NVMTL_SASS_LAUNCH_TEST)
static BOOL nvmtl_sass_error(NSError **error, NSInteger code, NSString *message)
{
    if (error)
        *error = [NSError errorWithDomain:@"NVMTLDomain" code:code
                                 userInfo:@{ NSLocalizedDescriptionKey: message }];
    return NO;
}
#endif

/* NVK's side, which owns the descriptor tables and therefore the indices (nvmtl_nvk_sass_bind.c) */
typedef int (*nvmtl_image_handle_fn)(void *view, uint32_t use, uint32_t plane, uint32_t *out);
typedef int (*nvmtl_sampler_handle_fn)(void *sampler, uint32_t plane, uint32_t *out);
typedef int (*nvmtl_dispatch_sass_fn)(void *cmd, const void *sass, uint32_t sass_len,
                                      uint32_t num_gprs, uint32_t smem_size, uint32_t num_barriers,
                                      const uint64_t *params, uint32_t num_params,
                                      const uint32_t block[3], const uint32_t grid[3]);

enum { NVMTL_SASS_USE_SAMPLED = 0, NVMTL_SASS_USE_STORAGE = 1 };

/* the launch record, as this launcher needs it in memory */
struct nvmtl_sass_program {
    const void *code;
    uint32_t    code_len;
    uint32_t    registers;
    uint32_t    barriers;
    uint32_t    static_shared;
    uint32_t    param_count;
    struct nvmtl_sass_param params[NVMTL_SASS_MAX_PARAMS];
    /* what each parameter refers to, in Metal's own numbering */
    uint32_t    metal_index[NVMTL_SASS_MAX_PARAMS];
    uint8_t     texture_is_storage[NVMTL_SASS_MAX_PARAMS];
    /* pointer and length only: an Objective-C object inside a struct this code memsets would
     *    have its retain bypassed. The bytes are kept alive by the array the caller passes in. */
    struct nvmtl_sass_table tables[NVMTL_SASS_MAX_TABLES];
    uint32_t    table_count;
    /* the jump table OCG puts in constant bank 2 for an indirect branch (`LDC c[0x2][Rn]` +
     *   `BRX`). Pointer and length only, kept alive by the array the caller passes in, same rule as
     *   the tables above. Zero for a kernel with no indirect branch, which is nearly all of them. */
    const void *bank2;
    uint32_t    bank2_len;
    /* what the cubin declares it needs beside its registers: scratch memory per thread
     *   (a local array the shader indexes dynamically) and the call/return stack per warp. Both are
     *   zero for most kernels; a kernel with either was refused outright until the launcher could
     *   allocate them. */
    uint32_t    scratch_bytes;
    uint32_t    call_stack_bytes;
    /* a `constant` array in the Metal source: its bytes land in `.nv.global.init` and constant
     *   bank 4 holds RELOCATED pointers at them. Unlike bank 2 this cannot be uploaded verbatim —
     *   the pointers are absolute and only exist once the data has an address. The relocations are
     *   pairs (offset in bank 4, offset inside the global data). */
    const void *bank4;
    uint32_t    bank4_len;
    const void *global_init;
    uint32_t    global_init_len;
    const uint32_t *bank4_relocs;
    uint32_t    bank4_reloc_count;
};

/* A record is a FILE. Any field may be missing, null or the wrong type, and a comparison sent to
 *    NSNull raises, which a real texture kernel's `access: null` would do, aborting the process.
 *    Every string read goes through this. */
static NSString *nvmtl_sass_str(id value)
{
    return [value isKindOfClass:NSString.class] ? (NSString *)value : nil;
}

/* One hex decoder. There are three of these now (bank 2, bank 4, the global data) and three
 *   hand-rolled loops is three places for an odd length or a stray character to get through. */
static NSMutableData *nvmtl_sass_unhex(NSString *hex, NSError **error)
{
    if (hex.length % 2) {
        nvmtl_sass_error(error, 1, @"a recorded byte string is not whole bytes");
        return nil;
    }
    NSMutableData *raw = [NSMutableData dataWithLength:hex.length / 2];
    uint8_t *bytes = raw.mutableBytes;
    for (NSUInteger i = 0; i < hex.length; i += 2) {
        unsigned int byte = 0;
        if (sscanf([hex substringWithRange:NSMakeRange(i, 2)].UTF8String, "%2x", &byte) != 1) {
            nvmtl_sass_error(error, 1, @"a recorded byte string is not hex");
            return nil;
        }
        bytes[i / 2] = (uint8_t)byte;
    }
    return raw;
}


static uint32_t nvmtl_sass_u32(id value, BOOL *ok)
{
    if (![value isKindOfClass:NSNumber.class]) { *ok = NO; return 0; }
    long long v = [value longLongValue];
    if (v < 0 || v > 0xFFFFFFFFLL) { *ok = NO; return 0; }
    return (uint32_t)v;
}

/*
 * Read one record. Returns NO and leaves `error` set if anything in it is not something this
 * launcher can honour — including a kind it does not know, which is how a newer compiler's output
 * is refused by an older driver instead of being misread.
 */
BOOL nvmtl_sass_program_read(NSDictionary *record, NSData *code,
                             struct nvmtl_sass_program *out, NSMutableArray *keepalive,
                             NSError **error)
{
    static NSDictionary *kinds;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        kinds = @{ @"buffer": @(NVMTL_SASS_BUFFER), @"texture": @(NVMTL_SASS_TEXTURE),
                   @"sampler": @(NVMTL_SASS_SAMPLER), @"const_data": @(NVMTL_SASS_CONST_TABLE),
                   @"inline_sampler": @(NVMTL_SASS_INLINE_SAMPLER),
                   @"address_table": @(NVMTL_SASS_ADDRESS_TABLE) };
    });
    if (![record isKindOfClass:NSDictionary.class] || !code.length || !out || !keepalive)
        return nvmtl_sass_error(error, 1, @"Direct SASS: the launch record is not readable");
    memset(out, 0, sizeof *out);

    BOOL ok = YES;
    NSArray *params = record[@"parameters"];
    if (![params isKindOfClass:NSArray.class] || params.count > NVMTL_SASS_MAX_PARAMS)
        return nvmtl_sass_error(error, 1, @"Direct SASS: the record has no usable parameter list");

    uint32_t sass_sz = nvmtl_sass_u32(record[@"sass_bytes"], &ok);
    out->registers = nvmtl_sass_u32(record[@"registers"], &ok);
    out->barriers = nvmtl_sass_u32(record[@"barriers"], &ok);
    out->static_shared = nvmtl_sass_u32(record[@"static_shared_bytes"], &ok);
    /* Absent means zero, which is the honest reading: every artifact compiled before the
     *   launcher could allocate scratch has no such field, and a kernel needing it was refused. A
     *   value that is PRESENT and malformed still fails `ok` and refuses the record. */
    if (record[@"frame_size"] != nil && record[@"frame_size"] != NSNull.null)
        out->scratch_bytes = nvmtl_sass_u32(record[@"frame_size"], &ok);
    if (record[@"max_stack"] != nil && record[@"max_stack"] != NSNull.null)
        out->call_stack_bytes = nvmtl_sass_u32(record[@"max_stack"], &ok);
    /* A CALL NEEDS THE SUM OF THE FRAMES, NOT THE LARGEST ONE. `frame_size` and
     *   `max_stack` describe the ENTRY alone; a kernel that really calls also carries a frame for
     *   each subroutine ptxas emitted, and the callee's frame sits below the caller's. Allocating
     *   the entry's 144 bytes for a kernel whose subroutine wants another 144 let that subroutine
     *   write outside the thread's slice, and the return came back to address 0 (trace_bvh
     *   faulted with `compute shader VA 0x0` this way). `local_bytes` is
     *   that sum, taken from the cubin's own EIATTR_FRAME_SIZE records; a record compiled before
     *   the field existed keeps the old behaviour. */
    if (record[@"local_bytes"] != nil && record[@"local_bytes"] != NSNull.null) {
        uint32_t local_total = nvmtl_sass_u32(record[@"local_bytes"], &ok);
        if (local_total > out->scratch_bytes)
            out->scratch_bytes = local_total;
    }
    if (!ok || !out->registers || out->registers > 255)
        return nvmtl_sass_error(error, 1, @"Direct SASS: the record's register count is unusable");
    /* the machine code and the record must be the same artifact */
    if (sass_sz != code.length)
        return nvmtl_sass_error(error, 1, @"Direct SASS: the record does not describe this machine code");

    for (NSDictionary *p in params) {
        if (![p isKindOfClass:NSDictionary.class])
            return nvmtl_sass_error(error, 1, @"Direct SASS: a parameter record is not readable");
        NSString *kind = nvmtl_sass_str(p[@"kind"]);
        if (!kind)
            return nvmtl_sass_error(error, 1, @"Direct SASS: a parameter has no readable kind");
        uint32_t i = out->param_count;
        struct nvmtl_sass_param *slot = &out->params[i];
        slot->offset = nvmtl_sass_u32(p[@"offset"], &ok);
        slot->size = nvmtl_sass_u32(p[@"size"], &ok);
        slot->unused = [p[@"unused_by_shader"] boolValue] ? 1 : 0;
        if (!ok)
            return nvmtl_sass_error(error, 1, @"Direct SASS: a parameter's placement is unusable");

        if ([kind isEqualToString:@"hidden"]) {
            NSString *what = nvmtl_sass_str(p[@"what"]);
            if (!what)
                return nvmtl_sass_error(error, 1, @"Direct SASS: a hidden parameter has no meaning");
            if ([what isEqualToString:@"threads_per_grid.x"]) slot->kind = NVMTL_SASS_GRID_X;
            else if ([what isEqualToString:@"threads_per_grid.y"]) slot->kind = NVMTL_SASS_GRID_Y;
            else if ([what isEqualToString:@"threads_per_grid.z"]) slot->kind = NVMTL_SASS_GRID_Z;
            else if ([what hasPrefix:@"threadgroup_offset["]) {
                slot->kind = NVMTL_SASS_TG_OFFSET;
                slot->index = (uint32_t)[[what substringFromIndex:19] integerValue];
                out->metal_index[i] = slot->index;
            } else {
                return nvmtl_sass_error(error, 1, @"Direct SASS: an unknown hidden parameter");
            }
        } else if (kinds[kind]) {
            slot->kind = [kinds[kind] unsignedIntValue];
            /* only a resource the encoder binds carries a Metal index; a lifted constant table and
             *    an inline sampler carry their bytes instead, and asking for an index they do not
             *    have refused every kernel with a table (caught by this file's own test) */
            if (slot->kind == NVMTL_SASS_BUFFER || slot->kind == NVMTL_SASS_TEXTURE ||
                slot->kind == NVMTL_SASS_SAMPLER || slot->kind == NVMTL_SASS_ADDRESS_TABLE)
                out->metal_index[i] = nvmtl_sass_u32(p[@"metal_index"], &ok);
            if (slot->kind == NVMTL_SASS_TEXTURE) {
                NSString *access = nvmtl_sass_str(p[@"access"]);
                /* write and read_write bind the storage descriptor, sample and read the sampled one */
                out->texture_is_storage[i] = ([access isEqualToString:@"write"] ||
                                              [access isEqualToString:@"read_write"]) ? 1 : 0;
            }
            if (slot->kind == NVMTL_SASS_CONST_TABLE || slot->kind == NVMTL_SASS_INLINE_SAMPLER) {
                NSString *hex = nvmtl_sass_str(p[slot->kind == NVMTL_SASS_CONST_TABLE
                                                 ? @"bytes_hex" : @"air_state_hex"]);
                if (!hex || hex.length % 2)
                    return nvmtl_sass_error(error, 1, @"Direct SASS: a constant table is not readable");
                if (slot->kind == NVMTL_SASS_CONST_TABLE) {
                    if (out->table_count >= NVMTL_SASS_MAX_TABLES)
                        return nvmtl_sass_error(error, 1, @"Direct SASS: too many constant tables");
                    NSMutableData *bytes = [NSMutableData dataWithLength:hex.length / 2];
                    uint8_t *dst = bytes.mutableBytes;
                    for (NSUInteger b = 0; b < hex.length / 2; b++) {
                        unsigned value = 0;
                        if (sscanf([[hex substringWithRange:NSMakeRange(b * 2, 2)] UTF8String],
                                   "%2x", &value) != 1)
                            return nvmtl_sass_error(error, 1, @"Direct SASS: a constant table is not hex");
                        dst[b] = (uint8_t)value;
                    }
                    if (bytes.length != slot->size)
                        return nvmtl_sass_error(error, 1, @"Direct SASS: a table's size disagrees with the record");
                    [keepalive addObject:bytes];
                    slot->index = out->table_count;
                    out->tables[out->table_count].bytes = bytes.bytes;
                    out->tables[out->table_count].size = (uint32_t)bytes.length;
                    out->table_count++;
                }
            }
            if (!ok)
                return nvmtl_sass_error(error, 1, @"Direct SASS: a resource index is unusable");
        } else {
            return nvmtl_sass_error(error, 1,
                [NSString stringWithFormat:@"Direct SASS: parameter kind '%@' is not implemented", kind]);
        }
        out->param_count++;
    }
    out->code = code.bytes;
    out->code_len = (uint32_t)code.length;
    /* constant bank 4, the global data its pointers point at, and the patch list. All three or
     *   none: a template with no relocations is a bank of null pointers, which the compiler already
     *   refuses, and this refuses it again rather than binding one. */
    { NSString *b4 = nvmtl_sass_str(record[@"constant_bank4_hex"]);
      NSArray *relocs = [record[@"constant_bank4_relocs"] isKindOfClass:NSArray.class]
                        ? record[@"constant_bank4_relocs"] : nil;
      if (b4.length || relocs.count) {
          NSString *gi = nvmtl_sass_str(record[@"global_init_hex"]);
          if (!b4.length || !relocs.count || !gi.length) {
              nvmtl_sass_error(error, 1, @"constant bank 4 needs its bytes, its global data and its "
                                        "relocations, and this record has only some of them");
              return NO;
          }
          NSMutableData *b4raw = nvmtl_sass_unhex(b4, error); if (!b4raw) return NO;
          NSMutableData *giraw = nvmtl_sass_unhex(gi, error); if (!giraw) return NO;
          NSMutableData *pairs = [NSMutableData dataWithLength:relocs.count * 2 * sizeof(uint32_t)];
          uint32_t *slot = pairs.mutableBytes;
          for (NSUInteger i = 0; i < relocs.count; i++) {
              NSArray *pair = relocs[i];
              if (![pair isKindOfClass:NSArray.class] || pair.count != 2) {
                  nvmtl_sass_error(error, 1, @"a constant bank 4 relocation is not a pair");
                  return NO;
              }
              BOOL ok2 = YES;
              slot[2 * i]     = nvmtl_sass_u32(pair[0], &ok2);
              slot[2 * i + 1] = nvmtl_sass_u32(pair[1], &ok2);
              if (!ok2 || slot[2 * i] + 8 > b4raw.length || slot[2 * i + 1] > giraw.length) {
                  nvmtl_sass_error(error, 1, @"a constant bank 4 relocation points outside the bank "
                                            "or outside the global data");
                  return NO;
              }
          }
          [keepalive addObject:b4raw]; [keepalive addObject:giraw]; [keepalive addObject:pairs];
          out->bank4 = b4raw.bytes;                 out->bank4_len = (uint32_t)b4raw.length;
          out->global_init = giraw.bytes;           out->global_init_len = (uint32_t)giraw.length;
          out->bank4_relocs = pairs.bytes;          out->bank4_reloc_count = (uint32_t)relocs.count;
      } }

    /* constant bank 2, as hex in the record. Decoded into the caller's keep-alive array so the
     *   bytes outlive this function, exactly like the address tables above. */
    { NSString *hex = nvmtl_sass_str(record[@"constant_bank2_hex"]);
      if (hex.length) {
          if (hex.length % 2) { nvmtl_sass_error(error, 1, @"constant bank 2 is not whole bytes"); return NO; }
          NSMutableData *raw = [NSMutableData dataWithLength:hex.length / 2];
          uint8_t *raw_bytes = raw.mutableBytes;  /* not named out: that is the program struct */
          for (NSUInteger i = 0; i < hex.length; i += 2) {
              unsigned int byte = 0;
              if (sscanf([hex substringWithRange:NSMakeRange(i, 2)].UTF8String, "%2x", &byte) != 1) {
                  nvmtl_sass_error(error, 1, @"constant bank 2 is not hex"); return NO;
              }
              raw_bytes[i / 2] = (uint8_t)byte;
          }
          [keepalive addObject:raw];
          out->bank2 = raw.bytes;
          out->bank2_len = (uint32_t)raw.length;
      } }
    return YES;
}

/*
 * The record's fields, for the resolve path in NVMTLSassResolve.m. Accessors rather than a shared
 * header so the struct's layout stays private to the file that fills it.
 */
uint32_t nvmtl_sass_program_param_count(const struct nvmtl_sass_program *p) { return p->param_count; }
const struct nvmtl_sass_param *nvmtl_sass_program_params(const struct nvmtl_sass_program *p) { return p->params; }
uint32_t nvmtl_sass_program_metal_index(const struct nvmtl_sass_program *p, uint32_t i)
{
    return i < p->param_count ? p->metal_index[i] : 0;
}
BOOL nvmtl_sass_program_is_storage(const struct nvmtl_sass_program *p, uint32_t i)
{
    return i < p->param_count && p->texture_is_storage[i];
}
const struct nvmtl_sass_table *nvmtl_sass_program_tables(const struct nvmtl_sass_program *p) { return p->tables; }
uint32_t nvmtl_sass_program_table_count(const struct nvmtl_sass_program *p) { return p->table_count; }
uint32_t nvmtl_sass_program_registers(const struct nvmtl_sass_program *p) { return p->registers; }
uint32_t nvmtl_sass_program_barriers(const struct nvmtl_sass_program *p) { return p->barriers; }
uint32_t nvmtl_sass_program_static_shared(const struct nvmtl_sass_program *p) { return p->static_shared; }
uint32_t nvmtl_sass_program_scratch(const struct nvmtl_sass_program *p) { return p->scratch_bytes; }
uint32_t nvmtl_sass_program_call_stack(const struct nvmtl_sass_program *p) { return p->call_stack_bytes; }
const void *nvmtl_sass_program_bank4(const struct nvmtl_sass_program *p) { return p->bank4; }
uint32_t nvmtl_sass_program_bank4_len(const struct nvmtl_sass_program *p) { return p->bank4_len; }
const void *nvmtl_sass_program_global_init(const struct nvmtl_sass_program *p) { return p->global_init; }
uint32_t nvmtl_sass_program_global_init_len(const struct nvmtl_sass_program *p) { return p->global_init_len; }
const uint32_t *nvmtl_sass_program_bank4_relocs(const struct nvmtl_sass_program *p) { return p->bank4_relocs; }
uint32_t nvmtl_sass_program_bank4_reloc_count(const struct nvmtl_sass_program *p) { return p->bank4_reloc_count; }
const void *nvmtl_sass_program_code(const struct nvmtl_sass_program *p) { return p->code; }
uint32_t nvmtl_sass_program_code_len(const struct nvmtl_sass_program *p) { return p->code_len; }
const void *nvmtl_sass_program_bank2(const struct nvmtl_sass_program *p) { return p->bank2; }
uint32_t nvmtl_sass_program_bank2_len(const struct nvmtl_sass_program *p) { return p->bank2_len; }

size_t nvmtl_sass_program_size(void) { return sizeof(struct nvmtl_sass_program); }

#if defined(NVMTL_SASS_LAUNCH_TEST)
/*
 * The record reader, on the CPU, with no Metal objects and no device: a record this launcher would
 * mis-launch has to be refused here, where it can be seen.
 */
#include <stdio.h>

static unsigned t_pass, t_fail;
static void t_arm(const char *name, BOOL ok)
{
    if (ok) t_pass++;
    else { t_fail++; printf("FAIL  %s\n", name); }
}

static NSDictionary *t_param(NSString *kind, uint32_t off, uint32_t size, NSDictionary *extra)
{
    NSMutableDictionary *d = [@{ @"kind": kind, @"offset": @(off), @"size": @(size) } mutableCopy];
    [d addEntriesFromDictionary:extra ?: @{}];
    return d;
}

int nvmtl_sass_launch_cpu_v1(void);
int nvmtl_sass_launch_cpu_v1(void)
{
  @autoreleasepool {
    NSData *code = [NSMutableData dataWithLength:256];
    struct nvmtl_sass_program prog;
    NSMutableArray *keep = [NSMutableArray array];
    NSError *err = nil;

    NSArray *good = @[
        t_param(@"buffer", 0, 8, @{ @"metal_index": @0 }),
        t_param(@"texture", 8, 8, @{ @"metal_index": @2, @"access": @"sample" }),
        t_param(@"sampler", 16, 8, @{ @"metal_index": @1 }),
        t_param(@"hidden", 24, 4, @{ @"what": @"threads_per_grid.x" }),
        t_param(@"hidden", 28, 4, @{ @"what": @"threads_per_grid.y" }),
        t_param(@"hidden", 32, 4, @{ @"what": @"threads_per_grid.z" }),
        t_param(@"hidden", 36, 4, @{ @"what": @"threadgroup_offset[1]" }),
    ];
    NSDictionary *rec = @{ @"parameters": good, @"sass_bytes": @256, @"registers": @32,
                           @"barriers": @1, @"static_shared_bytes": @0 };
    t_arm("a record with a buffer, a texture, a sampler, the grid and shared memory is read",
          nvmtl_sass_program_read(rec, code, &prog, keep, &err));
    t_arm("each parameter keeps the meaning the record gave it",
          prog.param_count == 7 && prog.params[0].kind == NVMTL_SASS_BUFFER &&
          prog.params[1].kind == NVMTL_SASS_TEXTURE && prog.metal_index[1] == 2 &&
          prog.texture_is_storage[1] == 0 &&
          prog.params[2].kind == NVMTL_SASS_SAMPLER && prog.metal_index[2] == 1 &&
          prog.params[3].kind == NVMTL_SASS_GRID_X && prog.params[5].kind == NVMTL_SASS_GRID_Z &&
          prog.params[6].kind == NVMTL_SASS_TG_OFFSET && prog.params[6].index == 1);

    NSDictionary *writable = @{ @"parameters": @[ t_param(@"texture", 0, 8,
                                   @{ @"metal_index": @0, @"access": @"write" }) ],
                                @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                                @"static_shared_bytes": @0 };
    t_arm("a texture written by the kernel binds the storage descriptor, not the sampled one",
          nvmtl_sass_program_read(writable, code, &prog, keep, &err) && prog.texture_is_storage[0] == 1);

    /* a real artifact had `access: null`; comparing it raised and aborted the process */
    NSDictionary *nulls = @{ @"parameters": @[ t_param(@"texture", 0, 8,
                                 @{ @"metal_index": @0, @"access": [NSNull null] }),
                             t_param(@"hidden", 8, 4, @{ @"what": @"threads_per_grid.x" }),
                             t_param(@"hidden", 12, 4, @{ @"what": @"threads_per_grid.y" }),
                             t_param(@"hidden", 16, 4, @{ @"what": @"threads_per_grid.z" }) ],
                             @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                             @"static_shared_bytes": @0 };
    t_arm("a null field in the record is read, not raised on",
          nvmtl_sass_program_read(nulls, code, &prog, keep, &err) && !prog.texture_is_storage[0]);
    NSDictionary *nokind = @{ @"parameters": @[ t_param(@"x", 0, 8, @{}) ],
                              @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                              @"static_shared_bytes": @0 };
    [(NSMutableDictionary *)nokind[@"parameters"][0] setObject:[NSNull null] forKey:@"kind"];
    t_arm("a parameter whose kind is null is refused",
          !nvmtl_sass_program_read(nokind, code, &prog, keep, &err));

    NSDictionary *mismatch = @{ @"parameters": good, @"sass_bytes": @128, @"registers": @32,
                                @"barriers": @0, @"static_shared_bytes": @0 };
    t_arm("a record that does not describe this machine code is refused",
          !nvmtl_sass_program_read(mismatch, code, &prog, keep, &err));

    NSDictionary *unknown = @{ @"parameters": @[ t_param(@"acceleration_structure", 0, 8, @{}) ],
                               @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                               @"static_shared_bytes": @0 };
    t_arm("a parameter kind this launcher does not implement is refused",
          !nvmtl_sass_program_read(unknown, code, &prog, keep, &err));

    NSDictionary *badhidden = @{ @"parameters": @[ t_param(@"hidden", 0, 4, @{ @"what": @"something_new" }) ],
                                 @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                                 @"static_shared_bytes": @0 };
    t_arm("a hidden parameter with an unknown meaning is refused",
          !nvmtl_sass_program_read(badhidden, code, &prog, keep, &err));

    NSDictionary *noregs = @{ @"parameters": good, @"sass_bytes": @256, @"registers": @0,
                              @"barriers": @0, @"static_shared_bytes": @0 };
    t_arm("a record with no register count is refused",
          !nvmtl_sass_program_read(noregs, code, &prog, keep, &err));

    NSDictionary *badtable = @{ @"parameters": @[ t_param(@"const_data", 0, 8, @{ @"bytes_hex": @"0011" }) ],
                                @"sass_bytes": @256, @"registers": @8, @"barriers": @0,
                                @"static_shared_bytes": @0 };
    t_arm("a constant table whose bytes disagree with its size is refused",
          !nvmtl_sass_program_read(badtable, code, &prog, keep, &err));

    NSDictionary *table_ok = @{ @"parameters": @[
            t_param(@"const_data", 0, 4, @{ @"bytes_hex": @"01020304" }),
            t_param(@"hidden", 8, 4, @{ @"what": @"threads_per_grid.x" }),
            t_param(@"hidden", 12, 4, @{ @"what": @"threads_per_grid.y" }),
            t_param(@"hidden", 16, 4, @{ @"what": @"threads_per_grid.z" }) ],
            @"sass_bytes": @256, @"registers": @8, @"barriers": @0, @"static_shared_bytes": @0 };
    BOOL read_ok = nvmtl_sass_program_read(table_ok, code, &prog, keep, &err);
    const uint8_t want[4] = { 1, 2, 3, 4 };
    t_arm("a constant table's bytes arrive in order",
          read_ok && prog.table_count == 1 && prog.tables[0].size == 4 &&
          memcmp(prog.tables[0].bytes, want, 4) == 0);

    /* and the packing the record feeds, with the handles NVK's side would have resolved */
    if (read_ok) {
        uint8_t block[NVMTL_SASS_MAX_PARAM_BYTES];
        uint32_t bytes = 0;
        const uint32_t threads[3] = { 8, 4, 1 };
        int rc = nvmtl_sass_pack(prog.params, prog.param_count, NULL, 0, NULL, 0, NULL, 0,
                                 prog.tables, prog.table_count, threads, block, sizeof block, &bytes);
        t_arm("the record's own parameters pack into a block", rc == 0 && bytes == 24);
        t_arm("the table landed where the record said", rc == 0 && memcmp(block, want, 4) == 0);
    }

    /* and a REAL artifact, if one is named: the record the compiler actually wrote for a real
     *    Metal function, read by the very function the driver uses. */
    const char *record_path = getenv("NVMTL_SASS_TEST_RECORD");
    const char *code_path = getenv("NVMTL_SASS_TEST_CODE");
    if (record_path && code_path) {
        NSData *json = [NSData dataWithContentsOfFile:@(record_path)];
        NSData *bin = [NSData dataWithContentsOfFile:@(code_path)];
        NSDictionary *real = json ? [NSJSONSerialization JSONObjectWithData:json options:0 error:NULL] : nil;
        NSMutableArray *rkeep = [NSMutableArray array];
        struct nvmtl_sass_program rp;
        NSError *rerr = nil;
        BOOL read = real && bin && nvmtl_sass_program_read(real, bin, &rp, rkeep, &rerr);
        t_arm("the compiler's own record for a real Metal function is accepted", read);
        if (read) {
            printf("  real artifact: %u parameters, %u registers, %u barriers, %u static shared, %u tables\n",
                   rp.param_count, rp.registers, rp.barriers, rp.static_shared, rp.table_count);
            unsigned tex = 0, smp = 0, buf = 0, tg = 0;
            for (uint32_t i = 0; i < rp.param_count; i++) {
                if (rp.params[i].kind == NVMTL_SASS_TEXTURE) tex++;
                else if (rp.params[i].kind == NVMTL_SASS_SAMPLER) smp++;
                else if (rp.params[i].kind == NVMTL_SASS_BUFFER) buf++;
                else if (rp.params[i].kind == NVMTL_SASS_TG_OFFSET) tg++;
            }
            printf("  resolves to: %u buffers, %u textures, %u samplers, %u threadgroup windows\n",
                   buf, tex, smp, tg);
        } else if (rerr) {
            printf("  refused: %s\n", rerr.localizedDescription.UTF8String);
        }
    }

    printf("sass launch: %u pass %u fail gpu_used=0\n", t_pass, t_fail);
    return t_fail ? 1 : 0;
  }
}
#endif
