/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <objc/runtime.h>
#import <objc/message.h>
#import <mach/mach.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <syslog.h>
#include <unistd.h>
#include <stdint.h>
#include <vulkan/vulkan_core.h>
#include <pthread.h>
#include <stdatomic.h>
#include <time.h>
#include "nvmtl_sample_positions.h"
#include "nvmtl_log_failure.h"
#include <sys/sysctl.h>
#ifndef NVMTL_RELEASE
#define NVMTL_RELEASE 0
#endif
#if NVMTL_RELEASE
static inline int nvmtl_rel_devpath(const char *p) {
    return p && (!strncmp(p, "/tmp/nvmtl-", 11) || !strncmp(p, "/private/tmp/nvmtl-", 19));
}
static inline int nvmtl_rel_access(const char *p, int m) { return nvmtl_rel_devpath(p) ? -1 : access(p, m); }
static inline FILE *nvmtl_rel_fopen(const char *p, const char *m) { return nvmtl_rel_devpath(p) ? NULL : fopen(p, m); }
// Function-like on purpose: an object-like `#define access` also renamed every Objective-C `-access` method in this
// one-TU plugin, so release builds answered -[MTLBinding access] with nothing (selector nvmtl_rel_access in the
// shipped binary). Apple's OpenCL then read every image as read-only and refused write-only images (-38).
#define access(p, m) nvmtl_rel_access(p, m)
#define fopen(p, m) nvmtl_rel_fopen(p, m)
#endif
int nvmtl_vk_init(void);
const char *nvmtl_vk_device_name(void);
void nvmtl_vk_counts(uint64_t *bm, uint64_t *bg, uint64_t *im, uint64_t *ig, size_t *bytes);
void nvmtl_vk_cmd_counts(uint64_t *begun, uint64_t *submitted, uint64_t *abandoned);

static int nvmtl_sample_payload_abi(id value) {
    if (![value isKindOfClass:[NSDictionary class]]) return 0;
    const unsigned long long expected[] = {NVMTL_SAMPLE_POSITION_OFFSET, NVMTL_SAMPLE_POSITION_BYTES, NVMTL_MAX_SAMPLE_POSITIONS, sizeof(nvmtl_sample_position)};
    NSArray *keys = @[@"offset", @"size", @"positions", @"stride"];
    for (NSUInteger i = 0; i < keys.count; ++i) {
        id number = value[keys[i]];
        if (![number isKindOfClass:[NSNumber class]]) return 0;
        const char *type = [number objCType];
        if (!type || !type[0] || type[1] || !strchr("cCsSiIlLqQ", type[0])) return 0;
        if ([number longLongValue] < 0 || [number unsignedLongLongValue] != expected[i]) return 0;
    }
    return 1;
}

#define NVLOG_SITES    256
#define NVLOG_VERBATIM 8
#define NVLOG_EVERY    4096
static struct { const char *fmt; uint64_t n; } g_nvlog_sites[NVLOG_SITES];
static pthread_mutex_t g_nvlog_lock = PTHREAD_MUTEX_INITIALIZER;
static int nvlog_dropped(const char *fmt, uint64_t *rep) {
    *rep = 0;
    pthread_mutex_lock(&g_nvlog_lock);
    int i = 0;
    for (; i < NVLOG_SITES; i++) {
        if (g_nvlog_sites[i].fmt == fmt) break;
        if (!g_nvlog_sites[i].fmt) { g_nvlog_sites[i].fmt = fmt; break; }
    }
    if (i == NVLOG_SITES) { pthread_mutex_unlock(&g_nvlog_lock); return 0; }
    uint64_t n = ++g_nvlog_sites[i].n;
    pthread_mutex_unlock(&g_nvlog_lock);
    if (n <= NVLOG_VERBATIM) return 0;
    if (n % NVLOG_EVERY == 0) { *rep = n; return 0; }
    return 1;
}
const char *nvmtl_log_path(void) {
#if NVMTL_RELEASE
    return "/private/tmp/nvmtl-off.log";
#endif
    // A diagnostic probe needs its own file even when the system log is writable.
    // Otherwise the per-run fault watcher reads an empty file and misses GPU faults.
    const char *requested = getenv("NVMTL_LOG_PATH");
    if (requested && *requested) return requested;
    static char path[1024]; static int decided;
    if (!decided) {
        decided = 1;
        FILE *f = fopen("/tmp/nvmtl.log", "a");
        if (f) { fclose(f); snprintf(path, sizeof path, "/tmp/nvmtl.log"); }
        else { const char *t = getenv("TMPDIR"); snprintf(path, sizeof path, "%s/nvmtl.log", (t && *t) ? t : "/private/tmp"); }
    }
    return path;
}
void nvlog(const char *fmt, ...) {
    uint64_t rep; if (nvlog_dropped(fmt, &rep)) return;
    char b[512]; va_list a; va_start(a, fmt); vsnprintf(b, sizeof b, fmt, a); va_end(a);
    char t[640];
    if (rep) snprintf(t, sizeof t, "%s   [logged %llu times now]", b, (unsigned long long)rep);
    else     snprintf(t, sizeof t, "%s", b);
#if NVMTL_RELEASE
    if (nvmtl_log_failure(t)) syslog(LOG_ERR, "NullMoth: %s", t);
#else
    syslog(LOG_NOTICE, "NVMTL: %s", t);
    FILE *f = fopen(nvmtl_log_path(), "a"); if (f) { fprintf(f, "pid %d NVMTL: %s\n", getpid(), t); fclose(f); }
#endif
}

#define NVMTL_OOM_STREAK      32
#define NVMTL_OOM_COOLDOWN_NS (250ull * 1000ull * 1000ull)
static _Atomic uint64_t g_oom_streak, g_oom_until;
static int nvmtl_alloc_blocked(void) {
    if (atomic_load(&g_oom_streak) < NVMTL_OOM_STREAK) return 0;
    if (clock_gettime_nsec_np(CLOCK_MONOTONIC) >= atomic_load(&g_oom_until)) {
        atomic_store(&g_oom_streak, NVMTL_OOM_STREAK - 1);
        return 0;
    }
    return 1;
}
static void nvmtl_alloc_ok(void) { atomic_store(&g_oom_streak, 0); }
static void nvmtl_alloc_failed(const char *what) {
    uint64_t n = atomic_fetch_add(&g_oom_streak, 1) + 1;
    atomic_store(&g_oom_until, clock_gettime_nsec_np(CLOCK_MONOTONIC) + NVMTL_OOM_COOLDOWN_NS);
    if (n < NVMTL_OOM_STREAK) return;
    uint64_t bm = 0, bg = 0, im = 0, ig = 0; size_t bytes = 0;
    nvmtl_vk_counts(&bm, &bg, &im, &ig, &bytes);
    uint64_t cb = 0, cs = 0, ca = 0; nvmtl_vk_cmd_counts(&cb, &cs, &ca);
    nvlog("ALLOCATION FAILING (%s) - throttling %llu ms. buffers made %llu freed %llu LIVE %llu | "
          "images made %llu freed %llu LIVE %llu | holding %llu MB of VkDeviceMemory | cmdbufs begun %llu submitted %llu abandoned %llu",
          what, (unsigned long long)(NVMTL_OOM_COOLDOWN_NS / 1000000ull),
          (unsigned long long)bm, (unsigned long long)bg, (unsigned long long)(bm - bg),
          (unsigned long long)im, (unsigned long long)ig, (unsigned long long)(im - ig),
          (unsigned long long)(bytes >> 20), (unsigned long long)cb, (unsigned long long)cs, (unsigned long long)ca);
}
@interface MTLIOAccelDevice : NSObject
- (instancetype)initWithAcceleratorPort:(mach_port_t)port;
@end
@protocol MTLDeviceSPI <NSObject> @end
@interface NVMTLDevice : MTLIOAccelDevice <MTLDevice, MTLDeviceSPI>
@end
#import "NVMTLObjects.h"
void nvmtl_vendor_compile_airs(NSDictionary<NSString *, NSString *> *airs,
                               NSDictionary<NSString *, NSString *> *stages);
void nvmtl_vendor_attach_sass(id device, id pipelineState, id function);
int  nvmtl_vendor_dispatch_sass(id pipelineState, nvk_cmdbuf *c,
                                const uint32_t threads[3], const uint32_t tg[3]);
id   nvmtl_vendor_apple_pipeline(id device, id function);
NSDictionary<NSString *, NSData *> *nvmtl_translate_metallib(NSData *lib, NSDictionary **stages, NSDictionary **airs);
void nvmtl_vendor_compile_airs(NSDictionary<NSString *, NSString *> *airs,
                               NSDictionary<NSString *, NSString *> *stages);
#include <sys/stat.h>
NSDictionary<NSString *, NSData *> *nvmtl_translate_metallib_k(NSData *lib, NSData *knownKey, NSDictionary **stages, NSDictionary **airs);
NSData *nvmtl_lib_sha256(NSData *lib);
NSData *nvmtl_libkey_for_file(NSString *path, NSData *lib, const struct stat *before);
int nvmtl_stat_same(const struct stat *a, const struct stat *b);
static char gNVMTLLibKeyAssoc, gNVMTLLibFileAssoc;
NSArray<NSString *> *nvmtl_externs_of_airs(NSDictionary<NSString *, NSString *> *airs);
NSSet<NSString *> *nvmtl_mtlb_extern_names(NSData *raw);
@interface NSObject (NVMTLExternSPI)
- (id)newExternFunctionWithName:(NSString *)name;
- (id)nvmtlAppleTwinOf:(id)lib;
@end
static nvk_queue *nvmtl_device_queue(void) {
    static nvk_queue q; static int made = 0;
    if (!made) { if (nvmtl_vk_queue_create(&q)) { nvlog("device queue FAILED"); return NULL; } made = 1; }
    return &q;
}
static bool nvmtl_accel_armed(void)
{
    int v = 0; size_t n = sizeof v;
    if (sysctlbyname("debug.nvaccelfb", &v, &n, NULL, 0) != 0) return false;
    return v != 0;
}
#include <dlfcn.h>
#include <libgen.h>
// The video libraries tile and untile frames in the Blackwell (GB20x) layout. Earlier GPUs' NVDEC/NVENC use the classic
// one: on an RTX 3070 Laptop (issue #65, measured against Apple's software codecs) H.264 decode showed shifted macroblock
// rows and a green stripe, and both encoders produced torn pictures. The decoder's only difference is its untiling mode
// (1 = Blackwell, 0 = classic), so earlier GPUs load libnvdec_h264_t2d.dylib, the same library with mode 0 (51 dB on
// GA104). The encoders are not registered on them, so apps use Apple's encoder until they are fixed for those GPUs.
static bool nvmtl_video_is_blackwell(void)
{
    return strstr(nvmtl_vk_device_name(), "NVK GB") != NULL;
}
static void nvmtl_nvdec_register(void)
{
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *me = getprogname();
        if (getenv("NVMTL_NO_NVDEC")) { nvlog("nvdec: NVMTL_NO_NVDEC set - not registering in %s", me); return; }
        if (!strcmp(me, "WindowServer")) return;
        typedef int32_t (*RegFn)(uint32_t, CFDictionaryRef, void *);
        RegFn reg = (RegFn)dlsym(RTLD_DEFAULT, "VTRegisterVideoDecoderWithInfo");
        if (!reg) { nvlog("nvdec: VideoToolbox is not loaded in %s - not registering", me); return; }
        Dl_info di;
        if (!dladdr((const void *)nvmtl_nvdec_register, &di) || !di.dli_fname) { nvlog("nvdec: dladdr failed - not registering"); return; }
        char dir[1024], path[1200];
        strlcpy(dir, di.dli_fname, sizeof dir);
        snprintf(path, sizeof path, "%s/%s", dirname(dir), nvmtl_video_is_blackwell() ? "libnvdec_h264.dylib" : "libnvdec_h264_t2d.dylib");
        void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h) { nvlog("nvdec: dlopen %s FAILED: %s", path, dlerror()); return; }
        void *fac = dlsym(h, "NVDecH264_CreateInstance");
        if (!fac) { nvlog("nvdec: %s has no NVDecH264_CreateInstance", path); return; }
        NSDictionary *info = @{ @"CMClassImplementationID": @"com.nvmtl.videodecoder.avc.nvdec",
                                @"VTRating": @1000, @"VTIsHardwareAccelerated": @YES };
        int32_t st = reg('avc1', (__bridge CFDictionaryRef)info, fac);
        nvlog("nvdec: H.264 decoder %s in %s (VTRegisterVideoDecoderWithInfo -> %d, rating 1000, %s)",
              st ? "NOT registered" : "REGISTERED", me, (int)st, path);
    });
}
static void nvmtl_nvenc_register(void)
{
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *me = getprogname();
        if (getenv("NVMTL_NO_NVENC")) { nvlog("nvenc: NVMTL_NO_NVENC set - not registering in %s", me); return; }
        if (!nvmtl_video_is_blackwell()) { nvlog("nvenc: %s is not Blackwell - H.264 encode stays on Apple's encoder", nvmtl_vk_device_name()); return; }
        if (!strcmp(me, "WindowServer")) return;
        typedef int32_t (*RegFn)(uint32_t, CFDictionaryRef, void *);
        RegFn reg = (RegFn)dlsym(RTLD_DEFAULT, "VTRegisterVideoEncoderWithInfo");
        if (!reg) { nvlog("nvenc: VideoToolbox is not loaded in %s - not registering", me); return; }
        Dl_info di;
        if (!dladdr((const void *)nvmtl_nvenc_register, &di) || !di.dli_fname) { nvlog("nvenc: dladdr failed - not registering"); return; }
        char dir[1024], path[1200];
        strlcpy(dir, di.dli_fname, sizeof dir);
        snprintf(path, sizeof path, "%s/libnvenc_h264.dylib", dirname(dir));
        void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h) { nvlog("nvenc: dlopen %s FAILED: %s", path, dlerror()); return; }
        void *fac = dlsym(h, "NVEncH264_CreateInstance");
        if (!fac) { nvlog("nvenc: %s has no NVEncH264_CreateInstance", path); return; }
        NSDictionary *info = @{ @"CMClassImplementationID": @"com.nvmtl.videoencoder.avc.nvenc",
                                @"CMClassImplementationName": @"NVIDIA NVENC H.264",
                                @"VTRating": @1000, @"VTIsHardwareAccelerated": @YES };
        int32_t st = reg('avc1', (__bridge CFDictionaryRef)info, fac);
        nvlog("nvenc: H.264 encoder %s in %s (VTRegisterVideoEncoderWithInfo -> %d, rating 1000, %s)",
              st ? "NOT registered" : "REGISTERED", me, (int)st, path);
    });
}
static void nvmtl_nvdec_hevc_register(void)
{
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *me = getprogname();
        if (getenv("NVMTL_NO_NVDEC_HEVC")) { nvlog("nvdec: NVMTL_NO_NVDEC_HEVC set - not registering HEVC in %s", me); return; }
        if (!strcmp(me, "WindowServer")) return;
        typedef int32_t (*RegFn)(uint32_t, CFDictionaryRef, void *);
        RegFn reg = (RegFn)dlsym(RTLD_DEFAULT, "VTRegisterVideoDecoderWithInfo");
        if (!reg) { nvlog("nvdec: VideoToolbox is not loaded in %s - not registering HEVC", me); return; }
        Dl_info di;
        if (!dladdr((const void *)nvmtl_nvdec_hevc_register, &di) || !di.dli_fname) { nvlog("nvdec: dladdr failed - not registering HEVC"); return; }
        char dir[1024], path[1200];
        strlcpy(dir, di.dli_fname, sizeof dir);
        snprintf(path, sizeof path, "%s/libnvdec_hevc.dylib", dirname(dir));
        void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h) { nvlog("nvdec: dlopen %s FAILED: %s", path, dlerror()); return; }
        void *fac = dlsym(h, "NVDecHEVC_CreateInstance");
        if (!fac) { nvlog("nvdec: %s has no NVDecHEVC_CreateInstance", path); return; }
        NSDictionary *info = @{ @"CMClassImplementationID": @"com.nvmtl.videodecoder.hevc.nvdec",
                                @"VTRating": @1000, @"VTIsHardwareAccelerated": @YES };
        int32_t st = reg('hvc1', (__bridge CFDictionaryRef)info, fac);
        nvlog("nvdec: HEVC decoder %s in %s (VTRegisterVideoDecoderWithInfo -> %d, rating 1000, %s)",
              st ? "NOT registered" : "REGISTERED", me, (int)st, path);
    });
}
static void nvmtl_nvenc_hevc_register(void)
{
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *me = getprogname();
        if (getenv("NVMTL_NO_NVENC_HEVC")) { nvlog("nvenc: NVMTL_NO_NVENC_HEVC set - not registering HEVC in %s", me); return; }
        if (!nvmtl_video_is_blackwell()) { nvlog("nvenc: %s is not Blackwell - HEVC encode stays on Apple's encoder", nvmtl_vk_device_name()); return; }
        if (!strcmp(me, "WindowServer")) return;
        typedef int32_t (*RegFn)(uint32_t, CFDictionaryRef, void *);
        RegFn reg = (RegFn)dlsym(RTLD_DEFAULT, "VTRegisterVideoEncoderWithInfo");
        if (!reg) { nvlog("nvenc: VideoToolbox is not loaded in %s - not registering HEVC", me); return; }
        Dl_info di;
        if (!dladdr((const void *)nvmtl_nvenc_hevc_register, &di) || !di.dli_fname) { nvlog("nvenc: dladdr failed - not registering HEVC"); return; }
        char dir[1024], path[1200];
        strlcpy(dir, di.dli_fname, sizeof dir);
        snprintf(path, sizeof path, "%s/libnvenc_hevc.dylib", dirname(dir));
        void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
        if (!h) { nvlog("nvenc: dlopen %s FAILED: %s", path, dlerror()); return; }
        void *fac = dlsym(h, "NVEncHEVC_CreateInstance");
        if (!fac) { nvlog("nvenc: %s has no NVEncHEVC_CreateInstance", path); return; }
        NSDictionary *info = @{ @"CMClassImplementationID": @"com.nvmtl.videoencoder.hevc.nvenc",
                                @"CMClassImplementationName": @"NVIDIA NVENC HEVC",
                                @"VTRating": @1000, @"VTIsHardwareAccelerated": @YES };
        int32_t st = reg('hvc1', (__bridge CFDictionaryRef)info, fac);
        nvlog("nvenc: HEVC encoder %s in %s (VTRegisterVideoEncoderWithInfo -> %d, rating 1000, %s)",
              st ? "NOT registered" : "REGISTERED", me, (int)st, path);
    });
}
static bool nvmtl_process_is_allowed(void)
{
    const char *me = getprogname();
    if (!me) return false;
    // A headless NVIDIA GPU (the display is on the Intel or AMD GPU) is a Metal device for apps only: WindowServer stays
    // on the GPU that owns the display, as on Apple's two-GPU Macs. The list below cannot express this: "*" matches
    // WindowServer before its "!WindowServer" line is reached.
    { int hv = 0; size_t hn = sizeof hv;
      if (!strcmp(me, "WindowServer") && sysctlbyname("debug.nvaccel_headless", &hv, &hn, NULL, 0) == 0 && hv) {
          nvlog("  headless GPU: no device for WindowServer (it composites on the GPU that owns the display)"); return false; } }
    if (getenv("NVMTL_ENABLE")) return true;
    if (!strcmp(me, "mtlprobe") || !strcmp(me, "nvmtltest") || !strcmp(me, "nvmtlrender")) return true;
    FILE *f = fopen("/Library/GPUBundles/nvmtl-allow.txt", "r");
    if (!f) f = fopen("/Library/Extensions/nvmtl-allow.txt", "r");
    if (!f) { nvlog("  no allow-list readable from this process: no device for %s (cannot read nvmtl-allow.txt)", me); return false; }
    char line[256]; bool ok = false;
    while (fgets(line, sizeof line, f)) {
        char *nl = strpbrk(line, "\r\n"); if (nl) *nl = 0;
        char *p = line; while (*p == ' ' || *p == '\t') p++;
        if (!*p || *p == '#') continue;
        if (*p == '-') { p++; while (*p == ' ' || *p == '\t') p++; if (!strcmp(p, me)) { nvlog("  denied by list: no device for %s", me); fclose(f); return false; } continue; }
        bool armedOnly = false;
        if (*p == '!') { armedOnly = true; p++; while (*p == ' ' || *p == '\t') p++; }
        if (strcmp(p, "*") && strcmp(p, me)) continue;
        if (armedOnly && !nvmtl_accel_armed()) {
            nvlog("  '%s' is armed-only and debug.nvaccelfb reads 0 -- no device (arm it with sysctl -w debug.nvaccelfb=1)", me);
            break;
        }
        ok = true; break;
    }
    fclose(f);
    if (ok) nvlog("  allow-listed: %s", me);
    return ok;
}

