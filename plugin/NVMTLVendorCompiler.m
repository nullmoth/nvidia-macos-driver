/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#import <objc/message.h>
#include <dlfcn.h>
#include <stdint.h>
#include <string.h>

void nvlog(const char *fmt, ...);

#define NVMTL_VENDOR_PLUGIN \
    "/Library/GPUBundles/NVIDIAShared.bundle/Contents/MacOS/NVIDIAShared"

#define NVMTL_COMPILER_FLAGS 286ull

#define NVMTL_STAGE_FRAGMENT 0x1010u
#define NVMTL_STAGE_VERTEX   0x1011u
#define NVMTL_STAGE_COMPUTE  0x1012u

static bool nvmtl_vendor_chip_supported(void);

static bool nvmtl_vendor_enabled(void) {
    static int on = -1;
    if (on < 0) on = getenv("NVMTL_VENDOR_COMPILER") ? 1 : 0;
    return on == 1 && nvmtl_vendor_chip_supported();
}

static const char *nvmtl_vendor_plugin_path(void) {
    const char *p = getenv("NVMTL_VENDOR_PLUGIN_PATH");
    return (p && *p && nvmtl_code_path_allowed(p)) ? p : NVMTL_VENDOR_PLUGIN;
}

struct nvmtl_hwinfo {
    uint32_t magic;
    uint32_t version;
    uint32_t arch_family;
    uint32_t arch_subtype;
    uint32_t sm_major, sm_minor;
    uint32_t flags, reserved0;
    uint64_t reserved[4];
};
#define NVMTL_HWINFO_MAGIC   0x5748564Eu
#define NVMTL_FAMILY_NVIDIA  0x01000016u

static const struct nvmtl_hwinfo *nvmtl_vendor_hwinfo(void) {
    static struct nvmtl_hwinfo hw;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        hw.magic       = NVMTL_HWINFO_MAGIC;
        hw.version     = 1;
        hw.arch_family = NVMTL_FAMILY_NVIDIA;
        static const struct { const char *prefix; uint32_t maj, min; } kChipSm[] = {
            { "TU1",   7, 5 },  { "GA100", 8, 0 },  { "GA10B", 8, 7 },  { "GA10", 8, 6 },
            { "GH100", 9, 0 },  { "AD10",  8, 9 },  { "GB10", 10, 0 },  { "GB20", 12, 0 },
        };
        // Unknown hardware must use the regular NVK/NAK path. Guessing Blackwell
        // here can produce cubins with instructions another GPU cannot execute.
        hw.sm_major = 0; hw.sm_minor = 0;
        const char *vk = nvmtl_vk_device_name();
        const char *chip = vk ? strstr(vk, "(NVK ") : NULL;
        bool known = false;
        if (chip) {
            chip += 5;
            for (size_t i = 0; i < sizeof kChipSm / sizeof kChipSm[0] && !known; i++)
                if (strncmp(chip, kChipSm[i].prefix, strlen(kChipSm[i].prefix)) == 0) {
                    hw.sm_major = kChipSm[i].maj; hw.sm_minor = kChipSm[i].min; known = true;
                }
        }
        if (known) nvlog("vendor-compiler: sm_%u%u read from the chip (%s)", hw.sm_major, hw.sm_minor, vk);
        else       nvlog("vendor-compiler: REFUSED unknown chip in \"%s\"; using NVK/NAK",
                         vk ? vk : "(no vulkan name)");
    });
    return &hw;
}

static bool nvmtl_vendor_chip_supported(void) {
    return nvmtl_vendor_hwinfo()->sm_major != 0;
}

static dispatch_data_t nvmtl_vendor_target_data(void) {
    static dispatch_data_t dd;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        dd = dispatch_data_create(nvmtl_vendor_hwinfo(), sizeof(struct nvmtl_hwinfo),
                                  dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0),
                                  DISPATCH_DATA_DESTRUCTOR_DEFAULT);
    });
    return dd;
}

struct nvmtl_cxx_string { char raw[24]; };
static void nvmtl_cxx_string(struct nvmtl_cxx_string *s, const char *text) {
    size_t n = strlen(text);
    memset(s, 0, sizeof *s);
    if (n > 22) n = 22;
    memcpy(s->raw, text, n);
    s->raw[23] = (char)(n & 0x7f);
}

