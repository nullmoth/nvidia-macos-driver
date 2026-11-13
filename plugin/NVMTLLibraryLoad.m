/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <xlocale.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <dirent.h>
#import "NVMTLObjects.h"
#include <dlfcn.h>
#include <stdatomic.h>
#include <spawn.h>
#include <sys/wait.h>
#include <signal.h>
#include <errno.h>
#include <fcntl.h>
#include <time.h>
#include <os/lock.h>
extern char **environ;

void nvlog(const char *fmt, ...);

#import <CommonCrypto/CommonDigest.h>
// The out-of-process AIR decoder: nvmtl-air-dis (plugin/air_dis.c) beside this plugin, built on the same upstream LLVM as the
// in-process reader (air-runtime/libLLVM.dylib). Earlier packages ran a copy of the Metal toolchain's air-opt from
// /Library/GPUBundles/nvmtl; that program is not ours to redistribute and is no longer looked for.
static const char *nvmtl_air_dis_path(void) {
    static char path[PATH_MAX]; static dispatch_once_t once;
    dispatch_once(&once, ^{
        Dl_info info;
        if (dladdr((const void *)&nvmtl_air_dis_path, &info) && info.dli_fname) {
            NSString *p = [[@(info.dli_fname) stringByDeletingLastPathComponent] stringByAppendingPathComponent:@"nvmtl-air-dis"];
            strlcpy(path, p.fileSystemRepresentation, sizeof path);
        }
    });
    return path[0] ? path : NULL;
}
#include <unistd.h>
static inline const char *nvmtl_pick_ll(const char *staged, const char *dev) {
    return access(staged, R_OK) == 0 ? staged : dev;
}
#ifndef NVMTL_XLATE_LIB
#define NVMTL_XLATE_LIB nvmtl_pick_ll("/Library/GPUBundles/nvmtl/libnvmtl_translate.dylib", \
                                      "/Library/GPUBundles/nvmtl/libnvmtl_translate.dylib")
#endif

typedef int (*nvmtl_translate_fn)(const char *ll, const char *stage, uint8_t **out, size_t *out_len, char *err, size_t err_len);
typedef int (*nvmtl_fc_fn)(const char *ll, const char *stage, const uint32_t *idx, const uint32_t *sizes,
                           const uint8_t *const *payloads, size_t n, uint8_t **out, size_t *out_len, char *err, size_t err_len);
typedef int (*nvmtl_refl_fn)(const char *ll, const char *stage, uint8_t **out, size_t *out_len, char *err, size_t err_len);
typedef int (*nvmtl_refl_fc_fn)(const char *ll, const char *stage,
                                const uint32_t *idx, const uint32_t *sizes, const uint8_t **payloads, size_t n,
                                uint8_t **out, size_t *out_len, char *err, size_t err_len);

typedef int (*nvmtl_lower_fn)(const uint8_t *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_lower_fn xlate_lower;
typedef int (*nvmtl_lower_ex_fn)(const uint8_t *, size_t, uint32_t, uint8_t **, size_t *, uint32_t *, char *, size_t);
static nvmtl_lower_ex_fn xlate_lower_ex;
typedef int (*nvmtl_to_uniform_fn)(const uint8_t *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_to_uniform_fn xlate_to_uniform;   /* the table of an unlowered module moved to a Uniform block */
static nvmtl_translate_fn xlate;
#include "nvmtl_spvcache_name.h"
#include "nvmtl_pretranslate.h"
#include "nvmtl_cache_file.h"
int nvmtl_vk_lost(void);
// The cache tag: the translator build (sha256-<64 hex>, 71 characters) plus every mode suffix. This buffer was 80 bytes,
// so a tag with -u2 and -p5 had room for only "-a" of "-as1", and any suffix after it was silently dropped: entries
// translated with and without the switches that follow shared one name, and a lookup could be served a module made in
// the other mode. nvmtl_spvcache_mode_tag refuses a tag that does not fit instead of cutting it.
static char gXlateTag[NVMTL_SPVCACHE_TAG_MAX] = "untagged";
// The translator build alone, without the mode suffixes (-u2, -p5, ...) gXlateTag adds: the stale-entry sweep keys on it.
static char gXlateBuildTag[NVMTL_SPVCACHE_TAG_MAX] = "untagged";
static void nvmtl_translator_cache_tag(const char *path, char *tag, size_t capacity) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    CC_SHA256_CTX context; CC_SHA256_Init(&context);
    BOOL complete = fd >= 0; uint8_t buffer[65536];
    if (fd >= 0) {
        for (;;) {
            ssize_t n = read(fd, buffer, sizeof buffer);
            if (n > 0) { CC_SHA256_Update(&context, buffer, (CC_LONG)n); continue; }
            if (n == 0) break;
            if (errno == EINTR) continue;
            complete = NO; break;
        }
        close(fd);
    }
    if (complete) {
        uint8_t digest[CC_SHA256_DIGEST_LENGTH]; CC_SHA256_Final(digest, &context);
        char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
        for (size_t i = 0; i < sizeof digest; ++i) snprintf(hex + 2*i, 3, "%02x", digest[i]);
        snprintf(tag, capacity, "sha256-%s", hex);
    } else {
        snprintf(tag, capacity, "private-%d-%llx", getpid(),
                 (unsigned long long)clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW));
        nvlog("translator cache: content read failed; using process-private identity");
    }
}
typedef int (*nvmtl_sampler_fn)(const char *, const char *, const char *, uint8_t **, size_t *, char *, size_t);
static nvmtl_sampler_fn xlate_sampler;
typedef void (*nvmtl_tglen_fn)(const uint32_t *, uint32_t);
static nvmtl_tglen_fn xlate_tglen;
typedef uint32_t (*nvmtl_stagein_fn)(const uint32_t *, uint32_t);
static nvmtl_stagein_fn xlate_stagein;
static _Thread_local BOOL g_translate_variant_forced;
static nvmtl_fc_fn xlate_fc;
// One translation per function for every set of function constants (NVMTL_FC_SPEC=1): nvmtl_translate_spec makes a module
// whose function constants are specialization constants, nvmtl_spec_apply fixes a set of values in it in one pass.
typedef int (*nvmtl_spec_fn)(const char *, const char *, uint8_t **, size_t *, uint8_t **, size_t *, char *, size_t);
typedef int (*nvmtl_spec_apply_fn)(const uint8_t *, size_t, const uint8_t *, size_t, const uint32_t *, const uint32_t *,
                                   const uint8_t *const *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_spec_fn xlate_spec;
static nvmtl_spec_apply_fn xlate_spec_apply;
static nvmtl_refl_fn xlate_refl;
static nvmtl_refl_fc_fn xlate_refl_fc;
typedef int (*nvmtl_vrefs_fn)(const char *, uint8_t **, size_t *, char *, size_t);
typedef int (*nvmtl_link_fn)(const char *, const char *, const char *const *, const char *const *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_vrefs_fn xlate_vrefs;
static nvmtl_link_fn xlate_link;
typedef int (*nvmtl_link_rt_fn)(const char *, const char *, const char *const *, const char *const *, size_t,
                                const char *const *, const char *const *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_link_rt_fn xlate_link_rt;
typedef int (*nvmtl_link_ext_fn)(const char *, const char *const *, size_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_link_ext_fn xlate_link_ext;
typedef int (*nvmtl_lower_mesh_fn)(const char *, const char *, uint8_t **, size_t *, uint8_t **, size_t *, uint32_t *, char *, size_t);
static nvmtl_lower_mesh_fn xlate_lower_mesh;
typedef int (*nvmtl_promote_uniform_fn)(const uint8_t *, size_t, uint32_t, uint32_t,
                                       uint8_t **, size_t *, uint32_t *, char *, size_t);
static nvmtl_promote_uniform_fn xlate_promote_uniform;
typedef int (*nvmtl_tile_rewrite_fn)(const char *, uint32_t, uint32_t, uint32_t, uint8_t **, size_t *, char *, size_t);
static nvmtl_tile_rewrite_fn xlate_tile_rewrite;

static int nvmtl_xlate_ready(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        Dl_info info; NSString *bundled = nil;
        if (dladdr((const void *)&nvmtl_xlate_ready, &info) && info.dli_fname)
            bundled = [[@(info.dli_fname) stringByDeletingLastPathComponent] stringByAppendingPathComponent:@"libnvmtl_translate.dylib"];
        const char *xpath = bundled && access(bundled.fileSystemRepresentation,R_OK)==0 ? bundled.fileSystemRepresentation : NVMTL_XLATE_LIB;
        void *h = dlopen(xpath, RTLD_NOW | RTLD_LOCAL);
        if (h) nvmtl_translator_cache_tag(xpath, gXlateTag, sizeof gXlateTag);
        strlcpy(gXlateBuildTag, gXlateTag, sizeof gXlateBuildTag);
        // Each mode below changes the SPIR-V a module translates to, so a cache entry written in one mode must never be
        // served in another: unrolled AIR, canonical pointers, argument-buffer samplers, and the two translator switches
        // an application profile sets.
        { const char *as = getenv("NVMTL_ARGBUF_SAMPLERS");
          if (h && !nvmtl_spvcache_mode_tag(gXlateTag, sizeof gXlateTag, gXlateBuildTag, !getenv("NVMTL_NO_AIR_UNROLL"),
                                       getenv("NVMTL_CANONICAL_POINTERS") != NULL, as && !strcmp(as, "1"),
                                       getenv("NVMTL_NO_PTR_PHI_LOAD") != NULL, getenv("NVMTL_NO_PIXEL_HW_FILTER") != NULL)) {
              // A tag that cannot name every mode cannot keep the modes apart: no cache entry is read or written.
              strlcpy(gXlateTag, "untagged", sizeof gXlateTag);
              nvlog("translate: cache tag does not fit - the shader cache is OFF in this process");
          } }
        // Typed dummy slots change every module that indexes the heap (a null resource id is redirected), so their
        // SPIR-V never shares a cache entry with the other mode.
        { extern int nvmtl_typed_dummies_wanted(void);
          if (h && strcmp(gXlateTag, "untagged") && nvmtl_typed_dummies_wanted()) strlcat(gXlateTag, "-td1", sizeof gXlateTag); }
        if (!h) { nvlog("translate: dlopen(%s): %s", NVMTL_XLATE_LIB, dlerror()); return; }
        xlate = (nvmtl_translate_fn)dlsym(h, "nvmtl_translate");
        { char tid[1200];
          nvlog("translate: opened %s (cache tag %s)%s", nvmtl_image_ident((const void *)xlate, tid, sizeof tid), gXlateTag,
                xlate ? "" : " - it has NO nvmtl_translate"); }
        xlate_lower = (nvmtl_lower_fn)dlsym(h, "nvmtl_lower_buffer_addresses");
        xlate_lower_ex = (nvmtl_lower_ex_fn)dlsym(h, "nvmtl_lower_buffer_addresses_ex");
        xlate_to_uniform = (nvmtl_to_uniform_fn)dlsym(h, "nvmtl_table_to_uniform");
        if (!xlate_to_uniform) nvlog("translate: no nvmtl_table_to_uniform in this translator - a module that address lowering refuses is refused outright while the table is Uniform");
        xlate_promote_uniform = (nvmtl_promote_uniform_fn)dlsym(h, "nvmtl_promote_gl_uniform");
        xlate_free = (nvmtl_free_fn)dlsym(h, "nvmtl_translate_free");
        xlate_sampler = (nvmtl_sampler_fn)dlsym(h, "nvmtl_translate_samplers");
        xlate_tglen = (nvmtl_tglen_fn)dlsym(h, "nvmtl_translate_set_threadgroup_lengths");
        xlate_stagein = (nvmtl_stagein_fn)dlsym(h, "nvmtl_translate_set_kernel_stage_in");
        if (!xlate_tglen) nvlog("translate: no nvmtl_translate_set_threadgroup_lengths in this translator — dynamic threadgroup memory stays unsized (MPS convolutions read zeros)");
        xlate_fc = (nvmtl_fc_fn)dlsym(h, "nvmtl_translate_fc");
        xlate_spec = (nvmtl_spec_fn)dlsym(h, "nvmtl_translate_spec");
        xlate_spec_apply = (nvmtl_spec_apply_fn)dlsym(h, "nvmtl_spec_apply");
        xlate_vrefs = (nvmtl_vrefs_fn)dlsym(h, "nvmtl_visible_refs");
        xlate_link = (nvmtl_link_fn)dlsym(h, "nvmtl_link_visible");
        xlate_link_rt = (nvmtl_link_rt_fn)dlsym(h, "nvmtl_link_runtime");
        if (!xlate_link_rt) nvlog("translate: no nvmtl_link_runtime in this translator - visible function tables cannot be linked");
        xlate_link_ext = (nvmtl_link_ext_fn)dlsym(h, "nvmtl_link_externs");
        xlate_tile_rewrite = (nvmtl_tile_rewrite_fn)dlsym(h, "nvmtl_tile_rewrite");
        if (!xlate_tile_rewrite) nvlog("translate: no nvmtl_tile_rewrite in this translator - tile functions cannot be built");
        xlate_lower_mesh = (nvmtl_lower_mesh_fn)dlsym(h, "nvmtl_lower_mesh");
        if (!xlate_lower_mesh) nvlog("translate: no nvmtl_lower_mesh in this translator - mesh pipelines will be refused");
        xlate_lower_mesh2 = (nvmtl_lower_mesh2_fn)dlsym(h, "nvmtl_lower_mesh2");
        if (!xlate_lower_mesh2) nvlog("translate: no nvmtl_lower_mesh2 in this translator - object stages, indirect mesh draws and mesh constants will be refused");
        if (!xlate_link_ext) nvlog("translate: no nvmtl_link_externs in this translator - dynamic-library externs cannot be linked");
        if (!xlate_link) nvlog("translate: no nvmtl_link_visible in this translator — kernels with visible-function references cannot be linked");
        xlate_refl = (nvmtl_refl_fn)dlsym(h, "nvmtl_reflect");
        xlate_refl_fc = (nvmtl_refl_fc_fn)dlsym(h, "nvmtl_reflect_fc");
        void (*setcaps)(uint32_t) = (void (*)(uint32_t))dlsym(h, "nvmtl_translate_set_caps");
        // Bits 0/1: formatless storage images. Bit 2: this plugin has no rasterization rate maps.
        // Bit 3: heap slots 1..5 are reserved for typed dummies (nvmtl_vk.c), a null resource id may be redirected to them.
        { extern int nvmtl_typed_dummies_wanted(void);
          if (setcaps) setcaps(3u | 4u | (nvmtl_typed_dummies_wanted() ? 8u : 0u)); }
        if (!setcaps) nvlog("translate: no nvmtl_translate_set_caps in this translator — written textures stay R32f-declared (one channel)");
        if (!xlate) { nvlog("translate: no nvmtl_translate symbol"); return; }
        nvlog("translate: in-process AIR->SPIR-V library loaded");
    });
    return xlate != NULL;
}

#ifndef NVMTL_AIRCACHE_DIR
#define NVMTL_AIRCACHE_DIR "/Library/GPUBundles/nvmtl/aircache"
#endif

static _Atomic unsigned long gAirHit, gAirMiss, gAirStored;

static BOOL nvmtl_dir_writable(NSString *d);   // defined with the SPIR-V cache folder below

// The AIR cache folder is chosen once, by writing to it. access(W_OK) answers yes for a folder a sandbox then refuses to
// open for writing, which is what WindowServer's sandbox does to the shared folder (it allows writes only inside its own
// named folders), so every AIR decode ran again on each boot. The first folder that takes a real write wins.
static NSString *nvmtl_aircache_dir(void) {
    static NSString *dir; static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *e = getenv("NVMTL_AIRCACHE");
        if (e) { dir = @(e); return; }
        NSMutableArray<NSString *> *c = [NSMutableArray arrayWithObject:@NVMTL_AIRCACHE_DIR];
        NSArray *caches = NSSearchPathForDirectoriesInDomains(NSCachesDirectory, NSUserDomainMask, YES);
        if (caches.count) [c addObject:[caches[0] stringByAppendingPathComponent:@"nvmtl/aircache"]];
        char ucd[PATH_MAX];
        const size_t n = confstr(_CS_DARWIN_USER_CACHE_DIR, ucd, sizeof ucd);
        if (n > 0 && n <= sizeof ucd) {
            [c addObject:[@(ucd) stringByAppendingPathComponent:@"nvmtl/aircache"]];
            // WindowServer's own named folder under the user cache folder
            [c addObject:[[@(ucd) stringByAppendingPathComponent:@(getprogname())] stringByAppendingPathComponent:@"nvmtl/aircache"]];
        }
        NSString *tmp = [NSTemporaryDirectory() stringByAppendingPathComponent:@"nvmtl/aircache"];
        [c addObject:tmp];
        for (NSString *d in c) if (nvmtl_dir_writable(d)) { dir = d; return; }
        dir = tmp;   // nothing takes a write: the stores fail and are counted, as before
    });
    return dir;
}

// The AIR of a function the translator refused is kept (bounded, see nvmtl_failed_air.h) so a log report carries the
// shader that failed, not only the reason. Nothing leaves the machine until the user presses Send logs. The folder is the
// first of the shared one and the process's own cache folder that takes a real write. NVMTL_NO_FAILED_AIR=1, or the file
// /Library/GPUBundles/nvmtl/no-failed-air, turns the capture off.
static NSString *nvmtl_failed_air_dir(void) {
    static NSString *dir; static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *off = getenv("NVMTL_NO_FAILED_AIR");
        if ((off && *off && *off != '0') || access("/Library/GPUBundles/nvmtl/no-failed-air", F_OK) == 0) return;
        NSMutableArray<NSString *> *c = [NSMutableArray arrayWithObject:@"/Library/GPUBundles/nvmtl/failed-air"];
        char ucd[PATH_MAX];
        const size_t n = confstr(_CS_DARWIN_USER_CACHE_DIR, ucd, sizeof ucd);
        if (n > 0 && n <= sizeof ucd) {
            [c addObject:[@(ucd) stringByAppendingPathComponent:@"nvmtl/failed-air"]];
            [c addObject:[[@(ucd) stringByAppendingPathComponent:@(getprogname())] stringByAppendingPathComponent:@"nvmtl/failed-air"]];
        }
        for (NSString *d in c) if (nvmtl_dir_writable(d)) { dir = d; return; }
    });
    return dir;
}
static void nvmtl_failed_air_store(NSString *function, const char *stage, const char *reason, NSString *text) {
    NSString *dir = nvmtl_failed_air_dir();
    if (!dir || !text.length) return;
    static NSLock *lock; static dispatch_once_t once; static size_t files, bytes;
    dispatch_once(&once, ^{
        lock = [NSLock new];
        for (NSString *f in [[NSFileManager defaultManager] contentsOfDirectoryAtPath:dir error:nil]) {
            NSDictionary *a = [[NSFileManager defaultManager] attributesOfItemAtPath:[dir stringByAppendingPathComponent:f] error:nil];
            if (a) { files++; bytes += (size_t)[a fileSize]; }
        }
    });
    NSData *body = [text dataUsingEncoding:NSUTF8StringEncoding];
    char head[480], file[160];
    if (!body || !nvmtl_failed_air_header(head, sizeof head, stage, reason)) return;
    if (!nvmtl_failed_air_file_name(file, sizeof file, function.UTF8String, nvmtl_failed_air_digest((const char *)body.bytes, body.length))) return;
    NSString *path = [dir stringByAppendingPathComponent:@(file)];
    [lock lock];
    BOOL have = [[NSFileManager defaultManager] fileExistsAtPath:path];
    const int admit = have ? NVMTL_FAILED_AIR_ADMIT : nvmtl_failed_air_admit(files, bytes, body.length + strlen(head));
    if (!have && admit == NVMTL_FAILED_AIR_ADMIT) {
        NSMutableData *out = [NSMutableData dataWithBytes:head length:strlen(head)];
        [out appendData:body];
        if ([out writeToFile:path atomically:YES]) { files++; bytes += out.length; }
    }
    [lock unlock];
    if (!have && admit != NVMTL_FAILED_AIR_ADMIT)
        nvlog("library: refused %s not kept for the report (%s)", function.UTF8String,
              admit == NVMTL_FAILED_AIR_TOO_BIG ? "larger than the per-function limit" : "the failed-air folder is full");
}
static _Atomic unsigned long gSpvHit, gSpvMiss, gSpvStored;
static _Atomic unsigned long gFcHit, gFcMiss, gFcStored;
static _Atomic unsigned long gFcMemoHit, gFcLookups;

// Cache files are written, and their last-use time touched, on one serial utility queue. These ran on the caller's
// thread: WindowServer's coreanimation.pipelines thread translates shaders on first use and paid an fsync (the atomic
// write) for each one while a window drag waited on it (upstream nullmoth/nvidia-macos-driver#370). Reads stay on the
// caller, and so does every memo fill, so a process never reads or translates the same thing twice. NVMTL_CACHE_SYNC_IO=1
// (or nvrm610.conf) runs the work on the caller again, for an A/B.
static int nvmtl_cache_sync_io(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_CACHE_SYNC_IO"); on = (e && *e && *e != '0') ? 1 : 0; }
    return on;
}
static dispatch_queue_t gCacheIoQueue;
// A short-lived process (a tool, a test) can exit with writes still queued; give them up to 2 s to land.
static void nvmtl_cache_io_drain(void) {
    if (!gCacheIoQueue) return;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    dispatch_async(gCacheIoQueue, ^{ dispatch_semaphore_signal(done); });
    dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC));
}
static dispatch_queue_t nvmtl_cache_io_queue(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        gCacheIoQueue = dispatch_queue_create("nvmtl.cache-io", dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_UTILITY, 0));
        atexit(nvmtl_cache_io_drain);
        nvlog("cache io: %s", nvmtl_cache_sync_io() ? "synchronous (NVMTL_CACHE_SYNC_IO)" : "cache writes and touches run on nvmtl.cache-io");
    });
    return gCacheIoQueue;
}
static void nvmtl_cache_io(dispatch_block_t work) {
    if (nvmtl_cache_sync_io()) { work(); return; }
    dispatch_async(nvmtl_cache_io_queue(), work);
}
// The touch only keeps the trimmer's age order, so once per file per process is enough; memo hits would otherwise
// queue a block on every lookup.
static void nvmtl_cache_touch(NSString *path) {
    if (!path) return;
    static NSMutableSet<NSString *> *touched; static os_unfair_lock lock = OS_UNFAIR_LOCK_INIT;
    NSString *p = [path copy];
    os_unfair_lock_lock(&lock);
    if (!touched) touched = [NSMutableSet set];
    const BOOL first = ![touched containsObject:p];
    if (first) [touched addObject:p];
    os_unfair_lock_unlock(&lock);
    if (first) nvmtl_cache_io(^{ utimes(p.fileSystemRepresentation, NULL); });
}
// One line after the first 500 function-constant lookups and every 2000 after: a boot log then shows whether the
// process (WindowServer above all) is translating or reading. Disk hits are the hits the memo did not serve.
static void nvmtl_fc_note_lookup(void) {
    const unsigned long n = atomic_fetch_add(&gFcLookups, 1) + 1;
    if (n != 500 && !(n > 500 && (n - 500) % 2000 == 0)) return;
    const unsigned long hit = gFcHit, memo = gFcMemoHit;
    nvlog("fc cache: %lu lookups: %lu memo hit, %lu disk hit, %lu miss, %lu stored", n, memo, hit > memo ? hit - memo : 0,
          (unsigned long)gFcMiss, (unsigned long)gFcStored);
}

static _Atomic unsigned long long gSpvAdded;
static _Atomic int gSpvTrimRunning;
static _Atomic int gSpvTrimArmed = 1;

typedef struct { long long use; unsigned long long size; } nvmtl_spvent;
static int nvmtl_spvent_older(const void *a, const void *b) {
    long long x = ((const nvmtl_spvent *)a)->use, y = ((const nvmtl_spvent *)b)->use;
    return (x > y) - (x < y);
}

static unsigned long long nvmtl_spvcache_rearm_bytes(void) {
    static unsigned long long rearm; static dispatch_once_t o;
    dispatch_once(&o, ^{
        rearm = 256ull * 1024 * 1024;
        const char *r = getenv("NVMTL_SPVCACHE_REARM_MB");
        if (r) rearm = strtoull(r, NULL, 10) * 1024ull * 1024;
    });
    return rearm;
}

static void nvmtl_spvcache_trim_now(const char *dirc) {
    unsigned long long cap = 2048ull * 1024 * 1024;
    const char *e = getenv("NVMTL_SPVCACHE_MAX_MB");
    if (e) { unsigned long long v = strtoull(e, NULL, 10); if (!v) { nvlog("spvcache: trim disabled by NVMTL_SPVCACHE_MAX_MB=0"); return; } cap = v * 1024ull * 1024; }
    unsigned long long pct = 80;
    { const char *t = getenv("NVMTL_SPVCACHE_TARGET_PCT");
      if (t) { unsigned long long v = strtoull(t, NULL, 10); if (v >= 10 && v <= 99) pct = v; } }
    unsigned long long target = cap / 100 * pct;

    DIR *dh = opendir(dirc);
    if (!dh) { nvlog("spvcache: trim CANNOT READ %s: %s - cache NOT bounded this process", dirc, strerror(errno)); return; }
    size_t room = 4096, n = 0; unsigned long skipped = 0;
    nvmtl_spvent *ents = malloc(room * sizeof *ents);
    unsigned long long total = 0;
    struct dirent *de; char p[PATH_MAX];
    while ((de = readdir(dh))) {
        if (de->d_name[0] == '.') continue;
        if (snprintf(p, sizeof p, "%s/%s", dirc, de->d_name) >= (int)sizeof p) { skipped++; continue; }
        struct stat st;
        if (stat(p, &st) != 0 || !S_ISREG(st.st_mode)) { skipped++; continue; }
        total += (unsigned long long)st.st_size;
        if (!ents) continue;
        if (n == room) {
            nvmtl_spvent *r = realloc(ents, room * 2 * sizeof *ents);
            if (!r) { free(ents); ents = NULL; continue; }
            ents = r; room *= 2;
        }
        ents[n].use = (long long)st.st_mtime; ents[n].size = (unsigned long long)st.st_size; n++;
    }
    closedir(dh);
    if (!ents) { nvlog("spvcache: trim ABANDONED - out of memory listing %s (%llu MiB counted so far)", dirc, total >> 20); return; }

    if (total <= cap) {
        nvlog("spvcache: %llu.%llu MiB in %lu files (%lu unreadable), under the %llu MiB cap - nothing trimmed",
              total >> 20, ((total & 0xFFFFFull) * 10) >> 20, (unsigned long)n, skipped, cap >> 20);
        free(ents); return;
    }
    qsort(ents, n, sizeof *ents, nvmtl_spvent_older);
    unsigned long long need = total - target;
    unsigned long long slack = need + need / 16;
    unsigned long long acc = 0; long long cutoff = ents[0].use; unsigned long plan = 0;
    for (size_t i = 0; i < n; i++) { acc += ents[i].size; cutoff = ents[i].use; plan = (unsigned long)(i + 1); if (acc >= slack) break; }
    free(ents);

    nvlog("spvcache: %llu.%llu MiB in %lu files (%lu unreadable) over the %llu MiB cap - TRIMMING oldest-use-first to %llu MiB (%llu%%): need %llu.%llu MiB, up to %lu files at last-use <= %lld",
          total >> 20, ((total & 0xFFFFFull) * 10) >> 20, (unsigned long)n, skipped, cap >> 20,
          target >> 20, pct, need >> 20, ((need & 0xFFFFFull) * 10) >> 20, plan, cutoff);

    dh = opendir(dirc);
    if (!dh) { nvlog("spvcache: trim ABANDONED before unlinking - reopen %s: %s - NOTHING was deleted", dirc, strerror(errno)); return; }
    unsigned long long freed = 0; unsigned long gone = 0, failed = 0; int last = 0;
    while ((de = readdir(dh))) {
        if (freed >= need) break;
        if (de->d_name[0] == '.') continue;
        if (snprintf(p, sizeof p, "%s/%s", dirc, de->d_name) >= (int)sizeof p) continue;
        struct stat st;
        if (stat(p, &st) != 0 || !S_ISREG(st.st_mode)) continue;
        if ((long long)st.st_mtime > cutoff) continue;
        if (unlink(p) == 0) { freed += (unsigned long long)st.st_size; gone++; }
        else { failed++; last = errno; }
    }
    closedir(dh);
    unsigned long long remain = total > freed ? total - freed : 0;
    nvlog("spvcache: trimmed %lu files / %llu.%llu MiB of %llu.%llu MiB needed, %lu unlinks failed (%s); %llu.%llu MiB of the walked set remain against a %llu MiB target%s",
          gone, freed >> 20, ((freed & 0xFFFFFull) * 10) >> 20,
          need >> 20, ((need & 0xFFFFFull) * 10) >> 20,
          failed, failed ? strerror(last) : "none",
          remain >> 20, ((remain & 0xFFFFFull) * 10) >> 20, target >> 20,
          freed >= need ? "" : " - SHORT, TARGET NOT REACHED");
}

static void nvmtl_spvcache_trim_async(NSString *dir) {
    unsigned long long rearm = nvmtl_spvcache_rearm_bytes();
    if (!atomic_load_explicit(&gSpvTrimArmed, memory_order_relaxed) &&
        (rearm == 0 || atomic_load_explicit(&gSpvAdded, memory_order_relaxed) < rearm)) return;
    if (atomic_exchange(&gSpvTrimRunning, 1)) return;
    atomic_store(&gSpvTrimArmed, 0);
    atomic_store(&gSpvAdded, 0);
    NSString *dirCopy = [dir copy];
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_BACKGROUND, 0), ^{
      locale_t nvPrevLocale = uselocale(LC_C_LOCALE);
      @try {
        char dirc[PATH_MAX];
        if ([dirCopy getFileSystemRepresentation:dirc maxLength:sizeof dirc]) nvmtl_spvcache_trim_now(dirc);
        else nvlog("spvcache: trim ABANDONED - no fs representation for the cache directory");
      } @catch (NSException *x) {
        nvlog("spvcache: trim ABANDONED for this process - %s: %s (patch_fsrep.py)",
              x.name.UTF8String ?: "?", x.reason.UTF8String ?: "?");
      }
      atomic_store(&gSpvTrimRunning, 0);
      uselocale(nvPrevLocale);
    });
}