@implementation NVMTLDevice
+ (void)load {
    char me[1200], ts[40] = "?"; time_t now = time(NULL); struct tm tm;
    if (localtime_r(&now, &tm)) strftime(ts, sizeof ts, "%Y-%m-%dT%H:%M:%S%z", &tm);
    nvlog("NVMTLDevice +load in pid %d (static class, image-owned) image %s at %s", getpid(),
          nvmtl_image_ident((const void *)nvmtl_image_ident, me, sizeof me), ts);
}
static void nvmtl_fill_pool_args(uint8_t *a, int second)
{
    memset(a, 0, 2440);
    a[0x005] = 0x04; a[0x008] = 0x01; a[0x00a] = 0x01; a[0x00c] = 0x01; a[0x01a] = 0x01;
    a[0x020] = 0x01; a[0x021] = 0x01; a[0x023] = 0x01; a[0x024] = 0x40; a[0x05a] = 0x01;
    a[0x105] = 0x10;
    if (second) { a[0x000] = 0x40; a[0x012] = 0x01; a[0x052] = 0x01; a[0x06a] = 0x04; }
}
static void nvmtl_install_hw_resource_pools(id dev)
{
    if (!getenv("NVMTL_HWPOOL")) { nvlog("  hw resource pools: NVMTL_HWPOOL unset, leaving slot 2 NULL"); return; }
    Class POOL = objc_getClass("MTLIOAccelResourcePool");
    Class RES  = objc_getClass("MTLIOAccelPooledResource");
    if (!POOL || !RES) { nvlog("  hw resource pools: MTLIOAccelResourcePool/PooledResource missing"); return; }
    SEL init = NSSelectorFromString(@"initWithDevice:resourceClass:resourceArgs:resourceArgsSize:options:");
    id (*mk)(id, SEL, id, Class, const void *, unsigned long, unsigned long) = (void *)objc_msgSend;
    static uint8_t args[3][2440];
    __unsafe_unretained id pools[3] = {0};
    for (int i = 0; i < 3; i++) {
        nvmtl_fill_pool_args(args[i], i == 1);
        pools[i] = mk([POOL alloc], init, dev, RES, args[i], 2440, 0);
        nvlog("  pool %d -> %p", i, (__bridge void *)pools[i]);
        if (!pools[i]) { nvlog("  hw resource pools: pool %d came back nil, NOT installing", i); return; }
    }
    void (*set)(id, SEL, __unsafe_unretained id *, int) = (void *)objc_msgSend;
    set(dev, NSSelectorFromString(@"setHwResourcePool:count:"), pools, 3);
    nvlog("  hw resource pools: installed 3 (slot 2 + count)");
}
- (instancetype)initWithAcceleratorPort:(mach_port_t)port {
    nvlog("-[NVMTLDevice initWithAcceleratorPort:%u] in %s", port, getprogname());
    self = [super initWithAcceleratorPort:port];
    if (!self) { nvlog("  super initWithAcceleratorPort: returned nil"); return nil; }
    if (!nvmtl_process_is_allowed()) { nvlog("  not allow-listed: no device for %s", getprogname()); return nil; }
    nvlog("MTLIOAccelDevice's initWithAcceleratorPort: -> %p", (__bridge void *)self);
    gNVMTLMainDevice = self;
    nvmtl_install_hw_resource_pools(self);
    [self nvmtlProbeGPUPass];
    if (nvmtl_vk_init()) {
        nvlog("  NVK did not come up in this process - refusing to be the Metal device for %s", getprogname());
        return nil;
    }
    nvmtl_nvdec_register();
    nvmtl_nvenc_register();
    nvmtl_nvdec_hevc_register();
    nvmtl_nvenc_hevc_register();
    SEL il = sel_registerName("initLimits");
    if ([self respondsToSelector:il]) {
        ((void (*)(id, SEL))objc_msgSend)(self, il);
        #define NVMTL_U(n) ((unsigned long)((NSUInteger (*)(id, SEL))objc_msgSend)(self, sel_registerName(n)))
        nvlog("limits: -initLimits (featureProfile %lu) -> maxTextureWidth2D %lu, maxComputeTextures %lu, maxComputeBuffers %lu",
              NVMTL_U("featureProfile"), NVMTL_U("maxTextureWidth2D"), NVMTL_U("maxComputeTextures"), NVMTL_U("maxComputeBuffers"));
        #undef NVMTL_U
    } else nvlog("limits: -initLimits not found - every table-served limit stays 0 (CoreImage will refuse renders)");
    return self;
}

- (void)nvmtlProbeGPUPass {
    if (access("/Library/GPUBundles/nvmtl-gpupass-probe", F_OK) != 0) return;
    static int done = 0; if (done) return; done = 1;
    if (nvmtl_vk_init()) { nvlog("gpupass probe: Vulkan is down in this process — cannot answer"); return; }
    NSString *p = @"/System/Library/Frameworks/CoreDisplay.framework/Versions/A/Resources/default.metallib";
    NSData *d = [NSData dataWithContentsOfFile:p];
    if (!d) { nvlog("gpupass probe: cannot read CoreDisplay's metallib"); return; }
    NSDictionary<NSString *, NSData *> *fns = nvmtl_translate_metallib(d, NULL, NULL);
    nvlog("gpupass probe: %lu functions translated from CoreDisplay's metallib", (unsigned long)fns.count);
    NSData *v = fns[@"ViewportToNDC"];
    if (!v) { nvlog("gpupass probe: NO ViewportToNDC — the vertex half is missing"); return; }
    for (NSString *fname in @[@"GPUPass", @"ColorFill", @"TextureCopy"]) {
        NSData *f = fns[fname];
        if (!f) { nvlog("gpupass probe: no %s in the translated set", fname.UTF8String); continue; }
        nvk_pipeline pipe; memset(&pipe, 0, sizeof pipe);
        int rc = nvmtl_vk_pipeline_create_depth(v.bytes, v.length, f.bytes, f.length, 1, 0, &pipe);
        nvlog("gpupass probe: ViewportToNDC + %s -> %s   (%lu + %lu bytes of SPIR-V)",
              fname.UTF8String, rc ? "*** vkCreateGraphicsPipelines FAILED ***" : "*** PIPELINE BUILT ***",
              (unsigned long)v.length, (unsigned long)f.length);
    }
}
- (NSString *)vendorName {
    static NSMutableSet *seen; static dispatch_once_t once; dispatch_once(&once, ^{ seen = [NSMutableSet new]; });
    Dl_info i = {0}; const char *who = dladdr(__builtin_return_address(0), &i) && i.dli_fname ? i.dli_fname : "?";
    @synchronized(seen) { NSString *k = @(who); if (![seen containsObject:k]) { [seen addObject:k]; nvlog("vendorName -> NVIDIA (asked by %s)", who); } }
    return @"NVIDIA";
}
- (NSString *)name {
    static int vk = 0; if (!vk) { vk = 1; nvmtl_vk_init(); }
    const char *vkname = nvmtl_vk_device_name();
    NSString *full = [NSString stringWithUTF8String:(vkname ? vkname : "NVIDIA GPU")];
    NSRange cut = [full rangeOfString:@" (NVK "];
    NSString *marketing = (cut.location != NSNotFound) ? [full substringToIndex:cut.location] : full;
    return [NSString stringWithFormat:@"%@ (NVMTL over %@)", marketing, full];
}
- (id<MTLCommandQueue>)newCommandQueue { return [self newCommandQueueWithMaxCommandBufferCount:64]; }
- (id<MTLCommandQueue>)newCommandQueueWithMaxCommandBufferCount:(NSUInteger)n {
    if (nvmtl_vk_init()) { nvlog("newCommandQueue: Vulkan/NVK is down — no queue"); return nil; }
    NVMTLCommandQueue *q = [NVMTLCommandQueue new];
    q->_capacityCount = n ? n : 64;
    q->_dev = self;
    if (nvmtl_vk_queue_create(&q->_q)) { nvlog("newCommandQueue: command pool FAILED"); return nil; }
    nvlog("newCommandQueue -> %p (NVK command pool)", (__bridge void *)q); return q; }

static void nvmtl_zero_new_buffer(NVMTLBuffer *b, NSUInteger len);
static __thread int gZeroSkip;
- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)len options:(MTLResourceOptions)opt {
    if (nvmtl_vk_init()) return nil;
    if (nvmtl_alloc_blocked()) return nil;
    NVMTLBuffer *b = [NVMTLBuffer new];
    b->_storage = (MTLStorageMode)((opt >> MTLResourceStorageModeShift) & 0xF);
    b->_ropt = opt & ((MTLResourceOptions)0xF | ((MTLResourceOptions)0x3 << MTLResourceHazardTrackingModeShift));
    int hv = (b->_storage == MTLStorageModeShared) || (b->_storage == MTLStorageModeManaged && getenv("NVMTL_MANAGED_SYSMEM"));
    if (nvmtl_vk_buffer_create(len, (hv && b->_storage == MTLStorageModeShared) ? 2 : hv, &b->_b)) {
        if (hv) { nvmtl_alloc_failed("newBufferWithLength"); nvlog("newBufferWithLength: FAILED"); return nil; }
        nvlog("newBufferWithLength:%lu private: VRAM refused - placing it in system memory", (unsigned long)len);
        if (nvmtl_vk_buffer_create(len, 1, &b->_b)) { nvmtl_alloc_failed("newBufferWithLength"); nvlog("newBufferWithLength: FAILED"); return nil; }
    }
    nvmtl_alloc_ok();
    if (b->_storage == MTLStorageModeManaged && !b->_b.map && nvmtl_vk_buffer_create(len, 1, &b->_shadow)) {
        nvlog("newBufferWithLength:%lu managed: shadow refused - placing it in system memory", (unsigned long)len);
        memset(&b->_shadow, 0, sizeof b->_shadow); nvmtl_vk_buffer_destroy(&b->_b);
        if (nvmtl_vk_buffer_create(len, 1, &b->_b)) { nvmtl_alloc_failed("newBufferWithLength"); return nil; }
    }
    if (!gZeroSkip) nvmtl_zero_new_buffer(b, len);
    nvlog("newBufferWithLength:%lu storage %d -> %p%s", (unsigned long)len, (int)b->_storage, (__bridge void *)b, b->_b.map ? "" : " VRAM"); return b; }

- (id<MTLBuffer>)newBufferWithBytes:(const void *)bytes length:(NSUInteger)len options:(MTLResourceOptions)opt {
    int priv = (((opt >> MTLResourceStorageModeShift) & 0xF) == MTLStorageModePrivate);
    MTLResourceOptions o2 = priv ? ((opt & ~(MTLResourceOptions)(0xF << MTLResourceStorageModeShift)) | MTLResourceStorageModeShared) : opt;
    gZeroSkip = bytes != NULL;
    id<MTLBuffer> b = [self newBufferWithLength:len options:o2];
    gZeroSkip = 0;
    if (b && b.contents && bytes) memcpy(b.contents, bytes, len);
    if (b && priv) ((NVMTLBuffer *)b)->_storage = MTLStorageModePrivate;
    if (b && bytes && ((NVMTLBuffer *)b)->_shadow.map) [b didModifyRange:NSMakeRange(0, len)];
    return b; }

