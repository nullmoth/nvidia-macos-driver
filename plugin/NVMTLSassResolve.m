/*
 * NVMTLSassResolve.m — resolve a launch record against what the Metal encoder has bound, and record
 * the dispatch.
 *
 * `NVMTLSassLaunch.m` turns the compiler's record into a list of parameter slots and says what each
 * one refers to in Metal's own numbering. This asks the encoder for those resources — the buffer at
 * index n, the texture at index n, the sampler at index n, the byte length of threadgroup index n —
 * turns each into the word the machine code expects, and hands the block to NVK's raw-SASS dispatch.
 *
 * EVERY RESOLVE CAN FAIL, AND EACH FAILURE IS A REFUSAL. An unbound texture, a view NVK will not
 *    give a descriptor index for, a threadgroup window larger than the hardware's shared memory: the
 *    function goes to the existing engine instead. A dispatch recorded against a half-resolved block
 *    reads another resource's descriptor, and neither the hardware nor the output would show it.
 *
 * Every resource resolved here is also retained until the command buffer completes. The GPU reads
 *    these addresses and descriptors long after this function returns, and a texture released in
 *    between is the same class of use-after-free that killed the present path once already.
 */
#import "NVMTLObjects.h"
#import "nvmtl_sass_params.h"
#import "nvmtl_vk.h"

#import <Foundation/Foundation.h>
#include <dlfcn.h>

/* A REFUSAL MUST BE READABLE. Every error return below goes through this, so a dispatch that does
 *    not run says why and where. A silent refusal looks exactly like a GPU fault with no cause.
 * VARIADIC on purpose: a message built with `[NSString stringWithFormat:@"…", x]` carries a comma
 *    the preprocessor counts as an argument separator (square brackets are not nesting). */
static BOOL nvmtl_sass_refused(NSError **error, int code, NSString *why, const char *where, int line) {
    nvlog("DIRECT_SASS_REFUSED %s:%d - %s", where, line, why.UTF8String);
    return nvmtl_sass_error(error, code, why);
}
#define NVMTL_SASS_NO(e, c, ...) nvmtl_sass_refused((e), (c), __VA_ARGS__, __FUNCTION__, __LINE__)

/* from NVMTLSassLaunch.m */
struct nvmtl_sass_program;
BOOL nvmtl_sass_program_read(NSDictionary *record, NSData *code,
                             struct nvmtl_sass_program *out, NSMutableArray *keepalive,
                             NSError **error);