// The SPIR-V cache folder is chosen once, by creating it and writing to it. Sandboxed agents (iconservicesagent) could
// not create the folder beside their AIR cache; every store failed with "folder doesn't exist" and every launch
// translated its shaders again. The process's own temporary folder is always writable inside its sandbox.
static BOOL nvmtl_dir_writable(NSString *d) {
    if (!d || ![[NSFileManager defaultManager] createDirectoryAtPath:d withIntermediateDirectories:YES attributes:nil error:nil]) return NO;
    NSString *probe = [d stringByAppendingPathComponent:[NSString stringWithFormat:@".probe-%d", getpid()]];
    const BOOL ok = [[NSData data] writeToFile:probe options:0 error:nil];
    if (ok) unlink(probe.fileSystemRepresentation);
    return ok;
}
// A cache entry's name carries the build of the translator that made it, so after a driver update none of the older
// entries can ever be found again: they are dead weight, and the first login after an update pays to translate
// everything from scratch while they sit there. The folder is swept once per process, on a background queue, so no
// frame waits for it.
// Measured on one machine: the window server's cache held 2,672 entries from seven translator builds (709 MB together
// with the AIR cache) of which 372 were usable, and the login after an update spent 107 s at 79% CPU in translation
// with nothing on screen. The size cap had never noticed, because the total stayed below it.
// The sweep must not run before the translator has been opened. Until then the build tag is "untagged", nothing would
// match it, and a cache the running driver can use would be deleted.
static void nvmtl_spvcache_drop_stale(const char *dirc) {
    if (!dirc || !strcmp(gXlateBuildTag, "untagged")) {
        nvlog("spvcache: stale-tag sweep SKIPPED - the translator has no cache tag yet; nothing was deleted");
        return;
    }
    // Every process shares this folder unless it names its own cache, and each process's modes (canonical pointers,
    // argument-buffer samplers, an application profile's translator switches) give its entries a different tag. Only
    // entries from another translator BUILD are stale; an entry of the same build in another mode belongs to a process
    // that runs in that mode, and deleting it made each process wipe the others' caches on every start.

    DIR *eh = opendir(dirc);
    if (!eh) {
        nvlog("spvcache: stale-tag sweep CANNOT READ %s: %s - entries from older drivers are left in place",
              dirc, strerror(errno));
        return;
    }
    unsigned long gone = 0, kept = 0, failed = 0;
    unsigned long long freed = 0;
    const struct dirent *de;
    while ((de = readdir(eh)) != NULL) {
        const size_t n = strlen(de->d_name);
        if (n < 7 || strcmp(de->d_name + n - 7, ".c3.spv") != 0) continue;   /* not one of ours */
        if (nvmtl_spvcache_same_build(de->d_name, gXlateBuildTag)) { kept++; continue; }
        char full[1200];
        if (snprintf(full, sizeof full, "%s/%s", dirc, de->d_name) >= (int)sizeof full) continue;
        struct stat st;
        const unsigned long long sz = (stat(full, &st) == 0) ? (unsigned long long)st.st_size : 0;
        if (unlink(full) == 0) { gone++; freed += sz; } else failed++;
    }
    closedir(eh);
    if (gone || failed)
        nvlog("spvcache: dropped %lu entries / %llu MiB left by earlier drivers (%lu of build %s kept, %lu could not be removed)",
              gone, freed >> 20, kept, gXlateBuildTag, failed);
}
static NSString *nvmtl_spvcache_dir(void) {
    static NSString *dir; static dispatch_once_t once;
    dispatch_once(&once, ^{
        NSString *beside = [[nvmtl_aircache_dir() stringByDeletingLastPathComponent] stringByAppendingPathComponent:@"spvcache"];
        NSString *tmp = [NSTemporaryDirectory() stringByAppendingPathComponent:@"nvmtl/spvcache"];
        // WindowServer's sandbox allows writes only inside its own named folders (…/C/WindowServer, …/T/WindowServer):
        // both folders above refused it, so it translated every CoreAnimation shader again on each boot (N-89, Tahoe).
        NSString *own = nil;
        { char ucd[PATH_MAX]; const size_t n = confstr(_CS_DARWIN_USER_CACHE_DIR, ucd, sizeof ucd);
          if (n > 0 && n <= sizeof ucd) own = [[@(ucd) stringByAppendingPathComponent:@(getprogname())] stringByAppendingPathComponent:@"nvmtl/spvcache"]; }
        if (nvmtl_dir_writable(beside)) { dir = beside; nvlog("spvcache: caching in %s", beside.UTF8String); }
        else if (nvmtl_dir_writable(own)) { dir = own; nvlog("spvcache: %s is not writable here - caching in %s", beside.UTF8String, own.UTF8String); }
        else if (nvmtl_dir_writable(tmp)) { dir = tmp; nvlog("spvcache: %s is not writable here - caching in %s", beside.UTF8String, tmp.UTF8String); }
        else nvlog("spvcache: no writable folder in this process (%s, %s) - translated shaders are not cached", beside.UTF8String, tmp.UTF8String);
    });
    return dir;
}
static NSString *nvmtl_spvcache_path_digest(const unsigned char h[CC_SHA256_DIGEST_LENGTH], NSString *stage) {
    // No tag, no file: an entry named without its modes could be served to a process in another mode.
    if (!strcmp(gXlateTag, "untagged")) return nil;
    NSString *d = nvmtl_spvcache_dir();
    if (!d) return nil;
    // Sweep the folder clear of other builds' entries once, here rather than where the folder is chosen: a process
    // that picks its folder before the translator is open does not know the build yet and would skip the sweep for
    // good. The window server does exactly that, so its cache kept seven builds' entries. By the time a path is
    // being built the build tag is real, because the name below is made from it.
    static dispatch_once_t sweepOnce;
    if (!getenv("NVMTL_SPVCACHE_KEEP_STALE")) dispatch_once(&sweepOnce, ^{
        NSString *chosen = d;
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
            char dirc[1024];
            if ([chosen getFileSystemRepresentation:dirc maxLength:sizeof dirc]) nvmtl_spvcache_drop_stale(dirc);
        });
    });
    char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
    for (int i = 0; i < CC_SHA256_DIGEST_LENGTH; i++) snprintf(hex + 2 * i, 3, "%02x", h[i]);
    nvmtl_spvcache_trim_async(d);
    return [d stringByAppendingPathComponent:[NSString stringWithFormat:@"%s.%@.%s.c3.spv", hex, stage, gXlateTag]];
}
static NSString *nvmtl_spvcache_path(NSData *air, NSString *stage) {
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(air.bytes, (CC_LONG)air.length, h);
    return nvmtl_spvcache_path_digest(h, stage);
}

/* The cache key of an AIR module with a short tail (function constants, sampler states ...) is SHA-256(size || air || tail).
 * The module is megabytes of text and the tail a few hundred bytes. VideoToolbox's Metal scaler makes its function again
 * on every transfer, so the same module was converted to UTF-8 and hashed three times per dispatch (function, reflection,
 * variant). Hash the module once, keep the hash state on the NSString that holds it, and finish each key from that state:
 * the digests, and so the file names, are the ones the one-shot hash gave. */
@interface NVMTLAirDigest : NSObject { @public CC_SHA256_CTX _ctx; }
@end
@implementation NVMTLAirDigest
@end
static const void *NVMTL_AIRDIGEST_KEY = &NVMTL_AIRDIGEST_KEY;
static const void *NVMTL_AIRTEXT_KEY = &NVMTL_AIRTEXT_KEY;
// Marks text nvmtl_air_to_text made; it is never edited, so state can be kept on it.
static const void *NVMTL_AIRTG_KEY = &NVMTL_AIRTG_KEY;
static NSString *nvmtl_air_text_made(NSString *s) {
    if (s.length >= 4096) objc_setAssociatedObject(s, NVMTL_AIRTEXT_KEY, @YES, OBJC_ASSOCIATION_RETAIN);
    return s;
}
// newFunctionWithName scanned the whole module twice on every call for these two markers (megabytes of text, each
// time a VideoToolbox transfer made its function). The answer is kept on the text once.
static const void *NVMTL_AIRFLAGS_KEY = &NVMTL_AIRFLAGS_KEY;
uint32_t nvmtl_air_text_flags(NSString *ll) {
    if (!ll) return 0;
    const BOOL keep = ll.length >= 4096 && objc_getAssociatedObject(ll, NVMTL_AIRTEXT_KEY) != nil;
    NSNumber *have = keep ? objc_getAssociatedObject(ll, NVMTL_AIRFLAGS_KEY) : nil;
    if (have) return have.unsignedIntValue;
    uint32_t f = NVMTL_AIRF_KNOWN;
    if ([ll rangeOfString:@"@air.dyld_lib_table"].location != NSNotFound) f |= NVMTL_AIRF_DYLD_TABLE;
    if ([ll rangeOfString:@"!\"air.intersection_function_table\""].location != NSNotFound) f |= NVMTL_AIRF_ISECT_TABLE;
    if (keep) objc_setAssociatedObject(ll, NVMTL_AIRFLAGS_KEY, @(f), OBJC_ASSOCIATION_RETAIN);
    return f;
}
static NVMTLAirDigest *nvmtl_air_state(NSString *ll) {
    if (!ll) return nil;
    // Only long strings we marked: every CF-backed string is a kind of NSMutableString, so a class test keeps nothing.
    const BOOL cacheable = ll.length >= 4096 && objc_getAssociatedObject(ll, NVMTL_AIRTEXT_KEY) != nil;
    NVMTLAirDigest *d = cacheable ? objc_getAssociatedObject(ll, NVMTL_AIRDIGEST_KEY) : nil;
    if (d) return d;
    NSData *air = [ll dataUsingEncoding:NSUTF8StringEncoding];
    if (!air) return nil;
    d = [NVMTLAirDigest new];
    uint64_t size = air.length;
    CC_SHA256_Init(&d->_ctx);
    CC_SHA256_Update(&d->_ctx, &size, sizeof size);
    for (NSUInteger off = 0; off < air.length; off += 1u << 30)
        CC_SHA256_Update(&d->_ctx, (const char *)air.bytes + off, (CC_LONG)MIN((NSUInteger)1u << 30, air.length - off));
    if (cacheable) objc_setAssociatedObject(ll, NVMTL_AIRDIGEST_KEY, d, OBJC_ASSOCIATION_RETAIN);
    return d;
}
static NSString *nvmtl_spvcache_path_air(NVMTLAirDigest *ad, NSData *tail, NSString *stage) {
    CC_SHA256_CTX c = ad->_ctx;
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Update(&c, tail.bytes, (CC_LONG)tail.length);
    CC_SHA256_Final(h, &c);
    return nvmtl_spvcache_path_digest(h, stage);
}
// The memo's name for an entry when this process has no cache folder (a sandbox that allows none): the same digest
// without a directory, so a process that cannot store files still keeps what it translated.
static NSString *nvmtl_spvcache_memo_key_air(NVMTLAirDigest *ad, NSData *tail, NSString *stage) {
    CC_SHA256_CTX c = ad->_ctx;
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Update(&c, tail.bytes, (CC_LONG)tail.length);
    CC_SHA256_Final(h, &c);
    char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
    for (int i = 0; i < CC_SHA256_DIGEST_LENGTH; i++) snprintf(hex + 2 * i, 3, "%02x", h[i]);
    return [NSString stringWithFormat:@"memo:%s.%@.%s", hex, stage, gXlateTag];
}
/* Cache-file contents kept in memory, by file name. The name is the content digest plus the translator tag, so an entry is the
 * answer for that exact input and translator; it is dropped by NSCache under memory pressure. */
static BOOL nvmtl_is_spirv(NSData *d) {
    uint32_t magic = 0;
    if (d.length < 20 || d.length % 4) return NO;
    memcpy(&magic, d.bytes, 4);
    return magic == 0x07230203;
}
static NSCache *nvmtl_memo(void) {
    static NSCache *c; static dispatch_once_t once;
    dispatch_once(&once, ^{ c = [NSCache new]; c.totalCostLimit = 96u << 20; c.countLimit = 256; });
    return c;
}

static BOOL nvmtl_air_unroll_wanted(NSData *air);
static NSString *nvmtl_aircache_path(NSData *air) {
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256(air.bytes, (CC_LONG)air.length, h);
    char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
    for (int i = 0; i < CC_SHA256_DIGEST_LENGTH; i++) snprintf(hex + 2 * i, 3, "%02x", h[i]);
    return [[nvmtl_aircache_dir() stringByAppendingPathComponent:@(hex)] stringByAppendingPathExtension:
            getenv("NVMTL_CANONICAL_POINTERS") ? (nvmtl_air_unroll_wanted(air) ? @"p5.u2.ll" : @"p5.ll")
                                               : (nvmtl_air_unroll_wanted(air) ? @"u2.ll" : @"ll")];
}

static BOOL nvmtl_run_air_opt(const char *input, const char *output) {
    posix_spawn_file_actions_t actions;
    int rc = posix_spawn_file_actions_init(&actions);
    if (rc) return NO;
    rc = posix_spawn_file_actions_addopen(&actions, STDOUT_FILENO, "/dev/null", O_WRONLY, 0);
    if (!rc) rc = posix_spawn_file_actions_addopen(&actions, STDERR_FILENO, "/dev/null", O_WRONLY, 0);
    pid_t child = -1;
    const char *tool = nvmtl_air_dis_path();
    if (!tool) { posix_spawn_file_actions_destroy(&actions); nvlog("library: nvmtl-air-dis not found beside the plugin"); return NO; }
    char *argv[] = { (char *)tool, "-S", (char *)input, "-o", (char *)output, NULL };
    // an empty environment: the tool needs none, and the caller's DYLD_ and other variables are not handed on
    char *noenv[] = { NULL };
    if (!rc) rc = posix_spawn(&child, tool, &actions, NULL, argv, noenv);
    posix_spawn_file_actions_destroy(&actions);
    if (rc) { nvlog("library: nvmtl-air-dis spawn failed: %s", strerror(rc)); return NO; }
    uint64_t deadline = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) + 20ull * 1000000000ull;
    int status = 0;
    for (;;) {
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) {
            if (WIFEXITED(status) && WEXITSTATUS(status) == 0) return YES;
            nvlog("library: nvmtl-air-dis failed (wait status %d)", status); return NO;
        }
        if (result < 0 && errno != EINTR) {
            nvlog("library: nvmtl-air-dis wait failed: %s", strerror(errno)); return NO;
        }
        if (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) >= deadline) {
            kill(child, SIGKILL);
            while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
            nvlog("library: nvmtl-air-dis exceeded 20-second limit"); return NO;
        }
        usleep(10000);
    }
}

static void *(*nvLLVMContextCreate)(void);
static void (*nvLLVMContextDispose)(void *);
static void (*nvLLVMContextSetDiagnosticHandler)(void *, void (*)(void *, void *), void *);
static void *(*nvLLVMCreateMemoryBufferWithMemoryRangeCopy)(const char *, size_t, const char *);
static void (*nvLLVMDisposeMemoryBuffer)(void *);
static int (*nvLLVMParseBitcodeInContext2)(void *, void *, void **);
static char *(*nvLLVMPrintModuleToString)(void *);
static void (*nvLLVMDisposeModule)(void *);
static void (*nvLLVMDisposeMessage)(char *);
static char *(*nvLLVMGetDiagInfoDescription)(void *);
static void *(*nvLLVMRunPasses)(void *, const char *, void *, void *);
static void *(*nvLLVMCreatePassBuilderOptions)(void);
static void (*nvLLVMDisposePassBuilderOptions)(void *);
static char *(*nvLLVMGetErrorMessage)(void *);
static void (*nvLLVMDisposeErrorMessage)(char *);
struct nvmtl_llvm_walk {
    void *(*GetFirstGlobal)(void *); void *(*GetNextGlobal)(void *); void *(*GlobalGetValueType)(void *);
    void *(*GetFirstUse)(void *); void *(*GetNextUse)(void *); void *(*GetUser)(void *);
    void *(*IsAConstantExpr)(void *); int (*GetConstOpcode)(void *); void *(*GetGEPSourceElementType)(void *);
    int (*GetNumOperands)(void *); void *(*GetOperand)(void *, unsigned); void *(*IsAConstantInt)(void *);
    long long (*ConstIntGetSExtValue)(void *); int (*GetTypeKind)(void *); unsigned (*GetIntTypeWidth)(void *);
    void *(*GetElementType)(void *); unsigned long long (*GetArrayLength2)(void *); void *(*GetModuleContext)(void *);
    void *(*Int64TypeInContext)(void *); void *(*ConstInt)(void *, unsigned long long, int);
    void *(*ConstInBoundsGEP2)(void *, void *, void **, unsigned); void (*ReplaceAllUsesWith)(void *, void *);
    void *(*GetFirstFunction)(void *); void *(*GetNextFunction)(void *); int (*GetLinkage)(void *);
    void *(*IsACallInst)(void *); void *(*GetCalledValue)(void *);
    unsigned (*GetEnumAttributeKindForName)(const char *, size_t); void *(*CreateEnumAttribute)(void *, unsigned, uint64_t);
    void (*AddAttributeAtIndex)(void *, unsigned, void *); void *(*GetEnumAttributeAtIndex)(void *, unsigned, unsigned);
    void *(*GetInstructionParent)(void *); void *(*GetBasicBlockParent)(void *);
    void *(*GetFirstBasicBlock)(void *); void *(*GetNextBasicBlock)(void *);
    void *(*GetFirstInstruction)(void *); void *(*GetNextInstruction)(void *);
    void *(*IsAPHINode)(void *); void *(*IsASelectInst)(void *); void *(*TypeOf)(void *);
};
typedef struct nvmtl_llvm_walk nvmtl_llvm_walk;
#define NVMTL_LLVM_WALK_NAMES "LLVMGetFirstGlobal", "LLVMGetNextGlobal", "LLVMGlobalGetValueType", "LLVMGetFirstUse", \
    "LLVMGetNextUse", "LLVMGetUser", "LLVMIsAConstantExpr", "LLVMGetConstOpcode", "LLVMGetGEPSourceElementType", \
    "LLVMGetNumOperands", "LLVMGetOperand", "LLVMIsAConstantInt", "LLVMConstIntGetSExtValue", "LLVMGetTypeKind", \
    "LLVMGetIntTypeWidth", "LLVMGetElementType", "LLVMGetArrayLength2", "LLVMGetModuleContext", "LLVMInt64TypeInContext", \
    "LLVMConstInt", "LLVMConstInBoundsGEP2", "LLVMReplaceAllUsesWith", \
    "LLVMGetFirstFunction", "LLVMGetNextFunction", "LLVMGetLinkage", "LLVMIsACallInst", "LLVMGetCalledValue", \
    "LLVMGetEnumAttributeKindForName", "LLVMCreateEnumAttribute", "LLVMAddAttributeAtIndex", "LLVMGetEnumAttributeAtIndex", \
    "LLVMGetInstructionParent", "LLVMGetBasicBlockParent", \
    "LLVMGetFirstBasicBlock", "LLVMGetNextBasicBlock", "LLVMGetFirstInstruction", "LLVMGetNextInstruction", \
    "LLVMIsAPHINode", "LLVMIsASelectInst", "LLVMTypeOf"
static int nvmtl_scalar_bytes(const nvmtl_llvm_walk *w, void *t) {
    switch (w->GetTypeKind(t)) {
    case 1: case 18: return 2; case 2: return 4; case 3: return 8;
    case 8: { unsigned b = w->GetIntTypeWidth(t); return (b == 8 || b == 16 || b == 32 || b == 64) ? (int)(b / 8) : 0; }
    default: return 0;
    }
}
static int nvmtl_retype_byte_geps(const nvmtl_llvm_walk *w, void *module) {
    void *i64 = w->Int64TypeInContext(w->GetModuleContext(module)); int n = 0;
    for (void *g = w->GetFirstGlobal(module); g; g = w->GetNextGlobal(g)) {
        void *vt = w->GlobalGetValueType(g);
        if (w->GetTypeKind(vt) != 11) continue;
        void *et = w->GetElementType(vt); int sz = nvmtl_scalar_bytes(w, et);
        if (sz <= 1) continue;
        unsigned long long len = w->GetArrayLength2(vt);
        void *hits[256]; void *repl[256]; int nh = 0;
        for (void *u = w->GetFirstUse(g); u && nh < 256; u = w->GetNextUse(u)) {
            void *ce = w->GetUser(u);
            if (!w->IsAConstantExpr(ce) || w->GetConstOpcode(ce) != 29 ) continue;
            if (w->GetNumOperands(ce) != 2 || w->GetOperand(ce, 0) != g) continue;
            void *src = w->GetGEPSourceElementType(ce);
            if (w->GetTypeKind(src) != 8 || w->GetIntTypeWidth(src) != 8) continue;
            void *ix = w->GetOperand(ce, 1); if (!w->IsAConstantInt(ix)) continue;
            long long b = w->ConstIntGetSExtValue(ix);
            if (b < 0 || b % sz || (unsigned long long)(b / sz) > len) continue;
            void *idx[2] = { w->ConstInt(i64, 0, 0), w->ConstInt(i64, (unsigned long long)(b / sz), 0) };
            hits[nh] = ce; repl[nh] = w->ConstInBoundsGEP2(vt, g, idx, 2); nh++;
        }
        for (int k = 0; k < nh; k++) if (repl[k] && repl[k] != hits[k]) { w->ReplaceAllUsesWith(hits[k], repl[k]); n++; }
    }
    return n;
}
static int nvmtl_llvm_walk_load(nvmtl_llvm_walk *w, void *library) {
    static const char *names[] = { NVMTL_LLVM_WALK_NAMES }; void **slot = (void **)w;
    for (unsigned k = 0; k < sizeof names / sizeof *names; k++) if (!(slot[k] = dlsym(library, names[k]))) return 0;
    return 1;
}
static nvmtl_llvm_walk gLLVMWalk;

static BOOL nvmtl_has_pointer_choice(const nvmtl_llvm_walk *w, void *module) {
    for (void *fn = w->GetFirstFunction(module); fn; fn = w->GetNextFunction(fn))
        for (void *bb = w->GetFirstBasicBlock(fn); bb; bb = w->GetNextBasicBlock(bb))
            for (void *i = w->GetFirstInstruction(bb); i; i = w->GetNextInstruction(i))
                if ((w->IsAPHINode(i) || w->IsASelectInst(i)) && w->GetTypeKind(w->TypeOf(i)) == 12) return YES;
    return NO;
}

// Marks every internal helper with exactly one direct caller always-inline, so the pointer it returns or takes is
// visible as SSA to the translator instead of crossing a call. One caller means no helper body is duplicated.
static unsigned nvmtl_inline_single_use_helpers(const nvmtl_llvm_walk *w, void *module) {
    unsigned inl = w->GetEnumAttributeKindForName("alwaysinline", strlen("alwaysinline"));
    unsigned no = w->GetEnumAttributeKindForName("noinline", strlen("noinline"));
    if (!inl || !no) return 0;
    void *context = w->GetModuleContext(module); unsigned count = 0;
    for (void *fn = w->GetFirstFunction(module); fn; fn = w->GetNextFunction(fn)) {
        int linkage = w->GetLinkage(fn);
        if (linkage != 8 && linkage != 9) continue; // LLVMInternalLinkage and LLVMPrivateLinkage
        void *use = w->GetFirstUse(fn);
        if (!use || w->GetNextUse(use)) continue;
        void *call = w->GetUser(use);
        if (!w->IsACallInst(call) || w->GetCalledValue(call) != fn) continue;
        if (w->GetBasicBlockParent(w->GetInstructionParent(call)) == fn) continue;
        if (w->GetEnumAttributeAtIndex(fn, ~0u, no)) continue;
        w->AddAttributeAtIndex(fn, ~0u, w->CreateEnumAttribute(context, inl, 0)); count++;
    }
    return count;
}

// Set only when the reader reports an ERROR about a module - it used to only log, so a module the reader
// had rejected was still compiled. Severity matters: the same handler carries warnings, remarks and notes,
// and treating those as fatal would refuse shaders that compile perfectly well today. The severity symbol
// is loaded optionally, so a libLLVM without it keeps the old behaviour rather than disabling the reader.
static int (*nvLLVMGetDiagInfoSeverity)(void *);
static _Thread_local int gAirError;
static void nvmtl_llvm_diagnostic(void *diagnostic, void *context) {
    (void)context;
    if (nvLLVMGetDiagInfoSeverity && nvLLVMGetDiagInfoSeverity(diagnostic) == 0) gAirError = 1;   // LLVMDSError
    char *message = nvLLVMGetDiagInfoDescription(diagnostic);
    if (message) { nvlog("library: bitcode reader: %s", message); nvLLVMDisposeMessage(message); }
}
static BOOL nvmtl_bitcode_reader_ready(void) {
    static dispatch_once_t once; static BOOL ready;
    dispatch_once(&once, ^{
        Dl_info info;
        if (!dladdr((const void *)&nvmtl_bitcode_reader_ready, &info) || !info.dli_fname) return;
        NSString *path = [[@(info.dli_fname) stringByDeletingLastPathComponent]
                         stringByAppendingPathComponent:@"air-runtime/libLLVM.dylib"];
        void *library = dlopen(path.fileSystemRepresentation, RTLD_NOW | RTLD_LOCAL);
        if (!library) { nvlog("library: in-memory bitcode reader unavailable: %s", dlerror()); return; }
#define NV_LLVM_LOAD(name) do { nv##name = (void *)dlsym(library, #name); if (!nv##name) { nvlog("library: bitcode reader missing " #name); return; } } while (0)
        NV_LLVM_LOAD(LLVMContextCreate);
        NV_LLVM_LOAD(LLVMContextDispose);
        NV_LLVM_LOAD(LLVMContextSetDiagnosticHandler);
        NV_LLVM_LOAD(LLVMCreateMemoryBufferWithMemoryRangeCopy);
        NV_LLVM_LOAD(LLVMDisposeMemoryBuffer);
        NV_LLVM_LOAD(LLVMParseBitcodeInContext2);
        NV_LLVM_LOAD(LLVMPrintModuleToString);
        NV_LLVM_LOAD(LLVMDisposeModule);
        NV_LLVM_LOAD(LLVMDisposeMessage);
        NV_LLVM_LOAD(LLVMGetDiagInfoDescription);
        // optional: absent in some builds, and its absence must not disable the reader
        nvLLVMGetDiagInfoSeverity = (int (*)(void *))dlsym(library, "LLVMGetDiagInfoSeverity");
        if (!nvLLVMGetDiagInfoSeverity) nvlog("library: bitcode reader has no LLVMGetDiagInfoSeverity - reader errors are logged only");
#undef NV_LLVM_LOAD
        nvLLVMRunPasses = dlsym(library, "LLVMRunPasses"); nvLLVMCreatePassBuilderOptions = dlsym(library, "LLVMCreatePassBuilderOptions");
        nvLLVMDisposePassBuilderOptions = dlsym(library, "LLVMDisposePassBuilderOptions");
        nvLLVMGetErrorMessage = dlsym(library, "LLVMGetErrorMessage"); nvLLVMDisposeErrorMessage = dlsym(library, "LLVMDisposeErrorMessage");
        if (!nvLLVMRunPasses || !nvLLVMCreatePassBuilderOptions || !nvLLVMDisposePassBuilderOptions || !nvLLVMGetErrorMessage || !nvLLVMDisposeErrorMessage) {
            nvLLVMRunPasses = NULL; nvlog("library: air unroll unavailable - libLLVM lacks the pass-builder API"); }
        if (nvLLVMRunPasses && !nvmtl_llvm_walk_load(&gLLVMWalk, library)) {
            nvLLVMRunPasses = NULL; nvlog("library: air unroll unavailable - libLLVM lacks the IR-walk API the byte-GEP retype needs"); }
        ready = YES;
        nvlog("library: in-memory bitcode reader ready");
    });
    return ready;
}
#define NVMTL_AIR_UNROLL_PIPELINE "function(sroa,loop(loop-rotate),indvars,loop-unroll<O3>,sroa,simplifycfg,early-cse)"
static BOOL nvmtl_air_unroll_wanted(NSData *air) {
    static int off = -1; if (off < 0) off = getenv("NVMTL_NO_AIR_UNROLL") != NULL;
    static const char needle[] = "air.simdgroup_matrix_8x8";
    return !off && air.length && memmem(air.bytes, air.length, needle, sizeof needle - 1) != NULL;
}
// The largest shader library measured in a real app is a few megabytes. The reader we hand this to is
// LLVM's, which its own documentation declines to call hardened against malicious input, so cap the blob
// before it gets there: a library over the cap falls through to the out-of-process path below, where a
// crash costs a failed translation instead of the app.
#define NVMTL_AIR_MAX_BYTES (64u * 1024u * 1024u)
static NSString *nvmtl_air_in_memory(NSData *air) {
    if (!nvmtl_bitcode_reader_ready()) return nil;
    if (air.length < 4 || air.length > NVMTL_AIR_MAX_BYTES) {
        nvlog("library: bitcode blob is %lu bytes - outside [4, %u], not read in process",
              (unsigned long)air.length, NVMTL_AIR_MAX_BYTES);
        return nil;
    }
    void *context = nvLLVMContextCreate();
    if (!context) return nil;
    nvLLVMContextSetDiagnosticHandler(context, nvmtl_llvm_diagnostic, NULL);
    void *buffer = nvLLVMCreateMemoryBufferWithMemoryRangeCopy(air.bytes, air.length, "nvmtl-air");
    void *module = NULL; NSString *result = nil;
    gAirError = 0;
    if (buffer && !nvLLVMParseBitcodeInContext2(context, buffer, &module) && module && !gAirError) {
        if (nvLLVMRunPasses && nvmtl_air_unroll_wanted(air)) {
            void *opts = nvLLVMCreatePassBuilderOptions();
            void *err = nvLLVMRunPasses(module, NVMTL_AIR_UNROLL_PIPELINE, NULL, opts);
            nvLLVMDisposePassBuilderOptions(opts);
            if (err) { char *m = nvLLVMGetErrorMessage(err); nvlog("library: air unroll FAILED (%s) - translating as decoded", m ? m : "?"); if (m) nvLLVMDisposeErrorMessage(m); }
            else { int rt = nvmtl_retype_byte_geps(&gLLVMWalk, module);
                   nvlog("library: air unroll: simdgroup_matrix module (%lu bytes) unrolled + sroa, %d byte GEPs retyped", (unsigned long)air.length, rt); }
        }
        // NVMTL_CANONICAL_POINTERS: inline single-use helpers and SROA so pointers reach the translator as plain SSA;
        // instcombine only when a pointer phi/select exists (it also rewrites typed GEPs to byte GEPs).
        if (getenv("NVMTL_CANONICAL_POINTERS")) {
            if (!nvLLVMRunPasses) { gAirError = 1; nvlog("library: pointer canonicalization requires the LLVM pass-builder API"); }
            else {
                unsigned helpers = nvmtl_inline_single_use_helpers(&gLLVMWalk, module);
                void *opts = nvLLVMCreatePassBuilderOptions();
                void *err = nvLLVMRunPasses(module, "always-inline,function(sroa,simplifycfg)", NULL, opts);
                nvlog("library: pointer canonicalization exposed %u single-use helpers", helpers);
                nvLLVMDisposePassBuilderOptions(opts);
                if (!err && nvmtl_has_pointer_choice(&gLLVMWalk, module)) {
                    opts = nvLLVMCreatePassBuilderOptions();
                    err = nvLLVMRunPasses(module, "function(instcombine,simplifycfg)", NULL, opts);
                    nvLLVMDisposePassBuilderOptions(opts);
                    nvlog("library: pointer selection simplification applied");
                }
                if (err) {
                    gAirError = 1;
                    char *m = nvLLVMGetErrorMessage(err);
                    nvlog("library: pointer canonicalization refused: %s", m ? m : "unknown LLVM error");
                    if (m) nvLLVMDisposeErrorMessage(m);
                }
            }
        }
        char *text = gAirError ? NULL : nvLLVMPrintModuleToString(module);
        if (text) {
            result = [NSString stringWithUTF8String:text]; nvLLVMDisposeMessage(text);
            // samesign only restricts defined inputs; the translator reads the plain integer predicate.
            if (getenv("NVMTL_CANONICAL_POINTERS"))
                result = [result stringByReplacingOccurrencesOfString:@"icmp samesign " withString:@"icmp "];
        }
    }
    if (module) nvLLVMDisposeModule(module);
    if (buffer) nvLLVMDisposeMemoryBuffer(buffer);
    nvLLVMContextDispose(context);
    return result;
}