static int nvmtl_depthfmt(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect)
{
    switch (f) {
    case MTLPixelFormatDepth16Unorm: *vk = VK_FORMAT_D16_UNORM; *aspect = VK_IMAGE_ASPECT_DEPTH_BIT; return 0;
    case MTLPixelFormatDepth32Float: *vk = VK_FORMAT_D32_SFLOAT; *aspect = VK_IMAGE_ASPECT_DEPTH_BIT; return 0;
    case MTLPixelFormatStencil8: *vk = VK_FORMAT_S8_UINT; *aspect = VK_IMAGE_ASPECT_STENCIL_BIT; return 0;
    case MTLPixelFormatDepth24Unorm_Stencil8: *vk = VK_FORMAT_D24_UNORM_S8_UINT; *aspect = VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT; return 0;
    case MTLPixelFormatDepth32Float_Stencil8: *vk = VK_FORMAT_D32_SFLOAT_S8_UINT; *aspect = VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT; return 0;
    case MTLPixelFormatX32_Stencil8: *vk = VK_FORMAT_D32_SFLOAT_S8_UINT; *aspect = VK_IMAGE_ASPECT_STENCIL_BIT; return 0;
    case MTLPixelFormatX24_Stencil8: *vk = VK_FORMAT_D24_UNORM_S8_UINT; *aspect = VK_IMAGE_ASPECT_STENCIL_BIT; return 0;
    default: return -1;
    }
}
static int nvmtl_pixfmt(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8)
{
    *a8 = 0;
    switch (f) {
    case MTLPixelFormatA8Unorm:          *vk = 9  ;               *bpp = 1;  *a8 = 1; return 0;
    case MTLPixelFormatR8Unorm:          *vk = 9  ;               *bpp = 1;  return 0;
    case MTLPixelFormatR8Unorm_sRGB:     *vk = 15 ;                *bpp = 1;  return 0;
    case MTLPixelFormatR8Snorm:          *vk = 10 ;               *bpp = 1;  return 0;
    case MTLPixelFormatR8Uint:           *vk = 13 ;                *bpp = 1;  return 0;
    case MTLPixelFormatR8Sint:           *vk = 14 ;                *bpp = 1;  return 0;
    case MTLPixelFormatR16Unorm:         *vk = 70 ;              *bpp = 2;  return 0;
    case MTLPixelFormatR16Snorm:         *vk = 71 ;              *bpp = 2;  return 0;
    case MTLPixelFormatR16Uint:          *vk = 74 ;               *bpp = 2;  return 0;
    case MTLPixelFormatR16Sint:          *vk = 75 ;               *bpp = 2;  return 0;
    case MTLPixelFormatR16Float:         *vk = 76 ;             *bpp = 2;  return 0;
    case MTLPixelFormatRG8Unorm:         *vk = 16 ;             *bpp = 2;  return 0;
    case MTLPixelFormatRG8Unorm_sRGB:    *vk = 16 ;  *bpp = 2;  return 0;
    case MTLPixelFormatRG8Snorm:         *vk = 17 ;             *bpp = 2;  return 0;
    case MTLPixelFormatRG8Uint:          *vk = 20 ;              *bpp = 2;  return 0;
    case MTLPixelFormatRG8Sint:          *vk = 21 ;              *bpp = 2;  return 0;
    case MTLPixelFormatR32Uint:          *vk = 98 ;               *bpp = 4;  return 0;
    case MTLPixelFormatR32Sint:          *vk = 99 ;               *bpp = 4;  return 0;
    case MTLPixelFormatR32Float:         *vk = 100 ;            *bpp = 4;  return 0;
    case MTLPixelFormatRG16Unorm:        *vk = 77 ;           *bpp = 4;  return 0;
    case MTLPixelFormatRG16Snorm:        *vk = 78 ;           *bpp = 4;  return 0;
    case MTLPixelFormatRG16Uint:         *vk = 81 ;            *bpp = 4;  return 0;
    case MTLPixelFormatRG16Sint:         *vk = 82 ;            *bpp = 4;  return 0;
    case MTLPixelFormatRG16Float:        *vk = 83 ;          *bpp = 4;  return 0;
    case MTLPixelFormatRGBA8Unorm:       *vk = 37 ;         *bpp = 4;  return 0;
    case MTLPixelFormatRGBA8Unorm_sRGB:  *vk = 43 ;          *bpp = 4;  return 0;
    case MTLPixelFormatRGBA8Snorm:       *vk = 38 ;         *bpp = 4;  return 0;
    case MTLPixelFormatRGBA8Uint:        *vk = 41 ;          *bpp = 4;  return 0;
    case MTLPixelFormatRGBA8Sint:        *vk = 42 ;          *bpp = 4;  return 0;
    case MTLPixelFormatBGRA8Unorm:       *vk = 44 ;         *bpp = 4;  return 0;
    case MTLPixelFormatBGRA8Unorm_sRGB:  *vk = 50 ;          *bpp = 4;  return 0;
    case MTLPixelFormatRGB10A2Unorm:     *vk = 64 ; *bpp = 4; return 0;
    case MTLPixelFormatRGB10A2Uint:      *vk = 68 ; *bpp = 4; return 0;
    case MTLPixelFormatBGR10A2Unorm:     *vk = 58 ; *bpp = 4; return 0;
    case MTLPixelFormatRG11B10Float:     *vk = 122 ; *bpp = 4; return 0;
    case MTLPixelFormatRGB9E5Float:      *vk = 123 ;  *bpp = 4; return 0;
    case MTLPixelFormatRG32Uint:         *vk = 101 ;           *bpp = 8;  return 0;
    case MTLPixelFormatRG32Sint:         *vk = 102 ;           *bpp = 8;  return 0;
    case MTLPixelFormatRG32Float:        *vk = 103 ;         *bpp = 8;  return 0;
    case MTLPixelFormatRGBA16Unorm:      *vk = 91 ;     *bpp = 8;  return 0;
    case MTLPixelFormatRGBA16Snorm:      *vk = 92 ;     *bpp = 8;  return 0;
    case MTLPixelFormatRGBA16Uint:       *vk = 95 ;      *bpp = 8;  return 0;
    case MTLPixelFormatRGBA16Sint:       *vk = 96 ;      *bpp = 8;  return 0;
    case MTLPixelFormatRGBA16Float:      *vk = 97 ;    *bpp = 8;  return 0;
    case MTLPixelFormatRGBA32Uint:       *vk = 107 ;     *bpp = 16; return 0;
    case MTLPixelFormatRGBA32Sint:       *vk = 108 ;     *bpp = 16; return 0;
    case MTLPixelFormatRGBA32Float:      *vk = 109 ;   *bpp = 16; return 0;
    case MTLPixelFormatBC1_RGBA:         *vk = 133 ;   *bpp = 8;  return 0;
    case MTLPixelFormatBC1_RGBA_sRGB:    *vk = 134 ;    *bpp = 8;  return 0;
    case MTLPixelFormatBC2_RGBA:         *vk = 135 ;        *bpp = 16; return 0;
    case MTLPixelFormatBC2_RGBA_sRGB:    *vk = 136 ;         *bpp = 16; return 0;
    case MTLPixelFormatBC3_RGBA:         *vk = 137 ;        *bpp = 16; return 0;
    case MTLPixelFormatBC3_RGBA_sRGB:    *vk = 138 ;         *bpp = 16; return 0;
    case MTLPixelFormatBC4_RUnorm:       *vk = 139 ;        *bpp = 8;  return 0;
    case MTLPixelFormatBC4_RSnorm:       *vk = 140 ;        *bpp = 8;  return 0;
    case MTLPixelFormatBC5_RGUnorm:      *vk = 141 ;        *bpp = 16; return 0;
    case MTLPixelFormatBC5_RGSnorm:      *vk = 142 ;        *bpp = 16; return 0;
    case MTLPixelFormatBC6H_RGBUfloat:   *vk = 143 ;      *bpp = 16; return 0;
    case MTLPixelFormatBC6H_RGBFloat:    *vk = 144 ;      *bpp = 16; return 0;
    case MTLPixelFormatBC7_RGBAUnorm:    *vk = 145 ;        *bpp = 16; return 0;
    case MTLPixelFormatBC7_RGBAUnorm_sRGB: *vk = 146 ;       *bpp = 16; return 0;
    case MTLPixelFormatInvalid:          *vk = 37 ;         *bpp = 4;  return 0;
    default: break;
    }
    static MTLPixelFormat seen[32]; static int nseen;
    int dup = 0; for (int i = 0; i < nseen; i++) if (seen[i] == f) dup = 1;
    if (!dup) { if (nseen < 32) seen[nseen++] = f; nvlog("pixel format %lu is NOT carried by this driver yet (compressed / depth-stencil / packed YUV) — nil", (unsigned long)f); }
    return -1;
}
int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8) { return nvmtl_pixfmt(f, vk, bpp, a8); }
int nvmtl_depthfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect) { return nvmtl_depthfmt(f, vk, aspect); }

static NSString *nvmtl_varying_key(id v) {
    if (![v isKindOfClass:[NSDictionary class]]) return nil;
    id s = ((NSDictionary *)v)[@"user_semantic"]; if ([s isKindOfClass:[NSString class]]) return s;
    id n = ((NSDictionary *)v)[@"name"]; return [n isKindOfClass:[NSString class]] ? n : nil;
}
static int nvmtl_varying_one_location(id v) {
    id t = [v isKindOfClass:[NSDictionary class]] ? ((NSDictionary *)v)[@"type_name"] : nil;
    return ![t isKindOfClass:[NSString class]] || ([(NSString *)t rangeOfString:@"x"].location == NSNotFound && [(NSString *)t rangeOfString:@"["].location == NSNotFound);
}
static NSData *nvmtl_varying_link(NVMTLFunction *vf, NVMTLFunction *ff) {
    NSDictionary *vr = nvmtl_function_reflection(vf), *fr = nvmtl_function_reflection(ff);
    NSArray *vv = [vr isKindOfClass:[NSDictionary class]] ? vr[@"varyings"] : nil, *fv = [fr isKindOfClass:[NSDictionary class]] ? fr[@"varyings"] : nil;
    if (![fv isKindOfClass:[NSArray class]] || !fv.count) return nil;
    if (![vv isKindOfClass:[NSArray class]]) { static int said; if (!said++) nvlog("varylink: %s has no reflected varyings - %s keeps its own Locations", vf->_fname.UTF8String ?: "?", ff->_fname.UTF8String ?: "?"); return nil; }
    enum { NL = 128 };
    uint8_t used[NL] = { 0 }, have[NL] = { 0 }; uint32_t remap[NL]; for (uint32_t i = 0; i < NL; i++) remap[i] = i;
    NSMutableDictionary<NSString *, NSNumber *> *out = [NSMutableDictionary new];
    for (id v in vv) {
        NSNumber *l = [v isKindOfClass:[NSDictionary class]] ? ((NSDictionary *)v)[@"location"] : nil;
        if (![l isKindOfClass:[NSNumber class]] || l.unsignedIntValue >= NL || !nvmtl_varying_one_location(v)) { static int said; if (!said++) nvlog("varylink: a vertex varying of %s is not one Location in 0..127 - pipeline keeps its Locations", vf->_fname.UTF8String ?: "?"); return nil; }
        used[l.unsignedIntValue] = 1; NSString *k = nvmtl_varying_key(v); if (k) out[k] = l;
    }
    uint32_t next = 0; int change = 0, unmatched = 0;
    for (id f in fv) {
        NSNumber *l = [f isKindOfClass:[NSDictionary class]] ? ((NSDictionary *)f)[@"location"] : nil;
        if (![l isKindOfClass:[NSNumber class]] || l.unsignedIntValue >= NL || !nvmtl_varying_one_location(f)) { static int said; if (!said++) nvlog("varylink: a fragment varying of %s is not one Location in 0..127 - pipeline keeps its Locations", ff->_fname.UTF8String ?: "?"); return nil; }
        uint32_t L = l.unsignedIntValue, T; NSString *k = nvmtl_varying_key(f); NSNumber *to = k ? out[k] : nil;
        if (to) T = to.unsignedIntValue;
        else { while (next < NL && used[next]) next++; if (next >= NL) return nil; T = next; used[next] = 1; unmatched++; }
        if (have[L]) return nil;
        have[L] = 1; remap[L] = T; if (T != L) change = 1;
    }
    if (!change) return nil;
    const uint32_t *w = (const uint32_t *)ff->_spirv.bytes; size_t n = ff->_spirv.length / 4;
    if (n < 5 || w[0] != 0x07230203u) return nil;
    NSMutableData *md = [ff->_spirv mutableCopy]; uint32_t *m = (uint32_t *)md.mutableBytes;
    NSMutableIndexSet *inputs = [NSMutableIndexSet indexSet]; int memberLoc = 0;
    for (size_t i = 5; i < n; ) { uint32_t op = m[i] & 0xffffu, wc = m[i] >> 16; if (!wc || i + wc > n) return nil;
        if (op == 59  && wc >= 4 && m[i + 3] == 1 ) [inputs addIndex:m[i + 2]];
        if (op == 72  && wc >= 5 && m[i + 3] == 30 ) memberLoc = 1;
        i += wc; }
    if (memberLoc) { static int said; if (!said++) nvlog("varylink: %s decorates Locations on block members - not linked (said once)", ff->_fname.UTF8String ?: "?"); return nil; }
    int moved = 0;
    for (size_t i = 5; i < n; ) { uint32_t op = m[i] & 0xffffu, wc = m[i] >> 16;
        if (op == 71  && wc >= 4 && m[i + 2] == 30  && [inputs containsIndex:m[i + 1]]) {
            uint32_t L = m[i + 3];
            if (L >= NL || !have[L]) { static int said; if (!said++) nvlog("varylink: %s reads Input Location %u that its reflection does not name - not linked", ff->_fname.UTF8String ?: "?", L); return nil; }
            if (remap[L] != L) { m[i + 3] = remap[L]; moved++; } }
        i += wc; }
    static unsigned told; if (told++ < 8 || told % 4096 == 0)
        nvlog("varylink: %s -> %s: %d fragment input(s) moved onto the vertex output of the same name, %d written by no vertex output (#%u)",
              vf->_fname.UTF8String ?: "?", ff->_fname.UTF8String ?: "?", moved, unmatched, told);
    return md;
}
static BOOL nvmtl_desc_fbo(MTLTextureDescriptor *d) {
    SEL s = sel_registerName("framebufferOnly");
    return d && [d respondsToSelector:s] && ((BOOL (*)(id, SEL))objc_msgSend)(d, s);
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d {
    if (nvmtl_vk_init()) return nil;
    if (d.textureType == MTLTextureTypeTextureBuffer) {
        extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
        uint32_t vkf = 0, bpp = 0; int a8 = 0;
        if (!nvmtl_vk_texel_on() || !d.width || nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8) || !bpp) {
            nvlog("newTextureWithDescriptor: texture buffer %lu x fmt %lu not carried -> nil", (unsigned long)d.width, (unsigned long)d.pixelFormat); return nil; }
        id<MTLBuffer> b = [self newBufferWithLength:(NSUInteger)d.width * bpp options:d.resourceOptions];
        return b ? [b newTextureWithDescriptor:d offset:0 bytesPerRow:(NSUInteger)d.width * bpp] : nil;
    }
    if (nvmtl_alloc_blocked()) return nil;
    NVMTLTexture *t = [NVMTLTexture new];
    t->_ropt = (MTLResourceOptions)d.cpuCacheMode | ((MTLResourceOptions)d.hazardTrackingMode << MTLResourceHazardTrackingModeShift)
             | ((MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift);
    t->_fbo = nvmtl_desc_fbo(d);
    t->_fmt = d.pixelFormat;
    uint32_t dvk = 0, dasp = 0;
    if (nvmtl_depthfmt(d.pixelFormat, &dvk, &dasp) == 0) {
        t->_q = nvmtl_device_queue();
        const uint32_t dtt = (uint32_t)d.textureType, dms = (d.textureType == MTLTextureType2DMultisample || dtt == 8) ? (uint32_t)d.sampleCount : 1u;
        if (nvmtl_vk_depth_create_full((uint32_t)d.width, (uint32_t)d.height, dvk, dasp, dms, dtt,
                                       nvmtl_vk_layers_for_desc(dtt, (uint32_t)d.arrayLength, 1u), (uint32_t)(d.mipmapLevelCount ? d.mipmapLevelCount : 1), &t->_i)) { nvlog("depth texture FAILED (format %lu)", (unsigned long)d.pixelFormat); return nil; }
        t->_usage = d.usage;
        nvlog("newTextureWithDescriptor: %lux%lu DEPTH32F type %lu layers %u mips %u -> %p", (unsigned long)d.width, (unsigned long)d.height, (unsigned long)d.textureType, t->_i.layers, t->_i.mips, (__bridge void *)t);
        return t;
    }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt(d.pixelFormat, &vkf, &bpp, &a8)) return nil;
    int wantStex = (d.usage & MTLTextureUsageShaderWrite) != 0;
    if (d.textureType == MTLTextureType2DMultisample || d.textureType == MTLTextureType2DMultisampleArray) {
        if (d.mipmapLevelCount != 1 || d.depth != 1) { nvlog("newTextureWithDescriptor: invalid multisample color descriptor"); return nil; }
        if (wantStex) { static _Atomic unsigned long msw; const unsigned long k = atomic_fetch_add(&msw, 1) + 1;
            if (!(k & (k - 1))) nvlog("newTextureWithDescriptor: %lu multisample texture(s) asked for ShaderWrite - created like Apple (M1/D300 create them); no storage view", k); }
        if (nvmtl_vk_image_create_ms_full((uint32_t)d.width, (uint32_t)d.height, vkf, bpp, (uint32_t)d.sampleCount, (uint32_t)d.textureType, (uint32_t)d.arrayLength, &t->_i)) { nvmtl_alloc_failed("newTextureWithDescriptor"); nvlog("newTextureWithDescriptor: FAILED (multisample)"); return nil; }
    } else
    if (nvmtl_vk_image_create_typed((uint32_t)d.width, (uint32_t)d.height, vkf, bpp, a8, (uint32_t)d.mipmapLevelCount, wantStex, (uint32_t)d.textureType, nvmtl_vk_layers_for_desc((uint32_t)d.textureType, (uint32_t)d.arrayLength, (uint32_t)d.depth), &t->_i)) { nvmtl_alloc_failed("newTextureWithDescriptor"); nvlog("newTextureWithDescriptor: FAILED"); return nil; }
    nvmtl_alloc_ok();
    t->_q = nvmtl_device_queue();
    t->_stex = t->_i.storage != 0;
    t->_usage = d.usage;
    if (wantStex && !t->_stex) { static unsigned long lost; lost++;
        if (!(lost & (lost - 1))) nvlog("newTextureWithDescriptor: pixel format %lu asked for ShaderWrite and NVK cannot make it a storage image — shader writes to it will be LOST (%lu texture(s) so far in this process)", (unsigned long)d.pixelFormat, lost); }
    nvlog("newTextureWithDescriptor: %lux%lu%s -> %p", (unsigned long)d.width, (unsigned long)d.height, t->_stex ? " +storage" : "", (__bridge void *)t); return t; }

typedef struct { uint32_t magic, seed, gen; } nvmtl_vram_tag;
#define NVMTL_VRAM_MAGIC 0x564d5634u
static unsigned long long g_surf_vram, g_surf_private, g_surfin_skip, g_surfin_copy;
static CFStringRef nvmtl_vram_tag_key(NSUInteger plane) { return CFStringCreateWithFormat(NULL, NULL, CFSTR("NVMTLVramAt:p%lu"), (unsigned long)plane); }
static int nvmtl_vram_tag_current(IOSurfaceRef surf, NSUInteger plane, uint32_t seed)
{
    CFStringRef k = nvmtl_vram_tag_key(plane); CFTypeRef v = IOSurfaceCopyValue(surf, k); CFRelease(k);
    nvmtl_vram_tag t = { 0, 0, 0 };
    int ok = v && CFGetTypeID(v) == CFDataGetTypeID() && CFDataGetLength((CFDataRef)v) == (CFIndex)sizeof t;
    if (ok) memcpy(&t, CFDataGetBytePtr((CFDataRef)v), sizeof t);
    if (v) CFRelease(v);
    return ok && t.magic == NVMTL_VRAM_MAGIC && t.seed == seed;
}
static uint32_t nvmtl_vram_gen(IOSurfaceRef surf, NSUInteger plane)
{
    CFStringRef k = nvmtl_vram_tag_key(plane); CFTypeRef v = IOSurfaceCopyValue(surf, k); CFRelease(k);
    nvmtl_vram_tag t = { 0, 0, 0 };
    if (v && CFGetTypeID(v) == CFDataGetTypeID() && CFDataGetLength((CFDataRef)v) == (CFIndex)sizeof t) memcpy(&t, CFDataGetBytePtr((CFDataRef)v), sizeof t);
    if (v) CFRelease(v);
    return t.magic == NVMTL_VRAM_MAGIC ? t.gen : 0;
}
static void nvmtl_vram_tag_write(IOSurfaceRef surf, NSUInteger plane, uint32_t seed, uint32_t gen)
{
    nvmtl_vram_tag t = { NVMTL_VRAM_MAGIC, seed, gen };
    CFDataRef dd = CFDataCreate(NULL, (const UInt8 *)&t, sizeof t); CFStringRef k = nvmtl_vram_tag_key(plane);
    IOSurfaceSetValue(surf, k, dd); CFRelease(k); CFRelease(dd);
}
static uint32_t nvmtl_vram_tag_bump(IOSurfaceRef surf, NSUInteger plane, uint32_t seed)
{
    const uint32_t g = nvmtl_vram_gen(surf, plane) + 1; nvmtl_vram_tag_write(surf, plane, seed, g); return g;
}
static void nvmtl_vram_tag_set(IOSurfaceRef surf, NSUInteger plane, uint32_t seed)
{
    nvmtl_vram_tag t = { NVMTL_VRAM_MAGIC, seed, nvmtl_vram_gen(surf, plane) };
    CFDataRef d = CFDataCreate(NULL, (const UInt8 *)&t, sizeof t); CFStringRef k = nvmtl_vram_tag_key(plane);
    IOSurfaceSetValue(surf, k, d); CFRelease(k); CFRelease(d);
}
static int nvmtl_surface_image_create(NVMTLTexture *t, IOSurfaceRef surf, NSUInteger plane, uint32_t w, uint32_t h,
                                      uint32_t vkf, uint32_t bpp, int a8, int stex)
{
    if (surf && plane == 0 && IOSurfaceGetPlaneCount(surf) <= 1 && nvmtl_vk_surface_share_on()) {
        size_t row = IOSurfaceGetBytesPerRow(surf), sh = IOSurfaceGetHeight(surf);
        if ((size_t)w * (bpp ? bpp : 4) <= row && h <= sh && row % 32 == 0) {
            nvk_buffer vb;
            if (!nvmtl_vk_surface_vram(IOSurfaceGetID(surf), 0, row * h, &vb)) {
                if (!nvmtl_vk_image_create_buffer_alias(w, h, vkf, bpp, a8, stex, 1, &vb, 0, row, &t->_i)) {
                    t->_vramBuf = vb; t->_vramOn = YES; g_surf_vram++;
                    uint32_t seed = IOSurfaceGetSeed(surf);
                    int current = nvmtl_vram_tag_current(surf, 0, seed);
                    if (current) { t->_surfSeed = seed; t->_surfSynced = YES; }
                    static unsigned n; if (n++ < 8 || n % 2000 == 0)
                        nvlog("item 7: surface %u %ux%u pitch %zu -> the family's VRAM%s (vram %llu, private %llu)", (unsigned)IOSurfaceGetID(surf),
                              w, h, row, current ? ", already current (no upload)" : "", g_surf_vram, g_surf_private);
                    return 0;
                }
                nvmtl_vk_buffer_destroy(&vb);
            }
        }
    }
    g_surf_private++;
    return nvmtl_vk_image_create_typed(w, h, vkf, bpp, a8, 1, stex, 2u, 1u, &t->_i);
}

- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d iosurface:(IOSurfaceRef)surf plane:(NSUInteger)plane {
    if (nvmtl_vk_init()) return nil;
    size_t w = d.width, h = d.height;
    if (!w || !h || w > UINT32_MAX || h > UINT32_MAX) return nil;
    if (surf && plane >= MAX((size_t)1, IOSurfaceGetPlaneCount(surf))) return nil;
    NVMTLTexture *t = [NVMTLTexture new];
    t->_ropt = (MTLResourceOptions)d.cpuCacheMode | ((MTLResourceOptions)d.hazardTrackingMode << MTLResourceHazardTrackingModeShift)
             | ((MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift);
    t->_fbo = nvmtl_desc_fbo(d);
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt(d.pixelFormat, &vkf, &bpp, &a8)) return nil;
    int wantStex = (d.usage & MTLTextureUsageShaderWrite) != 0;
    if (nvmtl_surface_image_create(t, surf, plane, (uint32_t)w, (uint32_t)h, vkf, bpp, a8, wantStex)) {
        nvlog("newTextureWithDescriptor:iosurface:plane: image create FAILED (%zux%zu)", w, h); return nil; }
    t->_q = nvmtl_device_queue();
    t->_surf = surf; if (surf) { CFRetain(surf); t->_surfOwned = YES; }
    t->_plane = plane;
    t->_fmt = d.pixelFormat;
    t->_stex = t->_i.storage != 0;
    t->_usage = d.usage;
    nvlog("newTextureWithDescriptor:iosurface:%p plane:%lu -> %p (%zux%zu fmt %lu)",
          (void *)surf, (unsigned long)plane, (__bridge void *)t, w, h, (unsigned long)d.pixelFormat);
    return t; }

- (id<MTLLibrary>)nvmtlNewLibraryWithSPIRV:(NSDictionary<NSString *, NSData *> *)fns {
    NVMTLLibrary *lib = [NVMTLLibrary new]; lib->_fns = fns;
    nvlog("library from SPIR-V: %lu functions", (unsigned long)fns.count); return lib; }

static id nvmtl_fail(NSError **err, NSString *why) {
    nvlog("%s", why ? why.UTF8String : "(no reason given)");
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorCompileFailure
                                    userInfo:@{ NSLocalizedDescriptionKey: why ?: @"failed" }];
    return nil;
}

- (id<MTLLibrary>)nvmtlAppleTwinOf:(NVMTLLibrary *)lib {
    if (![lib isKindOfClass:[NVMTLLibrary class]]) return nil;
    @synchronized (lib) { return [self nvmtlAppleTwinOfLocked:lib]; }
}
- (id<MTLLibrary>)nvmtlAppleTwinOfLocked:(NVMTLLibrary *)lib {
    if (lib->_twinTried) return lib->_appleTwin;
    lib->_twinTried = YES;
    static NSMapTable *twinByKey, *twinByFile; static dispatch_once_t twinOnce;
    dispatch_once(&twinOnce, ^{ twinByKey = [NSMapTable strongToWeakObjectsMapTable]; twinByFile = [NSMapTable strongToWeakObjectsMapTable]; });
    NSData *libKey = objc_getAssociatedObject(lib, &gNVMTLLibKeyAssoc), *ckey = getenv("NVMTL_NO_TWINMEMO") ? nil : libKey;
    if (ckey) {
        id t; @synchronized (twinByKey) { t = [twinByKey objectForKey:ckey]; }
        if (t) { lib->_appleTwin = t; nvlog("apple twin: shared by content key -> %s", object_getClassName(t)); return t; }
    }
    NSArray *file = getenv("NVMTL_NO_TWINURL") ? nil : objc_getAssociatedObject(lib, &gNVMTLLibFileAssoc);
    if (file.count == 2 && libKey && [file[1] length] == sizeof(struct stat)) {
        NSString *fp = file[0]; NSData *then = file[1]; struct stat now; char rp[PATH_MAX];
        Class ub = class_getSuperclass(object_getClass(self));
        NSString *byPath = [@"p:" stringByAppendingString:realpath(fp.fileSystemRepresentation, rp) ? @(rp) : fp];
        NSString *byIno = [NSString stringWithFormat:@"i:%llu:%llu", (unsigned long long)((const struct stat *)then.bytes)->st_dev,
                           (unsigned long long)((const struct stat *)then.bytes)->st_ino];
        id live = nil; NSData *liveKey = nil;
        if (!getenv("NVMTL_TWINURL_NOGUARD_TEST")) @synchronized (twinByKey) {
            for (NSString *k in @[byPath, byIno]) { id t = [twinByFile objectForKey:k]; NSData *tk = t ? objc_getAssociatedObject(t, &gNVMTLLibKeyAssoc) : nil;
                if (t && ![tk isEqual:libKey]) { live = t; liveKey = tk; break; } } }
        if (stat(fp.fileSystemRepresentation, &now) != 0 || !nvmtl_stat_same(then.bytes, &now))
            nvlog("apple twin: %s changed since it was indexed - building from the bytes instead", fp.UTF8String);
        else if (live)
            nvlog("apple twin: a live Apple library of %s holds other content (Apple's file cache would serve it) - building from the bytes instead", fp.UTF8String);
        else if (ub && class_respondsToSelector(ub, @selector(newLibraryWithURL:error:))) {
            struct objc_super usup = { self, ub }; NSError *ue = nil;
            id apple = ((id (*)(struct objc_super *, SEL, NSURL *, NSError **))objc_msgSendSuper)(
                           &usup, @selector(newLibraryWithURL:error:), [NSURL fileURLWithPath:fp], &ue);
            if (apple) {
                lib->_appleTwin = apple;
                objc_setAssociatedObject(apple, &gNVMTLLibKeyAssoc, libKey, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
                @synchronized (twinByKey) { if (ckey) [twinByKey setObject:apple forKey:ckey];
                                            [twinByFile setObject:apple forKey:byPath]; [twinByFile setObject:apple forKey:byIno]; }
                nvlog("apple twin: %s via its file (no container copy or hash) -> %s", fp.lastPathComponent.UTF8String, object_getClassName(apple));
                return apple; }
            nvlog("apple twin: super refused the file %s: %s - building from the bytes instead", fp.UTF8String,
                  [ue localizedDescription].UTF8String ?: "(no reason given)");
        }
    }
    NSData *raw = lib->_raw;
    if (!raw.length && lib->_rawPath.length)
        raw = [NSData dataWithContentsOfFile:lib->_rawPath options:NSDataReadingMappedIfSafe error:NULL];
    if (!raw.length) { nvlog("apple twin: this library kept no container bytes%s - cannot build one",
                             lib->_rawPath.length ? " and its file could not be re-read" : ""); return nil; }
    Class base = class_getSuperclass(object_getClass(self));
    if (!base || !class_respondsToSelector(base, @selector(newLibraryWithData:error:))) {
        nvlog("apple twin: base class %s has no newLibraryWithData:error: - nothing to build from",
              base ? class_getName(base) : "(none)");
        return nil; }
    dispatch_block_t keep = ^{ (void)raw; };
    dispatch_data_t dd = dispatch_data_create(raw.bytes, raw.length,
                                              dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0),
                                              getenv("NVMTL_NO_NOCOPY") ? DISPATCH_DATA_DESTRUCTOR_DEFAULT : keep);
    struct objc_super sup = { self, base };
    NSError *e = nil;
    id apple = ((id (*)(struct objc_super *, SEL, dispatch_data_t, NSError **))objc_msgSendSuper)(
                   &sup, @selector(newLibraryWithData:error:), dd, &e);
    if (!apple) {
        nvlog("apple twin: super refused the %lu B container: %s", (unsigned long)raw.length,
              [e localizedDescription].UTF8String ?: "(no reason given)");
        return nil; }
    lib->_appleTwin = apple;
    if (ckey) @synchronized (twinByKey) { [twinByKey setObject:apple forKey:ckey]; }
    nvlog("apple twin: %lu B container -> %s", (unsigned long)raw.length, object_getClassName(apple));
    return apple; }