static bool nvmtl_vendor_apple_loader(const char *path) {
    static int result = -1;
    if (result >= 0) return result == 1;
    result = 0;

    void *fw = dlopen("/System/Library/PrivateFrameworks/MTLCompiler.framework/MTLCompiler",
                      RTLD_NOW | RTLD_LOCAL);
    if (!fw) { nvlog("vendor compiler: cannot load MTLCompiler.framework: %s", dlerror()); return false; }

    void *(*Create)(const void *) = (void *(*)(const void *))dlsym(fw, "MTLCodeGenServiceCreate");
    void (*SetPluginPath)(void *, const char *, const void *, unsigned long) =
        (void (*)(void *, const char *, const void *, unsigned long))
            dlsym(fw, "MTLCodeGenServiceSetPluginPath");
    void (*Destroy)(void *) = (void (*)(void *))dlsym(fw, "MTLCodeGenServiceDestroy");
    if (!Create || !SetPluginPath) {
        nvlog("vendor compiler: MTLCompiler.framework is missing MTLCodeGenServiceCreate/SetPluginPath");
        return false;
    }

    struct nvmtl_cxx_string name;
    nvmtl_cxx_string(&name, "nvmtl");
    void *cgs = Create(&name);
    if (!cgs) { nvlog("vendor compiler: MTLCodeGenServiceCreate returned nil"); return false; }

    SetPluginPath(cgs, path, nvmtl_vendor_hwinfo(), (unsigned long)sizeof(struct nvmtl_hwinfo));

    void *ours = dlopen(path, RTLD_NOLOAD);
    nvlog("vendor compiler: Apple's loader ran for %s; plugin %s in this process", path,
          ours ? "IS resident" : "is NOT resident");
    result = ours ? 1 : 0;
    if (Destroy) Destroy(cgs);
    return result == 1;
}

struct nvmtl_nvs_reply {
    uint32_t magic, version, stage, flags;
    uint64_t cubin_offset, cubin_size, ptx_offset, ptx_size, air_size, reserved;
};
#define NVMTL_NVS_REPLY_MAGIC 0x524D564Eu

static void *nvmtl_vendor_compiler_handle(void) {
    static void *c;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *path = nvmtl_vendor_plugin_path();
        void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h) { nvlog("vendor compiler: dlopen(%s) failed: %s", path, dlerror()); return; }
        void *(*VCreate)(void *, void *) =
            (void *(*)(void *, void *))dlsym(h, "MTLCompilerCreate");
        if (!VCreate) { nvlog("vendor compiler: %s exports no MTLCompilerCreate", path); return; }
        c = VCreate(NULL, (void *)nvmtl_vendor_hwinfo());
        nvlog("vendor compiler: MTLCompilerCreate -> %p (%s)", c, path);
    });
    return c;
}

static uint32_t nvmtl_vendor_stage_tag(NSString *stage) {
    if ([stage isEqualToString:@"compute"] || [stage isEqualToString:@"kernel"])
        return NVMTL_STAGE_COMPUTE;
    if ([stage isEqualToString:@"fragment"]) return NVMTL_STAGE_FRAGMENT;
    if ([stage isEqualToString:@"vertex"])   return NVMTL_STAGE_VERTEX;
    return 0;
}