static NSString *nvmtl_air_to_text(NSData *air) {
    NSString *cache = nvmtl_aircache_path(air);
    NSString *hit = [NSString stringWithContentsOfFile:cache encoding:NSUTF8StringEncoding error:nil];
    if (hit.length) { gAirHit++; return nvmtl_air_text_made(hit); }
    gAirMiss++;
    NSString *memoryIR = nvmtl_air_in_memory(air);
    if (memoryIR.length) {
        NSString *text = [memoryIR copy], *file = [cache copy];
        nvmtl_cache_io(^{ if ([text writeToFile:file atomically:YES encoding:NSUTF8StringEncoding error:nil]) gAirStored++; });
        return nvmtl_air_text_made(memoryIR);
    }

    if (getenv("NVMTL_CANONICAL_POINTERS")) {
        nvlog("library: canonical AIR unavailable; decoder fallback cannot satisfy this contract");
        return nil;
    }
    NSString *dir = NSTemporaryDirectory();
    NSString *inp = [dir stringByAppendingPathComponent:[NSString stringWithFormat:@"nvmtl-%d-%p.air", getpid(), air]];
    NSString *outp = [inp stringByAppendingPathExtension:@"ll"];
    if (![air writeToFile:inp atomically:NO]) { nvlog("library: cannot write %s", inp.UTF8String); return nil; }
    if (!nvmtl_run_air_opt(inp.fileSystemRepresentation, outp.fileSystemRepresentation)) {
        unlink(inp.fileSystemRepresentation); unlink(outp.fileSystemRepresentation); return nil;
    }
    NSString *ll = [NSString stringWithContentsOfFile:outp encoding:NSUTF8StringEncoding error:nil];
    unlink(inp.UTF8String); unlink(outp.UTF8String);
    if (ll.length) {
        NSError *mkerr = nil;
        [[NSFileManager defaultManager] createDirectoryAtPath:nvmtl_aircache_dir()
                                  withIntermediateDirectories:YES attributes:nil error:&mkerr];
        NSString *text = [ll copy], *file = [cache copy];
        nvmtl_cache_io(^{ if ([text writeToFile:file atomically:YES encoding:NSUTF8StringEncoding error:nil]) gAirStored++; });
    }
    return nvmtl_air_text_made(ll);
}

static BOOL nvmtl_entry_of(NSString *ll, NSString **name, NSString **stage) {
    for (NSString *s in @[ @"vertex", @"fragment", @"kernel", @"mesh", @"object" ]) {
        NSString *key = [NSString stringWithFormat:@"!air.%@ = !{!", s];
        NSRange k = [ll rangeOfString:key];
        if (k.location == NSNotFound) continue;
        NSRange rest = NSMakeRange(NSMaxRange(k), ll.length - NSMaxRange(k));
        NSRange end = [ll rangeOfString:@"}" options:0 range:rest];
        if (end.location == NSNotFound) continue;
        NSString *node = [ll substringWithRange:NSMakeRange(rest.location, end.location - rest.location)];
        NSRange nc = [node rangeOfString:@","];
        if (nc.location != NSNotFound) node = [node substringToIndex:nc.location];
        NSString *decl = [NSString stringWithFormat:@"\n!%@ = !{", node];
        NSRange d = [ll rangeOfString:decl];
        if (d.location == NSNotFound) continue;
        NSRange nl = [ll rangeOfString:@"\n" options:0 range:NSMakeRange(NSMaxRange(d), ll.length - NSMaxRange(d))];
        const NSUInteger stop = (nl.location == NSNotFound) ? ll.length : nl.location;
        NSRange dr = NSMakeRange(NSMaxRange(d), stop - NSMaxRange(d));
        NSRange at = [ll rangeOfString:@"@" options:0 range:dr];
        if (at.location == NSNotFound) continue;
        NSRange after = NSMakeRange(NSMaxRange(at), stop - NSMaxRange(at));
        NSRange comma = [ll rangeOfString:@"," options:0 range:after];
        NSRange brace = [ll rangeOfString:@"}" options:0 range:after];
        NSUInteger nameEnd = stop;
        if (comma.location != NSNotFound) nameEnd = MIN(nameEnd, comma.location);
        if (brace.location != NSNotFound) nameEnd = MIN(nameEnd, brace.location);
        if (nameEnd <= after.location) continue;
        *name = [ll substringWithRange:NSMakeRange(after.location, nameEnd - after.location)];
        if ((*name).length >= 2 && [*name hasPrefix:@"\""] && [*name hasSuffix:@"\""])
            *name = [*name substringWithRange:NSMakeRange(1, (*name).length - 2)];
        *stage = s;
        return YES;
    }
    return NO;
}

static NSArray<NSString *> *nvmtl_visible_of(NSString *ll)
{
    NSMutableDictionary<NSString *, NSString *> *node = [NSMutableDictionary new];
    NSMutableSet<NSString *> *defined = [NSMutableSet new];
    NSMutableArray<NSString *> *ids = [NSMutableArray new];
    [ll enumerateLinesUsingBlock:^(NSString *line, BOOL *stop) {
        if ([line hasPrefix:@"define"]) {
            NSRange at = [line rangeOfString:@"@"];
            if (at.location == NSNotFound) return;
            NSUInteger s = at.location + 1, e;
            BOOL q = (s < line.length && [line characterAtIndex:s] == '"');
            if (q) s++;
            for (e = s; e < line.length; e++) { unichar c = [line characterAtIndex:e];
                if (q ? (c == '"') : (c == '(')) break; }
            if (e > s) [defined addObject:[line substringWithRange:NSMakeRange(s, e - s)]];
            return;
        }
        if (![line hasPrefix:@"!"]) return;
        if ([line hasPrefix:@"!air.visible = "] || [line hasPrefix:@"!air.ci = "] || [line hasPrefix:@"!air.intersection = "]) {
            NSRange eq = [line rangeOfString:@"="];
            for (NSString *tok in [[line substringFromIndex:eq.location] componentsSeparatedByString:@"!"]) {
                NSUInteger d = 0;
                while (d < tok.length) { unichar c = [tok characterAtIndex:d]; if (c < '0' || c > '9') break; d++; }
                if (d) [ids addObject:[tok substringToIndex:d]];
            }
            return;
        }
        NSRange eq = [line rangeOfString:@" = !{"];
        if (eq.location == NSNotFound || eq.location < 2) return;
        NSString *idn = [line substringWithRange:NSMakeRange(1, eq.location - 1)];
        NSRange at = [line rangeOfString:@"@" options:0 range:NSMakeRange(eq.location, line.length - eq.location)];
        if (at.location == NSNotFound) return;
        NSUInteger s = at.location + 1, e;
        BOOL q = (s < line.length && [line characterAtIndex:s] == '"');
        if (q) s++;
        for (e = s; e < line.length; e++) { unichar c = [line characterAtIndex:e];
            if (q ? (c == '"') : (c == ',' || c == ')' || c == '}' || c == ' ')) break; }
        if (e > s) node[idn] = [line substringWithRange:NSMakeRange(s, e - s)];
    }];
    NSMutableArray<NSString *> *out = [NSMutableArray new];
    for (NSString *i in ids) { NSString *nm = node[i]; if (nm && [defined containsObject:nm]) [out addObject:nm]; }
    return out;
}

static NSString *nvmtl_sym_at(NSString *line)
{
    NSRange at = [line rangeOfString:@"@"];
    if (at.location == NSNotFound) return nil;
    NSUInteger i = at.location + 1, n = line.length;
    if (i < n && [line characterAtIndex:i] == '"') {
        NSRange q = [line rangeOfString:@"\"" options:0 range:NSMakeRange(i + 1, n - i - 1)];
        if (q.location == NSNotFound) return nil;
        return [line substringWithRange:NSMakeRange(i + 1, q.location - i - 1)];
    }
    NSUInteger j = i;
    while (j < n) {
        unichar c = [line characterAtIndex:j];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') ||
              c == '_' || c == '.' || c == '$')) break;
        j++;
    }
    return j > i ? [line substringWithRange:NSMakeRange(i, j - i)] : nil;
}

static NSArray<NSString *> *nvmtl_exported_defs(NSString *ll)
{
    NSMutableArray<NSString *> *out = [NSMutableArray new];
    [ll enumerateLinesUsingBlock:^(NSString *line, BOOL *stop) {
        if (![line hasPrefix:@"define "]) return;
        NSRange at = [line rangeOfString:@"@"];
        if (at.location == NSNotFound) return;
        NSString *head = [line substringToIndex:at.location];
        for (NSString *k in @[@" internal ", @" private ", @" hidden ", @" available_externally "])
            if ([head rangeOfString:k].location != NSNotFound) return;
        NSString *s = nvmtl_sym_at(line);
        if (s.length && ![s hasPrefix:@"air."] && ![s hasPrefix:@"llvm."]) [out addObject:s];
    }];
    return out;
}

NSArray<NSString *> *nvmtl_externs_uncached(NSDictionary<NSString *, NSString *> *airs)
{
    if (!airs.count) return @[];
    NSMutableSet<NSString *> *declared = [NSMutableSet new], *defined = [NSMutableSet new];
    NSMutableSet *seen = [NSMutableSet new];
    for (NSString *fn in airs) {
        NSString *ll = airs[fn];
        if (!ll) continue;
        NSValue *key = [NSValue valueWithPointer:(__bridge const void *)ll];
        if ([seen containsObject:key]) continue;
        [seen addObject:key];
        [ll enumerateLinesUsingBlock:^(NSString *line, BOOL *stop) {
            BOOL isDecl = [line hasPrefix:@"declare"];
            if (!isDecl && ![line hasPrefix:@"define"]) return;
            NSString *sym = nvmtl_sym_at(line);
            if (!sym.length || [sym hasPrefix:@"air."] || [sym hasPrefix:@"llvm."]) return;
            [(isDecl ? declared : defined) addObject:sym];
        }];
    }
    [declared minusSet:defined];
    return declared.allObjects;
}