static NSString *nvmtl_link_dump_dir(void);
static void nvmtl_stitch_dump(const char *what, NSString *dag, NSArray *fns, id apple, id<MTLLibrary> mine) {
    NSString *dir = nvmtl_link_dump_dir();
    if (!dir) return;
    static _Atomic int seq; int n = ++seq;
    [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:nil];
    NSArray *names = mine ? [mine.functionNames sortedArrayUsingSelector:@selector(compare:)] : nil;
    NSMutableString *t = [NSMutableString stringWithFormat:@"what %s\nresult %@\n", what, names ? [names componentsJoinedByString:@","] : @"(nil)"];
    for (id f in fns) {
        if ([f isKindOfClass:[NVMTLFunction class]]) {
            NVMTLFunction *o = (NVMTLFunction *)f;
            [t appendFormat:@"fn %@ spec %@ fcv %s\n", o->_fname, o->_specName.length ? o->_specName : @"-", o->_fcv ? "yes" : "no"];
        } else [t appendFormat:@"fn foreign %s %@\n", object_getClassName(f), [f conformsToProtocol:@protocol(MTLFunction)] ? ((id<MTLFunction>)f).name : @"?"];
    }
    [t appendFormat:@"dag %lu B\n%@\n", (unsigned long)dag.length, dag ?: @"(none)"];
    NSString *base = [dir stringByAppendingFormat:@"/stitch-%03d", n];
    BOOL ok = [t writeToFile:[base stringByAppendingString:@".txt"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
    SEL s = sel_registerName("libraryDataContents");
    NSData *c = [apple respondsToSelector:s] ? ((id (*)(id, SEL))objc_msgSend)(apple, s) : nil;
    BOOL okc = [c isKindOfClass:[NSData class]] && [c writeToFile:[base stringByAppendingString:@".mtlb"] atomically:YES];
    nvlog("stitch dump %d (%s): %s, container %s", n, what, ok ? base.UTF8String : "text FAILED to write", okc ? "written" : "NOT written");
}
- (id<MTLLibrary>)newLibraryWithStitchedDescriptor:(id)desc error:(NSError **)err {
    if (err) *err = nil;
    NSArray *graphs = [desc respondsToSelector:@selector(functionGraphs)]
                    ? [desc performSelector:@selector(functionGraphs)] : nil;
    NSArray *fns = [desc respondsToSelector:@selector(functions)]
                 ? [desc performSelector:@selector(functions)] : nil;
    nvlog("newLibraryWithStitchedDescriptor: %lu graph(s), %lu function(s) supplied",
          (unsigned long)graphs.count, (unsigned long)fns.count);
    for (id g in graphs) {
        NSString *gn = [g respondsToSelector:@selector(functionName)]
                     ? [g performSelector:@selector(functionName)] : nil;
        NSArray *nodes = [g respondsToSelector:@selector(nodes)] ? [g performSelector:@selector(nodes)] : nil;
        id outn = [g respondsToSelector:@selector(outputNode)] ? [g performSelector:@selector(outputNode)] : nil;
        NSString *on = [outn respondsToSelector:@selector(name)] ? [outn performSelector:@selector(name)] : nil;
        nvlog("  graph \"%s\": %lu node(s), output \"%s\"", gn.UTF8String ?: "?",
              (unsigned long)nodes.count, on.UTF8String ?: "-");
        for (id n in nodes) {
            NSString *nn = [n respondsToSelector:@selector(name)] ? [n performSelector:@selector(name)] : nil;
            NSArray *args = [n respondsToSelector:@selector(arguments)]
                          ? [n performSelector:@selector(arguments)] : nil;
            NSMutableString *wiring = [NSMutableString new];
            for (id a in args) {
                id idx = [a respondsToSelector:@selector(argumentIndex)] ? [a valueForKey:@"argumentIndex"] : nil;
                NSString *an = [a respondsToSelector:@selector(name)] ? [a performSelector:@selector(name)] : nil;
                if (idx) [wiring appendFormat:@"arg%@ ", idx];
                else if (an.length) [wiring appendFormat:@"%@() ", an];
                else [wiring appendString:@"? "];
            }
            nvlog("    node \"%s\"(%s)", nn.UTF8String ?: "?", wiring.UTF8String);
        }
    }
    Class base = class_getSuperclass(object_getClass(self));
    if (!base || !class_respondsToSelector(base, @selector(newLibraryWithStitchedDescriptor:error:))) {
        nvlog("newLibraryWithStitchedDescriptor: base class %s does not implement it - nothing to bridge",
              base ? class_getName(base) : "(none)");
        return nvmtl_fail(err, @"newLibraryWithStitchedDescriptor: the base class does not implement it either");
    }
    id sendDesc = desc;
    if (fns.count) {
        NSUInteger swapped = 0;
        NSArray *subbed = [self nvmtlAppleFunctionsFor:fns what:"newLibraryWithStitchedDescriptor" swapped:&swapped error:err];
        if (!subbed) return nil;
        if (swapped) {
            id copy = [desc conformsToProtocol:@protocol(NSCopying)] ? [desc copy] : nil;
            if (!copy || ![copy respondsToSelector:@selector(setFunctions:)])
                return nvmtl_fail(err, @"newLibraryWithStitchedDescriptor: the descriptor cannot be rebuilt with Apple's functions");
            ((void (*)(id, SEL, NSArray *))objc_msgSend)(copy, @selector(setFunctions:), subbed);
            sendDesc = copy;
            nvlog("newLibraryWithStitchedDescriptor: %lu of %lu function(s) swapped for Apple's own",
                  (unsigned long)swapped, (unsigned long)fns.count);
        }
    }
    struct objc_super sup = { self, base };
    id apple = ((id (*)(struct objc_super *, SEL, id, NSError **))objc_msgSendSuper)(
                   &sup, @selector(newLibraryWithStitchedDescriptor:error:), sendDesc, err);
    if (!apple) {
        if (err && *err) { nvlog("newLibraryWithStitchedDescriptor: Apple's stitcher failed: %s",
                                 [*err localizedDescription].UTF8String ?: "(no reason)"); return nil; }
        return nvmtl_fail(err, @"newLibraryWithStitchedDescriptor: Apple's stitcher returned nothing and said nothing");
    }
    id<MTLLibrary> mine = [self nvmtlBridgeAppleLibrary:apple what:"newLibraryWithStitchedDescriptor" error:err];
    nvmtl_stitch_dump("newLibraryWithStitchedDescriptor", nil, ((MTLStitchedLibraryDescriptor *)desc).functions, apple, mine);
    return mine;
}

- (NSArray *)nvmtlAppleFunctionsFor:(NSArray *)fns what:(const char *)what swapped:(NSUInteger *)swappedOut error:(NSError **)err {
    if (err) *err = nil;
    if (swappedOut) *swappedOut = 0;
    NSMutableArray *subbed = [NSMutableArray arrayWithCapacity:fns.count];
    NSUInteger swapped = 0;
    for (id f in fns) {
        if (![f isKindOfClass:[NVMTLFunction class]]) { [subbed addObject:f]; continue; }
        NVMTLFunction *ours = (NVMTLFunction *)f;
        NSString *fname = ours->_fname ?: @"";
        id<MTLLibrary> twin = [self nvmtlAppleTwinOf:(NVMTLLibrary *)ours->_lib];
        id<MTLFunction> af = nil;
        if (twin && ours->_specName.length) {
            MTLFunctionDescriptor *fd = [MTLFunctionDescriptor functionDescriptor];
            fd.name = fname; fd.specializedName = ours->_specName;
            if (ours->_fcv) fd.constantValues = ours->_fcv;
            NSError *fe = nil;
            af = [twin newFunctionWithDescriptor:fd error:&fe];
            if (!af) nvlog("  %s: apple twin refused \"%s\" as \"%s\": %s", what, fname.UTF8String,
                           ours->_specName.UTF8String, [fe localizedDescription].UTF8String ?: "(no reason)");
        }
        if (twin && !af && ours->_fcv) {
            NSError *fe = nil;
            af = [twin newFunctionWithName:fname constantValues:ours->_fcv error:&fe];
            if (!af) nvlog("  %s: apple twin refused \"%s\" with its constant values: %s", what,
                           fname.UTF8String, [fe localizedDescription].UTF8String ?: "(no reason)");
        }
        if (twin && !af) af = [twin newFunctionWithName:fname];
        if (!af) {
            nvlog("%s: no Apple twin function for \"%s\" - refusing rather than handing Apple's compiler a function "
                  "it cannot read", what, fname.UTF8String);
            return nvmtl_fail(err, [NSString stringWithFormat:@"%s: cannot supply function \"%@\" to Apple's compiler", what, fname]);
        }
        [subbed addObject:af]; swapped++;
    }
    if (swappedOut) *swappedOut = swapped;
    return subbed;
}

- (id<MTLLibrary>)newLibraryWithDAG:(NSString *)dag functions:(NSArray *)fns error:(NSError **)err {
    if (err) *err = nil;
    Class base = class_getSuperclass(object_getClass(self));
    if (!base || !class_respondsToSelector(base, @selector(newLibraryWithDAG:functions:error:))) {
        nvlog("newLibraryWithDAG: base class %s does not implement it - nothing to bridge", base ? class_getName(base) : "(none)");
        return nvmtl_fail(err, @"newLibraryWithDAG: the base class does not implement it either");
    }
    NSUInteger swapped = 0;
    NSArray *afns = [self nvmtlAppleFunctionsFor:fns what:"newLibraryWithDAG" swapped:&swapped error:err];
    if (!afns) return nil;
    nvlog("newLibraryWithDAG: %lu B DAG, %lu of %lu function(s) swapped for Apple's own",
          (unsigned long)dag.length, (unsigned long)swapped, (unsigned long)fns.count);
    struct objc_super sup = { self, base };
    id apple = ((id (*)(struct objc_super *, SEL, id, id, NSError **))objc_msgSendSuper)(
                   &sup, @selector(newLibraryWithDAG:functions:error:), dag, afns, err);
    if (!apple) {
        if (err && *err) { nvlog("newLibraryWithDAG: Apple's stitcher failed: %s",
                                 [*err localizedDescription].UTF8String ?: "(no reason)"); return nil; }
        return nvmtl_fail(err, @"newLibraryWithDAG: Apple's stitcher returned nothing and said nothing");
    }
    id<MTLLibrary> mine = [self nvmtlBridgeAppleLibrary:apple what:"newLibraryWithDAG" error:err];
    nvmtl_stitch_dump("newLibraryWithDAG", dag, fns, apple, mine);
    return mine;
}

- (void)newLibraryWithStitchedDescriptor:(id)desc completionHandler:(void (^)(id<MTLLibrary>, NSError *))handler {
    NSError *e = nil;
    id<MTLLibrary> lib = [self newLibraryWithStitchedDescriptor:desc error:&e];
    if (handler) handler(lib, lib ? nil : e);
}
- (id<MTLLibrary>)newLibraryWithStitchedDescriptorSPI:(id)desc error:(NSError **)err {
    nvlog("newLibraryWithStitchedDescriptorSPI: routed through the public stitcher bridge");
    return [self newLibraryWithStitchedDescriptor:desc error:err];
}
- (id<MTLLibrary>)newLibraryWithStitchedDescriptor:(id)desc destinationBinaryArchive:(id)archive error:(NSError **)err {
    nvlog("newLibraryWithStitchedDescriptor:destinationBinaryArchive: archive %p NOT written (no binary archive support)", archive);
    return [self newLibraryWithStitchedDescriptor:desc error:err];
}

- (id<MTLLibrary>)newLibraryWithData:(dispatch_data_t)data error:(NSError **)err {
    if (err) *err = nil;
    NSData *bytes = [NSData dataWithData:(NSData *)data];
    NSDictionary *stages = nil, *airs = nil;
    NSDictionary<NSString *, NSData *> *fns = nvmtl_translate_metallib(bytes, &stages, &airs);
    if (!fns) return nvmtl_fail(err, @"newLibraryWithData: translation produced nothing");
    NVMTLLibrary *lib = [NVMTLLibrary new]; lib->_fns = fns; lib->_stages = stages; lib->_airs = airs; lib->_externs = nvmtl_externs_of_airs(airs);
    lib->_raw = bytes;
    nvmtl_vendor_compile_airs((NSDictionary<NSString *, NSString *> *)airs,
                              (NSDictionary<NSString *, NSString *> *)stages);
    return lib; }
- (id<MTLLibrary>)nvmtlBridgeAppleLibrary:(id)apple what:(const char *)what error:(NSError **)err {
    if (!apple) return nil;
    SEL s = sel_registerName("libraryDataContents");
    if (![apple respondsToSelector:s]) return nvmtl_fail(err, [NSString stringWithFormat:@"%s: Apple's library has no libraryDataContents", what]);
    NSData *mtlb = ((id (*)(id, SEL))objc_msgSend)(apple, s);
    if (![mtlb isKindOfClass:[NSData class]] || mtlb.length < 4) return nvmtl_fail(err, [NSString stringWithFormat:@"%s: libraryDataContents is not a container", what]);
    dispatch_data_t dd = dispatch_data_create(mtlb.bytes, mtlb.length, dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), DISPATCH_DATA_DESTRUCTOR_DEFAULT);
    id<MTLLibrary> mine = [self newLibraryWithData:dd error:err];
    nvlog("%s: Apple's %s (%lu B container) -> %s", what, object_getClassName(apple), (unsigned long)mtlb.length, mine ? "ours" : "FAILED to translate");
    return mine;
}
- (id)nvmtlLinkCI:(SEL)s functions:(id)fns info:(const void *)info error:(NSError **)err {
    struct objc_super sup = { self, class_getSuperclass(object_getClass(self)) };
    id apple = ((id (*)(struct objc_super *, SEL, id, const void *, NSError **))objc_msgSendSuper)(&sup, s, fns, info, err);
    if (!apple) {
        if (err && *err) { nvlog("%s: Apple's linker failed: %s", sel_getName(s), [*err localizedDescription].UTF8String ?: "(no reason)"); return nil; }
        return nvmtl_fail(err, [NSString stringWithFormat:@"%s: Apple's linker returned nothing and said nothing", sel_getName(s)]);
    }
    if ([apple isKindOfClass:[NVMTLLibrary class]]) return apple;
    return [self nvmtlBridgeAppleLibrary:apple what:sel_getName(s) error:err];
}
- (id)newLibraryWithImageFilterFunctionsSPI:(id)fns imageFilterFunctionInfo:(const void *)info error:(NSError **)err {
    return [self nvmtlLinkCI:_cmd functions:fns info:info error:err];
}
- (id)newLibraryWithCIFilters:(id)fns imageFilterFunctionInfo:(const void *)info error:(NSError **)err {
    return [self nvmtlLinkCI:_cmd functions:fns info:info error:err];
}
- (id<MTLLibrary>)newLibraryWithSource:(NSString *)src options:(MTLCompileOptions *)opt error:(NSError **)err {
    struct objc_super sup = { self, class_getSuperclass(object_getClass(self)) };
    id apple = ((id (*)(struct objc_super *, SEL, id, id, NSError **))objc_msgSendSuper)(&sup, @selector(newLibraryWithSource:options:error:), src, opt, err);
    id<MTLLibrary> mine = [self nvmtlBridgeAppleLibrary:apple what:"newLibraryWithSource" error:err];
    nvmtl_note_compile_dylibs(mine, opt.libraries);
    return mine;
}
- (void)newLibraryWithSource:(NSString *)src options:(MTLCompileOptions *)opt completionHandler:(MTLNewLibraryCompletionHandler)h {
    NSError *e = nil; id<MTLLibrary> lib = [self newLibraryWithSource:src options:opt error:&e];
    if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(lib, e); });
}
- (id)targetDeviceArchitecture {
    static id arch; static dispatch_once_t once;
    dispatch_once(&once, ^{
        Class c = NSClassFromString(@"MTLTargetDeviceArchitecture");
        if (!c) { nvlog("targetDeviceArchitecture: Metal has no MTLTargetDeviceArchitecture class — returning nil"); return; }
        arch = [c new];
        ((void (*)(id, SEL, unsigned))objc_msgSend)(arch, sel_registerName("setCpuType:"), 0x01000016u);
        ((void (*)(id, SEL, unsigned))objc_msgSend)(arch, sel_registerName("setSubType:"), 0u);
    });
    return arch;
}
- (id<MTLLibrary>)newDefaultLibrary {
    NSURL *u = [[NSBundle mainBundle] URLForResource:@"default" withExtension:@"metallib"];
    if (!u) { nvlog("newDefaultLibrary: the main bundle has no default.metallib"); return nil; }
    return [self newLibraryWithURL:u error:NULL];
}
- (id<MTLLibrary>)newDefaultLibraryWithBundle:(NSBundle *)b error:(NSError **)err {
    NSURL *u = [b URLForResource:@"default" withExtension:@"metallib"];
    if (!u) return nvmtl_fail(err, [NSString stringWithFormat:@"newDefaultLibraryWithBundle: %@ has no default.metallib", b.bundlePath]);
    return [self newLibraryWithURL:u error:err];
}
- (id<MTLLibrary>)newLibraryWithURL:(NSURL *)url error:(NSError **)err {
    if (err) *err = nil;
    NSString *fp = url.isFileURL ? url.absoluteURL.path : nil; struct stat s0; int st0 = fp ? stat(fp.fileSystemRepresentation, &s0) : -1;
    NSData *d = [NSData dataWithContentsOfURL:url];
    if (!d && url.path.length) {
        fp = url.path; st0 = stat(fp.fileSystemRepresentation, &s0);
        d = [NSData dataWithContentsOfFile:url.path];
        if (d) nvlog("newLibraryWithURL: %s has no scheme - read %lu bytes by path instead",
                     url.path.UTF8String, (unsigned long)d.length);
    }
    if (!d) return nvmtl_fail(err, [NSString stringWithFormat:@"newLibraryWithURL: cannot read %@", url.path ?: url.absoluteString]);
    NSDictionary *stages = nil, *airs = nil;
    NSData *ckey = (st0 == 0 ? nvmtl_libkey_for_file(fp, d, &s0) : nil) ?: nvmtl_lib_sha256(d);
    NSDictionary<NSString *, NSData *> *fns = nvmtl_translate_metallib_k(d, ckey, &stages, &airs);
    if (!fns) return nvmtl_fail(err, [NSString stringWithFormat:@"newLibraryWithURL: translation produced nothing for %@", url.path]);
    NVMTLLibrary *lib = [NVMTLLibrary new]; lib->_fns = fns; lib->_stages = stages; lib->_airs = airs; lib->_externs = nvmtl_externs_of_airs(airs);
    lib->_raw = d;
    objc_setAssociatedObject(lib, &gNVMTLLibKeyAssoc, ckey, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    if (st0 == 0) objc_setAssociatedObject(lib, &gNVMTLLibFileAssoc, @[[fp copy], [NSData dataWithBytes:&s0 length:sizeof s0]], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return lib; }

- (id<MTLLibrary>)nvmtlLibraryFromPath:(NSString *)path error:(NSError **)err {
    if (err) *err = nil;
    if (!path) return nvmtl_fail(err, @"newLibraryWithFile: nil path");
    struct stat s0; const int st0 = stat(path.fileSystemRepresentation, &s0);
    NSData *d = [NSData dataWithContentsOfFile:path];
    if (!d) return nvmtl_fail(err, [NSString stringWithFormat:@"newLibraryWithFile: cannot read %@", path]);
    NSDictionary *stages = nil, *airs = nil;
    NSData *ckey = (st0 == 0 ? nvmtl_libkey_for_file(path, d, &s0) : nil) ?: nvmtl_lib_sha256(d);
    NSDictionary<NSString *, NSData *> *fns = nvmtl_translate_metallib_k(d, ckey, &stages, &airs);
    if (!fns.count) return nvmtl_fail(err, [NSString stringWithFormat:@"newLibraryWithFile: translation produced nothing for %@", path]);
    nvlog("newLibraryWithFile: %s -> %lu functions", path.UTF8String, (unsigned long)fns.count);
    NVMTLLibrary *lib = [NVMTLLibrary new]; lib->_fns = fns; lib->_stages = stages; lib->_airs = airs; lib->_externs = nvmtl_externs_of_airs(airs);
    lib->_rawPath = [path copy];
    objc_setAssociatedObject(lib, &gNVMTLLibKeyAssoc, ckey, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    if (st0 == 0) objc_setAssociatedObject(lib, &gNVMTLLibFileAssoc, @[lib->_rawPath, [NSData dataWithBytes:&s0 length:sizeof s0]], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return lib; }

- (id<MTLLibrary>)newLibraryWithFile:(NSString *)path error:(NSError **)err {
    return [self nvmtlLibraryFromPath:path error:err]; }

enum { NVMTL_MTLBlendFactorUnspecialized = 19, NVMTL_MTLBlendOperationUnspecialized = 5 };
static int nvmtl_blend_factor(MTLBlendFactor f, uint32_t *v) {
    if ((NSUInteger)f == NVMTL_MTLBlendFactorUnspecialized) {
        nvlog("blend: factor %lu is MTLBlendFactorUnspecialized — the app expects this slot's factor at PSO specialization; not implemented",
            (unsigned long)f); return -1; }
    switch (f) {
        case MTLBlendFactorZero: *v = VK_BLEND_FACTOR_ZERO; return 0;
        case MTLBlendFactorOne: *v = VK_BLEND_FACTOR_ONE; return 0;
        case MTLBlendFactorSourceColor: *v = VK_BLEND_FACTOR_SRC_COLOR; return 0;
        case MTLBlendFactorOneMinusSourceColor: *v = VK_BLEND_FACTOR_ONE_MINUS_SRC_COLOR; return 0;
        case MTLBlendFactorSourceAlpha: *v = VK_BLEND_FACTOR_SRC_ALPHA; return 0;
        case MTLBlendFactorOneMinusSourceAlpha: *v = VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA; return 0;
        case MTLBlendFactorDestinationColor: *v = VK_BLEND_FACTOR_DST_COLOR; return 0;
        case MTLBlendFactorOneMinusDestinationColor: *v = VK_BLEND_FACTOR_ONE_MINUS_DST_COLOR; return 0;
        case MTLBlendFactorDestinationAlpha: *v = VK_BLEND_FACTOR_DST_ALPHA; return 0;
        case MTLBlendFactorOneMinusDestinationAlpha: *v = VK_BLEND_FACTOR_ONE_MINUS_DST_ALPHA; return 0;
        case MTLBlendFactorSourceAlphaSaturated: *v = VK_BLEND_FACTOR_SRC_ALPHA_SATURATE; return 0;
        case MTLBlendFactorBlendColor: *v = VK_BLEND_FACTOR_CONSTANT_COLOR; return 0;
        case MTLBlendFactorOneMinusBlendColor: *v = VK_BLEND_FACTOR_ONE_MINUS_CONSTANT_COLOR; return 0;
        case MTLBlendFactorBlendAlpha: *v = VK_BLEND_FACTOR_CONSTANT_ALPHA; return 0;
        case MTLBlendFactorOneMinusBlendAlpha: *v = VK_BLEND_FACTOR_ONE_MINUS_CONSTANT_ALPHA; return 0;
        case MTLBlendFactorSource1Color: case MTLBlendFactorOneMinusSource1Color:
        case MTLBlendFactorSource1Alpha: case MTLBlendFactorOneMinusSource1Alpha:
            if (nvmtl_vk_dual_src()) {
                *v = f == MTLBlendFactorSource1Color ? VK_BLEND_FACTOR_SRC1_COLOR : f == MTLBlendFactorOneMinusSource1Color ? VK_BLEND_FACTOR_ONE_MINUS_SRC1_COLOR
                   : f == MTLBlendFactorSource1Alpha ? VK_BLEND_FACTOR_SRC1_ALPHA : VK_BLEND_FACTOR_ONE_MINUS_SRC1_ALPHA;
                return 0; }
            nvlog("blend: factor %lu is DUAL-SOURCE (Source1*) and this device did not enable dualSrcBlend", (unsigned long)f); return -1;
        default:
            nvlog("blend: factor %lu is not a value this driver knows (newer than its Metal enum table)", (unsigned long)f); return -1;
    }
}
static int nvmtl_blend_op(MTLBlendOperation op, uint32_t *v) {
    if ((NSUInteger)op == NVMTL_MTLBlendOperationUnspecialized) {
        nvlog("blend: operation %lu is MTLBlendOperationUnspecialized — supplied at PSO specialization; not implemented",
            (unsigned long)op); return -1; }
    switch (op) {
        case MTLBlendOperationAdd: *v = VK_BLEND_OP_ADD; return 0;
        case MTLBlendOperationSubtract: *v = VK_BLEND_OP_SUBTRACT; return 0;
        case MTLBlendOperationReverseSubtract: *v = VK_BLEND_OP_REVERSE_SUBTRACT; return 0;
        case MTLBlendOperationMin: *v = VK_BLEND_OP_MIN; return 0;
        case MTLBlendOperationMax: *v = VK_BLEND_OP_MAX; return 0;
        default:
            nvlog("blend: operation %lu is not a value this driver knows", (unsigned long)op); return -1;
    }
}
static int nvmtl_blend_from(MTLRenderPipelineColorAttachmentDescriptor *ca, int noWrite, nvk_blend_state *b) {
    *b = (nvk_blend_state){ .enabled = ca.blendingEnabled,
        .src_rgb = VK_BLEND_FACTOR_ONE, .dst_rgb = VK_BLEND_FACTOR_ZERO, .op_rgb = VK_BLEND_OP_ADD,
        .src_alpha = VK_BLEND_FACTOR_ONE, .dst_alpha = VK_BLEND_FACTOR_ZERO, .op_alpha = VK_BLEND_OP_ADD };
    if (!noWrite) {
        if (ca.writeMask & MTLColorWriteMaskRed) b->write_mask |= VK_COLOR_COMPONENT_R_BIT;
        if (ca.writeMask & MTLColorWriteMaskGreen) b->write_mask |= VK_COLOR_COMPONENT_G_BIT;
        if (ca.writeMask & MTLColorWriteMaskBlue) b->write_mask |= VK_COLOR_COMPONENT_B_BIT;
        if (ca.writeMask & MTLColorWriteMaskAlpha) b->write_mask |= VK_COLOR_COMPONENT_A_BIT;
    }
    if (b->enabled && (nvmtl_blend_factor(ca.sourceRGBBlendFactor, &b->src_rgb) ||
        nvmtl_blend_factor(ca.destinationRGBBlendFactor, &b->dst_rgb) || nvmtl_blend_op(ca.rgbBlendOperation, &b->op_rgb) ||
        nvmtl_blend_factor(ca.sourceAlphaBlendFactor, &b->src_alpha) || nvmtl_blend_factor(ca.destinationAlphaBlendFactor, &b->dst_alpha) ||
        nvmtl_blend_op(ca.alphaBlendOperation, &b->op_alpha)))
        return -1;
    return 0;
}
static NVMTLFunction *nvmtl_empty_fragment(void) {
    static NVMTLFunction *fn; static dispatch_once_t once;
    dispatch_once(&once, ^{
        static const uint32_t w[] = { 0x07230203, 0x00010000, 0, 5, 0,
            0x00020011, 1, 0x0003000E, 0, 1,
            0x0005000F, 4, 1, 0x6E69616D, 0, 0x00030010, 1, 7,
            0x00020013, 2, 0x00030021, 3, 2,
            0x00050036, 2, 1, 0, 3, 0x000200F8, 4, 0x000100FD, 0x00010038 };
        NVMTLFunction *f = [NVMTLFunction new];
        f->_spirv = [NSData dataWithBytes:w length:sizeof w]; f->_fname = @"(no fragment function)"; f->_stage = @"fragment";
        fn = f;
    });
    return fn;
}
MTLRenderPipelineDescriptor *nvmtl_gl_rewrite_descriptor(MTLRenderPipelineDescriptor *d, NSError **err);
void nvmtl_gl_tag_reflection(id refl, MTLRenderPipelineDescriptor *d);
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTLRenderPipelineDescriptor *)d error:(NSError **)err {
    if (err) *err = nil;
    d = nvmtl_gl_rewrite_descriptor(d, err); if (!d) return nil;
    if (nvmtl_vk_init()) return nvmtl_fail(err, @"pipeline: Vulkan is not up in this process");
    id<MTLFunction> vfn = d.vertexFunction, ffn = d.fragmentFunction;
    if (!vfn) return nvmtl_fail(err, @"pipeline: descriptor is missing a vertex function");
    int noFrag = ffn == nil;
    if (noFrag) ffn = nvmtl_empty_fragment();
    if (![(id)vfn isKindOfClass:[NVMTLFunction class]] || ![(id)ffn isKindOfClass:[NVMTLFunction class]])
        return nvmtl_fail(err, [NSString stringWithFormat:
            @"pipeline: REFUSED - these functions are not ours: vertex %s, fragment %s "
             "(from a library entry point this driver does not implement)",
            object_getClassName(vfn), object_getClassName(ffn)]);
    NVMTLFunction *vf = (NVMTLFunction *)vfn, *ff = (NVMTLFunction *)ffn;
    { NSError *le = nil;
      id<MTLFunction> lv = nvmtl_link_stage(vfn, d.vertexLinkedFunctions, d.vertexPreloadedLibraries, &le);
      id<MTLFunction> lfr = lv ? (noFrag ? ffn : nvmtl_link_stage(ffn, d.fragmentLinkedFunctions, d.fragmentPreloadedLibraries, &le)) : nil;
      if (!lv || !lfr) { if (err) *err = le; return nil; }
      vf = (NVMTLFunction *)lv; ff = (NVMTLFunction *)lfr; }
    id samplePayload = nvmtl_function_reflection(ff)[@"fragment_sample_positions"];
    if (samplePayload && samplePayload != [NSNull null] && !nvmtl_sample_payload_abi(samplePayload))
        return nvmtl_fail(err, @"pipeline: fragment sample-position payload ABI mismatch");
    if ([samplePayload isKindOfClass:[NSDictionary class]] && !nvmtl_vk_sample_positions_backend())
        return nvmtl_fail(err, @"pipeline: fragment sample positions require the checked NVIDIA runtime payload contract");
    NVMTLRenderPipelineState *ps = [NVMTLRenderPipelineState new];
    nvk_rt rt; memset(&rt, 0, sizeof rt);
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        MTLRenderPipelineColorAttachmentDescriptor *ca = d.colorAttachments[i];
        if (ca.pixelFormat == MTLPixelFormatInvalid) continue;
        uint32_t svk = 0, sbpp = 0; int sa8 = 0;
        if (nvmtl_pixfmt(ca.pixelFormat, &svk, &sbpp, &sa8))
            return nvmtl_fail(err, [NSString stringWithFormat:@"pipeline: colour attachment %u pixel format %lu not carried by this driver", i, (unsigned long)ca.pixelFormat]);
        if (nvmtl_blend_from(ca, noFrag, &rt.blend[i]))
            return nvmtl_fail(err, [NSString stringWithFormat:@"pipeline: colour attachment %u has a blend factor or operation this driver cannot translate (the reason is the preceding \"blend:\" log line)", i]);
        rt.cfmt[i] = svk; rt.ncol = i + 1;
    }
    for (uint32_t i = 0; i < rt.ncol; i++) {
        const nvk_blend_state *b = &rt.blend[i];
        int dual = b->enabled && ((b->src_rgb >= 15 && b->src_rgb <= 18) || (b->dst_rgb >= 15 && b->dst_rgb <= 18) ||
                                  (b->src_alpha >= 15 && b->src_alpha <= 18) || (b->dst_alpha >= 15 && b->dst_alpha <= 18));
        if (!dual) continue;
        if (i != 0) return nvmtl_fail(err, [NSString stringWithFormat:@"pipeline: colour attachment %u uses a dual-source factor; only attachment 0 may (Metal and Vulkan alike)", i]);
        if (!nvmtl_spirv_has_index1(ff->_spirv.bytes, ff->_spirv.length))
            return nvmtl_fail(err, [NSString stringWithFormat:@"pipeline: %@ uses a dual-source blend factor and its fragment SPIR-V has no Index 1 output (the translator predates dual-source)", ff->_fname]);
        static int said; if (said++ < 4) nvlog("pipeline: par2 dual-source blending on %s", ff->_fname.UTF8String);
    }
    uint32_t dvk = 0, dasp = 0;
    MTLPixelFormat dpf = d.depthAttachmentPixelFormat != MTLPixelFormatInvalid ? d.depthAttachmentPixelFormat : d.stencilAttachmentPixelFormat;
    if (dpf != MTLPixelFormatInvalid && nvmtl_depthfmt(dpf, &dvk, &dasp)) return nvmtl_fail(err, @"pipeline: depth/stencil attachment pixel format not carried by this driver");
    rt.dfmt = dvk; rt.discard = d.rasterizationEnabled ? 0u : 1u;
    rt.a2c = d.alphaToCoverageEnabled ? 1u : 0u; rt.a2one = d.alphaToOneEnabled ? 1u : 0u;
    nvk_blend_state blend = rt.blend[0]; uint32_t cvk = rt.cfmt[0];
    ps->_vname = vf->_fname; ps->_fname = ff->_fname;
    ps->_blend = [NSString stringWithFormat:@"%u rgb(%u,%u,op %u) a(%u,%u,op %u) mask %x fmt %u", blend.enabled, blend.src_rgb, blend.dst_rgb,
        blend.op_rgb, blend.src_alpha, blend.dst_alpha, blend.op_alpha, blend.write_mask, (unsigned)cvk];
    if (blend.enabled || blend.write_mask != 0xf) nvlog("pipeline: blend %u rgb(%u,%u,op %u) alpha(%u,%u,op %u) vkmask %x", blend.enabled,
        blend.src_rgb, blend.dst_rgb, blend.op_rgb, blend.src_alpha, blend.dst_alpha, blend.op_alpha, blend.write_mask);
    for (uint32_t i = 0; i < rt.ncol; i++)
        if (rt.cfmt[i]) nvlog("pipeline: rt slot %u/%u cfmt %u blend %u rgb(%u,%u,op %u) alpha(%u,%u,op %u) vkmask %x%s",
            i, rt.ncol, rt.cfmt[i], rt.blend[i].enabled, rt.blend[i].src_rgb, rt.blend[i].dst_rgb, rt.blend[i].op_rgb,
            rt.blend[i].src_alpha, rt.blend[i].dst_alpha, rt.blend[i].op_alpha, rt.blend[i].write_mask,
            rt.blend[i].write_mask ? "" : "  <- writes NO colour");
    if (rt.a2c || rt.a2one) nvlog("pipeline: alphaToCoverage %u alphaToOne %u", rt.a2c, rt.a2one);
    nvk_vertex_input vin; memset(&vin, 0, sizeof vin); NSMutableString *vdesc = [NSMutableString string];
    MTLVertexDescriptor *vd = d.vertexDescriptor;
    for (NSUInteger a = 0; vd && a < NVMTL_NVIN; a++) {
        MTLVertexAttributeDescriptor *ad = vd.attributes[a]; if (!ad || ad.format == MTLVertexFormatInvalid) continue;
        NSUInteger bi = ad.bufferIndex; if (bi >= NVMTL_NVIN) return nvmtl_fail(err, @"pipeline: vertex attribute names a buffer index outside [0,31)");
        MTLVertexBufferLayoutDescriptor *ld = vd.layouts[bi];
        vin.attr[vin.nattr].location = (uint32_t)a; vin.attr[vin.nattr].buffer = (uint32_t)bi;
        vin.attr[vin.nattr].mtlfmt = (uint32_t)ad.format; vin.attr[vin.nattr].offset = (uint32_t)ad.offset; vin.nattr++;
        vin.layout[bi].stride = ld.stride == MTLBufferLayoutStrideDynamic ? NVMTL_STRIDE_DYNAMIC : (uint32_t)ld.stride; vin.layout[bi].step = (uint32_t)ld.stepFunction; vin.layout[bi].rate = (uint32_t)ld.stepRate;
        [vdesc appendFormat:@" a%lu=fmt%lu@buf%lu+%lu(stride %lu step %lu/%lu)", (unsigned long)a, (unsigned long)ad.format, (unsigned long)bi,
            (unsigned long)ad.offset, (unsigned long)ld.stride, (unsigned long)ld.stepFunction, (unsigned long)ld.stepRate];
    }
    ps->_vdesc = vdesc; ps->_descriptor = [d copy];
    if (!vf->_spirv.length || !ff->_spirv.length)
        return nvmtl_fail(err, [NSString stringWithFormat:
            @"pipeline: REFUSED — %@ is a visible function, not an entry point (vertex %lu B, fragment %lu B)",
            !vf->_spirv.length ? vf->_fname : ff->_fname,
            (unsigned long)vf->_spirv.length, (unsigned long)ff->_spirv.length]);
    uint32_t rsc = (uint32_t)([d respondsToSelector:@selector(rasterSampleCount)] ? d.rasterSampleCount : d.sampleCount); if (!rsc) rsc = 1;
    NSData *flk = nvmtl_varying_link(vf, ff); const void *fsb = flk ? flk.bytes : ff->_spirv.bytes; size_t fsl = flk ? flk.length : ff->_spirv.length;
    uint32_t tpt = 0, tcps = 0; int prc;
    if (nvmtl_spirv_tess_info(vf->_spirv.bytes, vf->_spirv.length, &tpt, &tcps)) {
        if (d.tessellationFactorFormat != MTLTessellationFactorFormatHalf)
            return nvmtl_fail(err, @"tess pipeline: REFUSED - tessellationFactorFormat is not Half");
        NSDictionary *tr = nvmtl_function_reflection(vf)[@"tessellation"]; if (![tr isKindOfClass:[NSDictionary class]]) tr = nil;
        NSNumber *rn = [tr[@"control_point_count"] isKindOfClass:[NSNumber class]] ? tr[@"control_point_count"] : nil;
        NSNumber *ra = [tr[@"amplification_count"] isKindOfClass:[NSNumber class]] ? tr[@"amplification_count"] : nil;
        NSDictionary *ri = [tr[@"instance_id"] isKindOfClass:[NSDictionary class]] ? tr[@"instance_id"] : nil;
        NSNumber *rl = [ri[@"location"] isKindOfClass:[NSNumber class]] ? ri[@"location"] : nil;
        if (ra.unsignedIntValue > 1) return nvmtl_fail(err, @"tess pipeline: REFUSED - vertex amplification with tessellation is not carried");
        uint32_t n = rn.unsignedIntValue ? rn.unsignedIntValue : tcps;
        if (rn.unsignedIntValue && tcps && rn.unsignedIntValue != tcps)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tess pipeline: REFUSED - %@ reflects %u control points and its module reads %u", vf->_fname, rn.unsignedIntValue, tcps]);
        if (!n || n > 32)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tess pipeline: REFUSED - %@ has no fixed control-point count in 1..32 (%u)", vf->_fname, n]);
        if (!tr) nvlog("tess pipeline: %s has no reflection - its instance_id Location is inferred from the interface", vf->_fname.UTF8String ?: "?");
        float mf = (float)d.maxTessellationFactor; if (mf > 64.0f) mf = 64.0f; if (mf < 1.0f) mf = 1.0f;
        nvk_tess_desc td = { tpt, n, (uint32_t)d.tessellationPartitionMode, (uint32_t)d.tessellationOutputWindingOrder,
            (uint32_t)d.tessellationFactorStepFunction, d.isTessellationFactorScaleEnabled ? 1u : 0u, (uint32_t)d.tessellationControlPointIndexType,
            mf, rl ? rl.intValue : -1 };
        prc = nvmtl_vk_pipeline_create_rt(vf->_spirv.bytes, vf->_spirv.length, fsb, fsl, &rt, vin.nattr ? &vin : NULL, rsc, &td, &ps->_p);
    } else
        prc = nvmtl_vk_pipeline_create_rt(vf->_spirv.bytes, vf->_spirv.length, fsb, fsl, &rt, vin.nattr ? &vin : NULL, rsc, NULL, &ps->_p);
    if (prc) {
        nvlog("pipeline FAILED: %s / %s — see the vk: line above for the VkResult", vf->_fname.UTF8String ?: "?", ff->_fname.UTF8String ?: "?");
        return nvmtl_fail(err, [NSString stringWithFormat:@"pipeline: vkCreateGraphicsPipelines FAILED (%@ / %@)", vf->_fname, ff->_fname]); }
    nvlog("newRenderPipelineStateWithDescriptor: -> %p (VkPipeline from %lu+%lu B of SPIR-V)",
          (__bridge void *)ps, (unsigned long)vf->_spirv.length, (unsigned long)ff->_spirv.length); nvmtl_render_keep_linked(ps, vf, ff); return ps; }

- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTLRenderPipelineDescriptor *)d options:(MTLPipelineOption)o
                                                          reflection:(MTLAutoreleasedRenderPipelineReflection *)r error:(NSError **)err {
    d = nvmtl_gl_rewrite_descriptor(d, err); if (!d) { if (r) *r = nil; return nil; }
    if (o & MTLPipelineOptionFailOnBinaryArchiveMiss) {
        NSString *why = nvmtl_archive_refusal(d.binaryArchives, nvmtl_archive_key_render(d));
        if (why) { if (r) *r = nil; return nvmtl_fail(err, why); }
    }
    id<MTLRenderPipelineState> ps = [self newRenderPipelineStateWithDescriptor:d error:err];
    if (r) *r = ps ? (MTLRenderPipelineReflection *)nvmtl_render_reflection(d) : nil;
    if (r && *r) nvmtl_gl_tag_reflection(*r, d);
    return ps;
}
static dispatch_queue_t nvmtl_pipeline_compile_queue(void) {
    static dispatch_queue_t queue; static dispatch_once_t once;
    dispatch_once(&once, ^{ queue = dispatch_queue_create("nvmtl.pipeline.compile", DISPATCH_QUEUE_SERIAL); });
    return queue;
}
- (void)newRenderPipelineStateWithDescriptor:(MTLRenderPipelineDescriptor *)d completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    MTLRenderPipelineDescriptor *snapshot = [d copy];
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; id<MTLRenderPipelineState> ps = [self newRenderPipelineStateWithDescriptor:snapshot error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, e); });
    }});
}
- (void)newRenderPipelineStateWithDescriptor:(MTLRenderPipelineDescriptor *)d options:(MTLPipelineOption)o
                           completionHandler:(MTLNewRenderPipelineStateWithReflectionCompletionHandler)h {
    MTLRenderPipelineDescriptor *snapshot = [d copy];
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; MTLRenderPipelineReflection *reflection = nil;
        id<MTLRenderPipelineState> ps = [self newRenderPipelineStateWithDescriptor:snapshot options:o reflection:&reflection error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, reflection, e); });
    }});
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithFunction:(id<MTLFunction>)fn options:(MTLPipelineOption)o
                                                        reflection:(MTLAutoreleasedComputePipelineReflection *)r error:(NSError **)err {
    id<MTLComputePipelineState> ps = [self newComputePipelineStateWithFunction:fn error:err];
    if (r) *r = ps ? (MTLComputePipelineReflection *)nvmtl_compute_reflection(fn) : nil;
    return ps;
}
- (void)newComputePipelineStateWithFunction:(id<MTLFunction>)fn completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; id<MTLComputePipelineState> ps = [self newComputePipelineStateWithFunction:fn error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, e); });
    }});
}
- (void)newComputePipelineStateWithFunction:(id<MTLFunction>)fn options:(MTLPipelineOption)o
                          completionHandler:(MTLNewComputePipelineStateWithReflectionCompletionHandler)h {
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; MTLComputePipelineReflection *reflection = nil;
        id<MTLComputePipelineState> ps = [self newComputePipelineStateWithFunction:fn options:o reflection:&reflection error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, reflection, e); });
    }});
}