static size_t nvmtl_vendor_compile_one(NSString *name, NSString *airText, NSString *stage) {
    void *c = nvmtl_vendor_compiler_handle();
    if (!c) return 0;
    static int (*Build)(void *, uint32_t, const char *, const void **, size_t *, const char **);
    static void (*Release)(void *);
    if (!Build) {
        void *h = dlopen(nvmtl_vendor_plugin_path(), RTLD_NOW | RTLD_LOCAL);
        Build = h ? (int (*)(void *, uint32_t, const char *, const void **, size_t *,
                             const char **))dlsym(h, "NVSCompileAIRText") : NULL;
        Release = h ? (void (*)(void *))dlsym(h, "MTLCompilerReleaseReply") : NULL;
        if (!Build) { nvlog("vendor compiler: no NVSCompileAIRText in the plugin"); return 0; }
    }
    uint32_t tag = nvmtl_vendor_stage_tag(stage);
    if (!tag) {
        nvlog("vendor compiler: %s has stage '%s', which is not one of the three measured tags "
              "- refused", name.UTF8String, stage.UTF8String);
        return 0;
    }

    const void *reply = NULL; size_t len = 0; const char *err = NULL;
    int rc = Build(c, tag, airText.UTF8String, &reply, &len, &err);
    if (rc != 0 || !reply || len < sizeof(struct nvmtl_nvs_reply)) {
        nvlog("vendor compiler: %s (stage %s) FAILED rc=%d: %s", name.UTF8String,
              stage.UTF8String, rc, err ? err : "(no message)");
        return 0;
    }
    const struct nvmtl_nvs_reply *h = reply;
    if (h->magic != NVMTL_NVS_REPLY_MAGIC) {
        nvlog("vendor compiler: %s reply magic %08x, expected NVMR", name.UTF8String, h->magic);
        return 0;
    }
    const unsigned char *cu = (const unsigned char *)reply + h->cubin_offset;
    bool elf = h->cubin_size > 4 && cu[0] == 0x7f && cu[1] == 'E' && cu[2] == 'L' && cu[3] == 'F';
    nvlog("vendor compiler: %s (stage %s, tag %#x) -> %llu-byte cubin%s, %llu-byte PTX, from %llu "
          "chars of AIR", name.UTF8String, stage.UTF8String, tag,
          (unsigned long long)h->cubin_size, elf ? " (ELF)" : " (NOT an ELF - suspect)",
          (unsigned long long)h->ptx_size, (unsigned long long)h->air_size);
    size_t n = elf ? (size_t)h->cubin_size : 0;
    if (Release) Release(c);
    return n;
}

void nvmtl_vendor_compile_airs(NSDictionary<NSString *, NSString *> *airs,
                              NSDictionary<NSString *, NSString *> *stages) {
    if (!nvmtl_vendor_enabled() || airs.count == 0) return;
    nvmtl_vendor_apple_loader(nvmtl_vendor_plugin_path());
    NSUInteger ok = 0, refused = 0;
    size_t bytes = 0;
    for (NSString *name in airs) {
        if (!([stages[name] isEqualToString:@"compute"] || [stages[name] isEqualToString:@"kernel"])) continue;
        size_t n = nvmtl_vendor_compile_one(name, airs[name], stages[name] ?: @"?");
        if (n) { ok++; bytes += n; } else refused++;
    }
    nvlog("vendor compiler: %lu function(s) -> %lu cubin(s), %lu refused, %zu bytes of GPU code",
          (unsigned long)airs.count, (unsigned long)ok, (unsigned long)refused, bytes);
}

@interface NVMTLDevice (VendorCompiler)
@end

@implementation NVMTLDevice (VendorCompiler)

- (id)compiler {
    struct objc_super sup = { self, class_getSuperclass(object_getClass(self)) };
    if (!nvmtl_vendor_enabled())
        return ((id (*)(struct objc_super *, SEL))objc_msgSendSuper)(&sup, @selector(compiler));

    static id mine;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        Class k = objc_getClass("MTLCompiler");
        SEL s = sel_registerName("initWithTargetData:cacheUUID:pluginPath:device:compilerFlags:");
        if (!k || !class_getInstanceMethod(k, s)) {
            nvlog("vendor compiler: MTLCompiler has no initWithTargetData:… on this OS - "
                  "falling back to Apple's compiler");
            return;
        }
        static unsigned char cacheUUID[32];
        if (!cacheUUID[0]) arc4random_buf(cacheUUID, sizeof cacheUUID);
        NSString *path = [NSString stringWithUTF8String:nvmtl_vendor_plugin_path()];
        id c = [k alloc];
        mine = ((id (*)(id, SEL, id, unsigned char *, id, id, unsigned long long))objc_msgSend)(
                   c, s, (id)nvmtl_vendor_target_data(), cacheUUID, path, self,
                   NVMTL_COMPILER_FLAGS);
        nvlog("vendor compiler: -compiler built MTLCompiler %p with pluginPath %s flags %llu",
              mine, path.UTF8String, NVMTL_COMPILER_FLAGS);
    });
    if (mine) return mine;
    return ((id (*)(struct objc_super *, SEL))objc_msgSendSuper)(&sup, @selector(compiler));
}

