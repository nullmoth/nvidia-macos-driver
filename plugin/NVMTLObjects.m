/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#import <QuartzCore/QuartzCore.h>
#include "nvmtl_format_twin.h"
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

void nvlog(const char *fmt, ...);
void nvlog_every(const char *fmt, ...);   // a periodic census line, never dropped by the repeat limiter

#import "NVMTLObjects.h"
#import <objc/runtime.h>
#import <objc/message.h>
static int nvmtl_trace_on(void);
// NVMTL_CB_TIME=1 (dev builds): one line per command buffer with its host and GPU timeline, so an app's idle GPU time can
// be split into: not yet committed (app), committed but not started (driver submit), done but not reported (completion).
#if !NVMTL_RELEASE
static int nvmtl_cbt_on(void) { static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_CB_TIME"); on = e && e[0] == '1'; } return on; }
static void nvmtl_cbt_line(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void nvmtl_cbt_line(const char *fmt, ...) {
    static FILE *f; static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_lock(&mu);
    if (!f) { char fp[512]; const char *td = getenv("TMPDIR"); snprintf(fp, sizeof fp, "%s/nvmtl-cbtime-%d.log", td && *td ? td : "/private/tmp", (int)getpid()); f = fopen(fp, "a"); }
    if (f) { va_list ap; va_start(ap, fmt); vfprintf(f, fmt, ap); va_end(ap); }
    pthread_mutex_unlock(&mu);
}
#else
static inline int nvmtl_cbt_on(void) { return 0; }
#define nvmtl_cbt_line(...) do { } while (0)
#endif
static inline uint64_t nvmtl_cbt_now(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }
static inline void nvmtl_cbt_kernel(NVMTLCommandBuffer *cb, const char *name) {
    if (!cb || !nvmtl_cbt_on()) return;
    if (!cb->_cbtND++ && name) snprintf(cb->_cbtK, sizeof cb->_cbtK, "%s", name);
}
static void nvtrace_emit(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
// Guard the call site: converting NSString arguments on every draw is expensive
// even when the trace writer immediately returns without emitting anything.
#define nvtrace(...) do { if (nvmtl_trace_on()) nvtrace_emit(__VA_ARGS__); } while (0)
// A private lock, not @synchronized on the object: the object is the app's (a command buffer, an encoder), and an app that
// holds @synchronized on it while it waits for the scheduled handler deadlocked against the Metal HUD, which asks for the ID
// from that handler (Unity games with MetalForceHudEnabled: Soulstone Survivors hung on a black screen at start).
static unsigned long long nvmtl_global_trace_id(id o) {
    static char key; static unsigned long long next = 1; static os_unfair_lock lock = OS_UNFAIR_LOCK_INIT;
    os_unfair_lock_lock(&lock);
    NSNumber *n = objc_getAssociatedObject(o, &key);
    if (!n) { n = @(__atomic_fetch_add(&next, 1, __ATOMIC_RELAXED)); objc_setAssociatedObject(o, &key, n, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
    const unsigned long long id = n.unsignedLongLongValue;
    os_unfair_lock_unlock(&lock);
    return id;
}
@protocol NVMTLFunctionConstantInit
- (instancetype)initWithName:(NSString *)name type:(NSUInteger)type index:(NSUInteger)index required:(BOOL)required;
@end
static MTLDataType nvmtl_air_fc_type(NSString *t) {
    static NSDictionary *base; static dispatch_once_t once;
    dispatch_once(&once, ^{ base = @{ @"float": @(MTLDataTypeFloat), @"half": @(MTLDataTypeHalf), @"bfloat": @(MTLDataTypeBFloat),
        @"int": @(MTLDataTypeInt), @"uint": @(MTLDataTypeUInt), @"short": @(MTLDataTypeShort), @"ushort": @(MTLDataTypeUShort),
        @"char": @(MTLDataTypeChar), @"uchar": @(MTLDataTypeUChar), @"bool": @(MTLDataTypeBool),
        @"long": @(MTLDataTypeLong), @"ulong": @(MTLDataTypeULong) }; });
    NSNumber *b = base[t];
    if (b) return (MTLDataType)b.unsignedIntegerValue;
    unichar last = t.length > 1 ? [t characterAtIndex:t.length - 1] : 0;
    if (last >= '2' && last <= '4' && (b = base[[t substringToIndex:t.length - 1]]))
        return (MTLDataType)(b.unsignedIntegerValue + (NSUInteger)(last - '1'));
    return MTLDataTypeNone;
}
#include <CommonCrypto/CommonDigest.h>

#define NVMTL_SET_VERTEX   0u
#define NVMTL_SET_FRAGMENT 1u

@class NVMTLBuffer;
static void nvmtl_buffer_sync_in(NVMTLBuffer *b);
@class NVMTLCommandBuffer;
static void nvmtl_buffer_sync_out(NVMTLBuffer *b);
static void nvbt(const char *tag, int *count, int max) {
    if (*count >= max) return; (*count)++;
    extern const char *nvmtl_log_path(void);
    FILE *f = fopen(nvmtl_log_path(), "a"); if (!f) return;
    NSArray *syms = [NSThread callStackSymbols];
    fprintf(f, "pid %d NVMTL: BT %s #%d (%lu frames)\n", getpid(), tag, *count, (unsigned long)syms.count);
    for (NSUInteger i = 1; i < syms.count && i < 14; i++) fprintf(f, "pid %d NVMTL:    %s\n", getpid(), [syms[i] UTF8String]);
    fclose(f);
}
static int g_bt_status, g_bt_enqueue, g_bt_handler, g_bt_release, g_bt_commit;

#pragma mark - buffer
static NSArray *nvmtl_vertex_attributes_for(NVMTLFunction *fn);
#include <os/lock.h>
static os_unfair_lock gManagedLock = OS_UNFAIR_LOCK_INIT;
static NSHashTable *gManagedDirty;
static _Atomic int gManagedAny;
static __thread int gManagedInFlush;
static os_unfair_lock gPurgeLock = OS_UNFAIR_LOCK_INIT;
static NSHashTable *gPurgeVol;
static _Atomic uint64_t gPurgeAsks;
extern void (*nvmtl_purge_report_hook)(const char *why);
static void nvmtl_purge_report(const char *why);

static _Atomic uint64_t gRes2Tick;
static void nvmtl_res2_note_use(NVMTLTexture *t);
static uint64_t nvmtl_res2_evict(uint64_t want);
extern uint64_t (*nvmtl_res2_evict_hook)(uint64_t want);
#include "nvmtl_env_knobs.h"
#include "nvmtl_repeat_bind.h"
#include "nvmtl_late_frames.h"
#include <IOKit/IOKitLib.h>
#include "nvmtl_fps_limit.h"
#include "nvmtl_shared_wc.h"
// The keys the user's own block of nvrm610.conf set ("my settings, kept on update"): an application profile never
// replaces them. The shipped lines above that block are defaults, which a profile does replace.
static char g_conf_mine[64][48];
static unsigned g_conf_nmine;
static void nvmtl_conf_note_mine(const char *key)
{
    for (unsigned i = 0; i < g_conf_nmine; i++) if (!strcmp(g_conf_mine[i], key)) return;
    if (g_conf_nmine < 64 && strlen(key) < sizeof g_conf_mine[0]) strcpy(g_conf_mine[g_conf_nmine++], key);
}
// Forgets what this process inherited and should not keep; returns how many conf-set knobs a parent passed down.
static unsigned nvmtl_conf_forget_inherited(void)
{
#if NVMTL_RELEASE
    // Inherited driver knobs are forgotten unless this process set NVMTL_PROCESS_ENV=1 (nvmtl_env_knobs.h has the rule).
    { extern char **environ; nvmtl_release_forget_env(environ); }
#endif
    unsigned forgot = 0;
    { const char *rec = getenv("NVMTL_CONF_SET");
      if (rec && *rec) { char *dup = strdup(rec), *save = NULL;
          for (char *kv = dup ? strtok_r(dup, ";", &save) : NULL; kv; kv = strtok_r(NULL, ";", &save)) {
              char *eq = strchr(kv, '='); if (!eq) continue; *eq = 0;
              const char *now = getenv(kv);
              if (now && !strcmp(now, eq + 1)) { unsetenv(kv); forgot++; }
          }
          free(dup); }
      unsetenv("NVMTL_CONF_SET"); }
    return forgot;
}
static void nvmtl_conf_read_file(unsigned forgot)
{
    char recbuf[2048]; size_t recn = 0; recbuf[0] = 0;
    FILE *f = fopen("/Library/GPUBundles/nvmtl/nvrm610.conf", "r");
    if (!f) { if (forgot) nvlog("conf: forgot %u inherited knob(s); no conf file", forgot); return; }
    char line[512];
    unsigned took = 0;
    // The last line for a key wins: the installer keeps the user's own block ("my settings, kept on update") at the end
    // of the file, and the first match used to win, so a shipped value of the same key above it silently beat the
    // user's (#399). Keys set by an earlier line of this file may be replaced; the real environment still wins.
    char fromFile[64][48]; unsigned nFromFile = 0;
    int mine = 0;   // inside the user's own block (the installer's markers, package/install.sh)
    while (fgets(line, sizeof line, f)) {
        char *p = line;
        while (*p == ' ' || *p == '\t') p++;
        if (!strncmp(p, "# --- my settings, kept on update ---", 37)) { mine = 1; continue; }
        if (!strncmp(p, "# --- end my settings ---", 25)) { mine = 0; continue; }
        if (*p == '#' || *p == '\n' || !*p) continue;
        char *eq = strchr(p, '=');
        if (!eq) continue;
        *eq = 0;
        char *key = p, *val = eq + 1;
        char *end = key + strlen(key);
        while (end > key && (end[-1] == ' ' || end[-1] == '\t')) *--end = 0;
        end = val + strlen(val);
        while (end > val && (end[-1] == '\n' || end[-1] == '\r' || end[-1] == ' ' || end[-1] == '\t')) *--end = 0;
        if (strncmp(key, "NVMTL_", 6) && strncmp(key, "NVK_", 4) && strncmp(key, "NVRM_", 5)) continue;
        bool ours = false;
        for (unsigned i = 0; i < nFromFile; i++) if (!strcmp(fromFile[i], key)) { ours = true; break; }
        if (getenv(key) && !ours) continue;
        if (setenv(key, val, 1) == 0) { if (mine) nvmtl_conf_note_mine(key);
            if (ours) { nvlog("conf: %s=%s (a later line of the file replaces an earlier one)", key, val); continue; }
            took++;
            if (nFromFile < 64 && strlen(key) < sizeof fromFile[0]) strcpy(fromFile[nFromFile++], key); }
    }
    fclose(f);
    // NVMTL_CONF_SET records the values this file set, as they stand after every line, so a child process forgets
    // exactly these (an override recorded at its first value would no longer match and be kept as if user-set)
    for (unsigned i = 0; i < nFromFile; i++) {
        const char *v = getenv(fromFile[i]); if (!v) continue;
        int k = snprintf(recbuf + recn, sizeof recbuf - recn, "%s%s=%s", recn ? ";" : "", fromFile[i], v);
        if (k > 0 && (size_t)k < sizeof recbuf - recn) recn += (size_t)k; else { recbuf[recn] = 0; break; }
    }
    if (recn) setenv("NVMTL_CONF_SET", recbuf, 1);
    if (took || forgot) nvlog("conf: %u knob(s) read from /Library/GPUBundles/nvmtl/nvrm610.conf, %u inherited conf knob(s) forgotten first (the environment still wins)", took, forgot);
}
// Which profile keys the user set: in this process's kept environment before the conf file was read (bit i = knob i),
// or in the user's block of the conf file.
typedef struct { const nvmtl_app_profile *p; uint64_t envBits; } nvmtl_profile_ctx;
static int nvmtl_profile_user_set(const char *key, void *vctx)
{
    const nvmtl_profile_ctx *c = vctx;
    for (unsigned i = 0; i < c->p->nknobs && i < 64; i++)
        if (!strcmp(c->p->knobs[i].key, key)) { if ((c->envBits >> i) & 1) return 1; break; }
    for (unsigned i = 0; i < g_conf_nmine; i++) if (!strcmp(g_conf_mine[i], key)) return 1;
    return 0;
}
// NVMTL_OS_VERSION=<major>[.<minor>[.<patch>]]: this process is told the operating system is at least that version
// ("0" = never, even for a profile). The application profile can carry a version too (nvmtl_app_profile.h): Cyberpunk
// 2077 reads -[NSProcessInfo operatingSystemVersion] once in device set-up and offers MetalFX frame generation only above
// macOS 26. That read happens before Metal loads this plugin, so the answer that counts is installed earlier, by
// libnvmtl_osver.dylib (nvmtl_osver.m), which the game's own build loads. This is the same answer installed again when
// the plugin loads, for a build without that library and for a process that sets the knob; it does nothing when the
// library already answered. The previous version replaced the method on NSProcessInfo only, which the real instance's
// class (a private subclass with its own implementation) never reached.
static void nvmtl_os_version_override(const char *me, const char *bid)
{
    int *done = dlsym(RTLD_DEFAULT, "nvmtl_osver_installed");
    if (done && *done) { nvlog("os version: already answered by libnvmtl_osver.dylib"); return; }
    const NSOperatingSystemVersion r = [[NSProcessInfo processInfo] operatingSystemVersion];
    const long real[3] = { (long)r.majorVersion, (long)r.minorVersion, (long)r.patchVersion };
    const char *env = getenv("NVMTL_OS_VERSION");
    long v[3];
    if (!nvmtl_os_version_answer(me, bid, getenv("NVMTL_APP_PROFILE"), env, real, v)) return;
    const int n = nvmtl_osver_install((NSOperatingSystemVersion){ v[0], v[1], v[2] });
    if (!n) { nvlog("os version: FAILED - NSProcessInfo has no operatingSystemVersion to replace"); return; }
    nvlog("os version: this process is told %ld.%ld.%ld instead of %ld.%ld.%ld (%s, installed late: reads made before the "
          "plugin loaded saw the real version)", v[0], v[1], v[2], real[0], real[1], real[2], env ? "NVMTL_OS_VERSION" : "application profile");
}
// Settles this process's driver knobs once, before any of them is read: forget what was inherited, read nvrm610.conf,
// then apply the application profile for this executable, if there is one (nvmtl_app_profile.h has the order of
// precedence). Every constructor that reads a knob calls this first; the first call does the work.
static void nvmtl_conf_settle(void)
{
    static int done;
    if (done) return;
    done = 1;
    unsigned forgot = nvmtl_conf_forget_inherited();
    const char *me = getprogname();
    // The main bundle's identifier; a process outside any bundle has none and gets no profile.
    char bid[256] = "";
    { CFBundleRef mb = CFBundleGetMainBundle(); CFStringRef s = mb ? CFBundleGetIdentifier(mb) : NULL;
      if (s && !CFStringGetCString(s, bid, sizeof bid, kCFStringEncodingUTF8)) bid[0] = 0; }
    const nvmtl_app_profile *p = nvmtl_app_profile_for(me, bid, getenv("NVMTL_APP_PROFILE"));
    nvmtl_profile_ctx ctx = { p, 0 };
    for (unsigned i = 0; p && i < p->nknobs && i < 64; i++) if (getenv(p->knobs[i].key)) ctx.envBits |= 1ull << i;
    nvmtl_conf_read_file(forgot);
    nvmtl_os_version_override(me, bid);
    if (p && !nvmtl_app_profile_for(me, bid, getenv("NVMTL_APP_PROFILE"))) {
        nvlog("profile: %s: off (NVMTL_APP_PROFILE=0)", me);
        p = NULL;
    }
    if (!p) return;
    // The profile's values join the conf file's record, so a child process (a crash reporter, a helper) forgets them.
    char rec[3072]; const char *had = getenv("NVMTL_CONF_SET");
    snprintf(rec, sizeof rec, "%s", had ? had : "");
    unsigned kept = 0, set = nvmtl_app_profile_apply(p, nvmtl_profile_user_set, &ctx, rec, sizeof rec, &kept);
    // Settings written and tested offline but not yet measured on a card: only for a user who asks (NVMTL_HWRT=1).
    { unsigned kept2 = 0; set += nvmtl_app_profile_apply_optin(p, getenv("NVMTL_HWRT"), nvmtl_profile_user_set, &ctx, rec, sizeof rec, &kept2); kept += kept2; }
    // The application gets its own shader-cache root (aircache, with spvcache beside it), as a launcher that sets
    // NVMTL_AIRCACHE does. A cache folder shared with every other process mixed entries of different modes. A cache the
    // user named (the process environment or nvrm610.conf) wins.
    char root[1024] = "", air[1100] = "";
    if (!getenv("NVMTL_AIRCACHE")) {
        NSArray *c = NSSearchPathForDirectoriesInDomains(NSCachesDirectory, NSUserDomainMask, YES);
        const char *caches = c.count ? [c[0] fileSystemRepresentation] : NULL;
        if (caches && nvmtl_app_cache_root(root, sizeof root, caches, bid)) {
            snprintf(air, sizeof air, "%s/aircache", root);
            char spv[1100]; snprintf(spv, sizeof spv, "%s/spvcache", root);
            [[NSFileManager defaultManager] createDirectoryAtPath:@(air) withIntermediateDirectories:YES
                                                       attributes:@{ NSFilePosixPermissions: @0700 } error:nil];
            [[NSFileManager defaultManager] createDirectoryAtPath:@(spv) withIntermediateDirectories:YES
                                                       attributes:@{ NSFilePosixPermissions: @0700 } error:nil];
            chmod(root, 0700); chmod(air, 0700); chmod(spv, 0700);
            if (access(air, W_OK) == 0 && setenv("NVMTL_AIRCACHE", air, 1) == 0) {
                size_t n = strlen(rec);
                if (n + strlen(air) + 18 < sizeof rec) snprintf(rec + n, sizeof rec - n, "%sNVMTL_AIRCACHE=%s", n ? ";" : "", air);
            } else air[0] = 0;
        }
    }
    if (rec[0]) setenv("NVMTL_CONF_SET", rec, 1);
    g_app_profile = p;
    nvlog("profile: %s (%s): %u setting(s) applied, %u left as the user set them%s; shader cache %s", me, bid, set, kept,
          p->shader_from ? "; one kernel edit armed" : "", air[0] ? air : (getenv("NVMTL_AIRCACHE") ?: "shared"));
}
__attribute__((constructor)) static void nvmtl_conf_knobs(void) { nvmtl_conf_settle(); }
static int nvmtl_dontcare_clears(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_DONTCARE_CLEARS"); on = (e && *e && *e != '0') ? 1 : 0; }
    return on;
}
static inline int nvmtl_load_keeps(MTLLoadAction a) {
    return a == MTLLoadActionLoad || (a == MTLLoadActionDontCare && !nvmtl_dontcare_clears());
}
static void nvmtl_res3_note_use(NVMTLBuffer *b);
static uint64_t nvmtl_ep_now(void);
static inline void nvmtl_res3_wait_move(int *rs) { while (__atomic_load_n(rs, __ATOMIC_ACQUIRE) == 2) sched_yield(); }
static void nvmtl_res2_kick(int scan);
static _Atomic int gRes2Scan;
static Class gNVMTLTexClass;
static inline void nvmtl_resref(id o, int d) {
    if (!gNVMTLTexClass) gNVMTLTexClass = [NVMTLTexture class];
    if (![o isKindOfClass:gNVMTLTexClass]) {
        static Class BC; if (!BC) BC = [NVMTLBuffer class];
        if (object_getClass(o) != BC) return;
        NVMTLBuffer *b = o;
        const int v = __atomic_add_fetch(&b->_resRefs, d, __ATOMIC_ACQ_REL);
        if (d > 0) { __atomic_store_n(&b->_useEp, nvmtl_ep_now(), __ATOMIC_RELEASE); nvmtl_res3_wait_move(&b->_rstate); nvmtl_res3_note_use(b); }
        else if (v == 0 && __atomic_load_n(&b->_wantOn, __ATOMIC_ACQUIRE)) nvmtl_res2_kick(1);
        return;
    }
    NVMTLTexture *t = o; if (t->_parent) t = t->_parent;
    const int v = __atomic_add_fetch(&t->_resRefs, d, __ATOMIC_ACQ_REL);
    if (d > 0) { __atomic_store_n(&t->_useEp, nvmtl_ep_now(), __ATOMIC_RELEASE); nvmtl_res3_wait_move(&t->_rstate);
                 nvmtl_res2_note_use(t); }
    else if (v == 0 && __atomic_load_n(&t->_wantOn, __ATOMIC_ACQUIRE)) nvmtl_res2_kick(1);
}
// A Metal object is equal only to itself, so the set hashes and compares pointers: NSMutableSet sends -hash and
// -isEqual: to every object it probes, two message sends per lookup on every bind of every draw (addObject: was the
// top plugin leaf on a game's render-encoding threads). CFRetain/CFRelease keep the set's ownership unchanged.
static NSMutableSet *nvmtl_identity_set(NSUInteger cap) {
    CFSetCallBacks cb = kCFTypeSetCallBacks; cb.equal = NULL; cb.hash = NULL;
    return (__bridge_transfer NSMutableSet *)CFSetCreateMutable(kCFAllocatorDefault, (CFIndex)cap, &cb);
}
@implementation NVMTLResSet
- (instancetype)init { if ((self = [super init])) _s = nvmtl_identity_set(0); return self; }
- (instancetype)initWithCapacity:(NSUInteger)n { if ((self = [super init])) _s = nvmtl_identity_set(n); return self; }
// 10-07 (a user's crash report, RTX 5060 Ti): Hackintool died in -[__NSSetM member:] under -addObject:, called
// from setFragmentTexture on CoreAnimation's async-render workqueue thread. NSMutableSet is not thread-safe and nothing
// here serialised it, so every operation now holds _lk. Enumeration hands out a snapshot, never the live set.
- (NSUInteger)count { os_unfair_lock_lock(&_lk); NSUInteger n = _s.count; os_unfair_lock_unlock(&_lk); return n; }
- (id)member:(id)o { os_unfair_lock_lock(&_lk); id m = [_s member:o]; os_unfair_lock_unlock(&_lk); return m; }
- (NSArray *)nvmtlSnapshot { os_unfair_lock_lock(&_lk); NSArray *a = _s.allObjects; os_unfair_lock_unlock(&_lk); return a; }
- (NSEnumerator *)objectEnumerator { return [[self nvmtlSnapshot] objectEnumerator]; }
// for-in runs only over a command buffer part's own copy (one thread), never over a set still being encoded into
- (NSUInteger)countByEnumeratingWithState:(NSFastEnumerationState *)st objects:(id __unsafe_unretained [])b count:(NSUInteger)n {
    return [_s countByEnumeratingWithState:st objects:b count:n]; }
// _mru: objects added lately, each in the set until it is removed (the set retains it, so its address cannot be reused
// while it is listed). Encoders add the same few buffers, pipelines and samplers draw after draw (Apple's GL rebinds its
// vertex, index and uniform buffers for every draw), and a hit skips the set's hash lookup.
- (void)addObject:(id)o { if (!o) return; const void *p = (__bridge const void *)o; os_unfair_lock_lock(&_lk);
    for (unsigned k = 0; k < 8; k++) if (_mru[k] == p) { os_unfair_lock_unlock(&_lk); return; }
    // A malloc_size() test used to sit here, meant to catch a freed resource before -member: calls
    // -hash on it. It cannot: a freed object's block normally stays a valid block in the default zone,
    // so malloc_size returns its old size and the test passes - while a tagged pointer, or any object
    // outside the default zone, reads 0 and a LIVE resource would be dropped untracked, manufacturing
    // the very use-after-free the test existed to prevent. A zone lookup under the hottest lock in the
    // driver, that both misses the bug and invents one, is worse than no test.
    id member = [_s member:o]; BOOL had = member != nil;
    if (!had) { [_s addObject:o]; member = o; }
    // An equal object need not be the object retained by NSMutableSet. Cache only the retained member's address.
    _mru[_mruNext++ & 7] = (__bridge const void *)member;
    os_unfair_lock_unlock(&_lk); if (!had) nvmtl_resref(o, 1); }
- (void)removeObject:(id)o { if (!o) return; os_unfair_lock_lock(&_lk); id member = [_s member:o];
    if (member) [_s removeObject:member];
    for (unsigned k = 0; k < 8; k++) if (_mru[k] == (__bridge const void *)member) _mru[k] = NULL;
    os_unfair_lock_unlock(&_lk); if (member) nvmtl_resref(member, -1); }
// Clearing, copying and sealing never go through -allObjects. That array is autoreleased into the CALLER's pool, and
// these run on the app's own commit thread: Geekbench 7 never drains that pool, so every resource a command buffer had
// ever touched stayed alive, and Super Resolution filled VRAM with 9 MB MPS buffers until allocations were refused.
// The sets below are owned (+1) and released at the end of scope.
- (void)removeAllObjects { NSMutableSet *fresh = nvmtl_identity_set(0), *old;
    os_unfair_lock_lock(&_lk); old = _s; _s = fresh; memset(_mru, 0, sizeof _mru); os_unfair_lock_unlock(&_lk);
    for (id o in old) nvmtl_resref(o, -1); }
- (id)copyWithZone:(NSZone *)z { atomic_fetch_add(&gRes2Tick, 1);
    NVMTLResSet *c = [NVMTLResSet new];
    os_unfair_lock_lock(&_lk); NSMutableSet *m = [_s mutableCopy]; os_unfair_lock_unlock(&_lk);
    c->_s = m;
    for (id o in m) nvmtl_resref(o, 1);
    return c; }
- (id)mutableCopyWithZone:(NSZone *)z { return [self copyWithZone:z]; }
// Moves every member into a new set and empties this one in a single locked step. Sealing a submission part used to
// copy and then clear in two steps, so a resource another thread bound in between was released by the clear without
// being in the part: the command buffer stopped holding it while the GPU work still used it. Membership moves to one
// new owner, so each resource's residency count is unchanged; the new set's -dealloc gives it back.
- (NVMTLResSet *)nvmtlTakeAll { atomic_fetch_add(&gRes2Tick, 1);
    NVMTLResSet *taken = [NVMTLResSet new]; NSMutableSet *empty = taken->_s;
    os_unfair_lock_lock(&_lk); taken->_s = _s; _s = empty; memset(_mru, 0, sizeof _mru); os_unfair_lock_unlock(&_lk);
    return taken; }
- (void)dealloc { for (id o in _s) nvmtl_resref(o, -1); }
@end

static BOOL nvmtl_purge_eligible(NVMTLTexture *t) {
    return !t->_parent && !t->_viewParent && !t->_backBuf && !t->_heap && !t->_subHeap && !t->_surf && !t->_vramOn && !t->_ownView
        && !t->_shareable && !t->_resid && !__atomic_load_n(&t->_nviews, __ATOMIC_ACQUIRE) && t->_i.img && !t->_i.placed
        && t->_i.usage && !t->_i.vfl && t->_i.samples <= 1 && !(t->_i.usage & 0x20u )
        // a swizzled sampling view (MTLTextureDescriptor.swizzle) is made against this VkImage; re-homing the memory
        // recreates the image and would leave that view pointing at the destroyed one (the depth-only view likewise)
        && !t->_swzView && !t->_sampledDepthView;
}
static uint64_t nvmtl_purge_reclaim(uint64_t need) {
    NSArray *all = nil;
    os_unfair_lock_lock(&gPurgeLock); all = gPurgeVol.count ? gPurgeVol.allObjects : nil; os_unfair_lock_unlock(&gPurgeLock);
    if (!all.count) return 0;
    uint64_t freed = 0; unsigned n = 0, busy = 0, inel = 0, fail = 0;
    for (int pass = 0; pass < 2 && freed < need; pass++)
        for (id o in all) {
            if (freed >= need) break;
            if (![o isKindOfClass:[NVMTLTexture class]]) continue;
            NVMTLTexture *t = o;
            uint32_t st = pass ? MTLPurgeableStateVolatile : MTLPurgeableStateEmpty;
            if (__atomic_load_n(&t->_purge, __ATOMIC_ACQUIRE) != st || t->_i.sysmem) continue;
            if (!nvmtl_purge_eligible(t)) { inel++; continue; }
            int z = 0; if (!__atomic_compare_exchange_n(&t->_rstate, &z, 2, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) continue;
            uint32_t want = st;
            if (!__atomic_compare_exchange_n(&t->_purge, &want, (uint32_t)MTLPurgeableStateEmpty, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                __atomic_store_n(&t->_rstate, 0, __ATOMIC_RELEASE); continue; }
            if (__atomic_load_n(&t->_resRefs, __ATOMIC_ACQUIRE)) {
                busy++; uint32_t e = MTLPurgeableStateEmpty;
                __atomic_compare_exchange_n(&t->_purge, &e, st, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
                __atomic_store_n(&t->_rstate, 0, __ATOMIC_RELEASE); continue; }
            uint64_t was = t->_i.alloc;
            if (nvmtl_vk_image_reback(&t->_i, 1, t->_fmt == MTLPixelFormatA8Unorm) == 0) {
                freed += was; n++; __atomic_store_n(&t->_rstate, 1, __ATOMIC_RELEASE);
            } else {
                fail++; uint32_t e = MTLPurgeableStateEmpty;
                __atomic_compare_exchange_n(&t->_purge, &e, st, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
                __atomic_store_n(&t->_rstate, 0, __ATOMIC_RELEASE); }
        }
    uint64_t s[6]; nvmtl_vk_res1_stats(s);
    nvlog("residency (res1): the wall is %llu MB short -> purged %u textures, %llu MB of VRAM given back (Empty/Volatile -> system memory, "
          "contents discarded as Metal allows); %u in use by a command buffer, %u not movable, %u rebuild failed; ever: %llu purged "
          "(%llu MB), %llu paged back on (%llu MB), %llu page-ons refused, %llu wall calls",
          (unsigned long long)(need >> 20), n, (unsigned long long)(freed >> 20), busy, inel, fail,
          (unsigned long long)s[0], (unsigned long long)s[3], (unsigned long long)s[1], (unsigned long long)s[4],
          (unsigned long long)s[2], (unsigned long long)s[5]);
    return freed;
}
extern uint64_t (*nvmtl_reclaim_hook)(uint64_t need);
static int nvmtl_res1_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_NO_PURGE_RECLAIM"); on = !(e && e[0] == '1');
        nvlog("residency (res1): purge under VRAM pressure %s (NVMTL_NO_PURGE_RECLAIM=%s)", on ? "ON" : "OFF", e ? e : "unset"); }
    return on;
}

#define NVMTL_EPN 256
static _Atomic uint64_t gRes2Epoch = 1, gRes2Drained;
static _Atomic int gRes2EpLive[NVMTL_EPN];
static uint64_t nvmtl_ep_enter(void) { const uint64_t e = atomic_load(&gRes2Epoch); atomic_fetch_add(&gRes2EpLive[e % NVMTL_EPN], 1); return e; }
static _Atomic unsigned gGraveN;
static void nvmtl_res2_cb_done(NVMTLCommandBuffer *cb) {
    const uint64_t e = __atomic_exchange_n(&cb->_rep, 0, __ATOMIC_ACQ_REL);
    if (!e) return;
    atomic_fetch_sub(&gRes2EpLive[e % NVMTL_EPN], 1);
    if (atomic_load(&gGraveN) || atomic_load(&gRes2Scan)) nvmtl_res2_kick(0);
}
static void nvmtl_ep_drain(void) {
    const uint64_t cur = atomic_load(&gRes2Epoch); uint64_t d = atomic_load(&gRes2Drained);
    while (d + 1 < cur && atomic_load(&gRes2EpLive[(d + 1) % NVMTL_EPN]) == 0) d++;
    atomic_store(&gRes2Drained, d);
}
static uint64_t nvmtl_ep_now(void) { return atomic_load(&gRes2Epoch); }
static BOOL nvmtl_ep_room(void) { return atomic_load(&gRes2Epoch) + 2 - atomic_load(&gRes2Drained) < NVMTL_EPN; }
typedef struct { nvk_image img; uint64_t ep; } nvmtl_grave;
static os_unfair_lock gGraveLock = OS_UNFAIR_LOCK_INIT;
static nvmtl_grave *gGrave; static unsigned gGraveCap; static uint64_t gGraveBytes, gGraveVram, gGraveFreed;
static void nvmtl_res2_reap(void) {
    nvmtl_ep_drain();
    const uint64_t d = atomic_load(&gRes2Drained);
    os_unfair_lock_lock(&gGraveLock);
    unsigned n = atomic_load(&gGraveN), k = 0;
    for (unsigned j = 0; j < n; j++) {
        if (gGrave[j].ep <= d) {
            gGraveBytes -= gGrave[j].img.alloc; if (!gGrave[j].img.sysmem) gGraveVram -= gGrave[j].img.alloc;
            nvmtl_vk_image_free_backing(&gGrave[j].img); gGraveFreed++;
        } else gGrave[k++] = gGrave[j];
    }
    atomic_store(&gGraveN, k);
    os_unfair_lock_unlock(&gGraveLock);
}
static void nvmtl_grave_put(const nvk_image *old) {
    os_unfair_lock_lock(&gGraveLock);
    unsigned n = atomic_load(&gGraveN);
    if (n == gGraveCap) { unsigned c = gGraveCap ? gGraveCap * 2 : 64; nvmtl_grave *g = realloc(gGrave, c * sizeof *g); if (g) { gGrave = g; gGraveCap = c; } }
    if (n < gGraveCap) {
        gGrave[n].img = *old; gGrave[n].ep = atomic_load(&gRes2Epoch);
        gGraveBytes += old->alloc; if (!old->sysmem) gGraveVram += old->alloc;
        atomic_store(&gGraveN, n + 1);
    } else { nvk_image leak = *old; (void)leak;
        nvlog("residency (res2): graveyard realloc FAILED - an old %llu MB backing is LEAKED, not freed in use", (unsigned long long)(old->alloc >> 20)); }
    os_unfair_lock_unlock(&gGraveLock);
}
static int gRes2On = -1; static uint64_t gRes2Cold, gRes2Hot, gRes2Margin;
static int nvmtl_res2_on(void) {
    if (gRes2On < 0) {
        const char *e = getenv("NVMTL_NO_RES2"), *c = getenv("NVMTL_RES2_COLD"), *h = getenv("NVMTL_RES2_HOT"), *m = getenv("NVMTL_RES2_MARGIN_MB");
        gRes2Cold = c && *c ? strtoull(c, NULL, 10) : 600;
        gRes2Hot = h && *h ? strtoull(h, NULL, 10) : 120;
        gRes2Margin = (m && *m ? strtoull(m, NULL, 10) : 256) << 20;
        gRes2On = !(e && e[0] == '1');
        nvlog("residency (res2): LRU page-off/page-on %s (NVMTL_NO_RES2=%s) - cold %llu ticks, hot %llu ticks, margin %llu MB",
              gRes2On ? "ON" : "OFF", e ? e : "unset", (unsigned long long)gRes2Cold, (unsigned long long)gRes2Hot, (unsigned long long)(gRes2Margin >> 20));
    }
    return gRes2On;
}
static BOOL nvmtl_res2_movable(NVMTLTexture *t) {
    return !t->_parent && !t->_viewParent && !t->_backBuf && !t->_heap && !t->_subHeap && !t->_surf && !t->_vramOn && !t->_ownView
        && !t->_shareable && !__atomic_load_n(&t->_nviews, __ATOMIC_ACQUIRE) && t->_i.img && !t->_i.placed && t->_i.usage
        && !t->_i.vfl && t->_i.samples <= 1 && !(t->_i.usage & 0x20u )
        // the move rebuilds _i.view and the slot only: a swizzled or depth-only view of the old image would outlive it,
        // and the slot rewrite would drop the swizzle (tools/check_rehome_views.py)
        && !t->_swzView && !t->_sampledDepthView;
}
static os_unfair_lock gRes2RegLock = OS_UNFAIR_LOCK_INIT;
static NSHashTable *gRes2Reg;
extern void (*nvmtl_res2_pressure_hook)(uint64_t want);
static _Atomic uint64_t gRes2Want;
static void nvmtl_res2_pressure(uint64_t want) {
    uint64_t w = atomic_load(&gRes2Want); while (want > w && !atomic_compare_exchange_weak(&gRes2Want, &w, want)) {}
    nvmtl_res2_kick(1);
}
static void nvmtl_res2_note_use(NVMTLTexture *t) {
    __atomic_store_n(&t->_lastUse, atomic_load(&gRes2Tick), __ATOMIC_RELAXED);
    if (!__atomic_load_n(&t->_lruReg, __ATOMIC_ACQUIRE)) {
        int z = 0;
        if (__atomic_compare_exchange_n(&t->_lruReg, &z, 1, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE) && nvmtl_res2_on() && nvmtl_res2_movable(t)) {
            os_unfair_lock_lock(&gRes2RegLock);
            if (!gRes2Reg) gRes2Reg = [NSHashTable weakObjectsHashTable];
            [gRes2Reg addObject:t];
            os_unfair_lock_unlock(&gRes2RegLock);
            if (!nvmtl_res2_pressure_hook) nvmtl_res2_pressure_hook = nvmtl_res2_pressure;
            if (!nvmtl_res2_evict_hook) nvmtl_res2_evict_hook = nvmtl_res2_evict;
        }
    }
    if (t->_i.sysmem && !__atomic_load_n(&t->_rstate, __ATOMIC_ACQUIRE) && !__atomic_load_n(&t->_wantOn, __ATOMIC_ACQUIRE)
        && __atomic_load_n(&t->_lruReg, __ATOMIC_ACQUIRE) && nvmtl_res2_on() && nvmtl_res2_movable(t))
        __atomic_store_n(&t->_wantOn, 1, __ATOMIC_RELEASE);
}
static dispatch_queue_t gRes2Q; static _Atomic int gRes2Pending, gRes2Scan; static _Atomic uint64_t gRes2LastNs;
static void nvmtl_res2_pass(void);
static os_unfair_lock gRes2PassLock = OS_UNFAIR_LOCK_INIT;
static __thread int gRes2InPass;
static uint64_t gRes2EvictCalls, gRes2EvictBytes;
static _Atomic int gRes2Force;
static void nvmtl_res2_pass_guarded(int from_worker) {
    if (gRes2InPass) return;
    if (!os_unfair_lock_trylock(&gRes2PassLock)) {
        if (from_worker) { nvmtl_res2_kick(1); return; }
        const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW); int got = 0;
        while (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0 < 30000000ull) { usleep(200); if (os_unfair_lock_trylock(&gRes2PassLock)) { got = 1; break; } }
        if (!got) { static unsigned said; if (said++ < 4) nvlog("residency (res2): allocating thread waited 30 ms for the pager and gave up - this allocation spills"); return; }
    }
    gRes2InPass = 1;
    nvmtl_res2_pass();
    gRes2InPass = 0;
    os_unfair_lock_unlock(&gRes2PassLock);
}
static uint64_t nvmtl_res2_evict(uint64_t want) {
    const int force = (int)(want >> 63); want &= ~(1ull << 63);
    if (gRes2On != 1 || !want || gRes2InPass) return 0;
    if (force) atomic_store(&gRes2Force, 1);
    const uint64_t before = nvmtl_vk_vram_free_now();
    uint64_t w = atomic_load(&gRes2Want);
    do { if (want <= w) break; } while (!atomic_compare_exchange_weak(&gRes2Want, &w, want));
    atomic_store(&gRes2Scan, 1);
    @autoreleasepool { nvmtl_res2_pass_guarded(0); }
    uint64_t after = nvmtl_vk_vram_free_now();
    static _Atomic uint64_t blindUntil;
    if (after <= before && gGraveVram && clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) >= atomic_load(&blindUntil)) {
        const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
        while (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0 < 16000000ull) {
            usleep(1000); nvmtl_res2_reap(); after = nvmtl_vk_vram_free_now();
            if (after > before) break;
        }
        if (after <= before) { atomic_store(&blindUntil, clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) + 250000000ull);
            static unsigned said; if (said++ < 4 || said % 256 == 0)
                nvlog("residency (res2): waited 16 ms for %llu MB of evicted VRAM to drain and it did not - this allocation spills (%u)",
                      (unsigned long long)(gGraveVram >> 20), said); }
    }
    const uint64_t freed = after > before ? after - before : 0;
    gRes2EvictCalls++; gRes2EvictBytes += freed;
    return freed;
}
static void nvmtl_res2_kick(int scan) {
    if (gRes2On != 1) return;
    if (scan) atomic_store(&gRes2Scan, 1);
    int z = 0; if (!atomic_compare_exchange_strong(&gRes2Pending, &z, 1)) return;
    static dispatch_once_t once; dispatch_once(&once, ^{ gRes2Q = dispatch_queue_create("nvmtl.residency", DISPATCH_QUEUE_SERIAL); });
    const uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW), last = atomic_load(&gRes2LastNs);
    const int64_t wait = last && now - last < 16000000ull ? (int64_t)(16000000ull - (now - last)) : 0;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, wait), gRes2Q, ^{
        atomic_store(&gRes2Pending, 0);
        @autoreleasepool { nvmtl_res2_pass_guarded(1); }
        atomic_store(&gRes2LastNs, clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW));
    });
}
extern int nvmtl_vk_mig_batch_begin(void); extern int nvmtl_vk_mig_batch_end(void);
static int gRes2Batching;
typedef struct { __unsafe_unretained NVMTLTexture *t; nvk_image old; } nvmtl_pend_t;
static nvmtl_pend_t *gPend; static unsigned gPendN, gPendCap; static uint64_t gBatchN, gBatchMoves;
static void nvmtl_grave_put(const nvk_image *old);
static void nvmtl_res2_batch_end(void);
static void nvmtl_res2_pend(NVMTLTexture *t, const nvk_image *old) {
    if (gPendN == gPendCap) { unsigned c = gPendCap ? gPendCap * 2 : 128; nvmtl_pend_t *n = realloc(gPend, c * sizeof *n); if (n) { gPend = n; gPendCap = c; } }
    if (gPendN < gPendCap) { gPend[gPendN].t = t; gPend[gPendN].old = *old; gPendN++; }
    else { nvmtl_res2_batch_end(); nvmtl_grave_put(old); __atomic_store_n(&t->_rstate, 0, __ATOMIC_RELEASE); }
    if (gPendN >= 128) { nvmtl_res2_batch_end(); nvmtl_vk_mig_batch_begin(); }
}
static void nvmtl_res2_batch_end(void) {
    (void)nvmtl_vk_mig_batch_end();
    for (unsigned k = 0; k < gPendN; k++) { nvmtl_grave_put(&gPend[k].old); __atomic_store_n(&gPend[k].t->_rstate, 0, __ATOMIC_RELEASE); }
    if (gPendN) { gBatchN++; gBatchMoves += gPendN;
        static unsigned said; if (said++ < 4 || said % 256 == 0) nvlog("residency (res2): evict-batch %llu: %u textures copied in ONE submission (%llu moves in %llu batches)",
            (unsigned long long)gBatchN, gPendN, (unsigned long long)gBatchMoves, (unsigned long long)gBatchN); }
    gPendN = 0;
}
static int nvmtl_res2_move(NVMTLTexture *t, int to_sys) {
    int z = 0; if (!__atomic_compare_exchange_n(&t->_rstate, &z, 2, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) return 1;
    int r = 1, held = 0;
    @synchronized (t) {
        if (!__atomic_load_n(&t->_resRefs, __ATOMIC_ACQUIRE) && __atomic_load_n(&t->_useEp, __ATOMIC_ACQUIRE) <= atomic_load(&gRes2Drained)) {
            nvk_image old; memset(&old, 0, sizeof old);
            r = nvmtl_vk_image_migrate(&t->_i, to_sys, t->_fmt == MTLPixelFormatA8Unorm, &old);
            if (r == 0) {
                if (t->_resid & (1ull << 62)) {
                    // the slot's sampled view follows the new image, and so must its storage descriptor (binding 2):
                    // the old image is freed below, and a kernel writing through a stale binding 2 stored into freed memory
                    const uint32_t slot = (uint32_t)(t->_resid & 0xffffffffu);
                    nvmtl_vk_bindless_rewrite(slot, t->_i.view);
                    nvmtl_vk_bindless_put_storage(slot, t->_stex ? [t nvview] : NULL);
                }
                if (gRes2Batching) { nvmtl_res2_pend(t, &old); held = 1; }
                else nvmtl_grave_put(&old);
            }
        }
    }
    if (!held) __atomic_store_n(&t->_rstate, 0, __ATOMIC_RELEASE);
    return r;
}
static Class gNVMTLBufClass;
static BOOL nvmtl_res3_movable(NVMTLBuffer *b) {
    if (!gNVMTLBufClass) gNVMTLBufClass = [NVMTLBuffer class];
    return object_getClass(b) == gNVMTLBufClass && b->_storage == MTLStorageModePrivate && b->_b.sparse && b->_b.buf && b->_b.mem
        && !b->_b.map && !b->_heap && !b->_subHeap && !b->_hostPtr && !b->_shadow.buf && !b->_texSeen && !b->_nbpk && !b->_subPlaced
        && !b->_b.pool && !b->_b.placed && !b->_b.imported && !b->_b.bar1 && !b->_b.moff;
}
static void nvmtl_res3_note_use(NVMTLBuffer *b) {
    __atomic_store_n(&b->_lastUse, atomic_load(&gRes2Tick), __ATOMIC_RELAXED);
    if (!b->_b.sparse) return;
    if (!__atomic_load_n(&b->_lruReg, __ATOMIC_ACQUIRE)) {
        int z = 0;
        if (__atomic_compare_exchange_n(&b->_lruReg, &z, 1, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE) && nvmtl_res2_on() && nvmtl_res3_movable(b)) {
            os_unfair_lock_lock(&gRes2RegLock);
            if (!gRes2Reg) gRes2Reg = [NSHashTable weakObjectsHashTable];
            [gRes2Reg addObject:b];
            os_unfair_lock_unlock(&gRes2RegLock);
            if (!nvmtl_res2_pressure_hook) nvmtl_res2_pressure_hook = nvmtl_res2_pressure;
            if (!nvmtl_res2_evict_hook) nvmtl_res2_evict_hook = nvmtl_res2_evict;
        }
    }
    if (b->_b.sysmem && !__atomic_load_n(&b->_rstate, __ATOMIC_ACQUIRE) && !__atomic_load_n(&b->_wantOn, __ATOMIC_ACQUIRE)
        && __atomic_load_n(&b->_lruReg, __ATOMIC_ACQUIRE) && nvmtl_res2_on() && nvmtl_res3_movable(b))
        __atomic_store_n(&b->_wantOn, 1, __ATOMIC_RELEASE);
}
static int nvmtl_res3_move(NVMTLBuffer *b, int to_sys) {
    int z = 0; if (!__atomic_compare_exchange_n(&b->_rstate, &z, 2, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) return 1;
    int r = 1;
    if (!__atomic_load_n(&b->_resRefs, __ATOMIC_ACQUIRE) && __atomic_load_n(&b->_useEp, __ATOMIC_ACQUIRE) <= atomic_load(&gRes2Drained))
        r = nvmtl_vk_buffer_migrate(&b->_b, to_sys);
    __atomic_store_n(&b->_rstate, 0, __ATOMIC_RELEASE);
    return r;
}
static inline BOOL r2_tex(id o) { return [o isKindOfClass:gNVMTLTexClass]; }
static inline int *r2_rst(id o)  { if (r2_tex(o)) { NVMTLTexture *t = o; return &t->_rstate; }  NVMTLBuffer *b = o; return &b->_rstate; }
static inline int *r2_refs(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return &t->_resRefs; } NVMTLBuffer *b = o; return &b->_resRefs; }
static inline int *r2_want(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return &t->_wantOn; }  NVMTLBuffer *b = o; return &b->_wantOn; }
static inline uint64_t *r2_useEp(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return &t->_useEp; } NVMTLBuffer *b = o; return &b->_useEp; }
static inline uint64_t *r2_last(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return &t->_lastUse; } NVMTLBuffer *b = o; return &b->_lastUse; }
static inline uint32_t r2_purge(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return __atomic_load_n(&t->_purge, __ATOMIC_ACQUIRE); }
                                        NVMTLBuffer *b = o; return __atomic_load_n(&b->_purge, __ATOMIC_ACQUIRE); }
static inline int r2_sys(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return t->_i.sysmem != 0; } NVMTLBuffer *b = o; return b->_b.sysmem != 0; }
static inline uint64_t r2_alloc(id o) { if (r2_tex(o)) { NVMTLTexture *t = o; return t->_i.alloc; } NVMTLBuffer *b = o; return b->_b.alloc; }
static inline BOOL r2_movable(id o) { return r2_tex(o) ? (((NVMTLTexture *)o)->_i.img && nvmtl_res2_movable(o)) : nvmtl_res3_movable(o); }
static inline int r2_move(id o, int to_sys) { return r2_tex(o) ? nvmtl_res2_move(o, to_sys) : nvmtl_res3_move(o, to_sys); }
static void nvmtl_res2_pass(void) {
    nvmtl_res2_reap();
    if (!atomic_exchange(&gRes2Scan, 0)) return;
    NSArray *all = nil;
    os_unfair_lock_lock(&gRes2RegLock); all = gRes2Reg.count ? gRes2Reg.allObjects : nil; os_unfair_lock_unlock(&gRes2RegLock);
    const uint64_t want = atomic_exchange(&gRes2Want, 0);
    if (!all.count) return;
    if (!gNVMTLTexClass) gNVMTLTexClass = [NVMTLTexture class];
    if (!nvmtl_ep_room()) { nvlog("residency (res2): PAUSED - a command buffer from %llu epochs ago has not completed; nothing moves until it does",
                                  (unsigned long long)(atomic_load(&gRes2Epoch) - atomic_load(&gRes2Drained))); return; }
    const uint64_t tick = atomic_load(&gRes2Tick);
    NSMutableArray *cold = [NSMutableArray new], *hot = [NSMutableArray new];
    const uint64_t drained = atomic_load(&gRes2Drained); unsigned undrained = 0;
    for (id o in all) {
        if (__atomic_load_n(r2_rst(o), __ATOMIC_ACQUIRE) || !r2_movable(o)) continue;
        if (__atomic_load_n(r2_useEp(o), __ATOMIC_ACQUIRE) > drained) { undrained++; continue; }
        const uint32_t ps = r2_purge(o);
        const uint64_t lu = __atomic_load_n(r2_last(o), __ATOMIC_RELAXED);
        if (!r2_sys(o)) {
            if (lu + (want ? 0 : gRes2Cold) <= tick && (ps == 0 || ps == MTLPurgeableStateNonVolatile) && !__atomic_load_n(r2_refs(o), __ATOMIC_ACQUIRE)) [cold addObject:o];
        } else if (__atomic_load_n(r2_want(o), __ATOMIC_ACQUIRE) || lu + gRes2Hot >= tick) [hot addObject:o];
    }
    [cold sortUsingComparator:^NSComparisonResult(id a, id b) { uint64_t x = *r2_last(a), y = *r2_last(b); return x < y ? NSOrderedAscending : x > y ? NSOrderedDescending : NSOrderedSame; }];
    [hot sortUsingComparator:^NSComparisonResult(id a, id b) { uint64_t x = *r2_last(a), y = *r2_last(b); return x > y ? NSOrderedAscending : x < y ? NSOrderedDescending : NSOrderedSame; }];
    unsigned on = 0, off = 0, onB = 0, offB = 0, busy = 0, noroom = 0; uint64_t onM = 0, offM = 0;
    const uint64_t coldest = cold.count ? tick - *r2_last(cold[0]) : 0;
    NSUInteger ci = 0;
    for (id t in (want ? @[] : hot)) {
        if (__atomic_load_n(r2_refs(t), __ATOMIC_ACQUIRE)) { busy++; continue; }
        const uint64_t need = r2_alloc(t);
        if (!nvmtl_vk_vram_room(need)) {
            uint64_t pending = 0;
            while (ci < cold.count && pending < need + gRes2Margin) {
                id v = cold[ci++];
                if (*r2_last(v) + gRes2Cold > *r2_last(t)) continue;
                const uint64_t a = r2_alloc(v); const BOOL vb = !r2_tex(v);
                if (r2_move(v, 1) == 0) { off++; offB += vb; offM += a; pending += a; }
            }
            noroom++;
            continue;
        }
        const uint64_t a = r2_alloc(t); const BOOL tb = !r2_tex(t);
        if (r2_move(t, 0) == 0) { on++; onB += tb; onM += a; __atomic_store_n(r2_want(t), 0, __ATOMIC_RELEASE); }
    }
    if (want) {
        const uint64_t target = nvmtl_vk_vram_headroom() + gRes2Margin + want;
        if (nvmtl_vk_mig_batch_begin() == 0) gRes2Batching = 1;
        { static unsigned said; if (said++ < 16 || said % 1024 == 0)
            nvlog("residency (res2): pressure pass wants %llu MB - registry %lu, victims left %lu of %lu, "
                  "stranded %lu, undrained %u, in use %u, free %llu MB + graveyard %llu MB vs target %llu MB",
                  (unsigned long long)(want >> 20), (unsigned long)all.count,
                  (unsigned long)(cold.count > ci ? cold.count - ci : 0), (unsigned long)cold.count,
                  (unsigned long)hot.count, undrained, busy,
                  (unsigned long long)(nvmtl_vk_vram_free_now() >> 20),
                  (unsigned long long)(gGraveVram >> 20), (unsigned long long)(target >> 20)); }
        const int force = atomic_exchange(&gRes2Force, 0);
        const uint64_t f0 = nvmtl_vk_vram_free_now() + gGraveVram;
        uint64_t cap = force ? want + gRes2Margin : (target > f0 ? target - f0 : 0);
        // BOUNDED on the estimated path. nvmtl_vk_vram_free_now() used to answer with the whole card
        // when the budget is unmeasured, so f0 always exceeded target, cap came out 0 and this loop
        // NEVER RAN - res2 eviction was silently inert on every card without VK_EXT_memory_budget.
        // Giving that branch a real free figure wakes it, and the estimated model counts images alone
        // while its headroom is the unclamped one, so an unbounded first pass would try to page out
        // about a gigabyte of textures on a model we have never measured. Evict what the request
        // actually asked for, not a full headroom restoration, until a run says more is safe.
        if (!nvmtl_vk_vram_measured()) {
            const uint64_t bounded = want + gRes2Margin;
            if (cap > bounded) { cap = bounded;
                static unsigned said; if (said++ < 8)
                    nvlog("residency (res2): budget is ESTIMATED, so this pass is bounded to %llu MB instead of the %llu MB a full headroom restore would ask for",
                          (unsigned long long)(bounded >> 20), (unsigned long long)((target > f0 ? target - f0 : 0) >> 20)); }
        }
        uint64_t movedM = 0;
        while (ci < cold.count && movedM < cap && (force || nvmtl_vk_vram_free_now() + gGraveVram < target)) {
            id v = cold[ci++]; const uint64_t a = r2_alloc(v); const BOOL vb = !r2_tex(v);
            if (r2_move(v, 1) == 0) { off++; offB += vb; offM += a; movedM += a; }
        }
        if (gRes2Batching) { nvmtl_res2_batch_end(); gRes2Batching = 0; }
    }
    if (on || off) {
        if (nvmtl_ep_room()) atomic_fetch_add(&gRes2Epoch, 1);
        uint64_t s[5], s3[6]; nvmtl_vk_res2_stats(s); nvmtl_vk_res3_stats(s3);
        nvlog("residency (res2): paged ON %u (%llu MB, %u buffers), paged OFF %u (%llu MB, %u buffers, coldest idle %llu ticks); %u in use, %u waiting for room; "
              "graveyard %u (%llu MB, %llu MB VRAM) freed %llu; free VRAM %llu MB; ever: textures off %llu (%llu MB) on %llu (%llu MB), %llu copy failures; "
              "buffers off %llu (%llu MB) on %llu (%llu MB), %llu stable-address buffers made (%llu in system memory at birth)",
              on, (unsigned long long)(onM >> 20), onB, off, (unsigned long long)(offM >> 20), offB, (unsigned long long)coldest, busy, noroom,
              atomic_load(&gGraveN), (unsigned long long)(gGraveBytes >> 20), (unsigned long long)(gGraveVram >> 20), (unsigned long long)gGraveFreed,
              (unsigned long long)(nvmtl_vk_vram_free_now() >> 20),
              (unsigned long long)s[0], (unsigned long long)s[2], (unsigned long long)s[1], (unsigned long long)s[3], (unsigned long long)s[4],
              (unsigned long long)s3[2], (unsigned long long)s3[4], (unsigned long long)s3[3], (unsigned long long)s3[5],
              (unsigned long long)s3[0], (unsigned long long)s3[1]);
    }
    if (noroom) { atomic_store(&gRes2Scan, 1); if (off) nvmtl_res2_kick(1); }
    if (undrained) { if (!(on || off) && nvmtl_ep_room()) atomic_fetch_add(&gRes2Epoch, 1); atomic_store(&gRes2Scan, 1); }
}

static void nvmtl_purge_track(id r, MTLPurgeableState s) {
    if (s != MTLPurgeableStateVolatile && s != MTLPurgeableStateEmpty && s != MTLPurgeableStateNonVolatile) return;
    os_unfair_lock_lock(&gPurgeLock);
    if (s == MTLPurgeableStateNonVolatile) { [gPurgeVol removeObject:r]; }
    else {
        if (!gPurgeVol) gPurgeVol = [NSHashTable weakObjectsHashTable];
        [gPurgeVol addObject:r];
        atomic_fetch_add(&gPurgeAsks, 1);
        nvmtl_purge_report_hook = nvmtl_purge_report;
        if (!nvmtl_reclaim_hook && nvmtl_res1_on()) nvmtl_reclaim_hook = nvmtl_purge_reclaim;
    }
    os_unfair_lock_unlock(&gPurgeLock);
}

static void nvmtl_purge_report(const char *why) {
    uint64_t nvol = 0, nemp = 0, bvol = 0, bemp = 0;
    NSArray *all = nil;
    os_unfair_lock_lock(&gPurgeLock);
    all = gPurgeVol.allObjects;
    os_unfair_lock_unlock(&gPurgeLock);
    for (id r in all) {
        uint32_t st = 0; uint64_t bytes = 0;
        if ([r isKindOfClass:[NVMTLTexture class]]) { NVMTLTexture *t = r; st = t->_purge; bytes = t->_i.alloc; }
        else if ([r isKindOfClass:[NVMTLBuffer class]]) { NVMTLBuffer *b = r; st = b->_purge; bytes = b->_b.alloc; }
        else continue;
        if (st == MTLPurgeableStateEmpty) { nemp++; bemp += bytes; }
        else if (st == MTLPurgeableStateVolatile) { nvol++; bvol += bytes; }
    }
    nvlog("purgeable at the %s spill: %llu Volatile holding %llu MB, %llu Empty holding %llu MB, of %llu asks ever"
          " - still marked after the res1 consumer ran (buffers, views, bindless or in-use textures): %llu MB Empty",
          why, (unsigned long long)nvol, (unsigned long long)(bvol >> 20),
          (unsigned long long)nemp, (unsigned long long)(bemp >> 20),
          (unsigned long long)atomic_load(&gPurgeAsks), (unsigned long long)(bemp >> 20));
}
static void nvmtl_pre_release(void *held) { @autoreleasepool { (void)CFBridgingRelease(held); } }
static void nvmtl_zero_flush(void);
static void nvmtl_managed_flush(void) {
    if (!atomic_load(&gManagedAny) || gManagedInFlush) return;
    gManagedInFlush = 1;
    NSMutableArray *todo = [NSMutableArray new];
    os_unfair_lock_lock(&gManagedLock);
    for (NVMTLBuffer *b in gManagedDirty.allObjects) {
        if (b->_dirtyHi > b->_dirtyLo) [todo addObject:@[b, @(b->_dirtyLo), @(b->_dirtyHi)]];
        b->_dirtyLo = b->_dirtyHi = 0;
    }
    [gManagedDirty removeAllObjects]; atomic_store(&gManagedAny, 0);
    os_unfair_lock_unlock(&gManagedLock);
    // A managed buffer can be created and dirtied after presubmit's first zero
    // snapshot. Claim its initial clear before uploading this dirty snapshot;
    // otherwise the next submission can clear data already uploaded here.
    if (todo.count) nvmtl_zero_flush();
    if (todo.count && nvmtl_vk_pre_open()) {
        for (NSArray *t in todo) {
            NVMTLBuffer *b = t[0]; NSUInteger lo = [t[1] unsignedIntegerValue], n = [t[2] unsignedIntegerValue] - lo;
            if (nvmtl_vk_pre_copy(&b->_shadow, lo, &b->_b, lo, n))
                nvlog("managed: upload of %lu bytes at %lu FAILED - the GPU keeps the old bytes", (unsigned long)n, (unsigned long)lo);
        }
        nvmtl_pre_release_hook = nvmtl_pre_release;
        nvmtl_vk_pre_hold((__bridge_retained void *)todo);
    } else
    for (NSArray *t in todo) {
        NVMTLBuffer *b = t[0]; NSUInteger lo = [t[1] unsignedIntegerValue], n = [t[2] unsignedIntegerValue] - lo;
        if (nvmtl_vk_copy_buffer(nvmtl_device_queue(), &b->_shadow, lo, &b->_b, lo, n))
            nvlog("managed: upload of %lu bytes at %lu FAILED - the GPU keeps the old bytes", (unsigned long)n, (unsigned long)lo);
    }
    gManagedInFlush = 0;
}
static void nvmtl_presubmit(void);
static void nvmtl_managed_mark(NVMTLBuffer *b, NSUInteger lo, NSUInteger hi) {
    os_unfair_lock_lock(&gManagedLock);
    if (!gManagedDirty) gManagedDirty = [NSHashTable weakObjectsHashTable];
    if (b->_dirtyHi <= b->_dirtyLo) { b->_dirtyLo = lo; b->_dirtyHi = hi; }
    else { b->_dirtyLo = MIN(b->_dirtyLo, lo); b->_dirtyHi = MAX(b->_dirtyHi, hi); }
    @autoreleasepool { [gManagedDirty addObject:b]; } atomic_store(&gManagedAny, 1);
    nvmtl_pre_submit_hook = nvmtl_presubmit;
    os_unfair_lock_unlock(&gManagedLock);
}
static os_unfair_lock gZeroLock = OS_UNFAIR_LOCK_INIT;
static NSHashTable *gZeroOwed;
static _Atomic int gZeroAny;
static __thread int gZeroInFlush;
static _Atomic unsigned long long gZeroHostN, gZeroHostB, gZeroGpuN, gZeroGpuB, gZeroTail;
static int nvmtl_zero_mode(void) {
    static int m = -1;
    if (m < 0) { const char *e = getenv("NVMTL_ZERO_FILL");
        m = !e ? 2 : (e[0] == '0' ? 0 : (strcmp(e, "host") == 0 ? 1 : 2));
        nvlog("zero-fill: new buffers %s (NVMTL_ZERO_FILL=%s)", m == 2 ? "are cleared - CPU-mapped by memset, Private by a GPU fill before the next submission"
              : m == 1 ? "are cleared only where the CPU maps them - Private keeps what VRAM held" : "are NOT cleared - they hold what the memory held", e ? e : "unset"); }
    return m;
}
static void nvmtl_zero_census(void) {
    nvlog("zero-fill census: %llu CPU-mapped buffers (%llu bytes) cleared by memset, %llu Private (%llu bytes) by the GPU, %llu tail bytes left (length not a multiple of 4)",
          (unsigned long long)gZeroHostN, (unsigned long long)gZeroHostB, (unsigned long long)gZeroGpuN, (unsigned long long)gZeroGpuB, (unsigned long long)gZeroTail);
}
static void nvmtl_zero_flush(void) {
    if (!atomic_load(&gZeroAny) || gZeroInFlush) return;
    gZeroInFlush = 1;
    os_unfair_lock_lock(&gZeroLock);
    NSArray *todo = gZeroOwed.allObjects; [gZeroOwed removeAllObjects]; atomic_store(&gZeroAny, 0);
    os_unfair_lock_unlock(&gZeroLock);
    nvk_cmdbuf c; unsigned n = 0; size_t bytes = 0;
    if (todo.count && nvmtl_vk_pre_open()) {
        for (NVMTLBuffer *b in todo) { const size_t w = b->_b.size & ~(size_t)3;
            if (b->_b.buf && w && nvmtl_vk_pre_fill(&b->_b, 0, w, 0) == 0) { n++; bytes += w; gZeroTail += b->_b.size - w; } }
        nvmtl_pre_release_hook = nvmtl_pre_release;
        nvmtl_vk_pre_hold((__bridge_retained void *)todo);
        const unsigned long long k = atomic_fetch_add(&gZeroGpuN, n); gZeroGpuB += bytes;
        if (!k || (k >> 12) != ((k + n) >> 12)) nvmtl_zero_census();
    } else if (todo.count && nvmtl_vk_cmd_begin(nvmtl_device_queue(), &c) == 0) {
        for (NVMTLBuffer *b in todo) { const size_t w = b->_b.size & ~(size_t)3;
            if (b->_b.buf && w && nvmtl_vk_cmd_fill_buffer(&c, &b->_b, 0, w, 0) == 0) { n++; bytes += w; gZeroTail += b->_b.size - w; } }
        if (nvmtl_vk_submit_wait(&c)) nvlog("zero-fill: GPU clear of %u Private buffers (%zu bytes) FAILED - they hold what VRAM held", n, bytes);
        else { const unsigned long long k = atomic_fetch_add(&gZeroGpuN, n); gZeroGpuB += bytes;
               if (!k || (k >> 12) != ((k + n) >> 12)) nvmtl_zero_census(); }
    } else if (todo.count) nvlog("zero-fill: no command buffer for %lu Private clears - they hold what VRAM held", (unsigned long)todo.count);
    gZeroInFlush = 0;
}
static void nvmtl_presubmit(void) { @autoreleasepool { nvmtl_zero_flush(); } @autoreleasepool { nvmtl_managed_flush(); } }
static int nvmtl_mzero_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_MANAGED_EAGER_ZERO"); on = !(e && e[0] == '1');
        nvlog("zero-fill: managed buffers %s (NVMTL_MANAGED_EAGER_ZERO=%s)", on ? "clear their VRAM copy on the GPU before the next submit and their CPU copy on first CPU touch"
              : "memset their CPU copy at creation and upload it (old)", e ? e : "unset"); }
    return on;
}
// A fresh host-visible allocation is zeroed by RM in the kernel. Trusted only after the first NVMTL_FRESH_PROBE of them are
// read back all-zero; one non-zero byte turns the skip off for the process (and that buffer is cleared as before).
// NVMTL_FRESH_ZERO=0: always clear (the old path).
#define NVMTL_FRESH_PROBE 4
static int nvmtl_fresh_is_zero(const nvk_buffer *nb, NSUInteger len) {
    static int on = -1; static _Atomic int checked, bad;
    if (on < 0) { const char *e = getenv("NVMTL_FRESH_ZERO"); on = !(e && e[0] == '0'); }
    if (!on || !nb || !nb->fresh || !nb->map || atomic_load(&bad)) return 0;
    if (atomic_load(&checked) >= NVMTL_FRESH_PROBE) return 1;
    const size_t n = MIN((size_t)len, nb->size); const uint64_t *w = (const uint64_t *)nb->map; uint64_t acc = 0;
    for (size_t i = 0; i < n / 8; i++) acc |= w[i];
    for (size_t i = n & ~(size_t)7; i < n; i++) acc |= ((const uint8_t *)nb->map)[i];
    if (acc) { atomic_store(&bad, 1); nvlog("zero-fill: a fresh %zu-byte system allocation was NOT zero - the kernel does not clear them here; clearing every buffer", n); return 0; }
    if (atomic_fetch_add(&checked, 1) + 1 == NVMTL_FRESH_PROBE) nvlog("zero-fill: %d fresh system allocations read back zero - fresh ones are no longer cleared twice", NVMTL_FRESH_PROBE);
    return 1;
}
static void nvmtl_zero_new_buffer(NVMTLBuffer *b, NSUInteger len) {
    const int m = nvmtl_zero_mode();
    if (!m || !b || !len) return;
    const int mz = b->_shadow.map && m >= 2 && b->_b.buf && !b->_b.map && nvmtl_mzero_on();
    if (mz && !nvmtl_fresh_is_zero(&b->_shadow, len)) __atomic_store_n(&b->_zshadow, 1, __ATOMIC_RELEASE);
    if (b->_shadow.map && !mz) {
        memset(b->_shadow.map, 0, len); nvmtl_managed_mark(b, 0, MIN((NSUInteger)b->_b.size, len));
    } else if (b->_b.map && !mz) {
        if (!nvmtl_fresh_is_zero(&b->_b, len)) memset(b->_b.map, 0, len);
    } else {
        if (m < 2 || !b->_b.buf) return;
        os_unfair_lock_lock(&gZeroLock);
        if (!gZeroOwed) gZeroOwed = [NSHashTable weakObjectsHashTable];
        @autoreleasepool { [gZeroOwed addObject:b]; }
        atomic_store(&gZeroAny, 1);
        nvmtl_pre_submit_hook = nvmtl_presubmit;
        os_unfair_lock_unlock(&gZeroLock);
        return;
    }
    const unsigned long long k = atomic_fetch_add(&gZeroHostN, 1); gZeroHostB += len;
    if (!k || ((k + 1) & 0xFFF) == 0) nvmtl_zero_census();
}
#define NVMTL_BUFPARK 4
static _Atomic uint64_t gBTNew, gBTRecycled, gBTParked, gBTEvicted;
static void nvmtl_buftex_census(const char *what) {
    const uint64_t n = atomic_load(&gBTNew) + atomic_load(&gBTRecycled);
    if (n & (n - 1)) return;
    nvlog("buffer texture (%s): %llu new, %llu recycled, %llu parked, %llu evicted", what,
          (unsigned long long)atomic_load(&gBTNew), (unsigned long long)atomic_load(&gBTRecycled),
          (unsigned long long)atomic_load(&gBTParked), (unsigned long long)atomic_load(&gBTEvicted));
}
static int nvmtl_bpark_take(NVMTLBuffer *b, const uint32_t *key, nvk_image *out) {
    int got = -1;
    os_unfair_lock_lock(&b->_bpl);
    for (uint32_t k = b->_nbpk; k-- > 0; ) {
        nvmtl_park_t *p = &b->_bpk[k];
        if (memcmp(p->key, key, sizeof p->key)) continue;
        *out = p->img; nvmtl_vk_image_recycled(out); memmove(p, p + 1, (size_t)(b->_nbpk - k - 1) * sizeof *p); b->_nbpk--; got = 0; break;
    }
    os_unfair_lock_unlock(&b->_bpl);
    return got;
}
static void nvmtl_bpark_put(NVMTLBuffer *b, nvk_image *img, const uint32_t *key) {
    nvk_image ev; int evict = 0, parked = 0;
    os_unfair_lock_lock(&b->_bpl);
    if (!b->_bpk) b->_bpk = calloc(NVMTL_BUFPARK, sizeof *b->_bpk);
    if (b->_bpk) {
        if (b->_nbpk == NVMTL_BUFPARK) { ev = b->_bpk[0].img; evict = 1;
            memmove(&b->_bpk[0], &b->_bpk[1], (NVMTL_BUFPARK - 1) * sizeof *b->_bpk); b->_nbpk--; }
        nvmtl_park_t *p = &b->_bpk[b->_nbpk++]; p->img = *img; memcpy(p->key, key, sizeof p->key); p->off = p->len = 0;
        parked = 1;
    }
    os_unfair_lock_unlock(&b->_bpl);
    if (parked) { nvmtl_vk_image_parked(img); memset(img, 0, sizeof *img); atomic_fetch_add(&gBTParked, 1); }
    if (evict) { nvmtl_vk_image_destroy(&ev); atomic_fetch_add(&gBTEvicted, 1); }
}
static NVMTLTexture *nvmtl_texbuf_make(NVMTLBuffer *b, MTLTextureDescriptor *d, NSUInteger offset)
{
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    extern nvk_queue *nvmtl_device_queue(void);
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (!b || !d || !d.width || nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8) || a8 || !bpp) {
        nvlog("texture buffer: format %lu width %lu is not carried -> nil", (unsigned long)d.pixelFormat, (unsigned long)d.width); return nil; }
    const size_t range = (size_t)d.width * bpp;
    NVMTLTexture *t = [NVMTLTexture new]; int st = 0;
    if (nvmtl_vk_texel_view_create(&b->_b, offset, range, vkf, (d.usage & MTLTextureUsageShaderWrite) != 0, &t->_tbAlias, &t->_tbView, &st)) {
        nvlog("texture buffer: %lu x fmt %lu at offset %lu of a %zu-byte buffer -> nil (reason above)", (unsigned long)d.width,
              (unsigned long)d.pixelFormat, (unsigned long)offset, b->_b.size); return nil; }
    t->_tbStorage = st != 0;
    t->_i.w = (uint32_t)d.width; t->_i.h = 1; t->_i.fmt = vkf; t->_i.bpp = bpp; t->_i.mtl_type = 9; t->_i.mips = 1; t->_i.layers = 1; t->_i.samples = 1;
    t->_q = nvmtl_device_queue(); t->_fmt = d.pixelFormat; t->_mips = 1; t->_usage = d.usage; t->_stex = NO;
    t->_backBuf = b; t->_backOff = offset; t->_backBPR = range;
    b->_texSeen = YES;
    nvlog("texture buffer: %lu texels fmt %lu (VkFormat %u, %s) at offset %lu -> %p", (unsigned long)d.width, (unsigned long)d.pixelFormat, vkf,
          st ? "read/write" : "read", (unsigned long)offset, (__bridge void *)t);
    return t;
}
@implementation NVMTLBuffer
@dynamic sparseBufferTier;
- (IOSurfaceRef)iosurface { return NULL; }
- (id<MTLTexture>)newTiledTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)offset bytesPerRow:(NSUInteger)bpr {
    id<MTLTexture> t = [self newTextureWithDescriptor:d offset:offset bytesPerRow:bpr];
    if (t || !d) return t;
    void *base = [self contents];
    if (!base || d.textureType != MTLTextureType2D || offset + bpr * d.height > _b.size) {
        nvlog("newTiledTextureWithDescriptor: %lux%lu bpr %lu off %lu: no linear view and no CPU copy (buffer %lu B) -> nil",
              (unsigned long)d.width, (unsigned long)d.height, (unsigned long)bpr, (unsigned long)offset, (unsigned long)_b.size);
        return nil; }
    MTLTextureDescriptor *cd = [d copy]; cd.storageMode = MTLStorageModeShared;
    id<MTLTexture> c = [[self device] newTextureWithDescriptor:cd];
    if (c) [c replaceRegion:MTLRegionMake2D(0, 0, d.width, d.height) mipmapLevel:0 withBytes:(char *)base + offset bytesPerRow:bpr];
    { static int said; if (!said++) nvlog("newTiledTextureWithDescriptor: linear view refused -> copied texture (%lux%lu bpr %lu; said once)",
                                          (unsigned long)d.width, (unsigned long)d.height, (unsigned long)bpr); }
    return c;
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)offset bytesPerRow:(NSUInteger)bpr {
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    if (!d) return nil;
    if (d.textureType == MTLTextureTypeTextureBuffer) return nvmtl_texbuf_make(self, d, offset);
    size_t w = d.width, h = d.height;
    if (d.textureType != MTLTextureType2D || d.mipmapLevelCount > 1 || d.sampleCount > 1 || d.arrayLength > 1 || d.depth > 1
        || !w || !h || w > UINT32_MAX || h > UINT32_MAX) {
        nvlog("buffer newTextureWithDescriptor:offset:%lu bytesPerRow:%lu: type %lu %zux%zu (mips %lu, samples %lu, array %lu, depth %lu) "
              "is not built - only a 2D single-level linear view -> nil", (unsigned long)offset, (unsigned long)bpr, (unsigned long)d.textureType,
              w, h, (unsigned long)d.mipmapLevelCount, (unsigned long)d.sampleCount, (unsigned long)d.arrayLength, (unsigned long)d.depth);
        return nil; }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8)) {
        nvlog("buffer newTextureWithDescriptor: pixelFormat %lu has no VkFormat -> nil", (unsigned long)d.pixelFormat); return nil; }
    NVMTLTexture *t = [NVMTLTexture new];
    static int noRecycle = -1; if (noRecycle < 0) { noRecycle = getenv("NVMTL_NO_BUFTEX_RECYCLE") != NULL;
        nvlog("buffer texture: dres R3 recycle %s", noRecycle ? "OFF (NVMTL_NO_BUFTEX_RECYCLE)" : "ON"); }
    const uint32_t bkey[12] = { 2, (uint32_t)w, (uint32_t)h, vkf, bpp, (uint32_t)a8, (d.usage & MTLTextureUsageShaderWrite) != 0,
        (d.usage & MTLTextureUsageRenderTarget) != 0, (uint32_t)offset, (uint32_t)((uint64_t)offset >> 32), (uint32_t)bpr, (uint32_t)((uint64_t)bpr >> 32) };
    const BOOL brec = !noRecycle && _nbpk && nvmtl_bpark_take(self, bkey, &t->_i) == 0;
    if (!brec && nvmtl_vk_image_create_buffer_alias((uint32_t)w, (uint32_t)h, vkf, bpp, a8, (d.usage & MTLTextureUsageShaderWrite) != 0,
                                           (d.usage & MTLTextureUsageRenderTarget) != 0, &_b, offset, bpr, &t->_i)) {
        nvlog("buffer newTextureWithDescriptor:offset:%lu bytesPerRow:%lu (%zux%zu fmt %lu, buffer %lu B, storage %lu) -> nil (reason above)",
              (unsigned long)offset, (unsigned long)bpr, w, h, (unsigned long)d.pixelFormat, (unsigned long)_b.size, (unsigned long)_storage);
        return nil; }
    t->_q = nvmtl_device_queue();
    t->_fmt = d.pixelFormat; t->_mips = 1;
    t->_stex = t->_i.storage == 1;
    t->_ropt |= (MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift;
    t->_usage = d.usage;
    _texSeen = YES;
    t->_backBuf = self; t->_backOff = offset; t->_backBPR = bpr;
    t->_bparkable = !noRecycle; memcpy(t->_pkey, bkey, sizeof bkey);
    atomic_fetch_add(brec ? &gBTRecycled : &gBTNew, 1); nvmtl_buftex_census(brec ? "recycled" : "new");
    nvmtl_texture_desc_swizzle(t, d);
    nvlog("buffer %p newTextureWithDescriptor:offset:%lu bytesPerRow:%lu -> %p (%zux%zu fmt %lu usage 0x%lx, storage %lu)", (__bridge void *)self,
          (unsigned long)offset, (unsigned long)bpr, (__bridge void *)t, w, h, (unsigned long)d.pixelFormat, (unsigned long)d.usage, (unsigned long)_storage);
    return t; }
- (void)setResponsibleProcess:(int)pid { static int said; if (!said++) nvlog("setResponsibleProcess: pid %d on a buffer - per-process attribution is not kept; nothing to set (said once)", pid); }
- (instancetype)initStandinWithDevice:(id)device bytesNoCopy:(void *)bytes length:(NSUInteger)length deallocator:(void (^)(void *, NSUInteger))deallocator {
    id d = device ? device : (id)gNVMTLMainDevice;
    id buf = [d newBufferWithBytesNoCopy:bytes length:length options:MTLResourceStorageModeShared deallocator:deallocator];
    { static int said; if (!said++) nvlog("initStandin: %lu-byte client-storage stand-in -> %s (said once)", (unsigned long)length,
                                          buf ? "a no-copy buffer over the caller's pages" : "nil (newBufferWithBytesNoCopy refused, reason above)"); }
    return buf;
}
- (NSUInteger)allocatedSize { return (NSUInteger)_b.size; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (MTLCPUCacheMode)cpuCacheMode { NVMTLBuffer *r = self; return nvmtl_res_cache(r->_heap ? r->_heap : r->_subHeap, r->_ropt); }
- (MTLHazardTrackingMode)hazardTrackingMode { NVMTLBuffer *r = self; return nvmtl_res_hazard(r->_heap ? r->_heap : r->_subHeap, r->_ropt); }
- (MTLResourceOptions)resourceOptions { return ((MTLResourceOptions)[self storageMode] << MTLResourceStorageModeShift) | (MTLResourceOptions)[self cpuCacheMode]
                                            | ((MTLResourceOptions)[self hazardTrackingMode] << MTLResourceHazardTrackingModeShift); }
- (BOOL)isAliasable { @synchronized (self) { return _aliased; } }
- (void)makeAliasable {
    NVMTLHeap *h = nil; NSUInteger c = 0, o = 0; BOOL p = NO;
    @synchronized (self) {
        if (!_subHeap || _aliased) return;
        h = _subHeap; c = _subCharge; _subCharge = 0; _aliased = YES;
        p = _subPlaced; o = _heapOffset; _subPlaced = NO;
    }
    if (c) { if (p) [h nvmtlGiveRange:o length:c]; [h nvmtlReleaseSubAllocation:c]; }
}
- (id<MTLBuffer>)remoteStorageBuffer { return nil; }
- (id<MTLBuffer>)newRemoteBufferViewForDevice:(id<MTLDevice>)d { return nil; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return nvmtl_owner_check(t); }
- (uint64_t)gpuAddress {   // a Metal 4 argument table resolves addresses back to buffers
    uint64_t a = nvmtl_vk_buffer_address(&_b); const uint64_t len = [self length];
    if (!nvmtl_m4_note_current(_m4note, a, len)) nvmtl_m4_note_buffer_cached(self, a, len, _m4note);
    return a;
}
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s {
    if (_heap || _subHeap || _hostPtr) return MTLPurgeableStateNonVolatile;
    MTLPurgeableState prev = nvmtl_purge(&_purge, s);
    nvmtl_purge_track(self, s);
    return prev;
}
- (void)addDebugMarker:(NSString *)m range:(NSRange)r {}
- (void)removeAllDebugMarkers {}
void nvmtl_vk_buffer_retire(nvk_buffer *b);
- (void)dealloc {
    if (_b.buf && nvmtl_trace_on()) nvtrace("BUFFREE 0x%llx len %zu", (unsigned long long)nvmtl_vk_buffer_address(&_b), _b.size);
    for (uint32_t k = 0; k < _nbpk; k++) nvmtl_vk_image_destroy(&_bpk[k].img);
    free(_bpk); _bpk = NULL; _nbpk = 0;
    nvmtl_vk_buffer_retire(&_b); if (_shadow.buf) nvmtl_vk_buffer_retire(&_shadow);
    if (_swc) { nvmtl_swc_free((nvmtl_swc *)_swc); free(_swc); _swc = NULL; }
    if (_subHeap) { NVMTLHeap *h = _subHeap; NSUInteger c = _subCharge; _subHeap = nil; _subCharge = 0;
                    if (_subPlaced && c) { _subPlaced = NO; [h nvmtlGiveRange:_heapOffset length:c]; }
                    [h nvmtlReleaseSubAllocation:c]; }
    if (_hostDealloc) { void (^d)(void *, NSUInteger) = _hostDealloc; _hostDealloc = nil;
                        d(_hostPtr ?: _b.map, (NSUInteger)(_hostLen ?: _b.size)); } }
- (MTLStorageMode)storageMode { return _storage; }
- (void *)contents { nvmtl_buf_settle(self); if (_swc && _hostPtr) return _hostPtr; return _shadow.map ? _shadow.map : (_hostPtr ? _hostPtr : _b.map); }
- (id<MTLHeap>)heap { return _heap ?: _subHeap; }
- (NSUInteger)heapOffset { return _heapOffset; }
- (NSUInteger)length { return _b.size; }
- (void)didModifyRange:(NSRange)r {
    nvmtl_buf_settle(self);
    if (_swc && _hostPtr) return;   // the caller's pages are diffed at the next submit; _shadow is only staging and holds old bytes
    if (_shadow.map) {
        if (r.location >= _b.size) return;
        NSUInteger n = MIN(r.length, _b.size - r.location);
        if (n) nvmtl_managed_mark(self, r.location, r.location + n);
        return;
    }
    nvmtl_buffer_sync_in(self); }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
@end

#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
static void nvmtl_surf_dirty(NVMTLCommandBuffer *cb, NVMTLTexture *t) {
    if (!cb || !t) return;
    NVMTLTexture *r = t->_parent ? t->_parent : t;
    if (!r->_surf) return;
    if (!cb->_surfDirty) cb->_surfDirty = [NSMutableSet new];
    [cb->_surfDirty addObject:r];
}
// A texture a kernel writes through a storage binding although NVK cannot make it a storage image - an IOSurface in a
// pitch-linear layout (ImageIO's RGBA16Unorm HDR / gain-map output), or a format NVK refuses linear storage for. The
// binding fell back to the dummy image and every write vanished: HDR photos decoded black. The kernel now writes a
// storage twin of the same VkFormat in optimal tiling, filled from the texture when this encoder first binds it and
// copied back after every dispatch (nvmtl_storage_twins_out), the IOSurface marked dirty as for a direct write.
static void nvmtl_retain_resource(NVMTLCommandBuffer *cb, id resource);
// The FNV-1a of a function's SPIR-V: what a per-kernel profile row, the pipeline key line and a fault-context dispatch
// record all carry, so one hash joins the three. Computed once per function.
uint32_t nvmtl_function_spv_fnv(id f)
{
    if (![f isKindOfClass:[NVMTLFunction class]]) return 0;
    NVMTLFunction *fn = f; static char kKey;
    NSNumber *h = objc_getAssociatedObject(fn, &kKey);
    if (h) return h.unsignedIntValue;
    uint32_t x = 2166136261u; const uint8_t *b = fn->_spirv.bytes;
    for (NSUInteger i = 0; i < fn->_spirv.length; i++) { x ^= b[i]; x *= 16777619u; }
    if (!fn->_spirv.length) x = 0;
    objc_setAssociatedObject(fn, &kKey, @(x), OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return x;
}
// The first 8 bytes of the sha256 of a function's AIR text, as the pipeline key line prints them: the module key a
// profile row or a fault-context dispatch is joined to. Computed once per function.
uint64_t nvmtl_function_air_key(id f)
{
    if (![f isKindOfClass:[NVMTLFunction class]]) return 0;
    NVMTLFunction *fn = f; static char kKey;
    NSNumber *h = objc_getAssociatedObject(fn, &kKey);
    if (h) return h.unsignedLongLongValue;
    uint64_t v = 0;
    if (fn->_air.length) {
        const char *s = fn->_air.UTF8String; unsigned char dg[CC_SHA256_DIGEST_LENGTH];
        CC_SHA256(s, (CC_LONG)strlen(s), dg);
        for (int i = 0; i < 8; i++) v = (v << 8) | dg[i];
    }
    objc_setAssociatedObject(fn, &kKey, @(v), OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return v;
}
static void nvmtl_heap_register(NVMTLHeap *heap, id resource);
static void nvmtl_use_heap(NVMTLCommandBuffer *cb, id<MTLHeap> heap);
// `writes`: the shader declares a storage binding there. A texture made with MTLTextureUsageUnknown (0) may be used
// any way Metal allows - ImageIO makes its HDR output IOSurface textures that way and writes them.
static void *nvmtl_storage_view_for(NVMTLCommandBuffer *cb, NVMTLTexture *t, BOOL writes) {
    if (!t) return NULL;
    if (t->_stex) return [t nvview];
    const MTLTextureUsage usage = (MTLTextureUsage)t->_usage;   // not -usage, which answers ShaderRead for Unknown
    if (!writes || !cb || cb->_c.in_rp || t->_parent || (usage && !(usage & MTLTextureUsageShaderWrite))) return NULL;
    nvk_image *im = [t nvi];
    // a multisampled attachment gets a storage twin only for a tile function (NVMTL_TILE_SHADING=1, shaderStorageImageMultisample)
    const BOOL ms = im && (im->samples ? im->samples : 1) > 1;
    const BOOL msOk = ms && im->mtl_type == 4 && nvmtl_tile_on() && nvmtl_vk_ms_storage_ok();
    if (!im || !im->img || !im->w || !im->h || (im->mips ? im->mips : 1) > 1 || (im->layers ? im->layers : 1) > 1
        || (ms ? !msOk : im->mtl_type != 2) || im->bw > 1) return NULL;
    // Created once per texture; two command buffers may encode the same texture on different threads at once.
    @synchronized (t) {
        if (!t->_stTwin.img) {
            if (t->_stTwinTried) return NULL;
            t->_stTwinTried = YES;
            // An sRGB format cannot be a storage image in Vulkan at all. Its twin is half float, filled and written back by
            // blits that decode and encode sRGB, so a kernel writes linear values and the texture ends up sRGB-encoded,
            // as a write to an sRGB texture does on Apple GPUs (Blender's edit-mode overlay wrote RGBA8Unorm_sRGB).
            const int srgb = nvmtl_vk_srgb_format(im->fmt);
            int made = ms ? (srgb ? -1 : nvmtl_vk_image_create_ms_storage(im->w, im->h, im->fmt, im->bpp, im->samples, &t->_stTwin))
                          : nvmtl_vk_image_create_typed(im->w, im->h, srgb ? 97u : im->fmt, srgb ? 8u : im->bpp, 0, 1, 1, 2, 1, &t->_stTwin);
            if (ms && !made) t->_stTwin.storage = 1;
            if (made || !t->_stTwin.storage) {
                if (t->_stTwin.img) nvmtl_vk_image_destroy(&t->_stTwin);
                memset(&t->_stTwin, 0, sizeof t->_stTwin);
                nvlog("storage twin: VkFormat %u %ux%u cannot be a storage image in optimal tiling either - shader writes to it are LOST", im->fmt, im->w, im->h);
                return NULL;
            }
            static int said; if (said++ < 4) nvlog("storage twin: %ux%u VkFormat %u%s is written through an optimal-tiled storage copy", im->w, im->h, im->fmt, t->_surf ? " (IOSurface)" : "");
        }
    }
    if (!cb->_stTwins) cb->_stTwins = [NSMutableSet new];
    if (![cb->_stTwins containsObject:t]) {
        if (t->_stTwin.fmt != im->fmt ? nvmtl_vk_cmd_blit_image_2d(&cb->_c, im, &t->_stTwin, im->w, im->h)
                                      : nvmtl_vk_cmd_copy_image_sub(&cb->_c, im, 0, 0, 0, 0, &t->_stTwin, 0, 0, 0, 0, im->w, im->h)) return NULL;
        [cb->_stTwins addObject:t];
        nvmtl_retain_resource(cb, t);
    }
    nvmtl_vk_cmd_prepare_sampled_image(&cb->_c, &t->_stTwin, 1);
    return t->_stTwin.view;
}
NSDictionary *nvmtl_function_reflection(NVMTLFunction *function);
static BOOL nvmtl_function_writes_texture(NVMTLFunction *fn, NSUInteger index) {
    NSArray *bindings = fn ? nvmtl_function_reflection(fn)[@"bindings"] : nil;
    if (![bindings isKindOfClass:[NSArray class]]) return NO;
    for (NSDictionary *b in bindings) {
        if (![b isKindOfClass:[NSDictionary class]] || [b[@"metal_index"] unsignedLongValue] != index) continue;
        NSString *kind = b[@"kind"], *access = b[@"access"];
        if (![kind isKindOfClass:[NSString class]] || [kind hasPrefix:@"Embedded"] || [kind isEqualToString:@"Buffer"] || [kind isEqualToString:@"Sampler"]
            || [kind isEqualToString:@"StaticSampler"]) continue;
        if ([kind isEqualToString:@"StorageImage"] || ([access isKindOfClass:[NSString class]] && ([access isEqualToString:@"Storage"] || [access isEqualToString:@"ReadWrite"] || [access isEqualToString:@"WriteOnly"]))) return YES;
    }
    return NO;
}
static void nvmtl_storage_twins_out(NVMTLCommandBuffer *cb) {
    for (NVMTLTexture *t in cb->_stTwins) {
        nvk_image *im = [t nvi];
        if (t->_stTwin.fmt != im->fmt ? nvmtl_vk_cmd_blit_image_2d(&cb->_c, &t->_stTwin, im, im->w, im->h)
                                      : nvmtl_vk_cmd_copy_image_sub(&cb->_c, &t->_stTwin, 0, 0, 0, 0, im, 0, 0, 0, 0, im->w, im->h)) {
            nvlog("storage twin: copy back to the %ux%u texture FAILED", im->w, im->h); continue; }
        nvmtl_vk_cmd_prepare_sampled_image(&cb->_c, &t->_stTwin, 1);
        nvmtl_surf_dirty(cb, t);
    }
}
static int g_trace_lines;
static int g_trace_bigonly, g_trace_mute;
static int nvmtl_trace_on(void) {
#if NVMTL_RELEASE
    return 0;   // the request files are refused in release builds (nvmtl_rel_access), so tracing never turns on
#endif
    static uint64_t next; static int on;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    static int cap = 2500;
    if (now >= next) { next = now + 250000000ull; int was = on; on = access("/tmp/nvmtl-trace-request", F_OK) == 0;
        g_trace_bigonly = on && access("/tmp/nvmtl-trace-big", F_OK) == 0; if (!g_trace_bigonly) g_trace_mute = 0;
        if (on && !was) { g_trace_lines = 0; cap = access("/tmp/nvmtl-trace-huge", F_OK) == 0 ? 600000 : access("/tmp/nvmtl-trace-long", F_OK) == 0 ? 20000 : 2500; } }
    return on && !g_trace_mute && g_trace_lines < cap;
}
static int nvtrace_scope(uint32_t w, uint32_t h) { int was = g_trace_mute; g_trace_mute = 0; (void)nvmtl_trace_on();
    g_trace_mute = g_trace_bigonly && (w < 1000 || h < 100); return was; }
static void nvtrace_emit(const char *fmt, ...) {
    if (!nvmtl_trace_on()) return;
    char p[96]; snprintf(p, sizeof p, "/tmp/nvmtl-trace-%d.txt", (int)getpid());
    int fd = open(p, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd < 0) { static int said; if (!said++) nvlog("G15 trace: cannot open %s (errno %d)", p, errno); return; }
    char b[700]; va_list a; va_start(a, fmt); int n = vsnprintf(b, sizeof b - 1, fmt, a); va_end(a);
    if (n < 0) n = 0; if (n > (int)sizeof b - 2) n = (int)sizeof b - 2;
    b[n++] = '\n'; if (write(fd, b, (size_t)n) != n) { static int said2; if (!said2++) nvlog("G15 trace: short write (errno %d)", errno); }
    close(fd); g_trace_lines++;
}
static void nvtrace_floats(const char *what, unsigned long idx, unsigned long off, const void *map, int nf) {
    if (!nvmtl_trace_on()) return;
    if (!map) { nvtrace("%s[%lu] off %lu (not CPU-visible)", what, idx, off); return; }
    const float *f = (const float *)((const char *)map + off); char s[420]; int k = 0;
    for (int i = 0; i < nf && k < (int)sizeof s - 16; i++) k += snprintf(s + k, sizeof s - (size_t)k, " %.3g", f[i]);
    nvtrace("%s[%lu] off %lu:%s", what, idx, off, s);
}
static void nvmtl_nonzero(const void *base, size_t row, size_t rowValid, uint32_t h, unsigned *nz, unsigned *n) {
    *nz = *n = 0; if (!base || !rowValid || !h) return;
    size_t take = rowValid < 256 ? rowValid : 256; uint32_t step = h > 16 ? h / 16 : 1;
    for (uint32_t y = 0; y < h; y += step) { const unsigned char *p = (const unsigned char *)base + (size_t)y * row;
        for (size_t x = 0; x < take; x++) { *nz += p[x] != 0; (*n)++; } }
}
// A full overwrite of a single 8-bit RGBA/BGRA subresource cannot consume the previous CPU contents.
// Keep all partial updates, views, arrays, volumes, compressed and multisample images
// on SurfaceIn: their untouched bytes still have to be preserved.
static BOOL nvmtl_surface_full_overwrite(NVMTLTexture *t, MTLRegion r, NSUInteger level, NSUInteger slice) {
    if (!t || !t->_surf || t->_parent || t->_viewParent || t->_tbView || t->_baseLevel || t->_baseSlice || level || slice)
        return NO;
    const nvk_image *i = &t->_i;
    if (i->bpp != 4 || (t->_fmt != MTLPixelFormatBGRA8Unorm && t->_fmt != MTLPixelFormatBGRA8Unorm_sRGB
        && t->_fmt != MTLPixelFormatRGBA8Unorm && t->_fmt != MTLPixelFormatRGBA8Unorm_sRGB)) return NO;
    return i->mtl_type == MTLTextureType2D && i->mips == 1 && i->layers == 1 && i->samples <= 1
        && i->bw <= 1 && i->bh <= 1 && !r.origin.x && !r.origin.y && !r.origin.z
        && r.size.width == i->w && r.size.height == i->h && r.size.depth == 1;
}
// A recorded overwrite only defines contents for subsequent commands in this
// command buffer. Do not promote global Seed/generation state before it executes:
// an abandoned command buffer must leave SurfaceIn able to restore CPU contents.
static void nvmtl_surface_overwrite_recorded(NVMTLCommandBuffer *cb, NVMTLTexture *t) {
    if (!cb || !t) return;
    NVMTLTexture *r = t->_parent ? t->_parent : t;
    if (!r->_surf) return;
    if (!cb->_surfOverwrites) cb->_surfOverwrites = [NSMutableSet new];
    [cb->_surfOverwrites addObject:r];
}
static BOOL nvmtl_surface_pending_write(NVMTLCommandBuffer *cb, NVMTLTexture *t);
static void nvmtl_surface_in_for_cb(NVMTLCommandBuffer *cb, NVMTLTexture *t) {
    if (!t) return;
    NVMTLTexture *r = t->_parent ? t->_parent : t;
    if (cb && r->_surf && [cb->_surfOverwrites containsObject:r]) return;
    // A prior committed buffer defines this GPU backing's contents.
    // Importing the old CPU surface here would run ahead of this buffer and
    // overwrite that earlier GPU write. Queue order or the application's event supplies the dependency.
    if (nvmtl_surface_pending_write(cb, r)) return;
    [t nvmtlSurfaceIn];
}
static unsigned nvmtl_surf_id(NVMTLTexture *t) { NVMTLTexture *r = (t && t->_parent) ? t->_parent : t; return (r && r->_surf) ? (unsigned)IOSurfaceGetID(r->_surf) : 0u; }
static int nvmtl_noseed(void) { static uint64_t next; static int on; uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (now >= next) { next = now + 250000000ull; int was = on; on = access("/tmp/nvmtl-noseed", F_OK) == 0;
        if (on && !was) nvlog("G17 EXPERIMENT ON: surface-out writes under a read-only lock, the IOSurface seed does not move");
        if (!on && was) nvlog("G17 experiment off: surface-out bumps the seed again"); }
    return on; }
static void nvmtl_census(const char *what, uint32_t w, uint32_t h, int surf) {
    static struct { const char *what; uint32_t w, h; int surf; } seen[48]; static int n; static os_unfair_lock lk = OS_UNFAIR_LOCK_INIT;
    if (__atomic_load_n(&n, __ATOMIC_RELAXED) == 48) return;   // full: every later pass skips the lock
    os_unfair_lock_lock(&lk);
    for (int i = 0; i < n; i++) if (seen[i].what == what && seen[i].w == w && seen[i].h == h && seen[i].surf == surf) { os_unfair_lock_unlock(&lk); return; }
    if (n == 48) { os_unfair_lock_unlock(&lk); return; }
    seen[n].what = what; seen[n].w = w; seen[n].h = h; seen[n].surf = surf; __atomic_store_n(&n, n + 1, __ATOMIC_RELAXED);
    os_unfair_lock_unlock(&lk);
    nvlog("G14 census: %s %ux%u iosurface=%d (first time in this process)", what, w, h, surf);
}
static void nvmtl_frame_dump(const char *tag, const void *base, size_t row, uint32_t w, uint32_t h);
static void nvmtl_surface_cpu_dump(NVMTLTexture *rt) {
    static uint64_t next; static int on;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (now >= next) { next = now + 250000000ull; on = access("/tmp/nvmtl-dump-request", F_OK) == 0; }
    if (!on || !rt || !rt->_surf || IOSurfaceGetBytesPerElement(rt->_surf) != 4 || IOSurfaceGetPlaneCount(rt->_surf) > 1) return;
    if (IOSurfaceLock(rt->_surf, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) { static int said; if (!said++) nvlog("G16 cpu dump: IOSurfaceLock failed"); return; }
    char tag[24]; snprintf(tag, sizeof tag, "cpu%u-", (unsigned)IOSurfaceGetID(rt->_surf));
    nvmtl_frame_dump(tag, IOSurfaceGetBaseAddress(rt->_surf), IOSurfaceGetBytesPerRow(rt->_surf), (uint32_t)IOSurfaceGetWidth(rt->_surf), (uint32_t)IOSurfaceGetHeight(rt->_surf));
    IOSurfaceUnlock(rt->_surf, kIOSurfaceLockReadOnly, NULL);
}
/* The frame dump keeps its documented trigger. The isolation these two knobs give — a private request
      file and a private directory, so two concurrent investigations cannot capture each other's frames —
      is worth keeping, but it arrived as a REPLACEMENT: with neither variable set, `touch
      /tmp/nvmtl-dump-request` stopped producing anything. That trigger is what every runbook, every rule
      file and every one of our own debugging sessions uses to get the frame instead of a photo of the
      monitor, and a dump that silently never fires is the worst possible failure for a diagnostic. So the
      private paths are an OVERRIDE and the documented default stands. */
static const char *nvmtl_dump_request_path(void) {
    const char *p = getenv("NVMTL_PRIVATE_CAPTURE_REQUEST");
    return (p && *p) ? p : "/tmp/nvmtl-dump-request";
}
static void nvmtl_frame_dump(const char *tag, const void *base, size_t row, uint32_t w, uint32_t h) {
    const char *request = nvmtl_dump_request_path();
    if ((uint64_t)w * h < 1024 || access(request, F_OK)) return;
    static struct { char tag[24]; uint32_t w, h; uint64_t last; } slot[32]; static int nslot;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW); int k, go = 0;
    @synchronized([NSObject class]) {
        for (k = 0; k < nslot; k++) if (slot[k].w == w && slot[k].h == h && !strncmp(slot[k].tag, tag, sizeof slot[k].tag - 1)) break;
        if (k == nslot && nslot < 32) { snprintf(slot[k].tag, sizeof slot[k].tag, "%s", tag); slot[k].w = w; slot[k].h = h; slot[k].last = 0; nslot++; }
        if (k < nslot && (!slot[k].last || now - slot[k].last >= 2000000000ull)) { slot[k].last = now; go = 1; }
    }
    if (!go) return;
    char dst[400], tmp[416]; const char *dir = getenv("NVMTL_PRIVATE_CAPTURE_DIR"); int fd = -1;
    if (!dir || !*dir) dir = "/tmp";   /* an override, not a requirement — see nvmtl_dump_request_path */
    for (int pass = 0; pass < 2 && fd < 0; pass++) {
        if (pass) { const char *t = getenv("TMPDIR"); if (!t || !*t || (errno != EPERM && errno != EACCES)) break; dir = t; }
        size_t dl = strlen(dir); const char *sep = (dl && dir[dl - 1] == '/') ? "" : "/";
        snprintf(dst, sizeof dst, "%s%snvmtl-frame-%d-%.23s%ux%u.bgra", dir, sep, (int)getpid(), tag, w, h); snprintf(tmp, sizeof tmp, "%s.tmp", dst);
        fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    }
    if (fd < 0) { static int said; if (!said++) nvlog("G13 frame dump: cannot open %s (errno %d)", tmp, errno); return; }
    uint32_t hdr[4] = { 0x3146564e , w, h, (uint32_t)row };
    int ok = write(fd, hdr, sizeof hdr) == (ssize_t)sizeof hdr && write(fd, base, row * h) == (ssize_t)(row * h);
    close(fd);
    if (ok && rename(tmp, dst) == 0) { static int told; if (!told++) nvlog("G13 frame dump: %ux%u written to %s", w, h, dst); }
    else { unlink(tmp); static int said2; if (!said2++) nvlog("G13 frame dump: write FAILED (errno %d)", errno); }
}
static void nvmtl_blit_dump(NVMTLTexture *d) {
    nvk_image *i = [d nvi]; if (!i || i->w < 1000 || i->h < 100 || i->bpp != 4) return;
    static uint64_t last; uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if ((last && now - last < 2000000000ull) || access("/tmp/nvmtl-dump-request", F_OK)) return;
    last = now; size_t row = (size_t)i->w * 4; void *px = malloc(row * i->h);
    if (!px) { nvlog("G17 blit dump: malloc FAILED"); return; }
    if (nvmtl_vk_image_read(d->_q, i, px, row)) nvlog("G17 blit dump: readback FAILED");
    else { char tag[24]; snprintf(tag, sizeof tag, "blit%u-", nvmtl_surf_id(d)); nvmtl_frame_dump(tag, px, row, i->w, i->h); }
    free(px);
}
// Present-time frame dump (request file /tmp/nvmtl-dump-request, the same one the other dumps use). The surface and blit
// dumps never see a game's steady-state drawable: a whole Cyberpunk benchmark with the request file present wrote 2
// dumps, both at load. This one is called from nvmtlFinishExecution, after every part of the command buffer has passed
// its fence, so every write to the drawable is finished, and before the presentation blocks hand the drawable to the
// compositor, which may recycle it after. Every 2 s while the file exists, into 8 rotating files (present-<k>-) so a run
// keeps a short history (~8 MB each at 1080p); the file is checked at most every 250 ms, so it is inert without it.
static void nvmtl_present_dump(NVMTLTexture *t) {
    static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER; static uint64_t next, last; static unsigned seq; static int on, said;
    if (!t || pthread_mutex_trylock(&mu)) return;   // another completion thread is dumping: skip, never wait
    const uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (now >= next) { next = now + 250000000ull; on = access(nvmtl_dump_request_path(), F_OK) == 0; }
    nvk_image *i = (on && (!last || now - last >= 2000000000ull)) ? [t nvi] : NULL;
    if (i && (i->bpp != 4 || i->w < 64 || i->h < 64)) {
        if (!said++) nvlog("present dump: drawable %ux%u bpp %u not dumpable (4-byte pixels only)", i->w, i->h, i->bpp);
        i = NULL;
    }
    if (!i) { pthread_mutex_unlock(&mu); return; }
    last = now; const unsigned k = seq++;
    size_t row = (size_t)i->w * 4; void *px = malloc(row * i->h);
    if (!px) nvlog("present dump: malloc FAILED");
    else if (nvmtl_vk_image_read(t->_q, i, px, row)) nvlog("present dump: readback FAILED (%ux%u)", i->w, i->h);
    else {
        char tag[24]; snprintf(tag, sizeof tag, "present-%u-", k % 8);
        nvmtl_frame_dump(tag, px, row, i->w, i->h);
        nvlog("present dump #%u: %ux%u fmt %u -> slot %u", k, i->w, i->h, (unsigned)i->fmt, k % 8);
    }
    free(px);
    pthread_mutex_unlock(&mu);
}
#pragma mark - texture
static _Atomic unsigned long long nvmtl_tex_objs_made, nvmtl_tex_objs_gone;
unsigned long long nvmtl_tex_objs_live(void) {
    unsigned long long made = nvmtl_tex_objs_made, gone = nvmtl_tex_objs_gone;
    return made > gone ? made - gone : 0;
}
unsigned long long nvmtl_tex_objs_released(void) { return nvmtl_tex_objs_gone; }
@implementation NVMTLTexture
// sparse textures (NVMTL_SPARSE_TEXTURES=1): where the mip tail starts, how big it is, and whether unmapped tiles read as zero
- (NSUInteger)firstMipmapInTail { const nvmtl_vk_sparse_info *si = _i.sparse; return si ? si->first_tail : 0; }
- (NSUInteger)tailSizeInBytes { const nvmtl_vk_sparse_info *si = _i.sparse; return si ? (NSUInteger)si->tail_size : 0; }
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
- (MTLTextureSparseTier)sparseTextureTier { const nvmtl_vk_sparse_info *si = _i.sparse; return (MTLTextureSparseTier)(si ? nvmtl_sparse_tier(si->strict) : 0u); }   // macOS 26; the build targets 15
#pragma clang diagnostic pop
+ (instancetype)alloc { nvmtl_tex_objs_made++; return [super alloc]; }
- (void)setResponsibleProcess:(int)pid { static int said; if (!said++) nvlog("setResponsibleProcess: pid %d on a texture - per-process attribution is not kept; nothing to set (said once)", pid); }
- (unsigned long long)protectionOptions { return 0; }
- (NSUInteger)allocatedSize { nvk_image *i = [self nvi]; if (i->alloc) return (NSUInteger)i->alloc;
    NSUInteger l = i->layers ? i->layers : 1, m = i->mips ? i->mips : 1, bpp = i->bpp ? i->bpp : 4, bw = i->bw ? i->bw : 1, bh = i->bh ? i->bh : 1, tot = 0;
    for (NSUInteger k = 0; k < m; k++) { NSUInteger w = i->w >> k ? i->w >> k : 1, h = i->h >> k ? i->h >> k : 1; tot += ((w + bw - 1) / bw) * ((h + bh - 1) / bh) * bpp; }
    return tot * l; }
- (MTLResourceID)gpuResourceID {
    nvmtl_heap_register((NVMTLHeap *)self.heap, self);
    @synchronized (self) { if (!_resid) _resid = nvmtl_resid_for_texture(self); }
    MTLResourceID rid; rid._impl = _resid; return rid;
}
extern int nvmtl_depthfmt_public(MTLPixelFormat, uint32_t *, uint32_t *);
- (nvk_image *)nvi { return _parent ? [_parent nvi] : &_i; }
- (void *)nvview {
    nvk_image *image = [self nvi];
    if (image->fmt == VK_FORMAT_D16_UNORM_S8_UINT || image->fmt == VK_FORMAT_D24_UNORM_S8_UINT || image->fmt == VK_FORMAT_D32_SFLOAT_S8_UINT) {
        uint32_t format = 0, aspect = 0;
        nvmtl_depthfmt_public(self.pixelFormat, &format, &aspect);
        if (aspect != VK_IMAGE_ASPECT_STENCIL_BIT) {
            @synchronized(self) {
                if (!_sampledDepthView && nvmtl_vk_image_view_create_range_aspect(image, image->fmt, 0, VK_IMAGE_ASPECT_DEPTH_BIT, _baseLevel, (uint32_t)self.mipmapLevelCount, _baseSlice, _parent ? _viewSlices : image->layers, (uint32_t)self.textureType, &_sampledDepthView)) return NULL;
                return _sampledDepthView;
            }
        }
    }
    return _ownView ? _ownView : image->view;
}
- (void *)nvsampleview { return _swzView ? _swzView : [self nvview]; }
/* MTLTextureDescriptor.swizzle applies when the texture is sampled or read, not to attachments or shader writes, so it
 * gets a view of its own for the sampled binds and the identity view stays for everything else. IconServices hands
 * gray + alpha image layers over as RG8 with swizzle (r, r, r, g); with the swizzle dropped, alpha landed in green. */
void nvmtl_texture_desc_swizzle(NVMTLTexture *t, MTLTextureDescriptor *d) {
    if (!t || !d || ![d respondsToSelector:@selector(swizzle)]) return;
    const MTLTextureSwizzleChannels sw = d.swizzle;
    if (sw.red == MTLTextureSwizzleRed && sw.green == MTLTextureSwizzleGreen && sw.blue == MTLTextureSwizzleBlue && sw.alpha == MTLTextureSwizzleAlpha) return;
    t->_swz = sw; t->_hasSwz = YES;
    if (d.textureType == MTLTextureTypeTextureBuffer || d.sampleCount > 1 || t->_tbView) {
        nvlog("texture swizzle %u,%u,%u,%u on texture type %lu (samples %lu) is not applied", sw.red, sw.green, sw.blue, sw.alpha,
              (unsigned long)d.textureType, (unsigned long)d.sampleCount); return; }
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    const uint8_t s4[4] = { (uint8_t)sw.red, (uint8_t)sw.green, (uint8_t)sw.blue, (uint8_t)sw.alpha };
    if (nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8) ||
        nvmtl_vk_image_view_create_range_swz([t nvi], vkf, a8, 0, 0, 0, 0, (uint32_t)d.textureType, s4, &t->_swzView)) {
        t->_swzView = NULL;
        nvlog("texture swizzle FAILED: no swizzled view for format %lu - samples keep the identity mapping", (unsigned long)d.pixelFormat); return; }
    { static int said; if (said++ < 8) nvlog("texture swizzle: format %lu samples r,g,b,a <- %u,%u,%u,%u", (unsigned long)d.pixelFormat, s4[0], s4[1], s4[2], s4[3]); }
}
- (id<MTLHeap>)heap { return _heap ?: _subHeap; }
- (NSUInteger)heapOffset { return _heapOffset; }
- (void)dealloc {
    // the heap slot goes back to the dummies BEFORE any view or buffer view it names is destroyed, so no slot ever
    // holds the index of a dead view (NVK reissues a destroyed view's descriptor index to the next view created)
    if (_resid) nvmtl_resid_texture_gone(_resid);
    if (_sampledDepthView) nvmtl_vk_image_view_destroy(_sampledDepthView);
    nvmtl_tex_objs_gone++;
    if (_tbView || _tbAlias) { nvmtl_vk_texel_view_destroy(_tbAlias, _tbView); _tbView = _tbAlias = NULL; }
    if (_ownView) nvmtl_vk_image_view_destroy(_ownView);
    if (_swzView) nvmtl_vk_image_view_destroy(_swzView);
    if (!_parent && _parkable && _subHeap && _i.img) nvmtl_heap_park(_subHeap, &_i, _pkey, _heapOffset, _pkLen);
    if (!_parent && _bparkable && _backBuf && _i.img) nvmtl_bpark_put(_backBuf, &_i, _pkey);
    if (!_parent) { nvmtl_vk_image_dying_label(_label.UTF8String); nvmtl_vk_image_destroy(&_i); }
    if (_stTwin.img) nvmtl_vk_image_destroy(&_stTwin);
    if (_sparseHeap) {   // a sparse texture's pages go back to its heap (the mapped blocks and the mip tail)
        @synchronized (_sparseHeap) {
            for (NSNumber *page in _sparseTiles.allValues) nvmtl_page_free(&_sparseHeap->_pool, page.unsignedIntValue);
            for (uint32_t k = 0; k < _sparseTailPages; k++) nvmtl_page_free(&_sparseHeap->_pool, (uint32_t)_sparseTailFirst + k);
            _sparseHeap->_used = (NSUInteger)_sparseHeap->_pool.used * NVMTL_SPARSE_BLOCK_BYTES;
        }
    }
    if (_y422Surf) { CFRelease(_y422Surf); _y422Surf = NULL; }
    if (!_parent && _vramOn) { if (!nvmtl_ic_put(&_vramBuf)) nvmtl_vk_buffer_destroy(&_vramBuf); _vramOn = NO; }
    if (!_parent) nvmtl_vk_host_pages_drop(&_hostPages);
    if (_surfOwned && _surf) CFRelease(_surf);
    if (_subHeap) { NVMTLHeap *h = _subHeap; NSUInteger c = _subCharge; _subHeap = nil; _subCharge = 0;
                    if (_subPlaced && c) { _subPlaced = NO; [h nvmtlGiveRange:_heapOffset length:c]; }
                    [h nvmtlReleaseSubAllocation:c]; }
}
- (BOOL)nvmtlSurfaceCopyable {
    size_t rowBytes = (size_t)_i.w * _i.bpp;
    size_t surfaceBytes = IOSurfaceGetWidthOfPlane(_surf, (uint32_t)_plane)
                        * IOSurfaceGetBytesPerElementOfPlane(_surf, (uint32_t)_plane);
    // The copy must also end inside the surface's allocation. Two Tahoe machines crashed in the row copy at a page
    // boundary while width, height and bytes per row all passed; the plane's rows can describe more memory than the
    // surface owns.
    const size_t bpr = IOSurfaceGetBytesPerRowOfPlane(_surf, (uint32_t)_plane);
    const uintptr_t b0 = (uintptr_t)IOSurfaceGetBaseAddress(_surf), bp = (uintptr_t)IOSurfaceGetBaseAddressOfPlane(_surf, (uint32_t)_plane);
    const size_t alloc = IOSurfaceGetAllocSize(_surf);
    const BOOL inAlloc = b0 && bp >= b0 && _i.h && bp - b0 <= alloc
        && (size_t)(_i.h - 1) <= (alloc - (bp - b0) - MIN(rowBytes, alloc - (bp - b0))) / (bpr ? bpr : 1)
        && rowBytes <= alloc - (bp - b0);
    if (_i.bpp && rowBytes <= surfaceBytes
        && IOSurfaceGetHeightOfPlane(_surf, (uint32_t)_plane) >= _i.h
        && bpr >= rowBytes && inAlloc) return YES;
    if (_i.bpp && !inAlloc) {
        static int saidAlloc; if (saidAlloc++ < 4) nvlog("G13 surface %p plane %lu NOT synced: %u rows of %zu B from plane offset %zu pass the surface's %zu-byte allocation",
            (void *)_surf, (unsigned long)_plane, _i.h, bpr, (size_t)(bp - b0), alloc);
        return NO;
    }
    static int said; if (said++ < 4) nvlog("G13 surface %p plane %lu NOT synced: image %ux%u bpp %u, surface bytesPerElement %zu — its pixels stay on one side",
        (void *)_surf, (unsigned long)_plane, _i.w, _i.h, _i.bpp, IOSurfaceGetBytesPerElementOfPlane(_surf, (uint32_t)_plane));
    return NO;
}
// IOSurface locking wires memory and updates its seed; it does not serialize
// this process's GPU readback and CPU import. Every wrapper of one plane must
// use the same monitor, including wrappers of shared family VRAM.
static id nvmtl_surface_sync(NVMTLTexture *t) {
    static char key;
    static NSMapTable *monitors;
    @synchronized ([NVMTLTexture class]) {
        id monitor = objc_getAssociatedObject(t, &key);
        if (!monitor) {
            if (!monitors) monitors = [NSMapTable strongToWeakObjectsMapTable];
            NSArray *surface = @[@(IOSurfaceGetID(t->_surf)), @(t->_plane)];
            monitor = [monitors objectForKey:surface];
            if (!monitor) { monitor = [NSObject new]; [monitors setObject:monitor forKey:surface]; }
            objc_setAssociatedObject(t, &key, monitor, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        return monitor;
    }
}
// NVMTL_SURFIN_UPLOAD=cpu: surface-in copies with the CPU (nvmtl_vk_image_write) instead of importing the pages
static bool nvmtl_surfin_import(void) {
    static int on = -1;
    if (on < 0) { const char *u = getenv("NVMTL_SURFIN_UPLOAD"); on = !(u && !strcmp(u, "cpu")); }
    return on;
}
// NVMTL_SURFOUT_READBACK=cpu: surface-out reads back through the staging buffer and a CPU copy (nvmtl_vk_image_read)
// instead of the GPU writing the surface's own pages
static bool nvmtl_surfout_import(void) {
    static int on = -1;
    if (on < 0) { const char *u = getenv("NVMTL_SURFOUT_READBACK"); on = !(u && !strcmp(u, "cpu")); }
    return on;
}
// Own-write tracking across the planes of one IOSurface (nvmtl_plane_seed.h). OFF unless NVMTL_PLANE_SEED=1 until it has
// been measured on a card with screen capture running; the seed behaviour it relies on (a read-write lock moves the seed by
// exactly one, unlock reports the same value) is measured on macOS.
#include "nvmtl_plane_seed.h"
static nvmtl_ps g_ps;
static os_unfair_lock g_ps_lock = OS_UNFAIR_LOCK_INIT;
static BOOL nvmtl_plane_seed_on(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_PLANE_SEED"); on = e && e[0] == '1'; nvlog("plane seed %s (NVMTL_PLANE_SEED=%s)", on ? "on" : "off", e ?: "unset"); }
    return on;
}
// A value kept in the surface itself, so a surface id that is reused after the surface is freed never matches a record.
static uint64_t nvmtl_plane_cookie(IOSurfaceRef s, BOOL create)
{
    static CFStringRef key; static dispatch_once_t once; static _Atomic uint64_t next = 1;
    dispatch_once(&once, ^{ key = CFStringCreateWithFormat(NULL, NULL, CFSTR("NVMTLPlaneSeed.%d"), (int)getpid()); });
    uint64_t v = 0; CFTypeRef c = IOSurfaceCopyValue(s, key);
    if (c && CFGetTypeID(c) == CFNumberGetTypeID()) CFNumberGetValue((CFNumberRef)c, kCFNumberSInt64Type, &v);
    if (c) CFRelease(c);
    if (!v && create) { v = ((uint64_t)getpid() << 32) | (atomic_fetch_add(&next, 1) & 0xffffffffu);
        CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt64Type, &v); IOSurfaceSetValue(s, key, n); CFRelease(n); }
    return v;
}
static unsigned long long g_surfin_plane_skip;
// The cookie that lets wrappers of one surface share an import of its pages (NVMTL_HOST_PAGES_SHARE=1); 0 when sharing is off,
// so the default path never touches the surface's attachments.
static uint64_t nvmtl_host_pages_cookie(IOSurfaceRef s)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_HOST_PAGES_SHARE"); on = e && e[0] == '1'; }
    return on ? nvmtl_plane_cookie(s, YES) : 0;
}
// A 4:2:2 texture wrapping an IOSurface plane (video frames): the surface's packed rows are copied and decoded whenever the
// surface has changed since the last use (the surface seed), by the CPU. The surface is never written back.
- (void)nvmtlY422SurfaceIn {
    @synchronized (self) {
        if (_y422Synced && IOSurfaceGetSeed(_y422Surf) == _y422Seed) return;
        uint32_t seed = 0;
        if (IOSurfaceLock(_y422Surf, kIOSurfaceLockReadOnly, &seed) != kIOReturnSuccess) { nvlog("4:2:2 surface-in: IOSurfaceLock FAILED"); return; }
        const uint8_t *base = IOSurfaceGetBaseAddressOfPlane(_y422Surf, (uint32_t)_y422Plane);
        const size_t row = IOSurfaceGetBytesPerRowOfPlane(_y422Surf, (uint32_t)_y422Plane), span = nvmtl_y422_row_bytes(_i.w);
        const size_t rows = IOSurfaceGetHeightOfPlane(_y422Surf, (uint32_t)_y422Plane);
        if (base && row >= span && rows >= _i.h) {
            for (uint32_t y = 0; y < _i.h; y++) memcpy((uint8_t *)_y422Raw.mutableBytes + (size_t)y * span, base + (size_t)y * row, span);
            _y422Seed = IOSurfaceGetSeed(_y422Surf);
        } else nvlog("4:2:2 surface-in: surface plane %lu is %zu bytes/row x %zu rows, the texture needs %zu x %u - frame skipped", (unsigned long)_y422Plane, row, rows, span, _i.h);
        IOSurfaceUnlock(_y422Surf, kIOSurfaceLockReadOnly, &seed);
        if (base && row >= span && rows >= _i.h) { [self nvmtlY422Upload:0 rows:_i.h]; _y422Synced = YES; }
    }
}
- (void)nvmtlSurfaceIn {
    if (_y422Surf) { [self nvmtlY422SurfaceIn]; return; }
    NVMTLTexture *r = _parent ? _parent : self;
    if (!r->_surf) return;
    @synchronized (nvmtl_surface_sync(r)) {
    if (r->_clientWritten) nvmtl_busy_wait(r->_surf, r->_plane);
    const uint32_t vgen = r->_vramOn ? 0 : nvmtl_vram_gen(r->_surf, r->_plane);
    if (r->_surfSynced && IOSurfaceGetSeed(r->_surf) == r->_surfSeed && (r->_vramOn || vgen == r->_surfGen)) return;
    if (!r->_clientWritten && nvmtl_busy_foreign(r->_surf, r->_plane)) { r->_clientWritten = YES; nvmtl_busy_wait(r->_surf, r->_plane); }
    if (r->_surfSynced && !r->_vramOn && vgen == r->_surfGen && nvmtl_plane_seed_on()) {
        // the seed moved, but only through our own surface-outs of other planes: this plane's image is still current
        const uint32_t now = IOSurfaceGetSeed(r->_surf);
        os_unfair_lock_lock(&g_ps_lock);
        const int covered = nvmtl_ps_covers(&g_ps, (uint32_t)IOSurfaceGetID(r->_surf), nvmtl_plane_cookie(r->_surf, NO), (uint32_t)r->_plane,
                                           r->_surfSeed, now, clock_gettime_nsec_np(CLOCK_UPTIME_RAW), 2000000000ull);
        os_unfair_lock_unlock(&g_ps_lock);
        if (covered) {
            r->_surfSeed = now; g_surfin_plane_skip++;
            static unsigned no; if (no++ < 6 || no % 5000 == 0) nvlog("surface-in SKIPPED (only our own writes to other planes since the last sync): %llu so far", g_surfin_plane_skip);
            return;
        }
    }
    if (![r nvmtlSurfaceCopyable]) return;
    { uint32_t now = IOSurfaceGetSeed(r->_surf);
      if (r->_vramOn && nvmtl_vram_tag_current(r->_surf, r->_plane, now)) {
          r->_surfSeed = now; r->_surfSynced = YES; g_surfin_skip++;
          static unsigned ns; if (ns++ < 6 || ns % 2000 == 0) nvlog("item 7: surface-in SKIPPED (shared VRAM current at seed %u): %llu skipped, %llu copied", now, g_surfin_skip, g_surfin_copy);
          return; } }
    uint32_t seed = 0;
    if (IOSurfaceLock(r->_surf, kIOSurfaceLockReadOnly, &seed) != kIOReturnSuccess) { nvlog("G13 surface-in: IOSurfaceLock FAILED"); return; }
    void *base = IOSurfaceGetBaseAddressOfPlane(r->_surf, (uint32_t)r->_plane);
    size_t row = IOSurfaceGetBytesPerRowOfPlane(r->_surf, (uint32_t)r->_plane);
    int rc = -1;
    if (base && row && nvmtl_surfin_import()) {
        const size_t off = (size_t)((uint8_t *)base - (uint8_t *)IOSurfaceGetBaseAddress(r->_surf));
        nvmtl_vk_host_pages_key((uint32_t)IOSurfaceGetID(r->_surf), nvmtl_host_pages_cookie(r->_surf));
        rc = nvmtl_vk_image_write_host(r->_q, &r->_i, base, row, IOSurfaceGetAllocSize(r->_surf) - off, &r->_hostPages);
        nvmtl_vk_host_pages_key(0, 0);
    }
    if (rc) rc = (base && row) ? nvmtl_vk_image_write(r->_q, &r->_i, base, row) : -1;
    if (!rc && r->_i.bpp == 4) { char tag[24]; snprintf(tag, sizeof tag, "in%u-", (unsigned)IOSurfaceGetID(r->_surf)); nvmtl_frame_dump(tag, base, row, r->_i.w, r->_i.h); }
    IOSurfaceUnlock(r->_surf, kIOSurfaceLockReadOnly, NULL);
    if (rc) { nvlog("G13 surface-in: upload FAILED (%ux%u) — retried at the next use", r->_i.w, r->_i.h); return; }
    r->_surfSeed = seed; r->_surfSynced = YES; r->_surfGen = vgen;
    g_surfin_copy++; if (r->_vramOn) nvmtl_vram_tag_set(r->_surf, r->_plane, seed);
    static unsigned n; if (n++ < 6 || n % 2000 == 0) nvlog("G13 surface-in #%u: %ux%u surface %p seed %u uploaded", n, r->_i.w, r->_i.h, (void *)r->_surf, seed);
    }
}
- (BOOL)nvmtlSurfaceOut { return [self nvmtlSurfaceOutDirtied:NO]; }
// A re-mark after the commit's (nvmtlSurfaceOutDirtied:) that found the accelerator lock taken is retried here, off the
// completion thread: a CPU lock that paged the surface off between the commit's two marks would otherwise leave its
// unfinished copy current until the surface is drawn again. Each retry is again a try that never waits for the lock,
// right after checking that the seed has not moved (a CPU write, or a newer commit marked it again): a mark that waited
// for the lock could land after a CPU write that came in meanwhile and make the older VRAM current over it. 50 tries,
// 2 ms apart; the texture then follows the seed the mark left.
static void nvmtl_surface_remark_later(NVMTLTexture *r, uint32_t seed) {
    static dispatch_queue_t queue; static dispatch_once_t once;
    dispatch_once(&once, ^{ queue = dispatch_queue_create("nvmtl.surface-remark", DISPATCH_QUEUE_SERIAL); });
    dispatch_async(queue, ^{
        int marked = 1;
        for (int i = 0; i < 50 && marked > 0; i++) {
            if (i) usleep(2000);
            if (IOSurfaceGetSeed(r->_surf) != seed) return;
            marked = nvmtl_vk_surface_dirty_try((uint32_t)IOSurfaceGetID(r->_surf), (uint32_t)r->_plane);
        }
        if (marked) return;
        @synchronized (nvmtl_surface_sync(r)) {
            if (r->_surfSeed != seed) return;
            const uint32_t now = IOSurfaceGetSeed(r->_surf);
            r->_surfSeed = now; r->_surfGen = nvmtl_vram_tag_bump(r->_surf, r->_plane, now);
        }
    });
}
// dirtied: the commit already marked this surface's shared VRAM newest in the kernel (WindowServer, with an accelerator
// whose page-off waits for the GPU). It is marked again in case a CPU lock paged it off since, but only if the accelerator
// lock is free: a page-off waiting for another surface's pending GPU write holds that lock until the write is released,
// by the same completion thread that would be blocked here. Taken, the commit's mark stands and the re-mark is retried
// off this thread (nvmtl_surface_remark_later).
- (BOOL)nvmtlSurfaceOutDirtied:(BOOL)dirtied {
    NVMTLTexture *r = _parent ? _parent : self;
    if (!r->_surf) return YES;
    @synchronized (nvmtl_surface_sync(r)) {
    if (![r nvmtlSurfaceCopyable]) { nvtrace("SURFOUT surf %u %ux%u NOT copyable", (unsigned)IOSurfaceGetID(r->_surf), r->_i.w, r->_i.h); return NO; }
    nvtrace("SURFOUT surf %u %ux%u vramOn %d t=%llu", (unsigned)IOSurfaceGetID(r->_surf), r->_i.w, r->_i.h, (int)r->_vramOn,
            (unsigned long long)(clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1000));
    const int marked = !r->_vramOn ? -1 : dirtied ? nvmtl_vk_surface_dirty_try((uint32_t)IOSurfaceGetID(r->_surf), (uint32_t)r->_plane)
                                                   : nvmtl_vk_surface_dirty((uint32_t)IOSurfaceGetID(r->_surf), (uint32_t)r->_plane);
    if (r->_vramOn && (dirtied || !marked)) {
        uint32_t seed = IOSurfaceGetSeed(r->_surf);
        r->_surfSeed = seed; r->_surfSynced = YES;
        r->_surfGen = nvmtl_vram_tag_bump(r->_surf, r->_plane, seed);
        if (dirtied && marked > 0) nvmtl_surface_remark_later(r, seed);
        static unsigned nd; if (nd++ < 6 || nd % 5000 == 0) nvlog("surface-out -> dirty, no copy (%ux%u surface %u seed %u, #%u)", r->_i.w, r->_i.h, (unsigned)IOSurfaceGetID(r->_surf), seed, nd);
        return YES;
    }
    uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    uint32_t lockopt = nvmtl_noseed() ? kIOSurfaceLockReadOnly : 0;
    uint32_t lockSeed = 0;
    if (IOSurfaceLock(r->_surf, lockopt, &lockSeed) != kIOReturnSuccess) { nvlog("G13 surface-out: IOSurfaceLock FAILED"); return NO; }
    void *base = IOSurfaceGetBaseAddressOfPlane(r->_surf, (uint32_t)r->_plane);
    size_t row = IOSurfaceGetBytesPerRowOfPlane(r->_surf, (uint32_t)r->_plane);
    int rc = -1;
    if (base && row && nvmtl_surfout_import()) {
        const size_t off = (size_t)((uint8_t *)base - (uint8_t *)IOSurfaceGetBaseAddress(r->_surf));
        nvmtl_vk_host_pages_key((uint32_t)IOSurfaceGetID(r->_surf), nvmtl_host_pages_cookie(r->_surf));
        rc = nvmtl_vk_image_read_host(r->_q, &r->_i, base, row, IOSurfaceGetAllocSize(r->_surf) - off, &r->_hostPages);
        nvmtl_vk_host_pages_key(0, 0);
    }
    if (rc) rc = (base && row) ? nvmtl_vk_image_read(r->_q, &r->_i, base, row) : -1;
    if (!rc && r->_i.bpp == 4) { char tag[24]; snprintf(tag, sizeof tag, "out%u-", (unsigned)IOSurfaceGetID(r->_surf)); nvmtl_frame_dump(tag, base, row, r->_i.w, r->_i.h); }
    uint32_t seed = 0; IOReturn unlocked = IOSurfaceUnlock(r->_surf, lockopt, &seed); if (lockopt) seed = IOSurfaceGetSeed(r->_surf);
    if (nvmtl_plane_seed_on() && !lockopt && unlocked == kIOReturnSuccess && IOSurfaceGetPlaneCount(r->_surf) > 1) {
        const uint64_t cookie = nvmtl_plane_cookie(r->_surf, YES);
        os_unfair_lock_lock(&g_ps_lock);
        nvmtl_ps_record(&g_ps, (uint32_t)IOSurfaceGetID(r->_surf), cookie, (uint32_t)r->_plane, lockSeed, seed, clock_gettime_nsec_np(CLOCK_UPTIME_RAW));
        os_unfair_lock_unlock(&g_ps_lock);
    }
    if (rc || unlocked != kIOReturnSuccess) { nvlog("G13 surface-out: readback FAILED (%ux%u)", r->_i.w, r->_i.h); return NO; }
    r->_surfSeed = seed; r->_surfSynced = YES;
    r->_surfGen = r->_vramOn ? r->_surfGen : nvmtl_vram_gen(r->_surf, r->_plane);
    if (r->_vramOn && !lockopt) nvmtl_vram_tag_set(r->_surf, r->_plane, seed);
    static unsigned n; if (n++ < 6 || n % 2000 == 0) nvlog("G13 surface-out #%u: %ux%u surface %p seed %u, %.2f ms", n, r->_i.w, r->_i.h,
        (void *)r->_surf, seed, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6);
    return YES;
    }
}
- (NSUInteger)width  { return MAX(1u, [self nvi]->w >> _baseLevel); }
- (NSUInteger)height { return MAX(1u, [self nvi]->h >> _baseLevel); }
- (NSUInteger)depth  { nvk_image *i = [self nvi]; return i->mtl_type == 7 ? MAX(1u, i->layers >> _baseLevel) : 1; }
- (NSUInteger)mipmapLevelCount { if (_parent) return _mips ? _mips : 1; uint32_t m = [self nvi]->mips; return m ? m : 1; }
- (id<MTLTexture>)parentTexture { return _viewParent; }
- (id<MTLResource>)rootResource { return _viewParent ? (id<MTLResource>)_viewParent : (id<MTLResource>)_backBuf; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return nvmtl_owner_check(t); }
- (NSUInteger)parentRelativeLevel { return _relativeLevel; }
- (NSUInteger)parentRelativeSlice { return _relativeSlice; }
- (NSUInteger)arrayLength { uint32_t l = _parent ? _viewSlices : ([self nvi]->layers ?: 1), t = (uint32_t)[self textureType]; return t == 6 ? l / 6 : (t == 1 || t == 3 || t == 8) ? l : 1; }
- (NSUInteger)sampleCount { return [self nvi]->samples > 1 ? [self nvi]->samples : 1; }
- (MTLTextureType)textureType { return (MTLTextureType)(_parent ? _viewType : [self nvi]->mtl_type); }
- (MTLPixelFormat)pixelFormat { return _fmt ? _fmt : MTLPixelFormatRGBA8Unorm; }
- (IOSurfaceRef)iosurface { return _shareable ? NULL : _surf; }
- (NSUInteger)iosurfacePlane { return _plane; }
- (MTLStorageMode)storageMode { NVMTLTexture *r = _parent ? _parent : self; if (r->_shareable) return MTLStorageModePrivate;
    NVMTLHeap *hp = r->_heap ? r->_heap : r->_subHeap; return hp ? hp->_storage : (MTLStorageMode)((r->_ropt >> MTLResourceStorageModeShift) & 0xF); }
- (BOOL)isFramebufferOnly { NVMTLTexture *r = _parent ? _parent : self; return r->_fbo; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (MTLCPUCacheMode)cpuCacheMode { NVMTLTexture *r = _parent ? _parent : self; return nvmtl_res_cache(r->_heap ? r->_heap : r->_subHeap, r->_ropt); }
- (MTLHazardTrackingMode)hazardTrackingMode { NVMTLTexture *r = _parent ? _parent : self; return nvmtl_res_hazard(r->_heap ? r->_heap : r->_subHeap, r->_ropt); }
- (MTLResourceOptions)resourceOptions { return ((MTLResourceOptions)[self storageMode] << MTLResourceStorageModeShift) | (MTLResourceOptions)[self cpuCacheMode]
                                            | ((MTLResourceOptions)[self hazardTrackingMode] << MTLResourceHazardTrackingModeShift); }
- (BOOL)isAliasable { @synchronized (self) { return _aliased; } }
- (void)makeAliasable {
    NVMTLHeap *h = nil; NSUInteger c = 0, o = 0; BOOL p = NO;
    @synchronized (self) {
        if (!_subHeap || _aliased) return;
        h = _subHeap; c = _subCharge; _subCharge = 0; _aliased = YES;
        p = _subPlaced; o = _heapOffset; _subPlaced = NO;
    }
    if (c) { if (p) [h nvmtlGiveRange:o length:c]; [h nvmtlReleaseSubAllocation:c]; }
}
- (MTLTextureUsage)usage { if (_usage) return (MTLTextureUsage)_usage; if (_parent) return [_parent usage]; return MTLTextureUsageShaderRead; }
- (id<MTLBuffer>)buffer { return _backBuf ? _backBuf : _parent ? [_parent buffer] : nil; }
- (NSUInteger)bufferOffset { return _backBuf ? _backOff : _parent ? [_parent bufferOffset] : 0; }
- (NSUInteger)bufferBytesPerRow { return _backBuf ? _backBPR : _parent ? [_parent bufferBytesPerRow] : 0; }
- (BOOL)isShareable { return _parent ? NO : _shareable; }
- (MTLTextureSwizzleChannels)swizzle { return _hasSwz ? _swz : MTLTextureSwizzleChannelsMake(MTLTextureSwizzleRed, MTLTextureSwizzleGreen, MTLTextureSwizzleBlue, MTLTextureSwizzleAlpha); }
- (BOOL)isSparse { NVMTLTexture *r = _parent ? _parent : self; return (r->_heap && r->_heap->_type == MTLHeapTypeSparse) || (r->_subHeap && r->_subHeap->_type == MTLHeapTypeSparse); }
- (uint32_t)swizzleKey { MTLTextureSwizzleChannels c = [self swizzle]; return (uint32_t)c.red | (uint32_t)c.green << 8 | (uint32_t)c.blue << 16 | (uint32_t)c.alpha << 24; }
- (BOOL)allowGPUOptimizedContents { return YES; }
- (MTLTextureCompressionType)compressionType { return MTLTextureCompressionTypeLossless; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s {
    if (_parent || _viewParent || _backBuf || _heap || _subHeap) return MTLPurgeableStateNonVolatile;
    MTLPurgeableState prev = nvmtl_purge(&_purge, s);
    nvmtl_purge_track(self, s);
    if (s == MTLPurgeableStateNonVolatile) {
        int one = 1;
        if (__atomic_compare_exchange_n(&_rstate, &one, 2, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
            int r = __atomic_load_n(&_resRefs, __ATOMIC_ACQUIRE) ? 1 : nvmtl_vk_image_reback(&_i, 0, _fmt == MTLPixelFormatA8Unorm);
            nvlog("residency (res1): a purged %ux%u texture was made NonVolatile -> %s", _i.w, _i.h,
                  r == 0 ? "paged back onto the card" : r == 1 ? "stays in system memory (the wall or a command buffer said no)" : "rebuild FAILED - stays in system memory");
            __atomic_store_n(&_rstate, 0, __ATOMIC_RELEASE);
        }
    }
    return prev;
}
- (void)addDebugMarker:(NSString *)m range:(NSRange)r {}
- (void)removeAllDebugMarkers {}
- (id)newSharedTextureHandle {
    if (_parent || !_shareable || !_surf) return nil;
    if (![MTLSharedTextureHandle instancesRespondToSelector:@selector(initWithIOSurface:label:)]) {
        static int said; if (!said++) nvlog("newSharedTextureHandle: MTLSharedTextureHandle has no -initWithIOSurface:label: on this OS - nil (apps1)");
        return nil; }
    MTLSharedTextureHandle *h = [[MTLSharedTextureHandle alloc] initWithIOSurface:_surf label:_label];
    if (!h) { static int said; if (said++ < 4) nvlog("newSharedTextureHandle: Metal refused surface %u (its kMetalRegistryID names no live device) - nil (apps1)",
                                                   IOSurfaceGetID(_surf)); }
    return h;
}
- (id<MTLTexture>)remoteStorageTexture { return nil; }
- (id<MTLTexture>)newRemoteTextureViewForDevice:(id<MTLDevice>)d {
    return nil; }
// An app's region, level and slice are 64-bit; the copy path below takes 32-bit. A value that does not fit
// describes no texture, and narrowing it produces a DIFFERENT region that then passes every bounds check that
// follows: an origin of 2^32 reads as 0, so the copy lands at the start of the texture instead of being
// refused. The texture-buffer paths also add origin and width together, which wraps for an origin near 2^64
// and would memcpy far outside the buffer. Apple raises an exception on a region like this; refuse it here,
// before anything is narrowed.
static BOOL nvmtl_region_fits(MTLRegion r, NSUInteger level, NSUInteger slice) {
    return r.origin.x <= UINT32_MAX && r.origin.y <= UINT32_MAX && r.origin.z <= UINT32_MAX
        && r.size.width <= UINT32_MAX && r.size.height <= UINT32_MAX && r.size.depth <= UINT32_MAX
        && level <= UINT32_MAX && slice <= UINT32_MAX;
}
// ---- GPU-encoded indirect command buffers (NVMTL_GPU_ICB=1)
int nvmtl_gpu_icb_on(void) { static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_GPU_ICB"); on = e && e[0] == '1'; } return on; }
static NSHashTable *g_icb_cand[2]; static os_unfair_lock g_icb_cand_lock = OS_UNFAIR_LOCK_INIT;
void nvmtl_icb_register_pipeline(id ps, BOOL compute) {
    os_unfair_lock_lock(&g_icb_cand_lock);
    if (!g_icb_cand[compute]) g_icb_cand[compute] = [NSHashTable weakObjectsHashTable];
    [g_icb_cand[compute] addObject:ps];
    os_unfair_lock_unlock(&g_icb_cand_lock);
}
NSArray *nvmtl_icb_pipelines(BOOL compute) {
    os_unfair_lock_lock(&g_icb_cand_lock); NSArray *a = g_icb_cand[compute].allObjects ?: @[]; os_unfair_lock_unlock(&g_icb_cand_lock); return a;
}
// ---- GBGR422 / BGRG422 (nvmtl_yuv422.h): the image is decoded RGBA8, _y422Raw holds the packed bytes the app wrote
static _Thread_local int g_y422_bypass;   // set while the blit encoder copies its own decoded scratch into a 4:2:2 texture
int nvmtl_y422_blit_refused(id tex, const char *what) {
    if (g_y422_bypass) return 0;
    if (![tex isKindOfClass:[NVMTLTexture class]] || !((NVMTLTexture *)tex)->_y422) return 0;
    static _Atomic unsigned n; if (atomic_fetch_add(&n, 1) < 8) nvlog("%s: a 4:2:2 texture is not a blit source or destination in this driver - REFUSED (use replaceRegion / getBytes)", what);
    return 1;
}
- (void)nvmtlY422Upload:(uint32_t)y0 rows:(uint32_t)n {
    const uint32_t w = _i.w; const size_t rb = nvmtl_y422_row_bytes(w);
    uint8_t *rgba = malloc((size_t)w * 4 * n); if (!rgba) { nvlog("4:2:2 upload: out of memory"); return; }
    for (uint32_t y = 0; y < n; y++) nvmtl_y422_decode_row(_y422, (const uint8_t *)_y422Raw.bytes + (size_t)(y0 + y) * rb, w, rgba + (size_t)y * w * 4);
    if (nvmtl_vk_image_write_region_level_layer(_q, [self nvi], rgba, (size_t)w * 4, 0, y0, w, n, 0, 0)) nvlog("4:2:2 upload FAILED");
    free(rgba);
}
// 0 when the region is a whole-pair, in-bounds rectangle of this texture's single level and slice
- (BOOL)nvmtlY422Region:(MTLRegion)r level:(NSUInteger)level slice:(NSUInteger)slice what:(const char *)what {
    if (level || slice || r.origin.z || r.size.depth > 1 || r.origin.x > _i.w || r.size.width > _i.w - r.origin.x || r.origin.y > _i.h || r.size.height > _i.h - r.origin.y
        || !nvmtl_y422_region_ok((uint32_t)r.origin.x, (uint32_t)r.size.width)) {
        nvlog("%s: 4:2:2 region (%lu,%lu %lux%lu level %lu slice %lu) is outside the texture or not whole pairs - REFUSED", what, (unsigned long)r.origin.x, (unsigned long)r.origin.y,
              (unsigned long)r.size.width, (unsigned long)r.size.height, (unsigned long)level, (unsigned long)slice);
        return NO; }
    return YES;
}
- (void)nvmtlY422Replace:(MTLRegion)r withBytes:(const void *)bytes bytesPerRow:(NSUInteger)row {
    if (!bytes || !r.size.width || !r.size.height) return;
    const size_t rb = nvmtl_y422_row_bytes(_i.w), span = (size_t)r.size.width * 2u;
    if (row < span) { nvlog("replaceRegion: 4:2:2 bytesPerRow %lu is less than the %zu bytes of a row of the region - REFUSED", (unsigned long)row, span); return; }
    for (NSUInteger y = 0; y < r.size.height; y++)
        memcpy((uint8_t *)_y422Raw.mutableBytes + (size_t)(r.origin.y + y) * rb + (size_t)r.origin.x * 2u, (const uint8_t *)bytes + y * row, span);
    [self nvmtlY422Upload:(uint32_t)r.origin.y rows:(uint32_t)r.size.height];   // a pair's chroma reaches its neighbours: redecode the whole rows
}
- (void)nvmtlY422Get:(MTLRegion)r into:(void *)bytes bytesPerRow:(NSUInteger)row {
    if (_y422GpuWritten) { nvlog("getBytes: this 4:2:2 texture was written by a GPU copy; the packed bytes are not kept - REFUSED"); return; }
    const size_t rb = nvmtl_y422_row_bytes(_i.w), span = (size_t)r.size.width * 2u;
    if (!bytes || row < span) return;
    for (NSUInteger y = 0; y < r.size.height; y++)
        memcpy((uint8_t *)bytes + y * row, (const uint8_t *)_y422Raw.bytes + (size_t)(r.origin.y + y) * rb + (size_t)r.origin.x * 2u, span);
}
- (void)getBytes:(void *)bytes bytesPerRow:(NSUInteger)row fromRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level {
    if (!nvmtl_region_fits(region, level, 0)) { nvlog("getBytes: the region does not fit the copy path — REFUSED"); return; }
    if (_y422) { if ([self nvmtlY422Region:region level:level slice:0 what:"getBytes"]) [self nvmtlY422Get:region into:bytes bytesPerRow:row]; return; }
    if (level >= [self mipmapLevelCount]) { nvlog("getBytes: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    [self nvmtlSurfaceIn];
    nvmtl_census("getBytes", (uint32_t)region.size.width, (uint32_t)region.size.height, _surf != NULL);
    if (nvmtl_vk_image_read_region_level_layer(_q, [self nvi], (uint8_t *)bytes, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                               (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, _baseSlice))
        nvlog("texture getBytes: readback FAILED");
}
- (void)getBytes:(void *)bytes bytesPerRow:(NSUInteger)row bytesPerImage:(NSUInteger)img
      fromRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level slice:(NSUInteger)slice {
    if (!nvmtl_region_fits(region, level, slice)) { nvlog("getBytes:slice: the region does not fit the copy path — REFUSED"); return; }
    if (_y422) { if ([self nvmtlY422Region:region level:level slice:slice what:"getBytes"]) [self nvmtlY422Get:region into:bytes bytesPerRow:row]; return; }
    if (_tbView) {
        uint8_t *src = (uint8_t *)[(id<MTLBuffer>)_backBuf contents];
        if (!src || region.origin.x > _i.w || region.size.width > _i.w - region.origin.x) { nvlog("getBytes: texture buffer is not CPU-visible or the region is outside it"); return; }
        memcpy(bytes, src + _backOff + region.origin.x * _i.bpp, region.size.width * _i.bpp); return; }
    if (level >= [self mipmapLevelCount]) { nvlog("getBytes: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    [self nvmtlSurfaceIn];
    nvk_image *im = [self nvi]; BOOL vol = im->mtl_type == 7; NSUInteger nz = vol ? (region.size.depth ? region.size.depth : 1) : 1;
    NSUInteger per = img ? img : row * region.size.height; int rc = 0;
    for (NSUInteger zi = 0; zi < nz && !rc; zi++)
        rc = nvmtl_vk_image_read_region_level_layer(_q, im, (uint8_t *)bytes + zi * per, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                                    (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, (uint32_t)(vol ? region.origin.z + zi : slice + _baseSlice));
    if (rc) nvlog("texture getBytes:slice: readback FAILED");
}
// CoreAnimation uploads only level 0 of the mipmapped texture it draws a Dock badge digit into and samples it trilinear,
// so the levels nobody wrote need filling; a write to a higher level or generateMipmaps sets _mipsWritten and ends that.
static void nvmtl_mip_autofill(NVMTLTexture *t, const void *bytes, size_t row, MTLRegion region, NSUInteger level) {
    NVMTLTexture *r = t->_parent ?: t;
    if (level + t->_baseLevel > 0) { r->_mipsWritten = YES; return; }
    NSUInteger mips = t.mipmapLevelCount; if (mips <= 1 || r->_mipsWritten || t->_parent || !bytes || !row) return;
    nvk_image *im = [t nvi]; if (!im) return; uint32_t w = im->w, h = im->h;
    if (im->mtl_type != 2 || im->bw > 1 || (im->layers ? im->layers : 1) > 1 || w > 2048 || h > 2048) return;
    if (region.origin.x || region.origin.y || region.size.width != w || region.size.height != h) return;
    unsigned bpp; switch (t.pixelFormat) {
        case MTLPixelFormatA8Unorm: case MTLPixelFormatR8Unorm: case MTLPixelFormatR8Unorm_sRGB: bpp = 1; break;
        case MTLPixelFormatRG8Unorm: case MTLPixelFormatRG8Unorm_sRGB: bpp = 2; break;
        case MTLPixelFormatRGBA8Unorm: case MTLPixelFormatRGBA8Unorm_sRGB: case MTLPixelFormatBGRA8Unorm: case MTLPixelFormatBGRA8Unorm_sRGB: bpp = 4; break;
        default: return; }
    if (bpp != im->bpp) return;
    size_t cap = (size_t)((w + 1) / 2) * ((h + 1) / 2) * bpp; uint8_t *cur = malloc(cap), *nxt = malloc(cap);
    if (!cur || !nxt) { free(cur); free(nxt); return; }
    const uint8_t *src = bytes; size_t srow = row; uint32_t sw = w, sh = h;
    for (NSUInteger l = 1; l < mips; l++) {
        uint32_t dw = sw > 1 ? sw / 2 : 1, dh = sh > 1 ? sh / 2 : 1;
        for (uint32_t y = 0; y < dh; y++) for (uint32_t x = 0; x < dw; x++) {
            uint32_t x0 = 2 * x, x1 = 2 * x + 1 < sw ? 2 * x + 1 : x0, y0 = 2 * y, y1 = 2 * y + 1 < sh ? 2 * y + 1 : y0;
            const uint8_t *p00 = src + (size_t)y0 * srow + (size_t)x0 * bpp, *p01 = src + (size_t)y0 * srow + (size_t)x1 * bpp;
            const uint8_t *p10 = src + (size_t)y1 * srow + (size_t)x0 * bpp, *p11 = src + (size_t)y1 * srow + (size_t)x1 * bpp;
            uint8_t *d = nxt + ((size_t)y * dw + x) * bpp;
            for (unsigned c = 0; c < bpp; c++) d[c] = (uint8_t)((p00[c] + p01[c] + p10[c] + p11[c] + 2) / 4); }
        if (nvmtl_vk_image_write_region_level_layer(t->_q, im, nxt, (size_t)dw * bpp, 0, 0, dw, dh, (uint32_t)l, t->_baseSlice)) break;
        uint8_t *tmp = cur; cur = nxt; nxt = tmp; src = cur; srow = (size_t)dw * bpp; sw = dw; sh = dh;
    }
    free(cur); free(nxt);
}
- (void)replaceRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level withBytes:(const void *)bytes bytesPerRow:(NSUInteger)row {
    if (!nvmtl_region_fits(region, level, 0)) { nvlog("replaceRegion: the region does not fit the copy path — REFUSED"); return; }
    if (_y422) { if ([self nvmtlY422Region:region level:level slice:0 what:"replaceRegion"]) [self nvmtlY422Replace:region withBytes:bytes bytesPerRow:row]; return; }
    if (level >= [self mipmapLevelCount]) { nvlog("replaceRegion: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    if (region.size.depth > 1 || region.origin.z) { [self replaceRegion:region mipmapLevel:level slice:0 withBytes:bytes bytesPerRow:row bytesPerImage:row * region.size.height]; return; }
    { int was = nvtrace_scope([self nvi]->w, [self nvi]->h);
      if (nvmtl_trace_on()) { unsigned nz, nn; size_t valid = (size_t)region.size.width * [self nvi]->bpp; if (row && valid > row) valid = row;
        nvmtl_nonzero(bytes, row, valid, (uint32_t)region.size.height, &nz, &nn);
        nvtrace("REPLACE %lux%lu at %lu,%lu level %lu row %lu -> tex %ux%u surf %u | nonzero %u/%u sampled bytes", (unsigned long)region.size.width,
            (unsigned long)region.size.height, (unsigned long)region.origin.x, (unsigned long)region.origin.y, (unsigned long)level, (unsigned long)row,
            [self nvi]->w, [self nvi]->h, nvmtl_surf_id(self), nz, nn); }
      g_trace_mute = was; }
    if (!bytes || row < (size_t)_i.w * (_i.bpp ? _i.bpp : 4) || !nvmtl_surface_full_overwrite(self, region, level, 0)) [self nvmtlSurfaceIn];
    if (nvmtl_vk_image_write_region_level_layer(_q, [self nvi], (const uint8_t *)bytes, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                                (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, _baseSlice))
        nvlog("replaceRegion: upload FAILED");
    else { [self nvmtlSurfaceOut]; nvmtl_mip_autofill(self, bytes, row, region, level); }
}
- (void)replaceRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level slice:(NSUInteger)slice
            withBytes:(const void *)bytes bytesPerRow:(NSUInteger)row bytesPerImage:(NSUInteger)img {
    if (!nvmtl_region_fits(region, level, slice)) { nvlog("replaceRegion:slice: the region does not fit the copy path — REFUSED"); return; }
    if (_y422) { if ([self nvmtlY422Region:region level:level slice:slice what:"replaceRegion"]) [self nvmtlY422Replace:region withBytes:bytes bytesPerRow:row]; return; }
    if (_tbView) {
        id<MTLBuffer> bb = (id<MTLBuffer>)_backBuf; uint8_t *dst = (uint8_t *)[bb contents];
        if (!dst || region.origin.x > _i.w || region.size.width > _i.w - region.origin.x) { nvlog("replaceRegion: texture buffer is not CPU-visible or the region is outside it"); return; }
        const NSUInteger at = _backOff + region.origin.x * _i.bpp, len = region.size.width * _i.bpp; memcpy(dst + at, bytes, len);
        if (bb.storageMode == MTLStorageModeManaged) [bb didModifyRange:NSMakeRange(at, len)];
        return; }
    if (level >= [self mipmapLevelCount]) { nvlog("replaceRegion: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    if (!bytes || row < (size_t)_i.w * (_i.bpp ? _i.bpp : 4) || !nvmtl_surface_full_overwrite(self, region, level, slice)) [self nvmtlSurfaceIn];
    nvk_image *im = [self nvi]; BOOL vol = im->mtl_type == 7; NSUInteger nz = vol ? (region.size.depth ? region.size.depth : 1) : 1;
    if (!vol && (region.size.depth > 1 || region.origin.z)) { nvlog("replaceRegion:slice: depth %lu@z%lu on a non-3D texture — REFUSED", (unsigned long)region.size.depth, (unsigned long)region.origin.z); return; }
    NSUInteger per = img ? img : row * region.size.height; int rc = 0;
    for (NSUInteger zi = 0; zi < nz && !rc; zi++)
        rc = nvmtl_vk_image_write_region_level_layer(_q, im, (const uint8_t *)bytes + zi * per, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                                     (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, (uint32_t)(vol ? region.origin.z + zi : slice + _baseSlice));
    if (rc) nvlog("replaceRegion:slice: upload FAILED");
    else { [self nvmtlSurfaceOut]; if (!vol) nvmtl_mip_autofill(self, bytes, row, region, level); }
}
// A view of a texture buffer is another texel-buffer view of the same bytes. Apple's GL asks for one whenever a buffer
// texture is sampled through a sampler of another type (Minecraft 26's R8Sint/R32Float buffer textures), every draw
// until it gets one.
static NVMTLTexture *nvmtl_texbuf_view(NVMTLTexture *t, MTLPixelFormat f) {
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (!t->_backBuf || nvmtl_pixfmt_public(f, &vkf, &bpp, &a8) || a8 || !bpp || !t->_backBPR || t->_backBPR % bpp) {
        nvlog("newTextureView: texture-buffer view as format %lu is not carried -> nil", (unsigned long)f); return nil; }
    MTLTextureDescriptor *d = [MTLTextureDescriptor textureBufferDescriptorWithPixelFormat:f width:t->_backBPR / bpp
                                                                            resourceOptions:[t->_backBuf resourceOptions] usage:t->_usage];
    return nvmtl_texbuf_make(t->_backBuf, d, t->_backOff);
}
- (id<MTLTexture>)nvmtlViewWithFormat:(MTLPixelFormat)f levels:(NSRange)levels {
    if (!levels.length || levels.location >= self.mipmapLevelCount || levels.length > self.mipmapLevelCount - levels.location) return nil;
    if ([self nvi]->mtl_type == 9) return nvmtl_texbuf_view(self, f);
    NVMTLTexture *v = [NVMTLTexture new];
    { NVMTLTexture *vp_ = _parent ? _parent : self; __atomic_fetch_add(&vp_->_nviews, 1, __ATOMIC_ACQ_REL); }  v->_parent = _parent ? _parent : self; v->_usage = [self usage];
    v->_q = _q; v->_fmt = f; v->_surf = _surf; v->_plane = _plane;
    v->_mips = (uint32_t)levels.length;
    v->_viewParent = self; v->_relativeLevel = (uint32_t)levels.location;
    v->_viewType = (uint32_t)self.textureType;
    v->_viewSlices = _parent ? _viewSlices : (v->_viewType == 7 ? 1 : ([self nvi]->layers ?: 1));
    v->_baseSlice = _baseSlice; v->_baseLevel = _baseLevel + (uint32_t)levels.location;
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    extern int nvmtl_depthfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect);
    uint32_t dvk = 0, dasp = 0;
    if (nvmtl_depthfmt_public(f, &dvk, &dasp) == 0) {
        if (nvmtl_vk_image_view_create_range_aspect([v nvi], dvk, 0, dasp, v->_baseLevel, v->_mips, v->_baseSlice, v->_viewSlices, v->_viewType, &v->_ownView)) { nvlog("newTextureView: depth/stencil view FAILED (format %lu)", (unsigned long)f); return nil; }
        v->_stex = NO;
        nvlog("newTextureView: depth/stencil view format %lu aspect %u -> %p", (unsigned long)f, dasp, (__bridge void *)v);
        return v;
    }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt_public(f, &vkf, &bpp, &a8)) return nil;
    // a remapped texture's bits are R10G10B10A2: a BGR10A2 view reads them as 64, an RGB10A2 view as 58, so a
    // format-reinterpreting view means what it means on Metal
    { NVMTLTexture *root = _parent ? _parent : self;
      if (root->_rbSwap) { if (vkf == 58u) { vkf = 64u; v->_rbSwap = YES; } else if (vkf == 64u) vkf = 58u; } }
    if (vkf != [v nvi]->fmt && bpp != [v nvi]->bpp) {
        nvlog("newTextureView: pixel format %lu is %u B per element, its texture's is %u B - nil", (unsigned long)f, bpp, [v nvi]->bpp); return nil; }
    if (nvmtl_vk_image_view_create_range([v nvi], vkf, a8, v->_baseLevel, v->_mips, v->_baseSlice, v->_viewSlices, v->_viewType, &v->_ownView)) {
        nvlog("newTextureView: view create FAILED"); return nil;
    }
    v->_stex = [v nvi]->storage && !a8 && nvmtl_vk_format_is_storage(vkf);
    return v;
}
int nvmtl_vk_cmd_attachment_view_fmt(nvk_cmdbuf *c, nvk_image *img, int depth, uint32_t level, uint32_t slice, uint32_t vkfmt, int a8,
                                     void **out, uint32_t *w, uint32_t *h);
static uint32_t nvmtl_tex_attach_vkfmt(NVMTLTexture *t, int *a8) {
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    uint32_t vk = 0, bpp = 0; *a8 = 0;
    if (!t->_fmt || nvmtl_pixfmt_public(t->_fmt, &vk, &bpp, a8)) { *a8 = 0; return 0; }
    return vk;
}
- (id<MTLTexture>)newTextureViewWithPixelFormat:(MTLPixelFormat)f {
    return [self nvmtlViewWithFormat:f levels:NSMakeRange(0, [self mipmapLevelCount])];
}
- (id<MTLTexture>)newTextureViewWithPixelFormat:(MTLPixelFormat)f textureType:(MTLTextureType)tt
                                         levels:(NSRange)levels slices:(NSRange)slices {
    NSUInteger availableSlices = self.textureType == MTLTextureTypeCube ? 6 : self.textureType == MTLTextureTypeCubeArray ? self.arrayLength * 6 : self.arrayLength;
    if (!levels.length || levels.location >= self.mipmapLevelCount || levels.length > self.mipmapLevelCount - levels.location ||
        !slices.length || slices.location >= availableSlices || slices.length > availableSlices - slices.location) return nil;
    if (tt == MTLTextureType2D && !slices.location && slices.length <= 1 && [self nvi]->mtl_type == 2) return [self nvmtlViewWithFormat:f levels:levels];
    if (tt == MTLTextureType2DMultisample && !slices.location && slices.length <= 1 && [self nvi]->mtl_type == 4) return [self nvmtlViewWithFormat:f levels:levels];
    if (tt == MTLTextureTypeTextureBuffer || [self nvi]->mtl_type == 9)
        return tt == MTLTextureTypeTextureBuffer && [self nvi]->mtl_type == 9 ? nvmtl_texbuf_view(self, f) : nil;
    if (tt == MTLTextureType3D && [self nvi]->mtl_type == 7 && !slices.location) slices = NSMakeRange(0, 1);
    if (((tt == MTLTextureType1D || tt == MTLTextureType1DArray) && [self nvi]->mtl_type != (uint32_t)tt) || (tt == MTLTextureType2DMultisample && ([self nvi]->mtl_type != 4 && [self nvi]->mtl_type != 8)) || (tt == MTLTextureType2DMultisampleArray && [self nvi]->mtl_type != 8) || (([self nvi]->mtl_type == 4 || [self nvi]->mtl_type == 8) && tt != MTLTextureType2DMultisample && tt != MTLTextureType2DMultisampleArray) || (tt == MTLTextureType3D && [self nvi]->mtl_type != 7)) { nvlog("newTextureView: textureType %lu of a %u-typed texture — not carried", (unsigned long)tt, [self nvi]->mtl_type); return nil; }
    NVMTLTexture *v = [NVMTLTexture new];
    { NVMTLTexture *vp_ = _parent ? _parent : self; __atomic_fetch_add(&vp_->_nviews, 1, __ATOMIC_ACQ_REL); }  v->_parent = _parent ? _parent : self; v->_usage = [self usage];  v->_q = _q; v->_fmt = f; v->_surf = _surf; v->_plane = _plane; v->_mips = (uint32_t)levels.length;
    v->_viewParent = self; v->_relativeLevel = (uint32_t)levels.location; v->_relativeSlice = (uint32_t)slices.location;
    v->_viewType = (uint32_t)tt; v->_viewSlices = (uint32_t)slices.length;
    v->_baseSlice = _baseSlice + (uint32_t)slices.location; v->_baseLevel = _baseLevel + (uint32_t)levels.location;
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    extern int nvmtl_depthfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect);
    uint32_t depthFormat = 0, depthAspect = 0;
    if (!nvmtl_depthfmt_public(f, &depthFormat, &depthAspect)) {
        if (depthFormat != [v nvi]->fmt || nvmtl_vk_image_view_create_range_aspect([v nvi], depthFormat, 0, depthAspect, v->_baseLevel, v->_mips, v->_baseSlice, v->_viewSlices, (uint32_t)tt, &v->_ownView)) return nil;
        v->_stex = NO;
        return v;
    }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt_public(f, &vkf, &bpp, &a8)) return nil;
    // a remapped texture's bits are R10G10B10A2: a BGR10A2 view reads them as 64, an RGB10A2 view as 58, so a
    // format-reinterpreting view means what it means on Metal
    { NVMTLTexture *root = _parent ? _parent : self;
      if (root->_rbSwap) { if (vkf == 58u) { vkf = 64u; v->_rbSwap = YES; } else if (vkf == 64u) vkf = 58u; } }
    if (vkf != [v nvi]->fmt && bpp != [v nvi]->bpp) {
        nvlog("newTextureView: pixel format %lu is %u B per element, its texture's is %u B - nil", (unsigned long)f, bpp, [v nvi]->bpp); return nil; }
    uint32_t nslices = (uint32_t)slices.length;
    if (nvmtl_vk_image_view_create_range([v nvi], vkf, a8, v->_baseLevel, v->_mips, v->_baseSlice, nslices, (uint32_t)tt, &v->_ownView)) { nvlog("newTextureView: typed view FAILED"); return nil; }
    v->_stex = [v nvi]->storage && !a8 && nvmtl_vk_format_is_storage(vkf);
    return v;
}
// macOS 26: the same view as newTextureViewWithPixelFormat:textureType:levels:slices:swizzle:, described by an
// MTLTextureViewDescriptor. The descriptor is read by key because the plugin is built for macOS 15, where the class does
// not exist. A field left at zero means "as the texture is": no pixel format keeps the texture's, an empty level or slice
// range covers every level or slice, and an all-zero swizzle is the identity.
- (id<MTLTexture>)newTextureViewWithDescriptor:(id)d {
    if (!d) return nil;
    MTLPixelFormat f = [[d valueForKey:@"pixelFormat"] unsignedIntegerValue];
    MTLTextureType tt = (MTLTextureType)[[d valueForKey:@"textureType"] unsignedIntegerValue];
    NSRange levels = [[d valueForKey:@"levelRange"] rangeValue], slices = [[d valueForKey:@"sliceRange"] rangeValue];
    MTLTextureSwizzleChannels sw = { 0, 0, 0, 0 };
    { id v = [d valueForKey:@"swizzle"];
      if ([v isKindOfClass:[NSValue class]] && !strcmp([(NSValue *)v objCType], @encode(MTLTextureSwizzleChannels))) [(NSValue *)v getValue:&sw size:sizeof sw]; }
    if (f == MTLPixelFormatInvalid) f = self.pixelFormat;
    if (!levels.length) levels = NSMakeRange(0, self.mipmapLevelCount);
    if (!slices.length) slices = NSMakeRange(0, self.textureType == MTLTextureTypeCube ? 6 : self.textureType == MTLTextureTypeCubeArray ? self.arrayLength * 6 : self.arrayLength);
    if (!sw.red && !sw.green && !sw.blue && !sw.alpha) sw = MTLTextureSwizzleChannelsMake(MTLTextureSwizzleRed, MTLTextureSwizzleGreen, MTLTextureSwizzleBlue, MTLTextureSwizzleAlpha);
    return [self newTextureViewWithPixelFormat:f textureType:tt levels:levels slices:slices swizzle:sw];
}
- (id<MTLTexture>)newTextureViewWithPixelFormat:(MTLPixelFormat)f textureType:(MTLTextureType)tt
                                         levels:(NSRange)levels slices:(NSRange)slices
                                        swizzle:(MTLTextureSwizzleChannels)sw {
    const BOOL identity = sw.red == MTLTextureSwizzleRed && sw.green == MTLTextureSwizzleGreen && sw.blue == MTLTextureSwizzleBlue && sw.alpha == MTLTextureSwizzleAlpha;
    if (!identity && [self nvi]->mtl_type == 9) {   // a Vulkan texel buffer has no component mapping
        static int said; if (!said++) nvlog("newTextureView:swizzle: a texture buffer cannot be swizzled -> nil (said once)");
        return nil; }
    NVMTLTexture *v = (NVMTLTexture *)[self newTextureViewWithPixelFormat:f textureType:tt levels:levels slices:slices];
    if (!v || identity) return v;
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    uint32_t vkf = 0, bpp = 0; int a8 = 0; void *nv = NULL;
    const uint8_t s4[4] = { (uint8_t)sw.red, (uint8_t)sw.green, (uint8_t)sw.blue, (uint8_t)sw.alpha };
    if (nvmtl_pixfmt_public(f, &vkf, &bpp, &a8) ||
        nvmtl_vk_image_view_create_range_swz([v nvi], vkf, a8, v->_baseLevel, v->_mips, v->_baseSlice, v->_viewSlices, v->_viewType, s4, &nv)) {
        nvlog("newTextureView:swizzle: par1 the swizzled view could not be built (format %lu) — the view keeps the identity mapping", (unsigned long)f);
        return v; }
    if (v->_ownView) nvmtl_vk_image_view_destroy(v->_ownView);
    v->_ownView = nv; v->_stex = NO;
    v->_swz = sw; v->_hasSwz = YES;
    nvlog("newTextureView:swizzle: par1 view %p maps r,g,b,a <- %u,%u,%u,%u", (__bridge void *)v, s4[0], s4[1], s4[2], s4[3]);
    return v;
}
@end

NSArray<NSDictionary *> *nvmtl_argbuf_fields(NVMTLFunction *function, NSUInteger ownerBufferIndex);
NSArray<NSDictionary *> *nvmtl_air_argbuf_layout(NVMTLFunction *fn, NSUInteger bufferIndex, NSUInteger *size, NSUInteger *align);
BOOL nvmtl_needs_visible_link(NSString *air, NSString *why);
void nvmtl_fc_expect_link(int on);
NSData *nvmtl_translate_with_fc(NSString *air, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                                const uint8_t *const *payloads, size_t n, NSString **err);
NSData *nvmtl_translate_with_fc_cached(NSString *air, NSString *stage, NSString *name, NSDictionary *constants,
                                       const uint32_t *idx, const uint32_t *sizes,
                                       const uint8_t *const *payloads, size_t n, NSString **err);

#pragma mark - library / function
id gNVMTLMainDevice = nil;

extern NSData *nvmtl_translate_samplers(NVMTLFunction *, NSDictionary *, uint32_t, NSString **);
extern NSData *nvmtl_translate_samplers_amp(NVMTLFunction *, NSDictionary *, uint32_t, uint32_t, NSString **);
extern NSData *nvmtl_translate_variant(NVMTLFunction *, NSDictionary *, NSDictionary *, NSString **);
// facts about a module's text that newFunctionWithName asks for on every call; kept on the text (NVMTLLibraryLoad.m)
enum { NVMTL_AIRF_KNOWN = 1, NVMTL_AIRF_DYLD_TABLE = 2, NVMTL_AIRF_ISECT_TABLE = 4 };
uint32_t nvmtl_air_text_flags(NSString *ll);
static char gNVMTLEntrySpvAssoc;
@interface NVMTLCIBinding : NSObject { @public NSString *_name; NVMTLType *_t; } @end
@implementation NVMTLCIBinding
- (NSString *)name { return _name ?: @""; }
- (NSUInteger)type { return 18; }
- (MTLBindingAccess)access { return MTLBindingAccessReadOnly; }
- (NSUInteger)index { return 0; }
- (BOOL)isUsed { return YES; }
- (BOOL)isActive { return YES; }
- (BOOL)isArgument { return NO; }
- (NSUInteger)arrayLength { return 1; }
- (id)dataTypeDescription { return _t; }
- (MTLDataType)dataType { return _t ? _t->_dataType : MTLDataTypeNone; }
- (NSString *)description { return [NSString stringWithFormat:@"<CI binding %@ dataType %lu>", _name, (unsigned long)self.dataType]; }
@end
@interface NVMTLCIStructType : NVMTLStructType { @public NSString *_typeName; } @end
@implementation NVMTLCIStructType
- (NSString *)typeName { return _typeName ?: @""; }
@end
static NSString *nvmtl_ci_trim(NSString *t) { return [t stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"!\" "]]; }
static NSString *nvmtl_ci_wtrim(NSString *t) { return [t stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]]; }
@interface NVMTLCIStitchBinding : NVMTLCIBinding { @public NSUInteger _bt; } @end
@implementation NVMTLCIStitchBinding
- (NSUInteger)type { return _bt; }
- (BOOL)isUsed { return NO; }
- (BOOL)isActive { return NO; }
@end
static MTLDataType nvmtl_datatype_for_air_name(NSString *t);
static char nvmtl_ci_key;
static NSString *nvmtl_ci_body(NSDictionary *nodes, NSString *ref) { return [ref isKindOfClass:[NSString class]] ? nodes[ref] : nil; }
static NSString *nvmtl_ci_quoted(NSString *body, NSString *tag) {
    NSString *needle = [NSString stringWithFormat:@"!\"%@\", !\"", tag];
    NSRange r = [body rangeOfString:needle]; if (r.location == NSNotFound) return nil;
    NSUInteger from = NSMaxRange(r);
    NSRange q = [body rangeOfString:@"\"" options:0 range:NSMakeRange(from, body.length - from)];
    return q.location == NSNotFound ? nil : [body substringWithRange:NSMakeRange(from, q.location - from)];
}
static NSArray *nvmtl_ci_refs(NSString *body) {
    NSMutableArray *out = [NSMutableArray new];
    for (NSString *p in [body componentsSeparatedByString:@","]) {
        NSString *t = [p stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
        if ([t hasPrefix:@"!"] && ![t hasPrefix:@"!\""]) [out addObject:t];
    }
    return out;
}
static NSDictionary *nvmtl_ci_entry(NVMTLFunction *fn)
{
    if (!fn || ![fn->_stage isEqualToString:@"visible"]) return nil;
    @synchronized (fn) {
        id have = objc_getAssociatedObject(fn, &nvmtl_ci_key);
        if (have) return have == [NSNull null] ? nil : have;
        NSDictionary *found = nil;
        NSString *ll = fn->_air, *fname = fn->_fname;
        if (ll.length && fname.length && [ll rangeOfString:@"\n!air.ci = "].location != NSNotFound) {
            NSMutableDictionary *nodes = [NSMutableDictionary new]; NSString *list = nil;
            for (NSString *line in [ll componentsSeparatedByString:@"\n"]) {
                if (![line hasPrefix:@"!"]) continue;
                NSRange eq = [line rangeOfString:@" = !{"]; if (eq.location == NSNotFound || ![line hasSuffix:@"}"]) continue;
                NSString *body = [line substringWithRange:NSMakeRange(NSMaxRange(eq), line.length - NSMaxRange(eq) - 1)];
                NSString *key = [line substringToIndex:eq.location];
                if ([key isEqualToString:@"!air.ci"]) list = body; else nodes[key] = body;
            }
            NSString *head = [NSString stringWithFormat:@"ptr @%@,", fname];
            for (NSString *ref in nvmtl_ci_refs(list ?: @"")) {
                NSString *entry = nodes[ref]; if (![entry hasPrefix:head]) continue;
                NSArray *parts = nvmtl_ci_refs([entry substringFromIndex:head.length]);
                if (parts.count < 2) break;
                NSString *retBody = nvmtl_ci_body(nodes, parts[0]);
                if (retBody && !nvmtl_ci_quoted(retBody, @"air.arg_type_name")) retBody = nvmtl_ci_body(nodes, nvmtl_ci_refs(retBody).firstObject);
                NVMTLType *ret = [NVMTLType new];
                NSString *rtn = retBody ? nvmtl_ci_quoted(retBody, @"air.arg_type_name") : nil;
                ret->_dataType = rtn ? nvmtl_datatype_for_air_name(rtn) : MTLDataTypeFloat4;
                NSMutableArray *args = [NSMutableArray new]; NSMutableString *say = [NSMutableString new];
                for (NSString *aref in nvmtl_ci_refs(nvmtl_ci_body(nodes, parts[1]) ?: @"")) {
                    NSString *ab = nodes[aref]; if (!ab) continue;
                    NSString *tn = nvmtl_ci_quoted(ab, @"air.arg_type_name") ?: @"";
                    BOOL builtin = [ab rangeOfString:@"!\"air.ci_builtin\""].location != NSNotFound;
                    NVMTLCIBinding *b = [NVMTLCIBinding new]; b->_t = [NVMTLType new];
                    b->_name = nvmtl_ci_quoted(ab, @"air.arg_name") ?: [NSString stringWithFormat:@"arg%lu", (unsigned long)args.count];
                    MTLDataType dt = nvmtl_datatype_for_air_name(tn);
                    if (!builtin && dt != MTLDataTypeTexture && dt != MTLDataTypeSampler) dt = MTLDataTypePointer;
                    b->_t->_dataType = dt;
                    [args addObject:b]; [say appendFormat:@" %@:%@=%lu", b->_name, tn, (unsigned long)dt];
                }
                found = @{ @"args": args, @"ret": ret };
                nvlog("CI REFL %s: functionType 4, returns %lu, %lu argument(s)%s", fname.UTF8String, (unsigned long)ret->_dataType, (unsigned long)args.count, say.UTF8String);
                break;
            }
        }
        objc_setAssociatedObject(fn, &nvmtl_ci_key, found ?: [NSNull null], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        return found;
    }
}
static MTLDataType nvmtl_ci_air_dtype(NSDictionary *nodes, NSString *ref) {
    NSString *b = ref ? nodes[ref] : nil; if (!b) return MTLDataTypeNone;
    NSArray *p = [b componentsSeparatedByString:@", "];
    NSUInteger n = 1; NSString *el = b;
    if ([b hasPrefix:@"!\"air.vector_type\""] && p.count >= 6) { n = (NSUInteger)[[p[5] stringByReplacingOccurrencesOfString:@"i32 " withString:@""] integerValue]; el = nodes[nvmtl_ci_wtrim(p[4])] ?: @""; }
    NSArray *q = [el componentsSeparatedByString:@", "];
    NSInteger sz = q.count > 1 ? [[q[1] stringByReplacingOccurrencesOfString:@"i32 " withString:@""] integerValue] : 0;
    if (n < 1 || n > 4) return MTLDataTypeNone;
    if ([el hasPrefix:@"!\"air.float_type\""]) return (MTLDataType)((sz == 2 ? MTLDataTypeHalf : MTLDataTypeFloat) + n - 1);
    return MTLDataTypeNone;
}
static char nvmtl_ci_stitch_key;
static NSDictionary *nvmtl_ci_stitch_entry(NVMTLFunction *fn)
{
    if (!fn || ![fn->_stage isEqualToString:@"visible"]) return nil;
    @synchronized (fn) {
        id have = objc_getAssociatedObject(fn, &nvmtl_ci_stitch_key);
        if (have) return have == [NSNull null] ? nil : have;
        NSDictionary *found = nil;
        NSString *ll = fn->_air, *fname = fn->_fname;
        if (ll.length && fname.length && [ll rangeOfString:@"\n!air.visible = "].location != NSNotFound
            && [ll rangeOfString:@"!\"air.stitching_info\""].location != NSNotFound) {
            NSMutableDictionary *nodes = [NSMutableDictionary new]; NSString *list = nil;
            for (NSString *line in [ll componentsSeparatedByString:@"\n"]) {
                if (![line hasPrefix:@"!"]) continue;
                NSRange eq = [line rangeOfString:@" = !{"]; if (eq.location == NSNotFound || ![line hasSuffix:@"}"]) continue;
                NSString *body = [line substringWithRange:NSMakeRange(NSMaxRange(eq), line.length - NSMaxRange(eq) - 1)];
                NSString *key = [line substringToIndex:eq.location];
                if ([key isEqualToString:@"!air.visible"]) list = body; else nodes[key] = body;
            }
            NSString *head = [NSString stringWithFormat:@"ptr @%@,", fname];
            for (NSString *ref in nvmtl_ci_refs(list ?: @"")) {
                NSString *entry = nodes[ref]; if (![entry hasPrefix:head]) continue;
                NSArray *parts = nvmtl_ci_refs([entry substringFromIndex:head.length]);
                if (parts.count < 3) break;
                NSString *outs = nvmtl_ci_body(nodes, parts[0]), *ins = nvmtl_ci_body(nodes, parts[1]), *si = nvmtl_ci_body(nodes, parts[2]);
                if (![si hasPrefix:@"!\"air.stitching_info\""]) break;
                NSString *ob = nvmtl_ci_body(nodes, nvmtl_ci_refs(outs ?: @"").firstObject);
                NVMTLType *ret = [NVMTLType new];
                NSString *rtn = ob ? nvmtl_ci_quoted(ob, @"air.arg_type_name") : nil;
                ret->_dataType = rtn ? nvmtl_datatype_for_air_name(rtn) : MTLDataTypeFloat4;
                NSArray *inRefs = nvmtl_ci_refs(ins ?: @""), *siRefs = nvmtl_ci_refs(si);
                NSMutableArray *args = [NSMutableArray new]; NSMutableString *say = [NSMutableString new];
                for (NSUInteger k = 1; k < siRefs.count; k++) {
                    NSString *sa = nvmtl_ci_body(nodes, siRefs[k]);
                    if (![sa hasPrefix:@"!\"air.stitching_argument\""]) continue;
                    NSArray *sp = [sa componentsSeparatedByString:@", "];
                    NSString *nm = sp.count >= 3 ? [sp[2] stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"!\" "]] : nil;
                    NSString *st = nvmtl_ci_body(nodes, sp.count >= 2 ? [sp[1] stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]] : nil);
                    NSString *tt = nvmtl_ci_body(nodes, nvmtl_ci_refs(st ?: @"").firstObject);
                    NVMTLCIStitchBinding *b = [NVMTLCIStitchBinding new]; b->_t = [NVMTLType new];
                    b->_name = nm.length ? nm : [NSString stringWithFormat:@"arg%lu", (unsigned long)args.count];
                    if ([tt hasPrefix:@"!\"air.struct_type\""]) {
                        b->_bt = 29;
                        NVMTLCIStructType *T = [NVMTLCIStructType new]; T->_dataType = MTLDataTypeStruct;
                        NSArray *tp = [tt componentsSeparatedByString:@", "];
                        T->_typeName = tp.count >= 5 ? nvmtl_ci_trim(tp[4]) : @"";
                        NSString *fl = tp.count >= 6 ? nvmtl_ci_body(nodes, nvmtl_ci_wtrim(tp[5])) : nil;
                        NSMutableArray *ms = [NSMutableArray array];
                        for (NSString *fr in nvmtl_ci_refs(fl ?: @"")) {
                            NSString *fb = nvmtl_ci_body(nodes, fr); if (![fb hasPrefix:@"!\"air.record_field\""]) continue;
                            NSArray *fp = [fb componentsSeparatedByString:@", "];
                            NVMTLStructMember *m = [NVMTLStructMember new];
                            m->_offset = fp.count > 1 ? (NSUInteger)[[fp[1] stringByReplacingOccurrencesOfString:@"i32 " withString:@""] integerValue] : 0;
                            m->_name = fp.count > 4 ? nvmtl_ci_trim(fp[4]) : @"";
                            m->_dataType = nvmtl_ci_air_dtype(nodes, fp.count > 3 ? nvmtl_ci_wtrim(fp[3]) : nil);
                            m->_argIndex = ms.count; [ms addObject:m];
                        }
                        T->_members = ms; b->_t = T;
                    }
                    else {
                        NSUInteger pos = args.count;
                        NSString *ib = pos < inRefs.count ? nvmtl_ci_body(nodes, inRefs[pos]) : nil;
                        NSString *tn = ib ? nvmtl_ci_quoted(ib, @"air.arg_type_name") : nil;
                        b->_bt = 18; b->_t->_dataType = tn ? nvmtl_datatype_for_air_name(tn) : MTLDataTypeNone;
                    }
                    [args addObject:b]; [say appendFormat:@" %@:%lu/%lu", b->_name, (unsigned long)b->_bt, (unsigned long)b->_t->_dataType];
                }
                found = @{ @"args": args, @"ret": ret };
                nvlog("CI STITCH REFL %s: functionType 5, returns %lu, %lu argument(s)%s", fname.UTF8String, (unsigned long)ret->_dataType, (unsigned long)args.count, say.UTF8String);
                break;
            }
        }
        objc_setAssociatedObject(fn, &nvmtl_ci_stitch_key, found ?: [NSNull null], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        return found;
    }
}
@implementation NVMTLFunction
- (NSDictionary<NSString *, MTLFunctionConstant *> *)functionConstantsDictionary {
    static char key;
    NSDictionary *cached = objc_getAssociatedObject(self, &key);
    if (cached) return cached;
    NSMutableDictionary *d = [NSMutableDictionary new];
    Class ci = NSClassFromString(@"MTLFunctionConstantInternal");
    if (!_air.length) {
        static int said; if (!said++) nvlog("functionConstantsDictionary: \"%s\" has no retained AIR - answering EMPTY (said once)", _fname.UTF8String ?: "?");
    } else if (!ci || ![ci instancesRespondToSelector:@selector(initWithName:type:index:required:)]) {
        static int said; if (!said++) nvlog("functionConstantsDictionary: this Metal has no MTLFunctionConstantInternal initWithName:type:index:required: - answering EMPTY (said once)");
    } else {
        static NSRegularExpression *row, *any; static dispatch_once_t once;
        dispatch_once(&once, ^{
            row = [NSRegularExpression regularExpressionWithPattern:@"^![0-9]+ = !\\{[^\\n]*\\.MTL_FC_INIT_[^\\n]*, !\"([^\"]+)\", !\"([^\"]+)\", i32 ([0-9]+), i1 (true|false)\\}$" options:NSRegularExpressionAnchorsMatchLines error:NULL];
            any = [NSRegularExpression regularExpressionWithPattern:@"^![0-9]+ = !\\{[^\\n]*\\.MTL_FC_INIT_" options:NSRegularExpressionAnchorsMatchLines error:NULL];
        });
        NSRange all = NSMakeRange(0, _air.length);
        NSArray<NSTextCheckingResult *> *ms = [row matchesInString:_air options:0 range:all];
        NSUInteger rows = [any numberOfMatchesInString:_air options:0 range:all];
        if (rows != ms.count) { static int said; if (!said++) nvlog("functionConstantsDictionary: \"%s\" has %lu MTL_FC_INIT rows, %lu in the measured shape - the AIR row moved; the rest are NOT answered (said once)", _fname.UTF8String ?: "?", (unsigned long)rows, (unsigned long)ms.count); }
        for (NSTextCheckingResult *r in ms) {
            NSString *ty = [_air substringWithRange:[r rangeAtIndex:1]], *nm = [_air substringWithRange:[r rangeAtIndex:2]];
            NSUInteger ix = (NSUInteger)[[_air substringWithRange:[r rangeAtIndex:3]] longLongValue];
            BOOL req = [[_air substringWithRange:[r rangeAtIndex:4]] isEqualToString:@"true"];
            MTLDataType dt = nvmtl_air_fc_type(ty);
            if (dt == MTLDataTypeNone) { static int said; if (!said++) nvlog("functionConstantsDictionary: AIR type \"%s\" (constant %s) has no MTLDataType mapping - answered as None (said once)", ty.UTF8String, nm.UTF8String); }
            id c = [(id<NVMTLFunctionConstantInit>)[ci alloc] initWithName:nm type:(NSUInteger)dt index:ix required:req];
            if (c) d[nm] = c;
        }
    }
    NSDictionary *out = [d copy];
    objc_setAssociatedObject(self, &key, out, OBJC_ASSOCIATION_RETAIN);
    return out;
}
- (MTLFunctionOptions)options { return MTLFunctionOptionNone; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (const void *)bitCodeHash {
    if (!_bcHashed) {
        NSMutableData *d = [NSMutableData data];
        [d appendData:[(_fname ?: @"") dataUsingEncoding:NSUTF8StringEncoding]];
        if (_air.length)         [d appendData:[_air dataUsingEncoding:NSUTF8StringEncoding]];
        else if (_spirv.length)  [d appendData:_spirv];
        if (_fc.count)           [d appendData:[[_fc description] dataUsingEncoding:NSUTF8StringEncoding]];
        CC_SHA256(d.bytes, (CC_LONG)d.length, _bcHash);
        _bcHashed = YES;
    }
    return _bcHash;
}
- (id<MTLArgumentEncoder>)newArgumentEncoderWithBufferIndex:(NSUInteger)index {
    // The AIR layout names every member (constants and nested structs too) with Metal's [[id]] and offset; the
    // translator's fields add any resource the AIR walk did not reach.
    NSUInteger airSize = 0, airAlign = 0;
    NSMutableArray *fields = [nvmtl_air_argbuf_layout(self, index, &airSize, &airAlign) mutableCopy];
    NSMutableSet *ids = [NSMutableSet set]; for (NSDictionary *f in fields) [ids addObject:f[@"argument_index"]];
    for (NSDictionary *f in nvmtl_argbuf_fields(self, index)) if (![ids containsObject:@([f[@"argument_index"] unsignedLongValue])]) [fields addObject:f];
    if (!fields.count && !airSize) { nvlog("newArgumentEncoderWithBufferIndex:%lu — %s declares no argument buffer there", (unsigned long)index, _fname.UTF8String); return nil; }
    NVMTLArgumentEncoder *e = [NVMTLArgumentEncoder new];
    e->_fields = fields; e->_fieldByIndex = nvmtl_index_argument_fields(fields);
    e->_airSize = airSize; e->_airAlign = airAlign;
    return e;
}
- (NSArray<MTLVertexAttribute *> *)vertexAttributes { return (NSArray<MTLVertexAttribute *> *)nvmtl_vertex_attributes_for(self); }
- (NSArray<MTLAttribute *> *)stageInputAttributes { return (NSArray<MTLAttribute *> *)nvmtl_vertex_attributes_for(self); }
- (MTLPatchType)patchType { uint32_t pt = 0, n = 0; return nvmtl_spirv_tess_info(_spirv.bytes, _spirv.length, &pt, &n) ? (MTLPatchType)pt : MTLPatchTypeNone; }
- (NSInteger)patchControlPointCount {
    uint32_t pt = 0, n = 0; if (!nvmtl_spirv_tess_info(_spirv.bytes, _spirv.length, &pt, &n)) return -1;
    NSDictionary *tr = nvmtl_function_reflection(self)[@"tessellation"];
    NSNumber *k = [tr isKindOfClass:[NSDictionary class]] && [tr[@"control_point_count"] isKindOfClass:[NSNumber class]] ? tr[@"control_point_count"] : nil;
    return k.integerValue > 0 ? k.integerValue : (NSInteger)n;
}
- (id<MTLArgumentEncoder>)newArgumentEncoderWithBufferIndex:(NSUInteger)index reflection:(MTLAutoreleasedArgument *)reflection
    API_DEPRECATED("Use MTLDevice's newArgumentEncoderWithBufferBinding: instead", macos(10.13, 13.0), ios(11.0, 16.0)) {
    if (reflection) *reflection = (MTLArgument *)nvmtl_argument_for_buffer(self, index);
    id<MTLArgumentEncoder> e = [self newArgumentEncoderWithBufferIndex:index];
    nvlog("newArgumentEncoderWithBufferIndex:%lu reflection: -> %p (%s)", (unsigned long)index, (__bridge void *)e, _fname.UTF8String);
    return e;
}
- (NSString *)name { return _specName.length ? _specName : _fname; }
- (MTLFunctionType)functionType {
    if ([_stage isEqualToString:@"visible"] && _air.length && [_air rangeOfString:@"!air.intersection = "].location != NSNotFound) {
        __block BOOL isect = NO; NSString *key = [NSString stringWithFormat:@"ptr @%@,", _fname];
        [_air enumerateLinesUsingBlock:^(NSString *l, BOOL *stop) {
            if ([l hasPrefix:@"!"] && [l rangeOfString:key].location != NSNotFound &&
                ([l rangeOfString:@"!\"air.bounding_box\""].location != NSNotFound || [l rangeOfString:@"!\"air.triangle\""].location != NSNotFound)) { isect = YES; *stop = YES; } }];
        if (isect) return MTLFunctionTypeIntersection;
    }
    if ([_stage isEqualToString:@"visible"]) return nvmtl_ci_entry(self) ? (MTLFunctionType)4 : MTLFunctionTypeVisible;
    if ([_stage isEqualToString:@"kernel"] || [_stage isEqualToString:@"compute"]) return MTLFunctionTypeKernel;
    if ([_stage isEqualToString:@"fragment"]) return MTLFunctionTypeFragment;
    if ([_stage isEqualToString:@"mesh"]) return MTLFunctionTypeMesh;
    if ([_stage isEqualToString:@"object"]) return MTLFunctionTypeObject;
    return MTLFunctionTypeVertex;
}
- (NSArray *)arguments { return (nvmtl_ci_entry(self) ?: nvmtl_ci_stitch_entry(self))[@"args"]; }
- (id)returnType { return (nvmtl_ci_entry(self) ?: nvmtl_ci_stitch_entry(self))[@"ret"]; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
@end
static MTLDataType nvmtl_datatype_for_air_name(NSString *t);
@implementation NVMTLType
- (MTLDataType)dataType { return _dataType; }
@end
@implementation NVMTLVertexAttribute
- (NSString *)name { return _name ?: @""; }
- (NSUInteger)attributeIndex { return _index; }
- (MTLDataType)attributeType { return _dataType; }
- (BOOL)isActive { return YES; }
- (BOOL)isPatchData { return NO; }
- (BOOL)isPatchControlPointData { return NO; }
@end
static NSArray *nvmtl_vertex_attributes_for(NVMTLFunction *fn) {
    NSDictionary *refl = nvmtl_function_reflection(fn);
    NSMutableArray *out = [NSMutableArray new];
    for (NSDictionary *a in refl[@"vertex_attributes"]) {
        if (![a isKindOfClass:[NSDictionary class]]) continue;
        NVMTLVertexAttribute *v = [NVMTLVertexAttribute new];
        v->_index = [a[@"location"] unsignedLongValue];
        v->_name = [a[@"name"] isKindOfClass:[NSString class]] ? a[@"name"] : [NSString stringWithFormat:@"attribute%lu", (unsigned long)v->_index];
        v->_dataType = [a[@"type_name"] isKindOfClass:[NSString class]] ? nvmtl_datatype_for_air_name(a[@"type_name"]) : MTLDataTypeFloat4;
        [out addObject:v];
    }
    return out;
}
@implementation NVMTLTextureReferenceType
- (MTLDataType)textureDataType { return _texDataType; }
- (MTLTextureType)textureType { return _texType; }
- (MTLBindingAccess)access { return _access; }
- (BOOL)isDepthTexture { return _depth; }
@end
@implementation NVMTLStructType
- (NSArray *)members { return _members ?: @[]; }
- (id)memberByName:(NSString *)n { for (NVMTLStructMember *m in _members) if ([m->_name isEqualToString:n]) return m; return nil; }
@end
@implementation NVMTLArrayType
- (MTLDataType)elementType { return _elem; }
- (NSUInteger)arrayLength { return _len; }
- (NSUInteger)stride { return _stride; }
- (NSUInteger)argumentIndexStride { return 0; }
- (id)elementStructType { return nil; }
- (id)elementArrayType { return nil; }
- (id)elementTextureReferenceType { return nil; }
- (id)elementPointerType { return nil; }
@end
@implementation NVMTLPointerType
- (MTLDataType)elementType { return _elem; }
- (MTLBindingAccess)access { return _access; }
- (NSUInteger)alignment { return _align ?: 16; }
- (NSUInteger)dataSize { return _size; }
- (BOOL)elementIsArgumentBuffer { return _isArgBuf; }
- (id)elementStructType { return _elemStruct; }
- (id)elementArrayType { return nil; }
@end
@implementation NVMTLStructMember
- (NSString *)name { return _name ?: @""; }
- (NSUInteger)offset { return _offset; }
- (MTLDataType)dataType { return _dataType; }
- (NSUInteger)argumentIndex { return _argIndex; }
- (id)structType { return _struct; }
- (id)arrayType { return _arr; }
- (id)textureReferenceType { return _texRef; }
- (id)pointerType { return _ptr; }
- (NSString *)description { return [NSString stringWithFormat:@"<member %@ id %lu off %lu type %lu>", _name, (unsigned long)_argIndex, (unsigned long)_offset, (unsigned long)_dataType]; }
@end
@implementation NVMTLArgument
- (NSString *)name { return _name ?: @""; }
- (NSUInteger)type { return _type; }
- (MTLBindingAccess)access { return _access; }
- (NSUInteger)index { return _index; }
- (BOOL)isActive { return _active; }
- (BOOL)isUsed { return _active; }
- (BOOL)isArgument { return YES; }
- (NSUInteger)arrayLength { return _arrayLength ?: 1; }
- (NSUInteger)bufferAlignment { return _bufAlign ?: 16; }
- (NSUInteger)bufferDataSize { return _bufSize; }
- (MTLDataType)bufferDataType { return _bufDataType; }
- (id)bufferStructType { return _bufStruct; }
- (id)bufferPointerType { return _bufPtr; }
- (MTLTextureType)textureType { return _texType; }
- (MTLDataType)textureDataType { return _texDataType; }
- (BOOL)isDepthTexture { return _depth; }
- (NSUInteger)threadgroupMemoryAlignment { return 16; }
- (NSUInteger)threadgroupMemoryDataSize { return 0; }
- (NSString *)description { return [NSString stringWithFormat:@"<arg %@ type %lu index %lu size %lu members %lu>", _name, (unsigned long)_type, (unsigned long)_index, (unsigned long)_bufSize, (unsigned long)_bufStruct->_members.count]; }
@end
@interface NVMTLBufferArgument : NVMTLArgument <MTLBufferBinding> @end
@implementation NVMTLBufferArgument @dynamic access, argument, bufferAlignment, bufferDataSize, bufferDataType, bufferPointerType, bufferStructType, index, name, type, used; @end
@interface NVMTLTextureArgument : NVMTLArgument <MTLTextureBinding> @end
@implementation NVMTLTextureArgument @dynamic access, argument, arrayLength, depthTexture, index, name, textureDataType, textureType, type, used; @end
@interface NVMTLStageInArgument : NVMTLBufferArgument @end
@implementation NVMTLStageInArgument
- (BOOL)isArgument { return NO; }
@end
@implementation NVMTLRenderPipelineReflection
- (NSArray *)vertexArguments { return _v ?: @[]; }
- (NSArray *)fragmentArguments { return _f ?: @[]; }
- (NSArray *)tileArguments { return @[]; }
- (NSArray *)vertexBindings { return _v ?: @[]; }
- (NSArray *)fragmentBindings { return _f ?: @[]; }
- (NSArray *)tileBindings { return @[]; }
- (NSArray *)objectBindings { return _o ?: @[]; }
- (NSArray *)meshBindings { return _m ?: @[]; }
@end
@implementation NVMTLComputePipelineReflection
- (NSArray *)arguments { return _a ?: @[]; }
- (NSArray *)bindings { return _a ?: @[]; }
@end

static MTLDataType nvmtl_datatype_for_air_name(NSString *t) {
    if (!t.length) return MTLDataTypeNone;
    if ([t hasPrefix:@"texture"] || [t hasPrefix:@"depth"]) return MTLDataTypeTexture;
    if ([t isEqualToString:@"sampler"]) return MTLDataTypeSampler;
    static NSDictionary *tab; static dispatch_once_t once;
    dispatch_once(&once, ^{ tab = @{
        @"float": @(MTLDataTypeFloat), @"float2": @(MTLDataTypeFloat2), @"float3": @(MTLDataTypeFloat3), @"float4": @(MTLDataTypeFloat4),
        @"float2x2": @(MTLDataTypeFloat2x2), @"float3x3": @(MTLDataTypeFloat3x3), @"float4x4": @(MTLDataTypeFloat4x4), @"float3x4": @(MTLDataTypeFloat3x4), @"float4x3": @(MTLDataTypeFloat4x3),
        @"half": @(MTLDataTypeHalf), @"half2": @(MTLDataTypeHalf2), @"half3": @(MTLDataTypeHalf3), @"half4": @(MTLDataTypeHalf4),
        @"int": @(MTLDataTypeInt), @"int2": @(MTLDataTypeInt2), @"int3": @(MTLDataTypeInt3), @"int4": @(MTLDataTypeInt4),
        @"uint": @(MTLDataTypeUInt), @"uint2": @(MTLDataTypeUInt2), @"uint3": @(MTLDataTypeUInt3), @"uint4": @(MTLDataTypeUInt4),
        @"short": @(MTLDataTypeShort), @"ushort": @(MTLDataTypeUShort), @"char": @(MTLDataTypeChar), @"uchar": @(MTLDataTypeUChar), @"bool": @(MTLDataTypeBool),
        @"long": @(MTLDataTypeLong), @"ulong": @(MTLDataTypeULong) }; });
    NSNumber *n = tab[t];
    if (n) return (MTLDataType)n.integerValue;
    if ([t hasSuffix:@"*"] || [t hasPrefix:@"device "] || [t hasPrefix:@"constant "]) return MTLDataTypePointer;
    return MTLDataTypeStruct;
}
static MTLBindingAccess nvmtl_access_for(id a) {
    NSString *s = [a isKindOfClass:[NSString class]] ? a : nil;
    if ([s isEqualToString:@"ReadWrite"] || [s isEqualToString:@"Storage"]) return MTLBindingAccessReadWrite;
    if ([s isEqualToString:@"WriteOnly"]) return MTLBindingAccessWriteOnly;
    return MTLBindingAccessReadOnly;
}
static MTLTextureType nvmtl_textype_for_shape(id shape) {
    NSString *s = [shape isKindOfClass:[NSString class]] ? shape : [shape description];
    if (!s.length) return MTLTextureType2D;
    if ([s rangeOfString:@"CubeArray"].location != NSNotFound) return MTLTextureTypeCubeArray;
    if ([s rangeOfString:@"Cube"].location != NSNotFound) return MTLTextureTypeCube;
    if ([s rangeOfString:@"3D"].location != NSNotFound || [s rangeOfString:@"3d"].location != NSNotFound || [s rangeOfString:@"Three"].location != NSNotFound) return MTLTextureType3D;
    if ([s rangeOfString:@"1D"].location != NSNotFound || [s rangeOfString:@"1d"].location != NSNotFound || [s rangeOfString:@"One"].location != NSNotFound)
        return [s rangeOfString:@"Array"].location != NSNotFound ? MTLTextureType1DArray : MTLTextureType1D;
    if ([s rangeOfString:@"Multisample"].location != NSNotFound || [s rangeOfString:@"MS"].location != NSNotFound) return MTLTextureType2DMultisample;
    return [s rangeOfString:@"Array"].location != NSNotFound ? MTLTextureType2DArray : MTLTextureType2D;
}
// texture2d<uint, sample> -> UInt, <int ...> -> Int, <half ...> -> Half, anything else (float, depth, unknown) -> Float
static MTLDataType nvmtl_texdatatype_for_air_name(NSString *tn) {
    if (![tn isKindOfClass:[NSString class]]) return MTLDataTypeFloat;
    if ([tn rangeOfString:@"<uint"].location != NSNotFound || [tn rangeOfString:@"<ushort"].location != NSNotFound) return MTLDataTypeUInt;
    if ([tn rangeOfString:@"<int"].location != NSNotFound || [tn rangeOfString:@"<short"].location != NSNotFound) return MTLDataTypeInt;
    if ([tn rangeOfString:@"<half"].location != NSNotFound) return MTLDataTypeHalf;
    return MTLDataTypeFloat;
}
static BOOL nvmtl_binding_active(NSDictionary *b) { id acc = b[@"access"]; return !([acc isKindOfClass:[NSString class]] && [acc isEqualToString:@"Unused"]); }
static NVMTLArgument *nvmtl_build_buffer_argument(NVMTLFunction *fn, NSUInteger index, NSDictionary *binding, NSDictionary *refl, NSDictionary *names) {
    NVMTLArgument *a = [NVMTLBufferArgument new];
    a->_type = MTLBindingTypeBuffer; a->_index = index; a->_active = nvmtl_binding_active(binding); a->_arrayLength = 1; a->_bufAlign = 16;
    NSString *key = [NSString stringWithFormat:@"buffer:%lu", (unsigned long)index];
    a->_name = names[@"args"][key] ?: [NSString stringWithFormat:@"buffer%lu", (unsigned long)index];
    a->_access = nvmtl_access_for(binding[@"access"]);
    id ds = binding[@"declared_size"]; a->_bufSize = [ds isKindOfClass:[NSNumber class]] ? [ds unsignedLongValue] : 0;
    NSMutableArray *members = [NSMutableArray new];
    NSDictionary *memberNames = names[@"members"], *types = names[@"types"];
    NSString *bufKeyPrefix = [NSString stringWithFormat:@"%lu:", (unsigned long)index];
    NSMutableDictionary *kindAt = [NSMutableDictionary new];
    for (NSDictionary *b in refl[@"bindings"]) {
        NSDictionary *src = b[@"embedded_source"]; if (![src isKindOfClass:[NSDictionary class]]) continue;
        if ([src[@"buffer_index"] unsignedLongValue] != index) continue;
        kindAt[src[@"field_offset"]] = [b[@"kind"] isEqualToString:@"EmbeddedArgBufferSampler"] ? @"sampler" : @"texture";
    }
    NSUInteger end = 0;
    for (NSDictionary *f0 in refl[@"argument_buffer_fields"]) {
        if ([f0[@"buffer_index"] unsignedLongValue] != index) continue;
        NVMTLStructMember *m = [NVMTLStructMember new];
        m->_offset = [f0[@"field_offset"] unsignedLongValue]; m->_argIndex = [f0[@"argument_index"] unsignedLongValue];
        NSString *mk = [bufKeyPrefix stringByAppendingFormat:@"%lu", (unsigned long)m->_argIndex];
        m->_name = memberNames[mk] ?: [NSString stringWithFormat:@"field%lu", (unsigned long)m->_argIndex];
        NSString *tn = types[mk]; NSString *kind = kindAt[f0[@"field_offset"]];
        if ([kind isEqualToString:@"sampler"]) m->_dataType = MTLDataTypeSampler;
        else if ([kind isEqualToString:@"texture"]) m->_dataType = MTLDataTypeTexture;
        else if ([f0[@"resource_buffer_index"] isKindOfClass:[NSNumber class]]) m->_dataType = MTLDataTypePointer;
        else m->_dataType = tn ? nvmtl_datatype_for_air_name(tn) : MTLDataTypePointer;
        if (m->_dataType == MTLDataTypeTexture) { m->_texRef = [NVMTLTextureReferenceType new]; m->_texRef->_dataType = MTLDataTypeTexture; m->_texRef->_texDataType = MTLDataTypeFloat;
            m->_texRef->_texType = [tn rangeOfString:@"cube"].location != NSNotFound ? MTLTextureTypeCube : [tn rangeOfString:@"3d"].location != NSNotFound ? MTLTextureType3D
                                 : [tn rangeOfString:@"2d_array"].location != NSNotFound ? MTLTextureType2DArray : MTLTextureType2D;
            m->_texRef->_depth = [tn hasPrefix:@"depth"]; }
        if (m->_dataType == MTLDataTypePointer) { m->_ptr = [NVMTLPointerType new]; m->_ptr->_dataType = MTLDataTypePointer; m->_ptr->_elem = MTLDataTypeStruct; m->_ptr->_align = 16; }
        NSUInteger e = m->_offset + (m->_dataType == MTLDataTypeFloat4x4 ? 64 : m->_dataType == MTLDataTypeFloat3x3 ? 48 : 8);
        if (e > end) end = e;
        [members addObject:m];
    }
    NSArray *airPlain = [names[@"plain"] isKindOfClass:[NSDictionary class]] ? names[@"plain"][@(index)] : nil;
    if (!members.count && [airPlain isKindOfClass:[NSArray class]] && airPlain.count) {
        NSUInteger i = 0;
        for (NSDictionary *am in airPlain) {
            NVMTLStructMember *m = [NVMTLStructMember new];
            m->_offset = [am[@"off"] unsignedLongValue]; m->_argIndex = i++; m->_name = am[@"name"];
            m->_dataType = nvmtl_datatype_for_air_name(am[@"type"]);
            if (m->_dataType == MTLDataTypeNone) m->_dataType = MTLDataTypeStruct;
            // struct_type_info gives the element size and an element count (0 = not an array). An array is reported as
            // Apple does, MTLDataTypeArray with its MTLArrayType: engines size their uniform copies from it. WAS: the element
            // alone, so Hades (The Forge) copied one float4x4 of its float4x4[2] and every screen-space sprite read zeros.
            NSUInteger cnt = [am[@"count"] unsignedLongValue], sz = [am[@"size"] unsignedLongValue];
            if (cnt > 1) { NVMTLArrayType *at = [NVMTLArrayType new]; at->_dataType = MTLDataTypeArray; at->_elem = m->_dataType; at->_len = cnt; at->_stride = sz;
                m->_arr = at; m->_dataType = MTLDataTypeArray; }
            NSUInteger e = m->_offset + sz * (cnt > 1 ? cnt : 1); if (e > end) end = e;
            [members addObject:m];
        }
    }
    if (!members.count && [binding[@"type_layout"] isKindOfClass:[NSDictionary class]]) {
        NSArray *ms = binding[@"type_layout"][@"Struct"];
        NSUInteger i = 0;
        for (NSDictionary *am in ms) {
            if (![am isKindOfClass:[NSDictionary class]]) continue;
            NVMTLStructMember *m = [NVMTLStructMember new];
            m->_offset = [am[@"offset"] unsignedLongValue]; m->_argIndex = i;
            NSString *mk = [bufKeyPrefix stringByAppendingFormat:@"%lu", (unsigned long)i];
            m->_name = memberNames[mk] ?: [NSString stringWithFormat:@"m%lu", (unsigned long)i];
            NSString *tn = types[mk]; m->_dataType = tn ? nvmtl_datatype_for_air_name(tn) : MTLDataTypeFloat;
            [members addObject:m]; i++;
        }
    }
    if (members.count) {
        [members sortUsingComparator:^NSComparisonResult(NVMTLStructMember *x, NVMTLStructMember *y) { return x->_offset < y->_offset ? NSOrderedAscending : x->_offset > y->_offset ? NSOrderedDescending : NSOrderedSame; }];
        a->_bufDataType = MTLDataTypeStruct;
        a->_bufStruct = [NVMTLStructType new]; a->_bufStruct->_dataType = MTLDataTypeStruct; a->_bufStruct->_members = members;
        if (!a->_bufSize) a->_bufSize = end;
    } else {
        NSString *tn = names[@"types"][key];
        a->_bufDataType = tn ? nvmtl_datatype_for_air_name(tn) : MTLDataTypeStruct;
        if (a->_bufDataType == MTLDataTypeTexture || a->_bufDataType == MTLDataTypeSampler) a->_bufDataType = MTLDataTypeStruct;
    }
    a->_bufPtr = [NVMTLPointerType new]; a->_bufPtr->_dataType = MTLDataTypePointer; a->_bufPtr->_elem = a->_bufDataType; a->_bufPtr->_access = a->_access;
    a->_bufPtr->_align = 16; a->_bufPtr->_size = a->_bufSize; a->_bufPtr->_isArgBuf = kindAt.count > 0 || [refl[@"argument_buffer_fields"] count] > 0; a->_bufPtr->_elemStruct = a->_bufStruct;
    return a;
}
NSArray<NVMTLArgument *> *nvmtl_arguments_for_function(NVMTLFunction *fn)
{
    if (!fn) return @[];
    @synchronized (fn) {
        if (fn->_arguments) return fn->_arguments;
        NSDictionary *refl = nvmtl_function_reflection(fn);
        NSDictionary *names = nvmtl_air_names(fn) ?: @{ @"args": @{}, @"members": @{}, @"types": @{} };
        NSMutableArray *out = [NSMutableArray new];
        NSMutableSet *seenBuf = [NSMutableSet new];
        for (NSDictionary *b in refl[@"bindings"]) {
            if (![b isKindOfClass:[NSDictionary class]]) continue;
            if (![b[@"param_index"] isKindOfClass:[NSNumber class]]) continue;
            NSString *kind = b[@"kind"]; NSUInteger idx = [b[@"metal_index"] unsignedLongValue];
            if ([kind isEqualToString:@"Buffer"]) {
                if ([seenBuf containsObject:@(idx)]) continue;
                [seenBuf addObject:@(idx)];
                [out addObject:nvmtl_build_buffer_argument(fn, idx, b, refl, names)];
            } else if ([kind isEqualToString:@"Texture"] || [kind isEqualToString:@"TextureArray"] || [kind isEqualToString:@"StorageImage"]) {
                NVMTLArgument *a = [NVMTLTextureArgument new]; a->_type = MTLBindingTypeTexture; a->_index = idx; a->_active = nvmtl_binding_active(b);
                a->_name = names[@"args"][[NSString stringWithFormat:@"texture:%lu", (unsigned long)idx]] ?: [NSString stringWithFormat:@"texture%lu", (unsigned long)idx];
                a->_access = [kind isEqualToString:@"StorageImage"] ? MTLBindingAccessReadWrite : nvmtl_access_for(b[@"access"]);
                a->_texType = nvmtl_textype_for_shape(b[@"texture_shape"]);
                // The AIR type name says what MTLArgument does: depth2d/depthcube/... is a depth texture, <uint>/<int>/<half>
                // the component type. Apple's GL picks a texture unit's sampler type from these two: with isDepthTexture NO
                // a sampler2DShadow unit counts as Float and its shader-side compare swizzle (texInfo) stays all Zero, so
                // every shadow lookup returned 0 (Unity's screen-space shadows, Northgard's lit ground: black).
                NSString *ttn = names[@"types"][[NSString stringWithFormat:@"texture:%lu", (unsigned long)idx]];
                a->_depth = [ttn isKindOfClass:[NSString class]] && [ttn rangeOfString:@"depth"].location != NSNotFound;
                a->_texDataType = nvmtl_texdatatype_for_air_name(ttn);
                id loc = b[@"descriptor"]; a->_arrayLength = [loc isKindOfClass:[NSDictionary class]] && [loc[@"count"] unsignedLongValue] > 1 ? [loc[@"count"] unsignedLongValue] : 1;
                [out addObject:a];
            } else if ([kind isEqualToString:@"Sampler"]) {
                NVMTLArgument *a = [NVMTLArgument new]; a->_type = MTLBindingTypeSampler; a->_index = idx; a->_active = nvmtl_binding_active(b); a->_arrayLength = 1;
                a->_name = names[@"args"][[NSString stringWithFormat:@"sampler:%lu", (unsigned long)idx]] ?: [NSString stringWithFormat:@"sampler%lu", (unsigned long)idx];
                [out addObject:a];
            }
        }
        [out sortUsingComparator:^NSComparisonResult(NVMTLArgument *x, NVMTLArgument *y) {
            if (x->_type != y->_type) return x->_type < y->_type ? NSOrderedAscending : NSOrderedDescending;
            return x->_index < y->_index ? NSOrderedAscending : x->_index > y->_index ? NSOrderedDescending : NSOrderedSame; }];
        NSMutableString *s = [NSMutableString new];
        for (NVMTLArgument *a in out) [s appendFormat:@" %@(%lu)=%@%@", a->_type == MTLBindingTypeBuffer ? @"buffer" : a->_type == MTLBindingTypeTexture ? @"texture" : @"sampler",
                                       (unsigned long)a->_index, a->_name, a->_bufStruct ? [NSString stringWithFormat:@"[%lu members, %lu B]", (unsigned long)a->_bufStruct->_members.count, (unsigned long)a->_bufSize] : @""];
        nvlog("REFL %s (%s): %lu argument(s)%s", fn->_fname.UTF8String ?: "?", fn->_stage.UTF8String ?: "?", (unsigned long)out.count, s.UTF8String);
        fn->_arguments = out;
        return out;
    }
}
NVMTLArgument *nvmtl_argument_for_buffer(NVMTLFunction *fn, NSUInteger index)
{
    for (NVMTLArgument *a in nvmtl_arguments_for_function(fn)) if (a->_type == MTLBindingTypeBuffer && a->_index == index) return a;
    return nil;
}
static NSArray *nvmtl_stage_in_arguments(MTLRenderPipelineDescriptor *d)
{
    MTLVertexDescriptor *vd = d.vertexDescriptor; if (!vd || !d.vertexFunction) return @[];
    NSMutableSet *used = [NSMutableSet set];
    for (NSDictionary *va in nvmtl_function_reflection((NVMTLFunction *)d.vertexFunction)[@"vertex_attributes"])
        if ([va isKindOfClass:[NSDictionary class]] && [va[@"location"] isKindOfClass:[NSNumber class]]) [used addObject:va[@"location"]];
    NSMutableIndexSet *bufs = [NSMutableIndexSet indexSet], *live = [NSMutableIndexSet indexSet];
    for (NSUInteger i = 0; i < 31; i++) {
        MTLVertexAttributeDescriptor *at = vd.attributes[i]; if (at.format == MTLVertexFormatInvalid) continue;
        [bufs addIndex:at.bufferIndex]; if ([used containsObject:@(i)]) [live addIndex:at.bufferIndex];
    }
    NSMutableArray *out = [NSMutableArray array]; __block NSUInteger n = 0;
    [bufs enumerateIndexesUsingBlock:^(NSUInteger bi, BOOL *stop) {
        NVMTLStageInArgument *a = [NVMTLStageInArgument new];
        a->_type = MTLBindingTypeBuffer; a->_index = bi; a->_active = [live containsIndex:bi]; a->_arrayLength = 1; a->_bufAlign = 4;
        a->_access = MTLBindingAccessReadOnly; a->_bufSize = vd.layouts[bi].stride; a->_bufDataType = MTLDataTypeStruct;
        a->_name = [NSString stringWithFormat:@"vertexBuffer.%lu", (unsigned long)n++];
        [out addObject:a];
    }];
    return out;
}
id nvmtl_render_reflection(MTLRenderPipelineDescriptor *d)
{
    NVMTLRenderPipelineReflection *r = [NVMTLRenderPipelineReflection new];
    r->_v = [nvmtl_stage_in_arguments(d) arrayByAddingObjectsFromArray:nvmtl_arguments_for_function((NVMTLFunction *)d.vertexFunction)];
    r->_f = nvmtl_arguments_for_function((NVMTLFunction *)d.fragmentFunction);
    return r;
}
void nvmtl_gl_tag_compute_reflection(id r, id fn);
id nvmtl_compute_reflection(id<MTLFunction> fn)
{
    NVMTLComputePipelineReflection *r = [NVMTLComputePipelineReflection new];
    r->_a = nvmtl_arguments_for_function((NVMTLFunction *)fn);
    nvmtl_gl_tag_compute_reflection(r, fn);
    return r;
}

NSDictionary *nvmtl_index_argument_fields(NSArray *fields) {
    NSMutableDictionary *index = [NSMutableDictionary new];
    for (NSDictionary *field in fields) {
        NSNumber *key = @([field[@"argument_index"] unsignedLongValue]);
        if (!index[key]) index[key] = field;
    }
    return [index copy];
}
@implementation NVMTLArgumentEncoder
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSUInteger)encodedLength {
    NSUInteger end = 0;
    for (NSDictionary *f in _fields) {
        NSUInteger e = [f[@"field_offset"] unsignedLongValue] + 8;
        if (e > end) end = e;
    }
    return MAX(end, _airSize) ?: 8;
}
- (NSUInteger)alignment { return _airAlign ?: 8; }
- (void)setArgumentBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off { _dst = (NVMTLBuffer *)b; _dstOffset = off; nvtrace("AE argbuf %p off %lu (%lu fields)", (__bridge void *)b, (unsigned long)off, (unsigned long)_fields.count); }
// The encoder of the argument buffer that the pointer member [[id(index)]] points at. Metal answers nil for a member that is not
// such a pointer (measured on an Apple GPU: tools/argbuf_oracle/nested_oracle.m, outer 16/8, nested 16/8, member 1 nil).
- (id<MTLArgumentEncoder>)newArgumentEncoderForBufferAtIndex:(NSUInteger)index {
    NSDictionary *f = [self nvmtlFieldAt:index];
    NSArray *nested = f[@"nested_fields"];
    if (![nested isKindOfClass:[NSArray class]]) { nvlog("newArgumentEncoderForBufferAtIndex:%lu - member %lu is not a pointer to an argument buffer - nil", (unsigned long)index, (unsigned long)index); return nil; }
    NVMTLArgumentEncoder *e = [NVMTLArgumentEncoder new];
    e->_fields = nested; e->_fieldByIndex = nvmtl_index_argument_fields(nested);
    e->_airSize = [f[@"nested_size"] unsignedLongValue]; e->_airAlign = [f[@"nested_align"] unsignedLongValue];
    nvlog("newArgumentEncoderForBufferAtIndex:%lu -> %lu field(s), %lu bytes", (unsigned long)index, (unsigned long)nested.count, (unsigned long)e->_airSize);
    return e;
}
- (void)setArgumentBuffer:(id<MTLBuffer>)b startOffset:(NSUInteger)off arrayElement:(NSUInteger)i {
    [self setArgumentBuffer:b offset:off + i * [self encodedLength]];
}
- (void)dealloc { free(_offAt); _offAt = NULL; }
- (NSDictionary *)nvmtlFieldAt:(NSUInteger)index {
    return _fieldByIndex[@(index)];
}
- (void)setBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)index {
    NVMTLBuffer *nb = (NVMTLBuffer *)b;
    if (!_dst || !nvmtl_buf_cpu(_dst)) { nvlog("argument encoder: no argument buffer set (or it is not host-visible)"); return; }
    NSDictionary *f = [self nvmtlFieldAt:index];
    if (!f) { nvlog("argument encoder: this function has no [[id(%lu)]] field — REFUSED", (unsigned long)index); return; }
    // The binding path checks the offset against the buffer length (nvmtl_vk_bind_buffer); this path
    // computes an address the shader will dereference, so it has to check the same thing.
    if (nb && off > nb->_b.size) {
        nvlog("argument encoder: offset %lu is past the %zu-byte buffer at [[id(%lu)]] - REFUSED",
              (unsigned long)off, nb->_b.size, (unsigned long)index);
        return;
    }
    uint64_t addr = nb ? nvmtl_vk_buffer_address(&nb->_b) + off : 0;
    const NSUInteger fieldOffset = [f[@"field_offset"] unsignedLongValue];
    const size_t size = _dst->_b.size;
    if (_dstOffset > size || fieldOffset > size - _dstOffset || 8 > size - _dstOffset - fieldOffset) {
        nvlog("argument encoder: field offset %lu+%lu does not fit %zu bytes — REFUSED", (unsigned long)_dstOffset, (unsigned long)fieldOffset, size); return;
    }
    NSUInteger at = _dstOffset + fieldOffset;
    if (at + 8 > _dst->_b.size) { nvlog("argument encoder: field at %lu is past the argument buffer (%zu bytes)", (unsigned long)at, _dst->_b.size); return; }
    memcpy((uint8_t *)nvmtl_buf_cpu(_dst) + at, &addr, 8);
}
- (void)setAccelerationStructure:(id<MTLAccelerationStructure>)as atIndex:(NSUInteger)index { [self setBuffer:(id<MTLBuffer>)as offset:0 atIndex:index]; }
- (void)setBuffers:(const id<MTLBuffer> __unsafe_unretained [])bs offsets:(const NSUInteger *)offs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setBuffer:bs[k] offset:offs ? offs[k] : 0 atIndex:r.location + k];
}
- (void)setTexture:(id<MTLTexture>)t atIndex:(NSUInteger)index {
    if (!_dst || !nvmtl_buf_cpu(_dst)) { nvlog("argument encoder: setTexture: no argument buffer set (or it is not host-visible)"); return; }
    NSDictionary *f = [self nvmtlFieldAt:index];
    if (!f) { nvlog("argument encoder: setTexture:atIndex:%lu — this function has no [[id(%lu)]] field — REFUSED", (unsigned long)index, (unsigned long)index); return; }
    uint64_t handle = t ? [t gpuResourceID]._impl : 0;
    const NSUInteger fieldOffset = [f[@"field_offset"] unsignedLongValue];
    const size_t size = _dst->_b.size;
    if (_dstOffset > size || fieldOffset > size - _dstOffset || 8 > size - _dstOffset - fieldOffset) {
        nvlog("argument encoder: field offset %lu+%lu does not fit %zu bytes — REFUSED", (unsigned long)_dstOffset, (unsigned long)fieldOffset, size); return;
    }
    NSUInteger at = _dstOffset + fieldOffset;
    if (at + 8 > _dst->_b.size) { nvlog("argument encoder: texture field at %lu is past the argument buffer (%zu bytes)", (unsigned long)at, _dst->_b.size); return; }
    memcpy((uint8_t *)nvmtl_buf_cpu(_dst) + at, &handle, 8);
    nvtrace("AE tex id %lu -> %p+%lu handle 0x%llx", (unsigned long)index, (__bridge void *)_dst, (unsigned long)at, (unsigned long long)handle);
}
- (void)setTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) [self setTexture:t ? t[k] : nil atIndex:r.location + k]; }
- (void)setSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)index {
    if (!_dst || !nvmtl_buf_cpu(_dst)) { nvlog("argument encoder: setSamplerState: no argument buffer set (or it is not host-visible)"); return; }
    NSDictionary *f = [self nvmtlFieldAt:index];
    if (!f) { nvlog("argument encoder: setSamplerState:atIndex:%lu — this function has no [[id(%lu)]] field — REFUSED", (unsigned long)index, (unsigned long)index); return; }
    uint64_t handle = s ? [s gpuResourceID]._impl : 0;
    const NSUInteger fieldOffset = [f[@"field_offset"] unsignedLongValue];
    const size_t size = _dst->_b.size;
    if (_dstOffset > size || fieldOffset > size - _dstOffset || 8 > size - _dstOffset - fieldOffset) {
        nvlog("argument encoder: field offset %lu+%lu does not fit %zu bytes — REFUSED", (unsigned long)_dstOffset, (unsigned long)fieldOffset, size); return;
    }
    NSUInteger at = _dstOffset + fieldOffset;
    if (at + 8 > _dst->_b.size) { nvlog("argument encoder: sampler field at %lu is past the argument buffer (%zu bytes)", (unsigned long)at, _dst->_b.size); return; }
    memcpy((uint8_t *)nvmtl_buf_cpu(_dst) + at, &handle, 8);
    nvtrace("AE samp id %lu -> %p+%lu handle 0x%llx", (unsigned long)index, (__bridge void *)_dst, (unsigned long)at, (unsigned long long)handle);
}
- (void)setSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) [self setSamplerState:s ? s[k] : nil atIndex:r.location + k]; }
- (void)nvmtlPutHandle:(uint64_t)h atIndex:(NSUInteger)index what:(const char *)what {
    if (!_dst || !nvmtl_buf_cpu(_dst)) { nvlog("argument encoder: set%s: no argument buffer set (or it is not host-visible)", what); return; }
    NSDictionary *f = [self nvmtlFieldAt:index];
    if (!f) { nvlog("argument encoder: set%s:atIndex:%lu — this function has no [[id(%lu)]] field — REFUSED", what, (unsigned long)index, (unsigned long)index); return; }
    const size_t size = _dst->_b.size; const NSUInteger fo = [f[@"field_offset"] unsignedLongValue];
    if (_dstOffset > size || fo > size - _dstOffset || 8 > size - _dstOffset - fo) {
        nvlog("argument encoder: %s field at %lu+%lu is past the argument buffer (%zu bytes)", what, (unsigned long)_dstOffset, (unsigned long)fo, size); return; }
    memcpy((uint8_t *)nvmtl_buf_cpu(_dst) + _dstOffset + fo, &h, 8);
    nvtrace("AE %s id %lu -> %p+%lu handle 0x%llx", what, (unsigned long)index, (__bridge void *)_dst, (unsigned long)(_dstOffset + fo), (unsigned long long)h);
}
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)p atIndex:(NSUInteger)i { [self nvmtlPutHandle:p ? p.gpuResourceID._impl : 0 atIndex:i what:"RenderPipelineState"]; }
- (void)setRenderPipelineStates:(const id<MTLRenderPipelineState> __unsafe_unretained [])p withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setRenderPipelineState:p ? p[k] : nil atIndex:r.location + k]; }
- (void)setComputePipelineState:(id<MTLComputePipelineState>)p atIndex:(NSUInteger)i { [self nvmtlPutHandle:p ? p.gpuResourceID._impl : 0 atIndex:i what:"ComputePipelineState"]; }
- (void)setComputePipelineStates:(const id<MTLComputePipelineState> __unsafe_unretained [])p withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setComputePipelineState:p ? p[k] : nil atIndex:r.location + k]; }
- (void)setIndirectCommandBuffer:(id<MTLIndirectCommandBuffer>)icb atIndex:(NSUInteger)index {
    { static int said; if (icb && !said++) nvlog("argument encoder: setIndirectCommandBuffer: writes the ICB's gpuResourceID; GPU-side ICB "
                                             "execution and encoding stay UNSUPPORTED (this ICB is a CPU replay list) (said once)"); }
    [self nvmtlPutHandle:icb ? icb.gpuResourceID._impl : 0 atIndex:index what:"IndirectCommandBuffer"];
}
- (void)setIndirectCommandBuffers:(const id<MTLIndirectCommandBuffer> __unsafe_unretained [])b withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setIndirectCommandBuffer:b ? b[k] : nil atIndex:r.location + k]; }
- (void *)constantDataAtIndex:(NSUInteger)index {
    if (!_offAt) {
        NSUInteger mx = 0; for (NSDictionary *fd in _fields) { NSUInteger a = [fd[@"argument_index"] unsignedLongValue]; if (a > mx) mx = a; }
        int64_t *o = (int64_t *)malloc((mx + 1) * sizeof *o);
        if (o) { for (NSUInteger k = 0; k <= mx; k++) o[k] = -1;
                 for (NSDictionary *fd in _fields) { NSUInteger a = [fd[@"argument_index"] unsignedLongValue]; if (o[a] < 0) o[a] = (int64_t)[fd[@"field_offset"] unsignedLongValue]; }
                 _offN = mx + 1; _offAt = o; }
    }
    if (!_offAt || index >= _offN || _offAt[index] < 0 || !_dst || !nvmtl_buf_cpu(_dst)) return NULL;
    const NSUInteger fieldOffset = (NSUInteger)_offAt[index];
    const size_t size = _dst->_b.size;
    if (_dstOffset >= size || fieldOffset >= size - _dstOffset) return NULL;
    return (uint8_t *)nvmtl_buf_cpu(_dst) + _dstOffset + fieldOffset;
}
@end

extern NSData *nvmtl_compile_library_entry(NSData *, NSString *, NSString *, NSString *, NSString **);
@implementation NVMTLLibrary
- (MTLLibraryType)type { return MTLLibraryTypeExecutable; }
- (NSString *)installName { return nil; }
- (NSData *)nvmtlEntrySpirv:(NSString *)name error:(NSError **)error {
    @synchronized(self) {
        NSData *cached = _compiledFns[name];
        if (cached) return cached;
    }

    NSData *air = _fns[name];
    NSString *ll = _airs[name], *stage = _stages[name];
    // The library index hands every library made from the same file the same bitcode objects, so the entry's SPIR-V is
    // kept on its bitcode: a library made again (VideoToolbox makes one per transfer) neither hashes nor reads it again.
    NSData *known = air ? objc_getAssociatedObject(air, &gNVMTLEntrySpvAssoc) : nil;
    NSString *why = nil;
    NSData *spv = known ?: nvmtl_compile_library_entry(air, ll, stage, name, &why);
    if (!spv) {
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:3
                                           userInfo:@{NSLocalizedDescriptionKey:why ?: @"entry translation failed"}];
        return nil;
    }
    if (!known && air) objc_setAssociatedObject(air, &gNVMTLEntrySpvAssoc, spv, OBJC_ASSOCIATION_RETAIN);

    @synchronized(self) {
        NSData *won = _compiledFns[name];
        if (won) return won;
        if (!_compiledFns) _compiledFns = [NSMutableDictionary new];
        _compiledFns[name] = spv;
    }
    return spv;
}
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
// NVMTL_AIR_DUMP=<dir>: write each function's AIR text, once, as <dir>/<name>.ll when an app first asks for it, so an
// app that builds its libraries from memory can be studied offline kernel by kernel. Nothing is written when unset.
static void nvmtl_air_dump(NSString *name, NSString *air)
{
    static const char *dir; static dispatch_once_t once;
    dispatch_once(&once, ^{ dir = getenv("NVMTL_AIR_DUMP"); if (dir && !*dir) dir = NULL; });
    if (!dir || !air.length || !name.length || [name containsString:@"/"]) return;
    NSString *path = [[@(dir) stringByAppendingPathComponent:name] stringByAppendingPathExtension:@"ll"];
    if ([[NSFileManager defaultManager] fileExistsAtPath:path]) return;
    [[NSFileManager defaultManager] createDirectoryAtPath:@(dir) withIntermediateDirectories:YES attributes:nil error:nil];
    [air writeToFile:path atomically:YES encoding:NSUTF8StringEncoding error:nil];
}
- (id<MTLFunction>)newFunctionWithName:(NSString *)name {
    NSData *spv = [_fns objectForKey:name];
    if (!spv && !_airs[name]) { nvlog("library: no function \"%s\" (this library holds %lu, externs %lu)", name.UTF8String, (unsigned long)(_fns.count + _airs.count), (unsigned long)_externs.count); return nil; }
    nvmtl_air_dump(name, _airs[name]);
    if (spv && ([_stages[name] isEqualToString:@"mesh"] || [_stages[name] isEqualToString:@"object"])) {
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = _stages[name];
        f->_air = _airs[name]; f->_lib = self; return f;
    }
    const uint32_t af = spv ? nvmtl_air_text_flags(_airs[name]) : 0;
    if (spv && (af & NVMTL_AIRF_DYLD_TABLE)) {
        nvlog("library: \"%s\" was compiled against a dynamic library - DEFERRED to pipeline creation", name.UTF8String);
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"");
        f->_air = _airs[name]; f->_lib = self; f->_needsLink = YES; return f;
    }
    if (spv && (af & NVMTL_AIRF_ISECT_TABLE)) {
        nvlog("library: \"%s\" takes an intersection function table - DEFERRED to pipeline creation", name.UTF8String);
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"");
        f->_air = _airs[name]; f->_lib = self; f->_needsLink = YES; return f;
    }
    if (spv) { NSError *te = nil; NSData *es = [self nvmtlEntrySpirv:name error:&te];
        if (!es && _airs[name] && nvmtl_needs_visible_link(_airs[name], te.localizedDescription)) {
            nvlog("library: \"%s\" calls a visible function (by reference or table) - DEFERRED to pipeline creation", name.UTF8String);
            NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"");
            f->_air = _airs[name]; f->_lib = self; f->_needsLink = YES; return f;
        }
        if (!es && _airs[name] && ([te.localizedDescription rangeOfString:@"`air.constant`"].location != NSNotFound || [name hasPrefix:@"__entry_"])) {
            nvlog("library: \"%s\" has a by-value (air.constant) parameter - DEFERRED to newFunctionWithPluginData (OpenCL via AMOGL)", name.UTF8String);
            NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"kernel");
            f->_air = _airs[name]; f->_lib = self; return f;
        }
        spv = es; if (!spv) return nil; }
    NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = spv; f->_fname = name; f->_stage = (_stages[name] ?: @""); f->_air = _airs[name]; f->_lib = self; return f;
}
static size_t nvmtl_fc_width(NSUInteger dataType)
{
    const struct { NSUInteger base; size_t bytes; } families[] = {
        {MTLDataTypeFloat,4},{MTLDataTypeHalf,2},{MTLDataTypeInt,4},{MTLDataTypeUInt,4},
        {MTLDataTypeShort,2},{MTLDataTypeUShort,2},{MTLDataTypeChar,1},{MTLDataTypeUChar,1},
        {MTLDataTypeBool,1},{MTLDataTypeLong,8},{MTLDataTypeULong,8}};
    for(size_t i=0;i<sizeof(families)/sizeof(families[0]);i++)
        if(dataType>=families[i].base && dataType<=families[i].base+3)
            return families[i].bytes*(dataType-families[i].base+1);
    switch (dataType) {
        case MTLDataTypeBool:  case MTLDataTypeChar:  case MTLDataTypeUChar:  return 1;
        case MTLDataTypeShort: case MTLDataTypeUShort: case MTLDataTypeHalf:  return 2;
        case MTLDataTypeInt:   case MTLDataTypeUInt:  case MTLDataTypeFloat:  return 4;
        case MTLDataTypeLong:  case MTLDataTypeULong:                          return 8;
        default: return 0;
    }
}
- (id<MTLFunction>)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)cv error:(NSError **)error {
    if (error) *error = nil;
    NSData *spv = [_fns objectForKey:name];
    nvmtl_air_dump(name, _airs[name]);
    if (!spv && _airs[name]) {
        if (cv) nvlog("library: visible function \"%s\" asked with constant values — carried on the function",
                      name.UTF8String);
        id<MTLFunction> vf = [self newFunctionWithName:name];
        if (cv && [(id)vf isKindOfClass:[NVMTLFunction class]]) ((NVMTLFunction *)vf)->_fcv = cv;
        return vf;
    }
    if (!spv) { nvlog("library: no function \"%s\" (this library holds %lu, externs %lu)", name.UTF8String,
                      (unsigned long)(_fns.count + _airs.count), (unsigned long)_externs.count);
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:1 userInfo:@{NSLocalizedDescriptionKey: @"no such function"}];
        return nil; }
    NSArray *vals = nil;
    SEL sel = NSSelectorFromString(@"newIndexedConstantArray");
    if (cv && [cv respondsToSelector:sel]) vals = ((id (*)(id, SEL))objc_msgSend)(cv, sel);
    SEL namedSel=NSSelectorFromString(@"newNamedConstantArray");
    NSArray *named=(cv && [cv respondsToSelector:namedSel]) ? ((id(*)(id,SEL))objc_msgSend)(cv,namedSel) : nil;
    if(named.count) {
        NSString *air=_airs[name];
        if(!air){if(error)*error=[NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey:@"named constants require retained AIR"}];return nil;}
        NSRegularExpression *re=[NSRegularExpression regularExpressionWithPattern:@"^![0-9]+ = !\\{[^\\n]*\\.MTL_FC_INIT_[^\\n]*, !\"[^\"]+\", !\"([^\"]+)\", i32 ([0-9]+), i1 (?:true|false)\\}$" options:NSRegularExpressionAnchorsMatchLines error:NULL];
        NSMutableDictionary *indices=[NSMutableDictionary new];
        for(NSTextCheckingResult *m in [re matchesInString:air options:0 range:NSMakeRange(0,air.length)])
            indices[[air substringWithRange:[m rangeAtIndex:1]]]=@([[air substringWithRange:[m rangeAtIndex:2]] longLongValue]);
        MTLFunctionConstantValues *merged=[MTLFunctionConstantValues new];
        NSUInteger applied=0;
        for(id v in named) {
            Ivar ni=class_getInstanceVariable([v class],"_name"),ti=class_getInstanceVariable([v class],"_dataType"),di=class_getInstanceVariable([v class],"_data");
            if(!ni || !ti || !di){if(error)*error=[NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey:@"unrecognized named-constant representation"}];return nil;}
            NSString *key=object_getIvar(v,ni);NSNumber *index=indices[key];
            if(!index)continue;
            const uint8_t *base=(const uint8_t *)(__bridge void *)v;
            NSUInteger type=*(const NSUInteger *)(base+ivar_getOffset(ti));
            const void *data=*(const void *const *)(base+ivar_getOffset(di));
            if(!data || !nvmtl_fc_width(type)){if(error)*error=[NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey:@"unsupported named-constant type"}];return nil;}
            [merged setConstantValue:data type:type atIndex:index.unsignedIntegerValue];applied++;
        }
        for(id v in vals) {
            Ivar ii=class_getInstanceVariable([v class],"_index"),ti=class_getInstanceVariable([v class],"_dataType"),di=class_getInstanceVariable([v class],"_data");
            if(!ii || !ti || !di){if(error)*error=[NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey:@"unrecognized indexed-constant representation"}];return nil;}
            const uint8_t *base=(const uint8_t *)(__bridge void *)v;
            [merged setConstantValue:*(const void *const *)(base+ivar_getOffset(di)) type:*(const NSUInteger *)(base+ivar_getOffset(ti)) atIndex:*(const NSUInteger *)(base+ivar_getOffset(ii))];
        }
        vals=((id(*)(id,SEL))objc_msgSend)(merged,sel);
        nvlog("named constants: %s applied %lu of %lu, merged %lu",name.UTF8String,(unsigned long)applied,(unsigned long)named.count,(unsigned long)vals.count);
    }
    if (vals.count == 0 && ([_stages[name] isEqualToString:@"mesh"] || [_stages[name] isEqualToString:@"object"]) && _airs[name]) {
        nvlog("newFunctionWithName:constantValues: \"%s\" is a %s function - %zu constant(s) DEFERRED to the mesh pipeline",
              name.UTF8String, [_stages[name] UTF8String], (size_t)0);
        if (error) *error = nil;
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fc = nil; f->_fname = name; f->_stage = _stages[name];
        f->_air = _airs[name]; f->_lib = self; f->_fcv = cv; return f;
    }
    if (vals.count == 0) {
        spv = [self nvmtlEntrySpirv:name error:error]; if (!spv) return nil;
        NVMTLFunction *f0 = [NVMTLFunction new];
        f0->_spirv = spv; f0->_fname = name; f0->_stage = (_stages[name] ?: @""); f0->_air = _airs[name]; f0->_lib = self; f0->_fcv = cv;
        nvlog("newFunctionWithName:constantValues: \"%s\" - no constants, unspecialized module (%lu B)",
              name.UTF8String, (unsigned long)spv.length);
        return f0;
    }
    NSUInteger capacity = vals.count;
        __attribute__((objc_precise_lifetime)) NSMutableData *packed = nvmtl_fc_storage(capacity);
        uint32_t *idx = packed.mutableBytes, *sz = idx ? idx + capacity : NULL;
        const uint8_t **pay = sz ? (const uint8_t **)(sz + capacity) : NULL;
        (void)pay; size_t n = 0;
    if (!packed) { if(error)*error=[NSError errorWithDomain:@"NVMTL" code:4 userInfo:@{NSLocalizedDescriptionKey:@"function constant storage allocation failed"}]; return nil; }
    for (id v in vals) {
        Ivar ii = class_getInstanceVariable([v class], "_index");
        Ivar it = class_getInstanceVariable([v class], "_dataType");
        Ivar id_ = class_getInstanceVariable([v class], "_data");
        if (!ii || !it || !id_) { nvlog("function constants: MTLIndexedConstantValue has no _index/_dataType/_data"); vals = nil; break; }
        const uint8_t *base = (const uint8_t *)(__bridge void *)v;
        NSUInteger index = *(const NSUInteger *)(base + ivar_getOffset(ii));
        NSUInteger dtype = *(const NSUInteger *)(base + ivar_getOffset(it));
        const uint8_t *data = *(const uint8_t *const *)(base + ivar_getOffset(id_));
        size_t w = nvmtl_fc_width(dtype);
        if (!w || !data) { nvlog("function constants: index %lu has data type %lu, which this driver does not encode", (unsigned long)index, (unsigned long)dtype);
            if (error) *error = [NSError errorWithDomain:@"NVMTL" code:2 userInfo:@{NSLocalizedDescriptionKey: [NSString stringWithFormat:@"function constant %lu: unsupported data type %lu", (unsigned long)index, (unsigned long)dtype]}];
            return nil; }
        idx[n] = (uint32_t)index; sz[n] = (uint32_t)w; pay[n] = data; n++;
    }
    NSString *e = nil;
    NSMutableDictionary *constants=[NSMutableDictionary new];
    for(size_t k=0;k<n;k++){NSMutableArray *bytes=[NSMutableArray new];for(uint32_t j=0;j<sz[k];j++)[bytes addObject:@(pay[k][j])];constants[[@(idx[k]) stringValue]]=bytes;}
    if (([_stages[name] isEqualToString:@"mesh"] || [_stages[name] isEqualToString:@"object"]) && _airs[name]) {
        nvlog("newFunctionWithName:constantValues: \"%s\" is a %s function - %zu constant(s) DEFERRED to the mesh pipeline",
              name.UTF8String, [_stages[name] UTF8String], n);
        if (error) *error = nil;
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fc = [constants copy]; f->_fname = name; f->_stage = _stages[name];
        f->_air = _airs[name]; f->_lib = self; f->_fcv = cv; return f;
    }
    // A function that names visible functions or a function table is only translated once its pipeline links them
    // (nvmtl_link_stage). Translating it here can only fail, and the failure is not cached: MPS's depthwise
    // weight-gradient kernels paid seconds of failed translation on the encoding thread in every Geekbench run.
    // The reference query is cheap, so defer at once. NVMTL_EAGER_LINK_PROBE=1 restores the translate-first order.
    static int eagerProbe = -1;
    if (eagerProbe < 0) { const char *ep = getenv("NVMTL_EAGER_LINK_PROBE"); eagerProbe = ep && ep[0] == '1'; }
    const BOOL linkFirst = !eagerProbe && _airs[name] && nvmtl_needs_visible_link(_airs[name], nil);
    NSData *spec = nil;
    if (!linkFirst) {
        nvmtl_fc_expect_link(1);
        spec = nvmtl_translate_with_fc_cached(_airs[name], _stages[name], name, constants, idx, sz, pay, n, &e);
        nvmtl_fc_expect_link(0);
    }
    if (!spec && _airs[name] && (linkFirst || nvmtl_needs_visible_link(_airs[name], e))) {
        nvlog("newFunctionWithName:constantValues: \"%s\" has visible-function references - DEFERRED to pipeline creation (%zu constant(s))", name.UTF8String, n);
        if (error) *error = nil;
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fc = [constants copy]; f->_fname = name; f->_stage = (_stages[name] ?: @"");
        f->_air = _airs[name]; f->_lib = self; f->_fcv = cv; f->_needsLink = YES; return f;
    }
    if (!spec) {
        nvlog("newFunctionWithName:constantValues: \"%s\" — %s", name.UTF8String, e.UTF8String ?: "specialization failed");
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:3 userInfo:@{NSLocalizedDescriptionKey: e ?: @"specialization failed"}];
        return nil;
    }
    nvlog("newFunctionWithName:constantValues: \"%s\" specialized with %zu constant(s)", name.UTF8String, n);
    NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = spec; f->_fc = [constants copy]; f->_fname = name; f->_stage = (_stages[name] ?: @""); f->_air = _airs[name]; f->_lib = self; f->_fcv = cv; return f;
}
- (void)newFunctionWithName:(NSString *)name constantValues:(MTLFunctionConstantValues *)cv
          completionHandler:(void (^)(id<MTLFunction>, NSError *))handler {
    if (!handler) return;
    NSError *err = nil;
    id<MTLFunction> f = [self newFunctionWithName:name constantValues:cv error:&err];
    handler(f, err);
}
- (id<MTLFunction>)newFunctionWithDescriptor:(id)desc error:(NSError **)error {
    if (error) *error = nil;
    if (!desc) {
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:1
                             userInfo:@{NSLocalizedDescriptionKey: @"newFunctionWithDescriptor: nil descriptor"}];
        nvlog("newFunctionWithDescriptor: nil descriptor");
        return nil;
    }
    NSString *name = [desc respondsToSelector:@selector(name)] ? [desc name] : nil;
    NSString *spec = [desc respondsToSelector:@selector(specializedName)] ? [desc specializedName] : nil;
    id cv = [desc respondsToSelector:@selector(constantValues)] ? [desc constantValues] : nil;
    if (![name isKindOfClass:[NSString class]] || !name.length) {
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:2
                             userInfo:@{NSLocalizedDescriptionKey: @"newFunctionWithDescriptor: descriptor has no name"}];
        nvlog("newFunctionWithDescriptor: %s has no usable name", object_getClassName(desc));
        return nil;
    }
    nvlog("newFunctionWithDescriptor: \"%s\"%s%s%s", name.UTF8String,
          spec.length ? " specializedName \"" : "", spec.length ? spec.UTF8String : "", spec.length ? "\"" : "");
    id<MTLFunction> f = cv ? [self newFunctionWithName:name constantValues:(MTLFunctionConstantValues *)cv error:error]
                           : [self newFunctionWithName:name];
    if (spec.length && [(id)f isKindOfClass:[NVMTLFunction class]]) ((NVMTLFunction *)f)->_specName = [spec copy];
    if (cv) return f;
    if (!f && error)
        *error = [NSError errorWithDomain:@"NVMTL" code:3
                  userInfo:@{NSLocalizedDescriptionKey:
                             [NSString stringWithFormat:@"this library has no function named %@", name]}];
    return f;
}
- (void)newFunctionWithDescriptor:(id)desc completionHandler:(void (^)(id<MTLFunction>, NSError *))handler {
    if (!handler) return;
    NSError *err = nil;
    id<MTLFunction> f = [self newFunctionWithDescriptor:desc error:&err];
    handler(f, err);
}
- (NSArray<NSString *> *)externFunctionNames {
    NSSet<NSString *> *x = nvmtl_mtlb_extern_names(_raw);
    return x.count ? x.allObjects : (_externs ?: @[]);
}

- (NSArray<NSString *> *)functionNames {
    NSMutableSet<NSString *> *s = [NSMutableSet setWithArray:_fns.allKeys];
    [s addObjectsFromArray:_airs.allKeys];
    [s minusSet:nvmtl_mtlb_extern_names(_raw)];
    return s.allObjects;
}

- (id)newExternFunctionWithName:(NSString *)name {
    if (![nvmtl_mtlb_extern_names(_raw) containsObject:name]) {
        nvlog("newExternFunctionWithName: \"%s\" is not an extern function of this library (%lu are)", name.UTF8String,
              (unsigned long)nvmtl_mtlb_extern_names(_raw).count);
        return nil;
    }
    id twin;
    @synchronized (self) { twin = [(id)gNVMTLMainDevice nvmtlAppleTwinOf:self]; }
    id f = [twin respondsToSelector:@selector(newExternFunctionWithName:)] ? [twin newExternFunctionWithName:name] : nil;
    if (!f) nvlog("newExternFunctionWithName: \"%s\" - the Apple twin %s it; answering nil (CI reports the kernel missing)",
                  name.UTF8String, twin ? "refused" : "could not be built for");
    return f;
}
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
@end

#define NVMTL_RESID_PIPE (1ull << 61)
static uint64_t nvmtl_resid_pipe(id o, uint64_t *slot) {
    static uint64_t seq;
    @synchronized (o) { if (!*slot) *slot = __atomic_add_fetch(&seq, 1, __ATOMIC_RELAXED) | NVMTL_RESID_PIPE; }
    return *slot;
}

#pragma mark - pipeline state
@implementation NVMTLRenderPipelineState
@dynamic allocatedSize, maxTotalThreadgroupsPerMeshGrid, maxTotalThreadsPerMeshThreadgroup, maxTotalThreadsPerObjectThreadgroup, meshThreadExecutionWidth, objectThreadExecutionWidth, reflection;
- (void)dealloc { nvmtl_vk_pipeline_destroy(&_p); }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSUInteger)maxTotalThreadsPerThreadgroup { return 1024; }
- (NSUInteger)threadExecutionWidth { return 32; }
- (BOOL)threadgroupSizeMatchesTileSize { return NO; }
- (MTLSize)requiredThreadsPerTileThreadgroup { return MTLSizeMake(0, 0, 0); }   // no tile pipelines here
- (MTLSize)requiredThreadsPerObjectThreadgroup { return _reqObj; }
- (MTLSize)requiredThreadsPerMeshThreadgroup { return _reqMesh; }
- (NSString *)label { return _descriptor.label; }
- (BOOL)supportIndirectCommandBuffers { return _descriptor.supportIndirectCommandBuffers; }
- (MTLResourceID)gpuResourceID {
    MTLResourceID r; r._impl = _descriptor.supportIndirectCommandBuffers ? nvmtl_resid_pipe(self, &_resid) : 0; return r; }
- (NSUInteger)imageblockSampleLength { return 0; }
- (NSUInteger)imageblockMemoryLengthForDimensions:(MTLSize)d { return 0; }
- (MTLShaderValidation)shaderValidation { return MTLShaderValidationDisabled; }
// render-stage function pointers and visible function tables (formerly the B41FunctionPointers category in NVMTLLibraryLoad.m)
- (id<MTLFunctionHandle>)functionHandleWithFunction:(id<MTLFunction>)f stage:(MTLRenderStages)st {
    const void *key = (st & MTLRenderStageFragment) ? &kNVMTLRenderFrag : (st & MTLRenderStageVertex) ? &kNVMTLRenderVert : NULL;
    id lf = key ? objc_getAssociatedObject(self, key) : nil;
    NSArray *c = lf ? objc_getAssociatedObject(lf, &kNVMTLTableFns) : nil;
    for (NSUInteger k = 0; k < c.count; k++) {
        NVMTLFunction *cf = c[k];
        if (cf == (id)f || [cf.name isEqualToString:f.name]) {
            NVMTLFunctionHandle *h = [NVMTLFunctionHandle new]; h->_id = k + 1; h->_fn = f; return h;
        }
    }
    nvlog("render functionHandleWithFunction: \"%s\" stage %lu is not a linked function of this pipeline (%lu linked) -> nil",
          f.name.UTF8String, (unsigned long)st, (unsigned long)c.count);
    return nil;
}
- (id<MTLVisibleFunctionTable>)newVisibleFunctionTableWithDescriptor:(MTLVisibleFunctionTableDescriptor *)d stage:(MTLRenderStages)st {
    return nvmtl_vft_new(d, (st & MTLRenderStageFragment) ? "fragment" : "vertex");
}
// intersection function tables (formerly the RT3IntersectionTables category in NVMTLLibraryLoad.m)
- (id<MTLIntersectionFunctionTable>)newIntersectionFunctionTableWithDescriptor:(MTLIntersectionFunctionTableDescriptor *)d stage:(MTLRenderStages)st {
    return nvmtl_ift_new(d, "render newIntersectionFunctionTableWithDescriptor");
}
@end

#pragma mark - encoder
static void nvmtl_retain_resource(NVMTLCommandBuffer *cb, id resource) {
    if (!resource) return;
    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    [cb->_resources addObject:resource];
}
// An app that declares a HEAP is not required to declare the resources inside it: useHeap: says the
// encoder may reach any of them indirectly, through an argument buffer or a bindless slot, and Metal
// keeps the heap alive and resident for the command buffer's lifetime. Every useHeap:/useHeaps:
// implementation here used to do nothing, so such a heap was retained by nobody: released after
// encoding, -[NVMTLHeap dealloc] ran nvmtl_vk_heap_destroy -> vkFreeMemory on the whole block while
// submitted work still read placed resources inside it, and the range lost its page table entry -
// the next access is an MMU fault on a valid-looking address. Cyberpunk 2077 references all four
// useHeap variants and creates heaps of 3 GiB and 384 MiB, so the window is large.
// The command buffer's resource set is class-checked everywhere it is read, so a heap sitting in it
// is skipped by the readers and only its lifetime matters.
static void nvmtl_retain_heaps(NVMTLCommandBuffer *cb, const id<MTLHeap> __unsafe_unretained heaps[], NSUInteger n) {
    if (!cb || !heaps) return;
    for (NSUInteger i = 0; i < n; i++) nvmtl_retain_resource(cb, heaps[i]);
}

// A heap in use: Metal's contract for useHeap: is that the encoder may reach any resource placed in the heap, through an
// argument buffer or a bindless slot, without naming it in useResource:. Every heap member is therefore held and made
// resident for the command buffer the way a used resource is. A pin is an immutable snapshot of the members at one
// version of the heap, shared by every command buffer that used the heap at that version, and it holds residency until
// the last submission that references it finishes. A delta pin covers a heap that only gained members since its last
// pin: it keeps that pin alive through _base and takes residency only on the new members (_own), so the members are not
// walked again on every use. The chain is collapsed by a full pin every 16 links so members the app frees are let go.
// _members and _copyCandidates are this link's only; -allMembers / -allCopyCandidates join the chain once and keep it.
//
// Two costs used to dominate a game's driver CPU time here (Cyberpunk 2077, a 3 GiB heap with thousands of members:
// 2249 full pins walking 11.9 million members in 32768 uses). First, every full pin took a residency reference
// (nvmtl_resref) on every member. Second, a pin died as soon as the last submission holding it finished, so a heap used
// once per frame was walked again from its weak membership table (one weak load under the runtime's weak lock per
// member) on nearly every frame even when nothing in it had changed.
//
// The residency reference is dead weight on a heap member: nvmtl_res2_movable / nvmtl_res3_movable and
// nvmtl_purge_eligible refuse anything with _heap, _subHeap or (textures) _backBuf, so the residency mover and the
// purger never page such a resource and never read its _resRefs, _useEp or _lastUse, and its _rstate never becomes 2,
// so the move wait is a no-op too. Members that are not heap-backed (or NVMTL_HEAP_PIN_LEGACY=1) still take it. _own is
// exactly the members that took one, so the release in -dealloc always matches the take.
// The one visible difference: the private isComplete / waitUntilComplete selectors on a heap member now report only
// the submissions that named it directly, not those that reached it only through useHeap:.
//
// The second cost is cut by keeping the newest pin of a heap alive for a short window after its full walk (see
// NVMTLHeapMembers), so the next frames reuse it or extend it with a delta instead of walking the heap again.
@interface NVMTLHeapPin : NSObject { @public NSArray *_members; NSArray *_copyCandidates; NSArray *_own; NVMTLHeapPin *_base; unsigned _depth;
                                     NSArray *_allMembers; NSArray *_allCopyCandidates; }
- (NSArray *)allMembers;
- (NSArray *)allCopyCandidates;
- (instancetype)initWithMembers:(NSArray *)members;
- (instancetype)initWithBase:(NVMTLHeapPin *)base added:(NSArray *)added;
- (instancetype)initFlatFromBase:(NVMTLHeapPin *)base added:(NSArray *)added;
@end
// NVMTL_HEAP_PIN_LEGACY=1 restores the pinning as it was before the kept pin, for A/B: every member takes a residency
// reference and no pin outlives its last submission.
static int nvmtl_heap_pin_legacy(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_HEAP_PIN_LEGACY"); on = e && e[0] == '1';
                  nvlog("heap pin: %s (NVMTL_HEAP_PIN_LEGACY=%s)", on ? "LEGACY - full residency refs, no kept pin" : "heap members skip residency refs, newest pin kept", e ? e : "unset"); }
    return on;
}
static _Atomic uint64_t gHeapPinRefsTaken, gHeapPinRefsSkipped;
// Does nvmtl_resref on this member do anything anyone reads? See the comment above NVMTLHeapPin for each reader.
static BOOL nvmtl_heap_pin_needs_ref(id o) {
    if (nvmtl_heap_pin_legacy()) return YES;
    if (!gNVMTLTexClass) gNVMTLTexClass = [NVMTLTexture class];
    if ([o isKindOfClass:gNVMTLTexClass]) {
        NVMTLTexture *t = o; if (t->_parent) t = t->_parent;   // nvmtl_resref counts on the owner
        return !(t->_heap || t->_subHeap || t->_backBuf);
    }
    static Class BC; if (!BC) BC = [NVMTLBuffer class];
    if (object_getClass(o) != BC) return NO;   // nvmtl_resref ignores every other class already
    NVMTLBuffer *b = o;
    return !(b->_heap || b->_subHeap);
}
@implementation NVMTLHeapPin
- (instancetype)initWithBase:(NVMTLHeapPin *)base added:(NSArray *)added {
    if ((self = [self initWithMembers:added])) { _base = base; _depth = base->_depth + 1; }
    return self;
}
- (NSArray *)allMembers {
    if (!_base) return _members;
    @synchronized (self) { if (!_allMembers) _allMembers = [[_base allMembers] arrayByAddingObjectsFromArray:_members]; return _allMembers; }
}
- (NSArray *)allCopyCandidates {
    if (!_base) return _copyCandidates;
    @synchronized (self) {
        if (!_allCopyCandidates) _allCopyCandidates = [[_base allCopyCandidates] arrayByAddingObjectsFromArray:_copyCandidates];
        return _allCopyCandidates;
    }
}
// Residency reference and host-copy classification for members this link adds; refs taken land in *own.
static void nvmtl_heap_pin_take(NSArray *members, NSMutableArray **own, NSMutableArray *copies) {
    uint64_t skipped = 0;
    for (id resource in members) {
        if (nvmtl_heap_pin_needs_ref(resource)) { nvmtl_resref(resource, 1); if (!*own) *own = [NSMutableArray new]; [*own addObject:resource]; }
        else skipped++;
        if (!copies) continue;
        NVMTLBuffer *buffer = nil;
        if ([resource isKindOfClass:[NVMTLBuffer class]]) buffer = resource;
        else if ([resource isKindOfClass:[NVMTLTexture class]]) {
            NVMTLTexture *texture = resource;
            buffer = texture->_backBuf ?: (texture->_parent ? texture->_parent->_backBuf : nil);
        }
        if (buffer && (buffer->_hostPtr || buffer->_shadow.map)) [copies addObject:resource];
    }
    atomic_fetch_add(&gHeapPinRefsTaken, members.count - skipped); atomic_fetch_add(&gHeapPinRefsSkipped, skipped);
}
- (instancetype)initWithMembers:(NSArray *)members {
    if ((self = [super init])) {
        _members = members;
        NSMutableArray *copies = [NSMutableArray new], *own = nil;
        nvmtl_heap_pin_take(members, &own, copies);
        _copyCandidates = [copies copy];
        _own = [own copy];
    }
    return self;
}
// A chain that reached 16 links collapses into one flat pin built from the chain's own strong arrays: the same members
// a walk of the weak table would find right now (every member alive is either in the chain or listed as added since),
// without one weak load per member. The flat pin takes its own references; the chain releases its own when it dies.
- (instancetype)initFlatFromBase:(NVMTLHeapPin *)base added:(NSArray *)added {
    if ((self = [super init])) {
        NSMutableArray *own = nil, *copies = [NSMutableArray new], *chainOwn = [NSMutableArray new];
        for (NVMTLHeapPin *p = base; p; p = p->_base) if (p->_own.count) [chainOwn addObjectsFromArray:p->_own];
        if (chainOwn.count) nvmtl_heap_pin_take(chainOwn, &own, nil);   // members that need a reference, again for this pin
        nvmtl_heap_pin_take(added, &own, copies);
        _members = [[base allMembers] arrayByAddingObjectsFromArray:added];
        _copyCandidates = [[base allCopyCandidates] arrayByAddingObjectsFromArray:copies];
        _own = [own copy];
    }
    return self;
}
- (void)dealloc { for (id resource in _own) nvmtl_resref(resource, -1); }   // only what this link took; _base releases the rest
@end
// The heap's membership: weak, so the heap never owns its members through this; _added lists the members registered
// since the last pin decision, for the next delta.
//
// What a use costs. Same version as the cached pin: nothing (reuse). Members added since: a delta link with just them.
// A chain of 16 links: one flat pin from the chain's strong arrays (no weak loads). Only when no pin of the heap is
// alive at all is the weak table walked, one weak load per member. Measured in Cyberpunk 2077 before these rules
// (3 GiB heap, ~12k members): 1329 full walks in 32768 uses even with a kept pin, because
//   - a pin was cached only if the heap's version had not moved while it was being built; the game registers members
//     all the time, a walk of 12k members takes long enough to see one, and the uncached pin left the next use with
//     no pin to extend, so it walked again (and again) - each walk invalidated by the next registration; and
//   - command buffers encoded on several threads that missed together each walked the same heap at once.
// Now _added is taken over at the decision (the new list collects what registers during the build), every built pin
// is installed with the version it covers, and one build runs per heap at a time: a second user waits for it
// (bounded, see kHeapPinWaitS) and then reuses or extends it.
//
// _keep holds the newest pin strongly for NVMTL_HEAP_PIN_KEEP_MS (default 200 ms, 0 = off) after a WALK, so a heap
// whose submissions drain between frames is not walked again every frame. The window is anchored to the walk: deltas
// and flat collapses move the keep to the new chain head but never extend the deadline, so once it passes the chain
// lives only as long as submissions hold it, as before, and the first drain after that lets every member the app
// released go. A walk can only happen when no pin is alive, so it never re-captures a member held only by an old pin.
// The keep is dropped by a timer at the deadline (this also breaks the heap -> membership -> kept pin -> member ->
// heap cycle when the app releases the heap) and when an automatic heap is out of room.
// Every access to the membership is under _cv's lock (a condition, so a waiting user can sleep on the build).
@interface NVMTLHeapMembers : NSObject { @public NSHashTable *_members; uint64_t _version; __weak NVMTLHeapPin *_pin; uint64_t _pinVersion; NSPointerArray *_added;
                                        NVMTLHeapPin *_keep; uint64_t _keepUntil, _keepGen; NSCondition *_cv; BOOL _building, _expired, _everPinned;
                                        // per-heap census
                                        uint64_t _uses, _reuses, _deltas, _flats, _waits, _walks, _walked, _why[4]; }
@end
@implementation NVMTLHeapMembers
- (instancetype)init { if ((self = [super init])) { _members = [NSHashTable weakObjectsHashTable]; _added = [NSPointerArray weakObjectsPointerArray];
                                                     _cv = [NSCondition new]; } return self; }
@end
// Per command buffer: the version of each heap it already holds a pin for, so a heap used by every encoder of a frame
// is pinned once per command buffer.
@interface NVMTLHeapUseCache : NSObject { @public NSLock *_lock; NSMutableDictionary *_versions; }
@end
@implementation NVMTLHeapUseCache
- (instancetype)init { if ((self = [super init])) { _lock = [NSLock new]; _versions = [NSMutableDictionary new]; } return self; }
@end
static const char kHeapMembers, kHeapUseCache;
static NVMTLHeapMembers *nvmtl_heap_members(NVMTLHeap *heap) {
    if (!heap) return nil;
    @synchronized (heap) {
        NVMTLHeapMembers *m = objc_getAssociatedObject(heap, &kHeapMembers);
        if (!m) { m = [NVMTLHeapMembers new]; objc_setAssociatedObject(heap, &kHeapMembers, m, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
        return m;
    }
}
static uint64_t nvmtl_heap_pin_keep_ns(void) {
    static int64_t ns = -1;
    if (ns < 0) { const char *e = getenv("NVMTL_HEAP_PIN_KEEP_MS"); long long ms = e && *e ? strtoll(e, NULL, 10) : 200;
                  if (ms < 0 || ms > 10000) { nvlog("heap pin: NVMTL_HEAP_PIN_KEEP_MS=%s out of range 0..10000 - using 200", e); ms = 200; }
                  ns = nvmtl_heap_pin_legacy() ? 0 : (int64_t)ms * 1000000;
                  nvlog("heap pin: newest pin kept %lld ms after a walk (NVMTL_HEAP_PIN_KEEP_MS=%s)", (long long)(ns / 1000000), e ? e : "unset"); }
    return (uint64_t)ns;
}
// A use that finds another thread building the heap's pin waits at most this long, then walks for itself without
// installing (correct, just the old cost) and says so: a build that never finishes must not hang the encoder.
static const double kHeapPinWaitS = 2.0;
static _Atomic uint64_t gHeapPinKeepDrops[2];   // [0] expired (timer or use), [1] heap out of room
// Takes the kept pin out under the lock; the caller lets it go OUTSIDE it: the last release of a pin can free members,
// whose dealloc returns heap ranges and destroys Vulkan objects.
// Returned retained, never autoreleased: an autoreleased pin would live until the app's pool drains, and the
// out-of-room retry would see no space come back.
static NVMTLHeapPin *nvmtl_heap_unkeep_locked(NVMTLHeapMembers *m, int why) NS_RETURNS_RETAINED;
static NVMTLHeapPin *nvmtl_heap_unkeep_locked(NVMTLHeapMembers *m, int why) {
    NVMTLHeapPin *k = m->_keep; m->_keep = nil; m->_keepUntil = 0;
    if (k) { atomic_fetch_add(&gHeapPinKeepDrops[why], 1); if (why == 0) m->_expired = YES; }
    return k;
}
// An automatic heap that cannot fit an allocation: let the kept pin go first, so members the app already released
// (and no submission holds) are freed and their space returns. YES when a pin was let go, so the caller retries once.
static BOOL nvmtl_heap_unkeep(NVMTLHeap *heap) {
    NVMTLHeapMembers *m = objc_getAssociatedObject(heap, &kHeapMembers);
    if (!m) return NO;
    BOOL had;
    @autoreleasepool {   // anything the last release of the pin autoreleases goes now, not at the app's next drain
        NVMTLHeapPin *k;
        [m->_cv lock]; k = nvmtl_heap_unkeep_locked(m, 1); [m->_cv unlock];
        had = k != nil;
        k = nil; (void)k;
    }
    return had;
}
static void nvmtl_heap_register(NVMTLHeap *heap, id resource) {
    if (!heap || !resource) return;
    NVMTLHeapMembers *m = nvmtl_heap_members(heap);
    [m->_cv lock];
    if (![m->_members containsObject:resource]) { [m->_members addObject:resource]; [m->_added addPointer:(__bridge void *)resource]; m->_version++; }
    [m->_cv unlock];
}
@interface NVMTLHeap () { @public BOOL _hzLegacy; } @end   // NVMTL_HAZARD: a member lives outside the heap's memory
// NVMTL_HEAP_PIN_LEGACY=1: the use path exactly as it was before the kept pin and the single build, for A/B.
static void nvmtl_use_heap_legacy(NVMTLCommandBuffer *cb, NVMTLHeap *heap, NVMTLHeapUseCache *cache) {
    NVMTLHeapMembers *members = nvmtl_heap_members(heap);
    [cache->_lock lock];
    NSArray *resources = nil, *added = nil; NVMTLHeapPin *pin = nil, *base = nil; uint64_t version;
    NSValue *key = [NSValue valueWithNonretainedObject:heap];
    [members->_cv lock]; {
        version = members->_version;
        NSNumber *prior = cache->_versions[key];
        if (!prior || prior.unsignedLongLongValue != version) {
            if (members->_pinVersion == version) pin = members->_pin;
            if (!pin) {
                base = members->_pin;   // the last pin, if a submission still holds it
                if (base && base->_depth < 16) { [members->_added compact]; added = members->_added.allObjects; }
                else { base = nil; resources = members->_members.allObjects; }
            }
        }
        { static _Atomic uint64_t calls, rebuilds, walked, deltas; const uint64_t c = atomic_fetch_add(&calls, 1) + 1;
          if (resources) { atomic_fetch_add(&rebuilds, 1); atomic_fetch_add(&walked, resources.count); }
          if (added) atomic_fetch_add(&deltas, 1);
          if ((c & 4095) == 0) nvlog("heap pin (legacy): %llu uses, %llu full pins walking %llu members, %llu delta pins", (unsigned long long)c,
                                     (unsigned long long)atomic_load(&rebuilds), (unsigned long long)atomic_load(&walked), (unsigned long long)atomic_load(&deltas)); }
    } [members->_cv unlock];
    // Residency can wait inside allocator locks: build the pin outside the membership monitor.
    if (resources || added) {
        pin = added ? [[NVMTLHeapPin alloc] initWithBase:base added:added] : [[NVMTLHeapPin alloc] initWithMembers:resources];
        [members->_cv lock]; {
            // The added list is cleared only when this pin is the one cached: if the heap gained members meanwhile, the
            // members just consumed stay listed, or the next delta would miss them (an unpinned member is a GPU fault).
            if (members->_version == version) { members->_pin = pin; members->_pinVersion = version;
                                                 members->_added = [NSPointerArray weakObjectsPointerArray]; }
        } [members->_cv unlock];
    }
    if (pin) {
        nvmtl_retain_resource(cb, heap);
        nvmtl_retain_resource(cb, pin);
        cache->_versions[key] = @(version);
    }
    [cache->_lock unlock];
}

enum { kHeapWalkFirst, kHeapWalkExpired, kHeapWalkDied, kHeapWalkTimeout };   // why a use had to walk the weak table
static void nvmtl_use_heap(NVMTLCommandBuffer *cb, id<MTLHeap> value) {
    NVMTLHeap *heap = (NVMTLHeap *)value;
    if (!cb || !heap || ![heap isKindOfClass:[NVMTLHeap class]]) return;
    if (nvmtl_hz_mode()) {
        if (!heap->_lzc) nvmtl_hz_use_heap(&cb->_c, heap->_hazard == MTLHazardTrackingModeTracked, heap->_hm.mem, heap->_hm.size, heap->_hzLegacy);
        else {   // a lazily backed heap: every memory it has so far
            os_unfair_lock_lock(&heap->_lzl);
            for (uint32_t k = 0; k < heap->_lzn; k++)
                nvmtl_hz_use_heap(&cb->_c, heap->_hazard == MTLHazardTrackingModeTracked, heap->_lzm[k].mem, heap->_lzm[k].size, heap->_hzLegacy);
            os_unfair_lock_unlock(&heap->_lzl);
        }
    }
    NVMTLHeapUseCache *cache;
    // A lock object of its own: the command buffer's monitor may be held by the application.
    @synchronized ([NVMTLHeapUseCache class]) {
        cache = objc_getAssociatedObject(cb, &kHeapUseCache);
        if (!cache) { cache = [NVMTLHeapUseCache new]; objc_setAssociatedObject(cb, &kHeapUseCache, cache, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
    }
    if (nvmtl_heap_pin_legacy()) { nvmtl_use_heap_legacy(cb, heap, cache); return; }
    NVMTLHeapMembers *members = nvmtl_heap_members(heap);
    [cache->_lock lock];
    NSValue *key = [NSValue valueWithNonretainedObject:heap];
    NSNumber *prior = cache->_versions[key];
    const uint64_t keepNs = nvmtl_heap_pin_keep_ns();
    enum { kNone, kReuse, kDelta, kFlat, kWalk } kind = kNone;
    NSArray *resources = nil, *added = nil; NVMTLHeapPin *pin = nil, *base = nil; uint64_t version = 0;
    NSMutableArray *dropped = nil;   // kept pins that expired while this use looked; released after the lock
    BOOL install = NO, waited = NO; int why = -1;
    NSDate *waitUntil = nil;
    [members->_cv lock];
    members->_uses++;
    for (;;) {
        if (members->_keep && clock_gettime_nsec_np(CLOCK_UPTIME_RAW) >= members->_keepUntil) {
            NVMTLHeapPin *d = nvmtl_heap_unkeep_locked(members, 0);
            if (!dropped) dropped = [NSMutableArray new];
            [dropped addObject:d];
        }
        version = members->_version;
        if (prior && prior.unsignedLongLongValue == version) break;   // this command buffer already holds a pin at this version
        NVMTLHeapPin *cur = members->_pin;
        if (cur && members->_pinVersion == version) { pin = cur; kind = kReuse; members->_reuses++; break; }
        if (members->_building) {
            if (!waitUntil) { waitUntil = [NSDate dateWithTimeIntervalSinceNow:kHeapPinWaitS]; members->_waits++; waited = YES; }
            if ([members->_cv waitUntilDate:waitUntil]) continue;
            // the build did not finish in time: walk privately, install nothing, leave the builder alone
            nvlog("heap pin: waited %.0f s for another thread's pin build of heap %p - walking %lu members privately (not installed)",
                  kHeapPinWaitS, (__bridge void *)heap, (unsigned long)members->_members.count);
            resources = members->_members.allObjects; kind = kWalk; why = kHeapWalkTimeout; break;
        }
        // This thread builds. _added is taken over now: members that register during the build land in the new list and
        // move the version, so the next use extends this pin with exactly them.
        [members->_added compact];
        if (cur && cur->_depth < 16) { base = cur; added = members->_added.allObjects; kind = kDelta; }
        else if (cur) { base = cur; added = members->_added.allObjects; kind = kFlat; }
        else { resources = members->_members.allObjects; kind = kWalk;
               why = !members->_everPinned ? kHeapWalkFirst : members->_expired ? kHeapWalkExpired : kHeapWalkDied; }
        members->_added = [NSPointerArray weakObjectsPointerArray];
        members->_building = YES; install = YES;
        break;
    }
    if (kind == kDelta) members->_deltas++;
    if (kind == kFlat) members->_flats++;
    if (kind == kWalk) { members->_walks++; members->_walked += resources.count; members->_why[why]++; }
    const BOOL sayHeap = kind == kWalk && (members->_walks <= 4 || (members->_walks & 63) == 0);
    uint64_t hc[11] = { members->_uses, members->_reuses, members->_deltas, members->_flats, members->_waits, members->_walks, members->_walked,
                        members->_why[0], members->_why[1], members->_why[2], members->_why[3] };
    const NSUInteger hcount = sayHeap ? members->_members.count : 0;
    [members->_cv unlock];
    dropped = nil; (void)dropped;   // outside the lock
    if (sayHeap)
        nvlog("heap pin: heap %p (%llu MB, ~%lu members): %llu uses, %llu reuses, %llu deltas, %llu flat collapses, %llu waits; "
              "%llu walks over %llu members (first %llu, after keep expiry %llu, pin died unkept %llu, wait timeout %llu)",
              (__bridge void *)heap, (unsigned long long)(heap->_size >> 20), (unsigned long)hcount,
              (unsigned long long)hc[0], (unsigned long long)hc[1], (unsigned long long)hc[2], (unsigned long long)hc[3], (unsigned long long)hc[4],
              (unsigned long long)hc[5], (unsigned long long)hc[6], (unsigned long long)hc[7], (unsigned long long)hc[8], (unsigned long long)hc[9],
              (unsigned long long)hc[10]);
    { static _Atomic uint64_t calls, walks, walked, deltas, flats, reuses, waits;
      const uint64_t c = atomic_fetch_add(&calls, 1) + 1;
      if (kind == kWalk) { atomic_fetch_add(&walks, 1); atomic_fetch_add(&walked, resources.count); }
      if (kind == kDelta) atomic_fetch_add(&deltas, 1);
      if (kind == kFlat) atomic_fetch_add(&flats, 1);
      if (kind == kReuse) atomic_fetch_add(&reuses, 1);
      if (waited) atomic_fetch_add(&waits, 1);
      if ((c & 4095) == 0) nvlog("heap pin: %llu uses (all heaps), %llu full pins walking %llu members, %llu delta pins, %llu flat collapses, "
                                 "%llu pin reuses, %llu waits on another build; member refs %llu taken / %llu skipped (heap-backed); "
                                 "keep dropped %llu expired, %llu for room",
                                 (unsigned long long)c, (unsigned long long)atomic_load(&walks), (unsigned long long)atomic_load(&walked),
                                 (unsigned long long)atomic_load(&deltas), (unsigned long long)atomic_load(&flats), (unsigned long long)atomic_load(&reuses),
                                 (unsigned long long)atomic_load(&waits),
                                 (unsigned long long)atomic_load(&gHeapPinRefsTaken), (unsigned long long)atomic_load(&gHeapPinRefsSkipped),
                                 (unsigned long long)atomic_load(&gHeapPinKeepDrops[0]), (unsigned long long)atomic_load(&gHeapPinKeepDrops[1])); }
    // Residency can wait inside allocator locks: build the pin outside the membership lock.
    if (kind == kDelta || kind == kFlat || kind == kWalk) {
        pin = kind == kDelta ? [[NVMTLHeapPin alloc] initWithBase:base added:added]
            : kind == kFlat  ? [[NVMTLHeapPin alloc] initFlatFromBase:base added:added]
            :                  [[NVMTLHeapPin alloc] initWithMembers:resources];
        if (install) {
            NVMTLHeapPin *replaced = nil; uint64_t keepGen = 0;
            [members->_cv lock];
            members->_pin = pin; members->_pinVersion = version; members->_everPinned = YES; members->_building = NO;
            // A walk opens a keep window; a delta or collapse inside an open window moves the keep to the new chain head
            // without extending the deadline; one after it closed is not kept.
            if (keepNs && kind == kWalk) { replaced = members->_keep; members->_keep = pin; members->_expired = NO;
                                           members->_keepUntil = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) + keepNs; keepGen = ++members->_keepGen; }
            else if (members->_keep) { replaced = members->_keep; members->_keep = pin; }
            [members->_cv broadcast];
            [members->_cv unlock];
            replaced = nil; (void)replaced;   // outside the lock
            if (keepGen) {
                __weak NVMTLHeapMembers *wm = members;
                dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)keepNs), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
                    NVMTLHeapMembers *m = wm; if (!m) return;
                    NVMTLHeapPin *k = nil;
                    [m->_cv lock]; if (m->_keepGen == keepGen) k = nvmtl_heap_unkeep_locked(m, 0); [m->_cv unlock];
                    k = nil; (void)k;
                });
            }
        }
    }
    base = nil; (void)base;
    if (pin) {
        nvmtl_retain_resource(cb, heap);
        nvmtl_retain_resource(cb, pin);
        cache->_versions[key] = @(version);
    }
    [cache->_lock unlock];
}

@implementation NVMTLCounter
- (NSString *)name { return MTLCommonCounterTimestamp; }
@end
@implementation NVMTLCounterSet
- (NSString *)name { return MTLCommonCounterSetTimestamp; }
- (NSArray *)counters { return _counters; }
@end
id<MTLCounterSet> nvmtl_timestamp_counter_set(void)
{
    static NVMTLCounterSet *set; static dispatch_once_t once;
    dispatch_once(&once, ^{ NVMTLCounterSet *s = [NVMTLCounterSet new]; s->_counters = @[[NVMTLCounter new]]; set = s; });
    return set;
}
@implementation NVMTLCounterSampleBuffer
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label ?: @""; }
- (NSUInteger)sampleCount { return _count; }
- (NSData *)resolveCounterRange:(NSRange)r {
    if (!r.length || r.location > _count || r.length > _count - r.location) {
        nvlog("resolveCounterRange: %lu+%lu is outside this buffer's %lu samples - nil", (unsigned long)r.location, (unsigned long)r.length, (unsigned long)_count);
        return nil;
    }
    NSMutableData *d = [NSMutableData dataWithLength:r.length * 8];
    if (nvmtl_vk_ts_read(_pool, (uint32_t)r.location, (uint32_t)r.length, d.mutableBytes)) { nvlog("resolveCounterRange: read FAILED - nil"); return nil; }
    return d;
}
- (void)dealloc { nvmtl_vk_ts_pool_destroy(_pool); _pool = NULL; }
@end
static void nvmtl_counter_recording_failed(NVMTLCommandBuffer *cb)
{
    cb->_c.counter_error = 1;
    [cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain
        code:MTLCommandBufferErrorInternal userInfo:@{NSLocalizedDescriptionKey:@"Counter recording failed"}]];
}
static void nvmtl_sample_counter(NVMTLCommandBuffer *cb, id sb, NSUInteger i, id who, BOOL barrier)
{
    static int said_foreign, said_range, said_fail;
    if (![sb isKindOfClass:[NVMTLCounterSampleBuffer class]]) {
        if (said_foreign++ < 4) nvlog("%s sampleCountersInBuffer: %s is not a sample buffer this device made - not sampled", object_getClassName(who), sb ? object_getClassName(sb) : "nil");
        nvmtl_counter_recording_failed(cb); return;
    }
    NVMTLCounterSampleBuffer *b = sb;
    if (i >= b->_count) {
        if (said_range++ < 4) nvlog("%s sampleCountersInBuffer: index %lu is past the buffer's %lu samples - not sampled", object_getClassName(who), (unsigned long)i, (unsigned long)b->_count);
        nvmtl_counter_recording_failed(cb); return;
    }
    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    [cb->_resources addObject:b];
    if (nvmtl_vk_cmd_ts_write(&cb->_c, b->_pool, (uint32_t)i, barrier)) {
        nvmtl_counter_recording_failed(cb);
        if (said_fail++ < 4) nvlog("%s sampleCountersInBuffer: index %lu - recording failed", object_getClassName(who), (unsigned long)i);
    }
}

unsigned nvmtl_apps1_resolve_miss, nvmtl_apps1_resolve_extra, nvmtl_apps1_ds_resolve_miss;
@interface NVMTLRenderCommandEncoder () { unsigned _groupDepth, _dumpDepth; NSString *_dumpName; BOOL _dumpPso; }
- (void)nvmtlDumpBoundFragmentBuffers;
@end

@interface NVMTLRenderCommandEncoder () { @public BOOL _hzFull; uint32_t _hzAttSkip; } @end   // NVMTL_HAZARD: keeps render passes; untracked attachments
// Metal does not order an untracked resource between encoders (nvmtl_hazard.h); NVMTL_HAZARD_HEAPS=all tracks everything.
static int nvmtl_hz_untracked_res(id r) {
    return nvmtl_hz_mode() > 0 && !nvmtl_hz_heaps_all() && [r respondsToSelector:@selector(hazardTrackingMode)] &&
           [(id<MTLResource>)r hazardTrackingMode] == MTLHazardTrackingModeUntracked;
}
// The same from the ivars, for the per-bind paths (no message sends): what -hazardTrackingMode returns. The answer is
// cached in the resource's own nvk struct on first use (hz_ut: 1 tracked, 2 untracked), so a bind costs one byte read:
// the mode of a resource never changes. 0 = not yet known, or the model is off.
static inline int nvmtl_hz_untracked_buf(NVMTLBuffer *b) {
    if (!b) return 0;
    const uint8_t v = b->_b.hz_ut;
    if (__builtin_expect(v != 0, 1)) return v == 2;
    if (nvmtl_hz_mode() <= 0) return 0;
    const uint8_t n = !nvmtl_hz_heaps_all() && nvmtl_res_hazard(b->_heap ? b->_heap : b->_subHeap, b->_ropt) == MTLHazardTrackingModeUntracked ? 2 : 1;
    b->_b.hz_ut = n; return n == 2;
}
static inline int nvmtl_hz_untracked_tex(NVMTLTexture *t) {
    if (!t) return 0;
    NVMTLTexture *r = t->_parent ? t->_parent : t;
    const uint8_t v = r->_i.hz_ut;
    if (__builtin_expect(v != 0, 1)) return v == 2;
    if (nvmtl_hz_mode() <= 0) return 0;
    const uint8_t n = !nvmtl_hz_heaps_all() && nvmtl_res_hazard(r->_heap ? r->_heap : r->_subHeap, r->_ropt) == MTLHazardTrackingModeUntracked ? 2 : 1;
    r->_i.hz_ut = n; return n == 2;
}
// Hazard model inputs (nvmtl_hazard.h): which buffer slots and storage textures a function writes, from the translator's
// reflection. A buffer is a write unless the reflection proves it read-only; a slot with no reflection entry is a read
// (only fixed-function vertex fetch reaches it). An unknown binding kind is a write of both. A function with no
// reflection writes everything. Argument-buffer contents are declared by useResource:usage:.
typedef struct { uint32_t buf; uint64_t tex[2]; } nvmtl_hz_writes;
static const nvmtl_hz_writes kNVMTLHzAll = { 0xffffffffu, { ~0ull, ~0ull } };
static nvmtl_hz_writes nvmtl_hz_fn_writes(id f) {
    if (![f isKindOfClass:[NVMTLFunction class]]) return kNVMTLHzAll;
    NVMTLFunction *fn = (NVMTLFunction *)f;
    static char kKey;
    NSData *cached = objc_getAssociatedObject(fn, &kKey);
    nvmtl_hz_writes w = { 0, { 0, 0 } };
    if (cached.length == sizeof w) { memcpy(&w, cached.bytes, sizeof w); return w; }
    NSArray *bindings = nvmtl_function_reflection(fn)[@"bindings"];
    if (![bindings isKindOfClass:[NSArray class]]) w = kNVMTLHzAll;
    else for (NSDictionary *b in bindings) {
        if (![b isKindOfClass:[NSDictionary class]] || ![b[@"metal_index"] isKindOfClass:[NSNumber class]]) continue;
        NSString *kind = [b[@"kind"] isKindOfClass:[NSString class]] ? b[@"kind"] : @"", *access = b[@"access"];
        const NSUInteger i = [b[@"metal_index"] unsignedLongValue];
        const BOOL ro = [access isKindOfClass:[NSString class]] &&
            ([access isEqualToString:@"ReadOnly"] || [access isEqualToString:@"Unused"] || [access isEqualToString:@"Sampled"]);
        if ([kind isEqualToString:@"Sampler"] || [kind isEqualToString:@"StaticSampler"] || [kind hasPrefix:@"Embedded"]) continue;
        const BOOL buf = [kind isEqualToString:@"Buffer"];
        const BOOL tex = [kind isEqualToString:@"Texture"] || [kind isEqualToString:@"TextureArray"] || [kind isEqualToString:@"StorageImage"];
        if (ro && !(tex && [kind isEqualToString:@"StorageImage"])) continue;
        if ((buf || !tex) && i < 32) w.buf |= 1u << i;
        if ((tex || !buf) && i < 128) w.tex[i >> 6] |= 1ull << (i & 63);
    }
    objc_setAssociatedObject(fn, &kKey, [NSData dataWithBytes:&w length:sizeof w], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return w;
}
static void nvmtl_hz_render_writes(NVMTLCommandBuffer *cb, NVMTLRenderPipelineState *p) {
    nvk_cmdbuf *c = &cb->_c;
    if (!__atomic_load_n(&p->_hzReady, __ATOMIC_ACQUIRE)) {   // once per pipeline; two threads computing it agree
        MTLRenderPipelineDescriptor *d = [p->_descriptor isKindOfClass:[MTLRenderPipelineDescriptor class]] ? p->_descriptor : nil;
        const nvmtl_hz_writes none = { 0, { 0, 0 } };
        const nvmtl_hz_writes v = d ? nvmtl_hz_fn_writes(d.vertexFunction) : kNVMTLHzAll;
        const nvmtl_hz_writes f = !d ? kNVMTLHzAll : d.fragmentFunction ? nvmtl_hz_fn_writes(d.fragmentFunction) : none;
        p->_hzBuf[0] = v.buf; p->_hzBuf[1] = f.buf; memcpy(p->_hzTex[0], v.tex, sizeof v.tex); memcpy(p->_hzTex[1], f.tex, sizeof f.tex);
        __atomic_store_n(&p->_hzReady, 1, __ATOMIC_RELEASE);
    }
    // a full rescan only when what the pipeline writes or samples differs from the last one's (pipelines switch per draw)
    const uint8_t ok = p->_p.gmask_ok ? 1 : 0;
    if (c->hz_bufw[0] == p->_hzBuf[0] && c->hz_bufw[1] == p->_hzBuf[1] && !memcmp(c->hz_texw, p->_hzTex, sizeof c->hz_texw) &&
        c->hz_tmask_ok[0] == ok && c->hz_tmask_ok[1] == ok && (!ok || !memcmp(c->hz_tmask, p->_p.gmask, sizeof c->hz_tmask))) return;
    c->hz_bufw[0] = p->_hzBuf[0]; c->hz_bufw[1] = p->_hzBuf[1];
    memcpy(c->hz_texw, p->_hzTex, sizeof c->hz_texw);
    c->hz_tmask_ok[0] = c->hz_tmask_ok[1] = ok;
    if (ok) memcpy(c->hz_tmask, p->_p.gmask, sizeof c->hz_tmask);
    c->hz_wm_dirty = 1;
}
static void nvmtl_hz_compute_writes(NVMTLCommandBuffer *cb, NVMTLComputePipelineState *ps) {
    nvk_cmdbuf *c = &cb->_c;
    nvmtl_hz_writes w = kNVMTLHzAll;
    if (ps) {
        if (!__atomic_load_n(&ps->_hzReady, __ATOMIC_ACQUIRE)) {
            const nvmtl_hz_writes f = nvmtl_hz_fn_writes(ps->_function);
            ps->_hzBuf = f.buf; memcpy(ps->_hzTex, f.tex, sizeof f.tex); __atomic_store_n(&ps->_hzReady, 1, __ATOMIC_RELEASE);
        }
        w.buf = ps->_hzBuf; memcpy(w.tex, ps->_hzTex, sizeof w.tex);
    }
    c->hz_bufw[0] = w.buf; c->hz_bufw[1] = 0;
    memcpy(c->hz_texw[0], w.tex, sizeof w.tex); memset(c->hz_texw[1], 0, sizeof c->hz_texw[1]);
    c->hz_tmask_ok[0] = ps && ps->_p.bmask_ok ? 1 : 0; c->hz_tmask_ok[1] = 0;
    if (c->hz_tmask_ok[0]) memcpy(c->hz_tmask[0], ps->_p.bmask, sizeof c->hz_tmask[0]);
    c->hz_wm_dirty = 1;
}
static void nvmtl_hz_use(NVMTLCommandBuffer *cb, id r, MTLResourceUsage u) {
    if (nvmtl_hz_mode() <= 0 || !cb || !r || nvmtl_hz_untracked_res(r)) return;
    const int w = (u & MTLResourceUsageWrite) != 0;
    if ([r isKindOfClass:[NVMTLBuffer class]]) nvmtl_hz_declare_buffer(&cb->_c, &((NVMTLBuffer *)r)->_b, w, 0);
    else if ([r isKindOfClass:[NVMTLTexture class]]) nvmtl_hz_declare_image(&cb->_c, [(NVMTLTexture *)r nvi], w);
    else nvmtl_hz_declare_any(&cb->_c, w);
}
// A tracked blit (nvmtl_hazard.h): key src (read) and dst (written), check, and close the scope when the method returns.
static void nvmtl_hz_blit_close(nvk_cmdbuf **c) { if (*c) nvmtl_hz_blit_end(*c); }
static nvk_cmdbuf *nvmtl_hz_blit_open(NVMTLCommandBuffer *cb, id src, id dst) {
    if (nvmtl_hz_mode() <= 0 || !cb) return NULL;
    nvk_cmdbuf *c = &cb->_c;
    if (!nvmtl_hz_blit_begin(c)) return NULL;
    for (int k = 0; k < 2; k++) {
        id r = k ? dst : src;
        if (!r || nvmtl_hz_untracked_res(r)) continue;
        if ([r isKindOfClass:[NVMTLBuffer class]]) nvmtl_hz_blit_key(c, &((NVMTLBuffer *)r)->_b, NULL, k);
        else if ([r isKindOfClass:[NVMTLTexture class]]) nvmtl_hz_blit_key(c, NULL, [(NVMTLTexture *)r nvi], k);
        else nvmtl_hz_blit_key(c, NULL, NULL, k);
    }
    nvmtl_hz_blit_go(c);
    return c;
}
#define NVMTL_HZ_BLIT(src, dst) __attribute__((cleanup(nvmtl_hz_blit_close))) nvk_cmdbuf *hzblit_ = nvmtl_hz_blit_open(_cb, (id)(src), (id)(dst)); (void)hzblit_
@implementation NVMTLRenderCommandEncoder
// NVMTL_HAZARD: end the pass, take a full barrier and begin it again with every attachment loaded (as a pipeline with
// fewer attachments does). forceFull: this encoder keeps render passes from here on.
// Framebuffer fetch emulated by copies (nvmtl_fetch_copy.h): the pass ends, the colour attachments this pipeline reads are
// copied, and the pass begins again with every attachment loaded, so the draw reads what the earlier draws wrote. The
// pipeline, viewport, scissor, cull and depth state are set again by the same path a hazard restart uses.
- (BOOL)nvmtlFetchCopy:(nvk_pipeline *)p {
    if (!_begun) return NO;
    uint32_t keep = 0;
    for (uint32_t i = 0; i < _passSig.ncol; i++) if (_passSig.cfmts[i]) keep |= NVMTL_LOAD_COLOUR(i);
    if (_passSig.has_depth) keep |= NVMTL_LOAD_DEPTH;
    if (_ps && _ps->_p.has_stencil) keep |= NVMTL_LOAD_STENCIL;
    const BOOL visibility = _visActive || _visPending;
    [self nvmtlCloseVisibility];
    nvmtl_vk_cmd_end_render(&_cb->_c);
    const int cr = nvmtl_vk_fetch_copy(&_cb->_c, &_pass, p->fetch_copy);
    _begun = NO; _pass.load |= keep; _load |= keep; _visPending = visibility;
    NVMTLRenderPipelineState *resume = _ps;
    if (resume) [self setRenderPipelineState:resume];
    if (!_begun) { nvlog("encoder: the render pass could not RESUME after a fetch copy - the rest of this encoder is not drawn"); return NO; }
    if (cr) { static _Atomic unsigned said; if (atomic_fetch_add(&said, 1) < 4) nvlog("encoder: a fetch copy failed - the draw is skipped"); return NO; }
    return YES;
}
- (void)nvmtlHzRestart:(BOOL)forceFull {
    if (forceFull) _hzFull = YES;
    if (!_begun) return;
    uint32_t keep = 0;
    for (uint32_t i = 0; i < _passSig.ncol; i++) if (_passSig.cfmts[i]) keep |= NVMTL_LOAD_COLOUR(i);
    if (_passSig.has_depth) keep |= NVMTL_LOAD_DEPTH;
    if (_ps && _ps->_p.has_stencil) keep |= NVMTL_LOAD_STENCIL;
    BOOL visibility = _visActive || _visPending;
    [self nvmtlCloseVisibility];
    nvmtl_vk_cmd_end_render(&_cb->_c);
    _begun = NO; _pass.load |= keep; _load |= keep; _visPending = visibility;
    nvmtl_vk_barrier_site(NVMTL_BARS_HAZARD_RESTART); nvmtl_vk_cmd_barrier(&_cb->_c);
    NVMTLRenderPipelineState *p = _ps;
    if (p) [self setRenderPipelineState:p];
}
// An indirect, patch or ICB draw, or a framebuffer-fetch pipeline: not checked by the model, so the pass is ordered by a
// render pass's own dependencies from here on.
- (void)nvmtlHzForceFull {
    const int m = nvmtl_hz_mode();
    if (!m) return;
    if (m == 1 && _cb->_c.hz_dyn) [self nvmtlHzRestart:YES];
    else { _hzFull = YES; nvmtl_hz_force_full(&_cb->_c, 0); }
}
- (NSUInteger)tileWidth { return _tileW ? _tileW : 32; }
- (NSUInteger)tileHeight { return _tileH ? _tileH : 32; }
// The tile entry points. No tile pipeline can exist (the device refuses to make one), so a call here is an application
// error; each is logged and ignored rather than raising an unrecognised-selector exception.
static void nvmtl_tile_ignored(const char *what) { static _Atomic unsigned n; if (atomic_fetch_add(&n, 1) < 8) nvlog("render encoder %s: tile shading is not carried - ignored", what); }
- (void)setTileBytes:(const void *)bytes length:(NSUInteger)length atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileBytes"); return; }
    if (!_tileByt) _tileByt = [NSMutableDictionary new];
    if (bytes && length) _tileByt[@(index)] = [NSData dataWithBytes:bytes length:length]; else [_tileByt removeObjectForKey:@(index)];
    [_tileBuf removeObjectForKey:@(index)];
}
- (void)setTileBuffer:(id<MTLBuffer>)buffer offset:(NSUInteger)offset atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileBuffer"); return; }
    if (!_tileBuf) _tileBuf = [NSMutableDictionary new];
    if (buffer) _tileBuf[@(index)] = @[buffer, @(offset)]; else [_tileBuf removeObjectForKey:@(index)];
    [_tileByt removeObjectForKey:@(index)];
}
- (void)setTileBufferOffset:(NSUInteger)offset atIndex:(NSUInteger)index {
    NSArray *e = _tileBuf[@(index)]; if (e) _tileBuf[@(index)] = @[e[0], @(offset)]; else nvmtl_tile_ignored("setTileBufferOffset with no buffer");
}
- (void)setTileBuffers:(const id<MTLBuffer> __unsafe_unretained [])buffers offsets:(const NSUInteger [])offsets withRange:(NSRange)range {
    for (NSUInteger k = 0; k < range.length; k++) [self setTileBuffer:buffers ? buffers[k] : nil offset:offsets ? offsets[k] : 0 atIndex:range.location + k];
}
- (void)setTileTexture:(id<MTLTexture>)texture atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileTexture"); return; }
    if (!_tileTex) _tileTex = [NSMutableDictionary new];
    if (texture) _tileTex[@(index)] = texture; else [_tileTex removeObjectForKey:@(index)];
}
- (void)setTileTextures:(const id<MTLTexture> __unsafe_unretained [])textures withRange:(NSRange)range {
    for (NSUInteger k = 0; k < range.length; k++) [self setTileTexture:textures ? textures[k] : nil atIndex:range.location + k];
}
- (void)setTileSamplerState:(id<MTLSamplerState>)sampler atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileSamplerState"); return; }
    if (!_tileSmp) _tileSmp = [NSMutableDictionary new];
    if (sampler) _tileSmp[@(index)] = @[sampler, [NSNull null], [NSNull null]]; else [_tileSmp removeObjectForKey:@(index)];
}
- (void)setTileSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])samplers withRange:(NSRange)range {
    for (NSUInteger k = 0; k < range.length; k++) [self setTileSamplerState:samplers ? samplers[k] : nil atIndex:range.location + k];
}
- (void)setTileSamplerState:(id<MTLSamplerState>)sampler lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileSamplerState"); return; }
    if (!_tileSmp) _tileSmp = [NSMutableDictionary new];
    if (sampler) _tileSmp[@(index)] = @[sampler, @(lo), @(hi)]; else [_tileSmp removeObjectForKey:@(index)];
}
- (void)setTileSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])samplers lodMinClamps:(const float [])lo lodMaxClamps:(const float [])hi withRange:(NSRange)range {
    for (NSUInteger k = 0; k < range.length; k++) [self setTileSamplerState:samplers ? samplers[k] : nil lodMinClamp:lo ? lo[k] : 0 lodMaxClamp:hi ? hi[k] : FLT_MAX atIndex:range.location + k];
}
- (void)setTileVisibleFunctionTable:(id<MTLVisibleFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileVisibleFunctionTable"); return; }
    if (!_tileVft) _tileVft = [NSMutableDictionary new];
    if (t) _tileVft[@(i)] = t; else [_tileVft removeObjectForKey:@(i)];
}
- (void)setTileVisibleFunctionTables:(const id<MTLVisibleFunctionTable> __unsafe_unretained [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileVisibleFunctionTable:ts ? ts[k] : nil atBufferIndex:r.location + k];
}
- (void)setTileIntersectionFunctionTable:(id<MTLIntersectionFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileIntersectionFunctionTable"); return; }
    if (!_tileIft) _tileIft = [NSMutableDictionary new];
    if (t) _tileIft[@(i)] = t; else [_tileIft removeObjectForKey:@(i)];
}
- (void)setTileIntersectionFunctionTables:(const id<MTLIntersectionFunctionTable> __unsafe_unretained [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileIntersectionFunctionTable:ts ? ts[k] : nil atBufferIndex:r.location + k];
}
- (void)setTileAccelerationStructure:(id<MTLAccelerationStructure>)a atBufferIndex:(NSUInteger)i {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("setTileAccelerationStructure"); return; }
    if (!_tileAs) _tileAs = [NSMutableDictionary new];
    if (a) _tileAs[@(i)] = a; else [_tileAs removeObjectForKey:@(i)];
}
// A tile function runs as a compute kernel between two render-pass segments (translator/src/native/tile_imageblock.rs): the pass so far
// is ended with every attachment kept, one threadgroup per tile runs over the render area with the colour attachments bound as storage
// images (the compute encoder's storage twins copy them in and out), and the pass resumes with those attachments loaded.
- (void)dispatchThreadsPerTile:(MTLSize)threadsPerTile {
    if (!nvmtl_tile_on()) { nvmtl_tile_ignored("dispatchThreadsPerTile"); return; }
    NVMTLRenderPipelineState *tp = _tilePs;
    if (!tp) { nvmtl_tile_ignored("dispatchThreadsPerTile with no tile pipeline set"); return; }
    const uint32_t tw = _tileW ? _tileW : 32, th = _tileH ? _tileH : 32, w = _pass.w, h = _pass.h;
    if (!w || !h || threadsPerTile.width * threadsPerTile.height * threadsPerTile.depth > 1024) { nvlog("dispatchThreadsPerTile: %ux%u area / %lu threads per tile - REFUSED", w, h, (unsigned long)(threadsPerTile.width * threadsPerTile.height * threadsPerTile.depth)); return; }
    NSString *why = nil; id<MTLComputePipelineState> cps = nvmtl_tile_compute_pipeline(tp, tw, th, &why);   // for the sample count the pipeline was made with
    if (!cps) { nvlog("dispatchThreadsPerTile: %s", why.UTF8String); return; }
    uint32_t keep = 0;
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        if (!_colTex[i]) continue;
        const uint32_t as = _colTex[i]->_i.samples > 1 ? _colTex[i]->_i.samples : 1u;
        if (as != tp->_tileSamples || (as > 1 ? _colTex[i]->_i.mtl_type != 4 : _colTex[i]->_i.mtl_type != 2)) {
            nvlog("dispatchThreadsPerTile: attachment %u has %u sample(s) and the tile pipeline was made for %u, or it is not a plain 2D texture - REFUSED", i, as, tp->_tileSamples); return; }
        keep |= NVMTL_LOAD_COLOUR(i);
    }
    if (_pass.ds) keep |= NVMTL_LOAD_DEPTH | (_ps && _ps->_p.has_stencil ? NVMTL_LOAD_STENCIL : 0u);
    if (_begun) {
        BOOL visibility = _visActive || _visPending;
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO; _visPending = visibility;
    } else if (nvmtl_vk_pass_kind(&_pass) == 2) {   // no draw yet: the pass's clears have to happen before the tile function reads the attachments
        if (nvmtl_vk_cmd_empty_pass(&_cb->_c, &_pass)) nvlog("dispatchThreadsPerTile: the pass's clears FAILED");
    }
    _pass.load |= keep; _load |= keep;
    NVMTLComputeCommandEncoder *ce = (NVMTLComputeCommandEncoder *)[_cb computeCommandEncoder];
    [ce setComputePipelineState:cps];
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) if (_colTex[i]) [ce setTexture:_colTex[i] atIndex:NVMTL_TILE_TEXTURE_BASE + i];
    for (NSNumber *k in _tileByt) { NSData *d = _tileByt[k]; [ce setBytes:d.bytes length:d.length atIndex:k.unsignedLongValue]; }
    for (NSNumber *k in _tileBuf) { NSArray *e = _tileBuf[k]; [ce setBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:k.unsignedLongValue]; }
    for (NSNumber *k in _tileTex) [ce setTexture:_tileTex[k] atIndex:k.unsignedLongValue];
    for (NSNumber *k in _tileTg) [ce setThreadgroupMemoryLength:[_tileTg[k] unsignedLongValue] atIndex:k.unsignedLongValue];
    for (NSNumber *k in _tileSmp) { NSArray *e = _tileSmp[k]; if ([e[1] isKindOfClass:[NSNumber class]]) [ce setSamplerState:e[0] lodMinClamp:[e[1] floatValue] lodMaxClamp:[e[2] floatValue] atIndex:k.unsignedLongValue]; else [ce setSamplerState:e[0] atIndex:k.unsignedLongValue]; }
    for (NSNumber *k in _tileVft) [ce setVisibleFunctionTable:_tileVft[k] atBufferIndex:k.unsignedLongValue];
    for (NSNumber *k in _tileIft) [ce setIntersectionFunctionTable:_tileIft[k] atBufferIndex:k.unsignedLongValue];
    for (NSNumber *k in _tileAs) [ce setAccelerationStructure:_tileAs[k] atBufferIndex:k.unsignedLongValue];
    [ce dispatchThreadgroups:MTLSizeMake((w + tw - 1) / tw, (h + th - 1) / th, 1) threadsPerThreadgroup:threadsPerTile];
    [ce endEncoding];
    if (_ps) [self setRenderPipelineState:_ps];   // the pass resumes with the attachments loaded
    nvlog("dispatchThreadsPerTile: %ux%u area, %ux%u tiles, %lu threads per tile", w, h, tw, th, (unsigned long)(threadsPerTile.width * threadsPerTile.height));
}
- (void)setThreadgroupMemoryLength:(NSUInteger)length offset:(NSUInteger)offset atIndex:(NSUInteger)index {
    if (!nvmtl_tile_on() || offset) { nvmtl_tile_ignored("setThreadgroupMemoryLength"); return; }
    if (!_tileTg) _tileTg = [NSMutableDictionary new];
    _tileTg[@(index)] = @(length);
}
- (unsigned long long)globalTraceObjectID { return nvmtl_global_trace_id(self); }
static NSMutableDictionary *g_sampvar; static pthread_mutex_t g_sampvar_lock = PTHREAD_MUTEX_INITIALIZER;
extern unsigned char *CC_SHA256(const void *data, uint32_t len, unsigned char *md);
static char kNVMTLSampvarAir;
static NSString *nvmtl_air_digest(NVMTLFunction *f) {
    if (!f) return @"-";
    NSString *h = objc_getAssociatedObject(f, &kNVMTLSampvarAir); if (h) return h;
    NSData *u = [f->_air dataUsingEncoding:NSUTF8StringEncoding]; unsigned char md[32];
    CC_SHA256(u.bytes, (uint32_t)u.length, md);
    NSMutableString *x = [NSMutableString stringWithCapacity:64]; for (int i = 0; i < 32; i++) [x appendFormat:@"%02x", md[i]];
    objc_setAssociatedObject(f, &kNVMTLSampvarAir, x, OBJC_ASSOCIATION_RETAIN_NONATOMIC); return x;
}
static NSString *nvmtl_sampvar_key(MTLRenderPipelineDescriptor *d, NVMTLFunction *vf, NVMTLFunction *ff, NSData *skey) {
    static int off = -1; if (off < 0) off = getenv("NVMTL_NO_SAMPVAR") != NULL;
    if (off || !d || !vf || !vf->_air || (ff && !ff->_air)) return nil;
    MTLRenderPipelineDescriptor *c = [d copy]; c.vertexFunction = nil; c.fragmentFunction = nil; c.label = @"";
    for (MTLLinkedFunctions *lf in @[c.vertexLinkedFunctions ?: (id)[NSNull null], c.fragmentLinkedFunctions ?: (id)[NSNull null]]) {
        if ((id)lf == [NSNull null]) continue;
        if (lf.functions.count || lf.binaryFunctions.count || lf.groups.count || lf.privateFunctions.count) return nil;
    }
    NSString *ds = c.description; NSRange nl = [ds rangeOfString:@"\n"];
    if (nl.location != NSNotFound) ds = [ds substringFromIndex:nl.location];
    static NSRegularExpression *addr; static dispatch_once_t once;
    dispatch_once(&once, ^{ addr = [NSRegularExpression regularExpressionWithPattern:@"0x[0-9a-fA-F]+" options:0 error:NULL]; });
    ds = [addr stringByReplacingMatchesInString:ds options:0 range:NSMakeRange(0, ds.length) withTemplate:@"@"];
    NSString *sk = [[NSString alloc] initWithData:skey encoding:NSUTF8StringEncoding] ?: @"";
    return [NSString stringWithFormat:@"%@|%@|%@|%@||%@|%@|%@|%@||%@||%@", nvmtl_air_digest(vf), vf->_fname, vf->_stage, vf->_fc ?: @{},
            nvmtl_air_digest(ff), ff ? ff->_fname : @"", ff ? ff->_stage : @"", ff ? (ff->_fc ?: @{}) : @{}, sk, ds];
}
static id nvmtl_sampvar_get(NSString *k) {
    if (!k) return nil;
    pthread_mutex_lock(&g_sampvar_lock); id e = g_sampvar[k]; pthread_mutex_unlock(&g_sampvar_lock);
    if (e) nvlog("sampvar: device cache HIT - a new pipeline state of the same shaders + samplers + descriptor reuses its variant");
    return e;
}
static void nvmtl_sampvar_put(NSString *k, id ps, NVMTLFunction *vf, NVMTLFunction *ff) {
    if (!k || !ps) return;
    pthread_mutex_lock(&g_sampvar_lock);
    if (!g_sampvar) g_sampvar = [NSMutableDictionary new];
    if (g_sampvar.count >= 1024) { [g_sampvar removeAllObjects]; nvlog("sampvar: device cache reached 1024 variants - cleared"); }
    g_sampvar[k] = ps;
    pthread_mutex_unlock(&g_sampvar_lock);
}
static NSDictionary *nvmtl_pixel_sampler_norm(NSDictionary *m) {
    static int off = -1; if (off < 0) off = getenv("NVMTL_NO_SAMPKEY") != NULL;
    if (off || !m.count) return m ?: @{};
    NSMutableDictionary *o = [NSMutableDictionary dictionaryWithCapacity:m.count];
    for (NSString *k in m) { NSMutableDictionary *st = [m[k] mutableCopy]; st[@"lod_max_clamp"] = @65504.0f; o[k] = st; }
    return o;
}
// The pixel-sampler tables are keyed by the slot number as a string. Building that string cost an NSNumber and an NSString
// on every sampler bind, ten percent of an OpenGL render thread in one profile; the sixteen slot keys are made once.
static NSString *nvmtl_sampler_slot_key(NSUInteger i) {
    static NSString *keys[16]; static dispatch_once_t once;
    dispatch_once(&once, ^{ for (NSUInteger k = 0; k < 16; k++) keys[k] = [NSString stringWithFormat:@"%lu", (unsigned long)k]; });
    return i < 16 ? keys[i] : [NSString stringWithFormat:@"%lu", (unsigned long)i];
}
static BOOL nvmtl_pixel_sampler_update(NSMutableDictionary *states, NSString *index, NSDictionary *state) {
    NSDictionary *old = states[index];
    if (old == state || [old isEqual:state]) return NO;
    if (state) states[index] = state; else [states removeObjectForKey:index];
    return YES;
}
- (NVMTLRenderPipelineState *)nvmtlSamplerPipeline {
    NVMTLRenderPipelineState *selected=_ps;
    {
        NSDictionary *vS=nil,*fS=nil;
        if (!_pixelSamplerKey) {
            NSDictionary *vertex=_vertexSamplers ?: @{}, *fragment=_fragmentSamplers ?: @{};
            for (NSArray *entry in _pixelSamplerKeys) {
                if ([entry[0] isEqual:vertex] && [entry[1] isEqual:fragment]) { _pixelSamplerKey=entry[2]; break; }
            }
            if (!_pixelSamplerKey) {
                vS=nvmtl_pixel_sampler_norm(vertex); fS=nvmtl_pixel_sampler_norm(fragment);
                _pixelSamplerKey=[NSJSONSerialization dataWithJSONObject:@[vS,fS] options:NSJSONWritingSortedKeys error:NULL];
                if (!_pixelSamplerKeys) _pixelSamplerKeys=[NSMutableArray new];
                if (_pixelSamplerKeys.count == 8) [_pixelSamplerKeys removeObjectAtIndex:0];
                [_pixelSamplerKeys addObject:@[[vertex copy],[fragment copy],_pixelSamplerKey]];
            }
        }
        NSData *key=_pixelSamplerKey;
        // The same pipeline with the same sampler values as this encoder's last draw: its choice stands, with no lock and no
        // lookup (ANGLE and GL renderers draw hundreds of times with one set). NVMTL_NO_SV_MEMO=1 looks it up every draw.
        static int svMemo = -1;
        if (svMemo < 0) svMemo = !nvmtl_switch_is_off(getenv("NVMTL_NO_SV_MEMO"));
        if (nvmtl_sv_memo_hit(svMemo, (__bridge const void *)_svFor, (__bridge const void *)_ps, (__bridge const void *)_svKey, (__bridge const void *)key, _svSel != nil)) return _svSel == (id)[NSNull null] ? nil : _svSel;
        _svFor=nil; _svKey=nil; _svSel=nil;
        @synchronized(_ps) {
            if(!_ps->_samplerVariants) _ps->_samplerVariants=[NSMutableDictionary new];
            id cached=_ps->_samplerVariants[key];
            if(cached==[NSNull null]) { _svFor=_ps; _svKey=key; _svSel=cached; return nil; }
            selected=cached;
            if(!selected) {
                // Reusing the key across pipelines still performs each pipeline's own variant lookup.
                if (!vS) vS=nvmtl_pixel_sampler_norm(_vertexSamplers);
                if (!fS) fS=nvmtl_pixel_sampler_norm(_fragmentSamplers);
                NVMTLFunction *vf=_ps->_linkedVert?:(id)_ps->_descriptor.vertexFunction,*ff=_ps->_linkedFrag?:(id)_ps->_descriptor.fragmentFunction;
                NSString *why=nil;
                NSString *gk=nvmtl_sampvar_key(_ps->_descriptor,vf,ff,key); selected=nvmtl_sampvar_get(gk);
                const uint32_t amp=nvmtl_mv_popcount(_ps->_p.view_mask);   // an amplified pipeline's variants stay multiview shaders
                NSData *v=selected?nil:nvmtl_translate_samplers_amp(vf,vS,0,amp,&why),*f=(selected||!v)?nil:(ff?nvmtl_translate_samplers_amp(ff,fS,_ps->_a8mask,amp,&why):[NSData data]);
                if(v && f) {
                    NVMTLFunction *vs=[NVMTLFunction new],*fs=ff?[NVMTLFunction new]:nil;
                    vs->_spirv=v;vs->_fname=vf->_fname;vs->_stage=vf->_stage;
                    if(fs){fs->_spirv=f;fs->_fname=ff->_fname;fs->_stage=ff->_stage;}
                    nvmtl_gl_uniform_inherit(vs, vf);
                    nvmtl_gl_uniform_inherit(fs, ff);
                    MTLRenderPipelineDescriptor *desc=[_ps->_descriptor copy];desc.vertexFunction=vs;desc.fragmentFunction=fs;
                    NSError *error=nil;selected=(id)[(id<MTLDevice>)gNVMTLMainDevice newRenderPipelineStateWithDescriptor:desc error:&error];
                    if(!selected)why=error.description;
                    else nvmtl_sampvar_put(gk,selected,vf,ff);
                }
                if(!selected){nvlog("pixel sampler specialization REFUSED %s: %s",_ps->_fname.UTF8String,why.UTF8String);_ps->_samplerVariants[key]=[NSNull null];_svFor=_ps;_svKey=key;_svSel=[NSNull null];return nil;}
                _ps->_samplerVariants[key]=selected;
                nvlog("pixel sampler variant built: %s / %s",_ps->_vname.UTF8String,_ps->_fname.UTF8String);
            }
        }
        _svFor=_ps; _svKey=key; _svSel=selected;
    }
    return selected;
}
- (void)setVertexAmplificationCount:(NSUInteger)count viewMappings:(const MTLVertexAmplificationViewMapping *)m {
    if (count > NVMTL_MV_HARD_MAX) count = NVMTL_MV_HARD_MAX;
    _ampCount = (uint32_t)count; _ampWhy = NULL;
    if (count < 2) return;
    if (!nvmtl_vk_multiview_views()) {
        static int said; if (!said++) nvlog("encoder: setVertexAmplificationCount:%lu with vertex amplification OFF (NVMTL_VERTEX_AMPLIFICATION=1 turns it on) - draws render one view", (unsigned long)count);
        return;
    }
    nvmtl_mv_mapping map[NVMTL_MV_HARD_MAX];
    for (NSUInteger i = 0; m && i < count; i++) map[i] = (nvmtl_mv_mapping){ (uint32_t)m[i].renderTargetArrayIndexOffset, (uint32_t)m[i].viewportArrayIndexOffset };
    _ampWhy = nvmtl_mv_mappings_refusal((uint32_t)count, m ? map : NULL);
    if (_ampWhy) { static int said; if (said++ < 8) nvlog("encoder: vertex amplification view mappings not carried: %s - amplified draws are refused", _ampWhy); }
}
- (BOOL)nvmtlBindSamplerPipeline {
    if (!_ps) return NO;
    if (_ps->_p.view_mask) {   // an amplified pipeline: the encoder must have asked for the views it renders
        const char *why = _ampWhy ?: nvmtl_mv_draw_refusal(_ps->_p.view_mask, _ampCount ? _ampCount : 1u);
        if (why) { static int said; if (said++ < 16) nvlog("encoder: amplified draw REFUSED (pipeline views 0x%x, encoder count %u): %s", _ps->_p.view_mask, _ampCount, why); return NO; }
    }
    if (_cb->_c.hz_draw_due || _cb->_c.hz_wm_dirty) {   // hazard model: only a draw with something new is checked
      const int hzr = nvmtl_hz_draw(&_cb->_c);   // 1: restart behind a barrier; 2: a restart storm, keep a render pass
      if (hzr == 1) [self nvmtlHzRestart:NO]; else if (hzr == 2) [self nvmtlHzRestart:YES];
      if (hzr && !_begun) return NO; }
    NVMTLRenderPipelineState *selected=_ps;
    if (_vertexSamplers.count || _fragmentSamplers.count) {
        selected=[self nvmtlSamplerPipeline];
        if (!selected) return NO;
    }
    if(!_cb->_resources)_cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:selected];
    if (selected->_p.fetch_copy && _begun && ![self nvmtlFetchCopy:&selected->_p]) return NO;
    nvmtl_vk_cmd_bind_pipeline(&_cb->_c,&selected->_p);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX,   (NVMTLFunction *)_ps->_descriptor.vertexFunction,   _vbufAt, _vbufOff);
    nvmtl_bind_embedded(_cb, NVMTL_SET_FRAGMENT, (NVMTLFunction *)_ps->_descriptor.fragmentFunction, _fbufAt, _fbufOff);
    nvmtl_vk_cmd_bind_set(&_cb->_c,&selected->_p);
    return YES;
}
- (BOOL)nvmtlSamplePositionError:(NSString *)message {
    _refused = YES;
    nvlog("%s", message.UTF8String);
    [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain
        code:MTLCommandBufferErrorInternal userInfo:@{NSLocalizedDescriptionKey:message}]];
    return NO;
}
- (BOOL)nvmtlBuildPass:(MTLRenderPassDescriptor *)d {
    NVMTLCommandBuffer *cb = _cb;
    memset(&_pass, 0, sizeof _pass); cb->_c.depth_view = NULL; _hzAttSkip = 0;
    for (int i = 0; i < NVMTL_NCOL; i++) _colTex[i] = nil;
    _tileW = (uint32_t)d.tileWidth; _tileH = (uint32_t)d.tileHeight; _tilePs = nil;
    MTLSamplePosition sample[8];
    NSUInteger sampleCount = [d getSamplePositions:NULL count:0];
    if (sampleCount && sampleCount <= 8) [d getSamplePositions:sample count:sampleCount];
    if (sampleCount == 1) return [self nvmtlSamplePositionError:@"sample positions: custom single-sample pattern is not a Metal descriptor contract"];
    if (sampleCount > 8) return [self nvmtlSamplePositionError:@"sample positions: more than eight positions; pass refused"];
    if (sampleCount) {
        _pass.sample_pattern.count = (uint32_t)sampleCount;
        for (NSUInteger i = 0; i < sampleCount; ++i)
            _pass.sample_pattern.position[i] = (nvmtl_sample_position){ sample[i].x, sample[i].y };
        if (!nvmtl_sample_pattern_valid(&_pass.sample_pattern)) return [self nvmtlSamplePositionError:@"sample positions: invalid positions; pass refused"];
    }
    _pass.depth_store_options = (uint32_t)d.depthAttachment.storeActionOptions;
    if (d.depthAttachment.storeAction == MTLStoreActionCustomSampleDepthStore)
        _pass.depth_store_options |= MTLStoreActionOptionCustomSamplePositions;
    _pass.stencil_store_options = (uint32_t)d.stencilAttachment.storeActionOptions;
    if (d.stencilAttachment.texture && _pass.stencil_store_options)
        return [self nvmtlSamplePositionError:@"sample positions: stencil store options are illegal"];
    if ((_pass.depth_store_options | _pass.stencil_store_options) & ~MTLStoreActionOptionCustomSamplePositions)
        return [self nvmtlSamplePositionError:@"sample positions: unknown depth/stencil store options"];

    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    uint32_t load = 0, w = UINT32_MAX, h = UINT32_MAX; BOOL any = NO;
    const uint32_t NL = d.renderTargetArrayLength > 1 ? (uint32_t)d.renderTargetArrayLength : 1u;
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        MTLRenderPassColorAttachmentDescriptor *ca = d.colorAttachments[i];
        id<MTLTexture> tx = ca.texture, rx = ca.resolveTexture;
        if (!tx) continue;
        if (![(id)tx isKindOfClass:[NVMTLTexture class]]) { nvlog("encoder: apps2 colour %u is a %s, not our texture — pass REFUSED", i, object_getClassName(tx)); return NO; }
        NVMTLTexture *t = (NVMTLTexture *)tx; uint32_t cw = 0, ch = 0;
        _colTex[i] = t;
        uint32_t zslice = [t nvi]->mtl_type == 7 ? (uint32_t)ca.depthPlane : (uint32_t)ca.slice;
        if (ca.depthPlane && [t nvi]->mtl_type != 7) { static int said;
            if (said++ < 4) nvlog("encoder: apps2 colour %u sets depthPlane %lu on a texture of type %u, which is not 3D — depthPlane IGNORED, its slice %lu is used", i, (unsigned long)ca.depthPlane, [t nvi]->mtl_type, (unsigned long)ca.slice); }
        int ta8 = 0; uint32_t tvk = nvmtl_tex_attach_vkfmt(t, &ta8);
        if (nvmtl_vk_cmd_attachment_view_n(&cb->_c, [t nvi], 0, t->_baseLevel + (uint32_t)ca.level, t->_baseSlice + zslice, NL, tvk, ta8, &_pass.colview[i], &cw, &ch)) return NO;
        _pass.col[i] = [t nvi]; _pass.colw[i] = cw; _pass.colh[i] = ch;
        if (nvmtl_hz_untracked_tex(t)) _hzAttSkip |= 1u << i;
        _pass.col_level[i] = t->_baseLevel + (uint32_t)ca.level; _pass.col_layer[i] = t->_baseSlice + zslice;
        if (_pass.col_level[i]) (t->_parent ?: t)->_mipsWritten = YES;
        _pass.colfmt[i] = tvk;
        MTLClearColor cc = ca.clearColor;   // an A8 target keeps Metal's alpha in its red lane, its clear too
        _pass.clear[i][0] = ta8 ? (float)cc.alpha : (float)cc.red; _pass.clear[i][1] = (float)cc.green; _pass.clear[i][2] = (float)cc.blue; _pass.clear[i][3] = (float)cc.alpha;
        if (ta8 == 2) { _pass.clear[i][0] = (float)cc.alpha; _pass.clear[i][1] = (float)cc.red; _pass.clear[i][2] = (float)cc.green; _pass.clear[i][3] = (float)cc.blue; }   // ARGB8 stored as RGBA8
        if (nvmtl_load_keeps(ca.loadAction)) load |= NVMTL_LOAD_COLOUR(i);
        if (cw < w) w = cw;
        if (ch < h) h = ch;
        any = YES;
        if (i) {
            [cb->_resources addObject:t];
            NVMTLTexture *sr = t->_parent ? t->_parent : t;
            if (sr->_surf) {
                if (ca.loadAction == MTLLoadActionClear) nvmtl_surface_overwrite_recorded(cb, sr);
                else nvmtl_surface_in_for_cb(cb, sr);
                nvmtl_surf_dirty(cb, sr); }
        }
        MTLStoreAction sa = ca.storeAction;
        if ((sa == MTLStoreActionMultisampleResolve || sa == MTLStoreActionStoreAndMultisampleResolve || sa == MTLStoreActionUnknown) && rx) {
            if (![(id)rx isKindOfClass:[NVMTLTexture class]]) { nvlog("encoder: apps2 resolve %u is a %s, not our texture — pass REFUSED", i, object_getClassName(rx)); return NO; }
            NVMTLTexture *r = (NVMTLTexture *)rx; uint32_t rw = 0, rh = 0;
            int ra8 = 0; uint32_t rvk = nvmtl_tex_attach_vkfmt(r, &ra8);
            if (nvmtl_vk_cmd_attachment_view_n(&cb->_c, [r nvi], 0, r->_baseLevel + (uint32_t)ca.resolveLevel, r->_baseSlice + (uint32_t)ca.resolveSlice, NL, rvk, ra8, &_pass.resview[i], &rw, &rh)) return NO;
            _pass.res[i] = [r nvi]; _pass.resw[i] = rw; _pass.resh[i] = rh;
            _pass.resfmt[i] = rvk;
            if (i) { [cb->_resources addObject:r]; NVMTLTexture *rs = r->_parent ? r->_parent : r; if (rs->_surf) nvmtl_surf_dirty(cb, rs); }
        }
    }
    MTLRenderPassDepthAttachmentDescriptor *da = d.depthAttachment; MTLRenderPassStencilAttachmentDescriptor *sa = d.stencilAttachment;
    id<MTLTexture> dtx = da.texture ? da.texture : sa.texture;
    if (dtx) {
        if (![(id)dtx isKindOfClass:[NVMTLTexture class]]) { nvlog("encoder: apps2 depth/stencil is a %s, not our texture — pass REFUSED", object_getClassName(dtx)); return NO; }
        if (da.texture && sa.texture && da.texture != sa.texture) { static int said; if (!said++) nvlog("encoder: apps2 separate depth and stencil textures — the stencil texture is NOT attached (one depth/stencil attachment)"); }
        MTLRenderPassAttachmentDescriptor *which = da.texture ? (MTLRenderPassAttachmentDescriptor *)da : (MTLRenderPassAttachmentDescriptor *)sa;
        NVMTLTexture *dt = (NVMTLTexture *)dtx; uint32_t dw = 0, dh = 0;
        if (nvmtl_vk_cmd_attachment_view_n(&cb->_c, [dt nvi], 1, dt->_baseLevel + (uint32_t)which.level, dt->_baseSlice + (uint32_t)which.slice, NL, 0, 0, &_pass.dsview, &dw, &dh)) return NO;
        {
            MTLStoreAction dsa = da.storeAction, ssa = sa.storeAction;
            BOOL dr = da.texture && da.resolveTexture && (dsa == MTLStoreActionMultisampleResolve || dsa == MTLStoreActionStoreAndMultisampleResolve || dsa == MTLStoreActionUnknown);
            BOOL sr = sa.texture && sa.resolveTexture && (ssa == MTLStoreActionMultisampleResolve || ssa == MTLStoreActionStoreAndMultisampleResolve || ssa == MTLStoreActionUnknown);
            id<MTLTexture> drx = dr ? da.resolveTexture : sr ? sa.resolveTexture : nil;
            if (drx && [(id)drx isKindOfClass:[NVMTLTexture class]]) {
                NVMTLTexture *r = (NVMTLTexture *)drx; uint32_t rw = 0, rh = 0;
                MTLRenderPassAttachmentDescriptor *rw_ = dr ? (MTLRenderPassAttachmentDescriptor *)da : (MTLRenderPassAttachmentDescriptor *)sa;
                if (nvmtl_vk_cmd_attachment_view_n(&cb->_c, [r nvi], 1, r->_baseLevel + (uint32_t)rw_.resolveLevel, r->_baseSlice + (uint32_t)rw_.resolveSlice, NL, 0, 0, &_pass.dresview, &rw, &rh)) return NO;
                _pass.dres = [r nvi];
                _pass.dresmode = dr ? 1u + (uint32_t)da.depthResolveFilter : 0u;
                _pass.sresmode = sr ? 1u + (uint32_t)sa.stencilResolveFilter : 0u;
                [cb->_resources addObject:r];
            } else if (drx) { nvlog("encoder: par1 depth/stencil resolve texture is a %s, not our texture — pass REFUSED", object_getClassName(drx)); return NO; }
        }
        _pass.ds = [dt nvi];
        if (nvmtl_hz_untracked_tex(dt)) _hzAttSkip |= 1u << 8;
        _pass.ds_level = dt->_baseLevel + (uint32_t)which.level;
        _pass.ds_layer = dt->_baseSlice + (uint32_t)which.slice;
        if (sa.texture) [cb->_resources addObject:sa.texture];
        if (dw < w) w = dw;
        if (dh < h) h = dh;
        any = YES;
        if (da.texture && nvmtl_load_keeps(da.loadAction)) load |= NVMTL_LOAD_DEPTH;
        if (sa.texture && nvmtl_load_keeps(sa.loadAction)) load |= NVMTL_LOAD_STENCIL;
    }
    _pass.clear_depth = (float)da.clearDepth; _pass.clear_stencil = sa.clearStencil;
    if (!any) {
        // A pass with no attachments: the fragment shader writes through images or buffers only, sized by the descriptor's
        // renderTargetWidth/Height (Blender Eevee's shadow pages, 8192x8192 with image atomics). Vulkan draws it as a render
        // pass with no attachments and a framebuffer of that size. Refusing it crashed Blender on its first shadowed frame
        // (#152). Metal requires the size for such a pass, so a pass without one is still refused.
        if (!d.renderTargetWidth || !d.renderTargetHeight) {
            nvlog("encoder: apps2 a render pass with no attachments and no renderTargetWidth/Height — pass REFUSED"); return NO; }
        w = (uint32_t)d.renderTargetWidth; h = (uint32_t)d.renderTargetHeight;
        static int said; if (said++ < 4) nvlog("encoder: render pass with no attachments, %ux%u (writes through images/buffers only)", w, h);
    }
    if (d.renderTargetWidth && d.renderTargetWidth < w) w = (uint32_t)d.renderTargetWidth;
    if (d.renderTargetHeight && d.renderTargetHeight < h) h = (uint32_t)d.renderTargetHeight;
    _pass.load = _load = load; _pass.w = w; _pass.h = h; _pass.layers = NL;
    { static uint32_t seen[16]; static unsigned nseen;
      const uint32_t shape = (uint32_t)d.colorAttachments[0].loadAction
                           | ((uint32_t)(da.texture ? da.loadAction + 1 : 0) << 4)
                           | ((uint32_t)(sa.texture ? sa.loadAction + 1 : 0) << 8)
                           | (load << 12);
      unsigned k = 0; for (; k < nseen; k++) if (seen[k] == shape) break;
      if (k == nseen && nseen < 16) {
        seen[nseen++] = shape;
        nvlog("encoder: dontcare pass shape — colour0 load action %lu, depth %s action %lu, stencil %s action %lu, load bits 0x%x (DontCare %s)",
              (unsigned long)d.colorAttachments[0].loadAction,
              da.texture ? "present" : "absent", (unsigned long)da.loadAction,
              sa.texture ? "present" : "absent", (unsigned long)sa.loadAction,
              load, nvmtl_dontcare_clears() ? "CLEARS (old behaviour)" : "keeps, as on Apple");
      } }
    cb->_c.depth_view = _pass.dsview;
    return YES;
}
- (BOOL)nvmtlBeginPassLoad:(uint32_t)load {
    if (_refused || !_ps) return NO;
    uint32_t keep = _pass.load; _pass.load = load;
    int rc = nvmtl_vk_cmd_begin_pass(&_cb->_c, &_pass, &_ps->_p);
    _pass.load = keep;
    return rc == 0;
}
- (void)nvmtlApplyDepthStencil {
    if (!_begun || !_ps || !_ps->_p.has_depth) return;
    nvmtl_vk_cmd_set_depth(&_cb->_c, _ds ? 1 : 0, _ds ? _ds->_write : 0, _ds ? _ds->_compare : 7);
    // Every encoder starts with the depth bounds test off, as on Apple; the pipeline holds it as dynamic state, which
    // must be set before the first draw, so it is written here at pass begin even when the app never asks for it.
    if (nvmtl_vk_cmd_set_depth_bounds(&_cb->_c, _dbOn, _dbLo, _dbHi) && _dbOn) {
        static int said; if (!said++) nvlog("setDepthTestMinBound:maxBound: REFUSED - this device has no depthBounds feature, the test is not applied"); }
    if (!_ps->_p.has_stencil) return;
    static const uint32_t off[6] = { 7, 0, 0, 0, 0xff, 0xff };
    nvmtl_vk_cmd_set_stencil(&_cb->_c, _ds && _ds->_stencil, _ds ? _ds->_sf : off, _ds ? _ds->_sb : off);
    if (!_hasRef) { nvmtl_vk_cmd_set_stencil_ref(&_cb->_c, 0, 0); _hasRef = YES; }
}
// Metal draws through the ATTACHMENT's format, not the pipeline's: a pipeline declared BGRA8 that renders into an RGBA8
// texture (or the reverse) writes the colour the shader returned into the texture's own channel order and reports no error
// (measured on Apple M1 Pro: a BGRA8 pipeline into an RGBA8 texture stores 255,128,64,255 for float4(1,.5,.25,1), exactly as
// an RGBA8 pipeline does). Vulkan builds a pass in the pipeline's format, so the pass was refused and the draws dropped.
// Cyberpunk with frame generation on asks for an RGBA8 back buffer while its pipelines are BGRA8. The pipeline is rebuilt
// from its own descriptor with the channel-swapped format the attachment really has, once per pipeline.
static uint32_t nvmtl_swizzle_twin_vk(uint32_t f) { return nvmtl_format_twin_vk(f); }
static MTLPixelFormat nvmtl_swizzle_twin_mtl(MTLPixelFormat f) { return (MTLPixelFormat)nvmtl_format_twin_mtl((uint32_t)f); }
- (NVMTLRenderPipelineState *)nvmtlColourTwinPipeline:(NVMTLRenderPipelineState *)p {
    // NVMTL_FORMAT_TWIN=0 restores the old refusal of a pass whose attachment format is not the pipeline's (an A/B switch).
    static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_FORMAT_TWIN"); on = !(e && e[0] == '0'); if (!on) nvlog("pipelines are NOT rebuilt for an attachment's channel order (NVMTL_FORMAT_TWIN=0)"); }
    if (!on || !p || !p->_descriptor) return p;
    const uint32_t n = p->_p.ncol ? p->_p.ncol : (p->_p.cfmt ? 1u : 0u);
    uint32_t swap = 0;
    for (uint32_t i = 0; i < n && i < NVMTL_NCOL; i++) {
        const uint32_t pf = p->_p.ncol ? p->_p.cfmts[i] : p->_p.cfmt;
        if (!pf || !_pass.col[i]) continue;
        const uint32_t af = _pass.colfmt[i] ? _pass.colfmt[i] : _pass.col[i]->fmt;
        if (af != pf && nvmtl_swizzle_twin_vk(pf) == af) swap |= 1u << i;
    }
    if (!swap) return p;
    NSString *key = [NSString stringWithFormat:@"colour-twin:%x", swap];
    @synchronized (p) {
        if (!p->_samplerVariants) p->_samplerVariants = [NSMutableDictionary new];
        id got = p->_samplerVariants[key];
        if (got == [NSNull null]) return p;
        if (got) return got;
        MTLRenderPipelineDescriptor *desc = [p->_descriptor copy];
        NVMTLFunction *vf = p->_linkedVert ?: (id)p->_descriptor.vertexFunction, *ff = p->_linkedFrag ?: (id)p->_descriptor.fragmentFunction;
        desc.vertexFunction = vf; desc.fragmentFunction = ff;
        for (uint32_t i = 0; i < n && i < NVMTL_NCOL; i++) if (swap & (1u << i)) {
            const MTLPixelFormat t = nvmtl_swizzle_twin_mtl(desc.colorAttachments[i].pixelFormat);
            if (t == MTLPixelFormatInvalid) { p->_samplerVariants[key] = [NSNull null]; return p; }
            desc.colorAttachments[i].pixelFormat = t;
        }
        NSError *error = nil;
        NVMTLRenderPipelineState *v = (NVMTLRenderPipelineState *)[(id<MTLDevice>)gNVMTLMainDevice newRenderPipelineStateWithDescriptor:desc error:&error];
        if (!v) { nvlog("pipeline %s / %s: no variant for the attachment's channel order (%s)", p->_vname.UTF8String ?: "?", p->_fname.UTF8String ?: "?", error.description.UTF8String ?: "?");
                  p->_samplerVariants[key] = [NSNull null]; return p; }
        p->_samplerVariants[key] = v;
        static _Atomic unsigned long made; const unsigned long k = atomic_fetch_add(&made, 1) + 1;
        if (!(k & (k - 1))) nvlog("pipeline %s / %s rebuilt for the attachment's channel order (colour mask 0x%x) [%lu made]", p->_vname.UTF8String ?: "?", p->_fname.UTF8String ?: "?", swap, k);
        return v;
    }
}
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)ps {
    ps = (id<MTLRenderPipelineState>)[self nvmtlColourTwinPipeline:(NVMTLRenderPipelineState *)ps];
    // The same pipeline again inside a begun pass changes nothing: Apple's OpenGL renderer sets it before every draw, so
    // GL games paid a full bind per draw (the GL-on-Metal per-draw overhead). Not inside an open pass-time pool.
    static int psoSkip = -1;   // NVMTL_NO_PSO_SKIP=1 (nvrm610.conf) sets the pipeline every time again, to A/B a glitch against the saving
    if (psoSkip < 0) psoSkip = !nvmtl_switch_is_off(getenv("NVMTL_NO_PSO_SKIP"));
    if (nvmtl_pso_skip(psoSkip, (__bridge const void *)ps, (__bridge const void *)_ps, _begun, _cb->_c.pt_open && _cb->_c.pt_pool)) return;
    // NVMTL_DUMP_PSO=<text>: the first encoder that sets a pipeline whose label contains it has the fragment buffers bound
    // at its end dumped, as NVMTL_DUMP_GROUP does, for apps that label pipelines but push no debug groups (diagnostic)
    // Only asked for when the switch is set: this runs on every pipeline bind, and an unconditional `ps.label` was a message
    // send plus a string retain/release per draw call in profiles of a game that never sets NVMTL_DUMP_PSO.
    { static const char *want; static int once; static _Atomic int armed; if (!once) { once = 1; want = getenv("NVMTL_DUMP_PSO"); }
      NSString *lbl = want ? ps.label : nil;
      if (want && !strcmp(want, "?") && lbl) { static NSMutableSet *seen; static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
          pthread_mutex_lock(&mu); if (!seen) seen = [NSMutableSet new]; BOOL n = ![seen containsObject:lbl]; if (n) [seen addObject:lbl];
          pthread_mutex_unlock(&mu); if (n) nvlog("NVMTL_DUMP_PSO: pipeline label %s", lbl.UTF8String); }
      if (want && *want && lbl && strstr(lbl.UTF8String ?: "", want) && !atomic_exchange(&armed, 1)) { _dumpPso = YES; _dumpName = [lbl copy]; } }
    { NVMTLRenderPipelineState *p_ = (NVMTLRenderPipelineState *)ps;
      if (p_ && nvmtl_trace_on()) {
          extern int nvmtl_vk_pipeline_stats(void *pipe, char *out, size_t n);
          uint32_t hv = 0, hf = 0;
          for (int s = 0; s < 2; s++) {
              id f = s ? p_->_descriptor.fragmentFunction : p_->_descriptor.vertexFunction;
              if (![f isKindOfClass:[NVMTLFunction class]]) continue;
              NSData *d = ((NVMTLFunction *)f)->_spirv; if (!d.length) continue;
              uint32_t h = 2166136261u; const uint8_t *b = d.bytes; for (NSUInteger i = 0; i < d.length; i++) { h ^= b[i]; h *= 16777619u; }
              if (s) hf = h; else hv = h;
              static NSMutableSet *saved; static os_unfair_lock sl = OS_UNFAIR_LOCK_INIT; BOOL fresh = NO;
              os_unfair_lock_lock(&sl); if (!saved) saved = [NSMutableSet new]; if (![saved containsObject:@(h)]) { [saved addObject:@(h)]; fresh = YES; } os_unfair_lock_unlock(&sl);
              if (fresh) { NSString *dir = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Caches/nvmtl/pstat"];
                  [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:nil];
                  NSString *pth = [dir stringByAppendingPathComponent:[NSString stringWithFormat:@"%08x.%s.spv", h, s ? "frag" : "vert"]];
                  if (![[NSFileManager defaultManager] fileExistsAtPath:pth]) [d writeToFile:pth atomically:YES]; }
          }
          char st[600]; nvmtl_vk_pipeline_stats(p_->_p.pipe, st, sizeof st);
          nvtrace("PSO %p %s / %s vs %08x fs %08x | %s", (__bridge void *)ps, p_->_vname.UTF8String ?: "?", p_->_fname.UTF8String ?: "?", hv, hf, st);
      } }
    NVMTLRenderPipelineState *p = (NVMTLRenderPipelineState *)ps;
    if (!p) { nvlog("encoder: nil render pipeline refused"); return; }
    if (p->_tileFn) { _tilePs = p; return; }   // a tile pipeline: dispatchThreadsPerTile runs it between render-pass segments
    if (_begun && !nvmtl_vk_pipeline_compatible(&_passSig, &p->_p)) {
        static int refuse = -1;   // NVMTL_NO_PASS_TRANSITION=1: the old refusal instead of the transition below (a split switch)
        if (refuse < 0) { const char *e = getenv("NVMTL_NO_PASS_TRANSITION"); refuse = e && e[0] == '1';
            if (refuse) nvlog("encoder: an incompatible render pipeline is REFUSED, not transitioned (NVMTL_NO_PASS_TRANSITION=1)"); }
        if (refuse) { nvlog("encoder: incompatible render pipeline refused before changing draw state"); return; }
        // Metal can omit unused attachments in a later pipeline. Vulkan requires a new compatible pass: end this one,
        // keep every attachment an earlier segment used, preserve the encoder's dynamic state and begin again below.
        // Refusing the pipeline instead dropped every draw until the next compatible one: Cyberpunk's parallel render
        // encoders replay thousands of such binds per frame (4,096+ refusals logged per run) and the frame lost them.
        uint32_t keep = 0;
        for (uint32_t i = 0; i < _passSig.ncol; i++)
            if (_passSig.cfmts[i]) keep |= NVMTL_LOAD_COLOUR(i);
        if (_passSig.has_depth) keep |= NVMTL_LOAD_DEPTH;
        if (_ps && _ps->_p.has_stencil) keep |= NVMTL_LOAD_STENCIL;
        BOOL visibility = _visActive || _visPending;
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO; _pass.load |= keep; _load |= keep; _visPending = visibility;
        static _Atomic unsigned long long transitions; const unsigned long long k = atomic_fetch_add(&transitions, 1) + 1;
        if (!(k & (k - 1))) nvlog("encoder: pipeline attachment transition #%llu preserves load mask 0x%x (pass %u colour, depth %u, %u samples -> pipeline %s / %s)",
                                 k, keep, _passSig.ncol, _passSig.has_depth, _passSig.samples, p->_vname.UTF8String ?: "?", p->_fname.UTF8String ?: "?");
    }
    if (_begun && p->_p.fetch_mask && nvmtl_hz_mode()) [self nvmtlHzForceFull];
    _ps = p;
    nvmtl_retain_resource(_cb, p);
    if (nvmtl_hz_mode()) nvmtl_hz_render_writes(_cb, p);
    if (!_begun) {
        if (_refused) return;
        _cb->_c.hz_force_full = _hzFull; _cb->_c.hz_att_skip = _hzAttSkip;
        if (nvmtl_vk_cmd_begin_pass(&_cb->_c, &_pass, &p->_p)) {
            nvlog("encoder: begin render FAILED — nothing is recorded for this pipeline");
            if (_pass.sample_pattern.count) [self nvmtlSamplePositionError:@"sample positions: cannot begin requested render pass"];
            return; }
        _begun = YES; [self nvmtlBeginVisibility]; _passFmt = p->_p.cfmt; _passDepth = p->_p.has_depth; _passSamples = p->_p.samples; nvmtl_vk_pipeline_sig(&p->_p, &_passSig);
        if (_resolve) { NVMTLTexture *rs = _resolve->_parent ? _resolve->_parent : _resolve; if (rs->_surf) nvmtl_surf_dirty(_cb, rs); }
        nvmtl_vk_cmd_set_cull(&_cb->_c, _cull);
        if (_hasVP) nvmtl_vk_cmd_set_viewport(&_cb->_c, _vp[0], _vp[1], _vp[2], _vp[3], _vp[4], _vp[5]);
        if (_hasSC) nvmtl_vk_cmd_set_scissor(&_cb->_c, (int32_t)_sc[0], (int32_t)_sc[1], _sc[2], _sc[3]);
        [self nvmtlApplyDepthStencil];
    } else if (!nvmtl_vk_pipeline_compatible(&_passSig, &p->_p)) {
        static int said; if (said++ < 8) nvlog("G12 setRenderPipelineState: pipeline (fmt %u depth %d) does NOT match the pass in progress "
            "(fmt %u depth %d) — NOT bound, the earlier pipeline keeps drawing", p->_p.cfmt, p->_p.has_depth, _passFmt, _passDepth);
    } else {
        nvmtl_vk_cmd_bind_pipeline(&_cb->_c, &p->_p);
        static int told; if (!told++) nvlog("G12 a second pipeline was bound inside one encoder (first time in this process)");
    }
    if (_cb->_c.pt_open && _cb->_c.pt_pool) {
        static const char kPtHash = 0; NSNumber *hn = objc_getAssociatedObject(p, &kPtHash);
        if (!hn) {
            uint32_t hv = 0, hf = 0;
            for (int s = 0; s < 2; s++) {
                id f = s ? p->_descriptor.fragmentFunction : p->_descriptor.vertexFunction;
                if (![f isKindOfClass:[NVMTLFunction class]]) continue;
                NSData *d = ((NVMTLFunction *)f)->_spirv; if (!d.length) continue;
                uint32_t h = 2166136261u; const uint8_t *b = d.bytes; for (NSUInteger i = 0; i < d.length; i++) { h ^= b[i]; h *= 16777619u; }
                if (s) hf = h; else hv = h;
                NSString *dir = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Caches/nvmtl/pstat"];
                NSString *pth = [dir stringByAppendingPathComponent:[NSString stringWithFormat:@"%08x.%s.spv", h, s ? "frag" : "vert"]];
                if (![[NSFileManager defaultManager] fileExistsAtPath:pth]) {
                    [[NSFileManager defaultManager] createDirectoryAtPath:dir withIntermediateDirectories:YES attributes:nil error:nil];
                    [d writeToFile:pth atomically:YES]; }
            }
            hn = @(((uint64_t)hv << 32) | hf);
            objc_setAssociatedObject(p, &kPtHash, hn, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            extern int nvmtl_vk_pipeline_stats(void *pipe, char *out, size_t n);
            char st[600]; nvmtl_vk_pipeline_stats(p->_p.pipe, st, sizeof st);
            nvlog("ptpso: vs %08x fs %08x | %s", hv, hf, st);
        }
        extern void nvmtl_vk_pt_note(nvk_cmdbuf *c, uint32_t fs);
        nvmtl_vk_pt_note(&_cb->_c, (uint32_t)(hn.unsignedLongLongValue & 0xffffffffu));
    }
}
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i {
    NVMTLBuffer *nb = (NVMTLBuffer *)b;
    if (!nb) { nvlog("setVertexBuffer: nil at index %lu", (unsigned long)i); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:nb];
    if (i < NVMTL_NBUFAT) { _vbufAt[i] = nb; _vbufOff[i] = off; }
    nvtrace_floats("VB", (unsigned long)i, (unsigned long)off, nb->_b.map, 24);
    if (nvmtl_hz_bind_buffer(&_cb->_c, nvmtl_hz_untracked_buf(nb), NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, off)) nvlog("setVertexBuffer: bind FAILED");
}
- (void)setVertexAccelerationStructure:(id<MTLAccelerationStructure>)as atBufferIndex:(NSUInteger)i { [self setVertexBuffer:(id<MTLBuffer>)as offset:0 atIndex:i];
    NVMTLAccelerationStructure *a = (NVMTLAccelerationStructure *)as;
    if (a && [(id)a isKindOfClass:[NVMTLAccelerationStructure class]] && a->_uid.buf && _cb) nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 31, &a->_uid, 0); }
- (void)setFragmentAccelerationStructure:(id<MTLAccelerationStructure>)as atBufferIndex:(NSUInteger)i { [self setFragmentBuffer:(id<MTLBuffer>)as offset:0 atIndex:i];
    NVMTLAccelerationStructure *a = (NVMTLAccelerationStructure *)as;
    if (a && [(id)a isKindOfClass:[NVMTLAccelerationStructure class]] && a->_uid.buf && _cb) nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_FRAGMENT, 31, &a->_uid, 0); }
- (void)setFragmentBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i {
    NVMTLBuffer *nb = (NVMTLBuffer *)b;
    if (!nb) { nvlog("setFragmentBuffer: nil at index %lu", (unsigned long)i); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:nb];
    if (i < NVMTL_NBUFAT) { _fbufAt[i] = nb; _fbufOff[i] = off; }
    nvtrace_floats("FB", (unsigned long)i, (unsigned long)off, nb->_b.map, 16);
    if (nvmtl_hz_bind_buffer(&_cb->_c, nvmtl_hz_untracked_buf(nb), NVMTL_SET_FRAGMENT, (uint32_t)i, &nb->_b, off)) nvlog("setFragmentBuffer: bind FAILED");
}
static NVMTLBuffer *nvmtl_bytes_ring(NVMTLCommandBuffer *cb, NSUInteger len, size_t *off);
- (void)nvmtlBindBytes:(const void *)bytes length:(NSUInteger)len atIndex:(NSUInteger)i set:(uint32_t)set {
    size_t roff = 0;
    NVMTLBuffer *nb = nvmtl_bytes_ring(_cb, len, &roff);
    const BOOL ring = nb != nil;
    if (!ring) {
        nb = [NVMTLBuffer new];
        if (nvmtl_vk_buffer_create(len < 16 ? 16 : len, 1, &nb->_b)) { nvlog("setBytes: buffer create FAILED"); return; }
    }
    if (nb->_b.map) memcpy((uint8_t *)nb->_b.map + roff, bytes, len);
    if (nvmtl_hz_bind_buffer(&_cb->_c, 1, set, (uint32_t)i, &nb->_b, roff)) nvlog("setBytes: bind FAILED");
    nvtrace_floats(set == NVMTL_SET_VERTEX ? "VBYTES" : "FBYTES", (unsigned long)i, 0, len >= 4 ? bytes : NULL, (int)(len / 4 > 24 ? 24 : len / 4));
    if (!ring) {
        if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
        [_cb->_scratch addObject:nb];
    }
    if (i < NVMTL_NBUFAT && set == NVMTL_SET_VERTEX)        { _vbufAt[i] = nb; _vbufOff[i] = roff; }
    else if (i < NVMTL_NBUFAT && set == NVMTL_SET_FRAGMENT) { _fbufAt[i] = nb; _fbufOff[i] = roff; }
}
- (void)setVertexBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlBindBytes:b length:l atIndex:i set:NVMTL_SET_VERTEX]; }
- (void)setFragmentBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlBindBytes:b length:l atIndex:i set:NVMTL_SET_FRAGMENT]; }
- (void)setVertexTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTexture *nt = (NVMTLTexture *)t;
    if (!nt) { nvlog("setVertexTexture: nil at index %lu", (unsigned long)i); return; }
    nvmtl_retain_resource(_cb, nt);
    nvmtl_surface_in_for_cb(_cb, nt);
    if (nt->_tbView) nvmtl_hz_bind_texel(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_hz_bind_texture(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, [nt nvsampleview])) nvlog("setVertexTexture: bind FAILED");
    nvmtl_hz_bind_storage(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, nt->_stex ? [nt nvview] : NULL);
    }
}
- (void)setFragmentSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)s;
    if (i >= 16 || nvmtl_vk_bind_sampler(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, ns ? &ns->_s : NULL)) {
        nvlog("setFragmentSamplerState: invalid index or bind failure"); return;
    }
    if (!_fragmentSamplers) _fragmentSamplers=[NSMutableDictionary new];
    NSString *key=nvmtl_sampler_slot_key(i);
    if (nvmtl_pixel_sampler_update(_fragmentSamplers,key,ns ? ns->_pixelState : nil)) _pixelSamplerKey=nil;
    nvtrace("FRAGMENT SAMPLER[%lu] pixel=%d bound=%d",(unsigned long)i,ns && ns->_pixelState!=nil,ns!=nil);
    if(ns) { if (!_cb->_resources) _cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:ns]; }
}
- (void)setVertexSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)s;
    if (i >= 16 || nvmtl_vk_bind_sampler(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, ns ? &ns->_s : NULL)) {
        nvlog("setVertexSamplerState: invalid index or bind failure"); return;
    }
    if (!_vertexSamplers) _vertexSamplers=[NSMutableDictionary new];
    NSString *key=nvmtl_sampler_slot_key(i);
    if (nvmtl_pixel_sampler_update(_vertexSamplers,key,ns ? ns->_pixelState : nil)) _pixelSamplerKey=nil;
    nvtrace("VERTEX SAMPLER[%lu] pixel=%d bound=%d",(unsigned long)i,ns && ns->_pixelState!=nil,ns!=nil);
    if(ns) { if (!_cb->_resources) _cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:ns]; }
}
- (void)setFragmentTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTexture *nt = (NVMTLTexture *)t;
    if (!nt) { nvlog("setFragmentTexture: nil at index %lu", (unsigned long)i); return; }
    nvmtl_retain_resource(_cb, nt);
    nvmtl_surface_cpu_dump(nt->_parent ? nt->_parent : nt);
    nvmtl_surface_in_for_cb(_cb, nt);
    { NVMTLTexture *rt = nt->_parent ? nt->_parent : nt; nvtrace("FTEX[%lu] %ux%u surf %u view %d", (unsigned long)i, [nt nvi]->w, [nt nvi]->h,
        rt->_surf ? (unsigned)IOSurfaceGetID(rt->_surf) : 0u, nt->_parent != nil); }
    if (nt->_tbView) nvmtl_hz_bind_texel(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_FRAGMENT, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_hz_bind_texture(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_FRAGMENT, (uint32_t)i, [nt nvsampleview])) nvlog("setFragmentTexture: bind FAILED");
    nvmtl_hz_bind_storage(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_FRAGMENT, (uint32_t)i, nt->_stex ? [nt nvview] : NULL);
    if (!nt->_stex && (nt.usage & MTLTextureUsageShaderWrite)) {
        // a fragment shader writing a texture that has no storage view writes the dummy: say which texture, once per shape
        // OpenGL rebinds every texture before each draw, and its textures carry ShaderWrite: this runs thousands of times
        // a frame, so the shape is a packed integer in a small table, with no string or object made per bind.
        static uint64_t seen[64]; static int nseen; static os_unfair_lock sl = OS_UNFAIR_LOCK_INIT;
        const uint64_t k = ((uint64_t)nt.pixelFormat << 40) ^ ((uint64_t)nt.usage << 8) ^ (uint64_t)nt.textureType ^ 0x8000000000000000ull;
        BOOL fresh = YES;
        os_unfair_lock_lock(&sl);
        for (int q = 0; q < nseen; q++) if (seen[q] == k) { fresh = NO; break; }
        if (fresh && nseen < 64) seen[nseen++] = k; else if (fresh) fresh = NO;   // a full table says nothing more
        os_unfair_lock_unlock(&sl);
        if (fresh) nvlog("setFragmentTexture: texture %lu is ShaderWrite but has no storage view (pixelFormat %lu, usage 0x%lx, type %lu, image storage %u) - a fragment write to it goes to the dummy",
                         (unsigned long)i, (unsigned long)nt.pixelFormat, (unsigned long)nt.usage, (unsigned long)nt.textureType, [nt nvi]->storage);
    }
    }
}
- (void)drawPrimitives:(MTLPrimitiveType)type vertexStart:(NSUInteger)start vertexCount:(NSUInteger)count
         instanceCount:(NSUInteger)instances baseInstance:(NSUInteger)baseInstance {
    if (!_begun) { nvlog("encoder: draw with no pipeline set"); return; }
    if (![self nvmtlBindSamplerPipeline]) return;
    nvtrace("DRAW type %u | vs %s | fs %s | blend %s", (unsigned)type, _ps ? _ps->_vname.UTF8String : "-", _ps ? _ps->_fname.UTF8String : "-", _ps ? _ps->_blend.UTF8String : "-");
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_); nvtrace("  BUFS%s", bufs_); }
    if (nvmtl_vk_cmd_set_topology(&_cb->_c, (unsigned)type)) return;
    if ([self nvmtlVertexInputs]) return;
    nvtrace("  direct start %lu count %lu instances %lu", (unsigned long)start, (unsigned long)count, (unsigned long)instances);
    nvmtl_vk_cmd_draw(&_cb->_c, (uint32_t)start, (uint32_t)count, (uint32_t)instances, (uint32_t)baseInstance);
}
- (int)nvmtlVertexInputs {
    if (_ps && _ps->_p.tess) { static int said; if (said++ < 8) nvlog("tess: a primitive draw on a tessellation pipeline (%s) — NOT drawn "
        "(Metal requires drawPatches:)", _ps->_vname.UTF8String ?: "?"); return -1; }
    if (!_ps || !_ps->_p.vin_mask) return 0;
    nvtrace("  VIN%s", _ps->_vdesc.UTF8String ?: " ?");
    int r = nvmtl_vk_cmd_vertex_inputs(&_cb->_c, &_ps->_p, _vstride, _vstrideSet);
    if (r) nvtrace("  NOT DRAWN: a vertex buffer the descriptor names is not bound, or its dynamic stride is unset or too large");
    return r;
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c instanceCount:(NSUInteger)i {
    [self drawPrimitives:t vertexStart:s vertexCount:c instanceCount:i baseInstance:0];
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c {
    [self drawPrimitives:t vertexStart:s vertexCount:c instanceCount:1 baseInstance:0];
}
- (void)setTessellationFactorBuffer:(id<MTLBuffer>)buffer offset:(NSUInteger)offset instanceStride:(NSUInteger)instanceStride {
    NVMTLBuffer *nb = (NVMTLBuffer *)buffer;
    if (nb && ![(id)nb isKindOfClass:[NVMTLBuffer class]]) { nvlog("setTessellationFactorBuffer: not our buffer (%s) — ignored", object_getClassName(buffer)); nb = nil; }
    if (nb) nvmtl_retain_resource(_cb, nb);
    _tfb = nb; _tfbOff = offset; _tfbStride = instanceStride;
}
- (void)setTessellationFactorScale:(float)scale { _tfScale = scale; _tfScaleSet = YES; }
- (BOOL)nvmtlTessAddress:(id<MTLBuffer>)buf offset:(NSUInteger)off align:(uint64_t)al what:(const char *)what out:(uint64_t *)out {
    *out = 0; if (!buf) return YES;
    if (![(id)buf isKindOfClass:[NVMTLBuffer class]]) { nvlog("tess draw: the %s is not our buffer (%s) — NOT drawn", what, object_getClassName(buf)); return NO; }
    NVMTLBuffer *nb = (NVMTLBuffer *)buf;
    if (off >= nb->_b.size) { nvlog("tess draw: %s offset %lu is outside its %zu bytes — NOT drawn", what, (unsigned long)off, nb->_b.size); return NO; }
    nvmtl_retain_resource(_cb, nb);
    uint64_t a = nvmtl_vk_buffer_address(&nb->_b);
    if (!a) { nvlog("tess draw: the %s has no device address — NOT drawn", what); return NO; }
    a += off;
    if (a & (al - 1)) { nvlog("tess draw: the %s at offset %lu is not %llu-byte aligned — NOT drawn", what, (unsigned long)off, (unsigned long long)al); return NO; }
    *out = a; return YES;
}
- (void)nvmtlPatches:(NSUInteger)n start:(NSUInteger)start count:(NSUInteger)count pib:(id<MTLBuffer>)pib pibOff:(NSUInteger)pibOff
                cpib:(id<MTLBuffer>)cpib cpibOff:(NSUInteger)cpibOff indexed:(BOOL)indexed instances:(NSUInteger)instances
                base:(NSUInteger)base indirect:(id<MTLBuffer>)ind indOff:(NSUInteger)indOff {
    if (!_begun) { nvlog("encoder: patch draw with no pipeline set"); return; }
    [self nvmtlHzForceFull];
    if (!_ps || !_ps->_p.tess) { nvlog("tess draw: the pipeline (%s) has no post-tessellation vertex function — NOT drawn", _ps ? _ps->_vname.UTF8String : "-"); return; }
    if (n != _ps->_p.tess_n) { nvlog("tess draw: %lu control points per patch, %s takes %u — NOT drawn", (unsigned long)n, _ps->_vname.UTF8String ?: "?", _ps->_p.tess_n); return; }
    if (!_tfb) { nvlog("tess draw: no tessellation factor buffer is set — NOT drawn"); return; }
    if (indexed != (_ps->_p.tess_cpidx != 0)) { nvlog("tess draw: a%s draw on a pipeline whose tessellationControlPointIndexType is %u — NOT drawn",
        indexed ? "n indexed" : " non-indexed", _ps->_p.tess_cpidx); return; }
    if (indexed && !cpib) { nvlog("tess draw: drawIndexedPatches: with no control-point index buffer — NOT drawn"); return; }
    if (_tfbStride & 3) { nvlog("tess draw: factor instanceStride %lu is not a multiple of 4 — NOT drawn", (unsigned long)_tfbStride); return; }
    uint64_t fac = 0, cpx = 0, pix = 0, ia = 0;
    if (![self nvmtlTessAddress:_tfb offset:_tfbOff align:4 what:"tessellation factor buffer" out:&fac] ||
        ![self nvmtlTessAddress:pib offset:pibOff align:4 what:"patch index buffer" out:&pix] ||
        ![self nvmtlTessAddress:cpib offset:cpibOff align:(_ps->_p.tess_cpidx == 1 ? 2 : 4) what:"control-point index buffer" out:&cpx] ||
        ![self nvmtlTessAddress:ind offset:indOff align:4 what:"indirect patch-draw buffer" out:&ia]) return;
    if (![self nvmtlBindSamplerPipeline]) return;
    nvtrace("DRAW patches %lu cp | vs %s | fs %s | blend %s", (unsigned long)n, _ps->_vname.UTF8String, _ps->_fname.UTF8String, _ps->_blend.UTF8String);
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_); nvtrace("  BUFS%s", bufs_); }
    if (ind) nvtrace("  indirect patches @%lu, pib %s", (unsigned long)indOff, pib ? "yes" : "no");
    else nvtrace("  patches start %lu count %lu instances %lu base %lu, pib %s", (unsigned long)start, (unsigned long)count,
                 (unsigned long)instances, (unsigned long)base, pib ? "yes" : "no");
    if (nvmtl_vk_cmd_draw_patches(&_cb->_c, &_ps->_p, fac, (uint32_t)_tfbStride, _tfScaleSet ? _tfScale : 1.0f, cpx, pix, (uint32_t)start,
                                  (uint32_t)count, (uint32_t)instances, (uint32_t)base, ind ? &((NVMTLBuffer *)ind)->_b : NULL, indOff))
        nvtrace("  NOT DRAWN: a vertex buffer the tessellation pipeline pulls is not bound");
}
- (void)drawPatches:(NSUInteger)n patchStart:(NSUInteger)start patchCount:(NSUInteger)count patchIndexBuffer:(id<MTLBuffer>)pib
    patchIndexBufferOffset:(NSUInteger)pibOff instanceCount:(NSUInteger)instances baseInstance:(NSUInteger)base {
    [self nvmtlPatches:n start:start count:count pib:pib pibOff:pibOff cpib:nil cpibOff:0 indexed:NO instances:instances base:base indirect:nil indOff:0];
}
- (void)drawPatches:(NSUInteger)n patchIndexBuffer:(id<MTLBuffer>)pib patchIndexBufferOffset:(NSUInteger)pibOff
     indirectBuffer:(id<MTLBuffer>)ind indirectBufferOffset:(NSUInteger)indOff {
    if (!ind) { nvlog("drawPatches:indirectBuffer: nil"); return; }
    [self nvmtlPatches:n start:0 count:0 pib:pib pibOff:pibOff cpib:nil cpibOff:0 indexed:NO instances:0 base:0 indirect:ind indOff:indOff];
}
- (void)drawIndexedPatches:(NSUInteger)n patchStart:(NSUInteger)start patchCount:(NSUInteger)count patchIndexBuffer:(id<MTLBuffer>)pib
    patchIndexBufferOffset:(NSUInteger)pibOff controlPointIndexBuffer:(id<MTLBuffer>)cpib controlPointIndexBufferOffset:(NSUInteger)cpibOff
             instanceCount:(NSUInteger)instances baseInstance:(NSUInteger)base {
    [self nvmtlPatches:n start:start count:count pib:pib pibOff:pibOff cpib:cpib cpibOff:cpibOff indexed:YES instances:instances base:base indirect:nil indOff:0];
}
- (void)drawIndexedPatches:(NSUInteger)n patchIndexBuffer:(id<MTLBuffer>)pib patchIndexBufferOffset:(NSUInteger)pibOff
    controlPointIndexBuffer:(id<MTLBuffer>)cpib controlPointIndexBufferOffset:(NSUInteger)cpibOff
             indirectBuffer:(id<MTLBuffer>)ind indirectBufferOffset:(NSUInteger)indOff {
    if (!ind) { nvlog("drawIndexedPatches:indirectBuffer: nil"); return; }
    [self nvmtlPatches:n start:0 count:0 pib:pib pibOff:pibOff cpib:cpib cpibOff:cpibOff indexed:YES instances:0 base:0 indirect:ind indOff:indOff];
}
- (void)nvmtlVisibilityError:(NSString *)message {
    nvlog("%s", message.UTF8String);
    [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain
        code:MTLCommandBufferErrorInternal userInfo:@{NSLocalizedDescriptionKey: message}]];
}
- (void)nvmtlCloseVisibility {
    if (_visActive) {
        nvmtl_vk_cmd_end_occlusion(&_cb->_c, _visSlot);
        if (!_visRecords) _visRecords = [NSMutableArray new];
        [_visRecords addObject:@[@(_visSlot), @(_visOffset)]];
        _visActive = NO;
    } else if (_visPending) {
        if (!_visRecords) _visRecords = [NSMutableArray new];
        [_visRecords addObject:@[@(UINT32_MAX), @(_visOffset)]];
    }
    _visPending = NO;
}
- (void)nvmtlBeginVisibility {
    if (!_visPending || !_begun) return;
    uint32_t query;
    if (nvmtl_vk_cmd_alloc_occlusion(&_cb->_c, &query) ||
        nvmtl_vk_cmd_begin_occlusion(&_cb->_c, query)) {
        [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: query allocation failed at offset %zu - pass refused", (size_t)_visOffset]];
        _refused = YES; return;
    }
    _visSlot = query; _visActive = YES; _visPending = NO;
}
- (void)setVisibilityResultMode:(MTLVisibilityResultMode)mode offset:(NSUInteger)offset {
    [self nvmtlCloseVisibility];
    if (mode == MTLVisibilityResultModeDisabled) return;
    if (!_visBuf || (offset & 7) || offset > _visBuf->_b.size ||
        _visBuf->_b.size - offset < sizeof(uint64_t) || offset > 262144) {
        [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: invalid byte offset %zu", (size_t)offset]]; return;
    }
    _visOffset = offset; _visPending = YES;
    [self nvmtlBeginVisibility];
}
- (void)endEncoding {
    _cb->_nEnd++;
    _svFor=nil; _svKey=nil; _svSel=nil;   // nothing of this encoder's sampler choice outlives it
    if (_dumpPso) { _dumpPso = NO; [self nvmtlDumpBoundFragmentBuffers]; }
    [self nvmtlCloseVisibility];
    if (_begun) nvmtl_vk_cmd_end_render(&_cb->_c);
    nvmtl_hz_encoder_end(&_cb->_c);
    NSMutableDictionary *groups = [NSMutableDictionary new];
    BOOL needScratch = NO;
    for (NSArray *record in _visRecords) {
        NSNumber *offset = record[1];
        NSMutableArray *queries = groups[offset];
        if (!queries) { queries = [NSMutableArray new]; groups[offset] = queries; }
        [queries addObject:record[0]];
        if (queries.count > 1) needScratch = YES;
    }
    NVMTLBuffer *scratch = nil;
    if (needScratch) {
        scratch = [NVMTLBuffer new];
        if (nvmtl_vk_buffer_create(_visRecords.count * 8, 0, &scratch->_b)) {
            [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: accumulation scratch allocation failed"]]; scratch = nil;
        } else {
            if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
            [_cb->_scratch addObject:scratch];
        }
    }
    size_t cursor = 0;
    for (NSNumber *key in groups) {
        NSArray *queries = groups[key]; size_t offset = key.unsignedLongLongValue;
        if (queries.count > 1 && !scratch) continue;
        nvk_buffer *dst = queries.count > 1 ? &scratch->_b : &_visBuf->_b;
        size_t first = cursor;
        for (NSNumber *value in queries) {
            uint32_t query = value.unsignedIntValue;
            size_t out = queries.count > 1 ? cursor++ * 8 : offset;
            if (query == UINT32_MAX) {
                if (nvmtl_vk_cmd_write_word(&_cb->_c, dst, out, 0) ||
                    nvmtl_vk_cmd_write_word(&_cb->_c, dst, out + 4, 0))
                    [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: empty result write failed at offset %zu", out]];
            } else if (nvmtl_vk_cmd_copy_occlusion(&_cb->_c, query, 1, dst, out))
                [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: result copy failed at offset %zu", out]];
        }
        if (queries.count > 1 && nvmtl_vk_cmd_sum_occlusion(&_cb->_c, &scratch->_b, first,
                                                          (uint32_t)queries.count, &_visBuf->_b, offset))
            [self nvmtlVisibilityError:[NSString stringWithFormat:@"visibility: accumulation failed at offset %zu", offset]];
    }
    _visRecords = nil;
    if (_begun) return;
    if (_refused) return;
    switch (nvmtl_vk_pass_kind(&_pass)) {
        case 1: { static int said; if (!said++) nvlog("encoder: apps2 a pass with no draws loads everything and resolves nothing — left untouched"); return; }
        case 2:
            if (nvmtl_vk_cmd_empty_pass(&_cb->_c, &_pass)) nvlog("encoder: apps2 empty pass FAILED — its clears and resolves are NOT applied");
            else { static int said; if (said++ < 4) nvlog("encoder: apps2 empty pass (the clears/resolves of a pass with no draws)"); }
            if (_resolve) { NVMTLTexture *rs = _resolve->_parent ? _resolve->_parent : _resolve; if (rs->_surf) nvmtl_surf_dirty(_cb, rs); }
            return;
        default: break;
    }
    if (_load & 1u) { static int said; if (!said++) nvlog("encoder: a Load pass with no draws — target left untouched"); return; }
    if (_target && nvmtl_vk_cmd_clear_image(&_cb->_c, [_target nvi], _clear))
        nvlog("encoder: clear-only pass FAILED");
    else if (_target)
        nvlog("encoder: clear-only pass (%.2f %.2f %.2f %.2f)", _clear[0], _clear[1], _clear[2], _clear[3]);
}
- (void)setViewport:(MTLViewport)v {
    nvtrace("VP %.4g %.4g %.4g %.4g z %.3g %.3g", v.originX, v.originY, v.width, v.height, v.znear, v.zfar);
    _vp[0] = (float)v.originX; _vp[1] = (float)v.originY; _vp[2] = (float)v.width; _vp[3] = (float)v.height; _vp[4] = (float)v.znear; _vp[5] = (float)v.zfar; _hasVP = YES;
    if (_begun) nvmtl_vk_cmd_set_viewport(&_cb->_c, _vp[0], _vp[1], _vp[2], _vp[3], _vp[4], _vp[5]);
}
- (void)setViewports:(const MTLViewport *)v count:(NSUInteger)n { if (v && n) [self setViewport:v[0]]; }
- (void)setScissorRects:(const MTLScissorRect *)r count:(NSUInteger)n { if (r && n) [self setScissorRect:r[0]]; }
- (void)setScissorRect:(MTLScissorRect)r {
    nvtrace("SC %lu %lu %lu %lu", (unsigned long)r.x, (unsigned long)r.y, (unsigned long)r.width, (unsigned long)r.height);
    _sc[0] = (uint32_t)r.x; _sc[1] = (uint32_t)r.y; _sc[2] = (uint32_t)r.width; _sc[3] = (uint32_t)r.height; _hasSC = YES;
    if (!_begun) return;
    nvmtl_vk_cmd_set_scissor(&_cb->_c, (int32_t)r.x, (int32_t)r.y, (uint32_t)r.width, (uint32_t)r.height);
}
- (void)setCullMode:(MTLCullMode)mode {
    _cull = (_cull & 4) | (mode == MTLCullModeFront ? 1 : mode == MTLCullModeBack ? 2 : 0);
    if (_begun) nvmtl_vk_cmd_set_cull(&_cb->_c, _cull);
}
- (void)setFrontFacingWinding:(MTLWinding)w {
    _cull = (_cull & 3) | (w == MTLWindingCounterClockwise ? 4 : 0);
    if (_begun) nvmtl_vk_cmd_set_cull(&_cb->_c, _cull);
}
// GPU-encoded commands (NVMTL_GPU_ICB=1): the records a kernel wrote are gathered, per candidate pipeline (one that was made with
// supportIndirectCommandBuffers, or the pipeline the encoder already holds) and per primitive type, into indirect arguments whose
// counts are zero for every slot that is not that pipeline's and that type's; each candidate then replays every slot as an indirect
// draw. Buffers are the encoder's own bindings (a command's recorded buffer addresses are not applied), indexed draws are not carried.
- (void)nvmtlGpuIcb:(NVMTLIndirectCommandBuffer *)b loc:(uint32_t)loc len:(uint32_t)len range:(NVMTLBuffer *)rb offset:(NSUInteger)ro {
    if (!b->_recBuf) return;
    const uint32_t slots = nvmtl_icb_slots(b->_cmds.count);
    NSMutableArray *cands = [NSMutableArray arrayWithObject:[NSNull null]]; [cands addObjectsFromArray:nvmtl_icb_pipelines(NO)];
    enum { TYPES = 5 };
    // indexed commands: their indices are copied into a scratch index buffer of `cap` indices per slot (NVMTL_ICB_INDEX_CAP, default 65536,
    // shrunk to fit a 256 MiB budget); a command over the cap is skipped whole and logged here, never truncated
    uint32_t cap = 0; NVMTLBuffer *idxBuf = nil;
    { static uint32_t want; if (!want) { const char *e = getenv("NVMTL_ICB_INDEX_CAP"); long v = e ? strtol(e, NULL, 10) : 0; want = v > 0 && v <= 0x7FFFFFFF ? (uint32_t)v : 65536u; }
      cap = nvmtl_icb_index_cap(slots, want);
      if (!cap) nvlog("GPU ICB: %u slots leave no room for an index block each within the 256 MiB scratch budget - indexed commands are skipped", slots);
      else if (cap < want) nvlog("GPU ICB: the index cap is %u per slot, not the %u asked for, to keep the scratch index buffer within 256 MiB; indexed commands with more indices are skipped", cap, want);
      else { static int said; if (!said++) nvlog("GPU ICB: indexed commands with more than %u indices are skipped whole (NVMTL_ICB_INDEX_CAP raises the cap)", cap); }
      if (cap) { idxBuf = nvmtl_icb_scratch_bytes((NSUInteger)slots * cap * 4u); if (!idxBuf) { nvlog("GPU ICB: the %u x %u index scratch FAILED - indexed commands are skipped", slots, cap); cap = 0; } } }
    const uint32_t kinds = cap ? 2u : 1u;
    if (!slots || (uint64_t)slots * cands.count * TYPES * kinds > 262144u) { nvlog("GPU ICB: %u slot(s) x %lu candidate pipeline(s) x 5 types is over the 262144 indirect draws one execution may record - REFUSED", slots, (unsigned long)cands.count); return; }
    NVMTLBuffer *big = nvmtl_icb_scratch_bytes((NSUInteger)slots * cands.count * TYPES * kinds * NVMTL_ICB_RECORD_BYTES);
    if (!big) { nvlog("GPU ICB: scratch FAILED"); return; }
    nvmtl_retain_resource(_cb, big); nvmtl_retain_resource(_cb, b->_recBuf); if (rb) nvmtl_retain_resource(_cb, rb);
    NVMTLRenderPipelineState *orig = _ps;
    if (_begun) {
        uint32_t keep = 0;
        for (uint32_t i = 0; i < _passSig.ncol; i++) if (_passSig.cfmts[i]) keep |= NVMTL_LOAD_COLOUR(i);
        if (_passSig.has_depth) keep |= NVMTL_LOAD_DEPTH;
        if (_ps && _ps->_p.has_stencil) keep |= NVMTL_LOAD_STENCIL;
        BOOL visibility = _visActive || _visPending;
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO; _pass.load |= keep; _load |= keep; _visPending = visibility;
    }
    for (NSUInteger k = 0; k < cands.count; k++) {
        const uint64_t token = k ? ((id<MTLRenderPipelineState>)cands[k]).gpuResourceID._impl : 0;
        for (uint32_t t = 0; t < TYPES; t++)
            for (uint32_t ix = 0; ix < kinds; ix++) {
                const size_t off = ((((size_t)k * TYPES + t) * kinds + ix) * slots) * NVMTL_ICB_RECORD_BYTES;
                if (nvmtl_vk_cmd_icb_gather(&_cb->_c, &b->_recBuf->_b, &big->_b, off, slots, 0, token, t, loc, len, NULL, ix, cap, rb ? &rb->_b : NULL, ro)) { nvlog("GPU ICB: the gather pass FAILED - nothing runs"); return; }
            }
    }
    if (cap && nvmtl_vk_cmd_icb_index_copy(&_cb->_c, &b->_recBuf->_b, &idxBuf->_b, slots, cap, loc, len, rb ? &rb->_b : NULL, ro)) { nvlog("GPU ICB: the index copy pass FAILED - indexed commands are skipped"); cap = 0; }
    if (idxBuf) nvmtl_retain_resource(_cb, idxBuf);
    // per-command buffers: one patched address-table copy per slot and stage (nvmtl_icb_record.h); pipelines that reach their buffers
    // through descriptors instead of the table (address lowering refused them) keep the encoder's own bindings
    BOOL tabs = NO;
    if (slots <= NVMTL_ICB_MAX_PATCH_SLOTS) {
        NVMTLBuffer *dsts = nvmtl_icb_scratch_bytes((NSUInteger)slots * 16u);
        if (dsts) { nvmtl_retain_resource(_cb, dsts); tabs = nvmtl_vk_cmd_icb_tables(&_cb->_c, &b->_recBuf->_b, &dsts->_b, slots) == 0; }
    }
    if (!tabs) { static int said; if (!said++) nvlog("GPU ICB: the per-command buffer tables could not be prepared - draws use the encoder's own buffer bindings"); }
    NSUInteger drawn = 0;
    for (NSUInteger k = 0; k < cands.count; k++) {
        NVMTLRenderPipelineState *p = k ? (NVMTLRenderPipelineState *)cands[k] : orig;
        if (!p) continue;
        [self setRenderPipelineState:p];
        for (uint32_t t = 0; t < TYPES; t++)
            for (uint32_t ix = 0; ix < kinds; ix++)
                for (uint32_t i = 0; i < slots; i++) {
                    const NSUInteger at = ((((NSUInteger)k * TYPES + t) * kinds + ix) * slots + i) * NVMTL_ICB_RECORD_BYTES;
                    if (ix && !cap) continue;
                    nvmtl_vk_cmd_icb_use(&_cb->_c, tabs ? i + 1u : 0u);
                    if (ix) [self drawIndexedPrimitives:(MTLPrimitiveType)t indexType:MTLIndexTypeUInt32 indexBuffer:idxBuf indexBufferOffset:0 indirectBuffer:big indirectBufferOffset:at];
                    else [self drawPrimitives:(MTLPrimitiveType)t indirectBuffer:big indirectBufferOffset:at];
                    drawn++;
                }
    }
    nvmtl_vk_cmd_icb_use(&_cb->_c, 0); nvmtl_vk_cmd_icb_tables_free(&_cb->_c);
    if (orig && _ps != orig) [self setRenderPipelineState:orig];
    nvlog("GPU ICB: %lu indirect draw(s) recorded for %u slot(s), %lu candidate pipeline(s)", (unsigned long)drawn, slots, (unsigned long)cands.count);
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r {
    NVMTLIndirectCommandBuffer *b = (NVMTLIndirectCommandBuffer *)icb;
    if (!b) { nvlog("executeCommandsInBuffer: nil"); return; }
    if (b->_recBuf && r.length <= UINT32_MAX && r.location <= UINT32_MAX) [self nvmtlGpuIcb:b loc:(uint32_t)r.location len:(uint32_t)r.length range:nil offset:0];
    [self nvmtlHzForceFull];
    NSUInteger n = 0;
    for (NSUInteger i = r.location; i < r.location + r.length && i < b->_cmds.count; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i];
        if (!c->_armed || c->_isCompute) continue;
        if (c->_ps) [self setRenderPipelineState:c->_ps];
        if (c->_hasCull) [self setCullMode:c->_cull];
        if (c->_hasWinding) [self setFrontFacingWinding:c->_winding];
        if (c->_hasFill) [self setTriangleFillMode:c->_fill];
        if (c->_hasClip) [self setDepthClipMode:c->_clip];
        if (c->_ds) [self setDepthStencilState:c->_ds];
        if (c->_hasBias) [self setDepthBias:c->_bias slopeScale:c->_slope clamp:c->_clamp];
        for (NSArray *e in c->_vbufs) {
            if (e.count > 3) [self setVertexBuffer:e[0] offset:[e[1] unsignedLongValue] attributeStride:[e[3] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
            else [self setVertexBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]]; }
        for (NSArray *e in c->_fbufs) [self setFragmentBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
        if (c->_indexed)
            [self drawIndexedPrimitives:c->_type indexCount:c->_count indexType:c->_itype indexBuffer:c->_ibuf
                      indexBufferOffset:c->_ioff instanceCount:c->_instances baseVertex:c->_baseVertex baseInstance:c->_baseInstance];
        else
            [self drawPrimitives:c->_type vertexStart:c->_start vertexCount:c->_count
                   instanceCount:c->_instances baseInstance:c->_baseInstance];
        n++;
    }
    nvlog("executeCommandsInBuffer: replayed %lu of %lu slot(s)", (unsigned long)n, (unsigned long)r.length);
}
// executeCommandsInBuffer:indirectBuffer:indirectBufferOffset: - the commands to run are chosen by a range the GPU wrote
// (nvmtl_icb_range.h). Every armed slot becomes an indirect draw whose count is zeroed outside that range.
// GPU-encoded commands (a kernel writing the ICB) are not carried: this ICB is the list the CPU encoded.
static NVMTLBuffer *nvmtl_icb_scratch_bytes(NSUInteger n) {
    id<MTLBuffer> b = [(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:n options:MTLResourceStorageModeShared];
    if (b) memset(b.contents, 0, n);
    return (NVMTLBuffer *)b;
}
static NVMTLBuffer *nvmtl_icb_scratch(uint32_t slots) {
    id<MTLBuffer> b = [(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:(NSUInteger)slots * NVMTL_ICB_RECORD_BYTES options:MTLResourceStorageModeShared];
    if (b) memset(b.contents, 0, (size_t)slots * NVMTL_ICB_RECORD_BYTES);
    return (NVMTLBuffer *)b;
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(id<MTLBuffer>)ib indirectBufferOffset:(NSUInteger)ioff {
    NVMTLIndirectCommandBuffer *b = (NVMTLIndirectCommandBuffer *)icb; NVMTLBuffer *rb = (NVMTLBuffer *)ib;
    const uint32_t slots = b ? nvmtl_icb_slots(b->_cmds.count) : 0;
    if (!b || !rb || !slots || ioff + 8 > rb.length || (ioff & 3)) { nvlog("executeCommandsInBuffer:indirectBuffer: unusable arguments (icb %p, buffer %p, %u slot(s), offset %lu) - nothing runs", b, rb, slots, (unsigned long)ioff); return; }
    NVMTLBuffer *sc = nvmtl_icb_scratch(slots);
    if (!sc) { nvlog("executeCommandsInBuffer:indirectBuffer: scratch of %u records FAILED - nothing runs", slots); return; }
    uint32_t *rec = (uint32_t *)sc.contents;
    for (uint32_t i = 0; i < slots; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i]; uint32_t *r = rec + i * NVMTL_ICB_RECORD_WORDS;
        if (!c->_armed || c->_isCompute) continue;
        if (c->_indexed) nvmtl_icb_record_draw_indexed(r, (uint32_t)c->_count, (uint32_t)c->_instances, (int32_t)c->_baseVertex, (uint32_t)c->_baseInstance);
        else nvmtl_icb_record_draw(r, (uint32_t)c->_count, (uint32_t)c->_instances, (uint32_t)c->_start, (uint32_t)c->_baseInstance);
    }
    [self nvmtlHzForceFull];
    nvmtl_retain_resource(_cb, sc); nvmtl_retain_resource(_cb, rb);
    if (_begun) {   // the masking pass is a compute dispatch: leave the render pass, keeping every attachment's contents
        uint32_t keep = 0;
        for (uint32_t i = 0; i < _passSig.ncol; i++) if (_passSig.cfmts[i]) keep |= NVMTL_LOAD_COLOUR(i);
        if (_passSig.has_depth) keep |= NVMTL_LOAD_DEPTH;
        if (_ps && _ps->_p.has_stencil) keep |= NVMTL_LOAD_STENCIL;
        BOOL visibility = _visActive || _visPending;
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO; _pass.load |= keep; _load |= keep; _visPending = visibility;
    }
    if (nvmtl_vk_cmd_icb_mask(&_cb->_c, &sc->_b, &rb->_b, ioff, slots, NVMTL_ICB_ZERO_DRAW)) { nvlog("executeCommandsInBuffer:indirectBuffer: the range pass FAILED - nothing runs"); return; }
    if (_ps) [self setRenderPipelineState:_ps];   // begins the pass again with the attachments kept
    NSUInteger n = 0;
    for (uint32_t i = 0; i < slots; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i];
        if (!c->_armed || c->_isCompute) continue;
        if (c->_ps) [self setRenderPipelineState:c->_ps];
        if (c->_hasCull) [self setCullMode:c->_cull];
        if (c->_hasWinding) [self setFrontFacingWinding:c->_winding];
        if (c->_hasFill) [self setTriangleFillMode:c->_fill];
        if (c->_hasClip) [self setDepthClipMode:c->_clip];
        if (c->_ds) [self setDepthStencilState:c->_ds];
        if (c->_hasBias) [self setDepthBias:c->_bias slopeScale:c->_slope clamp:c->_clamp];
        for (NSArray *e in c->_vbufs) {
            if (e.count > 3) [self setVertexBuffer:e[0] offset:[e[1] unsignedLongValue] attributeStride:[e[3] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
            else [self setVertexBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]]; }
        for (NSArray *e in c->_fbufs) [self setFragmentBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
        if (c->_indexed)
            [self drawIndexedPrimitives:c->_type indexType:c->_itype indexBuffer:c->_ibuf indexBufferOffset:c->_ioff indirectBuffer:sc indirectBufferOffset:(NSUInteger)i * NVMTL_ICB_RECORD_BYTES];
        else
            [self drawPrimitives:c->_type indirectBuffer:sc indirectBufferOffset:(NSUInteger)i * NVMTL_ICB_RECORD_BYTES];
        n++;
    }
    nvlog("executeCommandsInBuffer:indirectBuffer: %lu armed slot(s) of %u recorded as range-masked indirect draws", (unsigned long)n, slots);
    if (b->_recBuf) [self nvmtlGpuIcb:b loc:0 len:0 range:rb offset:ioff];
}
- (void)setDepthClipMode:(MTLDepthClipMode)mode {
    if (nvmtl_vk_cmd_set_depth_clip(&_cb->_c, mode == MTLDepthClipModeClip))
        nvlog("setDepthClipMode:%lu REFUSED — VK_EXT_depth_clip_enable is not enabled on this device",
              (unsigned long)mode);
}
- (void)setTriangleFillMode:(MTLTriangleFillMode)mode {
    if (nvmtl_vk_cmd_set_polygon_mode(&_cb->_c, mode == MTLTriangleFillModeLines))
        nvlog("setTriangleFillMode:%lu REFUSED — VK_EXT_extended_dynamic_state3 is not enabled on this device",
              (unsigned long)mode);
}
- (void)setDepthStencilState:(id<MTLDepthStencilState>)s {
    _ds = (NVMTLDepthStencilState *)s;
    [self nvmtlApplyDepthStencil];
}
// macOS 26: the depth bounds test. Setting a range turns the test on for the rest of the encoder; fragments whose STORED
// attachment depth is outside [min, max] are discarded. Apple applies no test until this is called.
- (void)setDepthTestMinBound:(float)lo maxBound:(float)hi {
    _dbOn = YES; _dbLo = lo; _dbHi = hi;
    [self nvmtlApplyDepthStencil];
}
// macOS 26: a logical-to-physical colour attachment map. Measured on Apple GPUs, a Metal 3 encoder accepts it and nothing
// changes, because a Metal 3 render pipeline always uses the identity mapping (only Metal 4 pipelines inherit the map).
// Without the method the call is an unrecognized selector and the app stops.
- (void)setColorAttachmentMap:(id)map {
    static int said;
    if (map && !said++) nvlog("render encoder setColorAttachmentMap: a Metal 3 pipeline uses the identity mapping, so the map has no effect, as on Apple GPUs (said once)");
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)type indexCount:(NSUInteger)count indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)off
                instanceCount:(NSUInteger)instances baseVertex:(NSInteger)baseVertex baseInstance:(NSUInteger)baseInstance {
    if (!_begun) { nvlog("drawIndexedPrimitives with no pipeline set"); return; }
    NVMTLBuffer *b = (NVMTLBuffer *)ib;
    if (!b) { nvlog("drawIndexedPrimitives with no index buffer"); return; }
    nvmtl_retain_resource(_cb, b);
    if (nvmtl_hz_mode() && !nvmtl_hz_untracked_buf(b)) nvmtl_hz_declare_buffer(&_cb->_c, &b->_b, 0, 1);
    if (![self nvmtlBindSamplerPipeline]) return;
    nvtrace("DRAW type %u | vs %s | fs %s | blend %s", (unsigned)type, _ps ? _ps->_vname.UTF8String : "-", _ps ? _ps->_fname.UTF8String : "-", _ps ? _ps->_blend.UTF8String : "-");
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_); nvtrace("  BUFS%s", bufs_); }
    if (nvmtl_vk_cmd_set_topology(&_cb->_c, (unsigned)type)) return;
    if ([self nvmtlVertexInputs]) return;
    nvtrace("  indexed count %lu u32 %d off %lu instances %lu baseVertex %ld", (unsigned long)count, it == MTLIndexTypeUInt32, (unsigned long)off, (unsigned long)instances, (long)baseVertex);
    nvmtl_vk_cmd_draw_indexed(&_cb->_c, &b->_b, off, (uint32_t)count, it == MTLIndexTypeUInt32,
                              (uint32_t)instances, (int32_t)baseVertex, (uint32_t)baseInstance);
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)n indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)off instanceCount:(NSUInteger)i {
    [self drawIndexedPrimitives:t indexCount:n indexType:it indexBuffer:ib indexBufferOffset:off
                  instanceCount:i baseVertex:0 baseInstance:0];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)n indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)off {
    [self drawIndexedPrimitives:t indexCount:n indexType:it indexBuffer:ib indexBufferOffset:off
                  instanceCount:1 baseVertex:0 baseInstance:0];
}

- (void)setStencilReferenceValue:(uint32_t)ref { nvmtl_vk_cmd_set_stencil_ref(&_cb->_c, ref, ref); _hasRef = YES; }
- (void)setStencilFrontReferenceValue:(uint32_t)f backReferenceValue:(uint32_t)b { nvmtl_vk_cmd_set_stencil_ref(&_cb->_c, f, b); _hasRef = YES; }
- (void)setDepthBias:(float)bias slopeScale:(float)slope clamp:(float)clamp { nvmtl_vk_cmd_set_depth_bias(&_cb->_c, bias, slope, clamp); }
- (void)setBlendColorRed:(float)r green:(float)g blue:(float)b alpha:(float)a {
    float rgba[4] = { r, g, b, a }; nvmtl_vk_cmd_set_blend_color(&_cb->_c, rgba);
}
- (void)setVertexBufferOffset:(NSUInteger)off atIndex:(NSUInteger)i {
    nvtrace("VBOFF[%lu] -> %lu", (unsigned long)i, (unsigned long)off);
    if (i < NVMTL_NBUFAT && _vbufAt[i]) _vbufOff[i] = off;
    if (nvmtl_vk_bind_buffer_offset(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, off))
        nvlog("setVertexBufferOffset: nothing bound at vertex index %lu", (unsigned long)i);
}
- (void)nvmtlVertexStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (i >= NVMTL_NVIN) { nvlog("apps-dyn: attributeStride at vertex buffer index %lu is outside [0,%d) — ignored", (unsigned long)i, NVMTL_NVIN); return; }
    if (st == MTLAttributeStrideStatic) { _vstrideSet &= ~(1u << i); return; }
    if (st > UINT32_MAX) { nvlog("apps-dyn: attributeStride %lu at vertex buffer index %lu does not fit — the index keeps NO stride", (unsigned long)st, (unsigned long)i);
        _vstrideSet &= ~(1u << i); return; }
    _vstride[i] = (uint32_t)st; _vstrideSet |= 1u << i;
    nvtrace("VBSTRIDE[%lu] -> %lu", (unsigned long)i, (unsigned long)st);
}
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    [self setVertexBuffer:b offset:off atIndex:i]; [self nvmtlVertexStride:st atIndex:i]; }
- (void)setVertexBufferOffset:(NSUInteger)off attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    [self setVertexBufferOffset:off atIndex:i]; [self nvmtlVertexStride:st atIndex:i]; }
- (void)setVertexBuffers:(const id<MTLBuffer> __unsafe_unretained [])bufs offsets:(const NSUInteger *)offs
        attributeStrides:(const NSUInteger *)sts withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (bufs[k])
        [self setVertexBuffer:bufs[k] offset:offs ? offs[k] : 0 attributeStride:sts ? sts[k] : MTLAttributeStrideStatic atIndex:r.location + k]; }
- (void)setVertexBytes:(const void *)b length:(NSUInteger)l attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    [self setVertexBytes:b length:l atIndex:i]; [self nvmtlVertexStride:st atIndex:i]; }
- (void)setFragmentBufferOffset:(NSUInteger)off atIndex:(NSUInteger)i {
    nvtrace("FBOFF[%lu] -> %lu", (unsigned long)i, (unsigned long)off);
    if (i < NVMTL_NBUFAT && _fbufAt[i]) _fbufOff[i] = off;
    if (nvmtl_vk_bind_buffer_offset(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, off))
        nvlog("setFragmentBufferOffset: nothing bound at fragment index %lu", (unsigned long)i);
}
- (void)setVertexBuffers:(const id<MTLBuffer> __unsafe_unretained [])bufs offsets:(const NSUInteger *)offs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (bufs[k]) [self setVertexBuffer:bufs[k] offset:offs ? offs[k] : 0 atIndex:r.location + k];
}
- (void)setFragmentBuffers:(const id<MTLBuffer> __unsafe_unretained [])bufs offsets:(const NSUInteger *)offs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (bufs[k]) [self setFragmentBuffer:bufs[k] offset:offs ? offs[k] : 0 atIndex:r.location + k];
}
- (void)setVertexTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (t[k]) [self setVertexTexture:t[k] atIndex:r.location + k];
}
- (void)setFragmentTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (t[k]) [self setFragmentTexture:t[k] atIndex:r.location + k];
}
- (void)setVertexSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setVertexSamplerState:ss[k] atIndex:r.location + k];
}
- (void)setFragmentSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setFragmentSamplerState:ss[k] atIndex:r.location + k];
}
- (void)setFragmentSamplerState:(id<MTLSamplerState>)ss lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i {
    [self setFragmentSamplerState:ss atIndex:i];
    NSString *key=nvmtl_sampler_slot_key(i);
    if(_fragmentSamplers[key]) {
        NSMutableDictionary *state=[_fragmentSamplers[key] mutableCopy];
        state[@"lod_min_clamp"]=@(lo);state[@"lod_max_clamp"]=@(hi);
        if (nvmtl_pixel_sampler_update(_fragmentSamplers,key,state)) _pixelSamplerKey=nil;
    }
}
- (void)setVertexSamplerState:(id<MTLSamplerState>)ss lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i {
    [self setVertexSamplerState:ss atIndex:i];
    NSString *key=nvmtl_sampler_slot_key(i);
    if(_vertexSamplers[key]) {
        NSMutableDictionary *state=[_vertexSamplers[key] mutableCopy];
        state[@"lod_min_clamp"]=@(lo);state[@"lod_max_clamp"]=@(hi);
        if (nvmtl_pixel_sampler_update(_vertexSamplers,key,state)) _pixelSamplerKey=nil;
    }
}
- (void)setFragmentSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for(NSUInteger k=0;k<r.length;k++) [self setFragmentSamplerState:ss[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location+k];
}
- (void)setVertexSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for(NSUInteger k=0;k<r.length;k++) [self setVertexSamplerState:ss[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location+k];
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u { nvmtl_retain_resource(_cb, r); nvmtl_hz_use(_cb, r, u); }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) nvmtl_use_heap(_cb, h[i]); }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n stages:(MTLRenderStages)st { nvtrace("USEHEAPS %lu stages %lu", (unsigned long)n, (unsigned long)st); for (NSUInteger i = 0; i < n; i++) [self useHeap:h[i] stages:st]; }
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u stages:(MTLRenderStages)st { nvmtl_retain_resource(_cb, r); nvmtl_hz_use(_cb, r, u); nvtrace("USERES %s %p usage %lu stages %lu", object_getClassName(r), (__bridge void *)r, (unsigned long)u, (unsigned long)st); }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i=0;i<n;i++) { nvmtl_retain_resource(_cb,r[i]); nvmtl_hz_use(_cb, r[i], u); } }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u stages:(MTLRenderStages)st { for (NSUInteger i=0;i<n;i++) { nvmtl_retain_resource(_cb,r[i]); nvmtl_hz_use(_cb, r[i], u); } nvtrace("USERES x%lu usage %lu stages %lu", (unsigned long)n, (unsigned long)u, (unsigned long)st); }
- (void)useHeap:(id<MTLHeap>)h { nvmtl_use_heap(_cb, h); }
- (void)useHeap:(id<MTLHeap>)h stages:(MTLRenderStages)st { (void)st; nvmtl_use_heap(_cb, h); }
- (void)sampleCountersInBuffer:(id)sb atSampleIndex:(NSUInteger)i withBarrier:(BOOL)b { nvmtl_sample_counter(_cb, sb, i, self, b); }
- (void)textureBarrier { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)updateFence:(id<MTLFence>)f afterStages:(MTLRenderStages)st {}
- (void)waitForFence:(id<MTLFence>)f beforeStages:(MTLRenderStages)st {
    // in a pass begun without a barrier the wait must order what came before the pass: restart it behind one
    if (nvmtl_hz_mode() == 1 && _cb->_c.hz_dyn) { [self nvmtlHzRestart:NO]; return; }
    if (nvmtl_hz_mode() == 2) nvmtl_hz_force_full(&_cb->_c, 1);
    nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c);
}
- (void)memoryBarrierWithScope:(MTLBarrierScope)scope afterStages:(MTLRenderStages)a beforeStages:(MTLRenderStages)b { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)memoryBarrierWithResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n afterStages:(MTLRenderStages)a beforeStages:(MTLRenderStages)b { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)drawPrimitives:(MTLPrimitiveType)type indirectBuffer:(id<MTLBuffer>)buf indirectBufferOffset:(NSUInteger)off {
    NVMTLBuffer *nb = (NVMTLBuffer *)buf;
    if (!nb) { nvlog("drawPrimitives:indirectBuffer: nil"); return; }
    [self nvmtlHzForceFull];
    nvmtl_retain_resource(_cb, nb);
    if (!_begun) { nvlog("drawPrimitives:indirectBuffer: no render pass"); return; }
    if (![self nvmtlBindSamplerPipeline]) return;
    nvtrace("DRAW type %u | vs %s | fs %s | blend %s", (unsigned)type, _ps ? _ps->_vname.UTF8String : "-", _ps ? _ps->_fname.UTF8String : "-", _ps ? _ps->_blend.UTF8String : "-");
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_); nvtrace("  BUFS%s", bufs_); }
    if (nvmtl_vk_cmd_set_topology(&_cb->_c, (unsigned)type)) return;
    if ([self nvmtlVertexInputs]) return;
    nvmtl_vk_cmd_draw_indirect(&_cb->_c, &nb->_b, off);
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)type indexType:(MTLIndexType)it indexBuffer:(id<MTLBuffer>)idx
          indexBufferOffset:(NSUInteger)ioff indirectBuffer:(id<MTLBuffer>)buf indirectBufferOffset:(NSUInteger)off {
    NVMTLBuffer *ni = (NVMTLBuffer *)idx, *nb = (NVMTLBuffer *)buf;
    if (!ni || !nb) { nvlog("drawIndexedPrimitives:indirectBuffer: nil"); return; }
    [self nvmtlHzForceFull];
    nvmtl_retain_resource(_cb, ni);
    nvmtl_retain_resource(_cb, nb);
    if (!_begun) { nvlog("drawIndexedPrimitives:indirectBuffer: no render pass"); return; }
    if (![self nvmtlBindSamplerPipeline]) return;
    nvtrace("DRAW type %u | vs %s | fs %s | blend %s", (unsigned)type, _ps ? _ps->_vname.UTF8String : "-", _ps ? _ps->_fname.UTF8String : "-", _ps ? _ps->_blend.UTF8String : "-");
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_); nvtrace("  BUFS%s", bufs_); }
    if (nvmtl_vk_cmd_set_topology(&_cb->_c, (unsigned)type)) return;
    if ([self nvmtlVertexInputs]) return;
    nvmtl_vk_cmd_draw_indexed_indirect(&_cb->_c, &ni->_b, ioff, it == MTLIndexTypeUInt32, &nb->_b, off);
}
- (void)pushDebugGroup:(NSString *)g {
    nvtrace("GROUP %s", g.UTF8String ?: "?");
    // NVMTL_SKIP_GROUP=<text>: draws inside a debug group whose name contains it are not recorded (finds a hanging shader)
    static const char *skip; static int once; if (!once) { once = 1; skip = getenv("NVMTL_SKIP_GROUP"); }
    _groupDepth++;
    if (skip && *skip && !_cb->_c.skip_group && strstr(g.UTF8String ?: "", skip)) { _cb->_c.skip_group = _groupDepth; nvlog("NVMTL_SKIP_GROUP: skipping %s", g.UTF8String); }
    // NVMTL_DUMP_GROUP=<text>: the first time a debug group containing it opens, the fragment buffers bound at that moment
    // are copied out after this command buffer completes, up to 64 KB each, to $TMPDIR/dump-f<index>.bin (diagnostic)
    static const char *dump; static int donce; static _Atomic int dumped; if (!donce) { donce = 1; dump = getenv("NVMTL_DUMP_GROUP"); }
    if (dump && *dump && strstr(g.UTF8String ?: "", dump) && !atomic_exchange(&dumped, 1)) { _dumpDepth = _groupDepth; _dumpName = [g copy]; }
}
- (void)nvmtlDumpBoundFragmentBuffers {
    {
        NSMutableDictionary *bound = [NSMutableDictionary new]; NSString *name = _dumpName;
        for (NSUInteger i = 0; i < NVMTL_NBUFAT; i++) if (_fbufAt[i]) bound[@(i)] = @[_fbufAt[i], @(_fbufOff[i])];
        [_cb addCompletedHandler:^(id<MTLCommandBuffer> done) {
            dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
                id<MTLDevice> dev = done.device; id<MTLCommandQueue> q = [dev newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer];
                id<MTLBlitCommandEncoder> be = [cb blitCommandEncoder]; NSMutableDictionary *outs = [NSMutableDictionary new];
                for (NSNumber *k in bound) { NVMTLBuffer *b = bound[k][0]; NSUInteger off = [bound[k][1] unsignedLongValue];
                    if (off >= b.length) continue; NSUInteger n = MIN(b.length - off, (NSUInteger)65536);
                    id<MTLBuffer> o = [dev newBufferWithLength:n options:MTLResourceStorageModeShared];
                    [be copyFromBuffer:b sourceOffset:off toBuffer:o destinationOffset:0 size:n]; outs[k] = o; }
                [be endEncoding]; [cb commit]; [cb waitUntilCompleted];
                for (NSNumber *k in outs) { id<MTLBuffer> o = outs[k];
                    NSString *path = [NSTemporaryDirectory() stringByAppendingFormat:@"dump-f%@.bin", k];
                    [[NSData dataWithBytes:o.contents length:o.length] writeToFile:path atomically:YES]; }
                nvlog("NVMTL_DUMP_GROUP: %lu fragment buffer(s) of '%s' written to %s (command buffer status %ld)", (unsigned long)outs.count,
                      name.UTF8String, NSTemporaryDirectory().UTF8String, (long)done.status);
            });
        }];
    }
}
- (void)popDebugGroup { if (_dumpDepth && _dumpDepth == _groupDepth) { [self nvmtlDumpBoundFragmentBuffers]; _dumpDepth = 0; }
    if (_cb->_c.skip_group == _groupDepth) _cb->_c.skip_group = 0; if (_groupDepth) _groupDepth--; }
- (void)insertDebugSignpost:(NSString *)s {}
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
// -commandBuffer is SPI on Apple's encoders (_MTLCommandEncoder), not in the MTLCommandEncoder protocol, but apps call it:
// Hades asks every render encoder for its command buffer each frame; the forwarding stub answered nil and messages to the
// nil command buffer did nothing. The encoder keeps _cb after endEncoding, so it answers then too, as Apple's does.
- (id<MTLCommandBuffer>)commandBuffer { return (id<MTLCommandBuffer>)_cb; }
- (void)setColorStoreAction:(MTLStoreAction)a atIndex:(NSUInteger)i {
    BOOL resolve = a == MTLStoreActionMultisampleResolve || a == MTLStoreActionStoreAndMultisampleResolve;
    BOOL planned = i < NVMTL_NCOL && _pass.res[i] != NULL;
    if (resolve && !planned) { nvmtl_apps1_resolve_miss++; static int said; if (said++ < 4)
        nvlog("encoder: apps1 setColorStoreAction:%lu atIndex:%lu — the pass began with no resolve texture on that slot: resolve NOT applied (apps3)", (unsigned long)a, (unsigned long)i); }
    else if (!resolve && planned) { nvmtl_apps1_resolve_extra++; static int said; if (said++ < 4)
        nvlog("encoder: apps1 setColorStoreAction:%lu atIndex:%lu — the resolve planned for MTLStoreActionUnknown stays: the resolve texture IS written", (unsigned long)a, (unsigned long)i); }
}
- (void)setDepthStoreAction:(MTLStoreAction)a {
    if (a == MTLStoreActionCustomSampleDepthStore) _pass.depth_store_options |= MTLStoreActionOptionCustomSamplePositions;
    if (a == MTLStoreActionMultisampleResolve || a == MTLStoreActionStoreAndMultisampleResolve) { nvmtl_apps1_ds_resolve_miss++; static int said;
        if (said++ < 4) nvlog("encoder: apps1 setDepthStoreAction:%lu — depth resolve is not built: depth is stored, NOT resolved (apps3)", (unsigned long)a); }
}
- (void)setStencilStoreAction:(MTLStoreAction)a {
    if (a == MTLStoreActionMultisampleResolve || a == MTLStoreActionStoreAndMultisampleResolve) { nvmtl_apps1_ds_resolve_miss++; static int said;
        if (said++ < 4) nvlog("encoder: apps1 setStencilStoreAction:%lu — stencil resolve is not built: stencil is stored, NOT resolved (apps3)", (unsigned long)a); }
}
- (void)setColorStoreActionOptions:(MTLStoreActionOptions)x atIndex:(NSUInteger)i {
    if (x & ~MTLStoreActionOptionCustomSamplePositions) [self nvmtlSamplePositionError:@"unknown color store options"];
    (void)i;
}
- (void)setDepthStoreActionOptions:(MTLStoreActionOptions)x {
    _pass.depth_store_options = (uint32_t)x;
    if (x & ~MTLStoreActionOptionCustomSamplePositions) [self nvmtlSamplePositionError:@"unknown depth store options"];
}
- (void)setStencilStoreActionOptions:(MTLStoreActionOptions)x {
    _pass.stencil_store_options = (uint32_t)x;
    if (x) [self nvmtlSamplePositionError:@"stencil store options are illegal"];
}
// render-stage function pointers and visible function tables (formerly the B41FunctionPointers category in NVMTLLibraryLoad.m)
- (void)setFragmentVisibleFunctionTable:(id<MTLVisibleFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLVisibleFunctionTable class]]) { nvlog("setFragmentVisibleFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setFragmentBuffer:t ? ((NVMTLVisibleFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setFragmentVisibleFunctionTables:(const id<MTLVisibleFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setFragmentVisibleFunctionTable:ts[k] atBufferIndex:r.location + k];
}
- (void)setVertexVisibleFunctionTable:(id<MTLVisibleFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLVisibleFunctionTable class]]) { nvlog("setVertexVisibleFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setVertexBuffer:t ? ((NVMTLVisibleFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setVertexVisibleFunctionTables:(const id<MTLVisibleFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setVertexVisibleFunctionTable:ts[k] atBufferIndex:r.location + k];
}
// intersection function tables (formerly the RT3IntersectionTables category in NVMTLLibraryLoad.m)
- (void)setVertexIntersectionFunctionTable:(id<MTLIntersectionFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLIntersectionFunctionTable class]]) { nvlog("setVertexIntersectionFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setVertexBuffer:t ? ((NVMTLIntersectionFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setFragmentIntersectionFunctionTable:(id<MTLIntersectionFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLIntersectionFunctionTable class]]) { nvlog("setFragmentIntersectionFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setFragmentBuffer:t ? ((NVMTLIntersectionFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setVertexIntersectionFunctionTables:(const id<MTLIntersectionFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setVertexIntersectionFunctionTable:ts[k] atBufferIndex:r.location + k];
}
- (void)setFragmentIntersectionFunctionTables:(const id<MTLIntersectionFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setFragmentIntersectionFunctionTable:ts[k] atBufferIndex:r.location + k];
}
// mesh shaders (formerly the B42Mesh category in NVMTLLibraryLoad.m)
static NSMutableDictionary *nvmtl_mesh_table(NVMTLRenderCommandEncoder *e, BOOL object) {
    NVMTLMeshBinds *mb = objc_getAssociatedObject(e, &kNVMTLMeshBinds);
    if (!mb) { mb = [NVMTLMeshBinds new]; mb->_obj = [NSMutableDictionary new]; mb->_mesh = [NSMutableDictionary new];
               objc_setAssociatedObject(e, &kNVMTLMeshBinds, mb, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
    return object ? mb->_obj : mb->_mesh;
}
static void nvmtl_mesh_note(NVMTLRenderCommandEncoder *e, BOOL object, char kind, NSUInteger i, NSArray *v) {
    NSString *k = [NSString stringWithFormat:@"%c%lu", kind, (unsigned long)i];
    NSMutableDictionary *t = nvmtl_mesh_table(e, object);
    if (v) t[k] = v; else [t removeObjectForKey:k];
}
static void nvmtl_mesh_note_offset(NVMTLRenderCommandEncoder *e, BOOL object, NSUInteger i, NSUInteger off) {
    NSMutableDictionary *t = nvmtl_mesh_table(e, object); NSString *k = [NSString stringWithFormat:@"b%lu", (unsigned long)i];
    NSArray *v = t[k];
    if (v.count == 2) t[k] = @[v[0], @(off)];
    else nvlog("mesh: set%sBufferOffset at %lu with no buffer bound there - ignored", object ? "Object" : "Mesh", (unsigned long)i);
}
static void nvmtl_mesh_replay(NVMTLRenderCommandEncoder *e, NSDictionary *t) {
    for (NSString *k in t) {
        NSArray *v = t[k]; unichar c = [k characterAtIndex:0]; NSUInteger i = (NSUInteger)[[k substringFromIndex:1] integerValue];
        if (c == 'b') {
            if ([v[0] isKindOfClass:[NSData class]]) [e setVertexBytes:[v[0] bytes] length:[v[0] length] atIndex:i];
            else [e setVertexBuffer:v[0] offset:[v[1] unsignedIntegerValue] atIndex:i];
        } else if (c == 't') [e setVertexTexture:v[0] atIndex:i];
        else if (c == 's') {
            if (v.count == 3) [e setVertexSamplerState:v[0] lodMinClamp:[v[1] floatValue] lodMaxClamp:[v[2] floatValue] atIndex:i];
            else [e setVertexSamplerState:v[0] atIndex:i];
        }
    }
}
#define NVMTL_MESH_BINDERS(Stage, OBJ, FWD) \
- (void)set##Stage##Buffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i { nvmtl_mesh_note(self, OBJ, 'b', i, b ? @[b, @(off)] : nil); if (FWD) [self setVertexBuffer:b offset:off atIndex:i]; } \
- (void)set##Stage##BufferOffset:(NSUInteger)off atIndex:(NSUInteger)i { nvmtl_mesh_note_offset(self, OBJ, i, off); if (FWD) [self setVertexBufferOffset:off atIndex:i]; } \
- (void)set##Stage##Buffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)o withRange:(NSRange)r { \
    for (NSUInteger k = 0; k < r.length; k++) [self set##Stage##Buffer:b[k] offset:o[k] atIndex:r.location + k]; } \
- (void)set##Stage##Bytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { \
    nvmtl_mesh_note(self, OBJ, 'b', i, b ? @[[NSData dataWithBytes:b length:l]] : nil); if (FWD) [self setVertexBytes:b length:l atIndex:i]; } \
- (void)set##Stage##Texture:(id<MTLTexture>)t atIndex:(NSUInteger)i { nvmtl_mesh_note(self, OBJ, 't', i, t ? @[t] : nil); if (FWD) [self setVertexTexture:t atIndex:i]; } \
- (void)set##Stage##Textures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r { \
    for (NSUInteger k = 0; k < r.length; k++) [self set##Stage##Texture:t[k] atIndex:r.location + k]; } \
- (void)set##Stage##SamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i { nvmtl_mesh_note(self, OBJ, 's', i, s ? @[s] : nil); if (FWD) [self setVertexSamplerState:s atIndex:i]; } \
- (void)set##Stage##SamplerState:(id<MTLSamplerState>)s lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i { \
    nvmtl_mesh_note(self, OBJ, 's', i, s ? @[s, @(lo), @(hi)] : nil); if (FWD) [self setVertexSamplerState:s lodMinClamp:lo lodMaxClamp:hi atIndex:i]; } \
- (void)set##Stage##SamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s withRange:(NSRange)r { \
    for (NSUInteger k = 0; k < r.length; k++) [self set##Stage##SamplerState:s[k] atIndex:r.location + k]; } \
- (void)set##Stage##SamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r { \
    for (NSUInteger k = 0; k < r.length; k++) [self set##Stage##SamplerState:s[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location + k]; }
NVMTL_MESH_BINDERS(Mesh, NO, YES)
NVMTL_MESH_BINDERS(Object, YES, NO)
- (void)setObjectThreadgroupMemoryLength:(NSUInteger)l atIndex:(NSUInteger)i {
    static int told; if (told++ < 4) nvlog("mesh: setObjectThreadgroupMemoryLength:%lu atIndex:%lu is NOT carried - an object reading that threadgroup memory reads its static size", (unsigned long)l, (unsigned long)i);
}
- (NVMTLBuffer *)nvmtlMeshScratch:(uint64_t)bytes header:(const uint32_t *)h {
    NVMTLBuffer *sb = (NVMTLBuffer *)[(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:(NSUInteger)bytes options:MTLResourceStorageModePrivate];
    if (!sb) { nvlog("mesh: %llu B scratch buffer allocation FAILED - NOT drawn", bytes); return nil; }
    NVMTLBuffer *hb = [NVMTLBuffer new];
    if (nvmtl_vk_buffer_create(32, 1, &hb->_b) || !hb->_b.map) { nvlog("mesh: header staging buffer FAILED - NOT drawn"); return nil; }
    memcpy(hb->_b.map, h, 32);
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
    [_cb->_resources addObject:sb]; [_cb->_scratch addObject:hb];
    nvmtl_vk_cmd_end_render(&_cb->_c);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (nvmtl_vk_cmd_fill_buffer(&_cb->_c, &sb->_b, 0, (size_t)bytes, 0)) nvlog("mesh: scratch clear FAILED - counts may be stale");
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &hb->_b, 0, &sb->_b, 0, 32)) nvlog("mesh: header copy FAILED - the draw reads zero args");
    nvmtl_vk_cmd_barrier(&_cb->_c);
    return sb;
}
- (BOOL)nvmtlMeshResume {
    if (![self nvmtlBeginPassLoad:NVMTL_LOAD_ALL]) {
        _begun = NO; nvlog("mesh: render pass could not RESUME after the mesh stage - the rest of this encoder is not drawn"); return NO; }
    nvmtl_vk_cmd_set_cull(&_cb->_c, _cull);
    if (_hasVP) nvmtl_vk_cmd_set_viewport(&_cb->_c, _vp[0], _vp[1], _vp[2], _vp[3], _vp[4], _vp[5]);
    if (_hasSC) nvmtl_vk_cmd_set_scissor(&_cb->_c, (int32_t)_sc[0], (int32_t)_sc[1], _sc[2], _sc[3]);
    [self nvmtlApplyDepthStencil];
    return YES;
}
- (void)nvmtlMeshObjectDraw:(MTLSize)g object:(MTLSize)ot mesh:(MTLSize)mt info:(NVMTLMeshInfo *)mi {
    uint64_t nobj = (uint64_t)g.width * g.height * g.depth;
    if (!nobj || !ot.width || !ot.height || !ot.depth || !mt.width || !mt.height || !mt.depth) return;
    uint32_t tr = mi->_lay[7], rs = mi->_lay[8], block = mi->_lay[6];
    uint64_t bytes = tr + nobj * rs + nobj * (uint64_t)mi->_cap * block;
    if (bytes > 0x7fffffffull) { nvlog("mesh: %llu objects x (%u rec + %u x %u B) = %llu B scratch does not fit - NOT drawn", nobj, rs, mi->_cap, block, bytes); return; }
    uint32_t h[8] = { 0, (uint32_t)nobj, 1, 0,   0, (uint32_t)nobj, 0, 0 };
    NVMTLBuffer *sb = [self nvmtlMeshScratch:bytes header:h];
    if (!sb) return;
    [_cb->_resources addObject:mi->_k]; [_cb->_resources addObject:mi->_o]; [_cb->_resources addObject:_ps];
    NVMTLMeshBinds *mb = objc_getAssociatedObject(self, &kNVMTLMeshBinds);
    nvmtl_mesh_replay(self, mb ? mb->_obj : nil);
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 29, &sb->_b, 0);
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 30, &sb->_b, 0);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX, mi->_o->_function, _vbufAt, _vbufOff);
    uint32_t ol[3] = { (uint32_t)ot.width, (uint32_t)ot.height, (uint32_t)ot.depth };
    uint32_t tt[3] = { (uint32_t)(g.width * ot.width), (uint32_t)(g.height * ot.height), (uint32_t)(g.depth * ot.depth) };
    nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &mi->_o->_p, tt, ol);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    nvmtl_mesh_replay(self, mb ? mb->_mesh : nil);
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 29, &sb->_b, 0);
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 30, &sb->_b, 0);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX, mi->_k->_function, _vbufAt, _vbufOff);
    uint32_t ml[3] = { (uint32_t)mt.width, (uint32_t)mt.height, (uint32_t)mt.depth };
    nvmtl_vk_cmd_dispatch_indirect_tg(&_cb->_c, &mi->_k->_p, &sb->_b, 0, ml);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (![self nvmtlMeshResume]) return;
    MTLPrimitiveType pt = mi->_lay[2] == 3 ? MTLPrimitiveTypeTriangle : mi->_lay[2] == 2 ? MTLPrimitiveTypeLine : MTLPrimitiveTypePoint;
    static int told; if (told++ < 4) nvlog("mesh: \"%s\" %llu object threadgroup(s) x %ux%ux%u, mesh %ux%ux%u, cap %u -> %llu B scratch", mi->_name.UTF8String,
                                             nobj, ol[0], ol[1], ol[2], ml[0], ml[1], ml[2], mi->_cap, bytes);
    [self drawPrimitives:pt indirectBuffer:(id<MTLBuffer>)sb indirectBufferOffset:16];
}
- (void)drawMeshThreadgroups:(MTLSize)g threadsPerObjectThreadgroup:(MTLSize)ot threadsPerMeshThreadgroup:(MTLSize)mt {
    if (!_begun || !_ps) { nvlog("mesh: draw with no pipeline set"); return; }
    NVMTLMeshInfo *mi = objc_getAssociatedObject(_ps, &kNVMTLMeshInfo);
    if (!mi) { nvlog("mesh: drawMeshThreadgroups with a pipeline that has no mesh stage (%s) - NOT drawn", _ps->_fname.UTF8String); return; }
    if (mi->_o) { [self nvmtlMeshObjectDraw:g object:ot mesh:mt info:mi]; return; }
    uint64_t T = (uint64_t)g.width * g.height * g.depth, bytes = T * mi->_lay[6], verts = T * mi->_lay[1] * mi->_lay[2];
    if (!T || !mt.width || !mt.height || !mt.depth) return;
    if (bytes > 0x7fffffffull || verts > 0xffffffffull) { nvlog("mesh: %llu threadgroups x %u B does not fit - NOT drawn", T, mi->_lay[6]); return; }
    NVMTLBuffer *sb = (NVMTLBuffer *)[(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:(NSUInteger)bytes options:MTLResourceStorageModePrivate];
    if (!sb) { nvlog("mesh: %llu B scratch buffer allocation FAILED - NOT drawn", bytes); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:sb]; [_cb->_resources addObject:mi->_k]; [_cb->_resources addObject:_ps];
    nvmtl_vk_cmd_end_render(&_cb->_c);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (nvmtl_vk_cmd_fill_buffer(&_cb->_c, &sb->_b, 0, (size_t)bytes, 0)) nvlog("mesh: scratch clear FAILED - counts may be stale");
    nvmtl_vk_cmd_barrier(&_cb->_c);
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 30, &sb->_b, 0);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX, mi->_k->_function, _vbufAt, _vbufOff);
    uint32_t l[3] = { (uint32_t)mt.width, (uint32_t)mt.height, (uint32_t)mt.depth };
    uint32_t t[3] = { (uint32_t)(g.width * mt.width), (uint32_t)(g.height * mt.height), (uint32_t)(g.depth * mt.depth) };
    nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &mi->_k->_p, t, l);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (![self nvmtlBeginPassLoad:NVMTL_LOAD_ALL]) {
        _begun = NO; nvlog("mesh: render pass could not RESUME after the mesh stage - the rest of this encoder is not drawn"); return; }
    nvmtl_vk_cmd_set_cull(&_cb->_c, _cull);
    if (_hasVP) nvmtl_vk_cmd_set_viewport(&_cb->_c, _vp[0], _vp[1], _vp[2], _vp[3], _vp[4], _vp[5]);
    if (_hasSC) nvmtl_vk_cmd_set_scissor(&_cb->_c, (int32_t)_sc[0], (int32_t)_sc[1], _sc[2], _sc[3]);
    [self nvmtlApplyDepthStencil];
    MTLPrimitiveType pt = mi->_lay[2] == 3 ? MTLPrimitiveTypeTriangle : mi->_lay[2] == 2 ? MTLPrimitiveTypeLine : MTLPrimitiveTypePoint;
    static int told; if (told++ < 4) nvlog("mesh: \"%s\" %llu threadgroup(s) x %ux%ux%u -> %llu B scratch, %llu vertices", mi->_name.UTF8String,
                                             T, l[0], l[1], l[2], bytes, verts);
    [self drawPrimitives:pt vertexStart:0 vertexCount:(NSUInteger)verts instanceCount:1 baseInstance:0];
}
- (void)drawMeshThreads:(MTLSize)n threadsPerObjectThreadgroup:(MTLSize)ot threadsPerMeshThreadgroup:(MTLSize)mt {
    if (!mt.width || !mt.height || !mt.depth) return;
    MTLSize g = MTLSizeMake((n.width + mt.width - 1) / mt.width, (n.height + mt.height - 1) / mt.height, (n.depth + mt.depth - 1) / mt.depth);
    [self drawMeshThreadgroups:g threadsPerObjectThreadgroup:ot threadsPerMeshThreadgroup:mt];
}
- (void)drawMeshThreadgroupsWithIndirectBuffer:(id<MTLBuffer>)b indirectBufferOffset:(NSUInteger)off
    threadsPerObjectThreadgroup:(MTLSize)ot threadsPerMeshThreadgroup:(MTLSize)mt {
    if (!_begun || !_ps) { nvlog("mesh: indirect draw with no pipeline set"); return; }
    NVMTLMeshInfo *mi = objc_getAssociatedObject(_ps, &kNVMTLMeshInfo);
    if (!mi) { nvlog("mesh: drawMeshThreadgroupsWithIndirectBuffer: with a pipeline that has no mesh stage (%s) - NOT drawn", _ps->_fname.UTF8String); return; }
    static int told42c;
    if (mi->_o) { if (told42c++ < 4) nvlog("mesh: \"%s\" object stage + INDIRECT draw is batch 42c - NOT drawn", mi->_name.UTF8String); return; }
    if (!mi->_psi) { if (told42c++ < 4) nvlog("mesh: \"%s\" has no indirect variant (%s) - NOT drawn", mi->_name.UTF8String, mi->_indWhy.UTF8String ?: "?"); return; }
    NVMTLBuffer *ab = (NVMTLBuffer *)b;
    if (!ab || off + 12 > ab->_b.size) { nvlog("mesh: indirect buffer %p offset %lu does not hold 12 bytes - NOT drawn", (__bridge void *)b, (unsigned long)off); return; }
    if (!mt.width || !mt.height || !mt.depth) return;
    uint32_t tr = mi->_layi[7], block = mi->_layi[6];
    uint64_t bytes = tr + 16 + (uint64_t)mi->_capi * block;
    uint32_t h[8] = { 0, 1, 1, 0,   0, 1, 0, 0 };
    nvmtl_buffer_sync_in(ab);
    nvmtl_retain_resource(_cb, ab);
    NVMTLBuffer *sb = [self nvmtlMeshScratch:bytes header:h];
    if (!sb) return;
    if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &ab->_b, off, &sb->_b, tr, 12)) nvlog("mesh: indirect grid copy FAILED - the kernel reads a zero grid");
    nvmtl_vk_cmd_barrier(&_cb->_c);
    [_cb->_resources addObject:mi->_ki]; [_cb->_resources addObject:mi->_psi];
    nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 30, &sb->_b, 0);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX, mi->_ki->_function, _vbufAt, _vbufOff);
    uint32_t ml[3] = { (uint32_t)mt.width, (uint32_t)mt.height, (uint32_t)mt.depth };
    nvmtl_vk_cmd_dispatch_indirect_tg(&_cb->_c, &mi->_ki->_p, &ab->_b, off, ml);
    nvmtl_vk_cmd_barrier(&_cb->_c);
    if (![self nvmtlMeshResume]) return;
    MTLPrimitiveType pt = mi->_layi[2] == 3 ? MTLPrimitiveTypeTriangle : mi->_layi[2] == 2 ? MTLPrimitiveTypeLine : MTLPrimitiveTypePoint;
    static int told; if (told++ < 4) nvlog("mesh: \"%s\" INDIRECT from %p+%lu, mesh %ux%ux%u, cap %u -> %llu B scratch", mi->_name.UTF8String,
                                             (__bridge void *)b, (unsigned long)off, ml[0], ml[1], ml[2], mi->_capi, bytes);
    NVMTLRenderPipelineState *keep = _ps; _ps = mi->_psi;
    [self drawPrimitives:pt indirectBuffer:(id<MTLBuffer>)sb indirectBufferOffset:16];
    _ps = keep;
}
@end

@implementation NVMTLFence
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
@end

static _Atomic uint64_t gHAPlaced, gHAGiven, gHALive, gHAFallback, gHABacked, gHABackedBytes;
static int nvmtl_heap_grow(NVMTLHeap *h) {
    if (h->_nfr < h->_cfr) return 0;
    uint32_t nc = h->_cfr ? h->_cfr * 2 : 8;
    NSUInteger *n = realloc(h->_fr, (size_t)nc * 2 * sizeof(NSUInteger));
    if (!n) return -1;
    h->_fr = n; h->_cfr = nc; return 0;
}
static NSUInteger nvmtl_heap_take(NVMTLHeap *h, NSUInteger size, NSUInteger al) {
    NSUInteger got = NSUIntegerMax;
    if (!al) al = 1;
    os_unfair_lock_lock(&h->_fl);
    for (uint32_t i = 0; i < h->_nfr; i++) {
        NSUInteger ro = h->_fr[2 * i], rl = h->_fr[2 * i + 1], o = (ro + al - 1) / al * al;
        if (o < ro || o + size < o || o + size > ro + rl) continue;
        if (h->_lzc && size) {
            // a lazily backed heap: a resource that fits a slot must not straddle two (each slot may get its own memory);
            // a larger one starts on a slot boundary so it owns whole slots
            const NSUInteger c = h->_lzc;
            if (size > c ? o % c != 0 : o / c != (o + size - 1) / c) {
                NSUInteger o2 = (o / c + 1) * c; o2 = (o2 + al - 1) / al * al;
                if (o2 < o || o2 + size < o2 || o2 + size > ro + rl) continue;
                o = o2;
            }
        }
        NSUInteger lo = o - ro, hi = ro + rl - (o + size);
        if (lo && hi) {
            if (nvmtl_heap_grow(h)) break;
            memmove(&h->_fr[2 * (i + 1)], &h->_fr[2 * i], (size_t)(h->_nfr - i) * 2 * sizeof(NSUInteger));
            h->_fr[2 * i + 1] = lo; h->_fr[2 * (i + 1)] = o + size; h->_fr[2 * (i + 1) + 1] = hi; h->_nfr++;
        } else if (lo) h->_fr[2 * i + 1] = lo;
        else if (hi) { h->_fr[2 * i] = o + size; h->_fr[2 * i + 1] = hi; }
        else { memmove(&h->_fr[2 * i], &h->_fr[2 * (i + 1)], (size_t)(h->_nfr - i - 1) * 2 * sizeof(NSUInteger)); h->_nfr--; }
        got = o; break;
    }
    os_unfair_lock_unlock(&h->_fl);
    return got;
}
static const char *nvmtl_heap_give(NVMTLHeap *h, NSUInteger o, NSUInteger n) {
    const char *why = NULL;
    os_unfair_lock_lock(&h->_fl);
    uint32_t i = 0; while (i < h->_nfr && h->_fr[2 * i] < o) i++;
    int hasP = i > 0, hasN = i < h->_nfr;
    if (!n || o + n < o || o + n > h->_size) why = "outside the heap";
    else if (hasP && h->_fr[2 * (i - 1)] + h->_fr[2 * (i - 1) + 1] > o) why = "overlaps the free range before it (a double give)";
    else if (hasN && o + n > h->_fr[2 * i]) why = "overlaps the free range after it (a double give)";
    else {
        int mP = hasP && h->_fr[2 * (i - 1)] + h->_fr[2 * (i - 1) + 1] == o, mN = hasN && o + n == h->_fr[2 * i];
        if (mP && mN) { h->_fr[2 * (i - 1) + 1] += n + h->_fr[2 * i + 1];
                        memmove(&h->_fr[2 * i], &h->_fr[2 * (i + 1)], (size_t)(h->_nfr - i - 1) * 2 * sizeof(NSUInteger)); h->_nfr--; }
        else if (mP) h->_fr[2 * (i - 1) + 1] += n;
        else if (mN) { h->_fr[2 * i] = o; h->_fr[2 * i + 1] += n; }
        else if (nvmtl_heap_grow(h)) why = "the free list could not grow (the range stays out of circulation)";
        else { memmove(&h->_fr[2 * (i + 1)], &h->_fr[2 * i], (size_t)(h->_nfr - i) * 2 * sizeof(NSUInteger));
               h->_fr[2 * i] = o; h->_fr[2 * i + 1] = n; h->_nfr++; }
    }
    os_unfair_lock_unlock(&h->_fl);
    return why;
}
static int nvmtl_heap_take_at_locked(NVMTLHeap *h, NSUInteger o, NSUInteger size) {
    if (!size || o + size < o) return -1;
    for (uint32_t i = 0; i < h->_nfr; i++) {
        NSUInteger ro = h->_fr[2 * i], rl = h->_fr[2 * i + 1];
        if (ro > o) break;
        if (o + size > ro + rl) continue;
        NSUInteger lo = o - ro, hi = ro + rl - (o + size);
        if (lo && hi) {
            if (nvmtl_heap_grow(h)) return -1;
            memmove(&h->_fr[2 * (i + 1)], &h->_fr[2 * i], (size_t)(h->_nfr - i) * 2 * sizeof(NSUInteger));
            h->_fr[2 * i + 1] = lo; h->_fr[2 * (i + 1)] = o + size; h->_fr[2 * (i + 1) + 1] = hi; h->_nfr++;
        } else if (lo) h->_fr[2 * i + 1] = lo;
        else if (hi) { h->_fr[2 * i] = o + size; h->_fr[2 * i + 1] = hi; }
        else { memmove(&h->_fr[2 * i], &h->_fr[2 * (i + 1)], (size_t)(h->_nfr - i - 1) * 2 * sizeof(NSUInteger)); h->_nfr--; }
        return 0;
    }
    return -1;
}
#define NVMTL_HEAPPARK 8
static _Atomic uint64_t gHTPlaced, gHTRecycled, gHTParked, gHTEvicted, gHTFallback, gHTMisfit;
static void nvmtl_heaptex_census(const char *what) {
    const uint64_t n = atomic_load(&gHTPlaced) + atomic_load(&gHTRecycled) + atomic_load(&gHTFallback);
    if (n & (n - 1)) return;
    nvlog("heap texture (%s): %llu placed new, %llu recycled, %llu parked, %llu evicted, %llu took the legacy fresh path, %llu misfit",
          what, (unsigned long long)atomic_load(&gHTPlaced), (unsigned long long)atomic_load(&gHTRecycled),
          (unsigned long long)atomic_load(&gHTParked), (unsigned long long)atomic_load(&gHTEvicted),
          (unsigned long long)atomic_load(&gHTFallback), (unsigned long long)atomic_load(&gHTMisfit));
}
static int nvmtl_heap_unpark(NVMTLHeap *h, const uint32_t *key, nvk_image *out, NSUInteger *off, NSUInteger *len) {
    int got = -1;
    os_unfair_lock_lock(&h->_fl);
    for (uint32_t k = h->_npk; k-- > 0; ) {
        nvmtl_park_t *p = &h->_pk[k];
        if (memcmp(p->key, key, sizeof p->key) || nvmtl_heap_take_at_locked(h, p->off, p->len)) continue;
        *out = p->img; *off = p->off; *len = p->len; nvmtl_vk_image_recycled(out);
        memmove(p, p + 1, (size_t)(h->_npk - k - 1) * sizeof *p); h->_npk--; got = 0; break;
    }
    os_unfair_lock_unlock(&h->_fl);
    return got;
}
static void nvmtl_heap_park(NVMTLHeap *h, nvk_image *img, const uint32_t *key, NSUInteger off, NSUInteger len) {
    nvk_image ev; int evict = 0, parked = 0;
    os_unfair_lock_lock(&h->_fl);
    if (!h->_pk) h->_pk = calloc(NVMTL_HEAPPARK, sizeof *h->_pk);
    if (h->_pk) {
        if (h->_npk == NVMTL_HEAPPARK) { ev = h->_pk[0].img; evict = 1;
            memmove(&h->_pk[0], &h->_pk[1], (NVMTL_HEAPPARK - 1) * sizeof *h->_pk); h->_npk--; }
        nvmtl_park_t *p = &h->_pk[h->_npk++]; p->img = *img; memcpy(p->key, key, sizeof p->key); p->off = off; p->len = len;
        parked = 1;
    }
    os_unfair_lock_unlock(&h->_fl);
    if (parked) { nvmtl_vk_image_parked(img); memset(img, 0, sizeof *img); atomic_fetch_add(&gHTParked, 1); }
    if (evict) { nvmtl_vk_image_destroy(&ev); atomic_fetch_add(&gHTEvicted, 1); }
}
static void nvmtl_heap_census(const char *what) {
    nvlog("heap alias (%s): %llu placed, %llu given back, %llu live = %llu MB bound in %llu backed heaps of %llu MB;"
          " %llu took the legacy fresh path", what,
          (unsigned long long)atomic_load(&gHAPlaced), (unsigned long long)atomic_load(&gHAGiven),
          (unsigned long long)(atomic_load(&gHAPlaced) - atomic_load(&gHAGiven)), (unsigned long long)(atomic_load(&gHALive) >> 20),
          (unsigned long long)atomic_load(&gHABacked), (unsigned long long)(atomic_load(&gHABackedBytes) >> 20),
          (unsigned long long)atomic_load(&gHAFallback));
}
// Every live app heap, for the device-loss report: the struct, its VkDeviceMemory, size, use, free ranges and park.
static NSHashTable *gHeaps; static os_unfair_lock gHeapsLock = OS_UNFAIR_LOCK_INIT;
static void nvmtl_heap_list_for_loss(NVMTLHeap *h) {
    os_unfair_lock_lock(&gHeapsLock);
    if (!gHeaps) gHeaps = [NSHashTable weakObjectsHashTable];
    [gHeaps addObject:h];
    os_unfair_lock_unlock(&gHeapsLock);
}
void nvmtl_heaps_dump(FILE *f, void (*emit)(void *, const char *)) {
    NSArray *all = nil;
    if (os_unfair_lock_trylock(&gHeapsLock)) { all = gHeaps.allObjects; os_unfair_lock_unlock(&gHeapsLock); }
    else { emit(f, "fault context: the heap registry is locked by another thread - heaps not listed"); return; }
    char line[600]; unsigned n = 0;
    for (NVMTLHeap *h in all) {
        size_t lzb = 0; for (uint32_t k = 0; k < h->_lzn; k++) lzb += h->_lzm[k].size;
        int o = snprintf(line, sizeof line, "fault context: heap %p \"%s\": VkDeviceMemory %p, size %lu, used %lu, %s, lazy %u memories %zu bytes, park %u, free ranges (%u):",
                         (__bridge void *)h, h.label.UTF8String ?: "", h->_hm.mem, (unsigned long)(h->_lzc ? h->_size : h->_hm.size), (unsigned long)h->_used, h->_backed ? "backed" : "legacy (not backed)",
                         h->_lzn, lzb, h->_npk, h->_nfr);
        if (os_unfair_lock_trylock(&h->_fl)) {
            for (uint32_t i = 0; i < h->_nfr && o < (int)sizeof line - 40; i++) o += snprintf(line + o, sizeof line - (size_t)o, " %lu+%lu", (unsigned long)h->_fr[2 * i], (unsigned long)h->_fr[2 * i + 1]);
            os_unfair_lock_unlock(&h->_fl);
        } else o += snprintf(line + o, sizeof line - (size_t)o, " (free list locked)");
        emit(f, line); n++;
    }
    snprintf(line, sizeof line, "fault context: %u live app heaps listed", n); emit(f, line);
}
static const size_t kLazySlot = 64u << 20, kLazyMin = 256u << 20;
static _Atomic uint64_t gHLazyHeaps, gHLazyReserved, gHLazyBacked, gHLazyMixed;
static int nvmtl_heap_lazy_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_HEAP_LAZY"); on = !(e && e[0] == '0');
                  nvlog("heap lazy backing: %s (automatic heaps of %zu MB and up take memory in %zu MB slots as resources land)",
                        on ? "ON" : "OFF (NVMTL_HEAP_LAZY=0)", kLazyMin >> 20, kLazySlot >> 20); }
    return on;
}
/* The memory and the offset inside it for a resource at heap offset off, length len. A non-lazy heap answers its one
 * _hm. A lazy heap answers the backing that owns every slot the range touches, creating one when none of them has memory
 * yet; NULL when the slots belong to different backings (the caller gives the range back and takes the legacy path). */
static nvk_heap *nvmtl_heap_mem_for(NVMTLHeap *h, NSUInteger off, NSUInteger len, NSUInteger *rel) {
    if (!h->_lzc) { *rel = off; return &h->_hm; }
    if (!len || off + len < off || off + len > h->_size) return NULL;
    const uint32_t s0 = (uint32_t)(off / h->_lzc), s1 = (uint32_t)((off + len - 1) / h->_lzc);
    nvk_heap *got = NULL;
    os_unfair_lock_lock(&h->_lzl);
    uint16_t own = h->_lzo[s0]; int same = 1;
    for (uint32_t s = s0 + 1; s <= s1; s++) if (h->_lzo[s] != own) { same = 0; break; }
    if (same && own) { got = &h->_lzm[own - 1]; *rel = off - h->_lzb[own - 1]; }
    else if (same && !own && h->_lzn < 65535) {
        if (h->_lzn == h->_lzcap) {
            const uint32_t nc = h->_lzcap ? h->_lzcap * 2 : 8;
            nvk_heap *nm = realloc(h->_lzm, nc * sizeof *nm); if (nm) h->_lzm = nm;
            size_t *nb = realloc(h->_lzb, nc * sizeof *nb); if (nb) h->_lzb = nb;
            if (nm && nb) h->_lzcap = nc;
        }
        if (h->_lzn < h->_lzcap) {
            const size_t base = (size_t)s0 * h->_lzc, end = (size_t)(s1 + 1) * h->_lzc, top = end < h->_size ? end : h->_size;
            nvk_heap *m = &h->_lzm[h->_lzn]; memset(m, 0, sizeof *m);
            if (!nvmtl_vk_heap_create(top - base, h->_storage == MTLStorageModeShared, m)) {
                nvmtl_vk_heap_zero(m, h->_storage == MTLStorageModeShared ? "shared slot" : "private slot");
                h->_lzb[h->_lzn] = base; h->_lzn++;
                for (uint32_t s = s0; s <= s1; s++) h->_lzo[s] = (uint16_t)h->_lzn;
                atomic_fetch_add(&gHLazyBacked, top - base);
                got = m; *rel = off - base;
            } else nvlog("heap lazy backing: %zu bytes for slots %u-%u of a %lu-byte heap REFUSED - legacy path", top - base, s0, s1, (unsigned long)h->_size);
        }
    } else if (!same) {
        const uint64_t n = atomic_fetch_add(&gHLazyMixed, 1);
        if (n < 4) nvlog("heap lazy backing: %lu+%lu spans slots %u-%u with different memories - legacy path", (unsigned long)off, (unsigned long)len, s0, s1);
    }
    os_unfair_lock_unlock(&h->_lzl);
    return got;
}
void nvmtl_heap_lazy_census(const char *why) {
    nvlog_every("heap lazy backing (%s): %llu heaps, %llu MB reserved by the app, %llu MB given memory, %llu mixed-slot fallbacks", why,
                (unsigned long long)gHLazyHeaps, (unsigned long long)(gHLazyReserved >> 20), (unsigned long long)(gHLazyBacked >> 20),
                (unsigned long long)gHLazyMixed);
}
void nvmtl_heap_back(NVMTLHeap *h, NSUInteger size, int shared) {
    if (size >= kLazyMin && nvmtl_heap_lazy_on()) {
        h->_lzslots = (uint32_t)((size + kLazySlot - 1) / kLazySlot);
        h->_lzo = calloc(h->_lzslots, sizeof *h->_lzo);
        if (h->_lzo) {
            h->_lzc = kLazySlot; h->_lzl = OS_UNFAIR_LOCK_INIT; memset(&h->_hm, 0, sizeof h->_hm);
            nvmtl_heap_list_for_loss(h);
            h->_fl = OS_UNFAIR_LOCK_INIT; h->_nfr = 0; h->_cfr = 0; h->_fr = NULL;
            if (nvmtl_heap_grow(h)) { free(h->_lzo); h->_lzo = NULL; h->_lzc = 0; nvlog("heap alias: no free list for a %lu-byte heap - legacy path", (unsigned long)size); return; }
            h->_fr[0] = 0; h->_fr[1] = size; h->_nfr = 1; h->_backed = YES;
            atomic_fetch_add(&gHABacked, 1); atomic_fetch_add(&gHLazyHeaps, 1); atomic_fetch_add(&gHLazyReserved, size);
            nvlog_every("heap lazy backing: %s heap of %lu bytes = %u slots, memory on first use", shared ? "shared" : "private", (unsigned long)size, h->_lzslots);
            return;
        }
    }
    if (nvmtl_vk_heap_create(size, shared, &h->_hm)) {
        nvlog("heap alias: an automatic %s heap of %lu bytes got no memory of its own - its resources take the legacy fresh path",
              shared ? "shared" : "private", (unsigned long)size);
        return;
    }
    nvmtl_vk_heap_zero(&h->_hm, shared ? "shared" : "private");
    nvmtl_heap_list_for_loss(h);
    h->_fl = OS_UNFAIR_LOCK_INIT; h->_nfr = 0; h->_cfr = 0; h->_fr = NULL;
    if (nvmtl_heap_grow(h)) { nvmtl_vk_heap_destroy(&h->_hm); nvlog("heap alias: no free list for a %lu-byte heap - legacy path", (unsigned long)size); return; }
    h->_fr[0] = 0; h->_fr[1] = size; h->_nfr = 1; h->_backed = YES;
    atomic_fetch_add(&gHABacked, 1); atomic_fetch_add(&gHABackedBytes, size);
}
@implementation NVMTLHeap
- (void)nvmtlGiveRange:(NSUInteger)off length:(NSUInteger)len {
    const char *why = nvmtl_heap_give(self, off, len);
    if (why) { nvlog("heap alias: range %lu+%lu NOT given back - %s", (unsigned long)off, (unsigned long)len, why); return; }
    atomic_fetch_add(&gHAGiven, 1); atomic_fetch_sub(&gHALive, len);
}
- (id<MTLBuffer>)nvmtlPlacedBuffer:(NSUInteger)len options:(MTLResourceOptions)opt charge:(NSUInteger)charge {
    MTLStorageMode sm = (MTLStorageMode)((opt >> MTLResourceStorageModeShift) & 0xF);
    if (sm != _storage) {
        static int said; if (said++ < 2) nvlog("heap alias: a storage-%d buffer from a storage-%d heap - legacy path", (int)sm, (int)_storage);
        return nil;
    }
    NSUInteger al = [_dev heapBufferSizeAndAlignWithLength:len options:opt].align;
    NSUInteger off = nvmtl_heap_take(self, charge, al);
    if (off == NSUIntegerMax) {
        nvlog("heap alias: no free range of %lu (align %lu) in a %lu-byte heap with %lu charged - legacy path",
              (unsigned long)charge, (unsigned long)al, (unsigned long)_size, (unsigned long)_used);
        return nil;
    }
    NVMTLBuffer *b = [NVMTLBuffer new];
    b->_storage = sm;
    b->_ropt = opt & ((MTLResourceOptions)0xF | ((MTLResourceOptions)0x3 << MTLResourceHazardTrackingModeShift));
    NSUInteger rel = off; nvk_heap *mem = nvmtl_heap_mem_for(self, off, charge, &rel);
    if (!mem || nvmtl_vk_buffer_create_placed(len, mem, rel, &b->_b)) {
        const char *why = nvmtl_heap_give(self, off, charge);
        nvlog("heap alias: bind of %lu at %lu FAILED - legacy path%s%s", (unsigned long)len, (unsigned long)off, why ? "; range kept out: " : "", why ? why : "");
        return nil;
    }
    b->_subHeap = self; b->_subCharge = charge; b->_subPlaced = YES; b->_heapOffset = off; _used += charge;
    nvmtl_heap_register(self, b);
    atomic_fetch_add(&gHAPlaced, 1); atomic_fetch_add(&gHALive, charge);
    nvmtl_heap_census("placed");
    return b;
}
- (unsigned long long)protectionOptions { return 0; }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSUInteger)size { return _size; }
- (NSUInteger)usedSize { return _used; }
- (NSUInteger)currentAllocatedSize { return _size; }
- (MTLStorageMode)storageMode { return _storage; }
- (MTLCPUCacheMode)cpuCacheMode { return _cache; }
- (MTLHeapType)type { return _type; }
- (MTLHazardTrackingMode)hazardTrackingMode { return _hazard == MTLHazardTrackingModeTracked ? MTLHazardTrackingModeTracked : MTLHazardTrackingModeUntracked; }
- (MTLResourceOptions)resourceOptions { return ((MTLResourceOptions)_storage << MTLResourceStorageModeShift) | (MTLResourceOptions)_cache
                                            | ((MTLResourceOptions)[self hazardTrackingMode] << MTLResourceHazardTrackingModeShift); }
- (NSUInteger)allocatedSize { return _size; }
- (void)dealloc { for (uint32_t k = 0; k < _npk; k++) nvmtl_vk_image_destroy(&_pk[k].img);
                  free(_pk); _pk = NULL; _npk = 0;
                  nvmtl_vk_heap_destroy(&_hm); free(_fr); _fr = NULL; free(_poolBits); _poolBits = NULL;
                  for (uint32_t k = 0; k < _lzn; k++) nvmtl_vk_heap_destroy(&_lzm[k]);
                  free(_lzm); free(_lzb); free(_lzo); _lzm = NULL; _lzb = NULL; _lzo = NULL; _lzn = 0; }
- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)len options:(MTLResourceOptions)opt offset:(NSUInteger)off {
    if (_type != MTLHeapTypePlacement) { nvlog("heap newBufferWithLength:offset: on a non-placement heap"); return nil; }
    NVMTLBuffer *b = [NVMTLBuffer new];
    b->_storage = (MTLStorageMode)((opt >> MTLResourceStorageModeShift) & 0xF);
    if (nvmtl_vk_buffer_create_placed(len, &_hm, off, &b->_b)) { nvlog("heap newBufferWithLength:%lu offset:%lu FAILED", (unsigned long)len, (unsigned long)off); return nil; }
    b->_heap = self; b->_heapOffset = off; if (off + len > _used) _used = off + len;
    nvmtl_heap_register(self, b);
    nvlog("heap newBufferWithLength:%lu offset:%lu -> %p", (unsigned long)len, (unsigned long)off, (__bridge void *)b);
    return b;
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)off {
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    extern int nvmtl_depthfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect);
    extern nvk_queue *nvmtl_device_queue(void);
    if (_type != MTLHeapTypePlacement) { nvlog("heap newTextureWithDescriptor:offset: on a non-placement heap"); return nil; }
    if (!d) return nil;
    NVMTLTexture *t = [NVMTLTexture new];
    t->_fmt = d.pixelFormat; t->_mips = (uint32_t)(d.mipmapLevelCount ? d.mipmapLevelCount : 1);
    t->_usage = d.usage;
    uint32_t dvk = 0, dasp = 0;
    if (nvmtl_depthfmt_public(d.pixelFormat, &dvk, &dasp) == 0) {
        if (nvmtl_vk_depth_create_ex_placed((uint32_t)d.width, (uint32_t)d.height, dvk, dasp, &_hm, off, &t->_i)) { nvlog("heap depth texture FAILED (format %lu offset %lu)", (unsigned long)d.pixelFormat, (unsigned long)off); return nil; }
        t->_q = nvmtl_device_queue(); t->_heap = self; t->_heapOffset = off;
        nvmtl_heap_register(self, t);
        nvlog("heap newTextureWithDescriptor: %lux%lu DEPTH offset:%lu -> %p", (unsigned long)d.width, (unsigned long)d.height, (unsigned long)off, (__bridge void *)t);
        return t;
    }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8)) return nil;
    const BOOL rbSwapPl = nvmtl_bgr10_storage_remap(d, &vkf);
    int wantStex = (d.usage & MTLTextureUsageShaderWrite) != 0;
    if (nvmtl_vk_image_create_typed_placed((uint32_t)d.width, (uint32_t)d.height, vkf, bpp, a8, (uint32_t)d.mipmapLevelCount, wantStex,
                                            (uint32_t)d.textureType, nvmtl_vk_layers_for_desc((uint32_t)d.textureType, (uint32_t)d.arrayLength, (uint32_t)d.depth), &_hm, off, &t->_i)) {
        nvlog("heap newTextureWithDescriptor: %lux%lu offset:%lu FAILED", (unsigned long)d.width, (unsigned long)d.height, (unsigned long)off); return nil; }
    t->_q = nvmtl_device_queue(); t->_stex = t->_i.storage == 1; t->_rbSwap = rbSwapPl; t->_heap = self; t->_heapOffset = off;
    nvmtl_heap_register(self, t);
    nvmtl_texture_desc_swizzle(t, d);
    nvlog("heap newTextureWithDescriptor: %lux%lu%s offset:%lu -> %p", (unsigned long)d.width, (unsigned long)d.height, t->_stex ? " +storage" : "", (unsigned long)off, (__bridge void *)t);
    return t;
}
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return nvmtl_purge(&_purge, s); }
- (void)nvmtlReleaseSubAllocation:(NSUInteger)bytes {
    if (bytes > _used) {
        nvlog("heap release %lu but only %lu charged - accounting underflow, clamped to 0", (unsigned long)bytes, (unsigned long)_used);
        _used = 0; return;
    }
    _used -= bytes;
}
- (NSUInteger)maxAvailableSizeWithAlignment:(NSUInteger)a {
    NSUInteger left = _size > _used ? _size - _used : 0;
    return (a > 1 && left % a) ? left - (left % a) : left;
}
- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)len options:(MTLResourceOptions)opt {
    NSUInteger charge = [_dev heapBufferSizeAndAlignWithLength:len options:opt].size;
    if (charge < len) charge = len;
    if (charge > [self maxAvailableSizeWithAlignment:0] && nvmtl_heap_unkeep(self)) nvlog("heap newBufferWithLength:%lu: out of room - let the kept heap pin go (%lu left now)", (unsigned long)len, (unsigned long)[self maxAvailableSizeWithAlignment:0]);
    if (charge > [self maxAvailableSizeWithAlignment:0]) { nvlog("heap newBufferWithLength:%lu (charge %lu) — only %lu left of %lu", (unsigned long)len, (unsigned long)charge, (unsigned long)(_size > _used ? _size - _used : 0), (unsigned long)_size); return nil; }
    if (_backed) { id<MTLBuffer> pb = [self nvmtlPlacedBuffer:len options:opt charge:charge]; if (pb) return pb;
                   if (nvmtl_heap_unkeep(self) && (pb = [self nvmtlPlacedBuffer:len options:opt charge:charge])) return pb;
                   atomic_fetch_add(&gHAFallback, 1); nvmtl_heap_census("fallback"); }
    gZeroSkip = 1; id<MTLBuffer> b = [_dev newBufferWithLength:len options:opt]; gZeroSkip = 0;
    if (!b) return nil;
    if (![(id)b isKindOfClass:[NVMTLBuffer class]]) {
        static int said; if (said++ < 2) nvlog("heap sub-allocation is a %s, not ours — UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)b class]) UTF8String]);
        return b;
    }
    NVMTLBuffer *nb = (NVMTLBuffer *)b; nb->_subHeap = self; nb->_subCharge = charge; _used += charge; _hzLegacy = YES;
    nvmtl_heap_register(self, nb);
    return b;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithSize:(NSUInteger)size {
    NSUInteger charge = [_dev heapAccelerationStructureSizeAndAlignWithSize:size].size;
    if (charge < size) charge = size;
    if (charge > [self maxAvailableSizeWithAlignment:0]) nvmtl_heap_unkeep(self);
    if (charge > [self maxAvailableSizeWithAlignment:0]) { nvlog("heap newAccelerationStructureWithSize:%lu (charge %lu) - only %lu left of %lu", (unsigned long)size, (unsigned long)charge, (unsigned long)(_size > _used ? _size - _used : 0), (unsigned long)_size); return nil; }
    id<MTLAccelerationStructure> as = [_dev newAccelerationStructureWithSize:size];
    if (!as) return nil;
    if (![(id)as isKindOfClass:[NVMTLAccelerationStructure class]]) {
        static int said; if (said++ < 2) nvlog("heap structure is a %s, not ours - UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)as class]) UTF8String]);
        return as;
    }
    NVMTLAccelerationStructure *na = (NVMTLAccelerationStructure *)as; na->_subHeap = self; na->_subCharge = charge; _used += charge; _hzLegacy = YES;
    nvmtl_heap_register(self, na);
    nvlog("heap newAccelerationStructureWithSize:%lu (charge %lu) -> %p", (unsigned long)size, (unsigned long)charge, (__bridge void *)na);
    return as;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithDescriptor:(MTLAccelerationStructureDescriptor *)desc {
    MTLAccelerationStructureSizes s = [_dev accelerationStructureSizesWithDescriptor:desc];
    return s.accelerationStructureSize ? [self newAccelerationStructureWithSize:s.accelerationStructureSize] : nil;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithSize:(NSUInteger)size offset:(NSUInteger)off {
    if (_type != MTLHeapTypePlacement) { nvlog("heap newAccelerationStructureWithSize:offset: on a non-placement heap"); return nil; }
    if (nvmtl_vk_init() || !nvmtl_vk_rt_available()) { nvlog("heap newAccelerationStructureWithSize:offset: no ray tracing on this NVK"); return nil; }
    if (off & 255) { nvlog("heap newAccelerationStructureWithSize:%lu offset:%lu - offset is not 256-aligned (heapAccelerationStructureSizeAndAlign says 256) - REFUSED", (unsigned long)size, (unsigned long)off); return nil; }
    const size_t len = size < 256 ? 256 : size;
    NVMTLAccelerationStructure *a = [NVMTLAccelerationStructure new];
    a->_storage = MTLStorageModePrivate;
    if (nvmtl_vk_buffer_create_placed(len, &_hm, off, &a->_b)) { nvlog("heap newAccelerationStructureWithSize:%lu offset:%lu FAILED", (unsigned long)size, (unsigned long)off); return nil; }
    a->_heap = self; a->_heapOffset = off; if (off + len > _used) _used = off + len;
    nvmtl_heap_register(self, a);
    nvlog("heap newAccelerationStructureWithSize:%lu offset:%lu -> %p", (unsigned long)size, (unsigned long)off, (__bridge void *)a);
    return a;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithDescriptor:(MTLAccelerationStructureDescriptor *)desc offset:(NSUInteger)off {
    MTLAccelerationStructureSizes s = [_dev accelerationStructureSizesWithDescriptor:desc];
    return s.accelerationStructureSize ? [self newAccelerationStructureWithSize:s.accelerationStructureSize offset:off] : nil;
}
- (id<MTLTexture>)nvmtlPlacedTexture:(MTLTextureDescriptor *)d charge:(NSUInteger)charge align:(NSUInteger)al {
    extern int nvmtl_pixfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8);
    extern int nvmtl_depthfmt_public(MTLPixelFormat f, uint32_t *vk, uint32_t *aspect);
    extern nvk_queue *nvmtl_device_queue(void);
    static int noPlace = -1, noRecycle = -1;
    if (noPlace < 0) { noPlace = getenv("NVMTL_NO_HEAPTEX_PLACE") != NULL; noRecycle = getenv("NVMTL_NO_HEAPTEX_RECYCLE") != NULL;
        nvlog("heap texture: dres R1 placement %s, recycle %s", noPlace ? "OFF (NVMTL_NO_HEAPTEX_PLACE)" : "ON",
              noRecycle ? "OFF (NVMTL_NO_HEAPTEX_RECYCLE)" : "ON"); }
    if (noPlace) return nil;
    if (_storage != MTLStorageModePrivate || d.storageMode != MTLStorageModePrivate
        || d.textureType == MTLTextureType2DMultisample || d.sampleCount > 1
        || !d.width || !d.height || d.width > UINT32_MAX || d.height > UINT32_MAX) return nil;
    uint32_t dvk = 0, dasp = 0, vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_depthfmt_public(d.pixelFormat, &dvk, &dasp) == 0 || nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8)) return nil;
    const BOOL rbSwapPl = nvmtl_bgr10_storage_remap(d, &vkf);
    const uint32_t w = (uint32_t)d.width, hh = (uint32_t)d.height, mips = (uint32_t)(d.mipmapLevelCount ? d.mipmapLevelCount : 1);
    const uint32_t type = (uint32_t)d.textureType, layers = nvmtl_vk_layers_for_desc(type, (uint32_t)d.arrayLength, (uint32_t)d.depth);
    const int wantStex = (d.usage & MTLTextureUsageShaderWrite) != 0;
    const uint32_t key[12] = { 1, w, hh, vkf, bpp, (uint32_t)a8, mips, (uint32_t)wantStex, type, layers, 0, 0 };
    NVMTLTexture *t = [NVMTLTexture new];
    NSUInteger off = 0, len = charge; BOOL rec = NO;
    if (!noRecycle && _npk && nvmtl_heap_unpark(self, key, &t->_i, &off, &len) == 0) {
        rec = YES; t->_i.layout = 0;
    } else {
        off = nvmtl_heap_take(self, charge, al);
        if (off == NSUIntegerMax) {
            static int said; if (said++ < 4) nvlog("heap texture: no free range of %lu (align %lu) in a %lu-byte heap with %lu charged - legacy path",
                                                   (unsigned long)charge, (unsigned long)al, (unsigned long)_size, (unsigned long)_used);
            return nil; }
        NSUInteger rel = off; nvk_heap *mem = nvmtl_heap_mem_for(self, off, charge, &rel);
        if (!mem || nvmtl_vk_image_create_typed_placed(w, hh, vkf, bpp, a8, mips, wantStex, type, layers, mem, rel, &t->_i)
            || !nvmtl_vk_image_placed_fits(&t->_i, charge)) {
            if (t->_i.img) { nvmtl_vk_image_destroy(&t->_i); atomic_fetch_add(&gHTMisfit, 1); }
            const char *why = nvmtl_heap_give(self, off, charge);
            nvlog("heap texture: %ux%u fmt %lu at %lu NOT placed - legacy path%s%s", w, hh, (unsigned long)d.pixelFormat, (unsigned long)off,
                  why ? "; range kept out: " : "", why ? why : "");
            return nil; }
    }
    t->_ropt = (MTLResourceOptions)d.cpuCacheMode | ((MTLResourceOptions)d.hazardTrackingMode << MTLResourceHazardTrackingModeShift)
             | ((MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift);
    t->_fmt = d.pixelFormat; t->_mips = mips; t->_usage = d.usage;
    t->_q = nvmtl_device_queue(); t->_stex = t->_i.storage == 1; t->_rbSwap = rbSwapPl;
    t->_subHeap = self; t->_subCharge = len; t->_subPlaced = YES; t->_heapOffset = off;
    nvmtl_heap_register(self, t);
    t->_parkable = !noRecycle; memcpy(t->_pkey, key, sizeof key); t->_pkLen = len;
    _used += len;
    atomic_fetch_add(&gHAPlaced, 1); atomic_fetch_add(&gHALive, len);
    atomic_fetch_add(rec ? &gHTRecycled : &gHTPlaced, 1); nvmtl_heaptex_census(rec ? "recycled" : "placed");
    // automatic heaps land here, not in -newTextureWithDescriptor:offset:; without this the descriptor's swizzle was lost
    nvmtl_texture_desc_swizzle(t, d);
    return t;
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d {
    if (!d) return nil;
    if (_sparse) return nvmtl_sparse_heap_texture(self, d);   // a sparse heap: a texture whose blocks are mapped later
    MTLSizeAndAlign sa = [_dev heapTextureSizeAndAlignWithDescriptor:d];
    NSUInteger charge = sa.size;
    if (_backed && charge) { id<MTLTexture> pt = [self nvmtlPlacedTexture:d charge:charge align:sa.align]; if (pt) return pt;
                             if (nvmtl_heap_unkeep(self) && (pt = [self nvmtlPlacedTexture:d charge:charge align:sa.align])) return pt;
                             atomic_fetch_add(&gHTFallback, 1); nvmtl_heaptex_census("fallback"); }
    if (!charge) charge = d.width * d.height * 4;
    id<MTLTexture> t = [_dev newTextureWithDescriptor:d];
    if (!t) return nil;
    if (![(id)t isKindOfClass:[NVMTLTexture class]]) {
        static int said; if (said++ < 2) nvlog("heap sub-allocation is a %s, not ours — UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)t class]) UTF8String]);
        return t;
    }
    NVMTLTexture *nt = (NVMTLTexture *)t; nt->_subHeap = self; nt->_subCharge = charge; _used += charge; _hzLegacy = YES;
    nvmtl_heap_register(self, nt);
    return t;
}
@end

@implementation NVMTLDepthStencilState
@dynamic gpuResourceID;
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (void)setLabel:(NSString *)l {}
- (BOOL)readsDepth { return _compare != 7 || _write; }
- (BOOL)writesDepth { return _write != 0; }
- (BOOL)readsStencil { return _stencil != 0; }
- (BOOL)writesStencil { if (!_stencil) return NO; for (int k = 1; k <= 3; k++) if ((_sf[k] && _sf[5]) || (_sb[k] && _sb[5])) return YES; return NO; }
@end

@implementation NVMTLSamplerState
- (MTLResourceID)gpuResourceID {
    @synchronized (self) { if (!_resid) _resid = nvmtl_resid_for_sampler(self); }
    MTLResourceID rid; rid._impl = _resid; return rid;
}
- (void)dealloc { nvmtl_vk_sampler_destroy(&_s); }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
@end

#pragma mark - compute / blit
@implementation NVMTLComputePipelineState
@dynamic allocatedSize, reflection;
- (void)dealloc { nvmtl_vk_pipeline_destroy(&_p); }
- (BOOL)supportIndirectCommandBuffers { return _icb; }
- (MTLResourceID)gpuResourceID {
    MTLResourceID r; r._impl = _icb ? nvmtl_resid_pipe(self, &_resid) : 0; return r; }
- (NSUInteger)imageblockMemoryLengthForDimensions:(MTLSize)d { return 0; }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (MTLShaderValidation)shaderValidation { return MTLShaderValidationDisabled; }
static NSUInteger nvmtl_env_uint(NSString *k, NSUInteger dflt) {
    NSString *s = [[NSProcessInfo processInfo] environment][k];
    return s.length ? (NSUInteger)s.integerValue : dflt;
}
static NSUInteger nvmtl_compute_maxt(void) { static NSUInteger v; if (!v) v = nvmtl_env_uint(@"NVMTL_MAXT", 1024); return v; }
// A required threadgroup size fixes the total too: Apple answers 64 for a pipeline that requires 64x1x1.
- (NSUInteger)maxTotalThreadsPerThreadgroup { NSUInteger v = nvmtl_compute_maxt(), n = _attrT ?: _askT;
    NSUInteger rq = _reqTG.width * _reqTG.height * _reqTG.depth; if (rq && (!n || rq < n)) n = rq;
    return n && n < v ? n : v; }
- (MTLSize)requiredThreadsPerThreadgroup { return _reqTG; }
- (NSUInteger)threadExecutionWidth { static NSUInteger v; if (!v) v = nvmtl_env_uint(@"NVMTL_TEW", 32); return v; }
- (NSUInteger)staticThreadgroupMemoryLength { return 0; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l {}
// intersection function tables (formerly the RT3IntersectionTables category in NVMTLLibraryLoad.m)
- (id<MTLIntersectionFunctionTable>)newIntersectionFunctionTableWithDescriptor:(MTLIntersectionFunctionTableDescriptor *)d {
    return nvmtl_ift_new(d, "newIntersectionFunctionTableWithDescriptor");
}
// function pointers and visible function tables (formerly the B39FunctionPointers category in NVMTLLibraryLoad.m)
- (id<MTLFunctionHandle>)functionHandleWithFunction:(id<MTLFunction>)f {
    NSArray *c = _function ? objc_getAssociatedObject(_function, &kNVMTLTableFns) : nil;
    for (NSUInteger k = 0; k < c.count; k++) {
        NVMTLFunction *cf = c[k];
        if (cf == (id)f || [cf.name isEqualToString:f.name]) {
            NVMTLFunctionHandle *h = [NVMTLFunctionHandle new]; h->_id = k + 1; h->_fn = f; return h;
        }
    }
    // THE PIPELINE'S OWN FUNCTION IS PART OF THE PIPELINE, so a handle for it is a legitimate
    //    request and nil is what stopped ray tracing. MEASURED on the game: it asks only for the ray
    //    generation shader's own handle (rgs_diffuse_main / rgs_importance_main), and on nil it goes no
    //    further - setVisibleFunctionTable, setIntersectionFunctionTable and setAccelerationStructure are
    //    called ZERO times in a whole run, so without a handle it cannot fill the table it just built and
    //    abandons RT entirely.
    // The id handed back is the table's RESERVED UNBOUND INDEX 0, not a dispatch slot: the linkage
    //    sizes the table as entries+1 with candidates at 1..N, so 0 is "no entry here" and the shader's
    //    own null test is what reads it. That is the truthful answer - the entry is not one of the table's
    //    candidates and the dispatcher has no case for it - and it lets the game proceed instead of
    //    giving up. If a frame shows the ray generation shader needed to be CALLED through this slot, the
    //    fix is to add the entry to the dispatcher, not to invent a slot number here.
    if (_function && (f == (id)_function
                      || [f.name isEqualToString:((NVMTLFunction *)_function)->_fname])) {
        NVMTLFunctionHandle *h = [NVMTLFunctionHandle new];
        h->_id = 0;
        h->_fn = f;
        nvlog("functionHandleWithFunction: \"%s\" IS this pipeline's own function - handle with the reserved unbound index 0 (%lu table candidate(s), none of them the entry)",
              f.name.UTF8String, (unsigned long)c.count);
        return h;
    }
    // "(0 linked)" alone cannot distinguish "the candidate list was never attached" from "this pipeline
    // was built from a different function object than the one it was attached to", so name both.
    nvlog("functionHandleWithFunction: \"%s\" is not a linked function of this pipeline (%lu linked, pipeline function \"%s\") -> nil",
          f.name.UTF8String, (unsigned long)c.count,
          _function ? ((NVMTLFunction *)_function)->_fname.UTF8String : "(none)");
    return nil;
}
- (id<MTLVisibleFunctionTable>)newVisibleFunctionTableWithDescriptor:(MTLVisibleFunctionTableDescriptor *)d {
    NSUInteger n = d.functionCount ? d.functionCount : 1;
    id<MTLBuffer> b = [(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:n * sizeof(uint64_t) options:MTLResourceStorageModeShared];
    if (!b) { nvlog("newVisibleFunctionTableWithDescriptor: %lu slots - buffer allocation FAILED -> nil", (unsigned long)n); return nil; }
    memset(b.contents, 0, n * sizeof(uint64_t));
    NVMTLVisibleFunctionTable *t = [NVMTLVisibleFunctionTable new]; t->_buf = b; t->_n = n;
    nvlog("newVisibleFunctionTableWithDescriptor: %lu slot(s) -> %p", (unsigned long)n, t);
    return t;
}
@end

@implementation NVMTLIndirectCommand
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)ps { _ps = ps; }
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i {
    if (!_vbufs) _vbufs = [NSMutableArray new];
    if (b) [_vbufs addObject:@[b, @(o), @(i)]];
}
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (!_vbufs) _vbufs = [NSMutableArray new];
    if (b) [_vbufs addObject:@[b, @(o), @(i), @(st)]];
}
- (void)setFragmentBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i {
    if (!_fbufs) _fbufs = [NSMutableArray new];
    if (b) [_fbufs addObject:@[b, @(o), @(i)]];
}
- (void)drawPrimitives:(MTLPrimitiveType)type vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c
         instanceCount:(NSUInteger)n baseInstance:(NSUInteger)bi {
    _type = type; _start = s; _count = c; _instances = n; _baseInstance = bi; _indexed = NO; _armed = YES;
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)type indexCount:(NSUInteger)c indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)io
                instanceCount:(NSUInteger)n baseVertex:(NSInteger)bv baseInstance:(NSUInteger)bi {
    _type = type; _count = c; _itype = it; _ibuf = ib; _ioff = io;
    _instances = n; _baseVertex = bv; _baseInstance = bi; _indexed = YES; _armed = YES;
}
- (void)setComputePipelineState:(id<MTLComputePipelineState>)ps { _cps = ps; _isCompute = YES; }
- (void)setKernelBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i {
    if (!_kbufs) _kbufs = [NSMutableArray new];
    if (b) [_kbufs addObject:@[b, @(o), @(i)]];
}
- (void)concurrentDispatchThreadgroups:(MTLSize)grid threadsPerThreadgroup:(MTLSize)tg {
    _grid = grid; _tg = tg; _isCompute = YES; _armed = YES;
}
- (void)concurrentDispatchThreads:(MTLSize)threads threadsPerThreadgroup:(MTLSize)tg {
    MTLSize g = { (threads.width + tg.width - 1) / tg.width,
                  (threads.height + tg.height - 1) / tg.height,
                  (threads.depth + tg.depth - 1) / tg.depth };
    [self concurrentDispatchThreadgroups:g threadsPerThreadgroup:tg];
}
- (void)setBarrier {}
- (void)clearBarrier {}
- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i {
    if (len) nvlog("indirect command setThreadgroupMemoryLength:%lu — dynamic threadgroup memory is not wired; IGNORED", (unsigned long)len);
}
- (void)setStageInRegion:(MTLRegion)r {}
- (void)setCullMode:(MTLCullMode)m { _cull = m; _hasCull = YES; }
- (void)setFrontFacingWinding:(MTLWinding)w { _winding = w; _hasWinding = YES; }
- (void)setTriangleFillMode:(MTLTriangleFillMode)f { _fill = f; _hasFill = YES; }
- (void)setDepthClipMode:(MTLDepthClipMode)c { _clip = c; _hasClip = YES; }
- (void)setDepthStencilState:(id<MTLDepthStencilState>)d { _ds = d; }
- (void)setDepthBias:(float)b slopeScale:(float)s clamp:(float)c { _bias = b; _slope = s; _clamp = c; _hasBias = YES; }
- (void)nvmtlCopyFrom:(NVMTLIndirectCommand *)o {
    if (!o || o == self) return;
    _ps = o->_ps; _vbufs = [o->_vbufs mutableCopy]; _fbufs = [o->_fbufs mutableCopy];
    _type = o->_type; _start = o->_start; _count = o->_count; _instances = o->_instances; _baseInstance = o->_baseInstance;
    _ibuf = o->_ibuf; _ioff = o->_ioff; _itype = o->_itype; _baseVertex = o->_baseVertex; _indexed = o->_indexed; _armed = o->_armed;
    _cull = o->_cull; _hasCull = o->_hasCull; _winding = o->_winding; _hasWinding = o->_hasWinding; _fill = o->_fill; _hasFill = o->_hasFill;
    _clip = o->_clip; _hasClip = o->_hasClip; _ds = o->_ds; _bias = o->_bias; _slope = o->_slope; _clamp = o->_clamp; _hasBias = o->_hasBias;
    _cps = o->_cps; _kbufs = [o->_kbufs mutableCopy]; _grid = o->_grid; _tg = o->_tg; _isCompute = o->_isCompute;
}
- (void)reset {
    _ps = nil; [_vbufs removeAllObjects]; [_fbufs removeAllObjects]; _ibuf = nil; _armed = NO;
    _hasCull = _hasWinding = _hasFill = _hasClip = _hasBias = NO; _ds = nil;
    _cps = nil; [_kbufs removeAllObjects]; _isCompute = NO;
}
@end

@implementation NVMTLIndirectCommandBuffer
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSUInteger)size { return _cmds.count; }
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = _recBuf ? ((id<MTLBuffer>)_recBuf).gpuAddress : nvmtl_resid_pipe(self, &_resid); return r; }
- (MTLStorageMode)storageMode { return (MTLStorageMode)((_opts >> MTLResourceStorageModeShift) & 0xF); }
- (MTLCPUCacheMode)cpuCacheMode { return (MTLCPUCacheMode)(_opts & 0xF); }
- (MTLHazardTrackingMode)hazardTrackingMode { return nvmtl_res_hazard(nil, _opts); }
- (MTLResourceOptions)resourceOptions { return ((MTLResourceOptions)[self storageMode] << MTLResourceStorageModeShift) | (MTLResourceOptions)[self cpuCacheMode]
                                            | ((MTLResourceOptions)[self hazardTrackingMode] << MTLResourceHazardTrackingModeShift); }
- (id<MTLHeap>)heap { return nil; }
- (NSUInteger)heapOffset { return 0; }
- (BOOL)isAliasable { return NO; }
- (void)makeAliasable {}
- (NSUInteger)allocatedSize { return 0; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return nvmtl_owner_check(t); }
- (id<MTLIndirectComputeCommand>)indirectComputeCommandAtIndex:(NSUInteger)i {
    if (i >= _cmds.count) { nvlog("indirectComputeCommandAtIndex:%lu — this buffer holds %lu", (unsigned long)i, (unsigned long)_cmds.count); return nil; }
    return _cmds[i];
}
- (id<MTLIndirectRenderCommand>)indirectRenderCommandAtIndex:(NSUInteger)i {
    if (i >= _cmds.count) { nvlog("indirectRenderCommandAtIndex:%lu — this buffer holds %lu", (unsigned long)i, (unsigned long)_cmds.count); return nil; }
    return _cmds[i];
}
- (void)resetWithRange:(NSRange)r {
    for (NSUInteger i = r.location; i < r.location + r.length && i < _cmds.count; i++) [_cmds[i] reset];
}
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return nvmtl_purge(&_purge, s); }
@end

enum { NVMTL_E78_H3 = 1, NVMTL_E78_H4 = 2, NVMTL_E78_H5 = 4, NVMTL_E78_P1 = 8, NVMTL_E78_H7 = 16 };
enum { E78C_ENC, E78C_DISP, E78C_FULL, E78C_NARROW, E78C_NONE, E78C_ENDFULL, E78C_SETNEW, E78C_SETREUSE, E78C_WRITES,
       E78C_RING, E78C_RINGBYTES, E78C_RINGCHUNK, E78C_BYTESOLD, E78C_VARHIT, E78C_VARMISS, E78C_CONC, E78C_N };
static uint64_t g_e78c[E78C_N];
static int g_e78_off = -1, g_e78_census = -1;
static int nvmtl_enc78_on(int row) {
    int off = __atomic_load_n(&g_e78_off, __ATOMIC_RELAXED);
    if (off < 0) {
        const char *e = getenv("NVMTL_ENC78_OFF"); off = 0;
        if (e) {
            if (strstr(e, "all")) off = 31;
            if (strstr(e, "h3")) off |= NVMTL_E78_H3;
            if (strstr(e, "h4")) off |= NVMTL_E78_H4;
            if (strstr(e, "h5")) off |= NVMTL_E78_H5;
            if (strstr(e, "p1")) off |= NVMTL_E78_P1;
            if (strstr(e, "h7")) off |= NVMTL_E78_H7;
            nvlog("denc: NVMTL_ENC78_OFF=%s -> rows off 0x%x", e, off);
        }
        __atomic_store_n(&g_e78_off, off, __ATOMIC_RELAXED);
    }
    return !(off & row);
}
static void nvmtl_enc78_count(int k, uint64_t n) { if (k >= 0 && k < E78C_N) __atomic_fetch_add(&g_e78c[k], n, __ATOMIC_RELAXED); }
static void nvmtl_enc78_census(void) {
    if (g_e78_census < 0) { const char *e = getenv("NVMTL_ENC78_CENSUS"); g_e78_census = e ? atoi(e) : 0; }
    uint64_t n = __atomic_add_fetch(&g_e78c[E78C_ENC], 1, __ATOMIC_RELAXED);
    if (!g_e78_census || (g_e78_census < 2 && (n & (n - 1)))) return;
    uint64_t v[E78C_N]; for (int k = 0; k < E78C_N; k++) v[k] = __atomic_load_n(&g_e78c[k], __ATOMIC_RELAXED);
    fprintf(stderr, "ENC78 enc %llu disp %llu full %llu narrow %llu none %llu endfull %llu setnew %llu setreuse %llu writes %llu "
            "ring %llu ringbytes %llu ringchunk %llu bytesold %llu varhit %llu varmiss %llu conc %llu rowsoff 0x%x\n",
            v[E78C_ENC], v[E78C_DISP], v[E78C_FULL], v[E78C_NARROW], v[E78C_NONE], v[E78C_ENDFULL], v[E78C_SETNEW],
            v[E78C_SETREUSE], v[E78C_WRITES], v[E78C_RING], v[E78C_RINGBYTES], v[E78C_RINGCHUNK], v[E78C_BYTESOLD],
            v[E78C_VARHIT], v[E78C_VARMISS], v[E78C_CONC], g_e78_off < 0 ? 0 : g_e78_off);
}
void nvmtl_vk_cmd_barrier_c2c(nvk_cmdbuf *c);
@interface NVMTLCommandBuffer () { @public NVMTLBuffer *_e78Ring; size_t _e78RingOff; } @end
@interface NVMTLCommandBuffer () { @public BOOL _g79Owed, _g79C2C; NSMutableArray *_g79Sig, *_g79Promised; } @end
enum { NVMTL_G79_DUPIDX = 32, NVMTL_G79_DUPMAX = 64 };
@interface NVMTLCommandBuffer () { @public __unsafe_unretained NVMTLBuffer *_g79BRing[NVMTL_G79_DUPIDX]; size_t _g79BOff[NVMTL_G79_DUPIDX];
    uint32_t _g79BLen[NVMTL_G79_DUPIDX]; uint8_t _g79B[NVMTL_G79_DUPIDX][NVMTL_G79_DUPMAX]; } @end
enum { NVMTL_G79_BAR = 1, NVMTL_G79_ENC = 2, NVMTL_G79_EV = 4, NVMTL_G79_DUP = 8, NVMTL_G79_WFOLD = 16, NVMTL_G79_SUB = 32 };
enum { G79C_ENCNARROW, G79C_ENCFULL, G79C_BARLAZY, G79C_BARFOLD, G79C_OWEDPAID, G79C_SIGRIDE, G79C_WAITSELF, G79C_WAITHELD, G79C_WAITSPLIT, G79C_BYTESDUP, G79C_WAITFOLD, G79C_ENDS, G79C_N };
static uint64_t g_g79c[G79C_N];
static int g_g79_off = -1;
static int nvmtl_g79_on(int row) {
    int off = __atomic_load_n(&g_g79_off, __ATOMIC_RELAXED);
    if (off < 0) {
        const char *e = getenv("NVMTL_G79_OFF"); off = 0;
        if (e) {
            if (strstr(e, "all")) off = 63;
            if (strstr(e, "bar")) off |= NVMTL_G79_BAR;
            if (strstr(e, "enc")) off |= NVMTL_G79_ENC;
            if (strstr(e, "ev")) off |= NVMTL_G79_EV;
            if (strstr(e, "dup")) off |= NVMTL_G79_DUP;
            if (strstr(e, "wfold")) off |= NVMTL_G79_WFOLD;
            if (strstr(e, "sub")) off |= NVMTL_G79_SUB;
            nvlog("gsub: NVMTL_G79_OFF=%s -> rows off 0x%x", e, off);
        }
        __atomic_store_n(&g_g79_off, off, __ATOMIC_RELAXED);
    }
    return !(off & row);
}
static void nvmtl_g79_count(int k) { __atomic_fetch_add(&g_g79c[k], 1, __ATOMIC_RELAXED); }
static void nvmtl_g79_census(void) {
    static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_G79_CENSUS"); on = e ? atoi(e) : 0; }
    uint64_t n = __atomic_add_fetch(&g_g79c[G79C_ENDS], 1, __ATOMIC_RELAXED);
    if (!on || (on < 2 && (n & (n - 1)))) return;
    fprintf(stderr, "G79 ends %llu encnarrow %llu encfull %llu barlazy %llu barfold %llu owedpaid %llu sigride %llu waitself %llu "
            "waitheld %llu waitsplit %llu bytesdup %llu waitfold %llu rowsoff 0x%x\n",
            n, g_g79c[G79C_ENCNARROW], g_g79c[G79C_ENCFULL], g_g79c[G79C_BARLAZY], g_g79c[G79C_BARFOLD], g_g79c[G79C_OWEDPAID],
            g_g79c[G79C_SIGRIDE], g_g79c[G79C_WAITSELF], g_g79c[G79C_WAITHELD], g_g79c[G79C_WAITSPLIT],
            g_g79c[G79C_BYTESDUP], g_g79c[G79C_WAITFOLD], g_g79_off < 0 ? 0 : g_g79_off);
}
static void nvmtl_g79_flush(NVMTLCommandBuffer *cb) {
    if (!cb) return;
    if (cb->_g79Owed) { nvmtl_hz_shadow(1); nvmtl_vk_barrier_site(NVMTL_BARS_OWED_FLUSH); nvmtl_vk_cmd_barrier(&cb->_c); nvmtl_hz_shadow(0); cb->_g79Owed = NO; nvmtl_g79_count(G79C_OWEDPAID); }
    cb->_g79C2C = NO;
}
@interface NVMTLComputeCommandEncoder () {
@public
    // Textures declared with useResource:usage:, keyed by their Vulkan image (a view and its root share one), latest
    // usage wins. An indirect dispatch reaches them only through argument buffers, so nothing else moves them into the
    // layout a shader access needs; see nvmtlPrepareIndirectImages.
    NSMutableDictionary *_indirectImages;
    NSData *_e78Key;
    NVMTLComputePipelineState *_e78VarFor, *_e78VarSel;
    NSUInteger _e78NDisp; BOOL _e78Concurrent, _e78Loose;
    BOOL _g79Bar;
}
@end
enum { NVMTL_E78_RING = 64 * 1024, NVMTL_E78_RING_ALIGN = 256, NVMTL_E78_RING_MAX = 16 * 1024 };
static NVMTLBuffer *nvmtl_bytes_ring(NVMTLCommandBuffer *cb, NSUInteger len, size_t *off) {
    size_t need = len < 16 ? 16 : len;
    if (!cb || need > NVMTL_E78_RING_MAX || !nvmtl_enc78_on(NVMTL_E78_H4)) return nil;
    size_t at = (cb->_e78RingOff + NVMTL_E78_RING_ALIGN - 1) & ~(size_t)(NVMTL_E78_RING_ALIGN - 1);
    if (!cb->_e78Ring || at + need > cb->_e78Ring->_b.size) {
        NVMTLBuffer *r = [NVMTLBuffer new];
        if (nvmtl_vk_buffer_create(NVMTL_E78_RING, 1, &r->_b) || !r->_b.map) {
            static int said; if (!said++) nvlog("denc: setBytes ring chunk create failed - per-call buffers instead");
            return nil;
        }
        if (!cb->_scratch) cb->_scratch = [NSMutableArray new];
        [cb->_scratch addObject:r];
        cb->_e78Ring = r; at = 0;
        nvmtl_enc78_count(E78C_RINGCHUNK, 1);
    }
    cb->_e78RingOff = at + need; *off = at;
    return cb->_e78Ring;
}

#include "nvconv_lib.h"
@interface NVMTLComputeCommandEncoder () { @public uint8_t _nvcKP[528]; uint32_t _nvcKPLen; uint8_t _nvcCP[272]; uint32_t _nvcCPLen; BOOL _nvcIn; } @end
static id<MTLComputePipelineState> g_nvc_ps[17];   /* 8..10 tensor-core 3/4/5_s, 11..13 32x32 3/4/5_t, 14..15 split-K 4/5_k, 16 reduce (optional) */
static NSMutableDictionary *g_nvc_part;            /* split-K partial-sum buffer per layer key */
#define NVC_NC 4                                   /* candidates: 0 fp32 tiles, 1 tensor cores, 2 32x32 tiles, 3 split-K */
static int nvmtl_nvconv_mma_on(void) { static int on = -1; if (on < 0) { const char *e = getenv("NVMTL_NVCONV_MMA"); on = !(e && e[0] == '0'); } return on; }
#define NVC_CMP 6
#define NVC_COPY 7
#define NVC_NVAR 7   /* epilogues 0-4, plus 5 and 6: residual blocks that add their own input */
static NSMutableDictionary *g_nvc_state;
static pthread_mutex_t g_nvc_lk = PTHREAD_MUTEX_INITIALIZER;
static _Atomic unsigned long g_nvc_ran, g_nvc_verified, g_nvc_refused, g_nvc_skips;
// nvlog keeps only 8 lines per format, so after Face Tracking's 8 layers every later verification (Super Resolution)
// vanished from the log. Dev builds also append every nvconv outcome to $TMPDIR/nvmtl-nvconv-<pid>.log, uncapped.
static void nvmtl_nvc_report(const char *fmt, ...) {
#if !NVMTL_RELEASE
    static char path[1024]; static dispatch_once_t once;
    dispatch_once(&once, ^{ const char *t = getenv("TMPDIR");
        snprintf(path, sizeof path, "%s/nvmtl-nvconv-%d.log", (t && *t) ? t : "/private/tmp", getpid()); });
    char b[400]; va_list a; va_start(a, fmt); vsnprintf(b, sizeof b, fmt, a); va_end(a);
    FILE *f = fopen(path, "a"); if (f) { fprintf(f, "%s\n", b); fclose(f); }
#else
    (void)fmt;
#endif
}
// Each distinct (reason, dispatch grid) is logged once, up to 96: a flat first-16 cap was spent entirely on Face
// Tracking, so the Super Resolution layers' reasons (the 1.5 s 9216x32 dispatch) never reached the log.
static void nvmtl_nvc_skip_log(const char *why, MTLSize grid) {
    static NSMutableSet *seen; static pthread_mutex_t lk = PTHREAD_MUTEX_INITIALIZER;
    const unsigned long k = ++g_nvc_skips;
    NSString *key = [NSString stringWithFormat:@"%s|%lux%lux%lu", why, (unsigned long)grid.width, (unsigned long)grid.height, (unsigned long)grid.depth];
    pthread_mutex_lock(&lk);
    if (!seen) seen = [NSMutableSet new];
    const BOOL fresh = seen.count < 96 && ![seen containsObject:key];
    if (fresh) [seen addObject:key];
    pthread_mutex_unlock(&lk);
    if (fresh) nvlog("nvconv: not taken - %s (grid %lux%lux%lu, %lu skips so far)", why,
                     (unsigned long)grid.width, (unsigned long)grid.height, (unsigned long)grid.depth, k);
    if (fresh) nvmtl_nvc_report("skip %s | grid %lux%lux%lu", why, (unsigned long)grid.width, (unsigned long)grid.height, (unsigned long)grid.depth);
}
#define NVC_SKIP(why) do { nvmtl_nvc_skip_log(why, grid); return 0; } while (0)
static BOOL nvmtl_nvconv_ready(id<MTLDevice> dev) {
    static int on = -1;
    if (on >= 0) return on;
    pthread_mutex_lock(&g_nvc_lk);
    if (on < 0) {
        const char *e = getenv("NVMTL_NVCONV"); on = !(e && e[0] == '0');
        if (on) {
            NSError *err = nil;
            dispatch_data_t dd = dispatch_data_create(nvconv_metallib, nvconv_metallib_len, NULL, DISPATCH_DATA_DESTRUCTOR_DEFAULT);
            id<MTLLibrary> lib = [dev newLibraryWithData:dd error:&err];
            NSString *names[8] = { @"nvconv3_b", @"nvconv3_s", @"nvconv4_b", @"nvconv4_s", @"nvconv5_b", @"nvconv5_s", @"nvconv_cmp", @"nvconv_copy" };
            for (int i = 0; i < 8 && on; i++) {
                id<MTLFunction> f = [lib newFunctionWithName:names[i]];
                g_nvc_ps[i] = f ? [dev newComputePipelineStateWithFunction:f error:&err] : nil;
                if (!g_nvc_ps[i]) { on = 0; nvlog("nvconv: %s did not build (%s) - MPS's conv stays everywhere", names[i].UTF8String, err.description.UTF8String); }
            }
            // the tensor-core forms are extra candidates: one that does not build turns only itself off
            NSString *mnames[9] = { @"nvconvm3_s", @"nvconvm4_s", @"nvconvm5_s", @"nvconv3_t", @"nvconv4_t", @"nvconv5_t",
                                    @"nvconv4_k", @"nvconv5_k", @"nvconv_reduce" };
            for (int i = 0; i < 9 && on; i++) {
                if (i < 3 && !nvmtl_nvconv_mma_on()) continue;
                id<MTLFunction> f = [lib newFunctionWithName:mnames[i]];
                g_nvc_ps[8 + i] = f ? [dev newComputePipelineStateWithFunction:f error:&err] : nil;
                if (!g_nvc_ps[8 + i]) nvlog("nvconv: %s did not build (%s) - that candidate is off", mnames[i].UTF8String, err.description.UTF8String);
            }
            g_nvc_part = [NSMutableDictionary new];
            g_nvc_state = [NSMutableDictionary new];
        }
        nvlog("nvconv: verified MPS-conv fast path %s (NVMTL_NVCONV=%s)", on ? "ON" : "OFF", e ? e : "unset");
    }
    pthread_mutex_unlock(&g_nvc_lk);
    return on;
}
static void nvmtl_nvconv_run(NVMTLComputeCommandEncoder *e, id<MTLComputePipelineState> ps, MTLSize grid, uint32_t variant) {
    NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
    e->_ps = (NVMTLComputePipelineState *)ps; e->_tgLen = nil; e->_nvcIn = YES;
    [e setBytes:&variant length:4 atIndex:30];
    [e dispatchThreadgroups:grid threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
    e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
}
// One candidate: a tiled kernel, or split-K = the slice kernel writing raw sums to part (buffer 7) and nvconv_reduce adding
// the slices and applying the epilogue into the layer's output (form 5: buffer 4, already bound; form 4: buffer 5).
static void nvmtl_nvconv_run_c(NVMTLComputeCommandEncoder *e, id<MTLComputePipelineState> ps, MTLSize grid, uint32_t variant,
                               BOOL splitk, id<MTLBuffer> part, uint32_t nsplit, uint32_t nar, uint64_t nout) {
    if (!splitk) { nvmtl_nvconv_run(e, ps, grid, variant); return; }
    [e setBuffer:part offset:0 atIndex:7]; [e setBytes:&nsplit length:4 atIndex:28];
    nvmtl_nvconv_run(e, ps, grid, variant);
    NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
    if (nar == 4) { NSArray *o = nvmtl_bufat(e, 3); if (o) [e setBuffer:o[0] offset:[o[1] unsignedLongValue] atIndex:5]; }
    const uint32_t form = nar;
    e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[16]; e->_tgLen = nil; e->_nvcIn = YES;
    [e setBytes:&form length:4 atIndex:27];
    [e dispatchThreads:MTLSizeMake((NSUInteger)nout, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
    e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
}
// NVMTL_NVCONV_DUMP=<dir>, for a layer nvconv skips with array offsets: copy every bound array whole (from its binding
// offset to the buffer's end, capped at 64 MB), run MPS's kernel, copy its output, and write all of it with the parameter
// words, so the offset rule can be solved offline from real data. First 12 such layers per process; MPS's result stands.
static int nvmtl_nvconv_dump_offsets(NVMTLComputeCommandEncoder *e, MTLSize grid, MTLSize tg, uint32_t nar, id<MTLDevice> dev,
                                     const char *tag, _Atomic unsigned *ctr) {
    static const char *dir; static dispatch_once_t once;
    dispatch_once(&once, ^{ dir = getenv("NVMTL_NVCONV_DUMP"); if (dir && !*dir) dir = NULL; });
    // each caller has its own counter (tag): Face Tracking's offset layers came first and used every slot of a shared one
    if (!dir || !g_nvc_ps[NVC_COPY] || atomic_fetch_add(ctr, 1) >= 12) return 0;
    _Atomic unsigned *taken = ctr;
    NSMutableArray *dumps = [NSMutableArray new];
    void (^copyAll)(NSString *, uint32_t) = ^(NSString *nm, uint32_t slot) {
        NSArray *bnd = nvmtl_bufat(e, slot); if (!bnd) return;
        id<MTLBuffer> from = bnd[0]; const NSUInteger off = [bnd[1] unsignedLongValue];
        if (off >= from.length) return;
        uint64_t count = MIN((uint64_t)(from.length - off) / 4, (uint64_t)(16u << 20));
        id<MTLBuffer> to = count ? [dev newBufferWithLength:(NSUInteger)count * 4 options:MTLResourceStorageModeShared] : nil;
        if (!to) return;
        const uint32_t cn = (uint32_t)count;
        NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
        e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[NVC_COPY]; e->_tgLen = nil; e->_nvcIn = YES;
        [e setBuffer:from offset:off atIndex:5]; [e setBuffer:to offset:0 atIndex:6]; [e setBytes:&cn length:4 atIndex:8];
        [e dispatchThreads:MTLSizeMake(cn, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
        [e setBuffer:from offset:off atIndex:slot];
        [dumps addObject:@[nm, to]];
    };
    for (uint32_t a = 0; a + 1 < nar; a++) copyAll([NSString stringWithFormat:@"arg%u", a], a);
    e->_nvcIn = YES; [e dispatchThreadgroups:grid threadsPerThreadgroup:tg]; e->_nvcIn = NO;   // MPS's own kernel
    copyAll(@"mps", nar - 1);
    NSData *kpc = [NSData dataWithBytes:e->_nvcKP length:e->_nvcKPLen], *cpc = [NSData dataWithBytes:e->_nvcCP length:e->_nvcCPLen];
    NSString *base = [@(dir) stringByAppendingFormat:@"/%d-%s%u", getpid(), tag, (unsigned)atomic_load(taken)];
    NSString *info = [NSString stringWithFormat:@"array-offset layer, %u arrays, grid %lux%lux%lu tg %lux%lux%lu\n", nar,
                      (unsigned long)grid.width, (unsigned long)grid.height, (unsigned long)grid.depth, (unsigned long)tg.width, (unsigned long)tg.height, (unsigned long)tg.depth];
    [e->_cb addCompletedHandler:^(id<MTLCommandBuffer> cb) {
        [[NSFileManager defaultManager] createDirectoryAtPath:base withIntermediateDirectories:YES attributes:nil error:nil];
        for (NSArray *d in dumps) { id<MTLBuffer> b = d[1];
            [[NSData dataWithBytes:b.contents length:b.length] writeToFile:[base stringByAppendingFormat:@"/%@.f32", d[0]] atomically:YES]; }
        [kpc writeToFile:[base stringByAppendingString:@"/kp.u32"] atomically:YES];
        [cpc writeToFile:[base stringByAppendingString:@"/cp.u32"] atomically:YES];
        [info writeToFile:[base stringByAppendingString:@"/shape.txt"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
    }];
    return 1;
}
// NVMTL_NVCONV_SHUFFLE=0 keeps fused depth-to-space layers on MPS (the off switch for that one mapping)
static BOOL nvmtl_nvconv_shuffle_enabled(void) {
    static int on = -1;
    if (on < 0) { const char *v = getenv("NVMTL_NVCONV_SHUFFLE"); on = !(v && v[0] == '0'); }
    return on;
}
// NVMTL_NVCONV_TIMED=0 takes every verified layer regardless of speed (the old behaviour)
static BOOL nvmtl_nvconv_timed(void) {
    static int on = -1;
    if (on < 0) { const char *v = getenv("NVMTL_NVCONV_TIMED"); on = !(v && v[0] == '0'); }
    return on;
}
id<MTLCounterSet> nvmtl_timestamp_counter_set(void);
static int nvmtl_nvconv_try(NVMTLComputeCommandEncoder *e, MTLSize grid, MTLSize tg) {
    if (e->_nvcIn || !e->_ps || !e->_ps->_function || ![e->_ps->_function->_fname isEqualToString:@"ndArrayConvolution2D"]) return 0;
    const uint32_t nar = e->_nvcKPLen == 368 ? 3 : e->_nvcKPLen == 448 ? 4 : e->_nvcKPLen == 528 ? 5 : 0;
    if (!nar || e->_nvcCPLen != 272) {
        char why[200]; const uint32_t *w = (const uint32_t *)e->_nvcKP;
        snprintf(why, sizeof why, "parameter blocks %u / %u bytes (first array sizes %u,%u,%u,%u, second %u,%u,%u,%u)", e->_nvcKPLen, e->_nvcCPLen,
                 w[4], w[5], w[6], w[7], w[24], w[25], w[26], w[27]);
        NVC_SKIP(why);
    }
    id<MTLDevice> dev = [e->_cb device];
    if (!nvmtl_nvconv_ready(dev)) return 0;
    const uint32_t *kp = (const uint32_t *)e->_nvcKP, *cp = (const uint32_t *)e->_nvcCP;
    const uint32_t cin = cp[32], cout = cp[33], kw = cp[34], kh = cp[35], groups = cp[36], batch = cp[37], sw = cp[38], sh = cp[39], dw = cp[40], dh = cp[41];
    if (groups != 1 || !cin || !cout || !kw || !kh || !batch || !sw || !sh || !dw || !dh) NVC_SKIP("groups/dims");
    // An array's four offset words are its view's origin (a concat branch: Face Tracking writes 256 channels at channel 256
    // of a 512-channel tensor). The app's buffer binding already points at that origin - solved from a dumped layer
    // (nvcdump2 off1): MPS's output equals ReLU(conv) at binding + pixel*512 + channel, max rel err 0 - so the words need
    // no handling beyond the strides the kernel already reads. Verification below still has the last word on every layer.
    for (uint32_t a = 0; a < nar; a++) if (kp[a * 20] | kp[a * 20 + 1] | kp[a * 20 + 2] | kp[a * 20 + 3]) {
        static _Atomic unsigned offTaken;
        if (nvmtl_nvconv_dump_offsets(e, grid, tg, nar, dev, "off", &offTaken)) return 1;
        break;
    }
    const uint32_t ob = (nar - 1) * 20;
    if (kp[4] != cin || kp[5] != sw || kp[6] != sh || kp[7] != batch || kp[8] != 1) NVC_SKIP("input descriptor");
    if (kp[24] != cout || kp[25] != cin || kp[26] != kw || kp[27] != kh || kp[28] != 1) NVC_SKIP("weights descriptor");
    // The output tensor is cout x dw x dh, or - a fused depth-to-space with factor r (Super Resolution's 1.5 s
    // 48x48x64 -> 1024 layer is written as 192x192x64) - cout/r^2 x dw*r x dh*r. nvconv.metal places channel
    // co = (dy*r + dx)*(cout/r^2) + c at (oy*r + dy, ox*r + dx, c): solved from the layer's dump, 8.9e-7 against MPS.
    const uint32_t oc = kp[ob + 4], ow = kp[ob + 5], oh = kp[ob + 6];
    const uint32_t shuf = (ow > dw && dw && ow % dw == 0) ? ow / dw : 1;
    if (oh != dh * shuf || ow != dw * shuf || (uint64_t)oc * shuf * shuf != cout || kp[ob + 7] != batch || shuf > 255 ||
        (shuf > 1 && !nvmtl_nvconv_shuffle_enabled())) {
        char why[200]; snprintf(why, sizeof why, "output sizes %u,%u,%u,%u vs cout %u, %ux%u, batch %u (in %ux%ux%u k%ux%u, nar %u)",
                                kp[ob + 4], kp[ob + 5], kp[ob + 6], kp[ob + 7], cout, dw, dh, batch, sw, sh, cin, kw, kh, nar);
        static _Atomic unsigned shufTaken;
        if (nvmtl_nvconv_dump_offsets(e, grid, tg, nar, dev, "shuf", &shufTaken)) return 1;
        NVC_SKIP(why); }
    // Strided output (a concat branch) is fine as long as no two outputs alias: channels innermost and dense, each wider
    // dimension at least as far apart as the one inside it spans. The dense layout is the case where all are equal.
    if (kp[ob + 8] != 1 || (ow > 1 && kp[ob + 9] < oc) || (oh > 1 && kp[ob + 10] < (uint64_t)(ow > 1 ? kp[ob + 9] : oc) * ow) ||
        (batch > 1 && kp[ob + 11] < (uint64_t)(oh > 1 ? kp[ob + 10] : (ow > 1 ? kp[ob + 9] : oc)) * oh)) {
        char why[160]; snprintf(why, sizeof why, "output strides %u,%u,%u,%u for cout %u x %u x %u (sizes %u,%u,%u,%u)",
                                kp[ob + 8], kp[ob + 9], kp[ob + 10], kp[ob + 11], cout, dw, dh, kp[ob + 4], kp[ob + 5], kp[ob + 6], kp[ob + 7]);
        NVC_SKIP(why); }
    if (nar >= 4 && (kp[44] != cout || kp[48] != 1)) NVC_SKIP("bias descriptor");
    // the residual is only read, through its own strides (nvconv.metal), so any layout is fine; only its span must fit
    if (nar == 5 && (kp[64] != cout || kp[65] != dw || kp[66] != dh || kp[67] != batch))
        NVC_SKIP("residual descriptor");
    NSArray *bi = nvmtl_bufat(e, 0), *bw = nvmtl_bufat(e, 1), *bo = nvmtl_bufat(e, nar - 1), *bb = nar >= 4 ? nvmtl_bufat(e, 2) : nil, *br = nar == 5 ? nvmtl_bufat(e, 3) : nil;
    if (!bi || !bw || !bo || (nar >= 4 && !bb) || (nar == 5 && !br)) NVC_SKIP("buffer slot unbound");
    #define NVC_ROOM(arr, need) ([(id<MTLBuffer>)arr[0] length] >= [arr[1] unsignedLongValue] + (uint64_t)(need) * 4)
    const uint64_t nin = (uint64_t)(cin - 1) * kp[8] + (uint64_t)(sw - 1) * kp[9] + (uint64_t)(sh - 1) * kp[10] + (uint64_t)(batch - 1) * kp[11] + 1;
    const uint64_t nwt = (uint64_t)(cout - 1) * kp[28] + (uint64_t)(cin - 1) * kp[29] + (uint64_t)(kw - 1) * kp[30] + (uint64_t)(kh - 1) * kp[31] + 1;
    const uint64_t nout = (uint64_t)cout * dw * dh * batch;
    // nspan: floats from the output binding to one past the last output, gaps included (== nout when dense)
    const uint64_t nspan = (uint64_t)(batch - 1) * kp[ob + 11] + (uint64_t)(oh - 1) * kp[ob + 10] + (uint64_t)(ow - 1) * kp[ob + 9] + oc;
    const uint64_t nres = nar == 5 ? (uint64_t)(cout - 1) * kp[68] + (uint64_t)(dw - 1) * kp[69] + (uint64_t)(dh - 1) * kp[70] + (uint64_t)(batch - 1) * kp[71] + 1 : 0;
    if (!NVC_ROOM(bi, nin) || !NVC_ROOM(bw, nwt) || !NVC_ROOM(bo, nspan) || (bb && !NVC_ROOM(bb, cout)) || (br && !NVC_ROOM(br, nres)) || nspan > 0xffffffffull) NVC_SKIP("buffer too short for f32");
    #undef NVC_ROOM
    const uint64_t M = (uint64_t)batch * dh * dw;
    const BOOL small = ((M + 127) / 128) * ((cout + 63) / 64) < 60;
    id<MTLComputePipelineState> ours = g_nvc_ps[(nar - 3) * 2 + (small ? 1 : 0)];
    const MTLSize og = small ? MTLSizeMake((M + 63) / 64, (cout + 31) / 32, 1) : MTLSizeMake((M + 127) / 128, (cout + 63) / 64, 1);
    // candidate kernels and grids; nil = that candidate does not apply to this layer
    const uint64_t KK = (uint64_t)kw * kh * cin, tiles32 = ((M + 31) / 32) * ((cout + 31) / 32);
    uint32_t nsplit = tiles32 ? (uint32_t)MIN(8ull, (240 + tiles32 - 1) / tiles32) : 0;
    nsplit = (uint32_t)MIN((uint64_t)nsplit, ((KK + 15) / 16) / 2);   /* at least two 16-deep chunks per slice */
    const uint64_t partBytes = (uint64_t)nsplit * M * cout * 4;
    const BOOL canK = nar >= 4 && shuf == 1 && nsplit >= 2 && partBytes <= (256ull << 20) && g_nvc_ps[14 + (nar - 4)] && g_nvc_ps[16];
    id<MTLComputePipelineState> cps[NVC_NC] = { ours, nvmtl_nvconv_mma_on() ? g_nvc_ps[8 + (nar - 3)] : nil, g_nvc_ps[11 + (nar - 3)],
                                                canK ? g_nvc_ps[14 + (nar - 4)] : nil };
    const MTLSize cg[NVC_NC] = { og, MTLSizeMake((M + 63) / 64, (cout + 31) / 32, 1), MTLSizeMake((M + 31) / 32, (cout + 31) / 32, 1),
                                 MTLSizeMake((M + 31) / 32, (cout + 31) / 32, nsplit) };
    id<MTLComputePipelineState> oursM = cps[1];
    NSMutableData *key = [NSMutableData dataWithBytes:&e->_ps length:sizeof(void *)];
    for (uint32_t a = 0; a < nar; a++) [key appendBytes:kp + a * 20 + 4 length:8 * 4];
    [key appendBytes:cp + 32 length:18 * 4];
    pthread_mutex_lock(&g_nvc_lk); NSNumber *st = g_nvc_state[key]; if (!st) g_nvc_state[key] = @0; pthread_mutex_unlock(&g_nvc_lk);
    if (st.intValue > 0) {
        const int c = st.intValue / 100, ep = st.intValue % 100 - 1;
        id<MTLBuffer> part = nil;
        if (c == 3) { pthread_mutex_lock(&g_nvc_lk); part = g_nvc_part[key]; pthread_mutex_unlock(&g_nvc_lk); }
        if (c < NVC_NC && cps[c] && (c != 3 || part)) {
            nvmtl_nvconv_run_c(e, cps[c], cg[c], (uint32_t)ep | shuf << 8, c == 3, part, nsplit, nar, M * cout);
            g_nvc_ran++; return 1;
        }
        return 0;   /* the chosen kernel is gone (cannot happen once built); MPS runs */
    }
    if (st) { static _Atomic unsigned long pend; if (st.intValue == 0 && ++pend <= 8) nvlog("nvconv: still pending (%lu)", (unsigned long)pend); return 0; }
    // Correct is not enough: on Background Blur's layers our kernel was slower than MPS (GB7 Blur 60,767 -> 56,277 with
    // nvconv on, while Super Resolution gained 136,044 -> 156,599). Time MPS's dispatch and each variant with the
    // device's own timestamp counters, and take the layer only where ours needs <= 90% of MPS's time.
    id<MTLCounterSampleBuffer> tsb = nil;
    if (nvmtl_nvconv_timed()) {
        MTLCounterSampleBufferDescriptor *td = [MTLCounterSampleBufferDescriptor new];
        td.counterSet = nvmtl_timestamp_counter_set(); td.sampleCount = 2 + 2 * NVC_NC * NVC_NVAR; td.storageMode = MTLStorageModeShared;
        tsb = [dev newCounterSampleBufferWithDescriptor:td error:nil];
    }
    if (tsb) [e sampleCountersInBuffer:tsb atSampleIndex:0 withBarrier:YES];
    e->_nvcIn = YES; [e dispatchThreadgroups:grid threadsPerThreadgroup:tg]; e->_nvcIn = NO;
    if (tsb) [e sampleCountersInBuffer:tsb atSampleIndex:1 withBarrier:YES];
    id<MTLBuffer> outBuf = bo[0]; const NSUInteger outOff = [bo[1] unsignedLongValue];
    // counters: 8 words per candidate (epilogues 0-6); samples from byte 32 * NVC_NC
    id<MTLBuffer> res = [dev newBufferWithLength:32 * NVC_NC + 48 options:MTLResourceStorageModeShared];
    memset(res.contents, 0, 32 * NVC_NC + 48);
    id<MTLBuffer> partV = cps[3] ? [dev newBufferWithLength:(NSUInteger)partBytes options:MTLResourceStorageModePrivate] : nil;
    const uint32_t n = (uint32_t)nspan;   // compared floats: the whole span, so a strided layer is checked in place
    // NVMTL_NVCONV_DUMP=<dir>: copy this layer's input, weights, bias, MPS's output and variant 0's output into
    // CPU-readable buffers and write them with the parameter words, so a refused layer's rule can be solved offline.
    static const char *dumpDir; static dispatch_once_t dumpOnce;
    dispatch_once(&dumpOnce, ^{ dumpDir = getenv("NVMTL_NVCONV_DUMP"); if (dumpDir && !*dumpDir) dumpDir = NULL; });
    NSMutableArray *dumps = dumpDir ? [NSMutableArray new] : nil;   // [name, shared buffer] pairs
    void (^copyOut)(NSString *, id<MTLBuffer>, NSUInteger, uint64_t) = ^(NSString *nm, id<MTLBuffer> from, NSUInteger off, uint64_t count) {
        if (!dumps || !from || !count || count > 0x10000000ull) return;
        id<MTLBuffer> to = [dev newBufferWithLength:(NSUInteger)count * 4 options:MTLResourceStorageModeShared];
        if (!to) return;
        const uint32_t cn = (uint32_t)count;
        NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
        e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[NVC_COPY]; e->_tgLen = nil; e->_nvcIn = YES;
        [e setBuffer:from offset:off atIndex:5]; [e setBuffer:to offset:0 atIndex:6]; [e setBytes:&cn length:4 atIndex:8];
        [e dispatchThreads:MTLSizeMake(cn, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
        [dumps addObject:@[nm, to]];
    };
    copyOut(@"in", bi[0], [bi[1] unsignedLongValue], nin);
    copyOut(@"w", bw[0], [bw[1] unsignedLongValue], nwt);
    if (bb) copyOut(@"b", bb[0], [bb[1] unsignedLongValue], cout);
    if (br) copyOut(@"res", br[0], [br[1] unsignedLongValue], nres);
    copyOut(@"mps", outBuf, outOff, nspan);
    NSData *kpCopy = dumps ? [NSData dataWithBytes:e->_nvcKP length:e->_nvcKPLen] : nil;
    NSData *cpCopy = dumps ? [NSData dataWithBytes:e->_nvcCP length:e->_nvcCPLen] : nil;
    for (uint32_t pass = 0; pass < NVC_NC; pass++) if (cps[pass] && (pass != 3 || partV))
    for (uint32_t v = 0; v < NVC_NVAR; v++) {
        id<MTLBuffer> scratch = [dev newBufferWithLength:(NSUInteger)nspan * 4 options:MTLResourceStorageModePrivate];
        NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
        if (nspan != nout) {
            // strided output: start the scratch as MPS's result, so the other concat branch's channels (which this layer
            // must not touch) compare equal and any write into them shows up as a mismatch
            e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[NVC_COPY]; e->_tgLen = nil; e->_nvcIn = YES;
            [e setBuffer:outBuf offset:outOff atIndex:5]; [e setBuffer:scratch offset:0 atIndex:6]; [e setBytes:&n length:4 atIndex:8];
            [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
            e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
        }
        [e setBuffer:scratch offset:0 atIndex:nar - 1];
        const NSUInteger ts0 = 2 + 2 * NVC_NVAR * pass + 2 * v;
        if (tsb) [e sampleCountersInBuffer:tsb atSampleIndex:ts0 withBarrier:YES];
        nvmtl_nvconv_run_c(e, cps[pass], cg[pass], v | shuf << 8, pass == 3, partV, nsplit, nar, M * cout);   // epilogue low byte, depth-to-space above
        if (tsb) [e sampleCountersInBuffer:tsb atSampleIndex:ts0 + 1 withBarrier:YES];
        if (v == 0 && !pass) copyOut(@"ours0", scratch, 0, nspan);
        e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[NVC_CMP]; e->_tgLen = nil; e->_nvcIn = YES;
        [e setBuffer:outBuf offset:outOff atIndex:5]; [e setBuffer:scratch offset:0 atIndex:6]; [e setBuffer:res offset:32 * pass + v * 4 atIndex:7];
        [e setBytes:&n length:4 atIndex:8]; [e setBuffer:res offset:32 * NVC_NC atIndex:9];
        [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
    }
    [e setBuffer:outBuf offset:outOff atIndex:nar - 1];
    char shape[112]; snprintf(shape, sizeof shape, "%ux%ux%u -> %u k%ux%u s%u,%u form %u%s%s", sw, sh, cin, cout, kw, kh, cp[46], cp[47], nar,
                              shuf > 1 ? " depth-to-space" : "", nspan != nout ? " strided" : "");
    NSString *sh_ = @(shape);
    { static _Atomic unsigned long issued; const unsigned long k = ++issued; if (k <= 96) nvlog("nvconv: verifying %s (%lu issued)", shape, k); }
    uint32_t availM = 0; for (int c = 0; c < NVC_NC; c++) if (cps[c] && (c != 3 || partV)) availM |= 1u << c;   /* blocks cannot capture arrays */
    [e->_cb addCompletedHandler:^(id<MTLCommandBuffer> cb) {
        const uint32_t *r = (const uint32_t *)res.contents;
        int pk[NVC_NC] = { 0 };                                  // first verified epilogue + 1, per candidate
        for (int c = 0; c < NVC_NC; c++) for (int v = 0; v < NVC_NVAR && !pk[c] && ((availM >> c) & 1); v++) if (r[8 * c + v] == 0) pk[c] = v + 1;
        int pick = pk[0], anyv = 0; for (int c = 0; c < NVC_NC; c++) anyv |= pk[c] != 0;
        double tm = -1, tc[NVC_NC] = { -1, -1, -1, -1 };
        NSData *ts = tsb ? [tsb resolveCounterRange:NSMakeRange(0, 2 + 2 * NVC_NC * NVC_NVAR)] : nil;
        if (anyv && ts.length == (2 + 2 * NVC_NC * NVC_NVAR) * 8) {
            const uint64_t *t = ts.bytes;
            if (t[1] > t[0]) tm = (double)(t[1] - t[0]);
            for (int c = 0; c < NVC_NC; c++) if (pk[c]) { const size_t i = 2 + 2 * NVC_NVAR * (size_t)c + 2 * (size_t)(pk[c] - 1);
                if (t[i + 1] > t[i]) tc[c] = (double)(t[i + 1] - t[i]); }
        }
        // the fastest verified candidate, taken only when it needs <= 90% of MPS's time; untimed = the fp32 tiles, as before
        int best = -1; for (int c = 0; c < NVC_NC; c++) if (pk[c] && tc[c] > 0 && tm > 0 && tc[c] <= 0.9 * tm && (best < 0 || tc[c] < tc[best])) best = c;
        const int slower = anyv && tm > 0 && best < 0;
        const int state = best >= 0 ? best * 100 + pk[best] : (pk[0] && tm < 0) ? pk[0] : anyv ? -2 : -1;
        if (best == 3) { pthread_mutex_lock(&g_nvc_lk); g_nvc_part[key] = partV; pthread_mutex_unlock(&g_nvc_lk); }
        pthread_mutex_lock(&g_nvc_lk); g_nvc_state[key] = @(state); pthread_mutex_unlock(&g_nvc_lk);
        static const char *cname[NVC_NC] = { "fp32 tiles", "tensor cores", "32x32 tiles", "split-K" };
        if (anyv) nvmtl_nvc_report("%s %s | MPS %.0f ours %.0f mma %.0f t32 %.0f splitk %.0f x%u (timestamp units; %s)", slower ? "SLOWER" : "FASTER", sh_.UTF8String,
                                   tm, tc[0], tc[1], tc[2], tc[3], nsplit, best >= 0 ? cname[best] : state > 0 ? "untimed, fp32 tiles taken" : "MPS keeps it");
        if (!pick) for (int c = 1; c < NVC_NC && !pick; c++) if (pk[c]) pick = 100 * c + pk[c];   // for the VERIFIED line below
        if (pick) { const unsigned long k = ++g_nvc_verified; if (k <= 64 || !(k & (k - 1))) nvlog("nvconv: VERIFIED %s epilogue %d (%lu so far)", sh_.UTF8String, pick - 1, k); }
        nvmtl_nvc_report("%s %s | epilogue %d | mismatches %u %u %u %u %u of %u | mma %u %u t32 %u %u splitk %u %u", pick ? "VERIFIED" : "REFUSED", sh_.UTF8String, pick - 1,
                         r[0], r[1], r[2], r[3], r[4], n, r[8], r[9], r[16], r[17], r[24], r[25]);
        if (dumps) {
            static _Atomic unsigned dseq; const unsigned id_ = atomic_fetch_add(&dseq, 1);
            NSString *base = [@(dumpDir) stringByAppendingFormat:@"/%d-%u-%@", getpid(), id_, pick ? @"ok" : @"refused"];
            [[NSFileManager defaultManager] createDirectoryAtPath:base withIntermediateDirectories:YES attributes:nil error:nil];
            for (NSArray *d in dumps) { id<MTLBuffer> b = d[1];
                [[NSData dataWithBytes:b.contents length:b.length] writeToFile:[base stringByAppendingFormat:@"/%@.f32", d[0]] atomically:YES]; }
            [kpCopy writeToFile:[base stringByAppendingString:@"/kp.u32"] atomically:YES];
            [cpCopy writeToFile:[base stringByAppendingString:@"/cp.u32"] atomically:YES];
            [[NSString stringWithFormat:@"%@\nmismatches %u %u %u %u %u of %u\n", sh_, r[0], r[1], r[2], r[3], r[4], n]
                writeToFile:[base stringByAppendingString:@"/shape.txt"] atomically:YES encoding:NSUTF8StringEncoding error:nil];
        }
        if (pick) {} else { const unsigned long k = ++g_nvc_refused; const float *sp = (const float *)(r + 8 * NVC_NC);
            if (k <= 64 || !(k & (k - 1))) nvlog("nvconv: REFUSED %s - mismatches %u/%u/%u/%u/%u of %u, MPS keeps it (%lu so far); MPS/ours(prelu) %g/%g %g/%g %g/%g %g/%g", sh_.UTF8String,
                                                  r[0], r[1], r[2], r[3], r[4], n, k, sp[0], sp[1], sp[2], sp[3], sp[4], sp[5], sp[6], sp[7]); }
    }];
    return 1;
}

// Metal binds a compute buffer at any 4-byte offset; Vulkan storage buffers need 16. A slot at an unaligned offset is bound to
// a dummy buffer, so a shader that reads its arguments through that slot reads zeros (Photoshop's duplicate-layer kernel, slot 1
// at offset 4100, then dereferenced a null pointer). With NVMTL_MISALIGNED_BUFFER_COPY=1 a CPU-visible buffer at such an
// offset is bound as a copy of the bytes from the offset on (up to 4 MB), the way setBytes binds, which is correct for data the
// kernel only reads. Bytes the kernel writes are carried back after completion. A buffer another GPU command writes earlier in
// the same command buffer is read as it was at encode time. Off unless set.
static int nvmtl_misaligned_copy_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_MISALIGNED_BUFFER_COPY"); on = e && *e && *e != '0'; }
    return on;
}
@implementation NVMTLComputeCommandEncoder
- (unsigned long long)globalTraceObjectID { return nvmtl_global_trace_id(self); }
static void nvmtl_stride_dropped(NSUInteger st) { static BOOL said; if (!said && st) { said = YES;
    nvlog("compute: attributeStride %lu dropped (dynamic strides not implemented) - buffer bound", (unsigned long)st); } }
- (void)setBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    nvmtl_stride_dropped(st); [self setBuffer:b offset:o atIndex:i]; }
- (void)setBufferOffset:(NSUInteger)o attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    nvmtl_stride_dropped(st); [self setBufferOffset:o atIndex:i]; }
- (void)setBuffers:(const id<MTLBuffer> __unsafe_unretained [])bs offsets:(const NSUInteger *)offs attributeStrides:(const NSUInteger *)sts withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setBuffer:bs ? bs[k] : nil offset:offs ? offs[k] : 0 attributeStride:sts ? sts[k] : 0 atIndex:r.location + k]; }
- (void)setBytes:(const void *)bytes length:(NSUInteger)len attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    nvmtl_stride_dropped(st); [self setBytes:bytes length:len atIndex:i]; }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])hs count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self useHeap:hs[i]]; }
- (void)setSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++)
        [self setSamplerState:s ? s[k] : nil lodMinClamp:lo ? lo[k] : 0.0f lodMaxClamp:hi ? hi[k] : FLT_MAX atIndex:r.location + k]; }
- (MTLDispatchType)dispatchType { return _e78Concurrent ? MTLDispatchTypeConcurrent : MTLDispatchTypeSerial; }
- (NVMTLComputePipelineState *)nvmtlSamplerPipeline {
    if (!_ps) return nil;
    NVMTLComputePipelineState *selected = _ps;
    if ((_samplers.count || _tgLen.count) && _e78VarSel && _e78VarFor == _ps && nvmtl_enc78_on(NVMTL_E78_H5)) {
        selected = _e78VarSel; nvmtl_enc78_count(E78C_VARHIT, 1);
    } else if (_samplers.count || _tgLen.count) {
        if (!_e78Key || !nvmtl_enc78_on(NVMTL_E78_H5))
            _e78Key = [NSJSONSerialization dataWithJSONObject:@{@"s": _samplers ?: @{}, @"tg": _tgLen ?: @{}} options:NSJSONWritingSortedKeys error:NULL];
        NSData *key = _e78Key;
        nvmtl_enc78_count(E78C_VARMISS, 1);
        @synchronized (_ps) {
            if (!_ps->_samplerVariants) _ps->_samplerVariants = [NSMutableDictionary new];
            id cached = _ps->_samplerVariants[key];
            if (cached == [NSNull null]) return nil;
            selected = cached;
            if (!selected) {
                NSString *why = nil;
                NSData *spirv = nvmtl_translate_variant(_ps->_function, _samplers, _tgLen, &why);
                if (spirv) {
                    selected = [NVMTLComputePipelineState new];
                    if (nvmtl_vk_compute_pipeline_create(spirv.bytes, spirv.length, &selected->_p)) {
                        selected = nil; why = @"Vulkan compute pipeline creation failed";
                    }
                    // A variant is re-translated from the kernel's AIR with this encoder's samplers or threadgroup sizes,
                    // so its module differs from the one the profile's edit was made from. Say so when the base was edited.
                    if (objc_getAssociatedObject(_ps, &g_app_edit_ps_key))
                        nvlog("profile: variant of edited kernel %s built (module %08x, %lu B): edit %s", _ps->_function->_fname.UTF8String,
                              nvmtl_spv_fnv1a(spirv.bytes, spirv.length), (unsigned long)spirv.length,
                              (selected && g_app_edit_last) ? "applied" : "NOT applied - this variant runs without the bounds check");
                }
                if (!selected) {
                    nvlog("compute pixel sampler specialization REFUSED %s: %s", _ps->_function->_fname.UTF8String, why.UTF8String);
                    _ps->_samplerVariants[key] = [NSNull null];
                    return nil;
                }
                _ps->_samplerVariants[key] = selected;
                nvlog("compute pixel sampler variant built: %s", _ps->_function->_fname.UTF8String);
            }
        }
        _e78VarFor = _ps; _e78VarSel = selected;
    }
    nvmtl_retain_resource(_cb, selected);
    nvmtl_bind_embedded_enc(_cb, NVMTL_SET_VERTEX, _ps->_function, self);
    return selected;
}
static int nvmtl_zprobe_on(void);
- (void)setComputePipelineState:(id<MTLComputePipelineState>)ps { _ps = (NVMTLComputePipelineState *)ps; nvmtl_retain_resource(_cb, ps);
    if (nvmtl_hz_mode()) nvmtl_hz_compute_writes(_cb, _ps);
    if (ps && nvmtl_zprobe_on())
        nvlog("ZENC setPipeline %s  tew=%lu maxT=%lu staticTG=%lu",
              _ps && _ps->_function ? _ps->_function->_fname.UTF8String : "?",
              (unsigned long)[ps threadExecutionWidth], (unsigned long)[ps maxTotalThreadsPerThreadgroup],
              (unsigned long)[ps staticThreadgroupMemoryLength]);
}
- (void)setBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i {
    NVMTLBuffer *nb = (NVMTLBuffer *)b;
    if (nb && (off & 15) && nvmtl_misaligned_copy_on() && nb->_b.map && nb->_b.size > off) {
        size_t n = nb->_b.size - off; if (n > ((size_t)4 << 20)) n = (size_t)4 << 20;
        static int said; if (!said++) nvlog("compute setBuffer: slot %lu at offset %lu is not 16-aligned - bound as a %lu-byte copy (NVMTL_MISALIGNED_BUFFER_COPY)", (unsigned long)i, (unsigned long)off, (unsigned long)n);
        NSData *orig = [NSData dataWithBytes:(const uint8_t *)nb->_b.map + off length:n];
        [self setBytes:orig.bytes length:n atIndex:i];
        // Bytes the kernel changed are carried back into the real buffer once the command buffer completes. Only changed
        // bytes move, so a write by anyone else to the rest of the range in the meantime is not undone.
        if (i < NVMTL_NBUFAT && _baObj[i] && ((NVMTLBuffer *)_baObj[i])->_b.map && _baLen[i] == n) {
            NVMTLBuffer *copy = (NVMTLBuffer *)_baObj[i]; size_t coff = _baOff[i]; NVMTLBuffer *home = nb; size_t hoff = off;
            [_cb addCompletedHandler:^(id<MTLCommandBuffer> done) {
                (void)done;
                if (!copy->_b.map || !home->_b.map || home->_b.size < hoff + n) return;
                const uint8_t *now = (const uint8_t *)copy->_b.map + coff, *was = orig.bytes; uint8_t *dst = (uint8_t *)home->_b.map + hoff;
                for (size_t k = 0; k < n; k++) if (now[k] != was[k]) dst[k] = now[k];
            }];
        } else {
            static int said2; if (!said2++) nvlog("compute setBuffer: the copy of an unaligned buffer could not be located - its writes are not carried back");
        }
        return;
    }
    if (!nb) {
        if (i < NVMTL_NBUFAT) { _baObj[i] = nil; _baN[i] = 0; }   // the binding table must not keep the previous resource
        size_t zoff = 0;
        NVMTLBuffer *zb = nvmtl_bytes_ring(_cb, 16, &zoff);
        const BOOL zring = zb != nil;
        if (!zring) {
            zb = [NVMTLBuffer new];
            if (nvmtl_vk_buffer_create(16, 1, &zb->_b)) { nvlog("compute setBuffer: nil at %lu - zero buffer create FAILED", (unsigned long)i); return; }
        }
        if (zb->_b.map) memset((uint8_t *)zb->_b.map + zoff, 0, 16);
        if (nvmtl_hz_bind_buffer(&_cb->_c, 1, NVMTL_SET_VERTEX, (uint32_t)i, &zb->_b, zoff)) nvlog("compute setBuffer: nil at %lu - bind FAILED", (unsigned long)i);
        if (!zring) {
            if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
            [_cb->_scratch addObject:zb];
        }
        if (nvmtl_zprobe_on()) nvlog("ZENC setBuffer  idx %-3lu len 0        off 0  (nil -> zero-filled)", (unsigned long)i);
        return;
    }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    // the same buffer at the same index of this encoder is already held by the command buffer's set
    if (i >= NVMTL_NBUFAT || _baObj[i] != (id)nb) [_cb->_resources addObject:nb];
    if (i < NVMTL_NBUFAT) { _baObj[i] = nb; _baOff[i] = off; _baN[i] = 2; }
    if (nvmtl_hz_bind_buffer(&_cb->_c, nvmtl_hz_untracked_buf(nb), NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, off)) nvlog("compute setBuffer: bind FAILED");
    if (nvmtl_zprobe_on()) {
        static const char *ZH = "0123456789abcdef";
        char hx[96]; size_t hn = 0;
        size_t cap = nb->_b.size < 32 ? nb->_b.size : 32;
        if (nb->_b.map) for (size_t k = 0; k < cap; k++) {
            uint8_t byte = ((const uint8_t *)nb->_b.map)[k];
            hx[hn++] = ZH[byte >> 4]; hx[hn++] = ZH[byte & 15];
        }
        hx[hn] = 0;
        nvlog("ZENC setBuffer  idx %-3lu len %-8lu off %lu  %s%s", (unsigned long)i,
              (unsigned long)[(id<MTLBuffer>)b length], (unsigned long)off,
              nb->_b.map ? "bytes " : "NO HOST MAP (private - bytes unreadable from here)", hx);
    }
}
- (void)setAccelerationStructure:(id<MTLAccelerationStructure>)as atBufferIndex:(NSUInteger)i {
    [self setBuffer:(id<MTLBuffer>)as offset:0 atIndex:i];
    NVMTLAccelerationStructure *a = (NVMTLAccelerationStructure *)as;
    if (a && [(id)a isKindOfClass:[NVMTLAccelerationStructure class]] && a->_uid.buf && _cb)
        nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, 31, &a->_uid, 0);
}
- (void)setBytes:(const void *)bytes length:(NSUInteger)len atIndex:(NSUInteger)i {
    if (i == 23 && len <= sizeof _nvcKP && bytes) { memcpy(_nvcKP, bytes, len); _nvcKPLen = (uint32_t)len; }
    else if (i == 29 && len == sizeof _nvcCP && bytes) { memcpy(_nvcCP, bytes, len); _nvcCPLen = (uint32_t)len; }
    size_t roff = 0;
    const BOOL dupable = _cb && bytes && len && len <= NVMTL_G79_DUPMAX && i < NVMTL_G79_DUPIDX && nvmtl_g79_on(NVMTL_G79_DUP);
    if (dupable && _cb->_g79BRing[i] && _cb->_g79BLen[i] == len && !memcmp(_cb->_g79B[i], bytes, len)) {
        if (nvmtl_hz_bind_buffer(&_cb->_c, 1, NVMTL_SET_VERTEX, (uint32_t)i, &_cb->_g79BRing[i]->_b, _cb->_g79BOff[i])) nvlog("compute setBytes: bind FAILED");
        // The binding table follows the binding: kernels run from compiled machine code read their buffer
        // addresses from it, and a stale entry hands the GPU an address that belongs to nothing.
        _baObj[i] = _cb->_g79BRing[i]; _baOff[i] = _baBase[i] = _cb->_g79BOff[i]; _baLen[i] = len; _baN[i] = 4;
        nvmtl_g79_count(G79C_BYTESDUP);
        return;
    }
    NVMTLBuffer *nb = nvmtl_bytes_ring(_cb, len, &roff);
    BOOL ring = nb != nil;
    if (!ring) {
        nb = [NVMTLBuffer new];
        if (nvmtl_vk_buffer_create(len < 16 ? 16 : len, 1, &nb->_b)) { nvlog("compute setBytes: create FAILED"); return; }
        nvmtl_enc78_count(E78C_BYTESOLD, 1);
    } else { nvmtl_enc78_count(E78C_RING, 1); nvmtl_enc78_count(E78C_RINGBYTES, len); }
    if (nb->_b.map) memcpy((uint8_t *)nb->_b.map + roff, bytes, len);
    if (dupable) {
        _cb->_g79BRing[i] = ring && nb->_b.map ? nb : nil; _cb->_g79BOff[i] = roff; _cb->_g79BLen[i] = (uint32_t)len;
        memcpy(_cb->_g79B[i], bytes, len);
    }
    if (i < NVMTL_NBUFAT) { _baObj[i] = nb; _baOff[i] = _baBase[i] = roff; _baLen[i] = len; _baN[i] = 4; }   // buffer, offset, inline length, base
    if (nvmtl_hz_bind_buffer(&_cb->_c, 1, NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, roff)) nvlog("compute setBytes: bind FAILED");
    if (nvmtl_zprobe_on()) {
        const uint32_t *zw = (const uint32_t *)bytes; char zb[220]; int zo = 0;
        for (NSUInteger k = 0; (k + 1) * 4 <= len && k < 13 && zo < 190; k++)
            zo += snprintf(zb + zo, sizeof zb - (size_t)zo, "%08x ", zw[k]);
        zb[zo] = 0;
        nvlog("ZENC setBytes   idx %-3lu len %-4lu map %s words: %s", (unsigned long)i,
              (unsigned long)len, nb->_b.map ? "yes" : "NO", zb);
    }
    if (ring) return;
    if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
    [_cb->_scratch addObject:nb];
}

#define NVMTL_ZPROBE_MAX 64
#define NVMTL_ZDISP_MAX 400
static NSMutableArray *g_zprobe;
static int nvmtl_zprobe_on(void) {
    static int on = -1;
    if (on < 0) on = NVMTL_RELEASE ? 0 : ([[NSProcessInfo processInfo] environment][@"NVMTL_ZPROBE"] ? 1 : 0);
    return on;
}
static void nvmtl_zprobe_dispatch(const char *kernel, const uint32_t *t, const uint32_t *l) {
    if (!nvmtl_zprobe_on()) return;
    static unsigned seq = 0;
    if (seq >= NVMTL_ZDISP_MAX) return;
    seq++;
    nvlog("ZDISP #%u %s threads %ux%ux%u local %ux%ux%u", seq, kernel ? kernel : "(no pipeline set)",
          t[0], t[1], t[2], l[0], l[1], l[2]);
}
static void nvmtl_zprobe_record(NVMTLTexture *t, const char *kernel, unsigned idx, unsigned stex) {
    if (!nvmtl_zprobe_on() || !t) return;
    @synchronized ([NVMTLTexture class]) {
        if (!g_zprobe) g_zprobe = [NSMutableArray new];
        if (g_zprobe.count >= NVMTL_ZPROBE_MAX) return;
        [g_zprobe addObject:@[t, @(kernel ?: "(no pipeline set)"), @(idx), @(stex)]];
    }
}
static void nvmtl_zprobe_drain(void) {
    if (!nvmtl_zprobe_on()) return;
    NSArray *snap = nil;
    @synchronized ([NVMTLTexture class]) { snap = g_zprobe; g_zprobe = nil; }
    for (NSArray *e in snap) {
        NVMTLTexture *t = e[0];
        const char *kn = [e[1] UTF8String];
        unsigned idx = [e[2] unsignedIntValue], stex = [e[3] unsignedIntValue];
        nvk_image *im = [t nvi];
        if (!im || !im->w || !im->h || !im->bpp) { nvlog("ZPROBE %s tex(%u) stex=%u: %p has no image - SKIPPED (a fact about the probe, not about the pixels)", kn, idx, stex, (__bridge void *)t); continue; }
        uint32_t rw = im->w > 256 ? 256 : im->w, rh = im->h > 256 ? 256 : im->h;
        size_t row = (size_t)rw * im->bpp;
        NSMutableData *md = [NSMutableData dataWithLength:row * rh];
        if (!md) continue;
        [t getBytes:md.mutableBytes bytesPerRow:row fromRegion:MTLRegionMake2D(0, 0, rw, rh) mipmapLevel:0];
        unsigned nz = 0, nn = 0;
        nvmtl_nonzero(md.mutableBytes, row, row, rh, &nz, &nn);
        nvlog("ZPROBE %s tex(%u) stex=%u: %p %ux%u x%u slice(s) bpp %u storage %u -> NONZERO %u/%u bytes in %ux%u of slice 0",
              kn, idx, stex, (__bridge void *)t, im->w, im->h, im->layers, im->bpp, im->storage, nz, nn, rw, rh);
    }
}
- (void)setTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTexture *nt = (NVMTLTexture *)t;
    if (nvmtl_sass_tables_wanted()) {
        if (!_sassTextures) _sassTextures = [NSMutableDictionary new];
        if (nt) _sassTextures[@(i)] = nt; else [_sassTextures removeObjectForKey:@(i)];
    }
    if (!nt) { nvlog("compute setTexture: nil at %lu", (unsigned long)i); return; }
    if (nt->_parent && nt->_baseLevel) nt->_parent->_mipsWritten = YES;
    nvmtl_zprobe_record(nt, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, (unsigned)i, nt->_stex ? 1u : 0u);
    nvmtl_retain_resource(_cb, nt);
    nvmtl_surface_in_for_cb(_cb, nt); if (nt->_stex) nvmtl_surf_dirty(_cb, nt);
    if (nt->_tbView) nvmtl_hz_bind_texel(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_vk_cmd_prepare_sampled_image(&_cb->_c, [nt nvi], nt->_stex)) { nvlog("compute setTexture: image layout preparation refused"); return; }
    if (nvmtl_hz_bind_texture(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, [nt nvsampleview])) nvlog("compute setTexture: bind FAILED");
    nvmtl_hz_bind_storage(&_cb->_c, [nt nvi], nvmtl_hz_untracked_tex(nt), NVMTL_SET_VERTEX, (uint32_t)i, nvmtl_storage_view_for(_cb, nt, nt->_stex || nvmtl_function_writes_texture(_ps ? _ps->_function : nil, i)));
    }
}
- (void)nvmtlE78Barrier:(BOOL)indirect {
    int kind;
    if (indirect) kind = 0;
    else if (!_e78NDisp) {
        kind = _cb->_g79C2C && nvmtl_g79_on(NVMTL_G79_ENC) ? 1 : 0;
        nvmtl_g79_count(kind ? G79C_ENCNARROW : G79C_ENCFULL);
    }
    // A concurrent compute encoder's later dispatches used to get NO barrier at all unless the app had called
    // memoryBarrierWithScope: (the H7 row: kind 2 = none). That reads Metal's concurrent dispatch type as a
    // promise that the dispatches are independent, and Cyberpunk's are not: a later dispatch read a buffer an
    // earlier one in the same encoder was still writing, so it consumed garbage - wrong lighting, and pointers
    // loaded out of that garbage faulted the channel at addresses no buffer covers (FAULT_PTE / FAULT_PDE).
    // Measured on the benchmark, every other setting held: H7 on faulted in every run that reached
    // the light pass; NVMTL_ENC78_OFF=h7 completed at 39.28 fps against 41.08 with it on, so the narrow
    // compute->compute barrier it costs is within noise. Concurrent dispatches now take that same narrow
    // barrier serial ones do (P1); NVMTL_ENC78_OFF still turns the rows off for an A/B.
    else if (nvmtl_enc78_on(NVMTL_E78_P1)) kind = 1;
    else kind = 0;
    // Hazard model: real mode decides the first dispatch's barrier (none without a conflict); any barrier kept here is
    // emitted before the model adds this dispatch, so a full one cannot erase it. Census shadows the first-dispatch full
    // barrier real mode would not emit.
    const int hzm = nvmtl_hz_mode(), hzfirst = !indirect && !_e78NDisp;
    if (hzm) {
        if (hzm == 1 && hzfirst) kind = 2;
        if (hzm == 1 && kind == 1 && nvmtl_hz_dispatch_decides()) kind = 2;   // the model records the narrow barrier on a conflict
        if (kind == 0) { nvmtl_hz_shadow(hzm == 2 && hzfirst); nvmtl_vk_barrier_site(hzfirst ? NVMTL_BARS_FIRST_DISPATCH : NVMTL_BARS_LATER_DISPATCH); nvmtl_vk_cmd_barrier(&_cb->_c); nvmtl_hz_shadow(0); }
        else if (kind == 1) nvmtl_vk_cmd_barrier_c2c(&_cb->_c);
        kind = 2;   // emitted above
        nvmtl_hz_dispatch(&_cb->_c, hzfirst);
    }
    _g79Bar = NO;
    const BOOL firstDispatch = !_e78NDisp;
    _e78NDisp++;
    if (kind == 0) { nvmtl_vk_barrier_site(firstDispatch ? NVMTL_BARS_FIRST_DISPATCH : NVMTL_BARS_LATER_DISPATCH); nvmtl_vk_cmd_barrier(&_cb->_c); }
    else if (kind == 1) nvmtl_vk_cmd_barrier_c2c(&_cb->_c);
    if (kind) _e78Loose = YES;
    nvmtl_enc78_count(E78C_DISP, 1); nvmtl_enc78_count(E78C_FULL + kind, 1);
    if (_e78Concurrent) nvmtl_enc78_count(E78C_CONC, 1);
}
// A threadgroup of 1024 threads fits once per SM at 40 registers (32 of 48 warps) and MPS's Gaussian ran 1.5x
// slower in 32x32 groups than in 32x4 on the RTX 5060 (gblin, identical pixels). A kernel that cannot see its
// grouping (nvmtl_air_grid_only) runs the same threads in groups of at most 128: height first, then depth, then
// width down to 32, each halved only while it stays a divisor, so the thread set and every grid position are
// unchanged. NVMTL_TG_RESHAPE=0 turns it off for an A/B run; =force skips the grid-only check, which only a test
// uses, to prove its probe sees a kernel that depends on its grouping.
static void nvmtl_tg_reshape(NVMTLComputePipelineState *ps, const uint32_t l[3], uint32_t out[3])
{
    out[0] = l[0]; out[1] = l[1]; out[2] = l[2];
    static int mode = -1;
    if (mode < 0) { const char *e = getenv("NVMTL_TG_RESHAPE"); mode = !e ? 1 : e[0] == '0' ? 0 : e[0] == 'f' ? 2 : 1; }
    if (!mode || !ps || l[0] * l[1] * l[2] <= 128) return;
    if (mode == 2) ps->_gridOnly = 1;
    if (!ps->_gridOnly) {
        ps->_gridOnly = nvmtl_air_grid_only(ps->_function) ? 1 : -1;
        if (ps->_gridOnly > 0) nvlog("dispatch: %s uses only its grid position - threadgroups above 128 threads are split",
                                     ps->_function ? ps->_function->_fname.UTF8String : "-");
    }
    if (ps->_gridOnly < 0) return;
    while (out[0] * out[1] * out[2] > 128 && out[1] % 2 == 0) out[1] /= 2;
    while (out[0] * out[1] * out[2] > 128 && out[2] % 2 == 0) out[2] /= 2;
    while (out[0] * out[1] * out[2] > 128 && out[0] > 32 && out[0] % 2 == 0) out[0] /= 2;
}
- (void)dispatchThreadgroups:(MTLSize)grid threadsPerThreadgroup:(MTLSize)tg {
    if (_ps && _ps->_sassKernel) {
        int ran = nvmtl_sass_dispatch(self, grid, tg, 0, !_ps->_sassOnly);
        if (ran) { if (ran > 0 && _cb->_stTwins.count) nvmtl_storage_twins_out(_cb); return; }
    }
    if (!_nvcIn && nvmtl_nvconv_try(self, grid, tg)) return;
    NVMTLComputePipelineState *selected = [self nvmtlSamplerPipeline];
    if (!selected) { nvlog("dispatch with no valid compute sampler pipeline"); return; }
    uint32_t l[3] = { (uint32_t)tg.width, (uint32_t)tg.height, (uint32_t)tg.depth };
    uint32_t t[3] = { (uint32_t)(grid.width * tg.width), (uint32_t)(grid.height * tg.height), (uint32_t)(grid.depth * tg.depth) };
    if (nvmtl_vk_tg_reject(l, t, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, selected->_p.lin)) return;
    nvmtl_zprobe_dispatch(_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, t, l);
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_);
        nvtrace("DISPATCH %s | threads %ux%ux%u tg %ux%ux%u", (_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL) ?: "-", t[0], t[1], t[2], l[0], l[1], l[2]);
        nvtrace("  BUFS%s", bufs_); }
    [self nvmtlE78Barrier:NO];
    if (nvmtl_vendor_dispatch_sass(selected, &_cb->_c, t, l) != 0) {
        uint32_t lr[3]; nvmtl_tg_reshape(_ps, l, lr);
        nvmtl_cbt_kernel(_cb, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL);
        nvmtl_vk_dispatch_label(_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL);
        nvmtl_vk_dispatch_key(_ps ? nvmtl_function_spv_fnv(_ps->_function) : 0, _ps ? nvmtl_function_air_key(_ps->_function) : 0);
        nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &selected->_p, t, lr);
    }
    if (_cb->_stTwins.count) nvmtl_storage_twins_out(_cb);
}
- (void)dispatchThreads:(MTLSize)threads threadsPerThreadgroup:(MTLSize)tg {
    if (_ps && _ps->_sassKernel) {
        int ran = nvmtl_sass_dispatch(self, threads, tg, 1, !_ps->_sassOnly);
        if (ran) { if (ran > 0 && _cb->_stTwins.count) nvmtl_storage_twins_out(_cb); return; }
    }
    NVMTLComputePipelineState *selected = [self nvmtlSamplerPipeline];
    if (!selected) { nvlog("dispatch with no valid compute sampler pipeline"); return; }
    uint32_t l[3] = { (uint32_t)tg.width, (uint32_t)tg.height, (uint32_t)tg.depth };
    uint32_t t[3] = { (uint32_t)threads.width, (uint32_t)threads.height, (uint32_t)threads.depth };
    if (nvmtl_vk_tg_reject(l, t, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, selected->_p.lin)) return;
    nvmtl_zprobe_dispatch(_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, t, l);
    if (nvmtl_trace_on()) { char bufs_[600]; nvmtl_vk_trace_buffers(&_cb->_c, bufs_, sizeof bufs_);
        nvtrace("DISPATCH %s | threads %ux%ux%u tg %ux%ux%u", (_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL) ?: "-", t[0], t[1], t[2], l[0], l[1], l[2]);
        nvtrace("  BUFS%s", bufs_); }
    [self nvmtlE78Barrier:NO];
    if (nvmtl_vendor_dispatch_sass(selected, &_cb->_c, t, l) != 0) {
        uint32_t lr[3]; nvmtl_tg_reshape(_ps, l, lr);
        nvmtl_cbt_kernel(_cb, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL);
        nvmtl_vk_dispatch_label(_ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL);
        nvmtl_vk_dispatch_key(_ps ? nvmtl_function_spv_fnv(_ps->_function) : 0, _ps ? nvmtl_function_air_key(_ps->_function) : 0);
        nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &selected->_p, t, lr);
    }
    if (_cb->_stTwins.count) nvmtl_storage_twins_out(_cb);
}
- (void)setBufferOffset:(NSUInteger)off atIndex:(NSUInteger)i {
    if (i < NVMTL_NBUFAT && _baN[i]) { _baOff[i] = off; _baN[i] = 2; }
    if (nvmtl_vk_bind_buffer_offset(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, off))
        nvlog("compute setBufferOffset: nothing bound at index %lu", (unsigned long)i);
}
// MPS (Lanczos scale, used by video players) sets an imageblock size on compute encoders.
// The translator keeps a kernel's imageblock cells in per-thread private memory and derives
// the block extent from the threadgroup size, so no encoder state is needed; without this
// method the call is an unrecognized selector and the app aborts.
- (void)setImageblockWidth:(NSUInteger)w height:(NSUInteger)h {
    static int logged;
    if (!logged++) nvlog("compute setImageblockWidth:%lu height:%lu (imageblock lives in thread-private memory)",
                         (unsigned long)w, (unsigned long)h);
}
- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i {
    if (!_tgLen) _tgLen = [NSMutableDictionary new];
    if (nvmtl_zprobe_on()) nvlog("ZENC tgMemory   idx %-3lu length %lu", (unsigned long)i, (unsigned long)len);
    NSUInteger was = [_tgLen[@(i).stringValue] unsignedIntegerValue];
    if (len) _tgLen[@(i).stringValue] = @(len);
    else     [_tgLen removeObjectForKey:@(i).stringValue];
    if (was != len) { _e78Key = nil; _e78VarSel = nil; }
}
// GPU-encoded dispatches (NVMTL_GPU_ICB=1). The threadgroup size a kernel recorded is only in the GPU record, and Vulkan needs it when the
// dispatch is recorded, so every candidate pipeline is tried at the sizes an encoder normally uses (the pipeline's thread execution width,
// its maximum, and its required size when it has one); a command recorded with any other size does not run, and that is logged.
// Buffers are the encoder's own bindings, as for draws.
- (void)nvmtlGpuIcb:(NVMTLIndirectCommandBuffer *)b loc:(uint32_t)loc len:(uint32_t)len range:(NVMTLBuffer *)rb offset:(NSUInteger)ro {
    if (!b->_recBuf) return;
    const uint32_t slots = nvmtl_icb_slots(b->_cmds.count);
    NSMutableArray *cands = [NSMutableArray array];
    if (_ps) [cands addObject:_ps];
    for (id p in nvmtl_icb_pipelines(YES)) if (p != _ps) [cands addObject:p];
    NSMutableArray *sizes = [NSMutableArray arrayWithCapacity:cands.count * 3];   // per candidate: up to three distinct sizes
    uint64_t total = 0;
    for (id<MTLComputePipelineState> c in cands) {
        NSMutableArray *mine = [NSMutableArray array];
        const MTLSize req = [(id)c isKindOfClass:[NVMTLComputePipelineState class]] ? ((NVMTLComputePipelineState *)c)->_reqTG : MTLSizeMake(0, 0, 0);
        const NSUInteger cand[3][3] = { { c.threadExecutionWidth, 1, 1 }, { c.maxTotalThreadsPerThreadgroup, 1, 1 }, { req.width, req.height, req.depth } };
        for (int k = 0; k < 3; k++) {
            if (!cand[k][0] || cand[k][0] > UINT32_MAX) continue;
            BOOL dup = NO; for (NSArray *m in mine) if ([m[0] unsignedLongValue] == cand[k][0] && [m[1] unsignedLongValue] == cand[k][1] && [m[2] unsignedLongValue] == cand[k][2]) dup = YES;
            if (!dup) [mine addObject:@[@(cand[k][0]), @(cand[k][1]), @(cand[k][2])]];
        }
        [sizes addObject:mine]; total += (uint64_t)mine.count * slots;
    }
    if (!slots || !cands.count || total > 262144u) { nvlog("GPU ICB compute: %u slot(s), %lu candidate kernel(s): nothing to run or over the 262144 indirect dispatches one execution may record", slots, (unsigned long)cands.count); return; }
    NVMTLBuffer *big = nvmtl_icb_scratch_bytes((NSUInteger)total * NVMTL_ICB_RECORD_BYTES);
    if (!big) { nvlog("GPU ICB compute: scratch FAILED"); return; }
    nvmtl_retain_resource(_cb, big); nvmtl_retain_resource(_cb, b->_recBuf); if (rb) nvmtl_retain_resource(_cb, rb);
    NVMTLComputePipelineState *orig = _ps; size_t at = 0;
    for (NSUInteger k = 0; k < cands.count; k++)
        for (NSArray *sz in sizes[k]) {
            const uint32_t tg[3] = { (uint32_t)[sz[0] unsignedLongValue], (uint32_t)[sz[1] unsignedLongValue], (uint32_t)[sz[2] unsignedLongValue] };
            if (nvmtl_vk_cmd_icb_gather(&_cb->_c, &b->_recBuf->_b, &big->_b, at * slots * NVMTL_ICB_RECORD_BYTES, slots, 1, ((id<MTLComputePipelineState>)cands[k]).gpuResourceID._impl, 0, loc, len, tg, 0, 0, rb ? &rb->_b : NULL, ro)) { nvlog("GPU ICB compute: the gather pass FAILED"); return; }
            at++;
        }
    // per-command buffers: kernel buffers are the set-0 address table, patched per slot from the records (nvmtl_icb_record.h)
    BOOL tabs = NO;
    if (slots <= NVMTL_ICB_MAX_PATCH_SLOTS) {
        NVMTLBuffer *dsts = nvmtl_icb_scratch_bytes((NSUInteger)slots * 16u);
        if (dsts) { nvmtl_retain_resource(_cb, dsts); tabs = nvmtl_vk_cmd_icb_tables(&_cb->_c, &b->_recBuf->_b, &dsts->_b, slots) == 0; }
    }
    if (!tabs) { static int said; if (!said++) nvlog("GPU ICB compute: the per-command buffer tables could not be prepared - dispatches use the encoder's own buffer bindings"); }
    at = 0; NSUInteger run = 0;
    for (NSUInteger k = 0; k < cands.count; k++) {
        [self setComputePipelineState:cands[k]];
        for (NSArray *sz in sizes[k]) {
            const MTLSize tg = MTLSizeMake([sz[0] unsignedLongValue], [sz[1] unsignedLongValue], [sz[2] unsignedLongValue]);
            for (uint32_t i = 0; i < slots; i++, run++) {
                nvmtl_vk_cmd_icb_use(&_cb->_c, tabs ? i + 1u : 0u);
                [self dispatchThreadgroupsWithIndirectBuffer:big indirectBufferOffset:(at * slots + i) * NVMTL_ICB_RECORD_BYTES threadsPerThreadgroup:tg];
            }
            at++;
        }
    }
    nvmtl_vk_cmd_icb_use(&_cb->_c, 0); nvmtl_vk_cmd_icb_tables_free(&_cb->_c);
    if (orig) [self setComputePipelineState:orig];
    nvlog("GPU ICB compute: %lu indirect dispatch(es) recorded for %u slot(s), %lu candidate kernel(s); commands recorded with another threadgroup size do not run", (unsigned long)run, slots, (unsigned long)cands.count);
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r {
    NVMTLIndirectCommandBuffer *b = (NVMTLIndirectCommandBuffer *)icb;
    if (!b) { nvlog("compute executeCommandsInBuffer: nil"); return; }
    NSUInteger n = 0;
    for (NSUInteger i = r.location; i < r.location + r.length && i < b->_cmds.count; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i];
        if (!c->_armed || !c->_isCompute) continue;
        if (c->_cps) [self setComputePipelineState:c->_cps];
        for (NSArray *e in c->_kbufs) [self setBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
        [self dispatchThreadgroups:c->_grid threadsPerThreadgroup:c->_tg];
        n++;
    }
    if (b->_recBuf && r.length <= UINT32_MAX && r.location <= UINT32_MAX) [self nvmtlGpuIcb:b loc:(uint32_t)r.location len:(uint32_t)r.length range:nil offset:0];
    nvlog("compute executeCommandsInBuffer: replayed %lu of %lu slot(s)", (unsigned long)n, (unsigned long)r.length);
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(id<MTLBuffer>)ib indirectBufferOffset:(NSUInteger)ioff {
    NVMTLIndirectCommandBuffer *b = (NVMTLIndirectCommandBuffer *)icb; NVMTLBuffer *rb = (NVMTLBuffer *)ib;
    const uint32_t slots = b ? nvmtl_icb_slots(b->_cmds.count) : 0;
    if (!b || !rb || !slots || ioff + 8 > rb.length || (ioff & 3)) { nvlog("compute executeCommandsInBuffer:indirectBuffer: unusable arguments (icb %p, buffer %p, %u slot(s), offset %lu) - nothing runs", b, rb, slots, (unsigned long)ioff); return; }
    NVMTLBuffer *sc = nvmtl_icb_scratch(slots);
    if (!sc) { nvlog("compute executeCommandsInBuffer:indirectBuffer: scratch of %u records FAILED - nothing runs", slots); return; }
    uint32_t *rec = (uint32_t *)sc.contents;
    for (uint32_t i = 0; i < slots; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i];
        if (c->_armed && c->_isCompute) nvmtl_icb_record_dispatch(rec + i * NVMTL_ICB_RECORD_WORDS, (uint32_t)c->_grid.width, (uint32_t)c->_grid.height, (uint32_t)c->_grid.depth);
    }
    nvmtl_retain_resource(_cb, sc); nvmtl_retain_resource(_cb, rb);
    if (nvmtl_vk_cmd_icb_mask(&_cb->_c, &sc->_b, &rb->_b, ioff, slots, NVMTL_ICB_ZERO_DISPATCH)) { nvlog("compute executeCommandsInBuffer:indirectBuffer: the range pass FAILED - nothing runs"); return; }
    NSUInteger n = 0;
    for (uint32_t i = 0; i < slots; i++) {
        NVMTLIndirectCommand *c = b->_cmds[i];
        if (!c->_armed || !c->_isCompute) continue;
        if (c->_cps) [self setComputePipelineState:c->_cps];
        for (NSArray *e in c->_kbufs) [self setBuffer:e[0] offset:[e[1] unsignedLongValue] atIndex:[e[2] unsignedLongValue]];
        [self dispatchThreadgroupsWithIndirectBuffer:sc indirectBufferOffset:(NSUInteger)i * NVMTL_ICB_RECORD_BYTES threadsPerThreadgroup:c->_tg];
        n++;
    }
    nvlog("compute executeCommandsInBuffer:indirectBuffer: %lu armed slot(s) of %u recorded as range-masked indirect dispatches", (unsigned long)n, slots);
    if (b->_recBuf) [self nvmtlGpuIcb:b loc:0 len:0 range:rb offset:ioff];
}
- (void)setSamplerState:(id<MTLSamplerState>)ss atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)ss;
    if (nvmtl_sass_tables_wanted()) {
        if (!_sassSamplers) _sassSamplers = [NSMutableDictionary new];
        if (ns) _sassSamplers[@(i)] = ns; else [_sassSamplers removeObjectForKey:@(i)];
    }
    if (!_samplers) _samplers = [NSMutableDictionary new];
    NSString *key = [NSString stringWithFormat:@"%lu", (unsigned long)i];
    id was = _samplers[key], now = ns ? ns->_pixelState : nil;
    if (ns && ns->_pixelState) _samplers[key] = ns->_pixelState;
    else [_samplers removeObjectForKey:key];
    if (was != now && ![was isEqual:now]) { _e78Key = nil; _e78VarSel = nil; }
    nvmtl_retain_resource(_cb, ns);
    if (nvmtl_vk_bind_sampler(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, ns ? &ns->_s : NULL)) nvlog("compute setSamplerState: bind FAILED");
}
- (void)setSamplerState:(id<MTLSamplerState>)ss lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i {
    [self setSamplerState:ss atIndex:i];
}
- (void)setBuffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)offs withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (b[k]) [self setBuffer:b[k] offset:offs ? offs[k] : 0 atIndex:r.location + k];
}
- (void)setTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) if (t[k]) [self setTexture:t[k] atIndex:r.location + k];
}
- (void)setSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setSamplerState:ss[k] atIndex:r.location + k];
}
- (void)dispatchThreadgroupsWithIndirectBuffer:(id<MTLBuffer>)buf indirectBufferOffset:(NSUInteger)off
                         threadsPerThreadgroup:(MTLSize)tg {
    NVMTLBuffer *nb = (NVMTLBuffer *)buf;
    if (!nb) { nvlog("dispatchThreadgroupsWithIndirectBuffer: nil"); return; }
    nvmtl_retain_resource(_cb, nb);
    // a pipeline with only compiled machine code has no Vulkan pipeline to run an indirect dispatch on
    if (_ps && _ps->_sassOnly) {
        [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
            userInfo:@{ NSLocalizedDescriptionKey: @"Indirect dispatch is not available for this kernel" }]];
        return;
    }
    NVMTLComputePipelineState *selected = [self nvmtlSamplerPipeline];
    if (!selected) { nvlog("indirect dispatch with no valid compute sampler pipeline"); return; }
    uint32_t l[3] = { (uint32_t)tg.width, (uint32_t)tg.height, (uint32_t)tg.depth };
    if (nvmtl_vk_tg_reject(l, NULL, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, selected->_p.lin)) return;
    [self nvmtlE78Barrier:YES];
    if (![self nvmtlPrepareIndirectImages]) return;
    nvmtl_cbt_kernel(_cb, selected->_function ? selected->_function->_fname.UTF8String : NULL);
    nvmtl_vk_dispatch_label(selected->_function ? selected->_function->_fname.UTF8String : NULL);
    nvmtl_vk_dispatch_key(nvmtl_function_spv_fnv(selected->_function), nvmtl_function_air_key(selected->_function));
    nvmtl_vk_cmd_dispatch_indirect_tg(&_cb->_c, &selected->_p, &nb->_b, off, l);
    if (_cb->_stTwins.count) nvmtl_storage_twins_out(_cb);
}
// Before an indirect dispatch, every texture the encoder declared with useResource:usage: is brought in for this command
// buffer and moved to the layout the access needs: GENERAL when it may be written as a storage image, read-only otherwise.
// A texture written earlier in the frame as a render target is still in its attachment layout; on NVIDIA that layout can
// keep the surface compressed, and a storage write into it without the transition reaches memory the image does not own
// (Cyberpunk's 2048x2048 RG11B10 clear faulted in a page no image or buffer held). Direct dispatches get the same
// preparation through setTexture:atIndex:; textures reached only through an argument buffer get it here.
- (BOOL)nvmtlPrepareIndirectImages {
    for (NSValue *key in _indirectImages) {
        NSArray *entry = _indirectImages[key]; NVMTLTexture *t = entry[0];
        MTLResourceUsage usage = [entry[1] unsignedLongLongValue];
        nvmtl_surface_in_for_cb(_cb, t);
        if (usage & MTLResourceUsageWrite) nvmtl_surf_dirty(_cb, t);
        if (nvmtl_vk_cmd_prepare_sampled_image(&_cb->_c, [t nvi], (usage & MTLResourceUsageWrite) && t->_stex)) {
            [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:@"Cannot prepare a declared indirect texture for dispatch"}]];
            return NO;
        }
    }
    return YES;
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u {
    nvmtl_retain_resource(_cb, r);
    nvmtl_hz_use(_cb, r, u);
    if ([(id)r isKindOfClass:[NVMTLTexture class]]) {
        NVMTLTexture *t = (NVMTLTexture *)r;
        if (!t->_tbView) {
            if (!_indirectImages) _indirectImages = [NSMutableDictionary new];
            _indirectImages[[NSValue valueWithPointer:[t nvi]]] = @[t, @(u)];
        }
    }
}
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i=0;i<n;i++) [self useResource:r[i] usage:u]; }
- (void)useHeap:(id<MTLHeap>)h { nvmtl_use_heap(_cb, h); }
- (void)sampleCountersInBuffer:(id)sb atSampleIndex:(NSUInteger)i withBarrier:(BOOL)b { nvmtl_sample_counter(_cb, sb, i, self, b); }
- (void)memoryBarrierWithScope:(MTLBarrierScope)scope {
    if (nvmtl_g79_on(NVMTL_G79_BAR)) { _g79Bar = YES; nvmtl_g79_count(G79C_BARLAZY); } else { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); } }
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)memoryBarrierWithResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n {
    if (nvmtl_g79_on(NVMTL_G79_BAR)) { _g79Bar = YES; nvmtl_g79_count(G79C_BARLAZY); } else { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); } }
- (void)pushDebugGroup:(NSString *)g { nvtrace("GROUP %s", g.UTF8String ?: "?"); }
- (void)popDebugGroup {}
- (void)insertDebugSignpost:(NSString *)sp {}
- (void)endEncoding {
    if (nvmtl_g79_on(NVMTL_G79_ENC)) {
        if (_e78NDisp) { _cb->_g79C2C = YES; if ((_e78Loose || _g79Bar) && nvmtl_hz_mode() != 1) _cb->_g79Owed = YES; }
        _e78Loose = NO; _g79Bar = NO;
    } else if (_e78Loose || _g79Bar) { if (nvmtl_hz_mode() != 1) { nvmtl_hz_shadow(1); nvmtl_vk_barrier_site(NVMTL_BARS_ENCODER_END); nvmtl_vk_cmd_barrier(&_cb->_c); nvmtl_hz_shadow(0); } _e78Loose = NO; _g79Bar = NO; nvmtl_enc78_count(E78C_ENDFULL, 1); }
    nvmtl_g79_census();
    nvmtl_hz_encoder_end(&_cb->_c);
    [_cb->_stTwins removeAllObjects];   // the next encoder copies each twin in again
    _cb->_nEnd++; nvmtl_enc78_census();
    // The command buffer holds every resource until the GPU is done; the encoder's own binding tables are dead after
    // this point, as in Apple's encoders. WAS: kept until dealloc, which MPS defers to the completion handler; by then
    // Espresso had recycled the objects they named and .cxx_destruct released freed memory (mediaanalysisd, issue #107).
    for (unsigned k = 0; k < NVMTL_NBUFAT; k++) { _baObj[k] = nil; _baN[k] = 0; }
    _samplers = nil; _tgLen = nil; _e78Key = nil;
}
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
- (id<MTLCommandBuffer>)commandBuffer { return (id<MTLCommandBuffer>)_cb; }   // SPI, see the render encoder
// intersection function tables (formerly the RT3IntersectionTables category in NVMTLLibraryLoad.m)
- (void)setIntersectionFunctionTable:(id<MTLIntersectionFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLIntersectionFunctionTable class]]) { nvlog("setIntersectionFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    [self setBuffer:t ? ((NVMTLIntersectionFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setIntersectionFunctionTables:(const id<MTLIntersectionFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setIntersectionFunctionTable:ts[k] atBufferIndex:r.location + k];
}
// function pointers and visible function tables (formerly the B39FunctionPointers category in NVMTLLibraryLoad.m)
- (void)setVisibleFunctionTable:(id<MTLVisibleFunctionTable>)t atBufferIndex:(NSUInteger)i {
    if (t && ![(id)t isKindOfClass:[NVMTLVisibleFunctionTable class]]) { nvlog("setVisibleFunctionTable: foreign %s - REFUSED", NSStringFromClass([(id)t class]).UTF8String); return; }
    { static _Atomic unsigned n; if (atomic_fetch_add(&n, 1) < 4) { FILE *lg = fopen(nvmtl_log_path(), "a"); if (lg) { fprintf(lg, "pid %d NVMTL: RTBIND setVisibleFunctionTable index %lu %s\n", getpid(), (unsigned long)i, t ? "table" : "nil"); fclose(lg); } } }
    [self setBuffer:t ? ((NVMTLVisibleFunctionTable *)t)->_buf : nil offset:0 atIndex:i];
}
- (void)setVisibleFunctionTables:(const id<MTLVisibleFunctionTable> [])ts withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setVisibleFunctionTable:ts[k] atBufferIndex:r.location + k];
}
@end

static NSUInteger nvmtl_texture_slice_count(NVMTLTexture *t) {
    MTLTextureType type = t.textureType;
    return type == MTLTextureTypeCube ? 6 : type == MTLTextureTypeCubeArray ? t.arrayLength * 6 : t.arrayLength;
}
// A private BGR10A2Unorm texture with no ShaderWrite, so it keeps VkFormat 58: Metal's own byte layout. Held by the command
// buffer until it completes.
static NVMTLTexture *nvmtl_rb_temp(NVMTLCommandBuffer *cb, id<MTLDevice> dev, NSUInteger w, NSUInteger h)
{
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGR10A2Unorm width:w height:h mipmapped:NO];
    td.storageMode = MTLStorageModePrivate; td.usage = MTLTextureUsageShaderRead | MTLTextureUsageRenderTarget;
    NVMTLTexture *t = (NVMTLTexture *)[dev newTextureWithDescriptor:td];
    if (!t || t->_rbSwap) { nvlog("blit: no BGR10A2 staging texture %lux%lu - this copy keeps R and B exchanged", (unsigned long)w, (unsigned long)h); return nil; }
    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    [cb->_resources addObject:t];
    return t;
}
static BOOL nvmtl_rb_root(NVMTLTexture *t) { return t && (t->_parent ?: t)->_rbSwap; }
@implementation NVMTLBlitCommandEncoder
- (unsigned long long)globalTraceObjectID { return nvmtl_global_trace_id(self); }
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so toBuffer:(id<MTLBuffer>)dst
     destinationOffset:(NSUInteger)dof size:(NSUInteger)size {
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    NVMTLBuffer *s = (NVMTLBuffer *)src, *d = (NVMTLBuffer *)dst;
    if (!s || !d) { nvlog("blit copyFromBuffer: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s];
    [_cb->_resources addObject:d];
    if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &s->_b, so, &d->_b, dof, size)) nvlog("blit copyFromBuffer: FAILED");
}
- (void)fillBuffer:(id<MTLBuffer>)buf range:(NSRange)range value:(uint8_t)value {
    NVMTL_HZ_BLIT(nil, buf);   // hazard model: a tracked blit
    NVMTLBuffer *b = (NVMTLBuffer *)buf;
    if (!b) { nvlog("blit fillBuffer: nil buffer"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:b];
    if (nvmtl_vk_cmd_fill_buffer(&_cb->_c, &b->_b, range.location, range.length, value))
        nvlog("blit fillBuffer: FAILED");
}
- (void)copyFromTexture:(id<MTLTexture>)src sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl
           sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)size toTexture:(id<MTLTexture>)dst
       destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dof {
    if (nvmtl_y422_blit_refused(src, "blit copyFromTexture") || nvmtl_y422_blit_refused(dst, "blit copyFromTexture")) return;
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    NVMTLTexture *s = (NVMTLTexture *)src, *d = (NVMTLTexture *)dst;
    if (d && dl + d->_baseLevel > 0) (d->_parent ?: d)->_mipsWritten = YES;
    if (!s || !d) { nvlog("blit copyFromTexture: nil"); return; }
    if (sl >= s.mipmapLevelCount || dl >= d.mipmapLevelCount || ss >= nvmtl_texture_slice_count(s) || ds >= nvmtl_texture_slice_count(d)) {
        nvlog("blit copyFromTexture: subresource outside view"); return;
    }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s];
    [_cb->_resources addObject:d];
    // one side holds BGR10A2 as R10G10B10A2: stage through Metal's byte layout so the copy moves what Metal would move
    if (nvmtl_rb_root(s) != nvmtl_rb_root(d) && [s nvi]->mtl_type != 7 && [d nvi]->mtl_type != 7 && (size.depth ? size.depth : 1) == 1) {
        NVMTLTexture *tmp = nvmtl_rb_temp(_cb, s.device, size.width, size.height);
        if (tmp) {
            if (nvmtl_rb_root(s)) {
                if (nvmtl_vk_cmd_blit_convert(&_cb->_c, [s nvi], (uint32_t)sl + s->_baseLevel, (uint32_t)ss + s->_baseSlice, (uint32_t)so.x, (uint32_t)so.y,
                                              [tmp nvi], 0, 0, 0, 0, (uint32_t)size.width, (uint32_t)size.height)) nvlog("blit: BGR10A2 staging FAILED");
                [self copyFromTexture:tmp sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:size toTexture:d
                     destinationSlice:ds destinationLevel:dl destinationOrigin:dof];
            } else {
                [self copyFromTexture:s sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:size toTexture:tmp
                     destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
                if (nvmtl_vk_cmd_blit_convert(&_cb->_c, [tmp nvi], 0, 0, 0, 0, [d nvi], (uint32_t)dl + d->_baseLevel, (uint32_t)ds + d->_baseSlice,
                                              (uint32_t)dof.x, (uint32_t)dof.y, (uint32_t)size.width, (uint32_t)size.height)) nvlog("blit: BGR10A2 staging FAILED");
            }
            return;
        }
    }
    { int was = nvtrace_scope([d nvi]->w, [d nvi]->h);
      nvtrace("BLIT tex %ux%u surf %u (%lu,%lu %lux%lu) -> tex %ux%u surf %u (%lu,%lu)", [s nvi]->w, [s nvi]->h, nvmtl_surf_id(s), (unsigned long)so.x,
          (unsigned long)so.y, (unsigned long)size.width, (unsigned long)size.height, [d nvi]->w, [d nvi]->h, nvmtl_surf_id(d), (unsigned long)dof.x, (unsigned long)dof.y);
      g_trace_mute = was; }
    nvmtl_surface_in_for_cb(_cb, s);
    const BOOL overwrite = [s nvi] != [d nvi] && (!s->_surf || !d->_surf || IOSurfaceGetID(s->_surf) != IOSurfaceGetID(d->_surf))
        && nvmtl_surface_full_overwrite(d, (MTLRegion){dof, size}, dl, ds);
    if (!overwrite) nvmtl_surface_in_for_cb(_cb, d);
    nvmtl_surf_dirty(_cb, d);
    if (nvmtl_vk_cmd_copy_image_sub3(&_cb->_c, [s nvi], (uint32_t)sl + s->_baseLevel, (uint32_t)ss + s->_baseSlice, (uint32_t)so.x, (uint32_t)so.y, (uint32_t)so.z,
                                [d nvi], (uint32_t)dl + d->_baseLevel, (uint32_t)ds + d->_baseSlice, (uint32_t)dof.x, (uint32_t)dof.y, (uint32_t)dof.z,
                                (uint32_t)size.width, (uint32_t)size.height, (uint32_t)(size.depth ? size.depth : 1)))
        nvlog("blit copyFromTexture: FAILED");
    else if (overwrite) nvmtl_surface_overwrite_recorded(_cb, d);
}
- (void)sampleCountersInBuffer:(id)sb atSampleIndex:(NSUInteger)i withBarrier:(BOOL)b { nvmtl_sample_counter(_cb, sb, i, self, b); }
- (void)resolveCounters:(id)sb inRange:(NSRange)r destinationBuffer:(id<MTLBuffer>)dst destinationOffset:(NSUInteger)off {
    NVMTLBuffer *b = (NVMTLBuffer *)dst;
    if (!b || !r.length) { nvlog("resolveCounters: no destination buffer or empty range"); nvmtl_counter_recording_failed(_cb); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:b];
    NVMTLCounterSampleBuffer *s = [sb isKindOfClass:[NVMTLCounterSampleBuffer class]] ? sb : nil;
    if (!s || r.location > s->_count || r.length > s->_count - r.location) {
        nvlog("resolveCounters: %s, range %lu+%lu of %lu samples - not resolved", s ? "range outside the buffer" : "not a sample buffer this device made",
              (unsigned long)r.location, (unsigned long)r.length, s ? (unsigned long)s->_count : 0ul);
        nvmtl_counter_recording_failed(_cb); return;
    }
    [_cb->_resources addObject:s];
    if (nvmtl_vk_cmd_ts_resolve(&_cb->_c, s->_pool, (uint32_t)r.location, (uint32_t)r.length, &b->_b, off))
        nvmtl_counter_recording_failed(_cb);
}
- (void)generateMipmapsForTexture:(id<MTLTexture>)tex {
    if (nvmtl_y422_blit_refused(tex, "blit generateMipmaps")) return;
    NVMTL_HZ_BLIT(nil, tex);   // hazard model: a tracked blit
    NVMTLTexture *t = (NVMTLTexture *)tex;
    if (!t) { nvlog("generateMipmapsForTexture: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:t];
    { int was = nvtrace_scope([t nvi]->w, [t nvi]->h); nvtrace("MIPS tex %ux%u surf %u", [t nvi]->w, [t nvi]->h, nvmtl_surf_id(t)); g_trace_mute = was; }
    (t->_parent ?: t)->_mipsWritten = YES;
    nvmtl_surface_in_for_cb(_cb, t); nvmtl_surf_dirty(_cb, t);
    if (nvmtl_vk_cmd_image_gen_mipmaps(&_cb->_c, [t nvi])) nvlog("generateMipmapsForTexture: FAILED");
}
- (void)copyFromTexture:(id<MTLTexture>)src sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl toTexture:(id<MTLTexture>)dst
       destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl sliceCount:(NSUInteger)nslices levelCount:(NSUInteger)nlevels {
    if (nvmtl_y422_blit_refused(src, "blit copyFromTexture") || nvmtl_y422_blit_refused(dst, "blit copyFromTexture")) return;
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    NVMTLTexture *s = (NVMTLTexture *)src, *d = (NVMTLTexture *)dst;
    if (!s || !d) { nvlog("blit copyFromTexture:sliceCount: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s]; [_cb->_resources addObject:d];
    for (NSUInteger k = 0; k < (nslices ? nslices : 1); k++)
        for (NSUInteger l = 0; l < (nlevels ? nlevels : 1); l++) {
            if (sl + l >= s.mipmapLevelCount || dl + l >= d.mipmapLevelCount) { nvlog("blit sliceCount: mip outside view"); return; }
            NSUInteger w = MAX((NSUInteger)1, s.width >> (sl + l)), h = MAX((NSUInteger)1, s.height >> (sl + l));
            NSUInteger dp = s.textureType == MTLTextureType3D ? MAX((NSUInteger)1, s.depth >> (sl + l)) : 1;
            [self copyFromTexture:s sourceSlice:ss + k sourceLevel:sl + l sourceOrigin:MTLOriginMake(0,0,0)
                      sourceSize:MTLSizeMake(w,h,dp) toTexture:d destinationSlice:ds + k destinationLevel:dl + l destinationOrigin:MTLOriginMake(0,0,0)];
        }
}
- (void)optimizeContentsForGPUAccess:(id<MTLTexture>)t {}
- (void)optimizeContentsForGPUAccess:(id<MTLTexture>)t slice:(NSUInteger)s level:(NSUInteger)l {}
- (void)optimizeContentsForCPUAccess:(id<MTLTexture>)t {}
- (void)optimizeContentsForCPUAccess:(id<MTLTexture>)t slice:(NSUInteger)s level:(NSUInteger)l {}
- (void)resetCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r {
    if ([(id)icb isKindOfClass:[NVMTLIndirectCommandBuffer class]]) [(NVMTLIndirectCommandBuffer *)icb resetWithRange:r];
    else nvlog("blit resetCommandsInBuffer: a %s is not our indirect command buffer — ignored", object_getClassName(icb));
}
- (void)copyIndirectCommandBuffer:(id<MTLIndirectCommandBuffer>)src sourceRange:(NSRange)r destination:(id<MTLIndirectCommandBuffer>)dst destinationIndex:(NSUInteger)di {
    if (![(id)src isKindOfClass:[NVMTLIndirectCommandBuffer class]] || ![(id)dst isKindOfClass:[NVMTLIndirectCommandBuffer class]]) {
        nvlog("blit copyIndirectCommandBuffer: not our indirect command buffers — ignored"); return; }
    NVMTLIndirectCommandBuffer *s = (NVMTLIndirectCommandBuffer *)src, *d = (NVMTLIndirectCommandBuffer *)dst;
    if (r.location + r.length > s->_cmds.count || di + r.length > d->_cmds.count) {
        nvlog("blit copyIndirectCommandBuffer: range [%lu,+%lu) -> %lu is outside the buffers (%lu / %lu slots) — ignored", (unsigned long)r.location,
              (unsigned long)r.length, (unsigned long)di, (unsigned long)s->_cmds.count, (unsigned long)d->_cmds.count); return; }
    for (NSUInteger k = 0; k < r.length; k++) [d->_cmds[di + k] nvmtlCopyFrom:s->_cmds[r.location + k]];
}
- (void)optimizeIndirectCommandBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r {}
- (void)copyFromTexture:(id<MTLTexture>)src toTexture:(id<MTLTexture>)dst {
    if (nvmtl_y422_blit_refused(src, "blit copyFromTexture") || nvmtl_y422_blit_refused(dst, "blit copyFromTexture")) return;
    NVMTLTexture *s = (NVMTLTexture *)src;
    if (!s) { nvlog("blit copyFromTexture:toTexture: nil"); return; }
    [self copyFromTexture:src sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0)
               sourceSize:MTLSizeMake(s.width, s.height, s.textureType == MTLTextureType3D ? s.depth : 1) toTexture:dst
         destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
}
// copyFromBuffer:toTexture: into a 4:2:2 texture: a compute pass decodes the packed rows into a scratch RGBA8 buffer, which the
// ordinary copy then moves into the texture. Whole-width rows of level 0 only (a pair's chroma reaches its neighbours).
// The CPU copy of the packed bytes is not updated, so getBytes after such a copy is refused.
- (void)nvmtlY422FromBuffer:(id<MTLBuffer>)src offset:(NSUInteger)so row:(NSUInteger)row size:(MTLSize)size toTexture:(id<MTLTexture>)dst
                      slice:(NSUInteger)ds level:(NSUInteger)dl origin:(MTLOrigin)dof {
    NVMTLBuffer *b = (NVMTLBuffer *)src; NVMTLTexture *t = (NVMTLTexture *)dst;
    const uint32_t w = t->_i.w;
    if (!b || ds || dl || dof.x || dof.z || size.depth > 1 || size.width != w || !size.height || dof.y > t->_i.h || size.height > t->_i.h - dof.y) {
        nvlog("blit copyFromBuffer:toTexture: a 4:2:2 copy must be whole-width rows of level 0 - REFUSED"); return; }
    NVMTLBuffer *sc = nvmtl_icb_scratch_bytes((NSUInteger)w * size.height * 4u);
    if (!sc) { nvlog("blit copyFromBuffer:toTexture: 4:2:2 scratch FAILED"); return; }
    nvmtl_retain_resource(_cb, sc); nvmtl_retain_resource(_cb, b);
    if (nvmtl_vk_cmd_y422_convert(&_cb->_c, &b->_b, so, row, &sc->_b, w, (uint32_t)size.height, (uint32_t)t->_fmt)) {
        nvlog("blit copyFromBuffer:toTexture: the 4:2:2 conversion pass was refused (offset/pitch must be multiples of 4, pitch >= 2*width) - nothing copied"); return; }
    t->_y422GpuWritten = YES;
    g_y422_bypass = 1;
    [self copyFromBuffer:sc sourceOffset:0 sourceBytesPerRow:(NSUInteger)w * 4u sourceBytesPerImage:0 sourceSize:MTLSizeMake(w, size.height, 1)
               toTexture:dst destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, dof.y, 0)];
    g_y422_bypass = 0;
}
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)row
   sourceBytesPerImage:(NSUInteger)img sourceSize:(MTLSize)size toTexture:(id<MTLTexture>)dst
      destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dof {
    if (nvmtl_y422_blit_refused(dst, "blit copyFromBuffer:toTexture")) { [self nvmtlY422FromBuffer:src offset:so row:row size:size toTexture:dst slice:ds level:dl origin:dof]; return; }
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    NVMTLBuffer *b = (NVMTLBuffer *)src; NVMTLTexture *t = (NVMTLTexture *)dst;
    if (!b || !t) { nvlog("blit copyFromBuffer:toTexture: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:b];
    [_cb->_resources addObject:t];
    if (nvmtl_rb_root(t) && [t nvi]->mtl_type != 7 && size.depth == 1) {   // buffer bytes are Metal's BGR10A2 layout
        NVMTLTexture *tmp = nvmtl_rb_temp(_cb, t.device, size.width, size.height);
        if (tmp) {
            [self copyFromBuffer:src sourceOffset:so sourceBytesPerRow:row sourceBytesPerImage:img sourceSize:size toTexture:tmp
                destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
            if (nvmtl_vk_cmd_blit_convert(&_cb->_c, [tmp nvi], 0, 0, 0, 0, [t nvi], (uint32_t)dl + t->_baseLevel, (uint32_t)ds + t->_baseSlice,
                                          (uint32_t)dof.x, (uint32_t)dof.y, (uint32_t)size.width, (uint32_t)size.height)) nvlog("blit: BGR10A2 staging FAILED");
            return;
        }
    }
    { nvk_image *ti = [t nvi]; uint32_t tl = (uint32_t)nvmtl_texture_slice_count(t), tm = (uint32_t)t.mipmapLevelCount; BOOL vol = ti->mtl_type == 7;
      if (ds >= tl || dl >= tm || (vol ? (dof.z + size.depth > MAX((NSUInteger)1, t.depth >> dl)) : (size.depth != 1 || dof.z))) {
          nvlog("blit copyFromBuffer:toTexture: slice %lu/%u level %lu/%u depth %lu@z%lu — outside this texture", (unsigned long)ds, tl, (unsigned long)dl, tm, (unsigned long)size.depth, (unsigned long)dof.z); return; } }
    { int was = nvtrace_scope([t nvi]->w, [t nvi]->h);
      if (nvmtl_trace_on()) { unsigned nz = 0, nn = 0; size_t valid = (size_t)size.width * [t nvi]->bpp; if (row && valid > row) valid = row;
        if (b->_b.map && size.height && so + (size_t)(size.height - 1) * row + valid <= b->_b.size)
            nvmtl_nonzero((const char *)b->_b.map + so, row, valid, (uint32_t)size.height, &nz, &nn);
        nvtrace("BLIT buf len %lu off %lu row %lu (%lux%lu) -> tex %ux%u surf %u (%lu,%lu) | nonzero %u/%u sampled bytes", (unsigned long)b->_b.size, (unsigned long)so,
            (unsigned long)row, (unsigned long)size.width, (unsigned long)size.height, [t nvi]->w, [t nvi]->h, nvmtl_surf_id(t), (unsigned long)dof.x, (unsigned long)dof.y, nz, nn); }
      g_trace_mute = was; }
    const BOOL overwrite = nvmtl_surface_full_overwrite(t, (MTLRegion){dof, size}, dl, ds);
    if (!overwrite) nvmtl_surface_in_for_cb(_cb, t);
    nvmtl_surf_dirty(_cb, t);
    { BOOL vol = [t nvi]->mtl_type == 7; NSUInteger nz = vol ? (size.depth ? size.depth : 1) : 1; NSUInteger per = img ? img : row * size.height; int rc = 0;
      for (NSUInteger zi = 0; zi < nz && !rc; zi++)
          rc = nvmtl_vk_cmd_copy_buffer_to_image_level_layer(&_cb->_c, &b->_b, so + zi * per, (uint32_t)row, [t nvi],
                                      (uint32_t)dof.x, (uint32_t)dof.y, (uint32_t)size.width, (uint32_t)size.height, (uint32_t)dl + t->_baseLevel, (uint32_t)(vol ? dof.z + zi : ds + t->_baseSlice));
      if (rc) nvlog("blit copyFromBuffer:toTexture: FAILED (level %lu slice %lu; pf %lu %lux%lu mips %lu; buffer %lu B off %lu row %lu img %lu; region (%lu,%lu) %lux%lux%lu)",
                    (unsigned long)dl, (unsigned long)ds, (unsigned long)t.pixelFormat, (unsigned long)t.width, (unsigned long)t.height,
                    (unsigned long)t.mipmapLevelCount, (unsigned long)b->_b.size, (unsigned long)so, (unsigned long)row, (unsigned long)img,
                    (unsigned long)dof.x, (unsigned long)dof.y, (unsigned long)size.width, (unsigned long)size.height, (unsigned long)size.depth);
      else if (overwrite) nvmtl_surface_overwrite_recorded(_cb, t);
      { NVMTLTexture *r = t->_parent ?: t;
        if (dl + t->_baseLevel > 0) r->_mipsWritten = YES;
        else if (!rc && !vol && !t->_parent && !r->_mipsWritten && t.mipmapLevelCount > 1 && !dof.x && !dof.y && size.width == [t nvi]->w && size.height == [t nvi]->h && [t nvi]->bw <= 1
                 && nvmtl_vk_cmd_image_gen_mipmaps(&_cb->_c, [t nvi])) nvlog("blit copyFromBuffer:toTexture: mip generation FAILED"); } }
}
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)row
   sourceBytesPerImage:(NSUInteger)img sourceSize:(MTLSize)size toTexture:(id<MTLTexture>)dst
      destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dof
               options:(MTLBlitOption)opt {
    if (nvmtl_y422_blit_refused(dst, "blit copyFromBuffer:toTexture")) { [self nvmtlY422FromBuffer:src offset:so row:row size:size toTexture:dst slice:ds level:dl origin:dof]; return; }
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    const uint32_t asp = (opt & MTLBlitOptionDepthFromDepthStencil) ? 2u : (opt & MTLBlitOptionStencilFromDepthStencil) ? 4u : 0u;
    if (opt & ~(MTLBlitOptionDepthFromDepthStencil | MTLBlitOptionStencilFromDepthStencil))
        nvlog("blit copyFromBuffer:toTexture:options:%lu — only the depth/stencil options are implemented", (unsigned long)opt);
    nvmtl_vk_copy_aspect_override(asp, asp == 2u ? 4u : asp == 4u ? 1u : 0u);
    [self copyFromBuffer:src sourceOffset:so sourceBytesPerRow:row sourceBytesPerImage:img sourceSize:size
               toTexture:dst destinationSlice:ds destinationLevel:dl destinationOrigin:dof];
    nvmtl_vk_copy_aspect_override(0, 0);
}
- (void)copyFromTexture:(id<MTLTexture>)src sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl
           sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)size toBuffer:(id<MTLBuffer>)dst
      destinationOffset:(NSUInteger)dof destinationBytesPerRow:(NSUInteger)row
destinationBytesPerImage:(NSUInteger)img {
    if (nvmtl_y422_blit_refused(src, "blit copyFromTexture:toBuffer")) return;
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    NVMTLTexture *t = (NVMTLTexture *)src; NVMTLBuffer *b = (NVMTLBuffer *)dst;
    if (!t || !b) { nvlog("blit copyFromTexture:toBuffer: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:t];
    [_cb->_resources addObject:b];
    if (nvmtl_rb_root(t) && [t nvi]->mtl_type != 7 && size.depth == 1) {   // hand the buffer Metal's BGR10A2 layout
        NVMTLTexture *tmp = nvmtl_rb_temp(_cb, t.device, size.width, size.height);
        if (tmp) {
            if (nvmtl_vk_cmd_blit_convert(&_cb->_c, [t nvi], (uint32_t)sl + t->_baseLevel, (uint32_t)ss + t->_baseSlice, (uint32_t)so.x, (uint32_t)so.y,
                                          [tmp nvi], 0, 0, 0, 0, (uint32_t)size.width, (uint32_t)size.height)) nvlog("blit: BGR10A2 staging FAILED");
            [self copyFromTexture:tmp sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:size toBuffer:dst
                destinationOffset:dof destinationBytesPerRow:row destinationBytesPerImage:img];
            return;
        }
    }
    { nvk_image *ti = [t nvi]; uint32_t tl = (uint32_t)nvmtl_texture_slice_count(t), tm = (uint32_t)t.mipmapLevelCount; BOOL vol = ti->mtl_type == 7;
      if (ss >= tl || sl >= tm || (vol ? (so.z + size.depth > MAX((NSUInteger)1, t.depth >> sl)) : (size.depth != 1 || so.z))) {
          nvlog("blit copyFromTexture:toBuffer: slice %lu/%u level %lu/%u depth %lu@z%lu — outside this texture", (unsigned long)ss, tl, (unsigned long)sl, tm, (unsigned long)size.depth, (unsigned long)so.z); return; } }
    nvmtl_surface_in_for_cb(_cb, t);
    nvmtl_census("copy texture->buffer", [t nvi]->w, [t nvi]->h, t->_surf != NULL);
    { int was = nvtrace_scope([t nvi]->w, [t nvi]->h);
      nvtrace("BLIT tex %ux%u surf %u (%lu,%lu %lux%lu) -> buf len %lu off %lu row %lu", [t nvi]->w, [t nvi]->h, nvmtl_surf_id(t), (unsigned long)so.x, (unsigned long)so.y,
          (unsigned long)size.width, (unsigned long)size.height, (unsigned long)b->_b.size, (unsigned long)dof, (unsigned long)row);
      g_trace_mute = was; }
    { BOOL vol = [t nvi]->mtl_type == 7; NSUInteger nz = vol ? (size.depth ? size.depth : 1) : 1; NSUInteger per = img ? img : row * size.height; int rc = 0;
      for (NSUInteger zi = 0; zi < nz && !rc; zi++)
          rc = nvmtl_vk_cmd_copy_image_to_buffer_level_layer(&_cb->_c, [t nvi], (uint32_t)so.x, (uint32_t)so.y,
                                      (uint32_t)size.width, (uint32_t)size.height, &b->_b, dof + zi * per, (uint32_t)row, (uint32_t)sl + t->_baseLevel, (uint32_t)(vol ? so.z + zi : ss + t->_baseSlice));
      if (rc) nvlog("blit copyFromTexture:toBuffer: FAILED (level %lu slice %lu)", (unsigned long)sl, (unsigned long)ss); }
}
- (void)copyFromTexture:(id<MTLTexture>)src sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl
           sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)size toBuffer:(id<MTLBuffer>)dst
      destinationOffset:(NSUInteger)dof destinationBytesPerRow:(NSUInteger)row
destinationBytesPerImage:(NSUInteger)img options:(MTLBlitOption)opt {
    if (nvmtl_y422_blit_refused(src, "blit copyFromTexture:toBuffer")) return;
    NVMTL_HZ_BLIT(src, dst);   // hazard model: a tracked blit
    const uint32_t asp = (opt & MTLBlitOptionDepthFromDepthStencil) ? 2u : (opt & MTLBlitOptionStencilFromDepthStencil) ? 4u : 0u;
    if (opt & ~(MTLBlitOptionDepthFromDepthStencil | MTLBlitOptionStencilFromDepthStencil))
        nvlog("blit copyFromTexture:toBuffer:options:%lu — only the depth/stencil options are implemented", (unsigned long)opt);
    nvmtl_vk_copy_aspect_override(asp, asp == 2u ? 4u : asp == 4u ? 1u : 0u);
    [self copyFromTexture:src sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:size toBuffer:dst
        destinationOffset:dof destinationBytesPerRow:row destinationBytesPerImage:img];
    nvmtl_vk_copy_aspect_override(0, 0);
}
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f { nvmtl_vk_barrier_site(NVMTL_BARS_FENCE_API); nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)pushDebugGroup:(NSString *)g { nvtrace("GROUP %s", g.UTF8String ?: "?"); }
- (void)popDebugGroup {}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)synchronizeResource:(id<MTLResource>)r {
    if (!r) return;
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:r];
    if ([(id)r isKindOfClass:[NVMTLBuffer class]] && ((NVMTLBuffer *)r)->_shadow.map) {
        NVMTLBuffer *b = (NVMTLBuffer *)r;
        __atomic_store_n(&b->_zshadow, 0, __ATOMIC_RELEASE);
        if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &b->_b, 0, &b->_shadow, 0, b->_b.size))
            [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:@"Cannot encode managed buffer synchronization"}]];
    }
    if ([(id)r isKindOfClass:[NVMTLTexture class]]) {
        NVMTLTexture *t = (NVMTLTexture *)r;
        NVMTLBuffer *bb = t->_backBuf ? t->_backBuf : (t->_parent ? t->_parent->_backBuf : nil);
        if (bb && bb->_shadow.map) {
            [_cb->_resources addObject:bb];
            __atomic_store_n(&bb->_zshadow, 0, __ATOMIC_RELEASE);
            if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &bb->_b, 0, &bb->_shadow, 0, bb->_b.size))
                [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                    userInfo:@{NSLocalizedDescriptionKey:@"Cannot encode managed buffer-texture synchronization"}]];
        }
    }
}
- (void)synchronizeTexture:(id<MTLTexture>)t slice:(NSUInteger)s level:(NSUInteger)l { [self synchronizeResource:t]; }
// The encoder lets go of its command buffer when encoding ends, as Metal's own encoders do. An app's autorelease pool
// can keep an ended encoder alive for a long time (a benchmark loop that never drains), and a strong reference here kept
// every such command buffer, and every buffer it used, alive with it: VRAM filled until allocations were refused.
- (void)endEncoding { if (_cb) { _cb->_nEnd++; _cb = nil; } }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
- (id<MTLCommandBuffer>)commandBuffer { return (id<MTLCommandBuffer>)(_cb ? _cb : _owner); }   // SPI, see the render encoder; _cb is gone after endEncoding
@end

#pragma mark - command buffer / queue
#import "NVMTLEventSync.m"
@implementation NVMTLCommandBuffer
// Never a lock on the command buffer itself: apps may hold @synchronized(commandBuffer) around -waitUntilCompleted
// (Unity 6 does), and this runs on the execution queue that completes it. Vampire Survivors hung on its first frame
// (1.14.2-1.14.4, sample from a user: the execution queue in objc_sync_enter here, Unity's thread in waitUntilCompleted).
- (void)nvmtlReleaseCapacity {
    if (__atomic_exchange_n(&_capacityHeld, NO, __ATOMIC_ACQ_REL)) dispatch_semaphore_signal(_queue->_capacity);
}
- (void)nvmtlRecordEncodingError:(NSError *)error { if (!_cbError) _cbError = error; }
- (void)setProtectionOptions:(unsigned long long)o { if (o) { static int said; if (!said++) nvlog("setProtectionOptions: 0x%llx asked - no protected-content path; protectionOptions stays 0 (said once)", o); } }
- (unsigned long long)globalTraceObjectID { return nvmtl_global_trace_id(self); }
- (BOOL)retainedReferences { return YES; }
- (MTLCommandBufferErrorOption)errorOptions { return MTLCommandBufferErrorOptionNone; }
- (id<MTLLogContainer>)logs { return nil; }
- (unsigned long long)protectionOptions { return 0; }
- (instancetype)init {
    if ((self = [super init])) {
        _completionCondition = [NSCondition new];
        // Made here, never lazily: two threads encoding into one command buffer could each find it nil and store
        // their own set, and the set that lost was freed with the resources only it held (and under the other
        // thread's addObject:).
        _resources = [NVMTLResSet new];
        _callbackGroup = dispatch_group_create(); _scheduledGroup = dispatch_group_create();
    }
    return self;
}
// as on the queue, but for this command buffer only: holding the set holds its allocations, and the
// resource set is released when the command buffer completes
- (void)useResidencySet:(id<MTLResidencySet>)r { nvmtl_retain_resource(self, r); }
- (void)useResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {
    if (!r) return;
    for (NSUInteger i = 0; i < n; i++) nvmtl_retain_resource(self, r[i]);
}
- (NSMutableDictionary *)userDictionary {
    @synchronized ([NVMTLCommandBuffer class]) { if (!_userDictionary) _userDictionary = [NSMutableDictionary new]; return _userDictionary; }
}
- (id<MTLParallelRenderCommandEncoder>)parallelRenderCommandEncoderWithDescriptor:(MTLRenderPassDescriptor *)d {
    nvmtl_g79_flush(self);
    NVMTLParallelRenderCommandEncoder *p = [NVMTLParallelRenderCommandEncoder new];
    _nEnc++;
    p->_cb = self; p->_desc = [d copy]; p->_subs = [NSMutableArray new]; p->_lock = [NSLock new];
    MTLSamplePosition copiedSample[8]; NSUInteger copiedCount = [d getSamplePositions:NULL count:0];
    if (copiedCount && copiedCount <= 8) [d getSamplePositions:copiedSample count:copiedCount];
    if (copiedCount <= 8) [p->_desc setSamplePositions:copiedSample count:copiedCount];

    nvlog("parallelRenderCommandEncoderWithDescriptor: -> %p (sub-encoders record, endEncoding replays into one pass)", (__bridge void *)p);
    return p;
}
- (id<MTLRenderCommandEncoder>)renderCommandEncoderWithDescriptor:(MTLRenderPassDescriptor *)d {
    nvmtl_g79_flush(self);
    NVMTLRenderCommandEncoder *e = [NVMTLRenderCommandEncoder new];
    _nEnc++;
    e->_cb = self; e->_target = (NVMTLTexture *)d.colorAttachments[0].texture;
    if (!_resources) _resources = [NVMTLResSet new];
    if (e->_target) [_resources addObject:e->_target];
    if (d.depthAttachment.texture) [_resources addObject:d.depthAttachment.texture];
    { MTLStoreAction sa = d.colorAttachments[0].storeAction;
      if ((sa == MTLStoreActionMultisampleResolve || sa == MTLStoreActionStoreAndMultisampleResolve || sa == MTLStoreActionUnknown) && d.colorAttachments[0].resolveTexture) {
          e->_resolve = (NVMTLTexture *)d.colorAttachments[0].resolveTexture; [_resources addObject:e->_resolve]; } }
    if (d.visibilityResultBuffer) [_resources addObject:d.visibilityResultBuffer];
    if (e->_target) { NVMTLTexture *sr = e->_target->_parent ? e->_target->_parent : e->_target;
        nvmtl_census("render target", [sr nvi]->w, [sr nvi]->h, sr->_surf != NULL);
        (void)nvtrace_scope([sr nvi]->w, [sr nvi]->h);
        nvtrace("PASS target %ux%u surf %u view %d load %lu store %lu clear (%.3g %.3g %.3g %.3g) depth %d", [sr nvi]->w, [sr nvi]->h,
            sr->_surf ? (unsigned)IOSurfaceGetID(sr->_surf) : 0u, e->_target->_parent != nil, (unsigned long)d.colorAttachments[0].loadAction,
            (unsigned long)d.colorAttachments[0].storeAction, d.colorAttachments[0].clearColor.red, d.colorAttachments[0].clearColor.green,
            d.colorAttachments[0].clearColor.blue, d.colorAttachments[0].clearColor.alpha, d.depthAttachment.texture != nil);
        if (sr->_surf) {
            if (d.colorAttachments[0].loadAction == MTLLoadActionClear) nvmtl_surface_overwrite_recorded(self, sr);
            else nvmtl_surface_in_for_cb(self, sr);
            nvmtl_surf_dirty(self, sr); } }
    const float defaultBlend[4] = { 0, 0, 0, 0 };
    nvmtl_vk_cmd_set_blend_color(&_c, defaultBlend);
    nvmtl_vk_cmd_set_line_width(&_c, 1.0f);
    (void)nvmtl_vk_cmd_set_polygon_mode(&_c, 0);
    // Metal encoder state starts fresh even when the command buffer also contains an earlier GL render pass.
    _c.restart_off = 0;
    nvmtl_vk_cmd_set_depth_bias(&_c, 0.0f, 0.0f, 0.0f);
    nvmtl_vk_cmd_set_provoking_last(&_c, 0);
    (void)nvmtl_vk_cmd_set_depth_clip(&_c, 1);
    e->_visBuf = (NVMTLBuffer *)d.visibilityResultBuffer;
    MTLClearColor cc = d.colorAttachments[0].clearColor;
    BOOL ca8 = d.colorAttachments[0].texture && d.colorAttachments[0].texture.pixelFormat == MTLPixelFormatA8Unorm;   // alpha lives in the red lane
    e->_clear[0] = ca8 ? (float)cc.alpha : (float)cc.red; e->_clear[1] = (float)cc.green; e->_clear[2] = (float)cc.blue; e->_clear[3] = (float)cc.alpha;
    if (![e nvmtlBuildPass:d]) e->_refused = YES;
    return e;
}
// Late frames reported to the integrated GPU's service (nvmtl_late_frames.h). Only when the GPU service says it wants them:
// the kext publishes "nullmoth-floor-percent" when boot-arg nmxintel_floor is set, and a Mac without it pays one registry
// lookup per process. A failed report switches the reporting off for the process.
static io_service_t nvmtl_late_service(void) {
    io_iterator_t it = 0; io_service_t found = 0, svc;
    if (IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("NMXGPU"), &it) != KERN_SUCCESS) return 0;
    while (!found && (svc = IOIteratorNext(it))) {
        CFTypeRef pct = IORegistryEntryCreateCFProperty(svc, CFSTR("nullmoth-floor-percent"), kCFAllocatorDefault, 0);
        if (pct) { CFRelease(pct); found = svc; } else IOObjectRelease(svc);
    }
    IOObjectRelease(it);
    return found;
}
static void nvmtl_late_present(void) {
    static int state = -1; static io_service_t svc; static struct nvmtl_late late; static os_unfair_lock lk = OS_UNFAIR_LOCK_INIT;
    if (state < 0) { os_unfair_lock_lock(&lk); if (state < 0) { svc = nvmtl_late_service(); state = svc ? 1 : 0; } os_unfair_lock_unlock(&lk); }
    if (state != 1) return;
    os_unfair_lock_lock(&lk);
    const uint32_t n = nvmtl_late_note(&late, clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW));
    os_unfair_lock_unlock(&lk);
    if (!n) return;
    CFNumberRef cn = CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &n);
    CFStringRef key = CFSTR("nullmoth-frame-late");
    CFDictionaryRef d = CFDictionaryCreate(kCFAllocatorDefault, (const void **)&key, (const void **)&cn, 1,
                                           &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    kern_return_t kr = d ? IORegistryEntrySetCFProperties(svc, d) : KERN_FAILURE;
    if (d) CFRelease(d);
    if (cn) CFRelease(cn);
    if (kr != KERN_SUCCESS) { state = 0; nvlog("late-frame report refused (0x%x): reporting is off for this process", kr); }
}
- (void)nvmtlRecordDrawable:(id<MTLDrawable>)drawable presentation:(dispatch_block_t)action {
    if (!drawable || _committed) return;
    nvmtl_late_present();
    {
        // Frame-rate limiter (NVMTL_FPS_LIMIT in the user's conf block; see nvmtl_fps_limit.h). The wait happens on the
        // application's own thread, as the Windows driver's maximum frame rate does, so its render loop slows down and
        // the GPU idles between frames. Resolved once per process: a process with no limit pays one load.
        static int fps = -1; static nvmtl_fps_pacer pacer; static os_unfair_lock pace_lock = OS_UNFAIR_LOCK_INIT;
        if (fps < 0) {
            fps = nvmtl_fps_limit_for(getenv("NVMTL_FPS_LIMIT"), getprogname());
            nvmtl_fps_pacer_init(&pacer, fps);
            if (fps) nvlog("frame-rate limit %d fps for %s", fps, getprogname());
        }
        if (fps) {
            os_unfair_lock_lock(&pace_lock);
            const uint64_t wait = nvmtl_fps_pace(&pacer, clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW));
            os_unfair_lock_unlock(&pace_lock);
            if (wait) { struct timespec ts = { (time_t)(wait / 1000000000ull), (long)(wait % 1000000000ull) }; nanosleep(&ts, NULL); }
        }
    }
    {
        extern void nvmtl_vk_submit_wait_stats(uint64_t *total_ns, uint64_t *waits);
        static int off = -1;
        if (off < 0) { const char *e = getenv("NVMTL_NO_FPS"); off = (NVMTL_RELEASE && !nvmtl_process_env_on()) ? 1 : (e && *e && *e != '0') ? 1 : 0; }
        if (!off) {
            static _Atomic uint64_t frames, t0, lastn, lastwns, lastwaits;
            const uint64_t n = atomic_fetch_add(&frames, 1) + 1;
            const uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
            uint64_t t = atomic_load(&t0);
            if (!t) atomic_store(&t0, now);
            else if (now - t >= 2000000000ull) {
                uint64_t wns = 0, waits = 0;
                nvmtl_vk_submit_wait_stats(&wns, &waits);
                const uint64_t dn = n - atomic_load(&lastn);
                const uint64_t dwns = wns - atomic_load(&lastwns);
                const uint64_t dwaits = waits - atomic_load(&lastwaits);
                const double secs = (double)(now - t) / 1e9;
                nvlog("fps: %.2f over %.1f s (%llu frames, %llu total); CPU blocked on the GPU %.1f%% of wall "
                      "across %llu submits (%.2f ms each, %.1f submits per frame)",
                      (double)dn / secs, secs, (unsigned long long)dn, (unsigned long long)n,
                      100.0 * (double)dwns / (double)(now - t), (unsigned long long)dwaits,
                      dwaits ? (double)dwns / (double)dwaits / 1e6 : 0.0,
                      dn ? (double)dwaits / (double)dn : 0.0);
                {
                    static FILE *ff; static long fbytes;
                    if (!ff) { char fp[512]; const char *td = getenv("TMPDIR"); snprintf(fp, sizeof fp, "%s/nvmtl-fps-%d.log", td && *td ? td : "/tmp", (int)getpid()); ff = fopen(fp, "a"); }
                    if (ff && fbytes < (1 << 20)) {
                        time_t tt = time(NULL); struct tm lt; localtime_r(&tt, &lt);
                        fbytes += fprintf(ff, "%02d:%02d:%02d fps %.2f frames %llu gpuwait %.1f%% submits/frame %.1f\n", lt.tm_hour, lt.tm_min, lt.tm_sec,
                                          (double)dn / secs, (unsigned long long)dn, 100.0 * (double)dwns / (double)(now - t), dn ? (double)dwaits / (double)dn : 0.0);
                        fflush(ff);
                    }
                }
                if (dn) {   // barrier census over the same window: which site asked for each full barrier (nvmtl_vk.c g_bars)
                    static uint64_t lastb[16]; uint64_t bnow[16]; double b[16]; nvmtl_vk_bar_counts(bnow);
                    for (int q = 0; q < 16; q++) b[q] = (double)(bnow[q] - lastb[q]) / dn;
                    const double full = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + b[7];
                    nvlog_every("bar census: per frame %.1f full barriers (first dispatch %.1f, later dispatch %.1f, encoder end %.1f, owed flush %.1f, "
                                "fence/api %.1f, hazard restart %.1f, copies/uploads %.1f, other %.1f), %.1f compute-to-compute, %.1f image transitions "
                                "(%.1f without a wait, %.1f not recorded), %.1f render passes",
                                full, b[1], b[2], b[3], b[4], b[5], b[6], b[0], b[7], b[8], b[9], b[10], b[11], b[12]);
                    memcpy(lastb, bnow, sizeof lastb);
                }
                if (nvmtl_hz_mode() && dn) {   // the hazard model's decisions over the same window (nvmtl_hazard.h)
                    static uint64_t lasth[32]; uint64_t h[32]; double d[32];
                    nvmtl_vk_hz_counts(h);
                    for (int q = 0; q < 32; q++) d[q] = (double)(h[q] - lasth[q]) / dn;
                    nvmtl_vk_hz_reasons_log((double)dn);   // top ten per context, one log line each
                    {   // the model's own CPU time per frame, by entry point (all encoding threads summed)
                        static uint64_t lastc[8]; uint64_t cn[8]; nvmtl_vk_hz_cpu(cn); double us[8], tot = 0;
                        for (int q = 0; q < 8; q++) { us[q] = (double)(cn[q] - lastc[q]) / 1000.0 / dn; tot += us[q]; }
                        nvlog_every("hazard model CPU: %.1f us per frame (draw checks %.1f, pass begins %.1f, dispatches %.1f, bindings %.1f, "
                              "useResource/heap %.1f, command wrappers %.1f, blits %.1f)", tot, us[0], us[1], us[2], us[3], us[4], us[5], us[6]);
                        memcpy(lastc, cn, sizeof lastc);
                    }
                    nvlog_every("hazard census (%s, heaps %s): per frame %.1f passes (%.1f no barrier, %.1f barrier, %.1f render pass), %.1f restarts "
                          "(%.1f storms), %.1f forced; %.1f first dispatches (%.1f no barrier), %.1f later-dispatch barriers; %.1f untracked-work "
                          "and %.1f end-of-buffer barriers; %.1f transitions without a wait, %.1f model clears; useHeap %.1f untracked / %.1f tracked "
                          "(%.1f read everything); %.1f blits (%.1f no barrier, %.1f barrier, %.1f pre-barriers without a wait); "
                          "%.1f untracked binding changes, %.1f keyless bindings, %.1f no-state (reasons: the 'hazard reason' lines); "
                          "later dispatches decided: %.1f compute-to-compute kept, %.1f skipped",
                          nvmtl_hz_mode() == 1 ? "acting" : "counting only", nvmtl_hz_heaps_all() ? "all" : "Metal",
                          d[0], d[1], d[2], d[3], d[4], d[18], d[19], d[5], d[6], d[7], d[8], d[9], d[10], d[11],
                          d[15], d[16], d[13], d[20], d[21], d[22], d[23], d[17], d[12], d[14], d[24], d[25]);
                    memcpy(lasth, h, sizeof lasth);
                }
                atomic_store(&t0, now); atomic_store(&lastn, n);
                atomic_store(&lastwns, wns); atomic_store(&lastwaits, waits);
            }
        }
    }
    if ([drawable respondsToSelector:@selector(texture)]) {
        _present = (NVMTLTexture *)[(id<CAMetalDrawable>)drawable texture];
        nvmtl_surf_dirty(self, _present);
    }
    if (!_presentations) _presentations = [NSMutableArray new];
    [_presentations addObject:[action copy]];
    _nPresent++;
}
- (void)presentDrawable:(id<MTLDrawable>)drawable {
    [self nvmtlRecordDrawable:drawable presentation:^{ [drawable present]; }];
}
- (void)presentDrawable:(id<MTLDrawable>)drawable atTime:(CFTimeInterval)t {
    [self nvmtlRecordDrawable:drawable presentation:^{ [drawable presentAtTime:t]; }];
}
- (void)presentDrawable:(id<MTLDrawable>)drawable afterMinimumDuration:(CFTimeInterval)d {
    [self nvmtlRecordDrawable:drawable presentation:^{ [drawable presentAfterMinimumDuration:d]; }];
}

- (void)pushDebugGroup:(NSString *)g { nvtrace("GROUP %s", g.UTF8String ?: "?"); }
- (void)popDebugGroup {}
- (void)encodeSignalEvent:(id<MTLEvent>)e value:(uint64_t)v { [self nvmtlRecordSignal:e value:v]; }
- (void)encodeWaitForEvent:(id<MTLEvent>)e value:(uint64_t)v { [self nvmtlRecordWait:e value:v]; }
// The display-pipe swap follows this call immediately in WindowServer. NVAccel
// cannot order that swap after an NVK fence, so wait for the pixels to finish
// before allowing the surface to become scanout. Other applications keep the
// normal submission-only contract.
- (BOOL)commitAndWaitUntilSubmitted {
    static int ws = -1;
    if (ws < 0) ws = (getprogname() && !strcmp(getprogname(), "WindowServer")) ? 1 : 0;
    [self commit];
    // nvmtl_ws_kernel_waits: the swap waits for this composite in NVAccel instead (nvmtl_ws_mark_pending at commit)
    if (ws && !nvmtl_ws_kernel_waits()) [self waitUntilCompleted]; else [self waitUntilScheduled];
    return self.status != MTLCommandBufferStatusError;
}
- (void)commit {
    if (nvmtl_cbt_on() && !_cbtCommit) _cbtCommit = nvmtl_cbt_now();
    [self nvmtlSubmitWithEvents];
    // NVMTL_DEBUG_WAIT_EACH=1: every command buffer is waited for and its completion traced, so a GPU hang is pinned to
    // the command buffer (and the debug groups traced before it) that never finished. A diagnostic: it serialises the app.
    static int each = -1; if (each < 0) { const char *e = getenv("NVMTL_DEBUG_WAIT_EACH"); each = e && e[0] == '1'; }
    if (each) { static _Atomic unsigned long n; const unsigned long k = ++n; nvtrace("CB commit #%lu %s", k, _label.UTF8String ?: "");
        [self waitUntilCompleted]; nvtrace("CB done #%lu status %ld", k, (long)self.status); }
}
- (id<MTLComputeCommandEncoder>)computeCommandEncoder {
    NVMTLComputeCommandEncoder *e = [NVMTLComputeCommandEncoder new]; e->_cb = self; _nEnc++; return e;
}
- (id<MTLComputeCommandEncoder>)computeCommandEncoderWithDispatchType:(MTLDispatchType)t {
    NVMTLComputeCommandEncoder *e = (NVMTLComputeCommandEncoder *)[self computeCommandEncoder];
    e->_e78Concurrent = t == MTLDispatchTypeConcurrent; return e; }
- (id<MTLComputeCommandEncoder>)computeCommandEncoderWithDescriptor:(MTLComputePassDescriptor *)d {
    return [self computeCommandEncoderWithDispatchType:d ? d.dispatchType : MTLDispatchTypeSerial]; }
- (id<MTLBlitCommandEncoder>)blitCommandEncoderWithDescriptor:(MTLBlitPassDescriptor *)d { return [self blitCommandEncoder]; }
- (id<MTLResourceStateCommandEncoder>)resourceStateCommandEncoder {
    if (!nvmtl_vk_sparse_textures_ok()) return nil;   // sparse mapping is the only thing this encoder does
    NVMTLResourceStateCommandEncoder *e = [NVMTLResourceStateCommandEncoder new]; e->_cb = self; _nEnc++; return e;
}
- (id<MTLResourceStateCommandEncoder>)resourceStateCommandEncoderWithDescriptor:(MTLResourceStatePassDescriptor *)d { return [self resourceStateCommandEncoder]; }
- (id<MTLBlitCommandEncoder>)blitCommandEncoder {
    nvmtl_g79_flush(self);
    NVMTLBlitCommandEncoder *e = [NVMTLBlitCommandEncoder new]; e->_cb = self; e->_owner = self; _nEnc++; return e;
}
- (id<MTLAccelerationStructureCommandEncoder>)accelerationStructureCommandEncoder {
    nvmtl_g79_flush(self);
    NVMTLAccelerationStructureCommandEncoder *e = [NVMTLAccelerationStructureCommandEncoder new]; e->_cb = self; e->_owner = self; _nEnc++; return e;
}
- (id<MTLAccelerationStructureCommandEncoder>)accelerationStructureCommandEncoderWithDescriptor:(MTLAccelerationStructurePassDescriptor *)d { return [self accelerationStructureCommandEncoder]; }
- (void)addCompletedHandler:(MTLCommandBufferHandler)h {
    if (!h) return;
    [_completionCondition lock];
    if (!_handlers) _handlers = [NSMutableArray new];
    [_handlers addObject:[h copy]]; _nHandlers++;
    [_completionCondition unlock];
}
- (void)addScheduledHandler:(MTLCommandBufferHandler)h {
    if (!h) return;
    [_completionCondition lock];
    if (!_scheduledHandlers) _scheduledHandlers = [NSMutableArray new];
    [_scheduledHandlers addObject:[h copy]];
    [_completionCondition unlock];
}
- (void)enqueue {
    @synchronized ([NVMTLCommandBuffer class]) {
        _nEnqueue++; [self nvmtlReserveQueue];
        [_completionCondition lock];
        if (_cbstatus == MTLCommandBufferStatusNotEnqueued) _cbstatus = MTLCommandBufferStatusEnqueued;
        [_completionCondition unlock];
    }
}
- (void)dealloc {
    nvmtl_res2_cb_done(self);
    if (!_committed && _c.cb) nvmtl_vk_cmd_abandon(&_c);
    [self nvmtlReleaseCapacity];
    @synchronized ([NVMTLCommandBuffer class]) {
        [self nvmtlReleaseReservation];
        nvmtl_pump_buffers();
    }
}
- (CFTimeInterval)GPUStartTime {
    [_completionCondition lock];
    CFTimeInterval value = _executionDone && !_cbError ? _gpuStart : 0;
    [_completionCondition unlock]; return value;
}
- (CFTimeInterval)GPUEndTime {
    [_completionCondition lock];
    CFTimeInterval value = _executionDone && !_cbError ? _gpuEnd : 0;
    [_completionCondition unlock]; return value;
}
- (CFTimeInterval)kernelStartTime { return _legacyKernelStart; }
- (CFTimeInterval)kernelEndTime { return _legacyKernelEnd; }
- (void)waitUntilCompleted {
    const uint64_t cbt0 = nvmtl_cbt_on() ? nvmtl_cbt_now() : 0;
    _nWait++; [_completionCondition lock];
    while (!_completionReady) [_completionCondition wait];
    [_completionCondition unlock];
    dispatch_group_wait(_callbackGroup, DISPATCH_TIME_FOREVER);
    if (cbt0) nvmtl_cbt_line("W %llu %llu %llu\n", (unsigned long long)_cbtSeq, (unsigned long long)cbt0, (unsigned long long)nvmtl_cbt_now());
}
- (void)waitUntilScheduled {
    _nWait++; [_completionCondition lock];
    while (!_wasScheduled && !_executionDone) [_completionCondition wait];
    [_completionCondition unlock];
    dispatch_group_wait(_scheduledGroup, DISPATCH_TIME_FOREVER);
}
- (MTLCommandBufferStatus)status { [_completionCondition lock]; MTLCommandBufferStatus s = _cbstatus; [_completionCondition unlock]; return s; }
- (NSError *)error { return _cbError; }
- (id<MTLCommandQueue>)commandQueue { return _queue; }
- (id<MTLDevice>)device { return _queue ? [_queue device] : nil; }
- (void)setLabel:(NSString *)l { _nLabel++; _label = [l copy]; }
- (NSString *)label { return _label; }
@end

@implementation NVMTLCommandQueue
- (NSUInteger)getGPUPriority { return 1; }
- (NSUInteger)getBackgroundGPUPriority { return 2; }
- (void)setSubmissionQueue:(id)q { static int said; if (!said++) nvlog("setSubmissionQueue: %s ignored - this backend has no submission queue (said once)", q ? "a queue" : "nil"); }
- (BOOL)setGPUPriority:(NSUInteger)priority {
    nvlog("queue: GPU priority hint %lu: a command queue cannot change its channel group once made; a game process is raised at device creation with NVMTL_GAME_GPU_PRIORITY=1 (nvmtl_game_priority.h)", (unsigned long)priority);
    return NO;
}
- (BOOL)setBackgroundGPUPriority:(NSUInteger)priority {
    nvlog("queue: background GPU priority hint %lu unavailable; retaining fixed backend priority", (unsigned long)priority);
    return NO;
}
// Residency is implicit in this backend, so these only have to hold the set: a set retains the
// allocations added to it, so the app may hand the queue its only reference and release its own.
// They are called from any thread, hence the lock.
- (void)addResidencySet:(id<MTLResidencySet>)r {
    if (!r) return;
    @synchronized (self) { if (!_resSets) _resSets = [NSMutableArray new]; [_resSets addObject:r]; }
}
- (void)addResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {
    if (!r) return;
    for (NSUInteger i = 0; i < n; i++) [self addResidencySet:r[i]];
}
- (void)removeResidencySet:(id<MTLResidencySet>)r {
    if (!r) return;
    @synchronized (self) { [_resSets removeObject:r]; }
}
- (void)removeResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {
    if (!r) return;
    for (NSUInteger i = 0; i < n; i++) [self removeResidencySet:r[i]];
}
- (void)dealloc { nvmtl_vk_queue_destroy(&_q); }
- (id<MTLCommandBuffer>)commandBuffer {
    dispatch_semaphore_t cap = nil;
    if (nvmtl_async_commit_on()) {
        @synchronized ([NVMTLCommandQueue class]) { if (!_capacity) _capacity = dispatch_semaphore_create(_capacityCount ? _capacityCount : 64); cap = _capacity; }
        dispatch_semaphore_wait(cap, DISPATCH_TIME_FOREVER);
    }
    NVMTLCommandBuffer *cb = [NVMTLCommandBuffer new];
    if (!cb) { if (cap) dispatch_semaphore_signal(cap); return nil; }
    cb->_queue = self; cb->_capacityHeld = cap != nil;
    if (nvmtl_cbt_on()) { static _Atomic uint64_t seq; cb->_cbtSeq = ++seq; cb->_cbtCreate = nvmtl_cbt_now(); }
    // The resource set exists from the start. Every encoder used to create it on first use
    // (`if (!cb->_resources) cb->_resources = [NVMTLResSet new]`), and encoders of one command buffer run on several
    // threads (parallel render encoders, CoreAnimation's async render queue): two threads both saw nil, the second
    // store released the set the first was adding to, and the app died in -[NVMTLResSet addObject:] (__rehash on a freed
    // member) or in its -dealloc - about 14 Macs from 1.15 to 2.0.1 (NotificationCenter, Music, mediaanalysisd, games).
    // The set itself takes a lock for every operation; only its creation was unguarded. The first-use checks now never fire.
    cb->_resources = [NVMTLResSet new];
    if (nvmtl_vk_cmd_begin(&_q, &cb->_c)) {
        // WindowServer asserts on this nil ("Failed to get a MTLCommandBuffer"); the cause is the "vk: command buffer
        // refused" line just above, and for a disabled backend the "disabling backend" line before it.
        nvlog("queue: command buffer begin FAILED in %s - the caller gets nil", getprogname());
        return nil;
    }
    return cb;
}
- (id<MTLCommandBuffer>)commandBufferWithUnretainedReferences { return [self commandBuffer]; }
- (id<MTLCommandBuffer>)commandBufferWithDescriptor:(MTLCommandBufferDescriptor *)d { return [self commandBuffer]; }
- (void)insertDebugCaptureBoundary {}
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { return _dev; }
- (id)completionQueue { return _completionQueue; }
- (void)setCompletionQueue:(id)q { _completionQueue = q; }
- (NSUInteger)maxCommandBufferCount { return nvmtl_async_commit_on() && _capacityCount ? _capacityCount : 64; }
@end

#pragma mark - residency + events (2026-09-21, honest no-op objects)
@implementation NVMTLResidencySet
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (uint64_t)allocatedSize { uint64_t s = 0; for (id<MTLAllocation> a in _allocs) s += [a allocatedSize]; return s; }
- (void)requestResidency {}
- (void)endResidency {}
- (void)addAllocation:(id<MTLAllocation>)a { if (a) [_allocs addObject:a]; }
- (void)addAllocations:(const id<MTLAllocation> __unsafe_unretained [])a count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) if (a[i]) [_allocs addObject:a[i]]; }
- (void)removeAllocation:(id<MTLAllocation>)a { if (a) [_allocs removeObject:a]; }
- (void)removeAllocations:(const id<MTLAllocation> __unsafe_unretained [])a count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) if (a[i]) [_allocs removeObject:a[i]]; }
- (void)removeAllAllocations { [_allocs removeAllObjects]; }
- (BOOL)containsAllocation:(id<MTLAllocation>)a { return a && [_allocs containsObject:a]; }
- (NSArray<id<MTLAllocation>> *)allAllocations { return [_allocs copy]; }
- (NSUInteger)allocationCount { return _allocs.count; }
- (void)commit {}
@end
// Sparse mapping is applied when the call is made, not at its place on the GPU timeline: a bind waits for the queue before it removes
// anything, so work already submitted is safe, and work encoded earlier in this same command buffer sees the new mapping.
@implementation NVMTLResourceStateCommandEncoder
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)endEncoding { if (_cb) { _cb->_nEnd++; _cb = nil; } }
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)updateFence:(id<MTLFence>)fence {}
- (void)waitForFence:(id<MTLFence>)fence {}
- (void)updateTextureMapping:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode region:(MTLRegion)region mipLevel:(NSUInteger)mip slice:(NSUInteger)slice {
    nvmtl_sparse_update(texture, mode, region, mip, slice);
}
- (void)updateTextureMappings:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode regions:(const MTLRegion [])regions mipLevels:(const NSUInteger [])mips
                       slices:(const NSUInteger [])slices numRegions:(NSUInteger)n {
    for (NSUInteger i = 0; i < n; i++) nvmtl_sparse_update(texture, mode, regions[i], mips[i], slices[i]);
}
// MTLMapIndirectBufferFormat: a count, then that many MTLMapIndirectArguments. Read when this is encoded, so it only works for a
// buffer the CPU wrote (a GPU-written list is not carried).
- (void)updateTextureMapping:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode indirectBuffer:(id<MTLBuffer>)buffer indirectBufferOffset:(NSUInteger)off {
    const uint8_t *p = (const uint8_t *)buffer.contents;
    if (!p || off + 4 > buffer.length) { nvlog("updateTextureMapping:indirectBuffer: the buffer is not CPU-readable - REFUSED"); return; }
    uint32_t n; memcpy(&n, p + off, 4);
    if ((uint64_t)n * sizeof(MTLMapIndirectArguments) + 4 > buffer.length - off) { nvlog("updateTextureMapping:indirectBuffer: %u mappings do not fit the buffer - REFUSED", n); return; }
    for (uint32_t i = 0; i < n; i++) {
        MTLMapIndirectArguments a; memcpy(&a, p + off + 4 + (size_t)i * sizeof a, sizeof a);
        nvmtl_sparse_update(texture, mode, MTLRegionMake3D(a.regionOriginX, a.regionOriginY, a.regionOriginZ, a.regionSizeWidth, a.regionSizeHeight, a.regionSizeDepth), a.mipMapLevel, a.sliceId);
    }
}
@end
@implementation NVMTLEvent
- (instancetype)init { if ((self = [super init])) _condition = [NSCondition new]; return self; }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (uint64_t)signaledValue { return nvmtl_event_read(_condition, &_value); }
- (void)setSignaledValue:(uint64_t)v { nvmtl_event_signal(self, _condition, &_value, nil, v); }
@end
@implementation NVMTLSharedEvent
- (instancetype)init { if ((self = [super init])) { _condition = [NSCondition new]; _notifications = [NSMutableArray new]; } return self; }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (uint64_t)signaledValue { return nvmtl_event_read(_condition, &_value); }
- (void)setSignaledValue:(uint64_t)v { nvmtl_event_signal(self, _condition, &_value, _notifications, v); }
- (MTLSharedEventHandle *)newSharedEventHandle {
    static int said; if (!said++) nvlog("newSharedEventHandle: -> nil (cross-process event sharing is not built; this event lives in pid %d only)", getpid());
    return nil; }
- (void)notifyListener:(MTLSharedEventListener *)l atValue:(uint64_t)v block:(MTLSharedEventNotificationBlock)b {
    nvmtl_event_notify(self, _condition, &_value, _notifications, l, v, b);
}
- (BOOL)waitUntilSignaledValue:(uint64_t)v timeoutMS:(uint64_t)ms {
    return nvmtl_event_wait(_condition, &_value, v, ms);
}
@end

typedef enum : uint8_t {
    NVP_INV = 0, NVP_PIPE, NVP_DS, NVP_VBUF, NVP_FBUF, NVP_VTEX, NVP_FTEX, NVP_VSAMP, NVP_FSAMP,
    NVP_CULL, NVP_WIND, NVP_FILL, NVP_SREF, NVP_SREF2, NVP_VIEWPORT, NVP_SCISSOR, NVP_DEPTHBIAS, NVP_BLENDCOL,
    NVP_DRAWIDX, NVP_DRAWIDX5, NVP_DRAW, NVP_DRAW3, NVP_USERES, NVP_USERES3, NVP_VBYTES, NVP_FBYTES,
    NVP_VOFF, NVP_FOFF, NVP_DEPTHCLIP, NVP_VBUFST, NVP_VOFFST, NVP_USEHEAP, NVP_USEHEAP2
} nvp_kind;
typedef struct {
    nvp_kind kind;
    uint32_t i0, i1, i2;
    uint64_t u0, u1, u2, u3, u4, u5;
    double   f0, f1, f2, f3, f4, f5;
    void    *o0;
    const void *p0;
} nvp_cmd;
// _heaps: heaps this sub-encoder declared. Sub-encoders record on their own threads, so they are
// collected here under the sub's own lock and handed to the real encoder during the single-threaded
// replay in -endEncoding, which is what puts them on the command buffer (nvmtl_retain_heaps).
@interface NVMTLDeferredRenderCommandEncoder : NSProxy { @public __weak NVMTLParallelRenderCommandEncoder *_owner; NSMutableArray *_inv, *_keep, *_heaps; BOOL _ended; NSString *_label; nvp_cmd *_cmds; uint32_t _ncmd, _capcmd; }
@end
// Method signatures for the sub-encoder, by selector. The translated game asks the proxy for a signature before
// messaging it, and +instanceMethodSignatureForSelector: walks the class's protocol lists each time (class_copyProtocolList,
// a malloc, a hash probe): ~320 render-thread samples in 20 s. A signature never changes for a selector, so the first
// answer is kept. Reads take no lock: a slot's signature is published before its selector. NVMTL_PAR_SIGCACHE=0 turns it off.
#define NVP_SIG_SLOTS 512u
static struct { _Atomic(SEL) sel; void *sig; } nvp_sig_tab[NVP_SIG_SLOTS];
static os_unfair_lock nvp_sig_lock = OS_UNFAIR_LOCK_INIT;
static int nvp_sig_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_PAR_SIGCACHE"); on = !(e && e[0] == '0'); }
    return on;
}
static inline unsigned nvp_sig_slot(SEL s) { return (unsigned)(((uintptr_t)(const void *)s >> 3) * 0x9E3779B1u) & (NVP_SIG_SLOTS - 1); }
static NSMethodSignature *nvp_sig_cached(SEL s) {
    if (!s || !nvp_sig_on()) return nil;
    for (unsigned k = 0, i = nvp_sig_slot(s); k < NVP_SIG_SLOTS; k++, i = (i + 1) & (NVP_SIG_SLOTS - 1)) {
        SEL t = atomic_load_explicit(&nvp_sig_tab[i].sel, memory_order_acquire);
        if (t == s) return (__bridge NSMethodSignature *)nvp_sig_tab[i].sig;
        if (!t) return nil;
    }
    return nil;
}
static void nvp_sig_remember(SEL s, NSMethodSignature *m) {
    if (!s || !m || !nvp_sig_on()) return;
    os_unfair_lock_lock(&nvp_sig_lock);
    for (unsigned k = 0, i = nvp_sig_slot(s); k < NVP_SIG_SLOTS * 3 / 4; k++, i = (i + 1) & (NVP_SIG_SLOTS - 1)) {
        SEL t = atomic_load_explicit(&nvp_sig_tab[i].sel, memory_order_relaxed);
        if (t == s) break;
        if (!t) { nvp_sig_tab[i].sig = (__bridge_retained void *)m;   // held for the life of the process
                  atomic_store_explicit(&nvp_sig_tab[i].sel, s, memory_order_release); break; }
    }
    os_unfair_lock_unlock(&nvp_sig_lock);
}
@implementation NVMTLDeferredRenderCommandEncoder
static int nvp_fast_off(void) {
    static int off = -1;
    if (off < 0) { const char *e = getenv("NVMTL_NO_PARFAST"); off = (e && *e && *e != '0') ? 1 : 0; }
    return off;
}
- (nvp_cmd *)nvpPush:(nvp_kind)k {
    if (_ncmd == _capcmd) {
        uint32_t cap = _capcmd ? _capcmd * 2 : 256;
        nvp_cmd *p = (nvp_cmd *)realloc(_cmds, (size_t)cap * sizeof(nvp_cmd));
        if (!p) return NULL;
        _cmds = p; _capcmd = cap;
    }
    nvp_cmd *c = &_cmds[_ncmd++];
    memset(c, 0, sizeof *c);
    c->kind = k;
    return c;
}
- (void)nvpKeep:(id)o {
    if (!o) return;
    if (!_keep) _keep = [NSMutableArray new];
    [_keep addObject:o];
}
#define NVP_BEGIN(K) if (nvp_fast_off()) { [(id)self nvpForwardSlow:_cmd]; return; } \
                     @synchronized (self) { nvp_cmd *c = [self nvpPush:(K)]; if (!c) return;
#define NVP_END      }
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)p {
    if (nvp_fast_off()) { [self nvpSlowObj:@selector(setRenderPipelineState:) obj:p]; return; }
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_PIPE]; if (!c) return; [self nvpKeep:p]; c->o0 = (__bridge void *)p; }
}
- (void)setDepthStencilState:(id<MTLDepthStencilState>)d {
    if (nvp_fast_off()) { [self nvpSlowObj:@selector(setDepthStencilState:) obj:d]; return; }
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DS]; if (!c) return; [self nvpKeep:d]; c->o0 = (__bridge void *)d; }
}
- (void)nvpBind:(nvp_kind)k obj:(id)o u0:(uint64_t)u0 idx:(uint64_t)i {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:k]; if (!c) return; [self nvpKeep:o]; c->o0 = (__bridge void *)o; c->u0 = u0; c->u1 = i; }
}
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i { [self nvpBind:NVP_VBUF obj:b u0:o idx:i]; }
- (void)setFragmentBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i { [self nvpBind:NVP_FBUF obj:b u0:o idx:i]; }
- (void)setVertexBufferOffset:(NSUInteger)o atIndex:(NSUInteger)i { [self nvpBind:NVP_VOFF obj:nil u0:o idx:i]; }
// The strided and heap forms used to fall through to -forwardInvocation:, which costs a method-signature lookup, an
// NSInvocation, retainArguments, and an -invokeWithTarget: at replay - for a call the game makes per draw. Measured on
// Cyberpunk: ~1,100 render-thread samples in 20 s between recording and replay. NVMTL_NO_PARFAST=1 restores the old path.
- (void)nvpStrided:(nvp_kind)k obj:(id)o u0:(uint64_t)u0 stride:(uint64_t)st idx:(uint64_t)i {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:k]; if (!c) return; [self nvpKeep:o]; c->o0 = (__bridge void *)o; c->u0 = u0; c->u1 = i; c->u2 = st; }
}
- (void)setVertexBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (nvp_fast_off()) {
        SEL sel = @selector(setVertexBuffer:offset:attributeStride:atIndex:);
        NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:sel]; if (!m) return;
        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:m]; inv.selector = sel;
        [inv setArgument:&b atIndex:2]; [inv setArgument:&o atIndex:3]; [inv setArgument:&st atIndex:4]; [inv setArgument:&i atIndex:5];
        [self forwardInvocation:inv]; return;
    }
    [self nvpStrided:NVP_VBUFST obj:b u0:o stride:st idx:i];
}
- (void)setVertexBufferOffset:(NSUInteger)o attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (nvp_fast_off()) {
        SEL sel = @selector(setVertexBufferOffset:attributeStride:atIndex:);
        NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:sel]; if (!m) return;
        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:m]; inv.selector = sel;
        [inv setArgument:&o atIndex:2]; [inv setArgument:&st atIndex:3]; [inv setArgument:&i atIndex:4];
        [self forwardInvocation:inv]; return;
    }
    [self nvpStrided:NVP_VOFFST obj:nil u0:o stride:st idx:i];
}
- (void)setFragmentBufferOffset:(NSUInteger)o atIndex:(NSUInteger)i { [self nvpBind:NVP_FOFF obj:nil u0:o idx:i]; }
- (void)setVertexTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i { [self nvpBind:NVP_VTEX obj:t u0:0 idx:i]; }
- (void)setFragmentTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i { [self nvpBind:NVP_FTEX obj:t u0:0 idx:i]; }
- (void)setVertexSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i { [self nvpBind:NVP_VSAMP obj:s u0:0 idx:i]; }
- (void)setFragmentSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i { [self nvpBind:NVP_FSAMP obj:s u0:0 idx:i]; }
- (void)nvpScalar:(nvp_kind)k u0:(uint64_t)a u1:(uint64_t)b {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:k]; if (!c) return; c->u0 = a; c->u1 = b; }
}
- (void)setCullMode:(MTLCullMode)m { [self nvpScalar:NVP_CULL u0:(uint64_t)m u1:0]; }
- (void)setFrontFacingWinding:(MTLWinding)w { [self nvpScalar:NVP_WIND u0:(uint64_t)w u1:0]; }
- (void)setTriangleFillMode:(MTLTriangleFillMode)f { [self nvpScalar:NVP_FILL u0:(uint64_t)f u1:0]; }
- (void)setDepthClipMode:(MTLDepthClipMode)m { [self nvpScalar:NVP_DEPTHCLIP u0:(uint64_t)m u1:0]; }
- (void)setStencilReferenceValue:(uint32_t)r { [self nvpScalar:NVP_SREF u0:r u1:0]; }
- (void)setStencilFrontReferenceValue:(uint32_t)f backReferenceValue:(uint32_t)b { [self nvpScalar:NVP_SREF2 u0:f u1:b]; }
- (void)setViewport:(MTLViewport)v {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_VIEWPORT]; if (!c) return;
        c->f0 = v.originX; c->f1 = v.originY; c->f2 = v.width; c->f3 = v.height; c->f4 = v.znear; c->f5 = v.zfar; }
}
- (void)setScissorRect:(MTLScissorRect)r {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_SCISSOR]; if (!c) return;
        c->u0 = r.x; c->u1 = r.y; c->u2 = r.width; c->u3 = r.height; }
}
- (void)setDepthBias:(float)b slopeScale:(float)s clamp:(float)cl {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DEPTHBIAS]; if (!c) return; c->f0 = b; c->f1 = s; c->f2 = cl; }
}
- (void)setBlendColorRed:(float)r green:(float)g blue:(float)b alpha:(float)a {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_BLENDCOL]; if (!c) return; c->f0 = r; c->f1 = g; c->f2 = b; c->f3 = a; }
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)n indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)ibo instanceCount:(NSUInteger)ic
                   baseVertex:(NSInteger)bv baseInstance:(NSUInteger)bi {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DRAWIDX]; if (!c) return; [self nvpKeep:ib];
        c->i0 = (uint32_t)t; c->i1 = (uint32_t)it; c->o0 = (__bridge void *)ib;
        c->u0 = n; c->u1 = ibo; c->u2 = ic; c->u3 = (uint64_t)bv; c->u4 = bi; }
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)n indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)ibo {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DRAWIDX5]; if (!c) return; [self nvpKeep:ib];
        c->i0 = (uint32_t)t; c->i1 = (uint32_t)it; c->o0 = (__bridge void *)ib; c->u0 = n; c->u1 = ibo; }
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)n
         instanceCount:(NSUInteger)ic baseInstance:(NSUInteger)bi {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DRAW]; if (!c) return;
        c->i0 = (uint32_t)t; c->u0 = s; c->u1 = n; c->u2 = ic; c->u3 = bi; }
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)n {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_DRAW3]; if (!c) return; c->i0 = (uint32_t)t; c->u0 = s; c->u1 = n; }
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_USERES]; if (!c) return; [self nvpKeep:r]; c->o0 = (__bridge void *)r; c->u0 = u; }
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u stages:(MTLRenderStages)st {
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_USERES3]; if (!c) return; [self nvpKeep:r]; c->o0 = (__bridge void *)r; c->u0 = u; c->u1 = st; }
}
- (void)nvmtlRecordBytes:(SEL)sel bytes:(const void *)b length:(NSUInteger)l index:(NSUInteger)i {
    NSData *d = [NSData dataWithBytes:b length:l];
    @synchronized (self) {
        [self nvpKeep:d];
        nvp_cmd *c = [self nvpPush:(sel == @selector(setVertexBytes:length:atIndex:)) ? NVP_VBYTES : NVP_FBYTES];
        if (!c) return;
        c->p0 = d.bytes; c->u0 = l; c->u1 = i;
    }
}
- (void)setVertexBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlRecordBytes:@selector(setVertexBytes:length:atIndex:) bytes:b length:l index:i]; }
- (void)setFragmentBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlRecordBytes:@selector(setFragmentBytes:length:atIndex:) bytes:b length:l index:i]; }
- (void)setVertexBytes:(const void *)b length:(NSUInteger)l attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    SEL sel = @selector(setVertexBytes:length:attributeStride:atIndex:); NSData *d = [NSData dataWithBytes:b length:l];
    NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:sel]; if (!m) return;
    NSInvocation *inv = [NSInvocation invocationWithMethodSignature:m]; inv.selector = sel;
    const void *p = d.bytes; [inv setArgument:&p atIndex:2]; [inv setArgument:&l atIndex:3]; [inv setArgument:&st atIndex:4]; [inv setArgument:&i atIndex:5];
    @synchronized (self) { if (!_keep) _keep = [NSMutableArray new]; [_keep addObject:d]; [_inv addObject:inv]; }
}
- (NSMethodSignature *)methodSignatureForSelector:(SEL)s {
    NSMethodSignature *m = nvp_sig_cached(s);
    if (m) return m;
    m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:s];
    if (!m) { nvlog("parallel sub-encoder: %s is not a render-encoder selector — recorded as a no-op", sel_getName(s)); m = [NSMethodSignature signatureWithObjCTypes:"v@:"]; }
    nvp_sig_remember(s, m);
    return m;
}
- (void)forwardInvocation:(NSInvocation *)inv {
    if (![NVMTLRenderCommandEncoder instancesRespondToSelector:inv.selector]) return;
    [inv retainArguments];
    @synchronized (self) {
        [_inv addObject:inv];
        nvp_cmd *c = [self nvpPush:NVP_INV]; (void)c;
        if (inv.selector == @selector(endEncoding)) _ended = YES;
    }
    { static const char *seen[48]; static unsigned n; const char *s = sel_getName(inv.selector);
      unsigned k = 0; for (; k < n; k++) if (seen[k] == s) break;
      if (k == n && n < 48) { seen[n++] = s; nvlog("parallel sub-encoder: %s still goes through NSInvocation", s); } }
}
- (void)nvpSlowObj:(SEL)sel obj:(id)o {
    NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:sel]; if (!m) return;
    NSInvocation *inv = [NSInvocation invocationWithMethodSignature:m]; inv.selector = sel;
    [inv setArgument:&o atIndex:2];
    [self forwardInvocation:inv];
}
- (void)dealloc { free(_cmds); _cmds = NULL; }
- (BOOL)respondsToSelector:(SEL)s { return [NVMTLRenderCommandEncoder instancesRespondToSelector:s]; }
- (BOOL)conformsToProtocol:(Protocol *)p { return [NVMTLRenderCommandEncoder conformsToProtocol:p]; }
- (id<MTLDevice>)device { return gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (id<MTLCommandBuffer>)commandBuffer { NVMTLParallelRenderCommandEncoder *p = _owner; return p ? (id<MTLCommandBuffer>)p->_cb : nil; }   // SPI; answered here, not recorded for replay
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)setVertexBuffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)o withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) if (b[k]) [(id)self setVertexBuffer:b[k] offset:o ? o[k] : 0 atIndex:r.location + k]; }
- (void)setVertexBuffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)o attributeStrides:(const NSUInteger *)s withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) if (b[k]) [(id)self setVertexBuffer:b[k] offset:o ? o[k] : 0 attributeStride:s ? s[k] : MTLAttributeStrideStatic atIndex:r.location + k]; }
- (void)setFragmentBuffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)o withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) if (b[k]) [(id)self setFragmentBuffer:b[k] offset:o ? o[k] : 0 atIndex:r.location + k]; }
- (void)setVertexTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) if (t[k]) [(id)self setVertexTexture:t[k] atIndex:r.location + k]; }
- (void)setFragmentTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) if (t[k]) [(id)self setFragmentTexture:t[k] atIndex:r.location + k]; }
- (void)setVertexSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) [(id)self setVertexSamplerState:s[k] atIndex:r.location + k]; }
- (void)setFragmentSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s withRange:(NSRange)r { for (NSUInteger k = 0; k < r.length; k++) [(id)self setFragmentSamplerState:s[k] atIndex:r.location + k]; }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i = 0; i < n; i++) [(id)self useResource:r[i] usage:u]; }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u stages:(MTLRenderStages)st { for (NSUInteger i = 0; i < n; i++) [(id)self useResource:r[i] usage:u stages:st]; }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self useHeap:h[i]]; }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n stages:(MTLRenderStages)st { for (NSUInteger i = 0; i < n; i++) [self useHeap:h[i] stages:st]; }
- (void)useHeap:(id<MTLHeap>)h {
    if (nvp_fast_off()) { [self nvpSlowObj:@selector(useHeap:) obj:h]; return; }
    @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_USEHEAP]; if (!c) return; [self nvpKeep:h]; c->o0 = (__bridge void *)h; }
}
- (void)useHeap:(id<MTLHeap>)h stages:(MTLRenderStages)st {
    if (!nvp_fast_off()) {
        @synchronized (self) { nvp_cmd *c = [self nvpPush:NVP_USEHEAP2]; if (!c) return; [self nvpKeep:h]; c->o0 = (__bridge void *)h; c->u0 = st; }
        return;
    }
    SEL sel = @selector(useHeap:stages:);
    NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:sel];
    NSInvocation *inv = [NSInvocation invocationWithMethodSignature:m]; inv.selector = sel;
    [inv setArgument:&h atIndex:2]; [inv setArgument:&st atIndex:3];
    [self forwardInvocation:inv];
}
@end
@implementation NVMTLParallelRenderCommandEncoder
- (id<MTLRenderCommandEncoder>)renderCommandEncoder {
    NVMTLDeferredRenderCommandEncoder *s = [NVMTLDeferredRenderCommandEncoder alloc];
    s->_owner = self; s->_inv = [NSMutableArray new];
    [_lock lock]; [_subs addObject:s]; [_lock unlock];
    return (id<MTLRenderCommandEncoder>)s;
}
- (void)endEncoding {
    if (_ended) { nvlog("parallel encoder: endEncoding twice"); return; }
    _ended = YES;
    [_lock lock]; NSArray *subs = [_subs copy]; [_subs removeAllObjects]; [_lock unlock];
    NVMTLRenderCommandEncoder *real = (NVMTLRenderCommandEncoder *)[_cb renderCommandEncoderWithDescriptor:_desc];
    NSUInteger n = 0, dropped = 0;
    for (NVMTLDeferredRenderCommandEncoder *s in subs) {
        if (!s->_ended) dropped++;
        @synchronized (s) {
            // straight into the command buffer's resource set, which is what keeps a heap's memory alive
            // while this submission still reads resources placed inside it (-useHeap: itself is
            // deprecated, and this is the same set it would have added them to)
            for (id<MTLHeap> heap in s->_heaps) nvmtl_retain_resource(real->_cb, heap);
            NSUInteger invi = 0;
            @autoreleasepool {
                for (uint32_t ci = 0; ci < s->_ncmd; ci++) {
                    const nvp_cmd *c = &s->_cmds[ci];
                    switch (c->kind) {
                    case NVP_INV: {
                        if (invi >= s->_inv.count) break;
                        NSInvocation *inv = s->_inv[invi++];
                        if (inv.selector == @selector(endEncoding)) break;
                        [inv invokeWithTarget:real]; inv.target = nil; n++;
                        break; }
                    case NVP_PIPE:   [real setRenderPipelineState:(__bridge id)c->o0]; n++; break;
                    case NVP_DS:     [real setDepthStencilState:(__bridge id)c->o0]; n++; break;
                    case NVP_VBUF:   [real setVertexBuffer:(__bridge id)c->o0 offset:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_FBUF:   [real setFragmentBuffer:(__bridge id)c->o0 offset:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_VOFF:   [real setVertexBufferOffset:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_FOFF:   [real setFragmentBufferOffset:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_VBUFST: [real setVertexBuffer:(__bridge id)c->o0 offset:(NSUInteger)c->u0 attributeStride:(NSUInteger)c->u2 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_VOFFST: [real setVertexBufferOffset:(NSUInteger)c->u0 attributeStride:(NSUInteger)c->u2 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_USEHEAP:  nvmtl_use_heap(real->_cb, (__bridge id)c->o0); n++; break;   // what -[real useHeap:] does
                    case NVP_USEHEAP2: [real useHeap:(__bridge id)c->o0 stages:(MTLRenderStages)c->u0]; n++; break;
                    case NVP_VTEX:   [real setVertexTexture:(__bridge id)c->o0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_FTEX:   [real setFragmentTexture:(__bridge id)c->o0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_VSAMP:  [real setVertexSamplerState:(__bridge id)c->o0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_FSAMP:  [real setFragmentSamplerState:(__bridge id)c->o0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_CULL:   [real setCullMode:(MTLCullMode)c->u0]; n++; break;
                    case NVP_WIND:   [real setFrontFacingWinding:(MTLWinding)c->u0]; n++; break;
                    case NVP_FILL:   [real setTriangleFillMode:(MTLTriangleFillMode)c->u0]; n++; break;
                    case NVP_DEPTHCLIP: [real setDepthClipMode:(MTLDepthClipMode)c->u0]; n++; break;
                    case NVP_SREF:   [real setStencilReferenceValue:(uint32_t)c->u0]; n++; break;
                    case NVP_SREF2:  [real setStencilFrontReferenceValue:(uint32_t)c->u0 backReferenceValue:(uint32_t)c->u1]; n++; break;
                    case NVP_VIEWPORT: {
                        MTLViewport v = { c->f0, c->f1, c->f2, c->f3, c->f4, c->f5 };
                        [real setViewport:v]; n++; break; }
                    case NVP_SCISSOR: {
                        MTLScissorRect r = { (NSUInteger)c->u0, (NSUInteger)c->u1, (NSUInteger)c->u2, (NSUInteger)c->u3 };
                        [real setScissorRect:r]; n++; break; }
                    case NVP_DEPTHBIAS: [real setDepthBias:(float)c->f0 slopeScale:(float)c->f1 clamp:(float)c->f2]; n++; break;
                    case NVP_BLENDCOL:  [real setBlendColorRed:(float)c->f0 green:(float)c->f1 blue:(float)c->f2 alpha:(float)c->f3]; n++; break;
                    case NVP_DRAWIDX:
                        [real drawIndexedPrimitives:(MTLPrimitiveType)c->i0 indexCount:(NSUInteger)c->u0
                                          indexType:(MTLIndexType)c->i1 indexBuffer:(__bridge id)c->o0
                                  indexBufferOffset:(NSUInteger)c->u1 instanceCount:(NSUInteger)c->u2
                                         baseVertex:(NSInteger)c->u3 baseInstance:(NSUInteger)c->u4]; n++; break;
                    case NVP_DRAWIDX5:
                        [real drawIndexedPrimitives:(MTLPrimitiveType)c->i0 indexCount:(NSUInteger)c->u0
                                          indexType:(MTLIndexType)c->i1 indexBuffer:(__bridge id)c->o0
                                  indexBufferOffset:(NSUInteger)c->u1]; n++; break;
                    case NVP_DRAW:
                        [real drawPrimitives:(MTLPrimitiveType)c->i0 vertexStart:(NSUInteger)c->u0
                                 vertexCount:(NSUInteger)c->u1 instanceCount:(NSUInteger)c->u2
                                baseInstance:(NSUInteger)c->u3]; n++; break;
                    case NVP_DRAW3:
                        [real drawPrimitives:(MTLPrimitiveType)c->i0 vertexStart:(NSUInteger)c->u0
                                 vertexCount:(NSUInteger)c->u1]; n++; break;
                    case NVP_USERES:  [real useResource:(__bridge id)c->o0 usage:(MTLResourceUsage)c->u0]; n++; break;
                    case NVP_USERES3: [real useResource:(__bridge id)c->o0 usage:(MTLResourceUsage)c->u0 stages:(MTLRenderStages)c->u1]; n++; break;
                    case NVP_VBYTES:  [real setVertexBytes:c->p0 length:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    case NVP_FBYTES:  [real setFragmentBytes:c->p0 length:(NSUInteger)c->u0 atIndex:(NSUInteger)c->u1]; n++; break;
                    }
                    if ((ci & 0xff) == 0xff) {  }
                }
            }
            free(s->_cmds); s->_cmds = NULL; s->_ncmd = s->_capcmd = 0;
            [s->_inv removeAllObjects]; [s->_keep removeAllObjects]; s->_owner = nil;
        }
    }
    [real endEncoding];
    nvlog("parallel encoder %p: replayed %lu call(s) from %lu sub-encoder(s)%s", (__bridge void *)self, (unsigned long)n, (unsigned long)subs.count,
          dropped ? " (some sub-encoders were never ended)" : "");
}
- (id<MTLDevice>)device { return gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (id<MTLCommandBuffer>)commandBuffer { return (id<MTLCommandBuffer>)_cb; }   // SPI, see the render encoder
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)setColorStoreAction:(MTLStoreAction)a atIndex:(NSUInteger)i { _desc.colorAttachments[i].storeAction = a; }
- (void)setDepthStoreAction:(MTLStoreAction)a { _desc.depthAttachment.storeAction = a; }
- (void)setStencilStoreAction:(MTLStoreAction)a { _desc.stencilAttachment.storeAction = a; }
- (void)setColorStoreActionOptions:(MTLStoreActionOptions)o atIndex:(NSUInteger)i { _desc.colorAttachments[i].storeActionOptions = o; }
- (void)setDepthStoreActionOptions:(MTLStoreActionOptions)o { _desc.depthAttachment.storeActionOptions = o; }
- (void)setStencilStoreActionOptions:(MTLStoreActionOptions)o {
    if (o) [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain
        code:MTLCommandBufferErrorInternal userInfo:@{NSLocalizedDescriptionKey:@"stencil store options are illegal"}]];
    _desc.stencilAttachment.storeActionOptions = o;
}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)g { nvtrace("GROUP %s", g.UTF8String ?: "?"); }
- (void)popDebugGroup {}
@end

// A geometry we cannot build still has to OCCUPY ITS INDEX. Skipping it compacted the array, so
// every later geometry moved down one and its geometryIndex stopped matching the descriptor the game's
// hit records are written against - while the intersection-function-table arithmetic kept counting the
// uncompacted positions. An empty slot loses one mesh; a shifted index silently mis-binds every hit
// record after it. A zero-primitive geometry is legal and its vertex data is never read.
static void nvmtl_geom_make_empty(nvmtl_geom *o)
{
    // vertices too: the caller's array is _Thread_local and REUSED across builds, so relying on its
    // memset would leave the previous build's device address in an empty slot the day that memset goes.
    o->count = 0; o->is_boxes = 0; o->boxes = 0; o->box_stride = 0; o->indices = 0; o->vertices = 0;
    o->vertex_format = 106; o->stride = 12; o->index_type = 1000165000u; o->max_vertex = 0;
}

// MTLAccelerationStructureCurveGeometryDescriptor into the fields NVK reads. Properties are read by key so the plugin builds against
// SDKs and deployment targets that predate the class. Returns 0 when the geometry is carried; nonzero leaves it to the EMPTY path.
static int nvmtl_curve_from_descriptor(id g, nvmtl_geom *o, NSMutableSet *track)
{
    NVMTLBuffer *cpb = (NVMTLBuffer *)[g valueForKey:@"controlPointBuffer"], *rb = (NVMTLBuffer *)[g valueForKey:@"radiusBuffer"],
                *ib = (NVMTLBuffer *)[g valueForKey:@"indexBuffer"];
    if (!cpb || !rb || !ib) { nvlog("acceleration structure: curve geometry without a control point, radius or index buffer - built EMPTY"); return 1; }
    long seg = [[g valueForKey:@"segmentCount"] longValue], cnt = [[g valueForKey:@"segmentControlPointCount"] longValue];
    long basis = [[g valueForKey:@"curveBasis"] longValue], type = [[g valueForKey:@"curveType"] longValue], caps = [[g valueForKey:@"curveEndCaps"] longValue];
    long cpf = [[g valueForKey:@"controlPointFormat"] longValue], rf = [[g valueForKey:@"radiusFormat"] longValue], it = [[g valueForKey:@"indexType"] longValue];
    // control points: float3 or half3; radii: float or half; indices uint16 or uint32. Anything else is refused, never guessed.
    int cph = cpf == MTLAttributeFormatHalf3, rh = rf == MTLAttributeFormatHalf;
    if (!(cpf == MTLAttributeFormatFloat3 || cph) || !(rf == MTLAttributeFormatFloat || rh)) {
        nvlog("acceleration structure: curve control point format %ld / radius format %ld is not carried - built EMPTY", cpf, rf); return 1; }
    if (seg <= 0 || cnt < 2 || cnt > 4 || basis < 0 || basis > 3 || (basis == 2 && cnt != 2) || (basis == 1 && cnt != 4) || type < 0 || type > 1 || caps < 0 || caps > 2) {
        nvlog("acceleration structure: curve geometry with %ld segments of %ld control points, basis %ld - not carried, built EMPTY", seg, cnt, basis); return 1; }
    if (track) { [track addObject:cpb]; [track addObject:rb]; [track addObject:ib]; }
    long cps = [[g valueForKey:@"controlPointStride"] longValue], rs = [[g valueForKey:@"radiusStride"] longValue];
    o->is_boxes = 1; o->is_curve = 1; o->count = (uint32_t)seg; o->box_stride = 24;
    o->cp_stride = (uint32_t)(cps ? cps : (cph ? 6 : 12)); o->radius_stride = (uint32_t)(rs ? rs : (rh ? 2 : 4));
    o->cp_addr = nvmtl_vk_buffer_address(&cpb->_b) + [[g valueForKey:@"controlPointBufferOffset"] unsignedLongValue];
    o->radius_addr = nvmtl_vk_buffer_address(&rb->_b) + [[g valueForKey:@"radiusBufferOffset"] unsignedLongValue];
    o->curve_indices = nvmtl_vk_buffer_address(&ib->_b) + [[g valueForKey:@"indexBufferOffset"] unsignedLongValue];
    // the flags word of nvk/curves/nvk_curve.h
    o->curve_flags = (uint32_t)basis | (uint32_t)cnt << 2 | (uint32_t)type << 5 | (uint32_t)caps << 6 | (cph ? 1u << 8 : 0) | (rh ? 1u << 9 : 0) | (it == MTLIndexTypeUInt32 ? 1u << 10 : 0);
    return 0;
}

uint32_t nvmtl_geoms_from_descriptor(id desc, nvmtl_geom *out, uint32_t max, int *instance, uint32_t *instance_count, NSMutableSet *track)
{
    *instance = 0; *instance_count = 0;
    if ([desc isKindOfClass:[MTLInstanceAccelerationStructureDescriptor class]]) {
        *instance = 1; *instance_count = (uint32_t)((MTLInstanceAccelerationStructureDescriptor *)desc).instanceCount; return 0;
    }
    if (![desc isKindOfClass:[MTLPrimitiveAccelerationStructureDescriptor class]]) {
        nvlog("acceleration structure: %s is not carried (primitive and instance descriptors are)", NSStringFromClass([desc class]).UTF8String); return 0;
    }
    NSArray *gs = ((MTLPrimitiveAccelerationStructureDescriptor *)desc).geometryDescriptors; uint32_t n = 0;
    for (MTLAccelerationStructureGeometryDescriptor *g in gs) {
        if (n >= max) { nvlog("acceleration structure: more than %u geometries — the rest are dropped", max); break; }
        nvmtl_geom *o = &out[n]; memset(o, 0, sizeof *o); o->opaque = g.opaque ? 1 : 0;
        if ([g isKindOfClass:[MTLAccelerationStructureTriangleGeometryDescriptor class]]) {
            MTLAccelerationStructureTriangleGeometryDescriptor *t = (MTLAccelerationStructureTriangleGeometryDescriptor *)g;
            NVMTLBuffer *vb = (NVMTLBuffer *)t.vertexBuffer;
            if (!vb) { nvlog("acceleration structure: triangle geometry %u has no vertex buffer - built EMPTY, keeping its index", n);
                       nvmtl_geom_make_empty(o); n++; continue; }
            if (track) [track addObject:vb];
            o->vertices = nvmtl_vk_buffer_address(&vb->_b) + t.vertexBufferOffset; o->stride = (uint32_t)t.vertexStride;
            o->count = (uint32_t)t.triangleCount;
            MTLAttributeFormat f = [t respondsToSelector:@selector(vertexFormat)] ? t.vertexFormat : MTLAttributeFormatFloat3;
            // The formats Mesa's BVH builder decodes, from the switch in load_vertices
            // (vk_bvh_helpers.h). It is the real contract: no driver sets
            // VK_FORMAT_FEATURE_ACCELERATION_STRUCTURE_VERTEX_BUFFER_BIT, so asking the device
            // answers no for every format, including plain float3. A four-component format is read
            // at its own width - the stride the game gave us still steps one vertex - and anything
            // narrower than the builder can decode is DROPPED, never widened into a read past the
            // end of the vertex buffer.
            uint32_t vkfmt = 0;
            switch (f) {
            case MTLAttributeFormatFloat2:            vkfmt = 103; break;  // R32G32_SFLOAT
            case MTLAttributeFormatFloat3:            vkfmt = 106; break;  // R32G32B32_SFLOAT
            case MTLAttributeFormatFloat4:            vkfmt = 109; break;  // R32G32B32A32_SFLOAT
            case MTLAttributeFormatHalf2:             vkfmt = 83;  break;  // R16G16_SFLOAT
            case MTLAttributeFormatHalf3:             vkfmt = 90;  break;  // R16G16B16_SFLOAT
            case MTLAttributeFormatHalf4:             vkfmt = 97;  break;  // R16G16B16A16_SFLOAT
            case MTLAttributeFormatShort2Normalized:  vkfmt = 78;  break;  // R16G16_SNORM
            case MTLAttributeFormatShort4Normalized:  vkfmt = 92;  break;  // R16G16B16A16_SNORM
            case MTLAttributeFormatChar2Normalized:   vkfmt = 20;  break;  // R8G8_SNORM
            case MTLAttributeFormatChar4Normalized:   vkfmt = 42;  break;  // R8G8B8A8_SNORM
            default: break;
            }
            if (!vkfmt) {
                nvmtl_rt_count_bump(NVMTL_RT_FMT_UNCARRIED);
                nvlog("acceleration structure: vertex format %lu has no format the builder decodes - geometry %u is built EMPTY, keeping its index, never read as float3",
                      (unsigned long)f, n);
                nvmtl_geom_make_empty(o); n++; continue;
            }
            o->vertex_format = vkfmt;
            // A vertexStride of 0 means TIGHTLY PACKED in Metal - the size of the vertex format,
            // not 12. Defaulting to 12 was harmless while float3 was the only format carried; now that
            // the narrow formats are accepted it would step 12 bytes through a buffer packed at 2 to 8
            // and read off the end, which is the same fault the format work was done to remove.
            if (!o->stride) {
                switch (vkfmt) {
                case 109: o->stride = 16; break;   // R32G32B32A32_SFLOAT
                case 106: o->stride = 12; break;   // R32G32B32_SFLOAT
                case 103: o->stride = 8;  break;   // R32G32_SFLOAT
                case 97:  o->stride = 8;  break;   // R16G16B16A16_SFLOAT
                case 92:  o->stride = 8;  break;   // R16G16B16A16_SNORM
                case 90:  o->stride = 6;  break;   // R16G16B16_SFLOAT
                case 83:  o->stride = 4;  break;   // R16G16_SFLOAT
                case 78:  o->stride = 4;  break;   // R16G16_SNORM
                case 42:  o->stride = 4;  break;   // R8G8B8A8_SNORM
                case 20:  o->stride = 2;  break;   // R8G8_SNORM
                default:  o->stride = 12; break;   // unreachable: vkfmt came from the switch above
                }
            }
            NVMTLBuffer *ib = (NVMTLBuffer *)t.indexBuffer;
            if (ib) { if (track) [track addObject:ib]; o->indices = nvmtl_vk_buffer_address(&ib->_b) + t.indexBufferOffset;
                      o->index_type = t.indexType == MTLIndexTypeUInt16 ? 0 : 1; o->max_vertex = 0xFFFFFF; }
            else { o->index_type = 1000165000u ; o->max_vertex = o->count ? o->count * 3 - 1 : 0; }
        } else if ([g isKindOfClass:[MTLAccelerationStructureBoundingBoxGeometryDescriptor class]]) {
            MTLAccelerationStructureBoundingBoxGeometryDescriptor *b = (MTLAccelerationStructureBoundingBoxGeometryDescriptor *)g;
            NVMTLBuffer *bb = (NVMTLBuffer *)b.boundingBoxBuffer;
            if (!bb) { nvlog("acceleration structure: box geometry %u has no buffer - built EMPTY, keeping its index", n);
                       nvmtl_geom_make_empty(o); n++; continue; }
            if (track) [track addObject:bb];
            o->is_boxes = 1; o->boxes = nvmtl_vk_buffer_address(&bb->_b) + b.boundingBoxBufferOffset;
            o->box_stride = (uint32_t)(b.boundingBoxStride ? b.boundingBoxStride : 24); o->count = (uint32_t)b.boundingBoxCount;
        } else if ([NSStringFromClass([g class]) containsString:@"Curve"] && getenv("NVK_CURVES") && getenv("NVK_CURVES")[0] == '1'
                   && !nvmtl_curve_from_descriptor(g, o, track)) {
            // carried as a curve geometry: the driver writes its boxes (NVK_CURVES=1 in NVK, same process)
        } else {
            nvlog("acceleration structure: geometry %u is a %s, not carried (motion / curves) - built EMPTY, keeping its index",
                  n, NSStringFromClass([g class]).UTF8String);
            nvmtl_geom_make_empty(o); n++; continue;
        }
        n++;
    }
    return n;
}

@implementation NVMTLAccelerationStructure
- (NSUInteger)size { return _b.size; }
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return MTLPurgeableStateNonVolatile; }
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = _as.addr ? _as.addr : nvmtl_vk_buffer_address(&_b); return r; }
- (BOOL)nvmtlEnsureCreated:(int)instance {
    if (_as.as) {
        if (_as.instance != instance) { nvlog("acceleration structure: built as %s before, now as %s — REFUSED", _as.instance ? "instances" : "primitives", instance ? "instances" : "primitives"); return NO; }
        return YES;
    }
    if (nvmtl_vk_accel_create(&_b, _b.size, instance, &_as)) { nvlog("acceleration structure: create FAILED"); return NO; }
    nvlog("acceleration structure %p: %s, %zu bytes at %llx", (__bridge void *)self, instance ? "instances" : "primitives", _b.size, (unsigned long long)_as.addr);
    return YES;
}
- (void)dealloc {
    nvmtl_rt_count_bump(NVMTL_RT_AS_DEAD);
    nvmtl_vk_accel_destroy(&_as);
    if (_uid.buf) nvmtl_vk_buffer_destroy(&_uid);
    if (_scratch.buf) nvmtl_vk_buffer_destroy(&_scratch);
#if !__has_feature(objc_arc)
    [super dealloc];
#endif
}
@end

@implementation NVMTLAccelerationStructureCommandEncoder
- (id<MTLDevice>)device { return _cb->_queue->_dev; }
- (id<MTLCommandBuffer>)commandBuffer { return (id<MTLCommandBuffer>)(_cb ? _cb : _owner); }   // SPI, see the render encoder
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)endEncoding { if (_cb) { _cb->_nEnd++; _cb = nil; } }   // lets go of the command buffer, as the blit encoder does
- (void)nvmtlTrack:(id)r { if (!r) return; if (!_cb->_resources) _cb->_resources = [NVMTLResSet new]; [_cb->_resources addObject:r]; }
- (void)buildAccelerationStructure:(id<MTLAccelerationStructure>)as descriptor:(MTLAccelerationStructureDescriptor *)desc
                     scratchBuffer:(id<MTLBuffer>)scratch scratchBufferOffset:(NSUInteger)soff {
    NVMTLAccelerationStructure *dst = (NVMTLAccelerationStructure *)as; NVMTLBuffer *sc = (NVMTLBuffer *)scratch;
    if (!dst || !sc) {
        nvmtl_rt_count_bump(dst ? NVMTL_RT_NIL_SCRATCH : NVMTL_RT_NIL_STRUCT);
        nvlog("build acceleration structure: nil %s", dst ? "scratch" : "structure"); return; }
    [self nvmtlTrack:dst]; [self nvmtlTrack:sc];
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    // One per thread: these are filled from the descriptor and read by the build, and the game
    // encodes from nine threads at once. Shared, two builds in flight overwrite each other's
    // geometry and a structure gets built from another mesh.
    static _Thread_local nvmtl_geom geoms[NVMTL_MAX_GEOMS]; int instance = 0; uint32_t icount = 0;
    uint32_t n = nvmtl_geoms_from_descriptor(desc, geoms, NVMTL_MAX_GEOMS, &instance, &icount, _cb->_resources);
    uint64_t scratch_addr = (nvmtl_vk_buffer_address(&sc->_b) + soff + 255ull) & ~255ull;
    if (![dst nvmtlEnsureCreated:instance]) return;
    if (!instance && [desc isKindOfClass:[MTLPrimitiveAccelerationStructureDescriptor class]]) {
        NSArray *gd = ((MTLPrimitiveAccelerationStructureDescriptor *)desc).geometryDescriptors; uint32_t add = 0;
        if (gd.count) {
            const NSUInteger o0 = [(MTLAccelerationStructureGeometryDescriptor *)gd[0] intersectionFunctionTableOffset]; BOOL same = YES, affine = YES;
            for (NSUInteger g = 0; g < gd.count; g++) {
                const NSUInteger o = [(MTLAccelerationStructureGeometryDescriptor *)gd[g] intersectionFunctionTableOffset];
                same = same && o == o0; affine = affine && o == o0 + g; }
            if (o0 > 0x7FFFFF || (!same && !affine))
                nvlog("build acceleration structure: geometry intersection function table offsets (first %lu, %lu geometries) are neither equal nor base + index - the slot reads the instance offset alone", (unsigned long)o0, (unsigned long)gd.count);
            else add = (uint32_t)o0 | (same ? 0u : 0x800000u);
        }
        dst->_iftAdd = add;
    }
    uint64_t inst_addr = 0;
    uint32_t nas_traced = 0;
    if (instance) {
        MTLInstanceAccelerationStructureDescriptor *idesc = (MTLInstanceAccelerationStructureDescriptor *)desc;
        NVMTLBuffer *ib = (NVMTLBuffer *)idesc.instanceDescriptorBuffer;
        if (!ib) { nvlog("build acceleration structure: instance descriptors nil"); return; }
        const MTLMatrixLayout matrixLayout = idesc.instanceTransformationMatrixLayout;
        if (matrixLayout != MTLMatrixLayoutColumnMajor && matrixLayout != MTLMatrixLayoutRowMajor) {
            [_cb nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal userInfo:@{NSLocalizedDescriptionKey:@"Unsupported acceleration-structure instance matrix layout"}]];
            return;
        }
        const MTLAccelerationStructureInstanceDescriptorType itype = idesc.instanceDescriptorType;
        if (itype != MTLAccelerationStructureInstanceDescriptorTypeDefault && itype != MTLAccelerationStructureInstanceDescriptorTypeUserID &&
            itype != MTLAccelerationStructureInstanceDescriptorTypeIndirect) {
            nvlog("build acceleration structure: instance descriptor type %lu is not carried (Default, UserID and Indirect are)", (unsigned long)itype); return; }
        const uint32_t natural = itype == MTLAccelerationStructureInstanceDescriptorTypeDefault ? 64 : itype == MTLAccelerationStructureInstanceDescriptorTypeUserID ? 68 : 72;
        NSArray *blases = idesc.instancedAccelerationStructures; static _Thread_local uint64_t addrs[4096]; uint32_t nas = 0;
        static _Thread_local uint32_t iftAdds[4096];
        uint32_t zero_as = 0;
        for (NVMTLAccelerationStructure *b in blases) { if (nas < 4096) { iftAdds[nas] = b->_iftAdd; addrs[nas++] = b->_as.addr; if (!b->_as.addr) zero_as++; } [self nvmtlTrack:b]; }
        // An instanced structure with no device address means its Vulkan object was never created,
        // so the reference written into the instance descriptor is null and the traversal walks a BVH
        // at address zero. Report it rather than handing the GPU work that is certain to fault.
        if (zero_as) {
            FILE *lg = fopen(nvmtl_log_path(), "a");
            if (lg) {
                fprintf(lg, "pid %d NVMTL: build acceleration structure: %u of %u instanced structures have NO device address - their instances reference null\n",
                        getpid(), zero_as, nas);
                fclose(lg);
            }
        }
        NVMTLBuffer *vk = [NVMTLBuffer new];
        size_t bytes = (size_t)(icount ? icount : 1) * (64 + 4);
        if (nvmtl_vk_buffer_create(bytes, 1, &vk->_b) || !vk->_b.map) { nvlog("build acceleration structure: instance conversion buffer FAILED"); return; }
        uint32_t stride = (uint32_t)(idesc.instanceDescriptorStride ? idesc.instanceDescriptorStride : natural);
        // The span is computed so it cannot WRAP. Written as offset + (count-1)*stride + natural > size, an app that
        // passes an offset or a stride near SIZE_MAX overflows the sum, the comparison reads as in-bounds, and the
        // refusal never fires - a bounds check that can wrap is not a bounds check. The reads it guards land in the
        // app's own buffer rather than ours, so this is not a privilege crossing, but the test is still wrong and the
        // correct form costs nothing: compare each term against what is LEFT instead of building a sum.
        const size_t instOff = (size_t)idesc.instanceDescriptorBufferOffset;
        const size_t instSpan = (size_t)(icount ? icount - 1 : 0) * stride + natural;
        const BOOL instOverflows = icount && stride && (size_t)(icount - 1) > (SIZE_MAX - natural) / stride;
        if (stride < natural || instOverflows || instOff > ib->_b.size || instSpan > ib->_b.size - instOff) {
            nvlog("build acceleration structure: %u instances of stride %u at %lu overrun the %zu-byte descriptor buffer - REFUSED", icount, stride,
                  (unsigned long)idesc.instanceDescriptorBufferOffset, ib->_b.size); return; }
        const size_t uidOff = (size_t)(icount ? icount : 1) * 64, uidBytes = (size_t)(icount ? icount : 1) * 4;
        memset((uint8_t *)vk->_b.map + uidOff, 0, uidBytes);
        const uint32_t ctype = itype == MTLAccelerationStructureInstanceDescriptorTypeIndirect ? 3u : (uint32_t)itype;
        // Convert on the GPU, as a dispatch in front of the build. Reading the descriptors on the
        // host can only see writes already submitted, and the structure id of an Indirect
        // descriptor is written INTO that buffer by the GPU, so a host read returns zeros and the
        // top-level structure comes out full of null children. The table of instanced-structure
        // addresses is ours and already known, so it is filled here and read by the kernel.
        NVMTLBuffer *tab = [NVMTLBuffer new];
        const size_t tab_bytes = (size_t)(nas ? nas : 1) * 16;
        int on_gpu = 0;
        if (nvmtl_vk_buffer_create(tab_bytes, 1, &tab->_b) || !tab->_b.map) {
            // Counted here too, or conv_gpu + conv_host stops equalling the number of conversions and
            // the route most worth seeing - the table failing under memory pressure - reads as nothing.
            nvmtl_rt_count_bump(NVMTL_RT_CONV_HOST);
            nvlog("build acceleration structure: structure address table (%zu B) FAILED - the conversion stays on the CPU", tab_bytes);
        } else {
            uint32_t *t = (uint32_t *)tab->_b.map;
            memset(t, 0, tab_bytes);
            for (uint32_t k = 0; k < nas; k++) {
                t[k * 4 + 0] = (uint32_t)(addrs[k] & 0xFFFFFFFFu);
                t[k * 4 + 1] = (uint32_t)(addrs[k] >> 32);
                t[k * 4 + 2] = iftAdds[k];
            }
            const uint64_t mtl_addr = nvmtl_vk_buffer_address(&ib->_b);
            nvmtl_convinst_args ca = {
                .src = mtl_addr ? mtl_addr + idesc.instanceDescriptorBufferOffset : 0,
                .dst = nvmtl_vk_buffer_address(&vk->_b),
                .uid = nvmtl_vk_buffer_address(&vk->_b) + uidOff,
                .astab = nvmtl_vk_buffer_address(&tab->_b),
                .stride = stride, .count = icount, .mtl_type = ctype, .n_as = nas, .matrix_layout = (uint32_t)matrixLayout,
            };
            on_gpu = nvmtl_vk_cmd_convert_instances(&_cb->_c, &ca) == 0;
            nvmtl_rt_count_bump(on_gpu ? NVMTL_RT_CONV_GPU : NVMTL_RT_CONV_HOST);
            if (on_gpu) {
                if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
                [_cb->_scratch addObject:tab];
                if (getenv("NVMTL_RT_ADDR_TRACE") && icount >= 4) {
                    struct { float m[12]; uint32_t cust_mask, sbt_flags; uint64_t ref; } w[8];
                    const uint32_t have = nvmtl_vk_convert_witness_read(w, 8);
                    FILE *lg = have ? fopen(nvmtl_log_path(), "a") : NULL;
                    for (uint32_t k = 0; lg && k < have; k++) {
                        // Which layout the app actually wrote, decided from the sample itself rather
                        // than argued about. Apple's header declares MTLPackedFloat4x3 as
                        // MTLPackedFloat3 columns[4], so the spec reading is m[col * 3 + row] and the
                        // translation is words 9..11. A DXR-shaped engine instead writes a row-major
                        // float[3][4], where the rows are words 0..2 / 4..6 / 8..10 and the
                        // translation is words 3, 7, 11. A rigid instance transform has unit row
                        // norms under the CORRECT reading and mixes the translation into the rows
                        // under the wrong one, so printing both norms names the layout outright.
                        const float *m = w[k].m;
                        const double n0 = sqrt((double)m[0]*m[0] + (double)m[1]*m[1] + (double)m[2]*m[2]);
                        const double n1 = sqrt((double)m[4]*m[4] + (double)m[5]*m[5] + (double)m[6]*m[6]);
                        const double n2 = sqrt((double)m[8]*m[8] + (double)m[9]*m[9] + (double)m[10]*m[10]);
                        const double c0 = sqrt((double)m[0]*m[0] + (double)m[3]*m[3] + (double)m[6]*m[6]);
                        const double c1 = sqrt((double)m[1]*m[1] + (double)m[4]*m[4] + (double)m[7]*m[7]);
                        const double c2 = sqrt((double)m[2]*m[2] + (double)m[5]*m[5] + (double)m[8]*m[8]);
                        const double drow = fabs(n0 - 1) + fabs(n1 - 1) + fabs(n2 - 1);
                        const double dcol = fabs(c0 - 1) + fabs(c1 - 1) + fabs(c2 - 1);
                        // A bare drow < dcol would name a layout off noise. An ALL-ZERO witness gives
                        // drow == dcol == 3.0 and would print "COLUMN-MAJOR" with full confidence - and
                        // zeros are the LIKELY reading here, because unwritten descriptors are exactly
                        // what the GPU conversion exists to fix. A scaled or non-rigid instance makes
                        // both errors large too. So one reading must look rigid AND beat the other
                        // clearly, or this says nothing.
                        const int rigid = drow < 0.05 || dcol < 0.05;
                        const int apart = fabs(drow - dcol) > 0.25;
                        const char *verdict = !rigid || !apart ? "CANNOT TELL from this sample"
                                            : drow < dcol ? "ROW-MAJOR (DXR shaped, NOT Apple's documented columns[4])"
                                                          : "COLUMN-MAJOR (Apple's documented MTLPackedFloat4x3)";
                        fprintf(lg, "pid %d NVMTL: RTCONV witness[%u] (this build has %u instances): ref=0x%llx custom=%u mask=0x%02x sbt=%u flags=0x%x\n",
                                getpid(), k, icount, (unsigned long long)w[k].ref, w[k].cust_mask & 0xFFFFFF,
                                w[k].cust_mask >> 24, w[k].sbt_flags & 0xFFFFFF, w[k].sbt_flags >> 24);
                        fprintf(lg, "pid %d NVMTL: RTCONV xform[%u]: raw %.3f %.3f %.3f %.3f / %.3f %.3f %.3f %.3f / %.3f %.3f %.3f %.3f\n",
                                getpid(), k, m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7],
                                m[8], m[9], m[10], m[11]);
                        fprintf(lg, "pid %d NVMTL: RTLAYOUT[%u]: as ROWS norms %.3f %.3f %.3f (err %.3f) translation %.1f %.1f %.1f | "
                                    "as COLUMNS norms %.3f %.3f %.3f (err %.3f) translation %.1f %.1f %.1f -> the app wrote %s\n",
                                getpid(), k, n0, n1, n2, drow, m[3], m[7], m[11],
                                c0, c1, c2, dcol, m[9], m[10], m[11], verdict);
                    }
                    if (lg) fclose(lg);
                    nvmtl_vk_cmd_convert_witness(&_cb->_c, &vk->_b, icount);
                }
            }
        }
        // The host fallback. It is wrong for descriptors the GPU has yet to write, so it says so
        // rather than quietly building a structure out of whatever the buffer held.
        if (!on_gpu) {
            const void *mtl_desc = nvmtl_buf_cpu(ib);
            size_t mtl_base = idesc.instanceDescriptorBufferOffset;
            void *staged = NULL;
            if (!mtl_desc) {
                const size_t span = (size_t)(icount ? icount - 1 : 0) * stride + natural;
                staged = malloc(span);
                if (!staged || nvmtl_vk_buffer_readback(&ib->_b, mtl_base, span, staged)) {
                    free(staged);
                    nvlog("build acceleration structure: could not stage %zu bytes of instance descriptors out of a device-only buffer", span);
                    return;
                }
                mtl_desc = staged;
                mtl_base = 0;
            }
            if (icount) nvlog("build acceleration structure: converting %u instances on the HOST - descriptors the GPU has not written yet read as zero", icount);
            nvmtl_vk_convert_instances((const uint8_t *)mtl_desc + mtl_base, stride, icount, addrs, nas, vk->_b.map,
                                       ctype, (uint32_t *)((uint8_t *)vk->_b.map + uidOff), iftAdds, (uint32_t)matrixLayout);
            free(staged);
        }
        if (dst->_uid.size < uidBytes) {
            if (dst->_uid.buf) { NVMTLBuffer *old = [NVMTLBuffer new]; old->_b = dst->_uid; if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new]; [_cb->_scratch addObject:old]; }
            memset(&dst->_uid, 0, sizeof dst->_uid);
            if (nvmtl_vk_buffer_create(uidBytes, 0, &dst->_uid)) { nvlog("build acceleration structure: userID side table (%zu B) FAILED - user_instance_id reads 0", uidBytes); memset(&dst->_uid, 0, sizeof dst->_uid); }
        }
        if (dst->_uid.buf && nvmtl_vk_cmd_copy_buffer(&_cb->_c, &vk->_b, uidOff, &dst->_uid, 0, uidBytes))
            nvlog("build acceleration structure: userID side table copy FAILED");
        if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
        [_cb->_scratch addObject:vk];
        [self nvmtlTrack:ib]; inst_addr = nvmtl_vk_buffer_address(&vk->_b); nas_traced = nas;
    }
    // Every address the build hands the GPU, with where its memory actually lives. A page fault in
    // NVK's traversal names an address and nothing else, so without this there is no way to tell which
    // of these allocations it was, or whether that allocation had been spilled out of VRAM.
    if (getenv("NVMTL_RT_ADDR_TRACE")) {
        FILE *lg = fopen(nvmtl_log_path(), "a");
        if (lg) {
            fprintf(lg, "pid %d NVMTL: RTADDR build %s: as=0x%llx size %zu backing=%s scratch=0x%llx(%s,%zu B) inst=0x%llx(%s) geoms=%u\n",
                    getpid(), instance ? "instances" : "primitives",
                    (unsigned long long)dst->_as.addr, dst->_b.size,
                    dst->_b.map ? "SYSTEM" : (dst->_b.bar1 ? "BAR1" : "VRAM"),
                    (unsigned long long)scratch_addr, sc->_b.map ? "SYSTEM" : (sc->_b.bar1 ? "BAR1" : "VRAM"), sc->_b.size,
                    (unsigned long long)inst_addr, instance ? "instbuf" : "none", instance ? icount : n);
            if (instance) fprintf(lg, "pid %d NVMTL: RTADDR tlas: %u instances over %u structures\n", getpid(), icount, nas_traced);
            fclose(lg);
        }
    }
    // What this build actually needs, for the geometry and instance count being recorded now - not
    // for whatever descriptor the game asked about when it sized the buffers.
    {
        size_t need_as = 0, need_scratch = 0;
        if (!nvmtl_vk_accel_sizes(geoms, n, instance, icount, &need_as, &need_scratch)) {
            const size_t have_scratch = sc->_b.size > soff ? sc->_b.size - soff : 0;
            // need_as == 0 is checked as well as the return code, because the return code alone can
            // never fail here: init and g_rt are already proven by nvmtlEnsureCreated, and n is capped
            // at NVMTL_MAX_GEOMS by construction. A guard whose condition cannot occur reads as
            // protection and gives none, and its counter is a clean zero forever.
            if (!need_as) {
                nvmtl_rt_count_bump(NVMTL_RT_SIZE_UNKNOWN);
                nvlog("build acceleration structure: the builder sized this build at ZERO bytes (%u geometr%s, %s) - REFUSED rather than build against a size we do not believe",
                      n, n == 1 ? "y" : "ies", instance ? "instances" : "primitives");
                return;
            }
            if (need_as > dst->_b.size) {
                nvmtl_rt_count_bump(NVMTL_RT_AS_SHORT);
                uint64_t prims = 0;
                for (uint32_t k = 0; k < n; k++) prims += geoms[k].count;
                FILE *lg = fopen(nvmtl_log_path(), "a");
                if (lg) {
                    fprintf(lg, "pid %d NVMTL: RTSHORT %s build needs %zu bytes, the structure has %zu: %u geometr%s, %llu primitive(s), icount %u - REFUSED\n",
                            getpid(), instance ? "instances" : "primitives", need_as, dst->_b.size,
                            n, n == 1 ? "y" : "ies", (unsigned long long)prims, icount);
                    for (uint32_t k = 0; k < n && k < 4; k++)
                        fprintf(lg, "pid %d NVMTL:   geom[%u] count %u stride %u fmt %u boxes %u\n",
                                getpid(), k, geoms[k].count, geoms[k].stride, geoms[k].vertex_format, geoms[k].is_boxes);
                    fclose(lg);
                }
                return;
            }
            if (need_scratch > have_scratch) {
                nvmtl_rt_count_bump(NVMTL_RT_SCRATCH_SHORT);
                // Our own scratch, kept on the structure so repeated builds reuse one allocation.
                if (dst->_scratch.size < need_scratch) {
                    if (dst->_scratch.buf) {
                        NVMTLBuffer *old = [NVMTLBuffer new]; old->_b = dst->_scratch;
                        if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
                        [_cb->_scratch addObject:old];
                        memset(&dst->_scratch, 0, sizeof dst->_scratch);
                    }
                    if (nvmtl_vk_buffer_create(need_scratch, 0, &dst->_scratch) &&
                        nvmtl_vk_buffer_create(need_scratch, 1, &dst->_scratch)) {
                        memset(&dst->_scratch, 0, sizeof dst->_scratch);
                        nvlog("build acceleration structure: needs %zu scratch bytes, has %zu, and our own scratch FAILED to allocate",
                              need_scratch, have_scratch);
                    }
                }
                if (dst->_scratch.buf) {
                    nvmtl_rt_count_bump(NVMTL_RT_SCRATCH_OURS);
                    nvlog("build acceleration structure: needs %zu scratch bytes and the game gave %zu - using our own",
                          need_scratch, have_scratch);
                    scratch_addr = (nvmtl_vk_buffer_address(&dst->_scratch) + 255ull) & ~255ull;
                }
            }
        } else {
            // A check that cannot run must block, never report clean. Without a size we cannot know
            // the build fits, and a build that writes past its structure takes the channel and the
            // process, where a missing structure only loses the geometry.
            nvmtl_rt_count_bump(NVMTL_RT_SIZE_UNKNOWN);
            nvlog("build acceleration structure: the required size could NOT be computed (%u geometries, %s) - REFUSED rather than risk writing past the structure",
                  n, instance ? "instances" : "primitives");
            return;
        }
    }
    nvmtl_rt_count_bump(instance ? NVMTL_RT_BUILD_INST : NVMTL_RT_BUILD_PRIM);
    if (instance) nvmtl_rt_count_add(NVMTL_RT_INSTANCES, icount);
    // Atomic: nine encoding threads reach this, and a plain ++ is a read-modify-write that can miss
    // the % 64 entirely under contention, so the census fired at an unpredictable rate.
    { static unsigned long long nb; if (__atomic_add_fetch(&nb, 1, __ATOMIC_RELAXED) % 64 == 0) nvmtl_rt_census(); }
    if (nvmtl_vk_cmd_build_accel(&_cb->_c, &dst->_as, geoms, n, inst_addr, icount, scratch_addr)) nvlog("build acceleration structure: FAILED");
    else nvlog("build acceleration structure %p: %s (%u %s)", (__bridge void *)dst, instance ? "instances" : "primitives", instance ? icount : n, instance ? "instances" : "geometries");
}
- (void)refitAccelerationStructure:(id<MTLAccelerationStructure>)src descriptor:(MTLAccelerationStructureDescriptor *)d
                       destination:(id<MTLAccelerationStructure>)dst scratchBuffer:(id<MTLBuffer>)s scratchBufferOffset:(NSUInteger)o {
    [self buildAccelerationStructure:(dst ? dst : src) descriptor:d scratchBuffer:s scratchBufferOffset:o];
}
- (void)refitAccelerationStructure:(id<MTLAccelerationStructure>)src descriptor:(MTLAccelerationStructureDescriptor *)d
                       destination:(id<MTLAccelerationStructure>)dst scratchBuffer:(id<MTLBuffer>)s scratchBufferOffset:(NSUInteger)o
                           options:(MTLAccelerationStructureRefitOptions)opts {
    [self buildAccelerationStructure:(dst ? dst : src) descriptor:d scratchBuffer:s scratchBufferOffset:o];
}
- (void)copyAccelerationStructure:(id<MTLAccelerationStructure>)src toAccelerationStructure:(id<MTLAccelerationStructure>)dst {
    NVMTLAccelerationStructure *s = (NVMTLAccelerationStructure *)src, *d = (NVMTLAccelerationStructure *)dst;
    if (!s || !d || !s->_as.as) { nvlog("copy acceleration structure: %s", s && d ? "source never built" : "nil"); return; }
    if (![d nvmtlEnsureCreated:s->_as.instance]) return;
    [self nvmtlTrack:s]; [self nvmtlTrack:d];
    if (nvmtl_vk_cmd_copy_accel(&_cb->_c, &s->_as, &d->_as)) nvlog("copy acceleration structure: FAILED");
    if (s->_uid.buf) {
        if (d->_uid.size < s->_uid.size) {
            if (d->_uid.buf) { NVMTLBuffer *old = [NVMTLBuffer new]; old->_b = d->_uid; if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new]; [_cb->_scratch addObject:old]; }
            memset(&d->_uid, 0, sizeof d->_uid);
            if (nvmtl_vk_buffer_create(s->_uid.size, 0, &d->_uid)) { nvlog("copy acceleration structure: side table FAILED"); memset(&d->_uid, 0, sizeof d->_uid); }
        }
        if (d->_uid.buf && nvmtl_vk_cmd_copy_buffer(&_cb->_c, &s->_uid, 0, &d->_uid, 0, s->_uid.size)) nvlog("copy acceleration structure: side table copy FAILED");
    }
}
- (void)copyAndCompactAccelerationStructure:(id<MTLAccelerationStructure>)src toAccelerationStructure:(id<MTLAccelerationStructure>)dst {
    [self copyAccelerationStructure:src toAccelerationStructure:dst];
}
- (void)writeCompactedAccelerationStructureSize:(id<MTLAccelerationStructure>)as toBuffer:(id<MTLBuffer>)buf offset:(NSUInteger)off {
    [self writeCompactedAccelerationStructureSize:as toBuffer:buf offset:off sizeDataType:MTLDataTypeUInt];
}
- (void)writeCompactedAccelerationStructureSize:(id<MTLAccelerationStructure>)as toBuffer:(id<MTLBuffer>)buf offset:(NSUInteger)off sizeDataType:(MTLDataType)t {
    NVMTLAccelerationStructure *a = (NVMTLAccelerationStructure *)as; NVMTLBuffer *b = (NVMTLBuffer *)buf;
    if (!a || !b) { nvlog("writeCompactedAccelerationStructureSize: nil"); return; }
    const uint64_t v = a->_b.size; const size_t n = t == MTLDataTypeULong ? 8 : 4;
    if (off > b->_b.size || b->_b.size - off < n) { nvlog("writeCompactedAccelerationStructureSize: %zu bytes at %lu is past the %zu-byte buffer - REFUSED", n, (unsigned long)off, b->_b.size); return; }
    if (off & 3) {
        if (!nvmtl_buf_cpu(b)) { nvlog("writeCompactedAccelerationStructureSize: offset %lu is not 4-aligned and the buffer is not host-visible - REFUSED", (unsigned long)off); return; }
        static int said; if (said++ < 2) nvlog("writeCompactedAccelerationStructureSize: offset %lu is not 4-aligned - written from the CPU at encode time (unordered)", (unsigned long)off);
        memcpy((uint8_t *)nvmtl_buf_cpu(b) + off, &v, n);
        if (b->_shadow.map) [b didModifyRange:NSMakeRange(off, n)];
        return;
    }
    [self nvmtlTrack:b];
    if (nvmtl_vk_cmd_write_word(&_cb->_c, &b->_b, off, (uint32_t)v) ||
        (n == 8 && nvmtl_vk_cmd_write_word(&_cb->_c, &b->_b, off + 4, (uint32_t)(v >> 32))))
        nvlog("writeCompactedAccelerationStructureSize: GPU write at %lu FAILED", (unsigned long)off);
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u { [self nvmtlTrack:r]; }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])rs count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i = 0; i < n; i++) [self nvmtlTrack:rs[i]]; }
- (void)useHeap:(id<MTLHeap>)h { nvmtl_use_heap(_cb, h); }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])hs count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self useHeap:hs[i]]; }
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f {}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)sampleCountersInBuffer:(id<MTLCounterSampleBuffer>)b atSampleIndex:(NSUInteger)i withBarrier:(BOOL)w { nvmtl_sample_counter(_cb, b, i, self, w); }
@end

// Shared buffers with a VRAM working copy (NVMTL_SHARED_VRAM=1, see nvmtl_shared_wc.h). One lock: the diff and the copy
// for a buffer must not interleave with another command buffer's, and the copies are synchronous and ordered.
static os_unfair_lock gSwcLock = OS_UNFAIR_LOCK_INIT;
static void nvmtl_swc_sync_in(NVMTLBuffer *b)
{
    nvmtl_buf_settle(b);
    nvmtl_swc_range r[16]; int n;
    os_unfair_lock_lock(&gSwcLock);
    // the host side is the caller's pages when there are any (then _shadow is staging for the transfer), else _shadow itself
    const uint8_t *host = b->_hostPtr ? (const uint8_t *)b->_hostPtr : (const uint8_t *)b->_shadow.map;
    n = nvmtl_swc_before_submit((nvmtl_swc *)b->_swc, host, r, 16);
    if (b->_hostPtr) for (int i = 0; i < n; i++) memcpy((uint8_t *)b->_shadow.map + r[i].lo, host + r[i].lo, r[i].hi - r[i].lo);
    for (int i = 0; i < n; i++)
        if (nvmtl_vk_copy_buffer(nvmtl_device_queue(), &b->_shadow, r[i].lo, &b->_b, r[i].lo, r[i].hi - r[i].lo)) {
            // the GPU keeps the old bytes: force a full upload at the next submit rather than trust the hashes
            ((nvmtl_swc *)b->_swc)->vram_valid = 0;
            nvlog("shared VRAM copy: upload of %zu bytes at %zu FAILED - the next submit uploads everything", r[i].hi - r[i].lo, r[i].lo);
            break; }
    os_unfair_lock_unlock(&gSwcLock);
}
static void nvmtl_swc_sync_out(NVMTLBuffer *b)
{
    os_unfair_lock_lock(&gSwcLock);
    if (nvmtl_vk_copy_buffer(nvmtl_device_queue(), &b->_b, 0, &b->_shadow, 0, b->_shadow.size < b->_b.size ? b->_shadow.size : b->_b.size))
        nvlog("shared VRAM copy: copy-back of %zu bytes FAILED - contents keeps the CPU's last bytes", b->_b.size);
    else {
        if (b->_hostPtr) memcpy(b->_hostPtr, b->_shadow.map, b->_hostLen < b->_shadow.size ? b->_hostLen : b->_shadow.size);
        nvmtl_swc_after_complete((nvmtl_swc *)b->_swc, b->_hostPtr ? b->_hostPtr : b->_shadow.map);
    }
    os_unfair_lock_unlock(&gSwcLock);
}
static void nvmtl_buffer_sync_in(NVMTLBuffer *b)
{
    if (b && b->_swc && b->_shadow.map) { nvmtl_swc_sync_in(b); return; }
    if (!b || !b->_hostPtr || !b->_b.map || !b->_hostLen) return;
    const uint64_t t0 = nvmtl_perf_now();
    memcpy(b->_b.map, b->_hostPtr, b->_hostLen);
    nvmtl_perf_note(NVP_SYNCIN, t0, b->_hostLen);
}

static void nvmtl_buffer_sync_out(NVMTLBuffer *b)
{
    if (b && b->_swc && b->_shadow.map) { nvmtl_swc_sync_out(b); return; }
    if (b && b->_shadow.map) {
        if (b->_syncOut) { b->_syncOut = 0; __atomic_store_n(&b->_zshadow, 0, __ATOMIC_RELEASE);
            if (nvmtl_vk_copy_buffer(nvmtl_device_queue(), &b->_b, 0, &b->_shadow, 0, b->_b.size)) nvlog("commit: managed sync-out of %zu bytes FAILED", b->_b.size); }
        return;
    }
    if (!b || !b->_hostPtr || !b->_b.map || !b->_hostLen) return;
    const uint64_t t0 = nvmtl_perf_now();
    memcpy(b->_hostPtr, b->_b.map, b->_hostLen);
    nvmtl_perf_note(NVP_SYNCOUT, t0, b->_hostLen);
}