NSSet<NSString *> *nvmtl_mtlb_extern_names_uncached(NSData *raw)
{
    NSMutableSet<NSString *> *out = [NSMutableSet new];
    const uint8_t *b = raw.bytes; NSUInteger n = raw.length;
    if (n < 0x28 || memcmp(b, "MTLB", 4) != 0) return out;
    uint64_t off = 0; memcpy(&off, b + 0x18, 8);
    if (off > n - 4) return out;   /* n >= 0x28 above; off + 4 would wrap for an offset near 2^64 */
    uint32_t count = 0; memcpy(&count, b + off, 4);
    NSUInteger pos = (NSUInteger)off + 4;
    for (uint32_t k = 0; k < count && pos + 4 <= n; k++) {
        uint32_t size = 0; memcpy(&size, b + pos, 4);
        if (size < 4 || pos + size > n) { nvlog("mtlb: function list entry %u runs past the container - stopping", k); break; }
        NSUInteger q = pos + 4, end = pos + size; NSString *name = nil; int type = -1;
        while (q + 4 <= end) {
            const uint8_t *tag = b + q; q += 4;
            if (!memcmp(tag, "ENDT", 4)) break;
            if (q + 2 > end) break;
            uint16_t len = 0; memcpy(&len, b + q, 2); q += 2;
            if (q + len > end) break;
            if (!memcmp(tag, "NAME", 4) && len) name = [[NSString alloc] initWithBytes:b + q length:strnlen((const char *)b + q, len) encoding:NSUTF8StringEncoding];
            else if (!memcmp(tag, "TYPE", 4) && len) type = b[q];
            q += len;
        }
        if (type == 5 && name.length) [out addObject:name];
        pos = end;
    }
    return out;
}
NSSet<NSString *> *nvmtl_mtlb_extern_names(NSData *raw) {
    if (!raw.length) return [NSSet set];
    static char key;
    @synchronized (raw) {
        NSSet *hit = objc_getAssociatedObject(raw, &key); if (hit) return hit;
        NSSet *r = [nvmtl_mtlb_extern_names_uncached(raw) copy];
        objc_setAssociatedObject(raw, &key, r, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        return r;
    }
}

static char gExternsKey;
NSArray<NSString *> *nvmtl_externs_of_airs(NSDictionary<NSString *, NSString *> *airs) {
    if(!airs.count)return @[];
    if([airs isKindOfClass:NSMutableDictionary.class])return nvmtl_externs_uncached(airs);
    @synchronized(airs) {
        NSArray *hit=objc_getAssociatedObject(airs,&gExternsKey);if(hit)return hit;
        NSArray *result=nvmtl_externs_uncached(airs);
        objc_setAssociatedObject(airs,&gExternsKey,result,OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        return result;
    }
}

// ---- cache files that prove they are whole (nvmtl_cache_file.h) ----
// Every translated-shader entry is written as payload+footer and read back only when the footer agrees, so a torn,
// truncated or older-format file is a miss (and is removed), never a shader. Nothing is persisted once the device is lost
// in this process: what was translated while the GPU was dying is not evidence of anything.
static _Atomic unsigned long gCacheRefused, gCacheSkippedLost;
static NSData *nvmtl_cache_read(NSString *path) {
    if (!path) return nil;
    size_t n = 0; nvmtl_cf_status st = NVMTL_CF_MISSING;
    uint8_t *b = nvmtl_cf_read(path.fileSystemRepresentation, &n, &st);
    if (!b) {
        if (st != NVMTL_CF_MISSING) {
            unsigned long k = atomic_fetch_add(&gCacheRefused, 1) + 1;
            if (!(k & (k - 1))) nvlog("shader cache: refused and removed %s: %s [%lu refused so far]", path.lastPathComponent.UTF8String, nvmtl_cf_why(st), k);
        }
        return nil;
    }
    return [NSData dataWithBytesNoCopy:b length:n freeWhenDone:YES];
}
// 0 on success, else the errno (or -1 when the device was lost and nothing was written)
static int nvmtl_cache_write(NSString *path, NSData *d) {
    if (!path || !d) return EINVAL;
    if (nvmtl_vk_lost()) {
        unsigned long k = atomic_fetch_add(&gCacheSkippedLost, 1) + 1;
        if (!(k & (k - 1))) nvlog("shader cache: not writing %s - the device is lost in this process [%lu skipped]", path.lastPathComponent.UTF8String, k);
        return -1;
    }
    return nvmtl_cf_write(path.fileSystemRepresentation, d.bytes, d.length);
}

// ---- one translation per module (nvmtl_pretranslate.h) ----
static void *nvmtl_flight_retain(void *p) { return (void *)CFBridgingRetain((__bridge id)p); }
static void nvmtl_flight_release(void *p) { CFRelease(p); }
static nvmtl_sf *nvmtl_translate_flight(void) {
    static nvmtl_sf sf; static dispatch_once_t once;
    dispatch_once(&once, ^{ nvmtl_sf_init(&sf, nvmtl_flight_retain, nvmtl_flight_release); });
    return &sf;
}
// A module the translator refused is refused again with the same message, so neither the application nor the background
// pass pays for the same failure twice.
static NSMutableDictionary<NSString *, NSString *> *gTranslateFailures;
static NSString *nvmtl_translate_failure_for(NSString *key) {
    @synchronized([NSProcessInfo processInfo]) { return gTranslateFailures[key]; }
}
static void nvmtl_translate_failure_note(NSString *key, NSString *why) {
    @synchronized([NSProcessInfo processInfo]) { if (!gTranslateFailures) gTranslateFailures = [NSMutableDictionary new]; gTranslateFailures[key] = why; }
}

NSData *nvmtl_compile_library_entry(NSData *air, NSString *ll, NSString *stage, NSString *name, NSString **error) {
    if (!air || !ll || !stage || !nvmtl_xlate_ready()) { if(error)*error=@"missing entry or translator"; return nil; }
    if (air.length >= 20 && *(const uint32_t *)air.bytes == 0x07230203u) return air;
        uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
        NSString *spvp = nvmtl_spvcache_path(air, stage);
        NSData *spvData = getenv("NVMTL_SPVCACHE_OFF") ? nil : nvmtl_cache_read(spvp);
        int hit = spvData.length >= 20 && *(const uint32_t *)spvData.bytes == 0x07230203u;
        uint8_t *spv = NULL; size_t spvn = 0; char err[512] = { 0 };
        if (hit) { gSpvHit++; nvmtl_cache_touch(spvp); }
        else {
            // One translation per module at a time: the background pre-translation (nvmtl_pretranslate_library) and the
            // application ask for the same modules, and whoever comes second takes the first one's result. The key is the
            // cache file name, which names the module and the translator's modes.
            NSString *sfKey = spvp ?: [NSString stringWithFormat:@"%p/%@", (__bridge void *)air, stage];
            nvmtl_sf *sf = nvmtl_translate_flight();
            void *shared = NULL;
            if (!nvmtl_sf_begin(sf, sfKey.UTF8String, 1, &shared)) {
                NSData *got = shared ? (NSData *)CFBridgingRelease(shared) : nil;
                if (!got) {
                    if (error) *error = nvmtl_translate_failure_for(sfKey) ?: @"translation failed (see the first FAILED line for this function)";
                    return nil;
                }
                gSpvHit++;
                return got;
            }
            // The owner looks at the cache once more: a translation that finished just before this caller arrived has
            // retired its entry because its file is stored.
            NSData *again = getenv("NVMTL_SPVCACHE_OFF") ? nil : nvmtl_cache_read(spvp);
            if (again.length >= 20 && *(const uint32_t *)again.bytes == 0x07230203u) {
                gSpvHit++; nvmtl_cache_touch(spvp);
                nvmtl_sf_publish(sf, sfKey.UTF8String, (__bridge void *)again);
                nvmtl_sf_retire(sf, sfKey.UTF8String);
                return again;
            }
            NSString *known = nvmtl_translate_failure_for(sfKey);
            if (known) {   // refused before: the same module gives the same refusal
                nvmtl_sf_publish(sf, sfKey.UTF8String, NULL); nvmtl_sf_retire(sf, sfKey.UTF8String);
                if (error) *error = known;
                return nil;
            }
            gSpvMiss++;
            if (xlate(ll.UTF8String, stage.UTF8String, &spv, &spvn, err, sizeof err) != 0) {
                nvlog("library: translate %s (%s) FAILED: %s", name.UTF8String, stage.UTF8String, err);
                nvmtl_translate_failure_note(sfKey, @(err));
                nvmtl_failed_air_store(name, stage.UTF8String, err, ll);
                nvmtl_sf_publish(sf, sfKey.UTF8String, NULL); nvmtl_sf_retire(sf, sfKey.UTF8String);
                if (error) *error = @(err); return nil;
            }
            spvData = [NSData dataWithBytes:spv length:spvn];
            nvmtl_sf_publish(sf, sfKey.UTF8String, (__bridge void *)spvData);
            if (spvp) {
                NSData *stored = [spvData copy]; NSString *file = [spvp copy]; NSString *flightKey = [sfKey copy];
                nvmtl_cache_io(^{
                    const int werr = nvmtl_cache_write(file, stored);
                    if (werr == 0) {
                        gSpvStored++;
                        atomic_fetch_add_explicit(&gSpvAdded, (unsigned long long)(stored.length + NVMTL_CF_FOOTER), memory_order_relaxed);
                    } else if (werr > 0) {
                        nvlog("spvcache: STORE FAILED %s (%zu bytes): %s", file.lastPathComponent.UTF8String,
                              (size_t)stored.length, strerror(werr));
                    }
                    nvmtl_sf_retire(nvmtl_translate_flight(), flightKey.UTF8String);   // the file (or its failure) is final
                });
            } else nvmtl_sf_retire(sf, sfKey.UTF8String);
        }
        if (spv && xlate_free) xlate_free(spv, spvn);
        { double ms = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6;
          nvtrace("XLATE %s %s %zu B %.1f ms %s", name.UTF8String, stage.UTF8String, (size_t)spvData.length, ms, hit ? "hit" : "miss"); }
        nvlog("library: %s (%s) -> %zu bytes of SPIR-V (%s)", name.UTF8String, stage.UTF8String, (size_t)spvData.length, hit ? "cache hit" : "translated");
        if (getenv("NVMTL_REFLECT") && xlate_refl) {
            uint8_t *j = NULL; size_t jn = 0; char je[512] = { 0 };
            if (xlate_refl(ll.UTF8String, stage.UTF8String, &j, &jn, je, sizeof je) == 0) {
                NSString *js = [[NSString alloc] initWithBytes:j length:jn encoding:NSUTF8StringEncoding];
                NSRange ab = [js rangeOfString:@"argument_buffer_fields"];
                if (ab.location != NSNotFound) {
                    NSUInteger end = MIN(js.length, ab.location + 400);
                    nvlog("reflect %s: %s", name.UTF8String, [[js substringWithRange:NSMakeRange(ab.location, end - ab.location)] UTF8String]);
                } else nvlog("reflect %s: no argument_buffer_fields in %zu bytes of reflection", name.UTF8String, jn);
                if (xlate_free) xlate_free(j, jn);
            } else nvlog("reflect %s FAILED: %s", name.UTF8String, je);
        }
    nvlog("library entry: spvcache %lu hit, %lu miss, %lu stored", gSpvHit, gSpvMiss, gSpvStored);
    return spvData;
}

// ---- background pre-translation of a new library's shaders ----
// NVMTL_PRETRANSLATE=1 (the Cyberpunk profile sets it): every entry point of a library the application creates is
// translated and stored in the shader cache by a small low-priority pool, so a cold first run does not translate each
// shader on the thread that asks. It does the work the application's own request would do, through the same
// single-flight entry (nvmtl_compile_library_entry), so a shader is translated once whoever asks first. Only the plain
// translation is made here: variants that depend on function-constant values, and functions that need linking at pipeline
// creation, are left to the pipeline path. NVMTL_PRETRANSLATE=0 or unset: nothing runs. NVMTL_PRETRANSLATE_THREADS (1..4, 2).
typedef struct { void *lib; void *name; } nvmtl_pre_job;
static _Atomic unsigned long gPreQueued, gPreDone, gPreTranslated, gPreCached, gPreSkipped, gPreFailed;
static nvmtl_pool *nvmtl_pre_pool(void) {
    static nvmtl_pool pool; static dispatch_once_t once;
    dispatch_once(&once, ^{
        const char *t = getenv("NVMTL_PRETRANSLATE_THREADS"); int n = t ? atoi(t) : 2;
        nvmtl_pool_init(&pool, n < 1 ? 1 : n > 4 ? 4 : n, 1);
    });
    return &pool;
}
static void nvmtl_pre_run(void *ctx) {
    nvmtl_pre_job *j = ctx;
    @autoreleasepool {
        NVMTLLibrary *lib = CFBridgingRelease(j->lib); NSString *name = CFBridgingRelease(j->name); free(j);
        NSData *air = lib->_fns[name]; NSString *ll = lib->_airs[name], *stage = lib->_stages[name];
        if (!air || !ll || !stage.length || ![@[@"kernel", @"vertex", @"fragment"] containsObject:stage]
            || (air.length >= 20 && *(const uint32_t *)air.bytes == 0x07230203u)
            || (nvmtl_air_text_flags(ll) & (NVMTL_AIRF_DYLD_TABLE | NVMTL_AIRF_ISECT_TABLE))) atomic_fetch_add(&gPreSkipped, 1);
        else {
            NSString *file = getenv("NVMTL_SPVCACHE_OFF") ? nil : nvmtl_spvcache_path(air, stage);
            if (file && access(file.fileSystemRepresentation, F_OK) == 0) atomic_fetch_add(&gPreCached, 1);
            else {
                NSString *why = nil;
                if (nvmtl_compile_library_entry(air, ll, stage, name, &why)) atomic_fetch_add(&gPreTranslated, 1);
                else atomic_fetch_add(&gPreFailed, 1);   // a refusal the pipeline path may still handle (linking, constants)
            }
        }
    }
    unsigned long done = atomic_fetch_add(&gPreDone, 1) + 1;
    if (done == atomic_load(&gPreQueued) || done % 1000 == 0)
        nvlog("pretranslate: %lu of %lu queued shaders processed - %lu translated, %lu already cached, %lu skipped, %lu refused",
              done, (unsigned long)atomic_load(&gPreQueued), (unsigned long)atomic_load(&gPreTranslated),
              (unsigned long)atomic_load(&gPreCached), (unsigned long)atomic_load(&gPreSkipped), (unsigned long)atomic_load(&gPreFailed));
}
void nvmtl_pretranslate_library(NVMTLLibrary *lib) {
    const char *on = getenv("NVMTL_PRETRANSLATE");
    if (!on || strcmp(on, "1") || !lib || !lib->_fns.count || !lib->_airs || !nvmtl_xlate_ready()) return;
    unsigned long n = 0;
    for (NSString *name in lib->_fns) {
        nvmtl_pre_job *j = malloc(sizeof *j); if (!j) break;
        j->lib = (void *)CFBridgingRetain(lib); j->name = (void *)CFBridgingRetain(name);
        atomic_fetch_add(&gPreQueued, 1);
        if (nvmtl_pool_submit(nvmtl_pre_pool(), nvmtl_pre_run, j)) {
            atomic_fetch_sub(&gPreQueued, 1); CFRelease(j->lib); CFRelease(j->name); free(j); break;
        }
        n++;
    }
    nvlog("pretranslate: library of %lu entry points queued for background translation", n);
}

NSDictionary<NSString *, NSData *> *nvmtl_index_metallib_uncached(NSData *lib, NSDictionary **stageOut, NSDictionary **airOut, NSMutableArray *rec)
{
    if (stageOut) *stageOut = nil;
    if (airOut) *airOut = nil;
    if (!nvmtl_xlate_ready()) return nil;
    NSMutableDictionary<NSString *, NSData *> *out = [NSMutableDictionary dictionary];
    NSMutableDictionary *stages = [NSMutableDictionary new], *airs = [NSMutableDictionary new];
    const uint8_t *b = lib.bytes; NSUInteger n = lib.length, found = 0, visible = 0;
    for (NSUInteger i = 0; i + 20 <= n; i++) {
        if (!(b[i] == 0xde && b[i+1] == 0xc0 && b[i+2] == 0x17 && b[i+3] == 0x0b)) continue;
        uint32_t off = 0, size = 0;
        memcpy(&off, b + i + 8, 4); memcpy(&size, b + i + 12, 4);
        if ((NSUInteger)off + size == 0 || i + off + size > n) continue;
        found++;
        NSData *air = [NSData dataWithBytes:b + i length:off + size];
        NSString *ll = nvmtl_air_to_text(air);
        if (!ll) continue;
        NSString *name = nil, *stage = nil;
        if (!nvmtl_entry_of(ll, &name, &stage)) {
            NSArray<NSString *> *vis = nvmtl_visible_of(ll);
            for (NSString *v in vis) { airs[v] = ll; stages[v] = @"visible"; }
            if (vis.count) [rec addObject:@[@(i), vis, @"visible"]];
            if (vis.count) { visible += vis.count; continue; }
            if (n > 0x0A && memcmp(b, "MTLB", 4) == 0 && b[0x0A] == 2) {
                NSArray<NSString *> *ex = nvmtl_exported_defs(ll);
                for (NSString *v in ex) { airs[v] = ll; stages[v] = @"visible"; }
                if (ex.count) {
                    [rec addObject:@[@(i), ex, @"visible"]];
                    visible += ex.count;
                    nvlog("library: dynamic-library blob %lu exports %lu function(s): %s", (unsigned long)found,
                          (unsigned long)ex.count, [[ex componentsJoinedByString:@","] UTF8String]);
                    continue;
                }
            }
            nvlog("library: blob %lu has no air entry-point metadata and no visible functions", (unsigned long)found);
            continue;
        }
        out[name] = air;
        stages[name] = stage;
        airs[name] = ll;
        [rec addObject:@[@(i), name, stage]];
        i += off + size - 1;
    }
    nvlog("library: metallib %lu bytes, %lu bitcode blobs, %lu entries indexed, %lu visible"
          " | aircache %lu hit, %lu miss, %lu stored | spvcache %lu hit, %lu miss, %lu stored (%s)",
          (unsigned long)n, (unsigned long)found, (unsigned long)out.count, (unsigned long)visible,
          gAirHit, gAirMiss, gAirStored, gSpvHit, gSpvMiss, gSpvStored, nvmtl_aircache_dir().UTF8String);
    if (stageOut) *stageOut = [stages copy];
    if (airOut) *airOut = [airs copy];
    return (out.count || visible) ? [out copy] : nil;
}

// A library-index record carries the externs read from the library's AIR text, and that text depends on this process's
// modes (canonical pointers inline helpers). It is named by the library AND the full cache tag, so a record written in
// one mode is never read in another; without a tag there is no record.
static NSString *nvmtl_idx_path(NSData *key) {
    (void)nvmtl_xlate_ready();   // the tag is made there
    if (!strcmp(gXlateTag, "untagged")) return nil;
    NSMutableString *h = [NSMutableString stringWithString:@"idx2-"]; const uint8_t *d = key.bytes;
    for (NSUInteger i = 0; i < key.length; i++) [h appendFormat:@"%02x", d[i]];
    char name[NVMTL_SPVCACHE_TAG_MAX + 160];
    if (!nvmtl_idx_record_name(name, sizeof name, h.UTF8String, gXlateTag)) return nil;
    return [nvmtl_aircache_dir() stringByAppendingPathComponent:@(name)];
}
static _Atomic unsigned long gLazyLibs, gLazyNames, gLazyForced, gLazyFail;
@interface NVMTLLazyAirs : NSDictionary { NSDictionary *_slot; NSArray *_blob; NSMutableArray *_text; os_unfair_lock _lk; }
- (instancetype)initWithSlots:(NSDictionary *)slot blobs:(NSArray *)blob;
- (NSUInteger)blobBytes;
@end
@implementation NVMTLLazyAirs
- (instancetype)initWithObjects:(const id [])o forKeys:(const id<NSCopying> [])k count:(NSUInteger)n { return self; }
- (instancetype)initWithSlots:(NSDictionary *)slot blobs:(NSArray *)blob {
    if (!(self = [super init])) return nil;
    _slot = [slot copy]; _blob = [blob copy]; _lk = OS_UNFAIR_LOCK_INIT;
    _text = [NSMutableArray arrayWithCapacity:blob.count];
    for (NSUInteger i = 0; i < blob.count; i++) [_text addObject:(id)kCFNull];
    const unsigned long libs = atomic_fetch_add(&gLazyLibs, 1) + 1; atomic_fetch_add(&gLazyNames, slot.count);
    if (!(libs & (libs - 1))) nvlog("lazy air: %lu disk-indexed libraries hold %lu names, %lu texts decoded on first use, %lu failed",
                                    libs, atomic_load(&gLazyNames), atomic_load(&gLazyForced), atomic_load(&gLazyFail));
    return self;
}
- (NSUInteger)count { return _slot.count; }
- (NSEnumerator *)keyEnumerator { return [_slot keyEnumerator]; }
- (NSUInteger)countByEnumeratingWithState:(NSFastEnumerationState *)st objects:(id __unsafe_unretained [])buf count:(NSUInteger)len {
    return [_slot countByEnumeratingWithState:st objects:buf count:len];
}
- (id)copyWithZone:(NSZone *)z { return self; }
- (NSUInteger)blobBytes { NSUInteger n = 0; for (NSData *d in _blob) n += d.length; return n; }
- (id)objectForKey:(id)key {
    NSNumber *s = _slot[key]; if (!s) return nil;
    const NSUInteger i = s.unsignedIntegerValue;
    os_unfair_lock_lock(&_lk); id t = _text[i]; os_unfair_lock_unlock(&_lk);
    if (t != (id)kCFNull) return t;
    NSString *ll = nvmtl_air_to_text(_blob[i]);
    if (!ll) {
        atomic_fetch_add(&gLazyFail, 1);
        nvlog("lazy air: blob %lu (asked as \"%s\") has no text - the function is absent, as in the uncached index",
              (unsigned long)i, [key description].UTF8String);
        return nil;
    }
    os_unfair_lock_lock(&_lk);
    if (_text[i] == (id)kCFNull) { _text[i] = ll; atomic_fetch_add(&gLazyForced, 1); } else ll = _text[i];
    os_unfair_lock_unlock(&_lk);
    const unsigned long f = atomic_load(&gLazyForced);
    if (!(f & (f - 1))) nvlog("lazy air: %lu texts decoded on first use across %lu disk-indexed libraries holding %lu names (%lu failed)",
                              f, atomic_load(&gLazyLibs), atomic_load(&gLazyNames), atomic_load(&gLazyFail));
    return ll;
}
@end
static NSDictionary *nvmtl_index_from_record(NSData *lib, NSDictionary *rec, NSDictionary **stageOut, NSDictionary **airOut, NSArray **extOut) {
    NSArray *blobs = rec[@"blobs"], *ext = rec[@"externs"];
    if (![blobs isKindOfClass:NSArray.class] || ![ext isKindOfClass:NSArray.class] || ![rec[@"length"] isEqual:@(lib.length)] || !blobs.count) return nil;
    NSMutableDictionary *out = [NSMutableDictionary new], *stages = [NSMutableDictionary new], *airs = [NSMutableDictionary new];
    const uint8_t *b = lib.bytes; NSUInteger n = lib.length, visible = 0;
    const int lazy = !getenv("NVMTL_NO_LAZYAIR"); NSMutableArray *blob = [NSMutableArray new];
    const int nocopy = !getenv("NVMTL_NO_NOCOPY");
    static _Atomic int nocopySaid;
    if (nocopy && !atomic_exchange(&nocopySaid, 1)) nvlog("library index: disk-indexed blobs reference the container bytes (no copy; NVMTL_NO_NOCOPY=1 copies)");
    for (NSArray *e in blobs) {
        if (![e isKindOfClass:NSArray.class] || e.count != 3 || ![e[0] isKindOfClass:NSNumber.class]) return nil;
        NSUInteger i = [e[0] unsignedIntegerValue];
        if (i + 20 > n || !(b[i] == 0xde && b[i+1] == 0xc0 && b[i+2] == 0x17 && b[i+3] == 0x0b)) return nil;
        uint32_t off = 0, size = 0; memcpy(&off, b + i + 8, 4); memcpy(&size, b + i + 12, 4);
        if ((NSUInteger)off + size == 0 || i + off + size > n) return nil;
        NSData *air = nocopy ? [[NSData alloc] initWithBytesNoCopy:(void *)(b + i) length:off + size deallocator:^(void *p, NSUInteger l) { (void)lib; }]
                             : [NSData dataWithBytes:b + i length:off + size];
        id ll = nil;
        if (lazy) { ll = @(blob.count); [blob addObject:air]; }
        else { ll = nvmtl_air_to_text(air); if (!ll) return nil; }
        if ([e[1] isKindOfClass:NSArray.class]) { for (NSString *v in e[1]) { airs[v] = ll; stages[v] = @"visible"; visible++; } }
        else if ([e[1] isKindOfClass:NSString.class] && [e[2] isKindOfClass:NSString.class]) { out[e[1]] = air; stages[e[1]] = e[2]; airs[e[1]] = ll; }
        else return nil;
    }
    if (!out.count && !visible) return nil;
    *stageOut = [stages copy]; *extOut = ext;
    *airOut = lazy ? [[NVMTLLazyAirs alloc] initWithSlots:airs blobs:blob] : [airs copy];
    return [out copy];
}
static NSDictionary *nvmtl_index_metallib_disk(NSData *lib, NSData *key, NSDictionary **stageOut, NSDictionary **airOut) {
    NSString *p = getenv("NVMTL_IDXCACHE_OFF") ? nil : nvmtl_idx_path(key);
    NSData *d = p ? [NSData dataWithContentsOfFile:p] : nil;
    if (d) {
        NSDictionary *rec = [NSPropertyListSerialization propertyListWithData:d options:0 format:NULL error:NULL];
        NSDictionary *st = nil, *ai = nil; NSArray *ext = nil;
        NSDictionary *fns = [rec isKindOfClass:NSDictionary.class] ? nvmtl_index_from_record(lib, rec, &st, &ai, &ext) : nil;
        if (fns) {
            objc_setAssociatedObject(ai, &gExternsKey, ext, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            nvlog("library index: DISK hit bytes=%lu entries=%lu names=%lu externs=%lu", (unsigned long)lib.length, (unsigned long)fns.count, (unsigned long)ai.count, (unsigned long)ext.count);
            if (stageOut) *stageOut = st; if (airOut) *airOut = ai;
            return fns;
        }
        nvlog("library index: disk record %s does not validate against this library - rebuilding it", p.lastPathComponent.UTF8String);
    }
    NSMutableArray *rec = [NSMutableArray new]; NSDictionary *st = nil, *ai = nil;
    NSDictionary *fns = nvmtl_index_metallib_uncached(lib, &st, &ai, rec);
    if (stageOut) *stageOut = st; if (airOut) *airOut = ai;
    if (fns && st && ai && p && rec.count) {
        NSArray *ext = nvmtl_externs_of_airs(ai) ?: @[];
        NSData *out = [NSPropertyListSerialization dataWithPropertyList:@{@"v": @1, @"length": @(lib.length), @"blobs": rec, @"externs": ext}
                                                                 format:NSPropertyListBinaryFormat_v1_0 options:0 error:NULL];
        NSString *file = [p copy], *tmp = [p stringByAppendingFormat:@".%d.tmp", getpid()];
        const unsigned long blobs = (unsigned long)rec.count, externs = (unsigned long)ext.count;
        nvmtl_cache_io(^{
            if (out && [out writeToFile:tmp atomically:NO] && rename(tmp.fileSystemRepresentation, file.fileSystemRepresentation) == 0)
                nvlog("library index: disk record stored (%lu blobs, %lu externs) %s", blobs, externs, file.lastPathComponent.UTF8String);
            else { unlink(tmp.fileSystemRepresentation); nvlog("library index: could not store the disk record %s", file.lastPathComponent.UTF8String); }
        });
    }
    return fns;
}

typedef struct { char magic[8]; uint64_t dev, ino, size; int64_t mts, mtn, cts, ctn; uint8_t edge[32], key[32]; } nvmtl_lk1_rec;
static _Atomic unsigned long gLkHit, gLkMiss, gLkStale, gLkStored;
NSData *nvmtl_lib_sha256(NSData *lib) {
    CC_SHA256_CTX hash; CC_SHA256_Init(&hash);
    const uint8_t *ptr=lib.bytes; NSUInteger remaining=lib.length;
    while (remaining) { CC_LONG chunk=(CC_LONG)MIN(remaining,(NSUInteger)UINT32_MAX); CC_SHA256_Update(&hash,ptr,chunk);ptr+=chunk;remaining-=chunk; }
    uint8_t digest[CC_SHA256_DIGEST_LENGTH];CC_SHA256_Final(digest,&hash);
    return [NSData dataWithBytes:digest length:sizeof(digest)];
}
static void nvmtl_lk1_edge(NSData *lib, uint8_t out[32]) {
    const NSUInteger n = lib.length, e = MIN(n, (NSUInteger)65536); const uint8_t *b = lib.bytes; const uint64_t len = n;
    CC_SHA256_CTX h; CC_SHA256_Init(&h); CC_SHA256_Update(&h, &len, sizeof len);
    CC_SHA256_Update(&h, b, (CC_LONG)e); CC_SHA256_Update(&h, b + n - e, (CC_LONG)e); CC_SHA256_Final(out, &h);
}
int nvmtl_stat_same(const struct stat *a, const struct stat *b) {
    return a->st_dev == b->st_dev && a->st_ino == b->st_ino && a->st_size == b->st_size &&
           a->st_mtimespec.tv_sec == b->st_mtimespec.tv_sec && a->st_mtimespec.tv_nsec == b->st_mtimespec.tv_nsec &&
           a->st_ctimespec.tv_sec == b->st_ctimespec.tv_sec && a->st_ctimespec.tv_nsec == b->st_ctimespec.tv_nsec;
}
NSData *nvmtl_libkey_for_file(NSString *path, NSData *lib, const struct stat *before) {
    if (!path.length || !lib.length || getenv("NVMTL_NO_LIBMEMO")) return nil;
    const int weak = getenv("NVMTL_LIBMEMO_WEAK_TEST") != NULL, noct = weak || getenv("NVMTL_LIBMEMO_NOCTIME_TEST") != NULL;
    struct stat now;
    if (stat(path.fileSystemRepresentation, &now) != 0 || !nvmtl_stat_same(before, &now) || (uint64_t)now.st_size != lib.length) {
        nvlog("library key memo: %s changed while it was read (or cannot be stat'ed) - hashing the bytes, storing nothing", path.UTF8String);
        return nil; }
    uint8_t ph[CC_SHA256_DIGEST_LENGTH]; const char *pp = path.fileSystemRepresentation; CC_SHA256(pp, (CC_LONG)strlen(pp), ph);
    char name[48]; snprintf(name, sizeof name, "lk1-%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x%02x.bin",
                            ph[0], ph[1], ph[2], ph[3], ph[4], ph[5], ph[6], ph[7], ph[8], ph[9], ph[10], ph[11]);
    NSString *rp = [nvmtl_aircache_dir() stringByAppendingPathComponent:@(name)];
    uint8_t edge[32]; nvmtl_lk1_edge(lib, edge);
    nvmtl_lk1_rec r; NSData *rd = [NSData dataWithContentsOfFile:rp];
    if (rd.length == sizeof r) {
        memcpy(&r, rd.bytes, sizeof r);
        const int stat_eq = !memcmp(r.magic, "NVLK1\0\0\0", 8) && r.dev == (uint64_t)now.st_dev && r.ino == (uint64_t)now.st_ino &&
            r.size == (uint64_t)now.st_size && r.mts == now.st_mtimespec.tv_sec &&
            (weak || r.mtn == now.st_mtimespec.tv_nsec) && (noct || (r.cts == now.st_ctimespec.tv_sec && r.ctn == now.st_ctimespec.tv_nsec));
        if (stat_eq && weak) {
            nvlog("library key memo: NVMTL_LIBMEMO_WEAK_TEST served the recorded key for %s - no ctime, no content check (negative control only)", path.UTF8String);
            return [NSData dataWithBytes:r.key length:sizeof r.key]; }
        if (stat_eq && !memcmp(r.edge, edge, sizeof edge)) {
            const unsigned long h = atomic_fetch_add(&gLkHit, 1) + 1;
            if (!(h & (h - 1))) nvlog("library key memo: hit %lu (%lu B %s) - whole-library hash skipped; misses %lu stale %lu stored %lu%s",
                                      h, (unsigned long)lib.length, path.lastPathComponent.UTF8String, atomic_load(&gLkMiss),
                                      atomic_load(&gLkStale), atomic_load(&gLkStored), noct ? " (NOCTIME_TEST: ctime ignored)" : "");
            return [NSData dataWithBytes:r.key length:sizeof r.key]; }
        if (stat_eq) {
            atomic_fetch_add(&gLkStale, 1);
            nvlog("library key memo: STALE record refused for %s - stat matches%s but the first/last 64 KiB differ; re-hashing",
                  path.UTF8String, noct ? " (ctime ignored by NOCTIME_TEST)" : "");
        }
    }
    atomic_fetch_add(&gLkMiss, 1);
    NSData *key = nvmtl_lib_sha256(lib);
    memset(&r, 0, sizeof r); memcpy(r.magic, "NVLK1\0\0\0", 8);
    r.dev = (uint64_t)now.st_dev; r.ino = (uint64_t)now.st_ino; r.size = (uint64_t)now.st_size;
    r.mts = now.st_mtimespec.tv_sec; r.mtn = now.st_mtimespec.tv_nsec; r.cts = now.st_ctimespec.tv_sec; r.ctn = now.st_ctimespec.tv_nsec;
    memcpy(r.edge, edge, sizeof edge); memcpy(r.key, key.bytes, sizeof r.key);
    [[NSFileManager defaultManager] createDirectoryAtPath:nvmtl_aircache_dir() withIntermediateDirectories:YES attributes:nil error:nil];
    NSString *tmp = [rp stringByAppendingFormat:@".%d.tmp", getpid()], *file = [rp copy], *base = [path.lastPathComponent copy], *entry = @(name);
    NSData *record = [NSData dataWithBytes:&r length:sizeof r];
    const unsigned long libBytes = (unsigned long)lib.length;
    nvmtl_cache_io(^{
        if ([record writeToFile:tmp atomically:NO] && rename(tmp.fileSystemRepresentation, file.fileSystemRepresentation) == 0) {
            const unsigned long st = atomic_fetch_add(&gLkStored, 1) + 1;
            if (!(st & (st - 1))) nvlog("library key memo: stored %lu (%lu B %s)", st, libBytes, base.UTF8String);
        } else { unlink(tmp.fileSystemRepresentation); nvlog("library key memo: could not store %s", entry.UTF8String); }
    });
    return key;
}
NSDictionary<NSString *, NSData *> *nvmtl_translate_metallib_k(NSData *lib, NSData *knownKey, NSDictionary **stageOut, NSDictionary **airOut);
NSDictionary<NSString *, NSData *> *nvmtl_translate_metallib(NSData *lib, NSDictionary **stageOut, NSDictionary **airOut) {
    return nvmtl_translate_metallib_k(lib, nil, stageOut, airOut); }

NSDictionary<NSString *, NSData *> *nvmtl_translate_metallib_k(NSData *lib, NSData *knownKey, NSDictionary **stageOut, NSDictionary **airOut) {
    if (stageOut) *stageOut = nil;
    if (airOut) *airOut = nil;
    if (!lib) return nil;
    static NSObject *lock;
    static NSMutableDictionary *entries;
    static NSMapTable *weakEntries;
    static char metadataKey;
    static NSMutableArray *lru;
    static NSUInteger retainedBytes;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ lock=[NSObject new]; entries=[NSMutableDictionary new]; weakEntries=[NSMapTable strongToWeakObjectsMapTable]; lru=[NSMutableArray new]; });
    NSData *key=knownKey.length==CC_SHA256_DIGEST_LENGTH ? knownKey : nvmtl_lib_sha256(lib);
    @synchronized(lock) {
        NSDictionary *hit=[weakEntries objectForKey:key];
        NSArray *meta=hit ? objc_getAssociatedObject(hit,&metadataKey) : nil;
        if(hit) {
            [lru removeObject:key];[lru addObject:key];
            if(stageOut)*stageOut=meta[0];if(airOut)*airOut=meta[1];
            nvlog("library index: hit bytes=%lu retained=%lu entries=%lu",(unsigned long)lib.length,(unsigned long)retainedBytes,(unsigned long)entries.count);
            return hit;
        }
    }
    NSDictionary *stages=nil,*airs=nil;
    NSDictionary *fns=nvmtl_index_metallib_disk(lib,key,&stages,&airs);
    if(stageOut)*stageOut=stages;if(airOut)*airOut=airs;
    if(!fns || !stages || !airs)return fns;
    const NSUInteger budget=128ull*1024*1024;
    NSUInteger cost=4096;
    for(NSData *data in fns.allValues) { if(cost>budget || data.length>budget-cost){cost=budget+1;break;}cost+=data.length; }
    const BOOL lazyAirs=[airs isKindOfClass:NVMTLLazyAirs.class];
    if(lazyAirs){ NSUInteger units=((NVMTLLazyAirs *)airs).blobBytes*4; if(cost>budget-512 || units>(budget-cost-512)/2) cost=budget+1; else cost+=units*2+512; }
    if(!lazyAirs) for(NSString *name in airs) {
        NSString *ir=airs[name];NSUInteger units=ir.length+name.length;
        if(cost>budget-512 || units>(budget-cost-512)/2){cost=budget+1;break;}
        cost+=units*2+512;
    }
    if (!fns.count) fns = [NSMutableDictionary dictionaryWithDictionary:fns];
    objc_setAssociatedObject(fns,&metadataKey,@[stages,airs,@(cost)],OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    @synchronized(lock) {
        NSDictionary *existing=[weakEntries objectForKey:key];
        if(!existing) {
            [lru removeObject:key];
            while(lru.count && (lru.count>=16 || (cost<=budget && retainedBytes>budget-cost))) {
                NSData *old=lru.firstObject;NSDictionary *owned=entries[old];
                if(owned)retainedBytes-=[objc_getAssociatedObject(owned,&metadataKey)[2] unsignedIntegerValue];
                [entries removeObjectForKey:old];[weakEntries removeObjectForKey:old];[lru removeObjectAtIndex:0];
            }
            [weakEntries setObject:fns forKey:key];[lru addObject:key];
            if(cost<=budget){entries[key]=fns;retainedBytes+=cost;}
            nvlog("library index: stored cost=%lu retained=%lu entries=%lu weak=%d",(unsigned long)cost,(unsigned long)retainedBytes,(unsigned long)lru.count,cost>budget);
        }
    }
    return fns;
}

static NSString *nvmtl_link_dump_dir(void) {
    const char *v = getenv("NVMTL_LINK_DUMP");
    if (!v || !*v) return nil;
    NSString *root = (v[0] == '/') ? [NSString stringWithUTF8String:v] : @"/tmp";
    return root ? [root stringByAppendingFormat:@"/nvmtl-link-%d", getpid()] : nil;
}
// Set by a caller that defers a function needing linked visible functions to pipeline creation. On that path a
// first specialization that stops at an unlinked call is expected, and logging it as a failure put a false
// failure in every report from a Mac drawing the system's icons. Everywhere else the line stays a failure,
// because a caller that is not deferring has no second chance and the refusal is real.
static __thread int g_fc_expect_link;
void nvmtl_fc_expect_link(int on) { g_fc_expect_link = on; }
BOOL nvmtl_needs_visible_link(NSString *air, NSString *why);
// Writes the module handed to the translator (AIR text after linking, stage, constant values, error) to <dir>/fc-<pid>-<n>,
// so a translation can be replayed offline with the same input. Nothing is written when dir is unset.
static void nvmtl_fc_dump(const char *dd, NSString *ll, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                          const uint8_t *const *payloads, size_t n, const char *e)
{
    if (!dd || !*dd) return;
    NSString *dir = @(dd); static _Atomic unsigned seq;
    [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:nil];
    NSString *base = [dir stringByAppendingFormat:@"/fc-%d-%u", getpid(), atomic_fetch_add(&seq, 1)];
    [ll writeToFile:[base stringByAppendingString:@".ll"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
    NSMutableString *meta = [NSMutableString stringWithFormat:@"stage %@\nerror %s\n", stage, e ? e : ""];
    for (size_t i = 0; i < n; i++) {
        [meta appendFormat:@"fc %u size %u bytes", idx[i], sizes[i]];
        for (uint32_t b = 0; b < sizes[i] && payloads[i]; b++) [meta appendFormat:@" %02x", payloads[i][b]];
        [meta appendString:@"\n"];
    }
    [meta writeToFile:[base stringByAppendingString:@".txt"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
}
// NVMTL_FC_SPEC=1: function-constant variants are made from one specialization-constant module per function instead of one
// translation each. OFF by default: the module keeps the branches the values decide, so a variant whose constants would
// have folded the module small in a translation of its own comes out larger here; whether that costs more in the GPU
// compiler than the translation saved has to be measured on a card.
static BOOL nvmtl_fc_spec_on(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_FC_SPEC"); on = e && e[0] == '1';
                  nvlog("function constants: one module per function with specialization constants %s (NVMTL_FC_SPEC=%s)", on ? "on" : "off", e ?: "unset"); }
    return on;
}
// The specialization-constant module of an AIR function and the table of its constants, from memory, from the cache
// folder, or translated now (once per function and translator build). Entry layout: 'SPC1', table bytes, table, module.
static BOOL nvmtl_fc_spec_module(NSString *ll, NSString *stage, NSData **module, NSData **table, NSString **why)
{
    NVMTLAirDigest *ad = nvmtl_air_state(ll);
    NSData *tail = [@"spec1" dataUsingEncoding:NSUTF8StringEncoding];
    NSString *path = ad ? nvmtl_spvcache_path_air(ad, tail, stage) : nil;
    NSString *key = ad ? (path ?: nvmtl_spvcache_memo_key_air(ad, tail, stage)) : nil;
    NSData *entry = key ? [nvmtl_memo() objectForKey:key] : nil;
    if (!entry && path) entry = nvmtl_cache_read(path);
    uint32_t magic = 0, tlen = 0;
    if (entry.length >= 8) { memcpy(&magic, entry.bytes, 4); memcpy(&tlen, (const char *)entry.bytes + 4, 4); }
    if (magic != 0x31435053u || (size_t)tlen % 20 != 0 || entry.length < 8 + (size_t)tlen + 20) {
        static NSObject *lock; static dispatch_once_t once; dispatch_once(&once, ^{ lock = [NSObject new]; });
        @synchronized (lock) {
            entry = key ? [nvmtl_memo() objectForKey:key] : nil;   // another thread may have just made it
            if (entry.length >= 8) { memcpy(&magic, entry.bytes, 4); memcpy(&tlen, (const char *)entry.bytes + 4, 4); }
            if (!(entry.length >= 8 && magic == 0x31435053u)) {
                uint8_t *o = NULL, *t = NULL; size_t on = 0, tn = 0; char e[512] = {0};
                const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
                if (xlate_spec(ll.UTF8String, stage.UTF8String, &o, &on, &t, &tn, e, sizeof e) != 0) {
                    if (why) *why = [NSString stringWithUTF8String:e[0] ? e : "no specialization-constant module"];
                    return NO;
                }
                NSMutableData *made = [NSMutableData dataWithCapacity:8 + tn + on];
                uint32_t hdr[2] = { 0x31435053u, (uint32_t)tn };
                [made appendBytes:hdr length:8]; [made appendBytes:t length:tn]; [made appendBytes:o length:on];
                if (xlate_free) { xlate_free(o, on); xlate_free(t, tn); }
                entry = made; tlen = (uint32_t)tn;
                nvlog("fc spec: made the one module for this function: %lu B, %lu constants, %.1f ms", (unsigned long)on, (unsigned long)(tn / 20),
                      (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6);
                if (path) { NSData *stored = [entry copy]; NSString *file = [path copy]; nvmtl_cache_io(^{ (void)nvmtl_cache_write(file, stored); }); }
            }
            if (key) [nvmtl_memo() setObject:entry forKey:key cost:entry.length];
        }
    }
    if (entry.length < 8 + (size_t)tlen) { if (why) *why = @"specialization-constant cache entry is short"; return NO; }
    *table = [entry subdataWithRange:NSMakeRange(8, tlen)];
    *module = [entry subdataWithRange:NSMakeRange(8 + tlen, entry.length - 8 - tlen)];
    return YES;
}
static NSData *nvmtl_fc_via_spec(NSString *ll, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                                 const uint8_t *const *payloads, size_t n, NSString **why)
{
    NSData *module = nil, *table = nil;
    if (!nvmtl_fc_spec_module(ll, stage, &module, &table, why)) return nil;
    uint8_t *out = NULL; size_t out_len = 0; char e[512] = {0};
    if (xlate_spec_apply(module.bytes, module.length, table.bytes, table.length, idx, sizes, payloads, n, &out, &out_len, e, sizeof e) != 0) {
        if (why) *why = [NSString stringWithUTF8String:e[0] ? e : "applying the constants failed"];
        return nil;
    }
    NSData *d = [NSData dataWithBytes:out length:out_len];
    if (xlate_free) xlate_free(out, out_len);
    return d;
}
NSData *nvmtl_translate_with_fc(NSString *ll, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                                const uint8_t *const *payloads, size_t n, NSString **err)
{
    if (!nvmtl_xlate_ready()) { if (err) *err = @"the AIR->SPIR-V translator is not loaded"; return nil; }
    if (!xlate_fc) { if (err) *err = @"this translator has no nvmtl_translate_fc — rebuild nvmtl_translate"; return nil; }
    if (!ll || !stage.length) { if (err) *err = @"no AIR kept for this function"; return nil; }
    uint8_t *out = NULL; size_t out_len = 0; char e[512] = {0};
    // NVMTL_FC_DUMP_ALL=<dir>: keep every module before it is translated, including one whose translation never returns.
    nvmtl_fc_dump(getenv("NVMTL_FC_DUMP_ALL"), ll, stage, idx, sizes, payloads, n, NULL);
    if (n > 0 && nvmtl_fc_spec_on() && xlate_spec && xlate_spec_apply) {
        NSString *why = nil;
        NSData *viaSpec = nvmtl_fc_via_spec(ll, stage, idx, sizes, payloads, n, &why);
        if (viaSpec) return viaSpec;
        nvlog("function constants: the one-module path declined (%s); translating this set", why.UTF8String ?: "?");
    }
    if (xlate_fc(ll.UTF8String, stage.UTF8String, idx, sizes, payloads, n, &out, &out_len, e, sizeof e) != 0) {
        if (g_fc_expect_link && nvmtl_needs_visible_link(ll, @(e)))
            nvlog("function constants: this function calls linked visible functions; it is specialized when its pipeline is created");
        else nvlog("function constants: re-translate FAILED: %s", e);
        // NVMTL_FC_FAIL_DUMP=<dir>: keep the refused module (AIR text, stage, constant values, error) so the refusal can be
        // replayed offline against the translator; nothing is written unless the variable is set.
        nvmtl_fc_dump(getenv("NVMTL_FC_FAIL_DUMP"), ll, stage, idx, sizes, payloads, n, e);
        if (err) *err = [NSString stringWithUTF8String:e[0] ? e : "specialization failed"];
        return nil;
    }
    NSData *d = [NSData dataWithBytes:out length:out_len];
    if (xlate_free) xlate_free(out, out_len);
    return d;
}

/* Function-constant warm-up.
 * WindowServer's watchdog killed it (bug_type 409, at the login window after a driver update) while its main thread
 * translated Core Animation's fixed_frag variants one after another, 1.3-2 s each: the update's new translator build
 * swept every cached variant, and the main thread asked for dozens. A set that took long is recorded beside the cache
 * (.fcwarm.v1: the AIR text's digest and the exact key bytes), and the first time a process asks for any variant of
 * that AIR text, every other recorded variant whose cache file is missing is translated on background threads into the
 * same cache files. The main thread then reads a file, or waits for the translation already running for it, instead
 * of starting its own. Entries are written by the same code with the same key bytes (recorded verbatim) and the same
 * translator, so a warmed file is the file a live miss would have written. NVMTL_FCWARM_OFF=1 turns it off. */
#define NVMTL_FCWARM_RECORD_MS 100      // chosen: a cached variant costs < 5 ms; the uber-shader variants cost 300-2000
#define NVMTL_FCWARM_MAX_RECORDS 1024   // chosen: WindowServer held 67 such variants; bounds the file at ~1 MB
#define NVMTL_FCWARM_JOIN_SEC 60        // chosen: longer than any measured translation (3.2 s), shorter than a hang
static int nvmtl_fcwarm_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_FCWARM_OFF"); on = (e && *e && *e != '0') ? 0 : 1; }
    return on;
}
static NSString *nvmtl_fcwarm_records_path(void) {
    NSString *d = nvmtl_spvcache_dir();
    // a dot name: the stale-build sweep removes only *.c3.spv and the trimmer skips dot files
    return d ? [d stringByAppendingPathComponent:@".fcwarm.v1"] : nil;
}
static NSString *nvmtl_fcwarm_air_hex(NVMTLAirDigest *ad) {
    CC_SHA256_CTX c = ad->_ctx;
    unsigned char h[CC_SHA256_DIGEST_LENGTH];
    CC_SHA256_Final(h, &c);
    char hex[2 * CC_SHA256_DIGEST_LENGTH + 1];
    for (int i = 0; i < CC_SHA256_DIGEST_LENGTH; i++) snprintf(hex + 2 * i, 3, "%02x", h[i]);
    return @(hex);
}
static os_unfair_lock gFcWarmLock = OS_UNFAIR_LOCK_INIT;
static NSMutableSet<NSString *> *gFcWarmRecorded;          // "<airhex> <base64 key>" lines in the file
static NSMutableSet<NSString *> *gFcWarmKicked;            // AIR digests this process has warmed
static NSMutableDictionary<NSString *, dispatch_group_t> *gFcWarmInFlight;   // cache path -> its background translation
static void nvmtl_fcwarm_load_locked(NSString *file) {
    if (gFcWarmRecorded) return;
    gFcWarmRecorded = [NSMutableSet set]; gFcWarmKicked = [NSMutableSet set]; gFcWarmInFlight = [NSMutableDictionary dictionary];
    NSString *text = file ? [NSString stringWithContentsOfFile:file encoding:NSUTF8StringEncoding error:NULL] : nil;
    for (NSString *line in [text componentsSeparatedByString:@"\n"])
        if (line.length > 65 && [line characterAtIndex:64] == ' ' && gFcWarmRecorded.count < NVMTL_FCWARM_MAX_RECORDS)
            [gFcWarmRecorded addObject:line];
}
// Remembers a slow set for the next process, once per set, appended on the cache-io queue.
static void nvmtl_fcwarm_record(NVMTLAirDigest *ad, NSData *key) {
    NSString *file = nvmtl_fcwarm_records_path();
    if (!file || !ad || !key) return;
    NSString *line = [NSString stringWithFormat:@"%@ %@", nvmtl_fcwarm_air_hex(ad), [key base64EncodedStringWithOptions:0]];
    os_unfair_lock_lock(&gFcWarmLock);
    nvmtl_fcwarm_load_locked(file);
    const BOOL fresh = ![gFcWarmRecorded containsObject:line] && gFcWarmRecorded.count < NVMTL_FCWARM_MAX_RECORDS;
    if (fresh) [gFcWarmRecorded addObject:line];
    os_unfair_lock_unlock(&gFcWarmLock);
    if (!fresh) return;
    NSData *append = [[line stringByAppendingString:@"\n"] dataUsingEncoding:NSUTF8StringEncoding];
    nvmtl_cache_io(^{
        int fd = open(file.fileSystemRepresentation, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
        if (fd < 0) return;
        if (write(fd, append.bytes, append.length) != (ssize_t)append.length) nvlog("fc warm-up: record write failed: %s", strerror(errno));
        close(fd);
    });
}
// The key bytes back to what nvmtl_translate_with_fc takes. NO for anything nvmtl_translate_with_fc_cached never wrote.
static BOOL nvmtl_fcwarm_unpack(NSData *key, NSString **stage, NSMutableData **packed, size_t *n) {
    NSDictionary *cfg = [NSJSONSerialization JSONObjectWithData:key options:0 error:NULL];
    NSDictionary *fc = [cfg isKindOfClass:[NSDictionary class]] ? cfg[@"constants"] : nil;
    NSString *st = [cfg isKindOfClass:[NSDictionary class]] ? cfg[@"stage"] : nil;
    if (![fc isKindOfClass:[NSDictionary class]] || ![st isKindOfClass:[NSString class]] || !fc.count || fc.count > 4096) return NO;
    const NSUInteger cap = fc.count;
    NSMutableData *m = [NSMutableData dataWithLength:cap * (2 * sizeof(uint32_t) + sizeof(uint8_t *) + 16)];
    uint32_t *idx = m.mutableBytes, *sz = idx + cap;
    const uint8_t **pay = (const uint8_t **)(sz + cap);
    uint8_t (*bytes)[16] = (uint8_t (*)[16])(pay + cap);
    size_t k = 0;
    for (NSString *ks in fc) {
        NSArray *a = fc[ks];
        if (![ks isKindOfClass:[NSString class]] || ![a isKindOfClass:[NSArray class]] || !a.count || a.count > 16) return NO;
        idx[k] = (uint32_t)ks.integerValue; sz[k] = (uint32_t)a.count;
        for (NSUInteger j = 0; j < a.count; j++) {
            if (![a[j] isKindOfClass:[NSNumber class]]) return NO;
            bytes[k][j] = (uint8_t)[a[j] unsignedCharValue];
        }
        pay[k] = bytes[k]; k++;
    }
    *stage = st; *packed = m; *n = k;
    return YES;
}
static dispatch_queue_t nvmtl_fcwarm_queue(void) {
    static dispatch_queue_t q; static dispatch_once_t once;
    dispatch_once(&once, ^{ q = dispatch_queue_create("nvmtl.fc-warm", dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_CONCURRENT, QOS_CLASS_UTILITY, 0)); });
    return q;
}
static dispatch_semaphore_t nvmtl_fcwarm_slots(void) {
    // two at a time: each translation of an uber-shader holds a few hundred MB, and WindowServer keeps the other cores
    static dispatch_semaphore_t s; static dispatch_once_t once;
    dispatch_once(&once, ^{ s = dispatch_semaphore_create(2); });
    return s;
}
// First variant of this AIR text in this process: translate every recorded sibling whose cache file is missing. The
// caller's own set (mine) is left to the caller, which translates it at once instead of queueing behind the others.
static void nvmtl_fcwarm_kick(NSString *ll, NVMTLAirDigest *ad, NSString *mine) {
    NSString *file = nvmtl_fcwarm_records_path();
    if (!file || !ad || !nvmtl_fcwarm_on()) return;
    NSString *hex = nvmtl_fcwarm_air_hex(ad);
    NSMutableArray<NSData *> *keys = [NSMutableArray array];
    os_unfair_lock_lock(&gFcWarmLock);
    nvmtl_fcwarm_load_locked(file);
    const BOOL first = ![gFcWarmKicked containsObject:hex];
    if (first) {
        [gFcWarmKicked addObject:hex];
        for (NSString *line in gFcWarmRecorded) {
            if (![line hasPrefix:hex]) continue;
            NSData *key = [[NSData alloc] initWithBase64EncodedString:[line substringFromIndex:65] options:0];
            if (key) [keys addObject:key];
        }
    }
    os_unfair_lock_unlock(&gFcWarmLock);
    if (!keys.count) return;
    unsigned queued = 0;
    for (NSData *key in keys) {
        NSString *path = nvmtl_spvcache_path_air(ad, key, @"fc1");
        if (!path || [path isEqualToString:mine] || [[NSFileManager defaultManager] fileExistsAtPath:path]) continue;
        NSString *stage = nil; NSMutableData *packed = nil; size_t n = 0;
        if (!nvmtl_fcwarm_unpack(key, &stage, &packed, &n)) continue;
        dispatch_group_t g = dispatch_group_create();
        os_unfair_lock_lock(&gFcWarmLock);
        const BOOL taken = gFcWarmInFlight[path] != nil;
        if (!taken) gFcWarmInFlight[path] = g;
        os_unfair_lock_unlock(&gFcWarmLock);
        if (taken) continue;
        queued++;
        dispatch_group_async(g, nvmtl_fcwarm_queue(), ^{
            dispatch_semaphore_wait(nvmtl_fcwarm_slots(), DISPATCH_TIME_FOREVER);
            const uint32_t *idx = packed.bytes, *sz = idx + n;
            const uint8_t *const *pay = (const uint8_t *const *)(sz + n);
            NSString *e = nil;
            NSData *spv = [[NSFileManager defaultManager] fileExistsAtPath:path] ? nil
                        : nvmtl_translate_with_fc(ll, stage, idx, sz, pay, n, &e);
            // written here, not on the cache-io queue, so the file is there when the group finishes
            if (spv && nvmtl_is_spirv(spv) && nvmtl_cache_write(path, spv) == 0) gFcStored++;   // same framed format the reader proves
            else if (!spv && e) nvlog("fc warm-up: a recorded set no longer translates: %s", e.UTF8String);
            dispatch_semaphore_signal(nvmtl_fcwarm_slots());
            os_unfair_lock_lock(&gFcWarmLock);
            [gFcWarmInFlight removeObjectForKey:path];
            os_unfair_lock_unlock(&gFcWarmLock);
        });
    }
    if (queued) nvlog("fc warm-up: %u recorded variant(s) of this function translating in the background", queued);
}
// A miss whose file a background translation is writing waits for it rather than translating again.
static BOOL nvmtl_fcwarm_join(NSString *path) {
    if (!path || !nvmtl_fcwarm_on()) return NO;
    os_unfair_lock_lock(&gFcWarmLock);
    dispatch_group_t g = gFcWarmInFlight[path];
    os_unfair_lock_unlock(&gFcWarmLock);
    if (!g) return NO;
    const long timedOut = dispatch_group_wait(g, dispatch_time(DISPATCH_TIME_NOW, (int64_t)NVMTL_FCWARM_JOIN_SEC * NSEC_PER_SEC));
    nvlog("fc warm-up: JOINED the background translation%s", timedOut ? " - it did not finish in time, translating here" : "");
    return !timedOut;
}

NSData *nvmtl_translate_with_fc_cached(NSString *ll, NSString *stage, NSString *name, NSDictionary *constants,
                                       const uint32_t *idx, const uint32_t *sizes,
                                       const uint8_t *const *payloads, size_t n, NSString **err)
{
    if (!ll || !constants.count || !nvmtl_xlate_ready())
        return nvmtl_translate_with_fc(ll, stage, idx, sizes, payloads, n, err);
    NSDictionary *config = @{ @"stage": stage ?: @"", @"entry": name ?: @"", @"constants": constants };
    NSData *json = [NSJSONSerialization dataWithJSONObject:config options:NSJSONWritingSortedKeys error:NULL];
    NVMTLAirDigest *airDigest = nvmtl_air_state(ll);
    if (!json || !airDigest) return nvmtl_translate_with_fc(ll, stage, idx, sizes, payloads, n, err);
    NSString *path = nvmtl_spvcache_path_air(airDigest, json, @"fc1");
    NSString *memoKey = path ?: nvmtl_spvcache_memo_key_air(airDigest, json, @"fc1");
    uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    const BOOL fcCacheOn = !getenv("NVMTL_FCCACHE_OFF");
    if (fcCacheOn) nvmtl_fcwarm_kick(ll, airDigest, path);
    NSData *hit = fcCacheOn ? [nvmtl_memo() objectForKey:memoKey] : nil;
    if (hit) { gFcHit++; gFcMemoHit++; nvmtl_cache_touch(path); nvmtl_fc_note_lookup(); return hit; }   // keep the trimmer's age order (off the caller)
    if (fcCacheOn && path) hit = nvmtl_cache_read(path);
    if (fcCacheOn && path && !hit && nvmtl_fcwarm_join(path)) hit = nvmtl_cache_read(path);
    uint32_t magic = 0;
    if (hit.length >= 20 && hit.length % 4 == 0) {
        memcpy(&magic, hit.bytes, 4);
        if (magic == 0x07230203) {
            gFcHit++; nvmtl_cache_touch(path);
            [nvmtl_memo() setObject:hit forKey:memoKey cost:hit.length];
            nvlog("fc cache: HIT %s %lu B %.1f ms [%lu hit, %lu miss, %lu stored]", name.UTF8String,
                  (unsigned long)hit.length, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6,
                  gFcHit, gFcMiss, gFcStored);
            nvmtl_fc_note_lookup();
            return hit;
        }
    }
    gFcMiss++;
    NSData *result = nvmtl_translate_with_fc(ll, stage, idx, sizes, payloads, n, err);
    if (result && fcCacheOn && path && nvmtl_fcwarm_on() &&
        clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0 >= (uint64_t)NVMTL_FCWARM_RECORD_MS * 1000000ull)
        nvmtl_fcwarm_record(airDigest, json);
    if (result) {
        if (path) {
            NSData *stored = [result copy]; NSString *file = [path copy];
            nvmtl_cache_io(^{ if (nvmtl_cache_write(file, stored) == 0) gFcStored++; });
        }
        if (fcCacheOn && nvmtl_is_spirv(result)) [nvmtl_memo() setObject:result forKey:memoKey cost:result.length];
        nvlog("fc cache: miss %s %lu B %.1f ms [%lu hit, %lu miss, %lu stored]", name.UTF8String,
              (unsigned long)result.length, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6,
              gFcHit, gFcMiss, gFcStored);
    }
    nvmtl_fc_note_lookup();
    return result;
}

static int nvmtl_reflect_air(NVMTLFunction *function, NSString *ll, NSString *stage,
                             uint8_t **j, size_t *jn, char *e, size_t elen)
{
    NSDictionary *fc = function ? function->_fc : nil;
    BOOL have = [fc isKindOfClass:[NSDictionary class]] && fc.count > 0;
    if (have && xlate_refl_fc) {
        NSUInteger capacity = fc.count;
        __attribute__((objc_precise_lifetime)) NSMutableData *packed = nvmtl_fc_storage(capacity);
        uint32_t *idx = packed.mutableBytes, *sz = idx ? idx + capacity : NULL;
        const uint8_t **pay = sz ? (const uint8_t **)(sz + capacity) : NULL;
        uint8_t (*bytes)[16] = pay ? (uint8_t (*)[16])(pay + capacity) : NULL; size_t n = 0, dropped = 0;
        if (!packed) { snprintf(e, elen, "function constant reflection allocation failed"); return -1; }
        for (NSString *k in fc) {
            NSArray *a = fc[k];
            if (![k isKindOfClass:[NSString class]] ||
                ![a isKindOfClass:[NSArray class]] || a.count == 0 || a.count > 16) { dropped++; continue; }
            idx[n] = (uint32_t)k.integerValue; sz[n] = (uint32_t)a.count;
            for (NSUInteger m = 0; m < a.count; m++) bytes[n][m] = (uint8_t)[a[m] unsignedCharValue];
            pay[n] = bytes[n]; n++;
        }
        if (dropped) { snprintf(e, elen, "function constant reflection refused %lu malformed value(s)", (unsigned long)dropped); return -1; }
        if (n) {
            int rc = xlate_refl_fc(ll.UTF8String, stage.UTF8String, idx, sz, pay, n, j, jn, e, elen);
            nvlog("reflection: %s carried %zu function constant(s) into reflection (rc %d)",
                  function->_fname.UTF8String ?: "?", n, rc);
            return rc;
        }
    }
    if (have && !xlate_refl_fc)
        nvlog("reflection: %s carries %lu function constant(s) but this translator exports no "
              "nvmtl_reflect_fc - any binding slot derived from a function constant WILL reflect at 0",
              function->_fname.UTF8String ?: "?", (unsigned long)fc.count);
    return xlate_refl(ll.UTF8String, stage.UTF8String, j, jn, e, elen);
}

static _Atomic unsigned long gRfHit, gRfMiss, gRfStored;
static NSData *nvmtl_reflect_air_cached(NVMTLFunction *function, NSString *ll, NSString *stage, char *e, size_t elen)
{
    NVMTLAirDigest *airDigest = nvmtl_air_state(ll);
    NSDictionary *fc = (function && [function->_fc isKindOfClass:[NSDictionary class]]) ? function->_fc : @{};
    NSDictionary *config = @{ @"stage": stage ?: @"", @"fc": fc, @"fcx": @(xlate_refl_fc != NULL) };
    NSData *json = [NSJSONSerialization isValidJSONObject:config]
                 ? [NSJSONSerialization dataWithJSONObject:config options:NSJSONWritingSortedKeys error:NULL] : nil;
    NSString *path = (airDigest && json) ? nvmtl_spvcache_path_air(airDigest, json, @"refl1") : nil;
    NSString *memoKey = path ?: ((airDigest && json) ? nvmtl_spvcache_memo_key_air(airDigest, json, @"refl1") : nil);
    const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    const BOOL useCache = memoKey && !getenv("NVMTL_REFLCACHE_OFF") && !getenv("NVMTL_SPVCACHE_OFF");
    if (useCache) {
        NSData *mem = [nvmtl_memo() objectForKey:memoKey];
        if (mem) { ++gRfHit; nvmtl_cache_touch(path); return mem; }
        NSData *hit = nvmtl_cache_read(path);
        id obj = hit.length ? [NSJSONSerialization JSONObjectWithData:hit options:0 error:NULL] : nil;
        if ([obj isKindOfClass:[NSDictionary class]] && [(NSDictionary *)obj count]) {
            unsigned long hn = ++gRfHit; nvmtl_cache_touch(path);
            [nvmtl_memo() setObject:hit forKey:memoKey cost:hit.length];
            if (!(hn & (hn - 1))) nvlog("refl cache: HIT %s %lu B %.2f ms [%lu hit, %lu miss, %lu stored]", function ? function->_fname.UTF8String : "?",
                                        (unsigned long)hit.length, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6, hn, (unsigned long)gRfMiss, (unsigned long)gRfStored);
            return hit;
        }
    }
    unsigned long mn = ++gRfMiss;
    uint8_t *j = NULL; size_t jn = 0;
    if (nvmtl_reflect_air(function, ll, stage, &j, &jn, e, elen) != 0) return nil;
    NSData *d = [NSData dataWithBytes:j length:jn];
    if (xlate_free) xlate_free(j, jn);
    id obj = d.length ? [NSJSONSerialization JSONObjectWithData:d options:0 error:NULL] : nil;
    if (path && [obj isKindOfClass:[NSDictionary class]] && [(NSDictionary *)obj count]) {
        NSData *stored = [d copy]; NSString *file = [path copy];
        nvmtl_cache_io(^{ if (nvmtl_cache_write(file, stored) == 0) gRfStored++; });
    }
    if (useCache && [obj isKindOfClass:[NSDictionary class]] && [(NSDictionary *)obj count]) [nvmtl_memo() setObject:d forKey:memoKey cost:d.length];
    if (!(mn & (mn - 1))) nvlog("refl cache: miss %s %zu B %.2f ms [%lu hit, %lu miss, %lu stored]", function ? function->_fname.UTF8String : "?",
                                jn, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6, (unsigned long)gRfHit, mn, (unsigned long)gRfStored);
    return d;
}

static _Atomic unsigned long gLkHit, gLkMiss, gLkStored;
static NSData *nvmtl_translate_link_cached(NSString *ll, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                                           const uint8_t *const *payloads, size_t n, NSString **err)
{
    NSData *air = [ll dataUsingEncoding:NSUTF8StringEncoding];
    NSData *st = [(stage ?: @"") dataUsingEncoding:NSUTF8StringEncoding];
    size_t *ord = calloc(n ? n : 1, sizeof *ord);
    if (!air || !st || !ord || !nvmtl_xlate_ready()) { free(ord); return nvmtl_translate_with_fc(ll, stage, idx, sizes, payloads, n, err); }
    for (size_t k = 0; k < n; k++) ord[k] = k;
    for (size_t a = 1; a < n; a++) for (size_t b = a; b > 0 && idx[ord[b - 1]] > idx[ord[b]]; b--) { size_t x = ord[b]; ord[b] = ord[b - 1]; ord[b - 1] = x; }
    NSMutableData *key = [NSMutableData data]; uint64_t size = air.length, sl = st.length, nn = n;
    [key appendBytes:&size length:sizeof size]; [key appendData:air];
    [key appendBytes:&sl length:sizeof sl]; [key appendData:st]; [key appendBytes:&nn length:sizeof nn];
    for (size_t k = 0; k < n; k++) { size_t q = ord[k];
        [key appendBytes:&idx[q] length:sizeof idx[q]]; [key appendBytes:&sizes[q] length:sizeof sizes[q]];
        if (sizes[q] && payloads[q]) [key appendBytes:payloads[q] length:sizes[q]]; }
    free(ord);
    NSString *path = nvmtl_spvcache_path(key, @"lk1");
    const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    NSData *hit = (getenv("NVMTL_LINKCACHE_OFF") || getenv("NVMTL_FCCACHE_OFF")) ? nil : nvmtl_cache_read(path);
    uint32_t magic = 0;
    if (hit.length >= 20 && hit.length % 4 == 0) {
        memcpy(&magic, hit.bytes, 4);
        if (magic == 0x07230203) {
            unsigned long hn = ++gLkHit; nvmtl_cache_touch(path);
            if (!(hn & (hn - 1))) nvlog("link cache: HIT %lu B %.2f ms [%lu hit, %lu miss, %lu stored]", (unsigned long)hit.length,
                                        (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6, hn, (unsigned long)gLkMiss, (unsigned long)gLkStored);
            return hit;
        }
    }
    unsigned long mn = ++gLkMiss;
    NSData *result = nvmtl_translate_with_fc(ll, stage, idx, sizes, payloads, n, err);
    if (result && path) {
        NSData *stored = [result copy]; NSString *file = [path copy];
        nvmtl_cache_io(^{ if (nvmtl_cache_write(file, stored) == 0) gLkStored++; });
    }
    if (!(mn & (mn - 1))) nvlog("link cache: miss %lu B %.2f ms [%lu hit, %lu miss, %lu stored]", (unsigned long)result.length,
                                (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6, (unsigned long)gLkHit, mn, (unsigned long)gLkStored);
    return result;
}

NSDictionary *nvmtl_function_reflection(NVMTLFunction *function)
{
    if (!function) return nil;
    @synchronized (function) {
        if (function->_refl) return function->_refl.count ? function->_refl : nil;
        function->_refl = @{};
        if (!nvmtl_xlate_ready() || !xlate_refl) { nvlog("reflection: translator not ready — %s cannot be reflected", function->_fname.UTF8String ?: "?"); return nil; }
        NSString *ll = function->_air, *stage = function->_stage, *name = function->_fname;
        if (!ll || !stage.length) { nvlog("reflection: %s has no AIR — cannot be reflected", name.UTF8String ?: "?"); return nil; }
        char e[512] = { 0 };
        NSData *d = nvmtl_reflect_air_cached(function, ll, stage, e, sizeof e);
        if (!d) { nvlog("reflection for %s FAILED: %s", name.UTF8String, e); return nil; }
        size_t jn = d.length;
        NSDictionary *refl = [NSJSONSerialization JSONObjectWithData:d options:0 error:NULL];
        if ([refl isKindOfClass:[NSDictionary class]] && refl.count) function->_refl = refl;
        else nvlog("reflection for %s: JSON did not parse (%zu bytes)", name.UTF8String, jn);
        return function->_refl.count ? function->_refl : nil;
    }
}

static NSString *nvmtl_air_quoted_after(NSString *line, NSString *key, NSUInteger from) {
    NSString *k = [NSString stringWithFormat:@"!\"%@\", !\"", key];
    NSRange r = [line rangeOfString:k options:0 range:NSMakeRange(from, line.length - from)];
    if (r.location == NSNotFound) return nil;
    NSUInteger s = r.location + r.length;
    NSRange q = [line rangeOfString:@"\"" options:0 range:NSMakeRange(s, line.length - s)];
    return q.location == NSNotFound ? nil : [line substringWithRange:NSMakeRange(s, q.location - s)];
}
static NSInteger nvmtl_air_i32_after(NSString *line, NSString *key) {
    NSString *k = [NSString stringWithFormat:@"!\"%@\", i32 ", key];
    NSRange r = [line rangeOfString:k];
    if (r.location == NSNotFound) return -1;
    return [[line substringFromIndex:r.location + r.length] integerValue];
}
static NSString *nvmtl_air_node_after(NSString *line, NSString *key, NSUInteger from, NSUInteger *end) {
    NSString *k = [NSString stringWithFormat:@"!\"%@\", !", key];
    NSRange r = [line rangeOfString:k options:0 range:NSMakeRange(from, line.length - from)];
    if (r.location == NSNotFound) return nil;
    NSUInteger s = r.location + r.length, e = s;
    while (e < line.length && isdigit([line characterAtIndex:e])) e++;
    if (end) *end = e;
    return e > s ? [@"!" stringByAppendingString:[line substringWithRange:NSMakeRange(s, e - s)]] : nil;
}
static NSString *nvmtl_air_quoted_before(NSString *line, NSUInteger at, NSUInteger *start) {
    if (at < 4 || [line characterAtIndex:at - 1] != '"') return nil;
    NSRange q = [line rangeOfString:@"!\"" options:NSBackwardsSearch range:NSMakeRange(0, at - 1)];
    if (q.location == NSNotFound) return nil;
    if (start) *start = q.location;
    return [line substringWithRange:NSMakeRange(q.location + 2, at - 1 - (q.location + 2))];
}
static void nvmtl_air_walk_struct(NSDictionary *nodes, NSString *node, NSString *bufKey, NSMutableDictionary *members, NSMutableDictionary *types, NSMutableSet *seen) {
    if (!node || [seen containsObject:node]) return;
    [seen addObject:node];
    NSString *line = nodes[node]; if (!line) return;
    NSString *tag = @"!\"air.indirect_argument\", ";
    NSRange r = [line rangeOfString:tag];
    while (r.location != NSNotFound) {
        NSUInteger nameStart = 0; NSString *name = r.location >= 2 ? nvmtl_air_quoted_before(line, r.location - 2, &nameStart) : nil;
        NSString *type = (name && nameStart >= 2) ? nvmtl_air_quoted_before(line, nameStart - 2, NULL) : nil;
        NSUInteger after = r.location + r.length;
        NSInteger id_ = -1;
        if ([line rangeOfString:@"i32 " options:NSAnchoredSearch range:NSMakeRange(after, line.length - after)].location != NSNotFound) {
            id_ = [[line substringFromIndex:after + 4] integerValue];
        } else if (after < line.length && [line characterAtIndex:after] == '!') {
            NSUInteger e = after + 1; while (e < line.length && isdigit([line characterAtIndex:e])) e++;
            NSString *ref = [line substringWithRange:NSMakeRange(after, e - after)];
            NSString *argLine = nodes[ref];
            if (argLine) {
                id_ = nvmtl_air_i32_after(argLine, @"air.location_index");
                if (id_ < 0) id_ = [[argLine substringFromIndex:MIN(argLine.length, (NSUInteger)[argLine rangeOfString:@"!{i32 "].location + 6)] integerValue];
            }
        }
        if (name && id_ >= 0) {
            NSString *k = [NSString stringWithFormat:@"%@:%ld", bufKey, (long)id_];
            if (!members[k]) { members[k] = name; if (type) types[k] = type; }
        }
        r = [line rangeOfString:tag options:0 range:NSMakeRange(after, line.length - after)];
    }
}
static void nvmtl_air_plain_struct(NSDictionary *nodes, NSString *node, NSInteger loc, NSMutableDictionary *plain) {
    NSString *line = node ? nodes[node] : nil; if (!line || loc < 0) return;
    if ([line rangeOfString:@"!\"air.indirect_argument\""].location != NSNotFound) return;
    NSRange br = [line rangeOfString:@"!{"]; if (br.location == NSNotFound) return;
    NSMutableArray *tok = [NSMutableArray array]; NSMutableString *cur = [NSMutableString string]; BOOL q = NO;
    for (NSUInteger i = br.location + 2; i < line.length; i++) {
        unichar c = [line characterAtIndex:i];
        if (c == '"') q = !q;
        if (!q && (c == ',' || c == '}')) { NSString *t = [cur stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]]; if (t.length) [tok addObject:t]; [cur setString:@""]; if (c == '}') break; continue; }
        [cur appendFormat:@"%C", c];
    }
    NSMutableArray *ms = [NSMutableArray array];
    for (NSUInteger i = 0; i + 4 < tok.count; ) {
        NSString *a = tok[i], *b = tok[i + 1], *c = tok[i + 2], *t = tok[i + 3], *n = tok[i + 4];
        if ([a hasPrefix:@"i32 "] && [b hasPrefix:@"i32 "] && [c hasPrefix:@"i32 "] && [t hasPrefix:@"!\""] && [n hasPrefix:@"!\""] && t.length > 3 && n.length > 3) {
            [ms addObject:@{ @"off": @([[a substringFromIndex:4] integerValue]), @"size": @([[b substringFromIndex:4] integerValue]),
                             @"count": @([[c substringFromIndex:4] integerValue]),
                             @"type": [t substringWithRange:NSMakeRange(2, t.length - 3)], @"name": [n substringWithRange:NSMakeRange(2, n.length - 3)] }];
            i += 5;
        } else i++;
    }
    if (ms.count) plain[@(loc)] = ms;
}
NSDictionary *nvmtl_air_names(NVMTLFunction *fn)
{
    if (!fn) return nil;
    @synchronized (fn) {
        if (fn->_airNames) return fn->_airNames;
        NSMutableDictionary *args = [NSMutableDictionary new], *members = [NSMutableDictionary new], *types = [NSMutableDictionary new];
        NSMutableDictionary *plain = [NSMutableDictionary new];
        fn->_airNames = @{ @"args": args, @"members": members, @"types": types, @"plain": plain };
        NSString *ll = fn->_air, *fname = fn->_fname;
        if (!ll.length || !fname.length) return fn->_airNames;
        NSMutableDictionary *nodes = [NSMutableDictionary new];
        NSString *entryLine = nil; NSString *entryNeedle = [NSString stringWithFormat:@"@%@, ", fname];
        NSArray *lines = [ll componentsSeparatedByString:@"\n"];
        for (NSString *line in lines) {
            if (![line hasPrefix:@"!"]) continue;
            NSRange eq = [line rangeOfString:@" = !{"]; if (eq.location == NSNotFound) continue;
            nodes[[line substringToIndex:eq.location]] = line;
            if (!entryLine && [line rangeOfString:entryNeedle].location != NSNotFound) entryLine = line;
        }
        if (!entryLine) { nvlog("air names: no entry node for %s", fname.UTF8String); return fn->_airNames; }
        NSRange nr = [entryLine rangeOfString:entryNeedle];
        NSArray *erefs = [[entryLine substringFromIndex:nr.location + nr.length] componentsSeparatedByString:@", "];
        if (erefs.count < 2) { nvlog("air names: the entry node for %s has %lu reference(s) after the function, no argument list — "
            "nothing named", fname.UTF8String, (unsigned long)erefs.count); return fn->_airNames; }
        NSString *argsRef = [erefs[1] stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"} "]];
        NSString *argsLine = nodes[argsRef]; if (!argsLine) return fn->_airNames;
        NSRange br = [argsLine rangeOfString:@"!{"]; if (br.location == NSNotFound) return fn->_airNames;
        NSArray *refs = [[[argsLine substringFromIndex:br.location + 2] stringByReplacingOccurrencesOfString:@"}" withString:@""] componentsSeparatedByString:@", "];
        for (NSString *ref0 in refs) {
            NSString *ref = [ref0 stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
            NSString *line = nodes[ref]; if (!line) continue;
            NSString *kind = nil;
            if ([line rangeOfString:@"!\"air.buffer\""].location != NSNotFound || [line rangeOfString:@"!\"air.indirect_buffer\""].location != NSNotFound) kind = @"buffer";
            else if ([line rangeOfString:@"!\"air.texture\""].location != NSNotFound) kind = @"texture";
            else if ([line rangeOfString:@"!\"air.sampler\""].location != NSNotFound) kind = @"sampler";
            else continue;
            NSInteger loc = nvmtl_air_i32_after(line, @"air.location_index");
            NSString *name = nvmtl_air_quoted_after(line, @"air.arg_name", 0);
            if (loc < 0 || !name) continue;
            NSString *k = [NSString stringWithFormat:@"%@:%ld", kind, (long)loc];
            args[k] = name;
            NSString *tn = nvmtl_air_quoted_after(line, @"air.arg_type_name", 0); if (tn) types[k] = tn;
            if ([kind isEqualToString:@"buffer"]) {
                NSString *st = nvmtl_air_node_after(line, @"air.struct_type_info", 0, NULL);
                if (st) nvmtl_air_walk_struct(nodes, st, [NSString stringWithFormat:@"%ld", (long)loc], members, types, [NSMutableSet new]);
                if (st) nvmtl_air_plain_struct(nodes, st, loc, plain);
            }
        }
        NSUInteger np = 0; for (NSArray *pm in plain.allValues) np += pm.count;
        nvlog("air names: %s — %lu argument(s), %lu struct member(s) named, %lu plain-struct member(s)", fname.UTF8String, (unsigned long)args.count, (unsigned long)members.count, (unsigned long)np);
        return fn->_airNames;
    }
}

static char kNVMTLGLUniformIndex;
static uint32_t nvmtl_gl_uniform_index(id function)
{
    NVMTLFunction *fn = [function isKindOfClass:[NVMTLFunction class]] ? function : nil;
    if (!fn) return UINT32_MAX;
    NSNumber *inherited = objc_getAssociatedObject(fn, &kNVMTLGLUniformIndex);
    if (inherited) return inherited.unsignedIntValue;
    if (!fn->_air.length) return UINT32_MAX;
    NSString *expected = [fn->_stage isEqualToString:@"vertex"] ? @"vs_uniform_buffer"
        : [fn->_stage isEqualToString:@"fragment"] ? @"fs_uniform_buffer" : nil;
    if (!expected) return UINT32_MAX;
    NSDictionary *names = nvmtl_air_names(fn);
    uint32_t found = UINT32_MAX;
    for (NSString *key in names[@"args"]) {
        if (![key hasPrefix:@"buffer:"] || ![names[@"args"][key] isEqual:expected]
            || ![names[@"access"][key] isEqual:@"read"]) continue;
        NSUInteger slot = [[key substringFromIndex:7] integerValue];
        if (slot >= 32 || found != UINT32_MAX) return UINT32_MAX;
        for (NSDictionary *binding in nvmtl_function_reflection(fn)[@"bindings"])
            if ([binding[@"kind"] isEqual:@"Buffer"] && [binding[@"metal_index"] unsignedIntegerValue] == slot
                && [binding[@"address_space"] isEqual:@2] && [binding[@"access"] isEqual:@"ReadOnly"])
                found = (uint32_t)slot;
    }
    return found;
}
static void nvmtl_gl_uniform_inherit(id function, id source)
{
    // Sampler-specialized functions have SPIR-V but no AIR. Carry only the
    // already checked GL slot, without changing their reflection/linking path.
    uint32_t slot = nvmtl_gl_uniform_index(source);
    if (function && slot < 32)
        objc_setAssociatedObject(function, &kNVMTLGLUniformIndex, @(slot), OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

static NSUInteger nvmtl_air_max_tg_scan(NSString *ll, NSString *fname);
NSUInteger nvmtl_air_max_tg(NVMTLFunction *fn)
{
    NSString *ll = fn ? fn->_air : nil, *fname = fn ? fn->_fname : nil;
    if (!ll.length || !fname.length) return 0;
    // The scan walks the whole module; a pipeline state is made for the same function on every VideoToolbox transfer,
    // so the answer is kept on the module text by function name.
    NSMutableDictionary *memo = nil;
    if (ll.length >= 4096 && objc_getAssociatedObject(ll, NVMTL_AIRTEXT_KEY)) @synchronized (ll) {
        memo = objc_getAssociatedObject(ll, NVMTL_AIRTG_KEY);
        if (!memo) { memo = [NSMutableDictionary new]; objc_setAssociatedObject(ll, NVMTL_AIRTG_KEY, memo, OBJC_ASSOCIATION_RETAIN); }
        NSNumber *known = memo[fname];
        if (known) return known.unsignedIntegerValue;
    }
    const NSUInteger found = nvmtl_air_max_tg_scan(ll, fname);
    if (memo) @synchronized (ll) { memo[fname] = @(found); }
    return found;
}
static NSUInteger nvmtl_air_max_tg_scan(NSString *ll, NSString *fname)
{
    for (NSString *needle in @[[NSString stringWithFormat:@"@%@, ", fname], [NSString stringWithFormat:@"@\"%@\", ", fname]])
    for (NSRange r = NSMakeRange(0, ll.length);;) {
        NSRange m = [ll rangeOfString:needle options:NSLiteralSearch range:r];
        if (m.location == NSNotFound) break;
        NSRange ln = [ll lineRangeForRange:m];
        r = NSMakeRange(NSMaxRange(ln), ll.length - NSMaxRange(ln));
        if ([ll characterAtIndex:ln.location] != '!' || [ll rangeOfString:@" = !{" options:NSLiteralSearch range:ln].location == NSNotFound) continue;
        NSString *tail = [ll substringWithRange:NSMakeRange(NSMaxRange(m), NSMaxRange(ln) - NSMaxRange(m))];
        NSArray *refs = [[tail stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"}\r\n "]] componentsSeparatedByString:@", "];
        for (NSUInteger k = 2; k < refs.count; k++) {
            NSString *want = [NSString stringWithFormat:@"\n%@ = !{!\"air.max_work_group_size\", i32 ", refs[k]];
            NSRange a = [ll rangeOfString:want options:NSLiteralSearch];
            if (a.location != NSNotFound)
                return (NSUInteger)MAX(0, [[ll substringWithRange:NSMakeRange(NSMaxRange(a), MIN((NSUInteger)12, ll.length - NSMaxRange(a)))] integerValue]);
        }
        return 0;
    }
    return 0;
}

NSArray<NSDictionary *> *nvmtl_argbuf_fields(NVMTLFunction *function, NSUInteger ownerBufferIndex)
{
    NSDictionary *refl = nvmtl_function_reflection(function);
    if (!refl) return nil;
    NSString *name = function->_fname;
    NSArray *all = refl[@"argument_buffer_fields"];
    if (![all isKindOfClass:[NSArray class]]) return nil;
    NSMutableArray *mine = [NSMutableArray new];
    for (NSDictionary *f in all)
        if ([f[@"buffer_index"] unsignedLongValue] == ownerBufferIndex) [mine addObject:f];
    nvlog("argument encoder: %s buffer(%lu) has %lu field(s)", name.UTF8String, (unsigned long)ownerBufferIndex, (unsigned long)mine.count);
    return mine;
}

// The tokens of one AIR metadata tuple `!N = !{a, b, ...}`, commas inside quoted strings kept
// (`!"texture2d<float, sample>"` is one token).
static NSArray<NSString *> *nvmtl_air_tuple_tokens(NSString *line)
{
    NSRange br = line ? [line rangeOfString:@"!{"] : NSMakeRange(NSNotFound, 0);
    if (br.location == NSNotFound) return @[];
    NSMutableArray *tok = [NSMutableArray array]; NSMutableString *cur = [NSMutableString string]; BOOL q = NO;
    for (NSUInteger i = br.location + 2; i < line.length; i++) {
        unichar c = [line characterAtIndex:i];
        if (c == '"') q = !q;
        if (!q && (c == ',' || c == '}')) {
            NSString *t = [cur stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
            if (t.length) [tok addObject:t];
            [cur setString:@""];
            if (c == '}') break;
            continue;
        }
        [cur appendFormat:@"%C", c];
    }
    return tok;
}
// YES when a kernel cannot tell how its threads are grouped: every argument is a buffer outside threadgroup memory, a
// texture, a sampler, thread_position_in_grid or threads_per_grid, and the module has no threadgroup memory, barrier,
// SIMD-group or quad operation and no imageblock. The dispatch may then run the same threads in smaller groups.
// Anything the scan does not recognise answers NO, which only costs the reshape.
static BOOL nvmtl_air_grid_only_scan(NSString *ll, NSString *fname)
{
    if (!ll.length || !fname.length) return NO;
    for (NSString *bad in @[@"addrspace(3)", @"@air.wg.barrier", @"@air.simdgroup", @"@air.simd_", @"@air.quad_",
                            @"imageblock", @"@air.atomic.local", @"air.visible_function_table", @"air.intersection_function_table"])
        if ([ll rangeOfString:bad options:NSLiteralSearch].location != NSNotFound) return NO;
    NSArray *allowed = @[@"!\"air.buffer\"", @"!\"air.texture\"", @"!\"air.sampler\"", @"!\"air.thread_position_in_grid\"",
                         @"!\"air.threads_per_grid\""];
    for (NSString *needle in @[[NSString stringWithFormat:@"@%@, ", fname], [NSString stringWithFormat:@"@\"%@\", ", fname]])
    for (NSRange r = NSMakeRange(0, ll.length);;) {
        NSRange m = [ll rangeOfString:needle options:NSLiteralSearch range:r];
        if (m.location == NSNotFound) break;
        NSRange ln = [ll lineRangeForRange:m];
        r = NSMakeRange(NSMaxRange(ln), ll.length - NSMaxRange(ln));
        if ([ll characterAtIndex:ln.location] != '!' || [ll rangeOfString:@" = !{" options:NSLiteralSearch range:ln].location == NSNotFound) continue;
        // `!K = !{ptr @fname, !outputs, !args, ...}`: the third token is the argument list
        NSArray<NSString *> *kt = nvmtl_air_tuple_tokens([ll substringWithRange:ln]);
        if (kt.count < 3 || ![kt[2] hasPrefix:@"!"]) return NO;
        NSRange al = [ll rangeOfString:[NSString stringWithFormat:@"\n%@ = !{", kt[2]] options:NSLiteralSearch];
        if (al.location == NSNotFound) return NO;
        for (NSString *arg in nvmtl_air_tuple_tokens([ll substringWithRange:[ll lineRangeForRange:NSMakeRange(al.location + 1, 1)]])) {
            NSRange an = [ll rangeOfString:[NSString stringWithFormat:@"\n%@ = !{", arg] options:NSLiteralSearch];
            if (an.location == NSNotFound) return NO;
            NSArray<NSString *> *at = nvmtl_air_tuple_tokens([ll substringWithRange:[ll lineRangeForRange:NSMakeRange(an.location + 1, 1)]]);
            if (at.count < 2 || ![allowed containsObject:at[1]]) return NO;
            NSUInteger as = [at indexOfObject:@"!\"air.address_space\""];
            if (as != NSNotFound && as + 1 < at.count && [at[as + 1] isEqualToString:@"i32 3"]) return NO;
        }
        return YES;
    }
    return NO;
}
// The answer for one kernel of one AIR text never changes, but it is asked once per pipeline state, and VideoToolbox's
// pixel-transfer session builds a new pipeline for every frame it converts: Photo Booth's camera thread spent 854 of its
// samples in this scan (issue #568), each scan a walk over the whole module text. The answer is kept per AIR string
// object (weak key, so it goes with the text) and kernel name.
static NSMapTable<NSString *, NSMutableDictionary<NSString *, NSNumber *> *> *g_gridonly_memo;
static os_unfair_lock g_gridonly_memo_lock = OS_UNFAIR_LOCK_INIT;
BOOL nvmtl_air_grid_only(NVMTLFunction *fn)
{
    if (!fn) return NO;
    NSString *air = fn->_air, *name = fn->_fname;
    if (!air.length || !name.length) return nvmtl_air_grid_only_scan(air, name);
    os_unfair_lock_lock(&g_gridonly_memo_lock);
    if (!g_gridonly_memo) g_gridonly_memo = [NSMapTable weakToStrongObjectsMapTable];
    NSNumber *hit = [g_gridonly_memo objectForKey:air][name];
    os_unfair_lock_unlock(&g_gridonly_memo_lock);
    if (hit) return hit.boolValue;
    const BOOL r = nvmtl_air_grid_only_scan(air, name);
    os_unfair_lock_lock(&g_gridonly_memo_lock);
    NSMutableDictionary *per = [g_gridonly_memo objectForKey:air];
    if (!per) { per = [NSMutableDictionary new]; [g_gridonly_memo setObject:per forKey:air]; }
    per[[name copy]] = @(r);
    os_unfair_lock_unlock(&g_gridonly_memo_lock);
    return r;
}

// One `air.struct_type_info` tuple: members of `i32 offset, i32 size, i32 count, !"type", !"name",
// !"air.indirect_argument", leaf`. A leaf is either a node with the member's `air.location_index` (its [[id]] inside the
// struct), or, for a nested struct - whose member starts with `!"air.struct_type_info", !child` - `i32 id`, the
// struct's own [[id]]. An encoder addresses a nested member by its struct's id plus its id inside the struct, at the
// struct's offset plus its own (Metal numbers ImageIO's xdr::colorTransformIn trc.lut 11 at byte 64 this way).
static void nvmtl_air_argbuf_walk(NSDictionary *nodes, NSString *node, NSUInteger baseId, NSUInteger baseOff, NSUInteger depth,
                                  NSUInteger bufferIndex, NSMutableArray *out)
{
    if (depth > 16) return;
    NSArray<NSString *> *tok = nvmtl_air_tuple_tokens(nodes[node]);
    for (NSUInteger i = 0; i < tok.count; ) {
        NSString *child = nil; NSUInteger j = i;
        if ([tok[j] isEqualToString:@"!\"air.struct_type_info\""] && j + 1 < tok.count) { child = tok[j + 1]; j += 2; }
        if (j + 6 >= tok.count || ![tok[j] hasPrefix:@"i32 "] || ![tok[j + 1] hasPrefix:@"i32 "] || ![tok[j + 2] hasPrefix:@"i32 "]
            || ![tok[j + 3] hasPrefix:@"!\""] || ![tok[j + 4] hasPrefix:@"!\""] || tok[j + 4].length < 3 || ![tok[j + 4] hasSuffix:@"\""]
            || ![tok[j + 5] isEqualToString:@"!\"air.indirect_argument\""]) {
            i++; continue;
        }
        const NSUInteger off = (NSUInteger)[[tok[j] substringFromIndex:4] integerValue], size = (NSUInteger)[[tok[j + 1] substringFromIndex:4] integerValue];
        NSString *ref = tok[j + 6], *name = [tok[j + 4] substringWithRange:NSMakeRange(2, tok[j + 4].length - 3)];
        if (child) {
            if ([ref hasPrefix:@"i32 "]) nvmtl_air_argbuf_walk(nodes, child, baseId + (NSUInteger)[[ref substringFromIndex:4] integerValue],
                                                               baseOff + off, depth + 1, bufferIndex, out);
        } else if ([ref hasPrefix:@"!"]) {
            NSString *leaf = nodes[ref]; NSInteger rel = leaf ? nvmtl_air_i32_after(leaf, @"air.location_index") : -1;
            NSString *kind = [leaf rangeOfString:@"!\"air.indirect_constant\""].location != NSNotFound ? @"constant"
                           : [leaf rangeOfString:@"!\"air.texture\""].location != NSNotFound ? @"texture"
                           : [leaf rangeOfString:@"!\"air.sampler\""].location != NSNotFound ? @"sampler" : @"other";
            if (rel >= 0) {
                NSMutableDictionary *fld = [@{ @"buffer_index": @(bufferIndex), @"argument_index": @(baseId + (NSUInteger)rel), @"field_offset": @(baseOff + off),
                                               @"size": @(size), @"kind": kind, @"name": name } mutableCopy];
                // a pointer to another argument buffer (`constant Inner *p [[id(n)]]`): its own struct's members, so that
                // -newArgumentEncoderForBufferAtIndex: can encode the buffer it points at
                if (leaf && [leaf rangeOfString:@"!\"air.indirect_buffer\""].location != NSNotFound) {
                    NSString *inner = nvmtl_air_node_after(leaf, @"air.struct_type_info", 0, NULL);
                    if (inner) {
                        NSMutableArray *nested = [NSMutableArray array];
                        nvmtl_air_argbuf_walk(nodes, inner, 0, 0, depth + 1, bufferIndex, nested);
                        fld[@"nested_fields"] = nested;
                        fld[@"nested_size"] = @(MAX(0, nvmtl_air_i32_after(leaf, @"air.arg_type_size")));
                        fld[@"nested_align"] = @(MAX(0, nvmtl_air_i32_after(leaf, @"air.arg_type_align_size")));
                    }
                }
                [out addObject:fld];
            }
        }
        i = j + 7;
    }
}
// Every member of the argument buffer at [[buffer(bufferIndex)]] as the function's AIR declares it, nested structs
// flattened, and the buffer's size and alignment. The translator's argument_buffer_fields carry only the resources
// a shader reaches; Metal's encoder also covers the constants and the nested structs (ImageIO's HDR converter sizes
// and fills xdr::colorTransformIn, which has no top-level resource, through it).
NSArray<NSDictionary *> *nvmtl_air_argbuf_layout(NVMTLFunction *fn, NSUInteger bufferIndex, NSUInteger *size, NSUInteger *align)
{
    if (size) *size = 0;
    if (align) *align = 0;
    NSString *ll = fn ? fn->_air : nil, *fname = fn ? fn->_fname : nil;
    if (!ll.length || !fname.length) return @[];
    NSMutableDictionary *nodes = [NSMutableDictionary new]; NSString *entryLine = nil; NSRange er = NSMakeRange(NSNotFound, 0);
    NSArray *needles = @[[NSString stringWithFormat:@"@%@, ", fname], [NSString stringWithFormat:@"@\"%@\", ", fname]];
    for (NSString *line in [ll componentsSeparatedByString:@"\n"]) {
        if (![line hasPrefix:@"!"]) continue;
        NSRange eq = [line rangeOfString:@" = !{"]; if (eq.location == NSNotFound) continue;
        nodes[[line substringToIndex:eq.location]] = line;
        if (!entryLine && [line rangeOfString:@"air."].location == NSNotFound)
            for (NSString *needle in needles) { NSRange r = [line rangeOfString:needle]; if (r.location != NSNotFound) { entryLine = line; er = r; break; } }
    }
    if (!entryLine) return @[];
    NSArray *erefs = [[entryLine substringFromIndex:NSMaxRange(er)] componentsSeparatedByString:@", "];
    if (erefs.count < 2) return @[];
    NSString *argsRef = [erefs[1] stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"} "]];
    NSMutableArray *out = [NSMutableArray array];
    for (NSString *ref in nvmtl_air_tuple_tokens(nodes[argsRef])) {
        NSString *line = nodes[ref];
        if ([line rangeOfString:@"!\"air.indirect_buffer\""].location == NSNotFound) continue;
        if (nvmtl_air_i32_after(line, @"air.location_index") != (NSInteger)bufferIndex) continue;
        if (size) *size = (NSUInteger)MAX(0, nvmtl_air_i32_after(line, @"air.arg_type_size"));
        if (align) *align = (NSUInteger)MAX(0, nvmtl_air_i32_after(line, @"air.arg_type_align_size"));
        NSString *st = nvmtl_air_node_after(line, @"air.struct_type_info", 0, NULL);
        if (st) nvmtl_air_argbuf_walk(nodes, st, 0, 0, 0, bufferIndex, out);
        break;
    }
    return out;
}

NSArray<NSDictionary *> *nvmtl_embedded_bindings(NVMTLFunction *function)
{
    if (!function) return nil;
    @synchronized (function) {
        if (function->_embedded) return function->_embedded;
        const char *who = function->_fname.UTF8String ?: "?";
        NSString *ll = function->_air, *stage = function->_stage;
        if (!ll || !stage.length) {
            nvlog("embedded: %s has no AIR to reflect — its argument-buffer resources CANNOT be resolved", who);
            function->_embedded = @[];
            return function->_embedded;
        }
        uint8_t *j = NULL; size_t jn = 0; char e[512] = { 0 };
        if (!nvmtl_xlate_ready() || !xlate_refl) {
            nvlog("embedded: no reflection in this translator — %s's argument-buffer resources CANNOT be resolved", who);
            function->_embedded = @[];
            return function->_embedded;
        }
        (void)j; (void)jn;
        NSData *d = nvmtl_reflect_air_cached(function, ll, stage, e, sizeof e);
        if (!d) {
            nvlog("embedded: reflection for %s FAILED: %s — resources NOT resolved", who, e);
            function->_embedded = @[];
            return function->_embedded;
        }
        NSDictionary *refl = [NSJSONSerialization JSONObjectWithData:d options:0 error:NULL];
        NSArray *all = refl[@"bindings"];
        NSMutableArray *mine = [NSMutableArray new];
        if ([all isKindOfClass:[NSArray class]]) {
            for (NSDictionary *b in all) {
                if (![b isKindOfClass:[NSDictionary class]]) continue;
                NSString *kind = b[@"kind"];
                if ([kind isEqualToString:@"EmbeddedArgBufferTexture"] ||
                    [kind isEqualToString:@"EmbeddedArgBufferSampler"]) [mine addObject:b];
            }
        } else {
            nvlog("embedded: reflection for %s has no bindings array — resources NOT resolved", who);
        }
        if (mine.count) nvlog("embedded: %s reaches %lu resource(s) through argument buffers", who, (unsigned long)mine.count);
        function->_embedded = mine;
        return function->_embedded;
    }
}

NSData *nvmtl_translate_variant_uncached(NVMTLFunction *fn, NSDictionary *states, NSDictionary *tgLen, NSString **err) {
    if (!states.count && !tgLen.count && !g_translate_variant_forced) return fn->_spirv;
    if (!fn->_air || !nvmtl_xlate_ready()) { if(err)*err=@"specialization has no AIR or translator";return nil; }
    if (states.count && !xlate_sampler) { if(err)*err=@"sampler specialization has no translator";return nil; }
    if (tgLen.count && !xlate_tglen) { if(err)*err=@"this translator cannot size threadgroup memory";return nil; }
    uint32_t lens[32] = {0}; uint32_t high = 0;
    for (NSString *k in tgLen) {
        NSInteger idx = k.integerValue; NSUInteger len = [tgLen[k] unsignedIntegerValue];
        if (idx < 0 || idx >= 32) { nvlog("threadgroup index %ld is outside 0..31 — IGNORED", (long)idx); continue; }
        lens[idx] = (uint32_t)len; if ((uint32_t)idx + 1 > high) high = (uint32_t)idx + 1;
    }
    if (high && xlate_tglen) xlate_tglen(lens, high);
    NSData *data = nil; uint8_t *out=NULL; size_t len=0; char e[1024]={0};
    if (states.count) {
        NSData *json=[NSJSONSerialization dataWithJSONObject:@{@"samplers":states,@"constants":fn->_fc ?: @{}} options:NSJSONWritingSortedKeys error:NULL];
        NSString *s=[[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
        if(xlate_sampler(fn->_air.UTF8String,fn->_stage.UTF8String,s.UTF8String,&out,&len,e,sizeof e)) { if(err)*err=@(e); }
        else { data=[NSData dataWithBytes:out length:len]; xlate_free(out,len); }
    } else if (fn->_fc.count) {
        if (!xlate_fc) { if(err)*err=@"this translator has no nvmtl_translate_fc \u2014 rebuild nvmtl_translate"; return nil; }
        NSUInteger capacity = fn->_fc.count;
        __attribute__((objc_precise_lifetime)) NSMutableData *packed = nvmtl_fc_storage(capacity);
        uint32_t *idx = packed.mutableBytes, *sz = idx ? idx + capacity : NULL;
        const uint8_t **pay = sz ? (const uint8_t **)(sz + capacity) : NULL;
        uint8_t (*bytes)[16] = pay ? (uint8_t (*)[16])(pay + capacity) : NULL; size_t nfc = 0;
        if (!packed) { if(err)*err=@"function constant variant allocation failed"; if (high && xlate_tglen) xlate_tglen(NULL, 0); return nil; }
        for (NSString *k in fn->_fc) {
            NSArray *a = fn->_fc[k];
            if (![a isKindOfClass:[NSArray class]] || a.count == 0 || a.count > 16) {
                if(err)*err=[NSString stringWithFormat:@"function constant %@ cannot be carried into the threadgroup variant", k];
                if (high && xlate_tglen) xlate_tglen(NULL, 0);
                return nil;
            }
            idx[nfc] = (uint32_t)k.integerValue; sz[nfc] = (uint32_t)a.count;
            for (NSUInteger j = 0; j < a.count; j++) bytes[nfc][j] = (uint8_t)[a[j] unsignedCharValue];
            pay[nfc] = bytes[nfc]; nfc++;
            if (getenv("NVMTL_ZPROBE")) {
                static const char *FH = "0123456789abcdef";
                char fx[40]; size_t fn2 = 0;
                for (uint32_t q = 0; q < sz[nfc-1] && q < 16; q++) {
                    fx[fn2++] = FH[bytes[nfc-1][q] >> 4]; fx[fn2++] = FH[bytes[nfc-1][q] & 15];
                }
                fx[fn2] = 0;
                nvlog("ZFC idx %-4u size %-2u bytes %s", idx[nfc-1], sz[nfc-1], fx);
            }
        }
        if(xlate_fc(fn->_air.UTF8String,fn->_stage.UTF8String,idx,sz,pay,nfc,&out,&len,e,sizeof e)) { if(err)*err=@(e); }
        else { data=[NSData dataWithBytes:out length:len]; xlate_free(out,len);
               nvlog("threadgroup variant: %s re-specialized with %zu constant(s) -> %lu B",
                     fn->_fname.UTF8String, nfc, (unsigned long)data.length); }
    } else {
        if(xlate(fn->_air.UTF8String,fn->_stage.UTF8String,&out,&len,e,sizeof e)) { if(err)*err=@(e); }
        else { data=[NSData dataWithBytes:out length:len]; xlate_free(out,len); }
    }
    if (high && xlate_tglen) xlate_tglen(NULL, 0);
    return data;
}

NSData *nvmtl_translate_variant(NVMTLFunction *fn, NSDictionary *states, NSDictionary *tgLen, NSString **err) {
    if (!states.count && !tgLen.count) return fn->_spirv;
    if (!fn->_air || !nvmtl_xlate_ready()) return nvmtl_translate_variant_uncached(fn,states,tgLen,err);
    NSDictionary *config=@{@"stage":fn->_stage ?: @"",@"entry":fn->_fname ?: @"",
        @"constants":fn->_fc ?: @{},@"samplers":states ?: @{},@"threadgroup":tgLen ?: @{}};
    NSData *json=[NSJSONSerialization dataWithJSONObject:config options:NSJSONWritingSortedKeys error:NULL];
    NVMTLAirDigest *airDigest=nvmtl_air_state(fn->_air);
    if(!json || !airDigest)return nvmtl_translate_variant_uncached(fn,states,tgLen,err);
    NSString *path=nvmtl_spvcache_path_air(airDigest,json,@"variant1");
    NSString *memoKey=path ?: nvmtl_spvcache_memo_key_air(airDigest,json,@"variant1");   // no cache folder: the memo still serves this process
    NSData *mem=[nvmtl_memo() objectForKey:memoKey];
    if(mem){nvmtl_cache_touch(path);return mem;}
    NSData *hit=nvmtl_cache_read(path);uint32_t magic=0;
    if(hit.length>=20 && hit.length%4==0){memcpy(&magic,hit.bytes,4);if(magic==0x07230203){[nvmtl_memo() setObject:hit forKey:memoKey cost:hit.length];nvlog("variant cache: hit %s",fn->_fname.UTF8String);return hit;}}
    NSData *result=nvmtl_translate_variant_uncached(fn,states,tgLen,err);
    if(result){if(nvmtl_is_spirv(result))[nvmtl_memo() setObject:result forKey:memoKey cost:result.length];NSData *keep=[result copy];NSString *file=[path copy],*entry=[fn->_fname copy];
        nvmtl_cache_io(^{BOOL stored=file&&nvmtl_cache_write(file,keep)==0;nvlog("variant cache: miss %s stored=%d",entry.UTF8String,stored);});}
    return result;
}

// A tile function's AIR rewritten into a compute kernel over the colour attachments (tile_rewrite in the translator).
NSString *nvmtl_tile_rewrite(NSString *ll, uint32_t tw, uint32_t th, uint32_t samples, NSString **err) {
    if (!nvmtl_xlate_ready() || !xlate_tile_rewrite) { if (err) *err = @"this translator cannot rewrite tile functions"; return nil; }
    uint8_t *out = NULL; size_t len = 0; char e[512] = {0};
    if (xlate_tile_rewrite(ll.UTF8String, tw, th, samples, &out, &len, e, sizeof e)) { if (err) *err = @(e); return nil; }
    NSString *text = [[NSString alloc] initWithBytes:out length:len encoding:NSUTF8StringEncoding]; xlate_free(out, len);
    return text;
}
// a8: Metal A8Unorm colour attachments (bit per location); the variant writes the output's alpha into the R8 lane.
NSData *nvmtl_translate_samplers_amp(NVMTLFunction *fn, NSDictionary *states, uint32_t a8, uint32_t amp, NSString **err);
NSData *nvmtl_translate_samplers(NVMTLFunction *fn, NSDictionary *states, uint32_t a8, NSString **err) {
    return nvmtl_translate_samplers_amp(fn, states, a8, 0, err);
}
// amp: the vertex amplification count of the pipeline (2 or more makes a multiview variant; 0 and 1 are the ordinary shader).
NSData *nvmtl_translate_samplers_amp(NVMTLFunction *fn, NSDictionary *states, uint32_t a8, uint32_t amp, NSString **err) {
    if (amp < 2) amp = 0;
    if (!states.count && !a8 && !amp) return fn->_spirv;
    if (!fn->_air && !states.count && !amp) return fn->_spirv;   // already a specialised variant (no AIR kept): nothing more to apply
    if (!fn->_air || !nvmtl_xlate_ready() || !xlate_sampler) { if(err)*err=@"sampler specialization has no AIR or translator";return nil; }
    NSMutableDictionary *jd=[@{@"samplers":states ?: @{},@"constants":fn->_fc ?: @{},@"a8_targets":@(a8)} mutableCopy];
    if (amp) jd[@"vertex_amplification"]=@(amp);   // part of the key text below, so a variant never shares a cache entry with the plain shader
    NSData *json=[NSJSONSerialization dataWithJSONObject:jd options:NSJSONWritingSortedKeys error:NULL];
    NSString *s=[[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
    // The same variant is asked for whenever a pipeline state is made again with these shaders and samplers (RenderBox
    // makes one for each icon it draws, in a fresh process per app). Kept like the compute variants.
    NSData *tail=[NSJSONSerialization dataWithJSONObject:@{@"stage":fn->_stage ?: @"",@"entry":fn->_fname ?: @"",@"key":s ?: @""} options:NSJSONWritingSortedKeys error:NULL];
    NVMTLAirDigest *airDigest=nvmtl_air_state(fn->_air);
    NSString *path=(airDigest && tail)?nvmtl_spvcache_path_air(airDigest,tail,@"sampler1"):nil;
    NSString *memoKey=path ?: ((airDigest && tail)?nvmtl_spvcache_memo_key_air(airDigest,tail,@"sampler1"):nil);
    if(memoKey){
        NSData *mem=[nvmtl_memo() objectForKey:memoKey];
        if(mem){nvmtl_cache_touch(path);return mem;}
        NSData *hit=nvmtl_cache_read(path);
        if(nvmtl_is_spirv(hit)){[nvmtl_memo() setObject:hit forKey:memoKey cost:hit.length];return hit;}
    }
    uint8_t *out=NULL;size_t len=0;char e[1024]={0};
    if(xlate_sampler(fn->_air.UTF8String,fn->_stage.UTF8String,s.UTF8String,&out,&len,e,sizeof e)) {if(err)*err=@(e);return nil;}
    NSData *data=[NSData dataWithBytes:out length:len];xlate_free(out,len);
    if(memoKey && nvmtl_is_spirv(data)){[nvmtl_memo() setObject:data forKey:memoKey cost:data.length];if(path){NSData *keep=[data copy];NSString *file=[path copy];nvmtl_cache_io(^{(void)nvmtl_cache_write(file,keep);});}}
    return data;
}

#pragma mark - batch 32: visible-function linking
static NSError *nvmtl_link_err(NSString *m) {
    /* "FAILED" keeps this line in the release syslog (nvmtl_log_failure): a refused link makes the caller's
     * pipeline nil, and RenderBox only reports that as a <private> "precondition failure". */
    nvlog("link FAILED: %s", m.UTF8String);
    return [NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey: m}];
}

/* MTLFunctionDescriptor.specializedName renames the function it creates, and a linked caller then refers
 * to it by that new name. RenderBox does exactly this for every custom shader: it asks for IconRendering's
 * glassHighlight_v1 / glow_v1 / sdfFill_v1 / shapeAwareGradientMask_v1 / clampToEdges_v1 (and its own
 * distanceGradient_v1 ...) with specializedName "custom_fn" and links it as a privateFunction into
 * primitive_/accumulator_/filter_custom_fragment and custom_effect_fragment, whose AIR references
 * !"custom_fn". The AIR we keep still defines @glassHighlight_v1, and the translator's validate_linkage()
 * refuses any symbol its module does not define ("linked module does not define authored visible function
 * reference \"custom_fn\""), so every such pipeline failed and the icon effect was never drawn.
 * These helpers mirror linked_functions.rs llvm_global()/module_defines(). */
static BOOL nvmtl_llvm_plain_char(unsigned char c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '.' || c == '$' || c == '-';
}
static NSString *nvmtl_llvm_global_name(NSString *sym) {
    const char *s = sym.UTF8String;
    if (!s || !*s) return nil;
    BOOL plain = YES;
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '\n' || *p == '\r') return nil;
        if (!nvmtl_llvm_plain_char(*p)) plain = NO;
    }
    if (plain) return [@"@" stringByAppendingString:sym];
    NSString *q = [[sym stringByReplacingOccurrencesOfString:@"\\" withString:@"\\5C"] stringByReplacingOccurrencesOfString:@"\"" withString:@"\\22"];
    return [NSString stringWithFormat:@"@\"%@\"", q];
}
static BOOL nvmtl_air_defines(NSString *ll, NSString *sym) {
    NSString *g = nvmtl_llvm_global_name(sym);
    if (!g || !ll.length) return NO;
    NSString *call = [g stringByAppendingString:@"("];
    __block BOOL found = NO;
    [ll enumerateLinesUsingBlock:^(NSString *line, BOOL *stop) {
        NSUInteger i = 0, n = line.length;
        while (i < n && ([line characterAtIndex:i] == ' ' || [line characterAtIndex:i] == '\t')) i++;
        if (n - i > 7 && [line compare:@"define " options:NSLiteralSearch range:NSMakeRange(i, 7)] == NSOrderedSame &&
            [line rangeOfString:call options:NSLiteralSearch].location != NSNotFound) { found = YES; *stop = YES; }
    }];
    return found;
}
/* Every whole-token use of global `from` (definition, calls, metadata such as !air.visible) becomes `to`.
 * nil when `to` is already a global of this module (nothing is renamed then). */
static NSString *nvmtl_air_rename_global(NSString *ll, NSString *from, NSString *to) {
    NSString *gf = nvmtl_llvm_global_name(from), *gt = nvmtl_llvm_global_name(to);
    if (!gf || !gt || !ll.length) return nil;
    BOOL quoted = [gf hasPrefix:@"@\""];
    NSMutableString *outs = [NSMutableString stringWithCapacity:ll.length + 64];
    NSUInteger pos = 0, n = ll.length, hits = 0;
    for (;;) {
        NSRange r = [ll rangeOfString:gf options:NSLiteralSearch range:NSMakeRange(pos, n - pos)];
        if (r.location == NSNotFound) break;
        NSUInteger end = NSMaxRange(r);
        unichar next = end < n ? [ll characterAtIndex:end] : 0;
        BOOL whole = quoted || end >= n || next >= 128 || !nvmtl_llvm_plain_char((unsigned char)next);
        [outs appendString:[ll substringWithRange:NSMakeRange(pos, r.location - pos)]];
        [outs appendString:whole ? gt : gf];
        if (whole) hits++;
        pos = end;
    }
    if (!hits) return nil;
    [outs appendString:[ll substringFromIndex:pos]];
    if (nvmtl_air_defines(ll, to)) return nil;
    return outs;
}
/* The AIR a linked function contributes, under the name its callers use. */
static NSString *nvmtl_link_candidate_air(NVMTLFunction *cf, NSString **nameOut) {
    NSString *air = cf->_air, *name = cf->_fname;
    if (cf->_specName.length && cf->_fname.length && ![cf->_specName isEqualToString:cf->_fname]) {
        if (nvmtl_air_defines(air, cf->_specName)) name = cf->_specName;
        else {
            NSString *renamed = nvmtl_air_defines(air, cf->_fname) ? nvmtl_air_rename_global(air, cf->_fname, cf->_specName) : nil;
            if (renamed) { air = renamed; name = cf->_specName;
                nvlog("link: \"%s\" renamed to its specializedName \"%s\"", cf->_fname.UTF8String, cf->_specName.UTF8String); }
            else nvlog("link: specializedName \"%s\" of \"%s\" could not be applied - FAILED to rename its AIR", cf->_specName.UTF8String, cf->_fname.UTF8String);
        }
    }
    if (nameOut) *nameOut = name;
    return air;
}
// A compute pipeline with a stage-input descriptor reads its [[stage_in]] attributes from the application's buffer by that
// descriptor's strides, offsets and formats, so its kernel is translated again with the layout (the kernel's own SPIR-V reads
// each member as an array of its own type at its natural size). A kernel with no stage-in attribute, or a descriptor that
// names none, keeps the function it has. NVMTL_NO_KERNEL_STAGE_IN=1 turns this off. A layout the translator cannot carry
// (a format or step function outside its table, an unaligned stride) fails the pipeline with the reason instead of
// running it with the wrong strides.
static NSError *nvmtl_stagein_err(NSString *m) {
    nvlog("%s", m.UTF8String);
    return [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorCompileFailure userInfo:@{NSLocalizedDescriptionKey: m}];
}
static NSString *nvmtl_stage_in_words(MTLStageInputOutputDescriptor *sd, NSMutableData *words) {
    uint32_t a[1 + 16 * 4 + 1 + 31 * 4]; uint32_t n = 1, na = 0; uint32_t bufs[31]; uint32_t nb = 0;
    for (NSUInteger i = 0; i < 16; i++) {
        MTLAttributeDescriptor *at = [sd.attributes objectAtIndexedSubscript:i];
        if (at.format == MTLAttributeFormatInvalid) continue;
        if (at.bufferIndex >= 31) return [NSString stringWithFormat:@"stage-in attribute %lu uses buffer %lu", (unsigned long)i, (unsigned long)at.bufferIndex];
        a[n++] = (uint32_t)i; a[n++] = (uint32_t)at.bufferIndex; a[n++] = (uint32_t)at.offset; a[n++] = (uint32_t)at.format; na++;
        BOOL seen = NO; for (uint32_t k = 0; k < nb; k++) if (bufs[k] == at.bufferIndex) seen = YES;
        if (!seen) bufs[nb++] = (uint32_t)at.bufferIndex;
    }
    if (!na) return nil;
    a[0] = na;
    a[n++] = nb;
    for (uint32_t k = 0; k < nb; k++) {
        MTLBufferLayoutDescriptor *ly = [sd.layouts objectAtIndexedSubscript:bufs[k]];
        a[n++] = bufs[k]; a[n++] = (uint32_t)ly.stride; a[n++] = (uint32_t)ly.stepFunction; a[n++] = (uint32_t)ly.stepRate;
    }
    [words appendBytes:a length:n * sizeof a[0]];
    return nil;
}

NVMTLFunction *nvmtl_kernel_with_stage_in(NVMTLFunction *fn, MTLStageInputOutputDescriptor *sd, NSError **err)
{
    if (err) *err = nil;
    if (!sd || !fn || getenv("NVMTL_NO_KERNEL_STAGE_IN")) return fn;
    if (!fn->_air.length || fn->_needsLink) return fn;     // linked kernels were translated by the linker; not carried here
    NSMutableData *words = [NSMutableData new];
    NSString *bad = nvmtl_stage_in_words(sd, words);
    if (bad) { if (err) *err = nvmtl_stagein_err([NSString stringWithFormat:@"compute stage-in: %@", bad]); return nil; }
    if (!words.length) return fn;
    if (!nvmtl_xlate_ready() || !xlate_stagein) {
        if (err) *err = nvmtl_stagein_err(@"compute stage-in: this translator has no nvmtl_translate_set_kernel_stage_in - rebuild nvmtl_translate");
        return nil;
    }
    NSString *key = [NSString stringWithFormat:@"stagein:%lu:%@:%@:%@", (unsigned long)fn->_air.hash, fn->_fname, [words base64EncodedStringWithOptions:0], fn->_fc ?: @""];
    static NSMutableDictionary *memo; static dispatch_once_t once; dispatch_once(&once, ^{ memo = [NSMutableDictionary new]; });
    NSData *spirv;
    @synchronized (memo) { spirv = memo[key]; }
    if (!spirv) {
        if (!xlate_stagein(words.bytes, (uint32_t)(words.length / 4))) {
            if (err) *err = nvmtl_stagein_err(@"compute stage-in: the translator refused the stage-input layout (a format, step function or count it does not carry)");
            return nil;
        }
        NSString *why = nil;
        g_translate_variant_forced = YES;
        spirv = nvmtl_translate_variant_uncached(fn, @{}, @{}, &why);
        g_translate_variant_forced = NO;
        xlate_stagein(NULL, 0);
        if (!spirv) {
            if (err) *err = nvmtl_stagein_err([NSString stringWithFormat:@"compute stage-in: translating \"%@\" with the layout failed: %@", fn->_fname, why ?: @"unknown"]);
            return nil;
        }
        @synchronized (memo) { memo[key] = spirv; }
        nvlog("compute stage-in: \"%s\" translated with %lu word(s) of layout -> %lu B", fn->_fname.UTF8String, (unsigned long)(words.length / 4), (unsigned long)spirv.length);
    }
    NVMTLFunction *sf = [NVMTLFunction new];
    sf->_spirv = spirv; sf->_fname = fn->_fname; sf->_stage = fn->_stage; sf->_air = fn->_air; sf->_fc = fn->_fc;
    sf->_lib = fn->_lib; sf->_fcv = fn->_fcv; sf->_specName = fn->_specName; sf->_needsLink = NO;
    return sf;
}

id<MTLFunction> nvmtl_link_kernel(MTLComputePipelineDescriptor *d, NSError **err)
{
    return nvmtl_link_stage(d.computeFunction, NVMTL_SINCE(11.0, MTLLinkedFunctions *, d.linkedFunctions, nil), NVMTL_SINCE(12.0, NSArray *, d.preloadedLibraries, nil), err);
}

#pragma mark - batch 41: dynamic libraries
@interface NVMTLDynamicLibrary : NSObject <MTLDynamicLibrary> { @public NVMTLLibrary *_lib; NSString *_installName, *_label; }
- (dispatch_data_t)airData NS_RETURNS_RETAINED;
@end
static char kNVMTLCompileDylibs;

static BOOL nvmtl_mtlb_dynamic(NSData *raw, NSString **nameOut, NSArray<NSString *> **importsOut)
{
    if (nameOut) *nameOut = nil;
    if (importsOut) *importsOut = @[];
    const uint8_t *b = raw.bytes; NSUInteger n = raw.length;
    if (n < 0x58 || memcmp(b, "MTLB", 4) != 0) return NO;
    uint64_t flo = 0, fls = 0; memcpy(&flo, b + 0x18, 8); memcpy(&fls, b + 0x20, 8);
    if (flo > n || fls > n - flo) return NO;
    NSUInteger pos = (NSUInteger)(flo + fls); uint64_t doff = 0, dsz = 0; BOOL have = NO;
    while (pos + 6 <= n && memcmp(b + pos, "ENDT", 4) != 0) {
        uint16_t len = 0; memcpy(&len, b + pos + 4, 2);
        if (pos + 6 + len > n) return NO;
        if (memcmp(b + pos, "HDYN", 4) == 0 && len == 16) { memcpy(&doff, b + pos + 6, 8); memcpy(&dsz, b + pos + 14, 8); have = YES; }
        pos += 6 + len;
    }
    if (!have) return YES;
    if (doff > n || dsz > n - doff) return NO;
    NSUInteger p = (NSUInteger)doff, end = (NSUInteger)(doff + dsz);
    NSMutableArray<NSString *> *imp = [NSMutableArray new];
    while (p + 6 <= end && memcmp(b + p, "ENDT", 4) != 0) {
        uint16_t len = 0; memcpy(&len, b + p + 4, 2);
        if (p + 6 + len > end) break;
        BOOL isName = memcmp(b + p, "NAME", 4) == 0, isDyn = memcmp(b + p, "DYNL", 4) == 0;
        if ((isName || isDyn) && len > 1 && b[p + 6 + len - 1] == 0) {
            NSString *s = [[NSString alloc] initWithBytes:b + p + 6 length:len - 1 encoding:NSUTF8StringEncoding];
            if (s && isName && nameOut) *nameOut = s;
            if (s && isDyn) [imp addObject:s];
        }
        p += 6 + len;
    }
    if (importsOut) *importsOut = [imp copy];
    return YES;
}

static NSError *nvmtl_b41_err(NSString *m) {
    nvlog("%s", m.UTF8String);
    return [NSError errorWithDomain:@"NVMTL" code:5 userInfo:@{NSLocalizedDescriptionKey: m}];
}

static NVMTLFunction *nvmtl_link_dylibs(NVMTLFunction *fn, NSArray *dylibs, NSError **err)
{
    NSMutableSet<NSString *> *decl = [NSMutableSet new], *def = [NSMutableSet new];
    [fn->_air enumerateLinesUsingBlock:^(NSString *line, BOOL *stop) {
        BOOL isDecl = [line hasPrefix:@"declare"];
        if (!isDecl && ![line hasPrefix:@"define"]) return;
        NSString *s = nvmtl_sym_at(line);
        if (!s.length || [s hasPrefix:@"air."] || [s hasPrefix:@"llvm."]) return;
        [(isDecl ? decl : def) addObject:s];
    }];
    [decl minusSet:def];
    if (!decl.count) return fn;
    if (!xlate_link_ext) { if (err) *err = nvmtl_b41_err(@"dylib: this translator has no nvmtl_link_externs - rebuild nvmtl_translate-merge with patch_xl_extern.py"); return nil; }
    NSMutableArray<NVMTLLibrary *> *libs = [NSMutableArray new];
    NSMutableString *from = [NSMutableString new];
    NSMutableArray *offered = [NSMutableArray arrayWithArray:dylibs ?: @[]];
    NSArray *compiled = fn->_lib ? objc_getAssociatedObject(fn->_lib, &kNVMTLCompileDylibs) : nil;
    [offered addObjectsFromArray:compiled ?: @[]];
    for (id dl in offered) {
        if (![dl isKindOfClass:[NVMTLDynamicLibrary class]]) { [from appendFormat:@" [foreign %@ - skipped]", NSStringFromClass([dl class])]; continue; }
        NVMTLDynamicLibrary *x = dl;
        if (x->_lib && [libs indexOfObjectIdenticalTo:x->_lib] == NSNotFound) { [libs addObject:x->_lib]; [from appendFormat:@" [%@]", x->_installName ?: @"(no installName)"]; }
    }
    NSArray<NSString *> *imports = nil;
    NVMTLLibrary *owner = [fn->_lib isKindOfClass:[NVMTLLibrary class]] ? (NVMTLLibrary *)fn->_lib : nil;
    if (owner) nvmtl_mtlb_dynamic(owner->_raw, NULL, &imports);
    for (NSString *path in imports) {
        BOOL dup = NO;
        for (id dl in offered) if ([dl isKindOfClass:[NVMTLDynamicLibrary class]] && [((NVMTLDynamicLibrary *)dl)->_installName isEqualToString:path]) dup = YES;
        if (dup) continue;
        if (![[NSFileManager defaultManager] fileExistsAtPath:path]) { [from appendFormat:@" [installName %@ NOT ON DISK]", path]; continue; }
        NSError *le = nil;
        id l = [(id<MTLDevice>)gNVMTLMainDevice newLibraryWithURL:[NSURL fileURLWithPath:path] error:&le];
        if ([l isKindOfClass:[NVMTLLibrary class]]) { [libs addObject:l]; [from appendFormat:@" [disk %@]", path]; }
        else [from appendFormat:@" [disk %@ UNREADABLE: %@]", path, le.localizedDescription ?: @"?"];
    }
    NSMutableArray<NSString *> *mods = [NSMutableArray new];
    for (NVMTLLibrary *l in libs) for (NSString *k in l->_airs) { NSString *m = l->_airs[k]; if ([mods indexOfObjectIdenticalTo:m] == NSNotFound) [mods addObject:m]; }
    NSString *want = [[decl.allObjects sortedArrayUsingSelector:@selector(compare:)] componentsJoinedByString:@","];
    size_t n = mods.count;
    const char **mls = calloc(n ? n : 1, sizeof *mls);
    if (!mls) { if (err) *err = nvmtl_b41_err(@"dylib: module table allocation failed"); return nil; }
    for (size_t i = 0; i < n; i++) mls[i] = mods[i].UTF8String;
    uint8_t *out = NULL; size_t len = 0; char e[1024] = {0};
    int rc = xlate_link_ext(fn->_air.UTF8String, mls, n, &out, &len, e, sizeof e);
    free(mls);
    if (rc != 0 || !out) {
        if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"dylib: \"%@\" needs %@ - %s; searched%@", fn->_fname, want, e[0] ? e : "link failed", from.length ? from : @" nothing (no preloadedLibraries, no compile-time libraries, no DYNL)"]);
        return nil;
    }
    NSString *linked = [[NSString alloc] initWithBytes:out length:(len && out[len-1] == 0) ? len - 1 : len encoding:NSUTF8StringEncoding];
    xlate_free(out, len);
    if (!linked) { if (err) *err = nvmtl_b41_err(@"dylib: linked AIR is not UTF-8"); return nil; }
    nvlog("dylib: \"%s\" extern(s) %s resolved from %zu module(s):%s (%lu -> %lu B AIR)", fn->_fname.UTF8String, want.UTF8String, n,
          from.UTF8String, (unsigned long)fn->_air.length, (unsigned long)linked.length);
    NVMTLFunction *df = [NVMTLFunction new];
    df->_spirv = nil; df->_fname = fn->_fname; df->_stage = fn->_stage; df->_air = linked; df->_fc = fn->_fc;
    df->_lib = fn->_lib; df->_fcv = fn->_fcv; df->_specName = fn->_specName; df->_needsLink = YES;
    return df;
}

// Whether this AIR reads a visible or an intersection function table. Both callers below used to decide that by
// searching for the argument metadata string !"air.visible_function_table", which a ray-tracing kernel from
// Apple's shader converter never carries: its table is a field of the dispatch-rays argument struct, not a
// kernel argument, so the only trace of it in the module is the intrinsic that reads it. Measured over 76
// ray-tracing kernels captured from a running game: 76 of 76 call
// air.get_function_pointer_visible_function_table, 0 of 76 carry the metadata either caller looked for. The
// table branch was therefore never taken, the plain link ran with no table candidates, the handle lookup
// survived specialization, and translation refused the module on the null compare its result feeds - 76
// refusals and not one "reads a visible function table" line in the run. Ask for the intrinsics, which are what
// actually reads the table; the metadata stays as a second form so a module that does carry it still matches.
BOOL nvmtl_air_reads_function_table(NSString *air)
{
    if (!air.length) return NO;
    static NSArray<NSString *> *forms;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        forms = @[@"@air.get_function_pointer_visible_function_table",
                  @"@air.get_size_visible_function_table",
                  @"@air.is_null_visible_function_table",
                  @"@air.get_null_intersection_function_table",
                  @"@air.set_buffer_intersection_function_table",
                  @"@air.set_intersection_function_table",
                  @"!\"air.visible_function_table\"",
                  @"!\"air.intersection_function_table\""];
    });
    for (NSString *f in forms)
        if ([air rangeOfString:f].location != NSNotFound) return YES;
    return NO;
}

// Whether a function needs linking before it can be translated: it calls visible functions it does not define,
// so the pipeline's linked functions have to supply them. This used to be decided by reading the translator's
// error text, and when the translator stopped refusing every visible-function reference outright, its message for
// this case changed ("is called on a live path but no linked function supplies it"). Nothing matched the new text,
// so the function was never deferred to pipeline creation; it came back nil and the caller drew nothing. On macOS
// 26 that is the system's icon and Liquid Glass shaders: icons missing pieces, glass menus drawn as a flat tint.
// The translator lists the references itself, so ask it. The known messages stay as a fallback for a translator
// that cannot answer, so a query that cannot run never reads as "no references".
// The answer for one AIR text never changes, but newFunctionWithName: asks it on every call: VideoToolbox's
// pixel-transfer shaders (System Settings > Displays thumbnails, issue #567) were scanned to the end of their text
// on each call, about three quarters of the pane's main-thread time. The result is kept per AIR string object
// (weak key, so it goes when the text does). Only a definite answer from the translator is kept.
static NSMapTable<NSString *, NSNumber *> *g_vlink_memo;
static os_unfair_lock g_vlink_memo_lock = OS_UNFAIR_LOCK_INIT;
static BOOL nvmtl_needs_visible_link_uncached(NSString *air, NSString *why, BOOL *definite);
BOOL nvmtl_needs_visible_link(NSString *air, NSString *why)
{
    if (!air.length) return nvmtl_needs_visible_link_uncached(air, why, NULL);
    os_unfair_lock_lock(&g_vlink_memo_lock);
    if (!g_vlink_memo) g_vlink_memo = [NSMapTable weakToStrongObjectsMapTable];
    NSNumber *hit = [g_vlink_memo objectForKey:air];
    os_unfair_lock_unlock(&g_vlink_memo_lock);
    if (hit) return hit.boolValue;
    BOOL definite = NO;
    const BOOL r = nvmtl_needs_visible_link_uncached(air, why, &definite);
    if (definite) {
        os_unfair_lock_lock(&g_vlink_memo_lock);
        [g_vlink_memo setObject:@(r) forKey:air];
        os_unfair_lock_unlock(&g_vlink_memo_lock);
    }
    return r;
}
static BOOL nvmtl_needs_visible_link_uncached(NSString *air, NSString *why, BOOL *definite)
{
    if (definite) *definite = NO;
    if (air.length && nvmtl_xlate_ready() && xlate_vrefs) {
        uint8_t *out = NULL; size_t len = 0; char e[256] = {0};
        if (xlate_vrefs(air.UTF8String, &out, &len, e, sizeof e) == 0) {
            id refs = [NSJSONSerialization JSONObjectWithData:[NSData dataWithBytes:out length:len] options:0 error:NULL];
            xlate_free(out, len);
            // the query lists direct references only; a function that calls through a visible or intersection
            // function table has none, and nvmtl_link_stage links it from the table candidates
            if ([refs isKindOfClass:[NSArray class]]) {
                if (definite) *definite = YES;
                return [(NSArray *)refs count] > 0 || nvmtl_air_reads_function_table(air);
            }
        }
    }
    return why && ([why rangeOfString:@"visible function reference"].location != NSNotFound
                || [why rangeOfString:@"Metal visible function table"].location != NSNotFound
                || [why rangeOfString:@"no linked function supplies it"].location != NSNotFound
                || [why rangeOfString:@".MTL_UNRESOLVED_VISIBLE_FN"].location != NSNotFound);
}

// A link that failed (the linker or the translation refused it) fails the same way for the same inputs. Since a refused
// visible-function module is deferred to the pipeline link (nvmtl_needs_visible_link), a module that really is broken would otherwise be
// linked and translated again for every pipeline that uses it. The key holds the inputs themselves: equal strings compare
// by pointer first, so a lookup costs no AIR comparison when the library hands out the same objects.
static NSCache *nvmtl_link_failures(void) {
    static NSCache *c; static dispatch_once_t once;
    dispatch_once(&once, ^{ c = [NSCache new]; c.countLimit = 256; });
    return c;
}
static id nvmtl_link_failure_key(NVMTLFunction *fn, NSDictionary *mods, NSArray *tc, NSDictionary *fc) {
    return @[fn->_air ?: @"", fn->_stage ?: @"", mods ?: @{}, tc ?: @[], fc ?: @{}];
}
id<MTLFunction> nvmtl_link_stage(id<MTLFunction> kIn, MTLLinkedFunctions *lfIn, NSArray *dylibs, NSError **err)
{
    if (err) *err = nil;
    id<MTLFunction> k = kIn;
    if (![(id)k isKindOfClass:[NVMTLFunction class]]) return k;
    NVMTLFunction *fn = (NVMTLFunction *)k;
    if (fn->_air.length && (dylibs.count || [fn->_air rangeOfString:@"@air.dyld_lib_table"].location != NSNotFound)) {
        NSError *de = nil; NVMTLFunction *df = nvmtl_link_dylibs(fn, dylibs, &de);
        if (!df) { if (err) *err = de; return nil; }
        fn = df;
    }
    // A PIPELINE'S LINKED FUNCTIONS MUST BE RECORDED EVEN WHEN NOTHING NEEDS LINKING.
    //    This early return skips the whole link - correctly, there is nothing to resolve - but it also
    //    threw away the descriptor's linkedFunctions, so the pipeline never learned them and
    //    functionHandleWithFunction had nothing to answer from. MEASURED: the game creates
    //    rgs_reference_wavefront_trace with needsLink=no and 24 linkedFunctions, then asks that pipeline
    //    for handles to ahs_main / chs_main_0..5 and gets nil for all 8 - after which it binds no visible
    //    function table, no intersection table and no acceleration structure, which is where ray tracing
    //    stopped. A shader that dispatches only through handles the HOST writes needs no link of its own.
    // The list is attached to the object the pipeline will hold (`k`), because that is what
    //    functionHandleWithFunction reads; only functions with AIR are recorded, since a handle is a
    //    name-to-index mapping and an AIR-less candidate could never be dispatched anyway.
    if (!fn->_needsLink) {
        NSMutableArray *handleFns = [NSMutableArray new];
        for (id c in (lfIn.functions ?: @[]))
            if ([c isKindOfClass:[NVMTLFunction class]] && ((NVMTLFunction *)c)->_air.length)
                [handleFns addObject:c];
        if (handleFns.count && !objc_getAssociatedObject(k, &kNVMTLTableFns)) {
            objc_setAssociatedObject(k, &kNVMTLTableFns, [handleFns copy], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            FILE *lg = fopen(nvmtl_log_path(), "a");
            if (lg) {
                fprintf(lg, "pid %d NVMTL: link: \"%s\" needs no link but carries %lu linked function(s) for its handles\n",
                        getpid(), fn->_fname.UTF8String, (unsigned long)handleFns.count);
                fclose(lg);
            }
        }
        return k;
    }
    if (!nvmtl_xlate_ready() || !xlate_vrefs || !xlate_link) {
        if (err) *err = nvmtl_link_err(@"this translator has no nvmtl_link_visible - rebuild nvmtl_translate-merge with patch_xl_link.py");
        return nil;
    }
    uint8_t *out = NULL; size_t len = 0; char e[1024] = {0};
    NSArray *refs = nil;
    if (xlate_vrefs(fn->_air.UTF8String, &out, &len, e, sizeof e) == 0) {
        refs = [NSJSONSerialization JSONObjectWithData:[NSData dataWithBytes:out length:len] options:0 error:NULL];
        xlate_free(out, len); out = NULL;
    }
    if (![refs isKindOfClass:[NSArray class]]) {
        if (err) *err = nvmtl_link_err([NSString stringWithFormat:@"\"%@\": cannot read its visible references (%s)", fn->_fname, e]);
        return nil;
    }
    NSMutableArray *cands = [NSMutableArray new];
    MTLLinkedFunctions *lf = lfIn;
    // PER-LINK, WITH THE ENTRY NAME. computePipelineDescriptor's line is deduped by format
    //    string (nvlog_dropped), so after a few cs_main_ pipelines every later one is suppressed and the
    //    RT entries' counts never appear. This says what THIS link was actually given, which is the only
    //    way to tell "the game passed no linked functions" from "we were called on a path that drops
    //    them" - rgs_reference_wavefront_trace gets 0 candidates and then 8 nil handle requests.
    // AND IT MUST BYPASS nvlog: nvlog_dropped dedupes by FORMAT-STRING POINTER, keeping the first
    //    few per call site and then every Nth, so one busy site hides everything after it. That is why
    //    the descriptor line only ever showed cs_main_ - the RT pipelines came later and were dropped -
    //    and it would silently do the same here, which already made one run read as "never linked".
    {
        FILE *lg = fopen(nvmtl_log_path(), "a");
        if (lg) {
            fprintf(lg, "pid %d NVMTL: link: \"%s\" given linkedFunctions=%s functions=%lu private=%lu binary=%lu groups=%lu, %lu dylib(s)\n",
                    getpid(), ((NVMTLFunction *)kIn)->_fname.UTF8String, lf ? "YES" : "nil",
                    (unsigned long)lf.functions.count,
                    (unsigned long)([lf respondsToSelector:@selector(privateFunctions)]
                                    ? [(NSArray *)[(id)lf valueForKey:@"privateFunctions"] count] : 0),
                    (unsigned long)NVMTL_SINCE(11.0, NSUInteger, lf.binaryFunctions.count, 0), (unsigned long)lf.groups.count,
                    (unsigned long)[dylibs count]);
            fclose(lg);
        }
    }
    if (lf.functions) [cands addObjectsFromArray:lf.functions];
    if ([lf respondsToSelector:@selector(privateFunctions)]) { NSArray *pf = [(id)lf valueForKey:@"privateFunctions"]; if ([pf isKindOfClass:[NSArray class]]) [cands addObjectsFromArray:pf]; }
    for (NSArray *g in lf.groups.allValues) if ([g isKindOfClass:[NSArray class]]) [cands addObjectsFromArray:g];
    NSMutableDictionary<NSString *, NSString *> *mods = [NSMutableDictionary new];
    NSMutableDictionary *fc = [NSMutableDictionary new];
    NSMutableString *seen = [NSMutableString new];
    for (id c in cands) {
        if (![c isKindOfClass:[NVMTLFunction class]]) { [seen appendFormat:@" [foreign %@ %@]", NSStringFromClass([c class]), [c name]]; continue; }
        NVMTLFunction *cf = (NVMTLFunction *)c;
        [seen appendFormat:@" [%@%@%@ air=%lu fc=%lu]", cf->_fname, cf->_specName ? @"/" : @"", cf->_specName ?: @"",
            (unsigned long)cf->_air.length, (unsigned long)cf->_fc.count];
        if (!cf->_air.length) continue;
        NSString *air = nvmtl_link_candidate_air(cf, NULL);
        /* Only names the module really defines: validate_linkage() refuses the whole link otherwise. */
        for (NSString *nm in @[cf->_specName ?: @"", cf.name ?: @"", cf->_fname ?: @""])
            if (nm.length && !mods[nm] && nvmtl_air_defines(air, nm)) mods[nm] = air;
        for (NSString *ki in cf->_fc) {
            if (fc[ki] && ![fc[ki] isEqual:cf->_fc[ki]]) nvlog("link: constant %s differs between linked functions - first kept", ki.UTF8String);
            else fc[ki] = cf->_fc[ki];
        }
    }
    [fc addEntriesFromDictionary:fn->_fc ?: @{}];
    nvlog("link: \"%s\" references %s; %lu linked function(s):%s", fn->_fname.UTF8String,
          [[refs componentsJoinedByString:@","] UTF8String], (unsigned long)cands.count, seen.UTF8String);
    NSMutableArray *missing = [NSMutableArray new];
    for (NSString *s in refs) {
        if (![s isKindOfClass:[NSString class]]) {
            if (err) *err = nvmtl_link_err([NSString stringWithFormat:@"\"%@\": malformed visible reference %@", fn->_fname, [s description]]);
            return nil;
        }
        if (!mods[s]) [missing addObject:s];
    }
    if (missing.count)
        nvlog("link: \"%s\": no linked function supplies %s - left unresolved; translation refuses it if a call survives the constants",
              fn->_fname.UTF8String, [[missing componentsJoinedByString:@", "] UTF8String]);
    NSArray *keys = mods.allKeys; size_t n = keys.count;
    const char **syms = calloc(n ? n : 1, sizeof *syms), **mls = calloc(n ? n : 1, sizeof *mls);
    if (!syms || !mls) { free(syms); free(mls); if (err) *err = nvmtl_link_err(@"link table allocation failed"); return nil; }
    for (size_t i = 0; i < n; i++) { syms[i] = [keys[i] UTF8String]; mls[i] = [mods[keys[i]] UTF8String]; }
    // A REFERENCED FUNCTION CAN BE THE ONE THAT READS THE TABLE, and testing only the entry's
    //    own AIR missed every one of them. MEASURED on the game's refused RT kernels: 60 of 60 surviving
    //    `air.get_function_pointer_visible_function_table` calls live in a LINKED-IN define, not in the
    //    entry — so this returned NO, the plain xlate_link path ran, it pulled that function in, and the
    //    lookup reached the emitter, where `icmp eq ptr %fp, null` refuses with "pointer nullness is not
    //    tracked". The table path has to be chosen on what the LINKED module will contain, which is the
    //    entry plus every module linked into it.
    BOOL usesTable = nvmtl_air_reads_function_table(fn->_air);
    if (!usesTable) {
        for (NSString *s in mods) {
            if (nvmtl_air_reads_function_table(mods[s])) {
                usesTable = YES;
                nvlog("link: \"%s\": the table is read by linked function %s, not by the entry - taking the visible-table path",
                      fn->_fname.UTF8String, s.UTF8String);
                break;
            }
        }
    }
    id failKey = nvmtl_link_failure_key(fn, mods, usesTable ? (lf.functions ?: @[]) : @[], fc);
    NSString *known = [nvmtl_link_failures() objectForKey:failKey];
    if (known) {
        free(syms); free(mls);
        if (err) *err = nvmtl_link_err(known);
        return nil;
    }
    NSMutableArray *tc = [NSMutableArray new];
    int rc;
    if (usesTable) {
        for (id c in (lf.functions ?: @[])) {
            if (![c isKindOfClass:[NVMTLFunction class]] || !((NVMTLFunction *)c)->_air.length) {
                free(syms); free(mls);
                if (err) *err = nvmtl_link_err([NSString stringWithFormat:@"\"%@\": linked function %@ has no AIR this driver can link", fn->_fname, [c name]]);
                return nil;
            }
            [tc addObject:c];
        }
        if (!xlate_link_rt) { free(syms); free(mls); if (err) *err = nvmtl_link_err(@"this translator has no nvmtl_link_runtime - rebuild with patch_xl_vft_ffi.py"); return nil; }
        size_t nr = 0; for (NSString *s in refs) if (mods[s]) nr++;
        const char **rs = calloc(nr ? nr : 1, sizeof *rs), **rm = calloc(nr ? nr : 1, sizeof *rm);
        const char **cs = calloc(tc.count ? tc.count : 1, sizeof *cs), **cm = calloc(tc.count ? tc.count : 1, sizeof *cm);
        size_t j = 0; for (NSString *s in refs) if (mods[s]) { rs[j] = s.UTF8String; rm[j] = [mods[s] UTF8String]; j++; }
        NSMutableArray *keep = [NSMutableArray arrayWithCapacity:tc.count * 2];
        for (NSUInteger i = 0; i < tc.count; i++) { NVMTLFunction *cf = tc[i]; NSString *cn = nil; NSString *ca = nvmtl_link_candidate_air(cf, &cn);
            [keep addObject:cn ?: @""]; [keep addObject:ca ?: @""];
            cs[i] = [keep[2 * i] UTF8String]; cm[i] = [keep[2 * i + 1] UTF8String]; }
        nvlog("link: \"%s\" reads a visible function table - %lu table candidate(s), %zu direct reference(s)", fn->_fname.UTF8String, (unsigned long)tc.count, nr);
        rc = xlate_link_rt(fn->_air.UTF8String, fn->_stage.UTF8String, rs, rm, nr, cs, cm, tc.count, &out, &len, e, sizeof e);
        free(rs); free(rm); free(cs); free(cm);
    } else
    rc = xlate_link(fn->_air.UTF8String, fn->_stage.UTF8String, syms, mls, n, &out, &len, e, sizeof e);
    free(syms); free(mls);
    if (rc != 0 || !out) {
        NSString *why = [NSString stringWithFormat:@"\"%@\": %s", fn->_fname, e[0] ? e : "link failed"];
        [nvmtl_link_failures() setObject:why forKey:failKey];
        if (err) *err = nvmtl_link_err(why);
        return nil;
    }
    NSString *linked = [[NSString alloc] initWithBytes:out length:(len && out[len-1] == 0) ? len - 1 : len encoding:NSUTF8StringEncoding];
    xlate_free(out, len);
    if (!linked) { if (err) *err = nvmtl_link_err(@"linked AIR is not UTF-8"); return nil; }
    NSString *dumpDir = nvmtl_link_dump_dir();
    if (dumpDir) {
        NSString *dir = dumpDir;
        [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:NULL];
        [fn->_air writeToFile:[dir stringByAppendingFormat:@"/%@.kernel.ll", fn->_fname] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
        [linked writeToFile:[dir stringByAppendingFormat:@"/%@.linked.ll", fn->_fname] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
        for (NSString *nm in mods) [mods[nm] writeToFile:[dir stringByAppendingFormat:@"/mod.%@.ll", nm] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
        nvlog("link: dumped to %s", dir.UTF8String);
    }
    size_t nfc = fc.count, k2 = 0;
    uint32_t *idx = calloc(nfc ? nfc : 1, sizeof *idx), *sz = calloc(nfc ? nfc : 1, sizeof *sz);
    uint8_t (*bytes)[16] = calloc(nfc ? nfc : 1, 16); const uint8_t **pay = calloc(nfc ? nfc : 1, sizeof *pay);
    if (!idx || !sz || !bytes || !pay) { free(idx); free(sz); free(bytes); free(pay); if (err) *err = nvmtl_link_err(@"constant table allocation failed"); return nil; }
    for (NSString *ki in fc) {
        NSArray *a = fc[ki];
        if (![a isKindOfClass:[NSArray class]] || a.count == 0 || a.count > 16) {
            free(idx); free(sz); free(bytes); free(pay);
            if (err) *err = nvmtl_link_err([NSString stringWithFormat:@"constant %@ cannot be carried into the linked kernel", ki]);
            return nil;
        }
        idx[k2] = (uint32_t)ki.integerValue; sz[k2] = (uint32_t)a.count;
        for (NSUInteger j = 0; j < a.count; j++) bytes[k2][j] = (uint8_t)[a[j] unsignedCharValue];
        pay[k2] = bytes[k2]; k2++;
    }
    if (dumpDir) {
        NSMutableString *t = [NSMutableString stringWithFormat:@"stage %@\n", fn->_stage ?: @""];
        for (size_t q = 0; q < k2; q++) {
            [t appendFormat:@"%u %u", idx[q], sz[q]];
            for (uint32_t j = 0; j < sz[q]; j++) [t appendFormat:@" %02x", pay[q][j]];
            [t appendString:@"\n"];
        }
        [t writeToFile:[dumpDir stringByAppendingFormat:@"/%@.fc.txt", fn->_fname] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
    }
    // Machine code compiled ahead of time for exactly this link (kernel AIR, chosen bodies, constants) runs
    // instead of a translation: the kernels that have it are the ones the translator cannot express or
    // expresses slowly, and asking it first cost minutes on MPS's depthwise convolution.
    {
        id sass = nvmtl_sass_kernel_for_linked(fn, mods, fc);
        if (sass) {
            free(idx); free(sz); free(bytes); free(pay);
            NVMTLFunction *mf = [NVMTLFunction new];
            mf->_fname = fn->_fname; mf->_stage = fn->_stage; mf->_air = linked; mf->_fc = [fc copy];
            mf->_lib = fn->_lib; mf->_fcv = fn->_fcv; mf->_specName = fn->_specName; mf->_sassKernel = sass;
            nvlog("link: \"%s\" runs from its compiled machine code; no translation", fn->_fname.UTF8String);
            return mf;
        }
    }
    NSString *te = nil;
    NSData *spv = nvmtl_translate_link_cached(linked, fn->_stage, idx, sz, pay, k2, &te);
    free(idx); free(sz); free(bytes); free(pay);
    if (!spv && dumpDir)
        [(te ?: @"(no reason)") writeToFile:[dumpDir stringByAppendingFormat:@"/%@.err.txt", fn->_fname] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
    if (!spv) {
        NSString *why = [NSString stringWithFormat:@"\"%@\" linked (%lu B AIR) but did not translate: %@", fn->_fname, (unsigned long)linked.length, te];
        [nvmtl_link_failures() setObject:why forKey:failKey];
        if (err) *err = nvmtl_link_err(why);
        return nil;
    }
    if (dumpDir)
        [spv writeToFile:[dumpDir stringByAppendingFormat:@"/%@.spv", fn->_fname] atomically:YES];
    nvlog("link: \"%s\" linked %lu -> %lu B AIR, %zu constant(s) -> %lu B SPIR-V", fn->_fname.UTF8String,
          (unsigned long)fn->_air.length, (unsigned long)linked.length, k2, (unsigned long)spv.length);
    NVMTLFunction *nf = [NVMTLFunction new];
    nf->_spirv = spv; nf->_fname = fn->_fname; nf->_stage = fn->_stage; nf->_air = linked; nf->_fc = [fc copy];
    nf->_lib = fn->_lib; nf->_fcv = fn->_fcv; nf->_specName = fn->_specName;
    // THE HANDLE API IS PIPELINE-LEVEL, NOT TABLE-LEVEL. functionHandleWithFunction: must
    //    answer for ANY function in the descriptor's linkedFunctions, whether or not the entry's own AIR
    //    reads a visible function table. MEASURED: `rgs_reference_wavefront_trace` never appears in a
    //    "reads a visible function table" line - it links 24 hit shaders but reads no table itself - so
    //    usesTable was NO, nothing was recorded, and all 8 handle requests for ahs_main / chs_main_0..5
    //    came back nil with "(0 linked)". The game then binds no table and sets no acceleration
    //    structure, which is where ray tracing stopped.
    // The candidates are collected here WITHOUT the usesTable branch's validation, which must stay
    //    an error only on the table path: a function with no AIR cannot be dispatched from a table, but
    //    it must not newly refuse a pipeline that never dispatches one.
    if (!tc.count) {
        for (id c in (lf.functions ?: @[]))
            if ([c isKindOfClass:[NVMTLFunction class]] && ((NVMTLFunction *)c)->_air.length)
                [tc addObject:c];
        if (tc.count)
            nvlog("link: \"%s\" reads no table but links %lu function(s) - recorded so their handles resolve",
                  nf->_fname.UTF8String, (unsigned long)tc.count);
    }
    if (tc.count) {
        objc_setAssociatedObject(nf, &kNVMTLTableFns, [tc copy], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        // say WHICH function object carries the candidate list, so a pipeline that later
        //    reports "(0 linked)" can be matched against it by name and object identity.
        nvlog("link: \"%s\" carries %lu table candidate(s) on function %p",
              nf->_fname.UTF8String, (unsigned long)tc.count, (__bridge void *)nf);
    }
    return nf;
}

#pragma mark - batch 39: function pointers (compute)
@implementation NVMTLFunctionHandle
- (MTLFunctionType)functionType { return _fn.functionType; }
- (NSString *)name { return _fn.name; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = _id; return r; }
@end
@implementation NVMTLVisibleFunctionTable
- (void)setFunction:(id<MTLFunctionHandle>)f atIndex:(NSUInteger)i {
    { static _Atomic unsigned n; if (atomic_fetch_add(&n, 1) < 8) { FILE *lg = fopen(nvmtl_log_path(), "a"); if (lg) { fprintf(lg, "pid %d NVMTL: RTBIND visible table setFunction slot %lu <- %s id %llu\n", getpid(), (unsigned long)i, f ? [[f name] UTF8String] : "nil", f && [(id)f isKindOfClass:[NVMTLFunctionHandle class]] ? (unsigned long long)((NVMTLFunctionHandle *)f)->_id : 0ull); fclose(lg); } } }
    if (i >= _n) { nvlog("visible function table: setFunction atIndex:%lu beyond %lu slots - IGNORED", (unsigned long)i, (unsigned long)_n); return; }
    if (f && ![(id)f isKindOfClass:[NVMTLFunctionHandle class]]) { nvlog("visible function table: foreign handle %s - slot %lu set NULL", NSStringFromClass([(id)f class]).UTF8String, (unsigned long)i); f = nil; }
    ((uint64_t *)_buf.contents)[i] = f ? ((NVMTLFunctionHandle *)f)->_id : 0;
}
- (void)setFunctions:(const id<MTLFunctionHandle> [])fs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setFunction:fs[k] atIndex:r.location + k];
}
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = _buf.gpuAddress; return r; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _buf.label; }
- (void)setLabel:(NSString *)l { _buf.label = l; }
- (MTLStorageMode)storageMode { return _buf.storageMode; }
- (MTLCPUCacheMode)cpuCacheMode { return _buf.cpuCacheMode; }
- (MTLHazardTrackingMode)hazardTrackingMode { return _buf.hazardTrackingMode; }
- (MTLResourceOptions)resourceOptions { return _buf.resourceOptions; }
- (id<MTLHeap>)heap { return nil; }
- (NSUInteger)heapOffset { return 0; }
- (BOOL)isAliasable { return NO; }
- (void)makeAliasable {}
- (NSUInteger)allocatedSize { return _buf.allocatedSize; }
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return MTLPurgeableStateNonVolatile; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return nvmtl_owner_check(t); }
- (id)forwardingTargetForSelector:(SEL)s { return _buf; }
@end

@implementation NVMTLIntersectionFunctionTable
- (uint64_t *)nvmtlWords { return (uint64_t *)_buf.contents; }
- (void)setFunction:(id<MTLFunctionHandle>)f atIndex:(NSUInteger)i {
    if (i >= _n) { nvlog("intersection function table: setFunction atIndex:%lu beyond %lu slots - IGNORED", (unsigned long)i, (unsigned long)_n); return; }
    if (f && ![(id)f isKindOfClass:[NVMTLFunctionHandle class]]) { nvlog("intersection function table: foreign handle %s - slot %lu set NULL", NSStringFromClass([(id)f class]).UTF8String, (unsigned long)i); f = nil; }
    [self nvmtlWords][NVMTL_IFT_BUFFERS + i] = f ? ((NVMTLFunctionHandle *)f)->_id : 0;
}
- (void)setFunctions:(const id<MTLFunctionHandle> [])fs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setFunction:fs[k] atIndex:r.location + k];
}
- (void)setOpaqueTriangleIntersectionFunctionWithSignature:(MTLIntersectionFunctionSignature)s atIndex:(NSUInteger)i {
    if (i >= _n) { nvlog("intersection function table: opaque triangle atIndex:%lu beyond %lu slots - IGNORED", (unsigned long)i, (unsigned long)_n); return; }
    [self nvmtlWords][NVMTL_IFT_BUFFERS + i] = NVMTL_IFT_OPAQUE;
}
- (void)setOpaqueTriangleIntersectionFunctionWithSignature:(MTLIntersectionFunctionSignature)s withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setOpaqueTriangleIntersectionFunctionWithSignature:s atIndex:r.location + k];
}
- (void)setOpaqueCurveIntersectionFunctionWithSignature:(MTLIntersectionFunctionSignature)s atIndex:(NSUInteger)i {
    nvlog("intersection function table: opaque CURVE function at %lu is not carried (no curve geometry) - slot left as it was", (unsigned long)i);
}
- (void)setOpaqueCurveIntersectionFunctionWithSignature:(MTLIntersectionFunctionSignature)s withRange:(NSRange)r {
    nvlog("intersection function table: opaque CURVE functions %lu+%lu are not carried (no curve geometry)", (unsigned long)r.location, (unsigned long)r.length);
}
- (void)setBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i {
    if (i >= NVMTL_IFT_BUFFERS) { nvlog("intersection function table: setBuffer atIndex:%lu beyond Metal's 31 - IGNORED", (unsigned long)i); return; }
    [self nvmtlWords][i] = b ? b.gpuAddress + off : 0;
}
- (void)setBuffers:(const id<MTLBuffer> [])bs offsets:(const NSUInteger [])offs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setBuffer:bs[k] offset:offs[k] atIndex:r.location + k];
}
- (void)setVisibleFunctionTable:(id<MTLVisibleFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLVisibleFunctionTable class]]) { nvlog("intersection function table: foreign visible table %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setBuffer:t ? ((NVMTLVisibleFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setVisibleFunctionTables:(const id<MTLVisibleFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setVisibleFunctionTable:ts[k] atBufferIndex:r.location + k];
}
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = _buf.gpuAddress; return r; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _buf.label; }
- (void)setLabel:(NSString *)l { _buf.label = l; }
- (MTLStorageMode)storageMode { return _buf.storageMode; }
- (MTLCPUCacheMode)cpuCacheMode { return _buf.cpuCacheMode; }
- (MTLHazardTrackingMode)hazardTrackingMode { return _buf.hazardTrackingMode; }
- (MTLResourceOptions)resourceOptions { return _buf.resourceOptions; }
- (id<MTLHeap>)heap { return nil; }
- (NSUInteger)heapOffset { return 0; }
- (BOOL)isAliasable { return NO; }
- (void)makeAliasable {}
- (NSUInteger)allocatedSize { return _buf.allocatedSize; }
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return MTLPurgeableStateNonVolatile; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return nvmtl_owner_check(t); }
- (id)forwardingTargetForSelector:(SEL)s { return _buf; }
@end
static id<MTLIntersectionFunctionTable> nvmtl_ift_new(MTLIntersectionFunctionTableDescriptor *d, const char *what) {
    const NSUInteger n = d.functionCount ? d.functionCount : 1, bytes = (NVMTL_IFT_BUFFERS + n) * sizeof(uint64_t);
    id<MTLBuffer> b = [(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:bytes options:MTLResourceStorageModeShared];
    if (!b) { nvlog("%s: intersection function table of %lu slot(s) - buffer allocation FAILED -> nil", what, (unsigned long)n); return nil; }
    memset(b.contents, 0, bytes);
    NVMTLIntersectionFunctionTable *t = [NVMTLIntersectionFunctionTable new]; t->_buf = b; t->_n = n;
    nvlog("%s: intersection function table, %lu slot(s) -> %p", what, (unsigned long)n, t);
    return t;
}

#pragma mark - batch 41: dynamic libraries (objects), render-stage function tables, binary archives
@implementation NVMTLDynamicLibrary
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)installName { return _installName; }
- (BOOL)serializeToURL:(NSURL *)url error:(NSError **)err {
    NSError *we = nil;
    if (_lib->_raw.length && [_lib->_raw writeToURL:url options:NSDataWritingAtomic error:&we]) return YES;
    if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"dylib serializeToURL: %@ - %@", url.path, we.localizedDescription ?: @"no container bytes"]);
    return NO;
}
- (dispatch_data_t)airData {
    NSData *r = _lib->_raw;
    return dispatch_data_create(r.bytes, r.length, NULL, DISPATCH_DATA_DESTRUCTOR_DEFAULT);
}
@end