@end

int nvmtl_vk_cmd_dispatch_sass(nvk_cmdbuf *c, const void *sass, uint32_t sass_len,
                               uint32_t regs, uint32_t smem, uint32_t barriers, uint32_t nparams,
                               const uint32_t threads[3], const uint32_t tg[3]);

#define NVMTL_CB0_PARAM_BASE 0x380u

#define NVMTL_EIFMT_NVAL 0x01u
#define NVMTL_EIFMT_BVAL 0x02u
#define NVMTL_EIFMT_HVAL 0x03u
#define NVMTL_EIFMT_SVAL 0x04u
#define NVMTL_EIATTR_REGCOUNT    0x2fu
#define NVMTL_EIATTR_KPARAM_INFO 0x17u

struct nvmtl_sass_kernel {
    uint32_t regs, smem, barriers, nparams;
    uint32_t text_len;
    const uint8_t *text;
};

static int nvmtl_elf_section(const uint8_t *b, size_t n, const char *want,
                            const uint8_t **out, uint32_t *out_len)
{
    if (n < 0x40 || memcmp(b, "\x7f" "ELF", 4) != 0 || b[4] != 2 || b[5] != 1) return 0;
    uint64_t shoff; uint16_t shentsize, shnum, shstrndx;
    memcpy(&shoff, b + 0x28, 8);
    memcpy(&shentsize, b + 0x3a, 2); memcpy(&shnum, b + 0x3c, 2); memcpy(&shstrndx, b + 0x3e, 2);
    if (shoff + (uint64_t)shentsize * shnum > n || shstrndx >= shnum) return 0;
    uint64_t stroff; memcpy(&stroff, b + shoff + (uint64_t)shentsize * shstrndx + 0x18, 8);
    if (stroff >= n) return 0;
    for (uint16_t i = 0; i < shnum; i++) {
        const uint8_t *sh = b + shoff + (uint64_t)shentsize * i;
        uint32_t nameoff; uint64_t off, size;
        memcpy(&nameoff, sh + 0x00, 4);
        memcpy(&off, sh + 0x18, 8);
        memcpy(&size, sh + 0x20, 8);
        if (stroff + nameoff >= n) continue;
        const char *nm = (const char *)b + stroff + nameoff;
        if (strcmp(nm, want) != 0) continue;
        if (off + size > n) return 0;
        if (out) *out = b + off;
        if (out_len) *out_len = (uint32_t)size;
        return 1;
    }
    return 0;
}

static int nvmtl_nv_info_walk(const uint8_t *d, uint32_t n, uint32_t *regs_out,
                              uint32_t *kparams_out, int *nonzero_extra)
{
    uint32_t i = 0;
    while (i + 4 <= n) {
        uint8_t fmt = d[i], attr = d[i + 1];
        uint16_t field; memcpy(&field, d + i + 2, 2);
        i += 4;
        if (fmt == NVMTL_EIFMT_SVAL) {
            if (i + field > n) return 0;
            if (attr == NVMTL_EIATTR_REGCOUNT && field >= 8 && regs_out)
                memcpy(regs_out, d + i + 4, 4);
            else if (attr == NVMTL_EIATTR_KPARAM_INFO && kparams_out)
                (*kparams_out)++;
            else if (field == 8 && nonzero_extra) {

                uint32_t v; memcpy(&v, d + i + 4, 4);
                if (v) *nonzero_extra = 1;
            }
            i += field;
        } else if (fmt == NVMTL_EIFMT_HVAL || fmt == NVMTL_EIFMT_BVAL || fmt == NVMTL_EIFMT_NVAL) {
            (void)field;
        } else {
            nvlog("cubin: unknown EIATTR format %#x at .nv.info+%u — refusing to keep walking",
                  fmt, i - 4);
            return 0;
        }
    }
    return 1;
}