static NSString *nvmtl_tg_mismatch(id fn, NSUInteger ask, const char *who) {
    if (!ask || ![(id)fn isKindOfClass:[NVMTLFunction class]]) return nil;
    NSUInteger attr = nvmtl_air_max_tg((NVMTLFunction *)fn);
    if (!attr || attr == ask) return nil;
    return [NSString stringWithFormat:@"Kernel specified max total threads per threadgroup (%lu) must match %s specified max total threads "
            "per threadgroup (%lu) - \"%@\" REFUSED, as both real drivers refuse it", (unsigned long)attr, who, (unsigned long)ask, ((NVMTLFunction *)fn)->_fname];
}
static void nvmtl_log_compute_descriptor(MTLComputePipelineDescriptor *d) {
    if (!d) return;
    MTLLinkedFunctions *lf = d.linkedFunctions;
    unsigned long f = 0, p = 0, b = 0, g = 0;
    if (lf) {
        f = (unsigned long)lf.functions.count;
        b = (unsigned long)lf.binaryFunctions.count;
        g = (unsigned long)lf.groups.count;
        if ([lf respondsToSelector:@selector(privateFunctions)])
            p = (unsigned long)[(NSArray *)[lf valueForKey:@"privateFunctions"] count];
    }
    nvlog("computePipelineDescriptor: \"%s\" linked=%s functions=%lu private=%lu binary=%lu groups=%lu maxTG=%lu multWidth=%d",
          d.computeFunction.name.UTF8String ?: "(nil)", lf ? "YES" : "no", f, p, b, g,
          (unsigned long)d.maxTotalThreadsPerThreadgroup,
          (int)d.threadGroupSizeIsMultipleOfThreadExecutionWidth);
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTLComputePipelineDescriptor *)d options:(MTLPipelineOption)o
                                                          reflection:(MTLAutoreleasedComputePipelineReflection *)r error:(NSError **)err {
    nvmtl_log_compute_descriptor(d);
    if (o & MTLPipelineOptionFailOnBinaryArchiveMiss) {
        NSString *why = nvmtl_archive_refusal(d.binaryArchives, nvmtl_archive_key_compute(d.computeFunction));
        if (why) { if (r) *r = nil; return nvmtl_fail(err, why); }
    }
    NSError *le = nil; id<MTLFunction> kfn = nvmtl_link_kernel(d, &le);
    if (!kfn) { if (err) *err = le; if (r) *r = nil; return nil; }
    NSString *tgWhy = nvmtl_tg_mismatch(kfn, d.maxTotalThreadsPerThreadgroup, "compute pipeline");
    if (tgWhy) { if (r) *r = nil; return nvmtl_fail(err, tgWhy); }
    id<MTLComputePipelineState> ps = [self newComputePipelineStateWithFunction:kfn error:err];
    if ([(id)ps isKindOfClass:[NVMTLComputePipelineState class]]) { ((NVMTLComputePipelineState *)ps)->_icb = d.supportIndirectCommandBuffers;
        ((NVMTLComputePipelineState *)ps)->_askT = d.maxTotalThreadsPerThreadgroup;
        ((NVMTLComputePipelineState *)ps)->_label = [d.label copy]; }
    if (r) *r = ps ? (MTLComputePipelineReflection *)nvmtl_compute_reflection(kfn) : nil;
    return ps;
}
- (void)newComputePipelineStateWithDescriptor:(MTLComputePipelineDescriptor *)d options:(MTLPipelineOption)o
                            completionHandler:(MTLNewComputePipelineStateWithReflectionCompletionHandler)h {
    MTLComputePipelineDescriptor *snapshot = [d copy];
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; MTLComputePipelineReflection *reflection = nil;
        id<MTLComputePipelineState> ps = [self newComputePipelineStateWithDescriptor:snapshot options:o reflection:&reflection error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, reflection, e); });
    }});
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTLComputePipelineDescriptor *)d error:(NSError **)err {
    nvlog("cpdesc: withDescriptor:error: -> options route");
    return [self newComputePipelineStateWithDescriptor:d options:MTLPipelineOptionNone reflection:nil error:err];
}
- (void)newComputePipelineStateWithDescriptor:(MTLComputePipelineDescriptor *)d
                            completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    MTLComputePipelineDescriptor *snapshot = [d copy];
    dispatch_async(nvmtl_pipeline_compile_queue(), ^{ @autoreleasepool {
        NSError *e = nil; id<MTLComputePipelineState> ps = [self newComputePipelineStateWithDescriptor:snapshot options:MTLPipelineOptionNone reflection:nil error:&e];
        if (h) dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ h(ps, e); });
    }});
}
- (BOOL)supportsFunctionPointers { return YES; }
- (BOOL)supportsFunctionPointersFromRender { return YES; }
- (BOOL)supportsDynamicLibraries { return YES; }
- (BOOL)supportsRenderDynamicLibraries { return YES; }
- (id<MTLDynamicLibrary>)newDynamicLibrary:(id<MTLLibrary>)lib error:(NSError **)err { return nvmtl_new_dylib(lib, err); }
- (id<MTLDynamicLibrary>)newDynamicLibraryWithURL:(NSURL *)url error:(NSError **)err { return nvmtl_new_dylib_url(url, err); }
- (id<MTLBinaryArchive>)newBinaryArchiveWithDescriptor:(MTLBinaryArchiveDescriptor *)d error:(NSError **)err { return nvmtl_new_binary_archive(d, err); }
- (BOOL)supportsShaderBarycentricCoordinates { return YES; }
- (BOOL)areBarycentricCoordsSupported { return YES; }
- (BOOL)supportsPrimitiveMotionBlur { return NO; }
- (NSMutableDictionary *)copyIOSurfaceSharedTextureProperties:(MTLTextureDescriptor *)d {
    const char *why = NULL; uint32_t vkf = 0, bpp = 0; int a8 = 0; MTLPixelFormat f = d ? d.pixelFormat : MTLPixelFormatInvalid;
    if (!d) why = "no descriptor";
    else if (d.textureType != MTLTextureType2D) why = "not a 2D texture";
    else if (d.mipmapLevelCount != 1) why = "mipmapped";
    else if (d.sampleCount > 1 || d.arrayLength != 1 || d.depth != 1) why = "multisampled, arrayed or deep";
    else if (!d.width || !d.height || d.width > 16384 || d.height > 16384) why = "size out of range";
    else if (f == MTLPixelFormatDepth16Unorm || f == MTLPixelFormatDepth32Float || f == MTLPixelFormatStencil8 || f == MTLPixelFormatDepth24Unorm_Stencil8
             || f == MTLPixelFormatDepth32Float_Stencil8 || f == MTLPixelFormatX32_Stencil8 || f == MTLPixelFormatX24_Stencil8) why = "a depth/stencil format";
    else if (f >= 130 && f < 250) why = "a block-compressed or subsampled format";
    else if (nvmtl_pixfmt(f, &vkf, &bpp, &a8) || !bpp) why = "a pixel format this driver cannot make";
    if (why) {
        nvlog("copyIOSurfaceSharedTextureProperties: fmt %lu %lux%lu type %lu mips %lu -> nil (%s; apps3)", (unsigned long)f,
              (unsigned long)(d ? d.width : 0), (unsigned long)(d ? d.height : 0), (unsigned long)(d ? d.textureType : 0), (unsigned long)(d ? d.mipmapLevelCount : 0), why);
        return nil; }
    size_t row = IOSurfaceAlignProperty(kIOSurfaceBytesPerRow, (size_t)d.width * bpp);
    size_t alloc = IOSurfaceAlignProperty(kIOSurfaceAllocSize, row * (size_t)d.height);
    NSMutableDictionary *p = [@{ (__bridge NSString *)kIOSurfaceWidth: @(d.width), (__bridge NSString *)kIOSurfaceHeight: @(d.height),
                                 (__bridge NSString *)kIOSurfaceBytesPerElement: @(bpp), (__bridge NSString *)kIOSurfaceBytesPerRow: @(row),
                                 (__bridge NSString *)kIOSurfaceAllocSize: @(alloc) } mutableCopy];
    nvlog("copyIOSurfaceSharedTextureProperties: fmt %lu %lux%lu -> %zu B/row, %zu B (apps1)", (unsigned long)f, (unsigned long)d.width, (unsigned long)d.height, row, alloc);
    return p;
}
- (id<MTLTexture>)newSharedTextureWithHandle:(MTLSharedTextureHandle *)h {
    id<MTLTexture> t = [super newSharedTextureWithHandle:h];
    BOOL ours = [(id)t isKindOfClass:[NVMTLTexture class]];
    if (ours) ((NVMTLTexture *)t)->_shareable = YES;
    nvlog("newSharedTextureWithHandle: %p -> %p%s", (__bridge void *)h, (__bridge void *)t, !t ? " (nil)" : ours ? " shareable" : " NOT ours - left as it is");
    return t;
}
- (id<MTLSharedEvent>)newSharedEventWithHandle:(MTLSharedEventHandle *)h {
    nvlog("newSharedEventWithHandle: %p -> nil (cross-process shared events are not built; apps3)", (__bridge void *)h);
    return nil;
}