id nvmtl_new_dylib(id<MTLLibrary> lib, NSError **err) {
    if (err) *err = nil;
    if (![(id)lib isKindOfClass:[NVMTLLibrary class]]) {
        if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"newDynamicLibrary: %s is not a library this driver made", object_getClassName(lib)]);
        return nil;
    }
    NVMTLLibrary *l = (NVMTLLibrary *)lib; const uint8_t *b = l->_raw.bytes;
    if (l->_raw.length < 0x58 || memcmp(b, "MTLB", 4) != 0 || b[0x0A] != 2) {
        if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"newDynamicLibrary: not a dynamic library (MTLB file type %d) - compile it with MTLLibraryTypeDynamic",
                                       l->_raw.length > 0x0A ? (int)b[0x0A] : -1]);
        return nil;
    }
    NSString *nm = nil; nvmtl_mtlb_dynamic(l->_raw, &nm, NULL);
    NVMTLDynamicLibrary *d = [NVMTLDynamicLibrary new]; d->_lib = l; d->_installName = nm;
    nvlog("newDynamicLibrary: %lu exported function(s), installName %s", (unsigned long)l->_airs.count, nm.UTF8String ?: "(none)");
    return d;
}
id nvmtl_new_dylib_url(NSURL *url, NSError **err) {
    id<MTLLibrary> l = [(id<MTLDevice>)gNVMTLMainDevice newLibraryWithURL:url error:err];
    return l ? nvmtl_new_dylib(l, err) : nil;
}
void nvmtl_note_compile_dylibs(id lib, NSArray *libraries) {
    if (![lib isKindOfClass:[NVMTLLibrary class]] || !libraries.count) return;
    objc_setAssociatedObject(lib, &kNVMTLCompileDylibs, [libraries copy], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

void nvmtl_render_keep_linked(id ps, id<MTLFunction> vf, id<MTLFunction> ff) {
    if (vf && objc_getAssociatedObject(vf, &kNVMTLTableFns)) objc_setAssociatedObject(ps, &kNVMTLRenderVert, vf, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    if (ff && objc_getAssociatedObject(ff, &kNVMTLTableFns)) objc_setAssociatedObject(ps, &kNVMTLRenderFrag, ff, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}
static id<MTLVisibleFunctionTable> nvmtl_vft_new(MTLVisibleFunctionTableDescriptor *d, const char *what) {
    NSUInteger n = d.functionCount ? d.functionCount : 1;
    id<MTLBuffer> b = [(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:n * sizeof(uint64_t) options:MTLResourceStorageModeShared];
    if (!b) { nvlog("%s newVisibleFunctionTableWithDescriptor: %lu slots - buffer allocation FAILED -> nil", what, (unsigned long)n); return nil; }
    memset(b.contents, 0, n * sizeof(uint64_t));
    NVMTLVisibleFunctionTable *t = [NVMTLVisibleFunctionTable new]; t->_buf = b; t->_n = n;
    nvlog("%s newVisibleFunctionTableWithDescriptor: %lu slot(s) -> %p", what, (unsigned long)n, t);
    return t;
}

@interface NVMTLBinaryArchive : NSObject <MTLBinaryArchive> { @public NSMutableSet<NSString *> *_keys; NSString *_label; } @end
static uint64_t nvmtl_fnv64(const char *s) {
    uint64_t h = 1469598103934665603ull;
    for (; s && *s; s++) { h ^= (uint8_t)*s; h *= 1099511628211ull; }
    return h;
}
static NSString *nvmtl_fn_key(id<MTLFunction> f) {
    if (![(id)f isKindOfClass:[NVMTLFunction class]]) return nil;
    NVMTLFunction *fn = (NVMTLFunction *)f;
    NSData *fc = fn->_fc.count ? [NSJSONSerialization dataWithJSONObject:fn->_fc options:NSJSONWritingSortedKeys error:NULL] : nil;
    NSString *fcs = fc ? [[NSString alloc] initWithData:fc encoding:NSUTF8StringEncoding] : @"";
    return [NSString stringWithFormat:@"%@#%016llx#%016llx", fn->_fname, nvmtl_fnv64(fn->_air.UTF8String), nvmtl_fnv64(fcs.UTF8String)];
}
NSString *nvmtl_archive_key_compute(id<MTLFunction> f) {
    NSString *k = nvmtl_fn_key(f); return k ? [@"compute:" stringByAppendingString:k] : nil;
}
NSString *nvmtl_archive_key_render(MTLRenderPipelineDescriptor *d) {
    NSString *v = nvmtl_fn_key(d.vertexFunction), *f = nvmtl_fn_key(d.fragmentFunction);
    return (v && f) ? [NSString stringWithFormat:@"render:%@|%@", v, f] : nil;
}
@implementation NVMTLBinaryArchive
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (BOOL)nvmtlAdd:(NSString *)key what:(const char *)what error:(NSError **)err {
    if (!key) { if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"binary archive: %s names a function this driver did not make - not added", what]); return NO; }
    @synchronized (self) { [_keys addObject:key]; }
    nvlog("binary archive %p: added %s", self, key.UTF8String);
    return YES;
}
- (BOOL)addComputePipelineFunctionsWithDescriptor:(MTLComputePipelineDescriptor *)d error:(NSError **)err {
    return [self nvmtlAdd:nvmtl_archive_key_compute(d.computeFunction) what:"addComputePipelineFunctions" error:err];
}
- (BOOL)addRenderPipelineFunctionsWithDescriptor:(MTLRenderPipelineDescriptor *)d error:(NSError **)err {
    return [self nvmtlAdd:nvmtl_archive_key_render(d) what:"addRenderPipelineFunctions" error:err];
}
- (BOOL)addTileRenderPipelineFunctionsWithDescriptor:(MTLTileRenderPipelineDescriptor *)d error:(NSError **)err {
    if (err) *err = nvmtl_b41_err(@"binary archive: tile render pipelines are not implemented by this driver - not added");
    return NO;
}
- (BOOL)addMeshRenderPipelineFunctionsWithDescriptor:(MTLMeshRenderPipelineDescriptor *)d error:(NSError **)err {
    if (err) *err = nvmtl_b41_err(@"binary archive: mesh render pipelines are not implemented by this driver yet - not added");
    return NO;
}
- (BOOL)addLibraryWithDescriptor:(MTLStitchedLibraryDescriptor *)d error:(NSError **)err {
    nvlog("binary archive %p: addLibraryWithDescriptor - a stitched library compiles on demand here; nothing to record", self);
    return YES;
}
- (BOOL)addFunctionWithDescriptor:(MTLFunctionDescriptor *)d library:(id<MTLLibrary>)lib error:(NSError **)err {
    return [self nvmtlAdd:[@"function:" stringByAppendingString:d.name ?: @"(nil)"] what:"addFunctionWithDescriptor" error:err];
}
- (BOOL)serializeToURL:(NSURL *)url error:(NSError **)err {
    NSArray *keys; @synchronized (self) { keys = [_keys.allObjects sortedArrayUsingSelector:@selector(compare:)]; }
    NSError *pe = nil;
    NSData *pl = [NSPropertyListSerialization dataWithPropertyList:@{@"format": @"NVMTLBinaryArchive", @"version": @1, @"keys": keys}
                                                            format:NSPropertyListBinaryFormat_v1_0 options:0 error:&pe];
    if (pl && [pl writeToURL:url options:NSDataWritingAtomic error:&pe]) { nvlog("binary archive %p: serialized %lu key(s) to %s", self, (unsigned long)keys.count, url.path.UTF8String); return YES; }
    if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"binary archive serializeToURL: %@ - %@", url.path, pe.localizedDescription ?: @"?"]);
    return NO;
}
@end
id nvmtl_new_binary_archive(MTLBinaryArchiveDescriptor *d, NSError **err) {
    if (err) *err = nil;
    NVMTLBinaryArchive *a = [NVMTLBinaryArchive new]; a->_keys = [NSMutableSet new];
    if (d.url) {
        NSData *data = [NSData dataWithContentsOfURL:d.url];
        if (!data) { if (err) *err = nvmtl_b41_err([NSString stringWithFormat:@"newBinaryArchiveWithDescriptor: cannot read %@", d.url.path ?: d.url.absoluteString]); return nil; }
        NSDictionary *pl = [NSPropertyListSerialization propertyListWithData:data options:0 format:NULL error:NULL];
        if ([pl isKindOfClass:[NSDictionary class]] && [pl[@"format"] isEqual:@"NVMTLBinaryArchive"] && [pl[@"keys"] isKindOfClass:[NSArray class]]) {
            for (id k in pl[@"keys"]) if ([k isKindOfClass:[NSString class]]) [a->_keys addObject:k];
        } else
            nvlog("newBinaryArchiveWithDescriptor: %s is not an archive this driver wrote (%lu B) - opened EMPTY, every lookup misses",
                  d.url.path.UTF8String, (unsigned long)data.length);
    }
    nvlog("newBinaryArchiveWithDescriptor: %s -> %p, %lu key(s)", d.url ? d.url.path.UTF8String : "(new)", a, (unsigned long)a->_keys.count);
    return a;
}
NSString *nvmtl_archive_refusal(NSArray *archives, NSString *key) {
    if (!archives.count) return nil;
    for (id a in archives)
        if (![a isKindOfClass:[NVMTLBinaryArchive class]]) {
            nvlog("binary archive: fail-on-miss against a foreign %s - NOT enforced", NSStringFromClass([a class]).UTF8String);
            return nil;
        }
    if (!key) return nil;
    for (NVMTLBinaryArchive *a in archives) { @synchronized (a) { if ([a->_keys containsObject:key]) return nil; } }
    return [NSString stringWithFormat:@"Unable to find %@ in binary archives (MTLPipelineOptionFailOnBinaryArchiveMiss, %lu archive(s) searched)",
            key, (unsigned long)archives.count];
}

#pragma mark - batch 42a: mesh shaders (compute emulation - NVK has no VK_EXT_mesh_shader)
static NSArray *nvmtl_lowered_stage_arguments(NVMTLComputePipelineState *kp, NVMTLFunction *stageFn) {
    if (!kp || !kp->_function || !stageFn) return @[];
    NSDictionary *names = nvmtl_air_names(stageFn);
    NSMutableArray *out = [NSMutableArray new];
    for (NVMTLArgument *a in nvmtl_arguments_for_function(kp->_function)) {
        if (a->_type == MTLBindingTypeBuffer && (a->_index == 29 || a->_index == 30)) continue;
        NSString *k = [NSString stringWithFormat:@"%@:%lu", a->_type == MTLBindingTypeBuffer ? @"buffer" : a->_type == MTLBindingTypeTexture ? @"texture" : @"sampler", (unsigned long)a->_index];
        NSString *n = names[@"args"][k]; if ([n isKindOfClass:[NSString class]]) a->_name = n;
        [out addObject:a];
    }
    return out;
}
@implementation NVMTLMeshInfo @end
static NSUInteger nvmtl_mesh_stage_max(NVMTLMeshInfo *mi, NVMTLComputePipelineState *k, NSUInteger rec, const char *stage) {
    if (rec) return rec;
    if (k) return [k maxTotalThreadsPerThreadgroup];
    uint32_t inv = 0; nvmtl_vk_limits(NULL, NULL, NULL, &inv, NULL);
    NSUInteger v = inv ?: nvmtl_compute_maxt();
    nvlog("mesh pipeline \"%s\": the %s stage's lowered kernel is out of reach - its limit answers the device's %lu", mi->_name.UTF8String, stage, (unsigned long)v);
    return v;
}
@implementation NVMTLRenderPipelineState (AppsPSOMesh)
- (NSUInteger)objectThreadExecutionWidth { NVMTLMeshInfo *mi = objc_getAssociatedObject(self, &kNVMTLMeshInfo);
    return mi && mi->_o ? [mi->_o threadExecutionWidth] : [self threadExecutionWidth]; }
- (NSUInteger)meshThreadExecutionWidth { NVMTLMeshInfo *mi = objc_getAssociatedObject(self, &kNVMTLMeshInfo);
    return mi && mi->_k ? [mi->_k threadExecutionWidth] : [self threadExecutionWidth]; }
- (NSUInteger)maxTotalThreadsPerObjectThreadgroup { NVMTLMeshInfo *mi = objc_getAssociatedObject(self, &kNVMTLMeshInfo);
    return mi && mi->_o ? nvmtl_mesh_stage_max(mi, mi->_o, mi->_tgObj, "object") : 0; }
- (NSUInteger)maxTotalThreadsPerMeshThreadgroup { NVMTLMeshInfo *mi = objc_getAssociatedObject(self, &kNVMTLMeshInfo);
    return mi ? nvmtl_mesh_stage_max(mi, mi->_k, mi->_tgMesh, "mesh") : 0; }
- (NSUInteger)maxTotalThreadgroupsPerMeshGrid { NVMTLMeshInfo *mi = objc_getAssociatedObject(self, &kNVMTLMeshInfo);
    return !mi ? 0 : mi->_o ? mi->_cap : (mi->_gridAsk ?: 1024); }
@end
@implementation NVMTLMeshBinds @end

static id nvmtl_mesh_fail(NSError **err, NSString *why) {
    nvlog("mesh pipeline: %s", why.UTF8String);
    if (err) *err = [NSError errorWithDomain:@"NVMTL" code:42 userInfo:@{NSLocalizedDescriptionKey: why}];
    return nil;
}

static NSData *nvmtl_mesh_translate(NSString *ll, NSString *stage, NSString **why) {
    uint32_t idx0 = 0, sz0 = 0; const uint8_t *pay0 = NULL;
    return nvmtl_translate_with_fc(ll, stage, &idx0, &sz0, &pay0, 0, why);
}

static NSData *nvmtl_mesh_translate_fc(NSString *ll, NSString *stage, NSDictionary *fc, NSString **why) {
    size_t n = fc.count;
    if (!n) return nvmtl_mesh_translate(ll, stage, why);
    NSMutableData *st = [NSMutableData dataWithLength:n * (2 * sizeof(uint32_t) + sizeof(void *))];
    uint32_t *idx = st.mutableBytes, *sz = idx + n; const uint8_t **pay = (const uint8_t **)(sz + n);
    NSMutableArray *keep = [NSMutableArray new]; size_t k = 0;
    for (NSString *key in fc) {
        NSArray *b = fc[key];
        if (![b isKindOfClass:[NSArray class]] || !b.count) { if (why) *why = [NSString stringWithFormat:@"function constant %@ has no bytes", key]; return nil; }
        NSMutableData *d = [NSMutableData dataWithLength:b.count];
        for (NSUInteger j = 0; j < b.count; j++) ((uint8_t *)d.mutableBytes)[j] = [b[j] unsignedCharValue];
        [keep addObject:d];
        idx[k] = (uint32_t)key.integerValue; sz[k] = (uint32_t)b.count; pay[k] = d.bytes; k++;
    }
    NSData *r = nvmtl_translate_with_fc(ll, stage, idx, sz, pay, n, why);
    (void)keep;
    return r;
}

static NSString *nvmtl_mesh_text(uint8_t *p, size_t n) {
    if (!p) return nil;
    NSString *s = [[NSString alloc] initWithBytes:p length:(n && p[n-1] == 0) ? n - 1 : n encoding:NSUTF8StringEncoding];
    xlate_free(p, n);
    return s;
}