static int nvmtl_cubin_parse(NSData *cubin, NSString *kernel, struct nvmtl_sass_kernel *out)
{
    const uint8_t *b = cubin.bytes;
    size_t n = cubin.length;
    char name[256];
    const uint8_t *sec; uint32_t len;

    memset(out, 0, sizeof *out);

    snprintf(name, sizeof name, ".text.%s", kernel.UTF8String);
    if (!nvmtl_elf_section(b, n, name, &out->text, &out->text_len)) {
        nvlog("cubin: no %s section — REFUSED", name);
        return -1;
    }

    uint32_t regs = 0, kparams = 0;
    int nonzero_extra = 0;
    if (!nvmtl_elf_section(b, n, ".nv.info", &sec, &len) ||
        !nvmtl_nv_info_walk(sec, len, &regs, NULL, &nonzero_extra)) {
        nvlog("cubin: .nv.info unreadable — REFUSED (a guessed register count silently corrupts "
              "a neighbouring CTA)");
        return -2;
    }
    if (!regs) { nvlog("cubin: .nv.info has no REGCOUNT — REFUSED"); return -3; }
    if (nonzero_extra) {
        nvlog("cubin: %s needs local memory or a call stack (a non-zero stack/frame entry) and the "
              "NVK hook pins both at 0 — REFUSED", kernel.UTF8String);
        return -4;
    }

    snprintf(name, sizeof name, ".nv.info.%s", kernel.UTF8String);
    if (nvmtl_elf_section(b, n, name, &sec, &len))
        nvmtl_nv_info_walk(sec, len, NULL, &kparams, NULL);

    snprintf(name, sizeof name, ".nv.constant0.%s", kernel.UTF8String);
    uint32_t from_cb = 0;
    if (nvmtl_elf_section(b, n, name, NULL, &len) && len > NVMTL_CB0_PARAM_BASE)
        from_cb = (len - NVMTL_CB0_PARAM_BASE) / 8;

    if (kparams && from_cb && kparams != from_cb) {
        nvlog("cubin: %s says %u parameter(s) in .nv.info but %u in .nv.constant0 (%u B) — "
              "REFUSED rather than picking one", kernel.UTF8String, kparams, from_cb, len);
        return -5;
    }
    out->nparams  = kparams ? kparams : from_cb;
    out->regs     = regs;
    out->smem     = 0;

    out->barriers = 0;
    nvlog("cubin: %s -> %u B of SASS, regs %u, %u parameter(s) (.nv.info %u / .nv.constant0 %u)",
          kernel.UTF8String, out->text_len, out->regs, out->nparams, kparams, from_cb);
    return 0;
}

@interface NVMTLComputeVariant : NSObject {
  @public
    NSData  *_cubin;
    NSData  *_ptx;
    const uint8_t *_text;
    uint32_t _textLen, _regs, _smem, _barriers, _nparams;
    NSString *_name;
}
@end

@implementation NVMTLComputeVariant

- (instancetype)initWithCompilerOutput:(id)out device:(id)dev pipelineStatisticsOutput:(id)stats
{
    if (!(self = [super init])) return nil;
    (void)dev; (void)stats;

    NSData *blob = [NSData dataWithData:(NSData *)out];
    if (blob.length < sizeof(struct nvmtl_nvs_reply)) {
        nvlog("variant: compiler output is %lu bytes, too short for our reply header",
              (unsigned long)blob.length);
        return nil;
    }
    const struct nvmtl_nvs_reply *h = blob.bytes;
    if (h->magic != NVMTL_NVS_REPLY_MAGIC) {
        nvlog("variant: compiler output magic %08x is not 'NVMR' — this reply is not ours",
              h->magic);
        return nil;
    }
    if (h->cubin_offset + h->cubin_size > blob.length || !h->cubin_size) {
        nvlog("variant: the cubin does not lie inside the reply (offset %llu size %llu of %lu)",
              (unsigned long long)h->cubin_offset, (unsigned long long)h->cubin_size,
              (unsigned long)blob.length);
        return nil;
    }
    _cubin = [NSData dataWithBytes:(const char *)blob.bytes + h->cubin_offset
                            length:(NSUInteger)h->cubin_size];
    if (h->ptx_size && h->ptx_offset + h->ptx_size <= blob.length)
        _ptx = [NSData dataWithBytes:(const char *)blob.bytes + h->ptx_offset
                              length:(NSUInteger)h->ptx_size];
    return self;
}