- (id<MTLDepthStencilState>)newDepthStencilStateWithDescriptor:(MTLDepthStencilDescriptor *)dd {
    NVMTLDepthStencilState *s = [NVMTLDepthStencilState new];
    s->_label = [dd.label copy];
    s->_write = dd.isDepthWriteEnabled ? 1 : 0;
    s->_compare = (int)dd.depthCompareFunction;
    MTLStencilDescriptor *sd[2] = { dd.frontFaceStencil, dd.backFaceStencil }; uint32_t *o[2] = { s->_sf, s->_sb };
    for (int f = 0; f < 2; f++) {
        MTLStencilDescriptor *x = sd[f];
        o[f][0] = x ? (uint32_t)x.stencilCompareFunction : 7u;   o[f][1] = x ? (uint32_t)x.stencilFailureOperation : 0u;
        o[f][2] = x ? (uint32_t)x.depthFailureOperation : 0u;    o[f][3] = x ? (uint32_t)x.depthStencilPassOperation : 0u;
        o[f][4] = x ? (uint32_t)(x.readMask & 0xffu) : 0xffu;   o[f][5] = x ? (uint32_t)(x.writeMask & 0xffu) : 0xffu;
        if (o[f][0] != 7u || o[f][1] || o[f][2] || o[f][3]) s->_stencil = 1;
    }
    nvlog("newDepthStencilStateWithDescriptor: -> %p (write %d, compare %d, stencil %d f{%u %u %u %u %x %x} b{%u %u %u %u %x %x})",
          (__bridge void *)s, s->_write, s->_compare, s->_stencil, s->_sf[0], s->_sf[1], s->_sf[2], s->_sf[3], s->_sf[4], s->_sf[5],
          s->_sb[0], s->_sb[1], s->_sb[2], s->_sb[3], s->_sb[4], s->_sb[5]);
    return s; }

- (id<MTLSamplerState>)newSamplerStateWithDescriptor:(MTLSamplerDescriptor *)sd {
    if (nvmtl_vk_init()) return nil;
    NVMTLSamplerState *s = [NVMTLSamplerState new];
    s->_label = [sd.label copy];
    if (!sd.normalizedCoordinates) {
        NSArray *addresses=@[@"ClampToEdge",@"MirrorClampToEdge",@"Repeat",@"MirroredRepeat",@"ClampToZero",@"ClampToBorder"];
        NSArray *compare=@[@"None",@"Less",@"Equal",@"LessEqual",@"Greater",@"NotEqual",@"GreaterEqual",@"Always"];
        if(sd.sAddressMode>=addresses.count || sd.tAddressMode>=addresses.count || sd.rAddressMode>=addresses.count || sd.compareFunction>=compare.count) return nil;
        s->_pixelState=@{@"min_filter":sd.minFilter==MTLSamplerMinMagFilterLinear?@"Linear":@"Nearest",
            @"mag_filter":sd.magFilter==MTLSamplerMinMagFilterLinear?@"Linear":@"Nearest",
            @"mip_filter":(@[@"None",@"Nearest",@"Linear"])[MIN((NSUInteger)sd.mipFilter,2u)],
            @"address_mode_s":addresses[sd.sAddressMode],@"address_mode_t":addresses[sd.tAddressMode],@"address_mode_r":addresses[sd.rAddressMode],
            @"coordinates":@"Pixel",@"compare_function":compare[sd.compareFunction],@"max_anisotropy":@(sd.maxAnisotropy),
            @"lod_min_clamp":@(sd.lodMinClamp),@"lod_max_clamp":@(sd.lodMaxClamp),
            @"border_color":(@[@"TransparentBlack",@"OpaqueBlack",@"OpaqueWhite"])[MIN((NSUInteger)sd.borderColor,2u)],
            @"reduction":@"WeightedAverage",@"lod_bias":@0};
        nvlog("pixel sampler: %s",s->_pixelState.description.UTF8String);
    }
    int linear = (sd.minFilter == MTLSamplerMinMagFilterLinear || sd.magFilter == MTLSamplerMinMagFilterLinear);
    int repeat = (sd.sAddressMode == MTLSamplerAddressModeRepeat);
    int failed;
    if (sd.normalizedCoordinates && !getenv("NVMTL_NO_SAMPLER_CONTRACT")) {
        if ((NSUInteger)sd.minFilter > 1 || (NSUInteger)sd.magFilter > 1 || (NSUInteger)sd.mipFilter > 2 ||
            (NSUInteger)sd.sAddressMode > 5 || (NSUInteger)sd.tAddressMode > 5 || (NSUInteger)sd.rAddressMode > 5 ||
            (NSUInteger)sd.compareFunction > 7 || (NSUInteger)sd.borderColor > 2 || sd.maxAnisotropy < 1 || sd.maxAnisotropy > 16) return nil;
        nvmtl_sampler_desc request = { .min_filter = (uint32_t)sd.minFilter, .mag_filter = (uint32_t)sd.magFilter,
            .mip_filter = (uint32_t)sd.mipFilter, .address_s = (uint32_t)sd.sAddressMode, .address_t = (uint32_t)sd.tAddressMode,
            .address_r = (uint32_t)sd.rAddressMode, .compare_function = (uint32_t)sd.compareFunction,
            .max_anisotropy = (uint32_t)sd.maxAnisotropy, .border_color = (uint32_t)sd.borderColor,
            .lod_min = sd.lodMinClamp, .lod_max = sd.lodMaxClamp };
        failed = nvmtl_vk_sampler_create_desc(&request, &s->_s);
    } else
        failed = nvmtl_vk_sampler_create(linear, repeat, &s->_s);
    if (failed) { nvlog("newSamplerStateWithDescriptor: FAILED"); return nil; }
    nvlog("newSamplerStateWithDescriptor: -> %p (%s, %s)", (__bridge void *)s,
          linear ? "linear" : "nearest", repeat ? "repeat" : "clamp");
    return s; }

- (id<MTLComputePipelineState>)newComputePipelineStateWithFunction:(id<MTLFunction>)fn error:(NSError **)err {
    if (err) *err = nil;
    if (nvmtl_vk_init()) return nvmtl_fail(err, @"newComputePipelineState: Vulkan is not up in this process");
    if (fn && ![(id)fn isKindOfClass:[NVMTLFunction class]])
        return nvmtl_fail(err, [NSString stringWithFormat:
            @"newComputePipelineState: REFUSED - this function is not ours: %s", object_getClassName(fn)]);
    NVMTLFunction *f = (NVMTLFunction *)fn;
    if (f && f->_needsLink) { NSError *le = nil; id<MTLFunction> lk = nvmtl_link_stage(fn, nil, nil, &le); if (!lk) { if (err) *err = le; return nil; } f = (NVMTLFunction *)lk; fn = lk; }
    if (!f || !f->_spirv) return nvmtl_fail(err, @"newComputePipelineState: function has no SPIR-V");
    if (f.functionType != MTLFunctionTypeKernel) {
        nvlog("newComputePipelineState: \"%s\" is a %s, not a kernel — REFUSED",
              f->_fname.UTF8String, f->_stage.length ? f->_stage.UTF8String : "non-kernel");
        if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorCompileFailure
                                        userInfo:@{ NSLocalizedDescriptionKey: @"function is not a kernel" }];
        return nil;
    }
    NVMTLComputePipelineState *ps = [NVMTLComputePipelineState new];
    ps->_function = f;
    ps->_attrT = nvmtl_air_max_tg(f);
    if (nvmtl_vk_compute_pipeline_create(f->_spirv.bytes, f->_spirv.length, &ps->_p)) {
        return nvmtl_fail(err, @"newComputePipelineState: vkCreateComputePipelines FAILED"); }
    nvlog("newComputePipelineStateWithFunction: %s -> %p (%lu B of SPIR-V)",
          f->_fname.UTF8String, (__bridge void *)ps, (unsigned long)f->_spirv.length);
    nvmtl_vendor_attach_sass(self, ps, f);
    { id apple = nvmtl_vendor_apple_pipeline(self, f); if (apple) return apple; }
    return ps; }