uint32_t nvmtl_sass_program_param_count(const struct nvmtl_sass_program *p);
const struct nvmtl_sass_param *nvmtl_sass_program_params(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_metal_index(const struct nvmtl_sass_program *p, uint32_t i);
BOOL nvmtl_sass_program_is_storage(const struct nvmtl_sass_program *p, uint32_t i);
const struct nvmtl_sass_table *nvmtl_sass_program_tables(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_table_count(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_registers(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_barriers(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_static_shared(const struct nvmtl_sass_program *p);
const void *nvmtl_sass_program_code(const struct nvmtl_sass_program *p);
uint32_t nvmtl_sass_program_code_len(const struct nvmtl_sass_program *p);

/* NVK's side: it owns the descriptor tables, so it owns the indices (nvmtl_nvk_sass_bind.c) */
typedef int (*nvmtl_image_handle_fn)(void *view, uint32_t use, uint32_t plane, uint32_t *out);
typedef int (*nvmtl_sampler_handle_fn)(void *sampler, uint32_t plane, uint32_t *out);

/* NVMTL_SASS_USE_SAMPLED / _STORAGE come from NVMTLSassLaunch.m, which is imported first */
#define NVMTL_SASS_SAMPLER_SHIFT 20

static nvmtl_image_handle_fn nvmtl_sass_image_fn(void)
{
    static nvmtl_image_handle_fn fn;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ if (g_lib) fn = (nvmtl_image_handle_fn)dlsym(g_lib, "nvmtl_nvk_sass_image_handle_v1"); });
    return fn;
}

static nvmtl_sampler_handle_fn nvmtl_sass_sampler_fn(void)
{
    static nvmtl_sampler_handle_fn fn;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ if (g_lib) fn = (nvmtl_sampler_handle_fn)dlsym(g_lib, "nvmtl_nvk_sass_sampler_handle_v1"); });
    return fn;
}

/*
 * The handle word for one Metal texture, as the shader reads it. A texture the kernel writes binds
 * the storage descriptor; one it samples or reads binds the sampled descriptor.
 */
/* 32 slots: the same ceiling the buffer-binding path already enforces, and the kernel's own
 *    folded count decides how many it reads. A table is 256 B. */
#define NVMTL_SASS_TABLE_SLOTS 32
/* Said once per process, with the first four entries: a table of zeroes is the difference between
 *    "the app bound nothing there" and "the wrong indices were read". */
static void nvmtl_sass_table_said(const uint64_t *slots, uint32_t base, uint32_t filled) {
    static _Atomic int said;
    if (atomic_exchange(&said, 1)) return;
    nvlog("DIRECT_SASS_ADDRESS_TABLE base=%u filled=%u [0]=0x%llx [1]=0x%llx [2]=0x%llx [3]=0x%llx",
          base, filled, (unsigned long long)slots[0], (unsigned long long)slots[1],
          (unsigned long long)slots[2], (unsigned long long)slots[3]);
}
static BOOL nvmtl_sass_texture_word(NVMTLComputeCommandEncoder *encoder, uint32_t index,
                                    BOOL storage, uint64_t *word, NSError **error)
{
    nvmtl_image_handle_fn resolve = nvmtl_sass_image_fn();
    if (!resolve)
        return NVMTL_SASS_NO(error, 3, @"Direct SASS: NVK has no texture-handle entry point");
    NVMTLTexture *texture = encoder->_sassTextures[@(index)];
    if (![texture isKindOfClass:NVMTLTexture.class])
        return NVMTL_SASS_NO(error, 1,
            [NSString stringWithFormat:@"Direct SASS: no texture is bound at index %u", index]);
    void *view = [texture nvview];
    if (!view)
        return NVMTL_SASS_NO(error, 1, @"Direct SASS: the bound texture has no view to bind with");
    uint32_t handle = 0;
    int rc = resolve(view, storage ? NVMTL_SASS_USE_STORAGE : NVMTL_SASS_USE_SAMPLED, 0, &handle);
    if (rc)
        return NVMTL_SASS_NO(error, 1,
            [NSString stringWithFormat:@"Direct SASS: NVK gave no descriptor index for the texture at %u (%d)",
                                       index, rc]);
    nvmtl_retain_resource(encoder->_cb, texture);
    *word = handle;
    return YES;
}

static BOOL nvmtl_sass_sampler_word(NVMTLComputeCommandEncoder *encoder, uint32_t index,
                                    uint64_t *word, NSError **error)
{
    nvmtl_sampler_handle_fn resolve = nvmtl_sass_sampler_fn();
    if (!resolve)
        return NVMTL_SASS_NO(error, 3, @"Direct SASS: NVK has no sampler-handle entry point");
    NVMTLSamplerState *sampler = encoder->_sassSamplers[@(index)];
    if (![sampler isKindOfClass:NVMTLSamplerState.class] || !sampler->_s.s)
        return NVMTL_SASS_NO(error, 1,
            [NSString stringWithFormat:@"Direct SASS: no sampler is bound at index %u", index]);
    uint32_t handle = 0;
    int rc = resolve(sampler->_s.s, 0, &handle);
    if (rc)
        return NVMTL_SASS_NO(error, 1,
            [NSString stringWithFormat:@"Direct SASS: NVK gave no descriptor index for the sampler at %u (%d)",
                                       index, rc]);
    nvmtl_retain_resource(encoder->_cb, sampler);
    /* the shader shifts the sampler into the high bits of the combined handle itself, so the word
     *    it reads for a sampler parameter is the index alone, already in that position */
    *word = (uint64_t)handle << NVMTL_SASS_SAMPLER_SHIFT;
    return YES;
}

/*
 * The shared window: Metal records a byte length per threadgroup index, the kernel reads one window
 * with a byte offset per index. Returns the window size in `window`.
 */
static BOOL nvmtl_sass_threadgroup(NVMTLComputeCommandEncoder *encoder,
                                   uint32_t *offsets, uint32_t slots,
                                   uint32_t *window, NSError **error)
{
    uint32_t lengths[NVMTL_SASS_MAX_PARAMS] = { 0 };
    uint32_t highest = 0;
    for (NSString *key in encoder->_tgLen) {
        uint32_t index = (uint32_t)[key integerValue];
        if (index >= slots)
            return NVMTL_SASS_NO(error, 1, @"Direct SASS: a threadgroup index beyond the record");
        NSNumber *length = encoder->_tgLen[key];
        if (![length isKindOfClass:NSNumber.class])
            return NVMTL_SASS_NO(error, 1, @"Direct SASS: a threadgroup length is unreadable");
        lengths[index] = (uint32_t)[length unsignedLongLongValue];
        if (index + 1 > highest)
            highest = index + 1;
    }
    *window = nvmtl_sass_threadgroup_window(lengths, highest, 16,
                                            g_lim.maxComputeSharedMemorySize, offsets);
    if (highest && !*window)
        return NVMTL_SASS_NO(error, 1, @"Direct SASS: the threadgroup memory asked for exceeds the device");
    return YES;
}

/*
 * Resolve the whole record and fill the parameter block. `threads` is Metal's dispatch size in
 * threads, which is what the kernel's own bounds guard compares against.
 */
BOOL nvmtl_sass_resolve_block(NVMTLComputeCommandEncoder *encoder,
                              const struct nvmtl_sass_program *program,
                              const uint32_t threads[3],
                              uint8_t *block, uint32_t block_capacity, uint32_t *block_bytes,
                              uint32_t *shared_bytes, NSError **error)
{
    const struct nvmtl_sass_param *params = nvmtl_sass_program_params(program);
    uint32_t count = nvmtl_sass_program_param_count(program);
    uint64_t addresses[NVMTL_SASS_MAX_PARAMS] = { 0 };
    uint64_t handles[NVMTL_SASS_MAX_PARAMS] = { 0 };
    uint32_t tg_offsets[NVMTL_SASS_MAX_PARAMS] = { 0 };
    struct nvmtl_sass_param resolved[NVMTL_SASS_MAX_PARAMS];
    uint32_t window = 0;

    if (!encoder || !program || !block || !block_bytes || !shared_bytes || count > NVMTL_SASS_MAX_PARAMS)
        return NVMTL_SASS_NO(error, 1, @"Direct SASS: the dispatch has nothing to resolve");
    if (!nvmtl_sass_threadgroup(encoder, tg_offsets, NVMTL_SASS_MAX_PARAMS, &window, error))
        return NO;

    for (uint32_t i = 0; i < count; i++) {
        resolved[i] = params[i];
        resolved[i].index = i;                  /* each parameter resolves into its own slot */
        uint32_t metal = nvmtl_sass_program_metal_index(program, i);

        switch (params[i].kind) {
        case NVMTL_SASS_BUFFER: {
            NSArray *entry = encoder->_bufAt[@(metal)];
            NVMTLBuffer *buffer = entry.count ? entry[0] : nil;
            uint64_t offset = entry.count > 1 ? [entry[1] unsignedLongLongValue] : 0;
            if (![buffer isKindOfClass:NVMTLBuffer.class]) {
                if (params[i].unused) {
                    addresses[i] = 0;          /* the shader never reads it */
                    break;
                }
                return NVMTL_SASS_NO(error, 1,
                    [NSString stringWithFormat:@"Direct SASS: no buffer is bound at index %u", metal]);
            }
            uint64_t address = nvmtl_vk_buffer_address(&buffer->_b);
            if (!address || offset >= buffer->_b.size || address > UINT64_MAX - offset)
                return NVMTL_SASS_NO(error, 1, @"Direct SASS: the bound buffer is unusable");
            nvmtl_retain_resource(encoder->_cb, buffer);
            /* the packer reads `addresses[p->index]`, and THIS array is filled by ordinal: a
             *    kernel binding buffer 23 had its address looked up at slot 23, which nobody wrote.
             *    Metal indices and ordinals coincide only for 0,1,2… which is why every kernel that
             *    worked bound exactly those. */
            resolved[i].index = i;
            addresses[i] = address + offset;
            break;
        }
        case NVMTL_SASS_TEXTURE:
            if (!nvmtl_sass_texture_word(encoder, metal,
                                         nvmtl_sass_program_is_storage(program, i), &handles[i], error))
                return NO;
            break;
        case NVMTL_SASS_SAMPLER:
            if (!nvmtl_sass_sampler_word(encoder, metal, &handles[i], error))
                return NO;
            break;
        case NVMTL_SASS_INLINE_SAMPLER:
            /* a constexpr sampler's state is in the record, not bound by the app: the pipeline
             * variant already created it, and its index comes from the same table as any sampler */
            if (!nvmtl_sass_sampler_word(encoder, metal, &handles[i], error))
                return NO;
            break;
        case NVMTL_SASS_ADDRESS_TABLE: {
            /* THE TABLE METAL MATERIALISES. One entry per Metal buffer index from this
             *    argument's own base, holding that buffer's address plus its bound offset; 0 where
             *    nothing is bound, which the kernel's folded count means it never reads. */
            NVMTLBuffer *table = [NVMTLBuffer new];
            if (nvmtl_vk_buffer_create(NVMTL_SASS_TABLE_SLOTS * sizeof(uint64_t), 1, &table->_b) ||
                !table->_b.map)
                return NVMTL_SASS_NO(error, 1, @"Direct SASS: no memory for an array_ref table");
            table->_storage = MTLStorageModeShared;
            uint64_t *slots = (uint64_t *)table->_b.map;
            /* The table is index-addressed, like the one the Vulkan path builds for the same
             *    argument: slot k holds the buffer bound at Metal index (this argument's index + k),
             *    and every slot with nothing bound holds the dummy buffer's address. MPS binds the
             *    members one index at a time, not as a range. The array's length comes from the
             *    module's folded static_init, not from how many buffers are bound, so an unfilled
             *    slot may still be read: zero cannot be read, 256 bytes of zeroes can. */
            const uint64_t dummy = nvmtl_vk_dummy_address();
            for (uint32_t k = 0; k < NVMTL_SASS_TABLE_SLOTS; k++) slots[k] = dummy;
            uint32_t first = metal, filled = 0;
            for (uint32_t k = 0; k < NVMTL_SASS_TABLE_SLOTS && filled < NVMTL_SASS_TABLE_SLOTS; k++) {
                NSArray *entry = encoder->_bufAt[@(first + k)];
                NVMTLBuffer *bound = entry.count ? entry[0] : nil;
                uint64_t off = entry.count > 1 ? [entry[1] unsignedLongLongValue] : 0;
                uint64_t addr = bound ? nvmtl_vk_buffer_address(&bound->_b) : 0;
                if (!bound || !addr || off >= bound->_b.size || addr > UINT64_MAX - off) continue;
                slots[k] = addr + off;
                filled++;
                /* every buffer the table names must outlive the dispatch, exactly like a bound
                 *    one: the GPU dereferences these addresses, not the Metal objects. */
                nvmtl_retain_resource(encoder->_cb, bound);
            }
            /* WHAT DID MPS ITSELF PUT AT THIS INDEX? The whole question is whether the app
             *    fills this array (and in what order) or the driver materialises it. The app's own
             *    buffer is right here, so READ IT rather than infer from a fault address: its first
             *    four words, and whether it is even CPU-readable. */
            {
                NSArray *own = encoder->_bufAt[@(metal)];
                NVMTLBuffer *src = own.count ? own[0] : nil;
                const uint64_t *words = src && src->_b.map ? (const uint64_t *)src->_b.map : NULL;
                static _Atomic int peeked;
                if (!atomic_exchange(&peeked, 1))
                    nvlog("DIRECT_SASS_ARRAYREF_SOURCE index=%u bound=%d size=%zu mapped=%d"
                          " [0]=0x%llx [1]=0x%llx [2]=0x%llx [3]=0x%llx", metal, src ? 1 : 0,
                          src ? src->_b.size : 0, words ? 1 : 0,
                          (unsigned long long)(words ? words[0] : 0),
                          (unsigned long long)(words ? words[1] : 0),
                          (unsigned long long)(words ? words[2] : 0),
                          (unsigned long long)(words ? words[3] : 0));
            }
            /* ONE RECORD PER DISPATCH, to a file, when NVMTL_SASS_TABLELOG names one. nvlog is
             *    bounded per call site and the per-draw trace only instruments the Vulkan path, so
             *    neither can show the dispatch that FAULTS — and that is the only one that matters.
             *    The last line of this file is that dispatch. */
            if (getenv("NVMTL_SASS_TABLELOG")) {
                NSMutableString *line = [NSMutableString stringWithFormat:@"%@ first=%u filled=%u entries",
                    encoder->_ps && encoder->_ps->_function ? encoder->_ps->_function->_fname : @"?",
                    first, filled];
                for (uint32_t k = 0; k < filled && k < 8; k++) [line appendFormat:@" 0x%llx", (unsigned long long)slots[k]];
                [line appendString:@" | bound"];
                for (uint32_t k = 0; k < 8; k++) {
                    NSArray *e = encoder->_bufAt[@(k)];
                    NVMTLBuffer *b = e.count ? e[0] : nil;
                    [line appendFormat:@" %u:%@", k, b ? [NSString stringWithFormat:@"%zu", b->_b.size] : @"-"];
                }
                /* MPS's own parameter structs, which is where the array's count and the
                 *    kernel's indices into it live (reflection: ndArrayParams 80 B, params 144 B).
                 *    Read as 32-bit words — a count or an index is readable either way. */
                for (uint32_t which = 0; which < 2; which++) {
                    uint32_t idx = which ? 29 : 23;
                    NSArray *pe = encoder->_bufAt[@(idx)];
                    NVMTLBuffer *pb = pe.count ? pe[0] : nil;
                    uint64_t poff = pe.count > 1 ? [pe[1] unsignedLongLongValue] : 0;
                    [line appendFormat:@" | PARAMS%u", idx];
                    if (!pb) { [line appendString:@" unbound"]; continue; }
                    if (!pb->_b.map) { [line appendFormat:@" private(%zu)", pb->_b.size]; continue; }
                    const uint32_t *w = (const uint32_t *)((const uint8_t *)pb->_b.map + poff);
                    /* FORTY words, not twenty: the kernel's strides live at +112 and +120, and a
                     *   dump that stopped at byte 76 could not show them. The binding's own size
                     *   and offset go on the line too — a read at +112 out of an 80-byte window is
                     *   the defect itself, not a detail. */
                    const uint32_t n = (uint32_t)MIN((size_t)40, (pb->_b.size - poff) / 4);
                    [line appendFormat:@"(size %zu off %llu)", pb->_b.size, (unsigned long long)poff];
                    for (uint32_t k = 0; k < n; k++) [line appendFormat:@" %u", w[k]];
                }
                /* what MPS asked for, and what the launcher will program */
                [line appendString:@" | TGLEN"];
                if (!encoder->_tgLen.count) [line appendString:@" none"];
                for (id k in [encoder->_tgLen.allKeys sortedArrayUsingSelector:@selector(compare:)])
                    [line appendFormat:@" %@=%@", k, encoder->_tgLen[k]];
                [line appendFormat:@" | STATIC %u", nvmtl_sass_program_static_shared(program)];
                /* the launch itself: the grid the kernel will read as [[threads_per_grid]],
                 *    the shared bytes we will program, and the packed constant block it reads its
                 *    sizes from. A GR exception comes from one of these three or from the data. */
                /* WHICH VARIANT: the cache is keyed per specialisation, so naming the one
                 *    that faults is enough to hand just that one back to the existing engine. */
                [line appendFormat:@" | VARIANT regs %u bytes %u",
                    nvmtl_sass_program_registers(program), nvmtl_sass_program_code_len(program)];
                [line appendFormat:@" | LAUNCH grid %ux%ux%u shared %u+%u",
                    threads[0], threads[1], threads[2],
                    nvmtl_sass_program_static_shared(program), window];
                [line appendString:@" block"];
                for (uint32_t k = 0; k < 10 && k * 4 < *block_bytes; k++)
                    [line appendFormat:@" %u", ((const uint32_t *)block)[k]];
                [line appendString:@"\n"];
                FILE *f = fopen(getenv("NVMTL_SASS_TABLELOG"), "a");
                if (f) { fputs(line.UTF8String, f); fclose(f); }
            }
            uint64_t self_address = nvmtl_vk_buffer_address(&table->_b);
            if (!self_address)
                return NVMTL_SASS_NO(error, 1, @"Direct SASS: the array_ref table has no address");
            nvmtl_retain_resource(encoder->_cb, table);
            resolved[i].index = i;        /* same rule: the packer indexes by ordinal */
            addresses[i] = self_address;
            nvmtl_sass_table_said(slots, first, filled);
            break;
        }
        case NVMTL_SASS_TG_OFFSET:
            resolved[i].index = nvmtl_sass_program_metal_index(program, i);
            break;
        case NVMTL_SASS_CONST_TABLE:
            resolved[i].index = params[i].index;    /* the table the record carried */
            break;
        case NVMTL_SASS_GRID_X:
        case NVMTL_SASS_GRID_Y:
        case NVMTL_SASS_GRID_Z:
            break;
        default:
            return NVMTL_SASS_NO(error, 1, @"Direct SASS: a parameter kind the launcher cannot resolve");
        }
    }

    int rc = nvmtl_sass_pack(resolved, count, addresses, count, handles, count,
                             tg_offsets, NVMTL_SASS_MAX_PARAMS,
                             nvmtl_sass_program_tables(program),
                             nvmtl_sass_program_table_count(program),
                             threads, block, block_capacity, block_bytes);
    if (rc) {
        /* Logged as well as returned: a silent refusal looks like a GPU fault with no cause. */
        nvlog("DIRECT_SASS_PACK REFUSED rc=%d params=%u — this dispatch did not run", rc, count);
        return NVMTL_SASS_NO(error, 1,
            [NSString stringWithFormat:@"Direct SASS: the parameter block was refused (%d)", rc]);
    }

    /* static shared memory the machine code declared, plus the window Metal asked for */
    *shared_bytes = nvmtl_sass_program_static_shared(program) + window;
    if (*shared_bytes > g_lim.maxComputeSharedMemorySize)
        return NVMTL_SASS_NO(error, 1, @"Direct SASS: the shared memory needed exceeds the device");
    /* EVERY parameter's resolved address, for EVERY dispatch — not only the array_ref kernels.
     *   `blend` has nine plain buffers and faulted at addr 0x0 with nothing on disk to say which one
     *   resolved to zero, because the per-dispatch record lived inside the address-table case. */
    if (getenv("NVMTL_SASS_TABLELOG")) {
        NSMutableString *line = [NSMutableString stringWithFormat:@"RESOLVED %@ count=%u",
            encoder->_ps && encoder->_ps->_function ? encoder->_ps->_function->_fname : @"?", count];
        for (uint32_t k = 0; k < count && k < 24; k++) {
            uint32_t metal = nvmtl_sass_program_metal_index(program, k);
            [line appendFormat:@" [%u kind %u idx %u a 0x%llx h 0x%llx]", k, resolved[k].kind, metal,
                (unsigned long long)addresses[k], (unsigned long long)handles[k]];
        }
        [line appendString:@"\n"];
        FILE *f = fopen(getenv("NVMTL_SASS_TABLELOG"), "a");
        if (f) { fputs(line.UTF8String, f); fclose(f); }
    }
    return YES;
}