- (BOOL)nvmtlBindKernel:(NSString *)name
{
    struct nvmtl_sass_kernel k;
    if (nvmtl_cubin_parse(_cubin, name, &k) != 0) return NO;
    _name     = name;
    _text     = k.text;
    _textLen  = k.text_len;
    _regs     = k.regs;
    _smem     = k.smem;
    _barriers = k.barriers;
    _nparams  = k.nparams;
    return YES;
}

@end

@interface NVMTLDevice (VendorVariant)
@end

@implementation NVMTLDevice (VendorVariant)

- (char **)newTranslatedDriverCompilerOptions:(id)options compilerOptionsSize:(uint64_t *)size
{
    if (size) *size = 0;
    if (options && nvmtl_vendor_enabled())
        nvlog("vendor variant: driverCompilerOptions is a %s — not translated yet, 0 options passed",
              object_getClassName(options));
    return NULL;
}

- (void)freeTranslatedDriverCompilerOptions:(char **)options compilerOptionsSize:(uint64_t)size
{
    (void)options; (void)size;
}

- (char *)getComputeFunctionId:(const void *)script compilerOptions:(char **)options
           compilerOptionsSize:(uint64_t)size
{
    (void)script; (void)options; (void)size;
    return NULL;
}

- (char *)getComputeFunctionId:(const void *)script function:(id)function
               compilerOptions:(char **)options compilerOptionsSize:(uint64_t)size
{
    (void)script; (void)function; (void)options; (void)size;
    return NULL;
}

- (id)computeVariantWithCompilerOutput:(id)compilerOutput pipelineStatisticsOutput:(id)stats
{
    if (!compilerOutput) return nil;
    NVMTLComputeVariant *v = [[NVMTLComputeVariant alloc] initWithCompilerOutput:compilerOutput
                                                                         device:self
                                                       pipelineStatisticsOutput:stats];
    nvlog("vendor variant: computeVariantWithCompilerOutput: -> %s",
          v ? "NVMTLComputeVariant" : "nil (the output was not ours)");
    return v;
}

@end

static const void *gNVMTLVariantAssoc = &gNVMTLVariantAssoc;

void nvmtl_vendor_attach_sass(id device, id ps, id function)
{
    if (!nvmtl_vendor_enabled() || !ps || !function || !device) return;
    NVMTLFunction *f = (NVMTLFunction *)function;
    if (!f->_air.length) { nvlog("vendor variant: %s has no AIR text", f->_fname.UTF8String); return; }

    void *c = nvmtl_vendor_compiler_handle();
    if (!c) return;
    static int (*Build)(void *, uint32_t, const char *, const void **, size_t *, const char **);
    static void (*Release)(void *);
    if (!Build) {
        void *h = dlopen(nvmtl_vendor_plugin_path(), RTLD_NOW | RTLD_LOCAL);
        Build = h ? (int (*)(void *, uint32_t, const char *, const void **, size_t *,
                             const char **))dlsym(h, "NVSCompileAIRText") : NULL;
        Release = h ? (void (*)(void *))dlsym(h, "MTLCompilerReleaseReply") : NULL;
        if (!Build) { nvlog("vendor variant: no NVSCompileAIRText in the plugin"); return; }
    }

    const void *reply = NULL; size_t len = 0; const char *err = NULL;
    if (Build(c, NVMTL_STAGE_COMPUTE, f->_air.UTF8String, &reply, &len, &err) != 0 || !reply || !len) {
        nvlog("vendor variant: %s did not compile: %s", f->_fname.UTF8String,
              err ? err : "(no message)");
        return;
    }
    dispatch_data_t out = dispatch_data_create(reply, len,
                              dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0),
                              DISPATCH_DATA_DESTRUCTOR_DEFAULT);
    if (Release) Release(c);

    id v = [device computeVariantWithCompilerOutput:(id)out pipelineStatisticsOutput:nil];
    if (!v) return;
    if (![v nvmtlBindKernel:f->_fname]) return;
    objc_setAssociatedObject(ps, gNVMTLVariantAssoc, v, OBJC_ASSOCIATION_RETAIN);
    NVMTLComputeVariant *cv = v;
    nvlog("vendor variant: %s -> pipeline %p holds a variant with %u B of SASS, regs %u, %u param(s)",
          f->_fname.UTF8String, (__bridge void *)ps, cv->_textLen, cv->_regs, cv->_nparams);
}