- (BOOL)supportsRaytracing { return nvmtl_vk_rt_available() != 0; }
- (BOOL)supportsRaytracingFromRender { return nvmtl_vk_rt_available() != 0; }
- (MTLAccelerationStructureSizes)accelerationStructureSizesWithDescriptor:(MTLAccelerationStructureDescriptor *)desc {
    MTLAccelerationStructureSizes s = { 0, 0, 0 };
    static nvmtl_geom geoms[NVMTL_MAX_GEOMS]; int instance = 0; uint32_t icount = 0;
    uint32_t n = nvmtl_geoms_from_descriptor(desc, geoms, NVMTL_MAX_GEOMS, &instance, &icount, nil);
    size_t as = 0, sc = 0;
    if (nvmtl_vk_accel_sizes(geoms, n, instance, icount, &as, &sc)) {
        nvlog("accelerationStructureSizesWithDescriptor: FAILED (%s)", nvmtl_vk_rt_available() ? "sizes" : "no ray tracing on this NVK"); return s; }
    s.accelerationStructureSize = as; s.buildScratchBufferSize = sc + 256; s.refitScratchBufferSize = sc + 256;
    nvlog("accelerationStructureSizesWithDescriptor: %s -> %zu bytes, scratch %zu", instance ? "instances" : "primitives", as, sc);
    return s;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithSize:(NSUInteger)size {
    if (nvmtl_vk_init() || !nvmtl_vk_rt_available()) { nvlog("newAccelerationStructureWithSize: no ray tracing on this NVK"); return nil; }
    NVMTLAccelerationStructure *a = [NVMTLAccelerationStructure new];
    a->_storage = MTLStorageModePrivate;
    if (nvmtl_vk_buffer_create(size < 256 ? 256 : size, 0, &a->_b)) { nvlog("newAccelerationStructureWithSize: FAILED"); return nil; }
    nvlog("newAccelerationStructureWithSize:%lu -> %p", (unsigned long)size, (__bridge void *)a); return a;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithDescriptor:(MTLAccelerationStructureDescriptor *)desc {
    MTLAccelerationStructureSizes s = [self accelerationStructureSizesWithDescriptor:desc];
    return s.accelerationStructureSize ? [self newAccelerationStructureWithSize:s.accelerationStructureSize] : nil;
}
- (MTLSizeAndAlign)heapAccelerationStructureSizeAndAlignWithSize:(NSUInteger)size {
    size_t want = size < 256 ? 256 : size, sz = want, al = 256;
    if (nvmtl_vk_buffer_size_align(want, &sz, &al)) { sz = want; al = 256; }
    if (sz < want) sz = want;
    if (al < 256) al = 256;
    MTLSizeAndAlign r = { sz, al }; return r;
}
- (MTLSizeAndAlign)heapAccelerationStructureSizeAndAlignWithDescriptor:(MTLAccelerationStructureDescriptor *)desc {
    MTLAccelerationStructureSizes s = [self accelerationStructureSizesWithDescriptor:desc];
    if (!s.accelerationStructureSize) { MTLSizeAndAlign r = { 0, 256 }; return r; }
    return [self heapAccelerationStructureSizeAndAlignWithSize:s.accelerationStructureSize];
}
- (BOOL)supportsBCTextureCompression { return YES; }
- (BOOL)supportsInt64 { return YES; }
- (BOOL)isQuadDataSharingSupported { return YES; }
- (BOOL)supportsCounterSampling:(MTLCounterSamplingPoint)p {
    return p == MTLCounterSamplingPointAtDrawBoundary || p == MTLCounterSamplingPointAtDispatchBoundary || p == MTLCounterSamplingPointAtBlitBoundary;
}
- (NSArray *)counterSets { return @[nvmtl_timestamp_counter_set()]; }
- (id<MTLCounterSampleBuffer>)newCounterSampleBufferWithDescriptor:(MTLCounterSampleBufferDescriptor *)desc error:(NSError **)err {
    const NSUInteger n = desc.sampleCount;
    NSInteger code = MTLCounterSampleBufferErrorInvalid;
    const char *why = !desc ? "no descriptor"
                    : ![desc.counterSet.name isEqualToString:MTLCommonCounterSetTimestamp] ? "a counter set this device does not offer"
                    : (!n || n > 32768) ? "a sample count outside 1..32768" : NULL;
    void *pool = why ? NULL : nvmtl_vk_ts_pool_create((uint32_t)n);
    if (!why && !pool) { why = "the query pool could not be made"; code = MTLCounterSampleBufferErrorInternal; }
    if (why) {
        nvlog("newCounterSampleBuffer: refused - %s (set %s, %lu samples)", why, desc.counterSet.name.UTF8String ?: "nil", (unsigned long)n);
        if (err) *err = [NSError errorWithDomain:MTLCounterErrorDomain code:code
                                        userInfo:@{NSLocalizedDescriptionKey: [NSString stringWithFormat:@"counter sample buffer refused: %s", why]}];
        return nil;
    }
    NVMTLCounterSampleBuffer *b = [NVMTLCounterSampleBuffer new];
    b->_pool = pool; b->_count = n; b->_storage = desc.storageMode; b->_label = [desc.label copy]; b->_dev = self;
    if (err) *err = nil;
    return b;
}
- (void)sampleTimestamps:(MTLTimestamp *)cpu gpuTimestamp:(MTLTimestamp *)gpu {
    uint64_t c = 0, g = 0;
    if (nvmtl_vk_gpu_timestamp(&c, &g)) {
        static int said; if (!said++) nvlog("sampleTimestamps: the GPU clock could not be read - gpu answers 0 (said once)");
    }
    if (cpu) *cpu = c;
    if (gpu) *gpu = g;
}
- (id<MTLResidencySet>)newResidencySetWithDescriptor:(MTLResidencySetDescriptor *)d error:(NSError **)err {
    if (err) *err = nil;
    NVMTLResidencySet *r = [NVMTLResidencySet new]; r->_dev = self; r->_label = d.label; r->_allocs = [NSMutableArray new];
    nvlog("newResidencySetWithDescriptor: -> %p (residency is implicit here)", (__bridge void *)r); return r;
}
- (id<MTLEvent>)newEvent { NVMTLEvent *e = [NVMTLEvent new]; e->_dev = self; return e; }
- (id<MTLSharedEvent>)newSharedEvent { NVMTLSharedEvent *e = [NVMTLSharedEvent new]; e->_dev = self; return e; }
- (MTLArgumentBuffersTier)argumentBuffersSupport { return MTLArgumentBuffersTier2; }
- (BOOL)supportsFamily:(MTLGPUFamily)f {
    switch ((NSInteger)f) {
        case MTLGPUFamilyMac2: case MTLGPUFamilyCommon1: case MTLGPUFamilyCommon2: case MTLGPUFamilyCommon3: return YES;
        case 2001 : return YES;
        case 5001 : return YES;
        default: return NO;
    }
}
- (BOOL)supportsFeatureSet:(MTLFeatureSet)fs { return fs >= 10000 && fs <= 10005; }
- (id<MTLHeap>)newHeapWithDescriptor:(MTLHeapDescriptor *)hd {
    if (!hd || !hd.size) { nvlog("newHeapWithDescriptor: no size"); return nil; }
    NVMTLHeap *h = [NVMTLHeap new];
    h->_dev = self; h->_size = hd.size; h->_used = 0; h->_storage = hd.storageMode; h->_cache = hd.cpuCacheMode; h->_type = hd.type;
    h->_hazard = hd.hazardTrackingMode;
    if (hd.type == MTLHeapTypePlacement) {
        if (nvmtl_vk_heap_create(hd.size, hd.storageMode == MTLStorageModeShared, &h->_hm)) { nvlog("newHeapWithDescriptor: PLACEMENT %lu bytes FAILED", (unsigned long)hd.size); return nil; }
        nvlog("newHeapWithDescriptor: PLACEMENT %lu bytes (%s) -> %p", (unsigned long)hd.size, hd.storageMode == MTLStorageModeShared ? "shared" : "private", (__bridge void *)h);
        return h;
    }
    if (!getenv("NVMTL_NO_HEAP_ALIAS") && (hd.storageMode == MTLStorageModePrivate || hd.storageMode == MTLStorageModeShared))
        nvmtl_heap_back(h, hd.size, hd.storageMode == MTLStorageModeShared);
    nvlog("newHeapWithDescriptor: %lu bytes -> %p", (unsigned long)hd.size, (__bridge void *)h);
    return h;
}
- (MTLSizeAndAlign)heapBufferSizeAndAlignWithLength:(NSUInteger)len options:(MTLResourceOptions)opt {
    size_t sz = len, al = 256;
    if (nvmtl_vk_buffer_size_align(len, &sz, &al)) { sz = len; al = 256; }
    MTLSizeAndAlign r = { sz, al }; return r;
}
- (MTLSizeAndAlign)heapTextureSizeAndAlignWithDescriptor:(MTLTextureDescriptor *)d {
    uint32_t dvk = 0, dasp = 0, vkf = 0, bpp = 0; int a8 = 0; size_t sz = 0, al = 256;
    int depth = d && nvmtl_depthfmt(d.pixelFormat, &dvk, &dasp) == 0;
    if (!d || (!depth && nvmtl_pixfmt(d.pixelFormat, &vkf, &bpp, &a8))) { MTLSizeAndAlign r = { 0, 256 }; return r; }
    uint32_t mtl_type = (uint32_t)d.textureType;
    uint32_t layers = nvmtl_vk_layers_for_desc(mtl_type, (uint32_t)d.arrayLength, (uint32_t)d.depth);
    if (nvmtl_vk_image_size_align((uint32_t)d.width, (uint32_t)d.height, depth ? dvk : vkf, (uint32_t)d.mipmapLevelCount, mtl_type, layers, depth,
                                  (d.usage & MTLTextureUsageShaderWrite) != 0 && !a8, &sz, &al)) { sz = d.width * d.height * (bpp ? bpp : 4); al = 256; }
    MTLSizeAndAlign r = { sz, al }; return r;
}
- (id<MTLIndirectCommandBuffer>)newIndirectCommandBufferWithDescriptor:(MTLIndirectCommandBufferDescriptor *)d
                                                       maxCommandCount:(NSUInteger)n options:(MTLResourceOptions)o {
    if (!n) { nvlog("newIndirectCommandBufferWithDescriptor: maxCommandCount 0"); return nil; }
    NVMTLIndirectCommandBuffer *icb = [NVMTLIndirectCommandBuffer new];
    icb->_dev = self; icb->_cmds = [NSMutableArray arrayWithCapacity:n];
    icb->_opts = o;
    for (NSUInteger i = 0; i < n; i++) [icb->_cmds addObject:[NVMTLIndirectCommand new]];
    nvlog("newIndirectCommandBufferWithDescriptor: %lu slots -> %p", (unsigned long)n, (__bridge void *)icb);
    return icb;
}
- (id<MTLFence>)newFence {
    NVMTLFence *f = [NVMTLFence new]; f->_dev = self; return f;
}
- (id<MTLArgumentEncoder>)newArgumentEncoderWithArguments:(NSArray<MTLArgumentDescriptor *> *)args {
    if (!args.count) { nvlog("newArgumentEncoderWithArguments: 0 argument(s) -> nil"); return nil; }
    NSMutableArray *fields = [NSMutableArray new]; NSUInteger off = 0;
    for (MTLArgumentDescriptor *a in args) {
        NSUInteger sz = 8;
        switch (a.dataType) {
            case MTLDataTypeFloat: case MTLDataTypeInt: case MTLDataTypeUInt: sz = 4; break;
            case MTLDataTypeFloat2: case MTLDataTypeInt2: case MTLDataTypeUInt2: sz = 8; break;
            case MTLDataTypeFloat3: case MTLDataTypeFloat4: case MTLDataTypeInt4: case MTLDataTypeUInt4: case MTLDataTypeInt3: case MTLDataTypeUInt3: sz = 16; break;
            case MTLDataTypeFloat4x4: sz = 64; break; case MTLDataTypeFloat3x3: sz = 48; break;
            case MTLDataTypeHalf: case MTLDataTypeShort: case MTLDataTypeUShort: sz = 2; break;
            case MTLDataTypeChar: case MTLDataTypeUChar: case MTLDataTypeBool: sz = 1; break;
            default: sz = 8; break;
        }
        NSUInteger align = sz >= 16 ? 16 : sz, n = a.arrayLength ? a.arrayLength : 1;
        off = (off + align - 1) / align * align;
        for (NSUInteger k = 0; k < n; k++)
            [fields addObject:@{ @"argument_index": @(a.index + k), @"field_offset": @(off + k * sz), @"buffer_index": @(NSUIntegerMax), @"descriptor_built": @YES }];
        off += n * sz;
    }
    NVMTLArgumentEncoder *e = [NVMTLArgumentEncoder new];
    e->_fields = fields; e->_fieldByIndex = nvmtl_index_argument_fields(fields);
    nvlog("newArgumentEncoderWithArguments: %lu descriptor(s) -> %lu field(s), %lu bytes", (unsigned long)args.count, (unsigned long)fields.count, (unsigned long)off);
    return e;
}
- (NSUInteger)currentAllocatedSize { return (NSUInteger)nvmtl_vk_allocated_bytes(); }
- (uint64_t)recommendedMaxWorkingSetSize {
    uint64_t ws = nvmtl_vk_working_set();
    nvlog("recommendedMaxWorkingSetSize -> %llu MB", (unsigned long long)(ws >> 20));
    return ws;
}
- (BOOL)hasUnifiedMemory { return NO; }
- (uint64_t)dedicatedMemorySize { return nvmtl_vk_vram_bytes(); }

- (BOOL)nvmtlSampleCountOK:(NSUInteger)n {
    if (n == 0 || (n & (n - 1)) != 0 || n > 64) return NO;
    uint32_t col = 0, dep = 0; nvmtl_vk_pubcaps(&col, &dep, NULL, NULL);
    if (!col) { static int said; if (!said++) nvlog("supportsTextureSampleCount:%lu: NVK reported no sample-count mask (Vulkan down) - answering NO (UNKNOWN)", (unsigned long)n); return NO; }
    return ((col & dep) & (uint32_t)n) != 0;
}
- (BOOL)supportsTextureSampleCount:(NSUInteger)n { return [self nvmtlSampleCountOK:n]; }
- (BOOL)supportsSampleCount:(NSUInteger)n { return [self nvmtlSampleCountOK:n]; }

- (NSUInteger)maxBufferLength {
    uint64_t range = 0; nvmtl_vk_pubcaps(NULL, NULL, &range, NULL);
    if (!range) { static int said; if (!said++) nvlog("maxBufferLength: NVK limits unavailable - answering 0 (UNKNOWN)"); return 0; }
    uint64_t three_q = nvmtl_vk_working_set() / 4 * 3;
    uint64_t v = range < three_q ? range : three_q;
    static int said;
    if (!said++) nvlog("maxBufferLength -> %llu MB (NVK maxStorageBufferRange %llu MB, 3/4 of working set %llu MB)",
                       (unsigned long long)(v >> 20), (unsigned long long)(range >> 20), (unsigned long long)(three_q >> 20));
    return (NSUInteger)v;
}

- (NSUInteger)minimumLinearTextureAlignmentForPixelFormat:(MTLPixelFormat)fmt {
    (void)fmt;
    uint64_t a = 0; nvmtl_vk_pubcaps(NULL, NULL, NULL, &a);
    uint64_t v = a > 256 ? a : 256;
    static int said;
    if (!said++) nvlog("minimumLinearTextureAlignmentForPixelFormat: -> %llu (NVK asked %llu, Apple's D300 ruler 256)",
                       (unsigned long long)v, (unsigned long long)a);
    return (NSUInteger)v;
}

- (MTLReadWriteTextureTier)readWriteTextureSupport { return MTLReadWriteTextureTier2; }

- (BOOL)areRasterOrderGroupsSupported { return NO; }
- (BOOL)areProgrammableSamplePositionsSupported { return nvmtl_vk_sample_positions_backend() != 0; }
- (void)getDefaultSamplePositions:(MTLSamplePosition *)positions count:(NSUInteger)count {
    nvmtl_sample_pattern p;
    if (!positions || count > UINT32_MAX || nvmtl_vk_sample_positions_default((uint32_t)count, &p)) return;
    for (NSUInteger i = 0; i < count; ++i) positions[i] = (MTLSamplePosition){ p.position[i].x, p.position[i].y };
}

- (BOOL)shouldMaximizeConcurrentCompilation { return [objc_getAssociatedObject(self, _cmd) boolValue]; }
- (void)setShouldMaximizeConcurrentCompilation:(BOOL)v {
    objc_setAssociatedObject(self, @selector(shouldMaximizeConcurrentCompilation), @(v ? YES : NO), OBJC_ASSOCIATION_RETAIN);
    nvlog("setShouldMaximizeConcurrentCompilation:%d - recorded; the translator path is still serialised", v ? 1 : 0);
}

- (id<MTLCommandQueue>)newCommandQueueWithDescriptor:(MTLCommandQueueDescriptor *)d {
    NSUInteger n = d ? d.maxCommandBufferCount : 0;
    if (d && [d respondsToSelector:@selector(logState)] && [d logState]) {
        static int said;
        if (!said++) nvlog("newCommandQueueWithDescriptor: an MTLLogState was set - shader logging is not implemented; the queue is created without it");
    }
    return [self newCommandQueueWithMaxCommandBufferCount:n ? n : 64];
}

- (MTLArchitecture *)architecture {
    id cached = objc_getAssociatedObject(self, _cmd);
    if (cached) return cached;
    Class ac = objc_getClass("MTLArchitecture");
    Ivar nv = ac ? class_getInstanceVariable(ac, "_name") : NULL;
    if (!ac || !nv) {
        static int said;
        if (!said++) nvlog("architecture: MTLArchitecture%s unavailable on this macOS - answering nil (UNKNOWN)", ac ? "'s _name ivar" : "");
        return nil;
    }
    NSMutableString *s = [NSMutableString stringWithString:@"nvk_"];
    int gap = 1;
    for (const char *p = nvmtl_vk_device_name(); *p; p++) {
        char c = *p;
        int alnum = (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
        if (alnum) { if (c >= 'A' && c <= 'Z') c = (char)(c - 'A' + 'a'); [s appendFormat:@"%c", c]; gap = 0; }
        else if (!gap) { [s appendString:@"_"]; gap = 1; }
    }
    id a = [[ac alloc] init];
    if (!a) { static int said; if (!said++) nvlog("architecture: [MTLArchitecture alloc] init failed - answering nil (UNKNOWN)"); return nil; }
    object_setIvar(a, nv, (__bridge id)CFBridgingRetain([s copy]));
    objc_setAssociatedObject(self, _cmd, a, OBJC_ASSOCIATION_RETAIN);
    nvlog("architecture -> %s", [s UTF8String]);
    return a;
}

- (NSUInteger)maxTextureLayers { uint32_t l = 0; nvmtl_vk_limits(&l, NULL, NULL, NULL, NULL); return l; }
- (NSUInteger)maxComputeThreadgroupMemory { uint32_t b = 0; nvmtl_vk_limits(NULL, &b, NULL, NULL, NULL); return b; }
- (NSUInteger)maxThreadgroupMemoryLength { return [self maxComputeThreadgroupMemory]; }
- (MTLSize)maxThreadsPerThreadgroup { uint32_t wg[3] = { 0, 0, 0 }; nvmtl_vk_limits(NULL, NULL, wg, NULL, NULL); return MTLSizeMake(wg[0], wg[1], wg[2]); }
- (NSUInteger)gpuCoreCount { uint32_t sm = 0; nvmtl_vk_limits(NULL, NULL, NULL, NULL, &sm);
    if (!sm) { static int said; if (!said++) nvlog("gpuCoreCount: NVK reported no SM count; answering 1 so MPS divides by something real"); return 1; }
    return sm; }

@end

#import "NVMTLDevice_contract.m"

#import "NVMTLObjects.m"
#import "NVMTLForward.m"
#import "NVMTLLibraryLoad.m"
#import "NVMTLGL.m"
#import "NVMTLEmbedded.m"
#import "NVMTLVendorCompiler.m"
#include "nvmtl_vk.c"