int nvmtl_vendor_dispatch_sass(id ps, nvk_cmdbuf *c, const uint32_t threads[3], const uint32_t tg[3])
{
    if (!nvmtl_vendor_enabled() || !ps || !c) return -1;
    NVMTLComputeVariant *v = objc_getAssociatedObject(ps, gNVMTLVariantAssoc);
    if (![v isKindOfClass:[NVMTLComputeVariant class]]) return -2;
    if (!v->_text || !v->_textLen) return -3;
    return nvmtl_vk_cmd_dispatch_sass(c, v->_text, v->_textLen, v->_regs, v->_smem, v->_barriers,
                                      v->_nparams, threads, tg);
}

@interface NVMTLLibrary (VendorTwin)
@end
@implementation NVMTLLibrary (VendorTwin)
@end

static pthread_key_t gNVMTLInApple;
static pthread_once_t gNVMTLInAppleOnce = PTHREAD_ONCE_INIT;
static void nvmtl_in_apple_init(void) { pthread_key_create(&gNVMTLInApple, NULL); }
static int nvmtl_in_apple(void)
{
    pthread_once(&gNVMTLInAppleOnce, nvmtl_in_apple_init);
    return pthread_getspecific(gNVMTLInApple) != NULL;
}
static void nvmtl_set_in_apple(int on)
{
    pthread_once(&gNVMTLInAppleOnce, nvmtl_in_apple_init);
    pthread_setspecific(gNVMTLInApple, on ? (void *)1 : NULL);
}

id nvmtl_vendor_apple_pipeline(id device, id function)
{
    if (!nvmtl_vendor_enabled() || !device || !function) return nil;
    if (nvmtl_in_apple()) return nil;
    if (!getenv("NVMTL_APPLE_PIPELINE")) return nil;

    NVMTLFunction *f = (NVMTLFunction *)function;
    if (![f isKindOfClass:[NVMTLFunction class]] || !f->_lib) {
        nvlog("apple pipeline: no owning library on \"%s\" — cannot reach Apple's twin",
              f->_fname.UTF8String);
        return nil;
    }

    id twin = [(id)device nvmtlAppleTwinOf:(NVMTLLibrary *)f->_lib];
    if (!twin) { nvlog("apple pipeline: no Apple twin for this library"); return nil; }

    NSError *fe = nil;
    id af = nil;
    if (f->_fcv) af = [twin newFunctionWithName:f->_fname constantValues:f->_fcv error:&fe];
    if (!af) af = [twin newFunctionWithName:f->_fname];
    if (!af) {
        nvlog("apple pipeline: the twin has no function \"%s\" — refusing rather than handing "
              "Apple's compiler a function it cannot read", f->_fname.UTF8String);
        return nil;
    }

    MTLComputePipelineDescriptor *d = [MTLComputePipelineDescriptor new];
    d.computeFunction = af;
    d.label = f->_fname;

    SEL sel = @selector(newComputePipelineStateWithDescriptor:error:);
    struct objc_super sup = { device, class_getSuperclass(object_getClass(device)) };
    NSError *e = nil;
    nvmtl_set_in_apple(1);
    id ps = ((id (*)(struct objc_super *, SEL, id, NSError **))objc_msgSendSuper)(&sup, sel, d, &e);
    nvmtl_set_in_apple(0);

    nvlog("apple pipeline: Apple's own newComputePipelineStateWithDescriptor: for \"%s\" -> %s%s",
          f->_fname.UTF8String, ps ? object_getClassName(ps) : "nil",
          ps ? "" : (e ? [[e localizedDescription] UTF8String] ?: " (no reason)" : " (no error)"));
    return ps;
}
