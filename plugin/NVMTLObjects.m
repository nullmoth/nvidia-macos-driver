/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#import <QuartzCore/QuartzCore.h>
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

void nvlog(const char *fmt, ...);

#import "NVMTLObjects.h"
#import <objc/runtime.h>
#import <objc/message.h>
static int nvmtl_trace_on(void);
static void nvtrace(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static unsigned long long nvmtl_global_trace_id(id o) {
    static char key; static unsigned long long next = 1;
    @synchronized (o) {
        NSNumber *n = objc_getAssociatedObject(o, &key);
        if (!n) { n = @(__atomic_fetch_add(&next, 1, __ATOMIC_RELAXED)); objc_setAssociatedObject(o, &key, n, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
        return n.unsignedLongLongValue;
    }
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
__attribute__((constructor)) static void nvmtl_conf_knobs(void)
{
#if NVMTL_RELEASE
    { extern char **environ; char *names[256]; unsigned n = 0;
      for (char **e = environ; e && *e && n < 256; e++)
          if (!strncmp(*e, "NVMTL_", 6) || !strncmp(*e, "NVK_", 4) || !strncmp(*e, "NAK_", 4) || !strncmp(*e, "NVRM_", 5)) {
              const char *eq = strchr(*e, '='); if (!eq) continue;
              names[n] = strndup(*e, (size_t)(eq - *e)); if (names[n]) n++; }
      for (unsigned i = 0; i < n; i++) { unsetenv(names[i]); free(names[i]); } }
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
    char recbuf[2048]; size_t recn = 0; recbuf[0] = 0;
    FILE *f = fopen("/Library/GPUBundles/nvmtl/nvrm610.conf", "r");
    if (!f) { if (forgot) nvlog("conf: forgot %u inherited knob(s); no conf file", forgot); return; }
    char line[512];
    unsigned took = 0;
    while (fgets(line, sizeof line, f)) {
        char *p = line;
        while (*p == ' ' || *p == '\t') p++;
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
        if (getenv(key)) continue;
        if (setenv(key, val, 0) == 0) { took++;
            int k = snprintf(recbuf + recn, sizeof recbuf - recn, "%s%s=%s", recn ? ";" : "", key, val);
            if (k > 0 && (size_t)k < sizeof recbuf - recn) recn += (size_t)k; else recbuf[recn] = 0; }
    }
    fclose(f);
    if (recn) setenv("NVMTL_CONF_SET", recbuf, 1);
    if (took || forgot) nvlog("conf: %u knob(s) read from /Library/GPUBundles/nvmtl/nvrm610.conf, %u inherited conf knob(s) forgotten first (the environment still wins)", took, forgot);
}
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
@implementation NVMTLResSet
- (instancetype)init { if ((self = [super init])) _s = [NSMutableSet new]; return self; }
- (instancetype)initWithCapacity:(NSUInteger)n { if ((self = [super init])) _s = [[NSMutableSet alloc] initWithCapacity:n]; return self; }
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
- (void)addObject:(id)o { if (!o) return; os_unfair_lock_lock(&_lk); BOOL had = [_s member:o] != nil; if (!had) [_s addObject:o];
    os_unfair_lock_unlock(&_lk); if (!had) nvmtl_resref(o, 1); }
- (void)removeObject:(id)o { if (!o) return; os_unfair_lock_lock(&_lk); BOOL had = [_s member:o] != nil; if (had) [_s removeObject:o];
    os_unfair_lock_unlock(&_lk); if (had) nvmtl_resref(o, -1); }
- (void)removeAllObjects { os_unfair_lock_lock(&_lk); NSArray *a = _s.allObjects; [_s removeAllObjects]; os_unfair_lock_unlock(&_lk);
    for (id o in a) nvmtl_resref(o, -1); }
- (id)copyWithZone:(NSZone *)z { atomic_fetch_add(&gRes2Tick, 1);
    NSArray *a = [self nvmtlSnapshot]; NVMTLResSet *c = [[NVMTLResSet alloc] initWithCapacity:a.count]; for (id o in a) [c addObject:o]; return c; }
- (id)mutableCopyWithZone:(NSZone *)z { return [self copyWithZone:z]; }
- (void)dealloc { for (id o in _s) nvmtl_resref(o, -1); }
@end

static BOOL nvmtl_purge_eligible(NVMTLTexture *t) {
    return !t->_parent && !t->_viewParent && !t->_backBuf && !t->_heap && !t->_subHeap && !t->_surf && !t->_vramOn && !t->_ownView
        && !t->_shareable && !t->_resid && !__atomic_load_n(&t->_nviews, __ATOMIC_ACQUIRE) && t->_i.img && !t->_i.placed
        && t->_i.usage && !t->_i.vpair && t->_i.samples <= 1 && !(t->_i.usage & 0x20u );
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
        && !t->_i.vpair && t->_i.samples <= 1 && !(t->_i.usage & 0x20u );
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
                if (t->_resid & (1ull << 62)) nvmtl_vk_bindless_rewrite((uint32_t)(t->_resid & 0xffffffffu), t->_i.view);
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
        const uint64_t f0 = nvmtl_vk_vram_free_now() + gGraveVram, cap = force ? want + gRes2Margin : (target > f0 ? target - f0 : 0); uint64_t movedM = 0;
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
static void nvmtl_zero_new_buffer(NVMTLBuffer *b, NSUInteger len) {
    const int m = nvmtl_zero_mode();
    if (!m || !b || !len) return;
    const int mz = b->_shadow.map && m >= 2 && b->_b.buf && !b->_b.map && nvmtl_mzero_on();
    if (mz) __atomic_store_n(&b->_zshadow, 1, __ATOMIC_RELEASE);
    if (b->_shadow.map && !mz) {
        memset(b->_shadow.map, 0, len); nvmtl_managed_mark(b, 0, MIN((NSUInteger)b->_b.size, len));
    } else if (b->_b.map && !mz) {
        memset(b->_b.map, 0, len);
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
        *out = p->img; memmove(p, p + 1, (size_t)(b->_nbpk - k - 1) * sizeof *p); b->_nbpk--; got = 0; break;
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
    if (parked) { memset(img, 0, sizeof *img); atomic_fetch_add(&gBTParked, 1); }
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
    t->_stex = t->_i.storage != 0;
    t->_ropt |= (MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift;
    t->_usage = d.usage;
    _texSeen = YES;
    t->_backBuf = self; t->_backOff = offset; t->_backBPR = bpr;
    t->_bparkable = !noRecycle; memcpy(t->_pkey, bkey, sizeof bkey);
    atomic_fetch_add(brec ? &gBTRecycled : &gBTNew, 1); nvmtl_buftex_census(brec ? "recycled" : "new");
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
- (uint64_t)gpuAddress { return nvmtl_vk_buffer_address(&_b); }
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
    if (_subHeap) { NVMTLHeap *h = _subHeap; NSUInteger c = _subCharge; _subHeap = nil; _subCharge = 0;
                    if (_subPlaced && c) { _subPlaced = NO; [h nvmtlGiveRange:_heapOffset length:c]; }
                    [h nvmtlReleaseSubAllocation:c]; }
    if (_hostDealloc) { void (^d)(void *, NSUInteger) = _hostDealloc; _hostDealloc = nil;
                        d(_hostPtr ?: _b.map, (NSUInteger)(_hostLen ?: _b.size)); } }
- (MTLStorageMode)storageMode { return _storage; }
- (void *)contents { nvmtl_buf_settle(self); return _shadow.map ? _shadow.map : (_hostPtr ? _hostPtr : _b.map); }
- (id<MTLHeap>)heap { return _heap ?: _subHeap; }
- (NSUInteger)heapOffset { return _heapOffset; }
- (NSUInteger)length { return _b.size; }
- (void)didModifyRange:(NSRange)r {
    nvmtl_buf_settle(self);
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
static int g_trace_lines;
static int g_trace_bigonly, g_trace_mute;
static int nvmtl_trace_on(void) {
    static uint64_t next; static int on;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    static int cap = 2500;
    if (now >= next) { next = now + 250000000ull; int was = on; on = access("/tmp/nvmtl-trace-request", F_OK) == 0;
        g_trace_bigonly = on && access("/tmp/nvmtl-trace-big", F_OK) == 0; if (!g_trace_bigonly) g_trace_mute = 0;
        if (on && !was) { g_trace_lines = 0; cap = access("/tmp/nvmtl-trace-long", F_OK) == 0 ? 20000 : 2500; } }
    return on && !g_trace_mute && g_trace_lines < cap;
}
static int nvtrace_scope(uint32_t w, uint32_t h) { int was = g_trace_mute; g_trace_mute = 0; (void)nvmtl_trace_on();
    g_trace_mute = g_trace_bigonly && (w < 1000 || h < 100); return was; }
static void nvtrace(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void nvtrace(const char *fmt, ...) {
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
static unsigned nvmtl_surf_id(NVMTLTexture *t) { NVMTLTexture *r = (t && t->_parent) ? t->_parent : t; return (r && r->_surf) ? (unsigned)IOSurfaceGetID(r->_surf) : 0u; }
static int nvmtl_noseed(void) { static uint64_t next; static int on; uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    if (now >= next) { next = now + 250000000ull; int was = on; on = access("/tmp/nvmtl-noseed", F_OK) == 0;
        if (on && !was) nvlog("G17 EXPERIMENT ON: surface-out writes under a read-only lock, the IOSurface seed does not move");
        if (!on && was) nvlog("G17 experiment off: surface-out bumps the seed again"); }
    return on; }
static void nvmtl_census(const char *what, uint32_t w, uint32_t h, int surf) {
    static struct { const char *what; uint32_t w, h; int surf; } seen[48]; static int n;
    @synchronized([NSObject class]) {
        for (int i = 0; i < n; i++) if (seen[i].what == what && seen[i].w == w && seen[i].h == h && seen[i].surf == surf) return;
        if (n == 48) return;
        seen[n].what = what; seen[n].w = w; seen[n].h = h; seen[n].surf = surf; n++;
    }
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
static void nvmtl_frame_dump(const char *tag, const void *base, size_t row, uint32_t w, uint32_t h) {
    if ((uint64_t)w * h < 1024 || access("/tmp/nvmtl-dump-request", F_OK)) return;
    static struct { char tag[24]; uint32_t w, h; uint64_t last; } slot[32]; static int nslot;
    uint64_t now = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW); int k, go = 0;
    @synchronized([NSObject class]) {
        for (k = 0; k < nslot; k++) if (slot[k].w == w && slot[k].h == h && !strncmp(slot[k].tag, tag, sizeof slot[k].tag - 1)) break;
        if (k == nslot && nslot < 32) { snprintf(slot[k].tag, sizeof slot[k].tag, "%s", tag); slot[k].w = w; slot[k].h = h; slot[k].last = 0; nslot++; }
        if (k < nslot && (!slot[k].last || now - slot[k].last >= 2000000000ull)) { slot[k].last = now; go = 1; }
    }
    if (!go) return;
    char dst[400], tmp[416]; const char *dir = "/tmp"; int fd = -1;
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
#pragma mark - texture
static _Atomic unsigned long long nvmtl_tex_objs_made, nvmtl_tex_objs_gone;
unsigned long long nvmtl_tex_objs_live(void) {
    unsigned long long made = nvmtl_tex_objs_made, gone = nvmtl_tex_objs_gone;
    return made > gone ? made - gone : 0;
}
unsigned long long nvmtl_tex_objs_released(void) { return nvmtl_tex_objs_gone; }
@implementation NVMTLTexture
+ (instancetype)alloc { nvmtl_tex_objs_made++; return [super alloc]; }
- (void)setResponsibleProcess:(int)pid { static int said; if (!said++) nvlog("setResponsibleProcess: pid %d on a texture - per-process attribution is not kept; nothing to set (said once)", pid); }
- (unsigned long long)protectionOptions { return 0; }
- (NSUInteger)allocatedSize { nvk_image *i = [self nvi]; if (i->alloc) return (NSUInteger)i->alloc;
    NSUInteger l = i->layers ? i->layers : 1, m = i->mips ? i->mips : 1, bpp = i->bpp ? i->bpp : 4, bw = i->bw ? i->bw : 1, bh = i->bh ? i->bh : 1, tot = 0;
    for (NSUInteger k = 0; k < m; k++) { NSUInteger w = i->w >> k ? i->w >> k : 1, h = i->h >> k ? i->h >> k : 1; tot += ((w + bw - 1) / bw) * ((h + bh - 1) / bh) * bpp; }
    return tot * l; }
- (MTLResourceID)gpuResourceID {
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
- (id<MTLHeap>)heap { return _heap ?: _subHeap; }
- (NSUInteger)heapOffset { return _heapOffset; }
- (void)dealloc {
    if (_sampledDepthView) nvmtl_vk_image_view_destroy(_sampledDepthView);
    nvmtl_tex_objs_gone++;
    if (_tbView || _tbAlias) { nvmtl_vk_texel_view_destroy(_tbAlias, _tbView); _tbView = _tbAlias = NULL; }
    if (_resid) nvmtl_resid_texture_gone(_resid);
    if (_ownView) nvmtl_vk_image_view_destroy(_ownView);
    if (!_parent && _parkable && _subHeap && _i.img) nvmtl_heap_park(_subHeap, &_i, _pkey, _heapOffset, _pkLen);
    if (!_parent && _bparkable && _backBuf && _i.img) nvmtl_bpark_put(_backBuf, &_i, _pkey);
    if (!_parent) nvmtl_vk_image_destroy(&_i);
    if (!_parent && _vramOn) { nvmtl_vk_buffer_destroy(&_vramBuf); _vramOn = NO; }
    if (_surfOwned && _surf) CFRelease(_surf);
    if (_subHeap) { NVMTLHeap *h = _subHeap; NSUInteger c = _subCharge; _subHeap = nil; _subCharge = 0;
                    if (_subPlaced && c) { _subPlaced = NO; [h nvmtlGiveRange:_heapOffset length:c]; }
                    [h nvmtlReleaseSubAllocation:c]; }
}
- (BOOL)nvmtlSurfaceCopyable {
    size_t rowBytes = (size_t)_i.w * _i.bpp;
    size_t surfaceBytes = IOSurfaceGetWidthOfPlane(_surf, (uint32_t)_plane)
                        * IOSurfaceGetBytesPerElementOfPlane(_surf, (uint32_t)_plane);
    if (_i.bpp && rowBytes <= surfaceBytes
        && IOSurfaceGetHeightOfPlane(_surf, (uint32_t)_plane) >= _i.h
        && IOSurfaceGetBytesPerRowOfPlane(_surf, (uint32_t)_plane) >= rowBytes) return YES;
    static int said; if (said++ < 4) nvlog("G13 surface %p plane %lu NOT synced: image %ux%u bpp %u, surface bytesPerElement %zu — its pixels stay on one side",
        (void *)_surf, (unsigned long)_plane, _i.w, _i.h, _i.bpp, IOSurfaceGetBytesPerElementOfPlane(_surf, (uint32_t)_plane));
    return NO;
}
- (void)nvmtlSurfaceIn {
    NVMTLTexture *r = _parent ? _parent : self;
    if (!r->_surf) return;
    const uint32_t vgen = r->_vramOn ? 0 : nvmtl_vram_gen(r->_surf, r->_plane);
    if (r->_surfSynced && IOSurfaceGetSeed(r->_surf) == r->_surfSeed && (r->_vramOn || vgen == r->_surfGen)) return;
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
    int rc = (base && row) ? nvmtl_vk_image_write(r->_q, &r->_i, base, row) : -1;
    if (!rc && r->_i.bpp == 4) { char tag[24]; snprintf(tag, sizeof tag, "in%u-", (unsigned)IOSurfaceGetID(r->_surf)); nvmtl_frame_dump(tag, base, row, r->_i.w, r->_i.h); }
    IOSurfaceUnlock(r->_surf, kIOSurfaceLockReadOnly, NULL);
    if (rc) { nvlog("G13 surface-in: upload FAILED (%ux%u) — retried at the next use", r->_i.w, r->_i.h); return; }
    r->_surfSeed = seed; r->_surfSynced = YES; r->_surfGen = vgen;
    g_surfin_copy++; if (r->_vramOn) nvmtl_vram_tag_set(r->_surf, r->_plane, seed);
    static unsigned n; if (n++ < 6 || n % 2000 == 0) nvlog("G13 surface-in #%u: %ux%u surface %p seed %u uploaded", n, r->_i.w, r->_i.h, (void *)r->_surf, seed);
}
- (BOOL)nvmtlSurfaceOut {
    NVMTLTexture *r = _parent ? _parent : self;
    if (!r->_surf) return YES;
    if (![r nvmtlSurfaceCopyable]) return NO;
    if (r->_vramOn && !nvmtl_vk_surface_dirty((uint32_t)IOSurfaceGetID(r->_surf), (uint32_t)r->_plane)) {
        uint32_t seed = IOSurfaceGetSeed(r->_surf);
        r->_surfSeed = seed; r->_surfSynced = YES;
        r->_surfGen = nvmtl_vram_tag_bump(r->_surf, r->_plane, seed);
        static unsigned nd; if (nd++ < 6 || nd % 5000 == 0) nvlog("surface-out -> dirty, no copy (%ux%u surface %u seed %u, #%u)", r->_i.w, r->_i.h, (unsigned)IOSurfaceGetID(r->_surf), seed, nd);
        return YES;
    }
    uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    uint32_t lockopt = nvmtl_noseed() ? kIOSurfaceLockReadOnly : 0;
    if (IOSurfaceLock(r->_surf, lockopt, NULL) != kIOReturnSuccess) { nvlog("G13 surface-out: IOSurfaceLock FAILED"); return NO; }
    void *base = IOSurfaceGetBaseAddressOfPlane(r->_surf, (uint32_t)r->_plane);
    size_t row = IOSurfaceGetBytesPerRowOfPlane(r->_surf, (uint32_t)r->_plane);
    int rc = (base && row) ? nvmtl_vk_image_read(r->_q, &r->_i, base, row) : -1;
    if (!rc && r->_i.bpp == 4) { char tag[24]; snprintf(tag, sizeof tag, "out%u-", (unsigned)IOSurfaceGetID(r->_surf)); nvmtl_frame_dump(tag, base, row, r->_i.w, r->_i.h); }
    uint32_t seed = 0; IOReturn unlocked = IOSurfaceUnlock(r->_surf, lockopt, &seed); if (lockopt) seed = IOSurfaceGetSeed(r->_surf);
    if (rc || unlocked != kIOReturnSuccess) { nvlog("G13 surface-out: readback FAILED (%ux%u)", r->_i.w, r->_i.h); return NO; }
    r->_surfSeed = seed; r->_surfSynced = YES;
    r->_surfGen = r->_vramOn ? r->_surfGen : nvmtl_vram_gen(r->_surf, r->_plane);
    if (r->_vramOn && !lockopt) nvmtl_vram_tag_set(r->_surf, r->_plane, seed);
    static unsigned n; if (n++ < 6 || n % 2000 == 0) nvlog("G13 surface-out #%u: %ux%u surface %p seed %u, %.2f ms", n, r->_i.w, r->_i.h,
        (void *)r->_surf, seed, (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6);
    return YES;
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
- (void)getBytes:(void *)bytes bytesPerRow:(NSUInteger)row fromRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level {
    if (level >= [self mipmapLevelCount]) { nvlog("getBytes: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    [self nvmtlSurfaceIn];
    nvmtl_census("getBytes", (uint32_t)region.size.width, (uint32_t)region.size.height, _surf != NULL);
    if (nvmtl_vk_image_read_region_level_layer(_q, [self nvi], (uint8_t *)bytes, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                               (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, _baseSlice))
        nvlog("texture getBytes: readback FAILED");
}
- (void)getBytes:(void *)bytes bytesPerRow:(NSUInteger)row bytesPerImage:(NSUInteger)img
      fromRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level slice:(NSUInteger)slice {
    if (_tbView) {
        uint8_t *src = (uint8_t *)[(id<MTLBuffer>)_backBuf contents];
        if (!src || region.origin.x + region.size.width > _i.w) { nvlog("getBytes: texture buffer is not CPU-visible or the region is outside it"); return; }
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
- (void)replaceRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level withBytes:(const void *)bytes bytesPerRow:(NSUInteger)row {
    if (level >= [self mipmapLevelCount]) { nvlog("replaceRegion: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    if (region.size.depth > 1 || region.origin.z) { [self replaceRegion:region mipmapLevel:level slice:0 withBytes:bytes bytesPerRow:row bytesPerImage:row * region.size.height]; return; }
    { int was = nvtrace_scope([self nvi]->w, [self nvi]->h);
      if (nvmtl_trace_on()) { unsigned nz, nn; size_t valid = (size_t)region.size.width * [self nvi]->bpp; if (row && valid > row) valid = row;
        nvmtl_nonzero(bytes, row, valid, (uint32_t)region.size.height, &nz, &nn);
        nvtrace("REPLACE %lux%lu at %lu,%lu level %lu row %lu -> tex %ux%u surf %u | nonzero %u/%u sampled bytes", (unsigned long)region.size.width,
            (unsigned long)region.size.height, (unsigned long)region.origin.x, (unsigned long)region.origin.y, (unsigned long)level, (unsigned long)row,
            [self nvi]->w, [self nvi]->h, nvmtl_surf_id(self), nz, nn); }
      g_trace_mute = was; }
    [self nvmtlSurfaceIn];
    if (nvmtl_vk_image_write_region_level_layer(_q, [self nvi], (const uint8_t *)bytes, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                                (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, _baseSlice))
        nvlog("replaceRegion: upload FAILED");
    else [self nvmtlSurfaceOut];
}
- (void)replaceRegion:(MTLRegion)region mipmapLevel:(NSUInteger)level slice:(NSUInteger)slice
            withBytes:(const void *)bytes bytesPerRow:(NSUInteger)row bytesPerImage:(NSUInteger)img {
    if (_tbView) {
        id<MTLBuffer> bb = (id<MTLBuffer>)_backBuf; uint8_t *dst = (uint8_t *)[bb contents];
        if (!dst || region.origin.x + region.size.width > _i.w) { nvlog("replaceRegion: texture buffer is not CPU-visible or the region is outside it"); return; }
        const NSUInteger at = _backOff + region.origin.x * _i.bpp, len = region.size.width * _i.bpp; memcpy(dst + at, bytes, len);
        if (bb.storageMode == MTLStorageModeManaged) [bb didModifyRange:NSMakeRange(at, len)];
        return; }
    if (level >= [self mipmapLevelCount]) { nvlog("replaceRegion: mip level %lu — this texture has %lu", (unsigned long)level, (unsigned long)[self mipmapLevelCount]); return; }
    [self nvmtlSurfaceIn];
    nvk_image *im = [self nvi]; BOOL vol = im->mtl_type == 7; NSUInteger nz = vol ? (region.size.depth ? region.size.depth : 1) : 1;
    if (!vol && (region.size.depth > 1 || region.origin.z)) { nvlog("replaceRegion:slice: depth %lu@z%lu on a non-3D texture — REFUSED", (unsigned long)region.size.depth, (unsigned long)region.origin.z); return; }
    NSUInteger per = img ? img : row * region.size.height; int rc = 0;
    for (NSUInteger zi = 0; zi < nz && !rc; zi++)
        rc = nvmtl_vk_image_write_region_level_layer(_q, im, (const uint8_t *)bytes + zi * per, row, (uint32_t)region.origin.x, (uint32_t)region.origin.y,
                                                     (uint32_t)region.size.width, (uint32_t)region.size.height, (uint32_t)level + _baseLevel, (uint32_t)(vol ? region.origin.z + zi : slice + _baseSlice));
    if (rc) nvlog("replaceRegion:slice: upload FAILED");
    else [self nvmtlSurfaceOut];
}
- (id<MTLTexture>)nvmtlViewWithFormat:(MTLPixelFormat)f levels:(NSRange)levels {
    if (!levels.length || levels.location >= self.mipmapLevelCount || levels.length > self.mipmapLevelCount - levels.location) return nil;
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
    if (vkf != [v nvi]->fmt && bpp != [v nvi]->bpp) {
        nvlog("newTextureView: pixel format %lu is %u B per element, its texture's is %u B - nil", (unsigned long)f, bpp, [v nvi]->bpp); return nil; }
    uint32_t nslices = (uint32_t)slices.length;
    if (nvmtl_vk_image_view_create_range([v nvi], vkf, a8, v->_baseLevel, v->_mips, v->_baseSlice, nslices, (uint32_t)tt, &v->_ownView)) { nvlog("newTextureView: typed view FAILED"); return nil; }
    v->_stex = [v nvi]->storage && !a8 && nvmtl_vk_format_is_storage(vkf);
    return v;
}
- (id<MTLTexture>)newTextureViewWithPixelFormat:(MTLPixelFormat)f textureType:(MTLTextureType)tt
                                         levels:(NSRange)levels slices:(NSRange)slices
                                        swizzle:(MTLTextureSwizzleChannels)sw {
    NVMTLTexture *v = (NVMTLTexture *)[self newTextureViewWithPixelFormat:f textureType:tt levels:levels slices:slices];
    if (!v || (sw.red == MTLTextureSwizzleRed && sw.green == MTLTextureSwizzleGreen && sw.blue == MTLTextureSwizzleBlue && sw.alpha == MTLTextureSwizzleAlpha)) return v;
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
NSData *nvmtl_translate_with_fc(NSString *air, NSString *stage, const uint32_t *idx, const uint32_t *sizes,
                                const uint8_t *const *payloads, size_t n, NSString **err);
NSData *nvmtl_translate_with_fc_cached(NSString *air, NSString *stage, NSString *name, NSDictionary *constants,
                                       const uint32_t *idx, const uint32_t *sizes,
                                       const uint8_t *const *payloads, size_t n, NSString **err);

#pragma mark - library / function
id gNVMTLMainDevice = nil;

extern NSData *nvmtl_translate_samplers(NVMTLFunction *, NSDictionary *, NSString **);
extern NSData *nvmtl_translate_variant(NVMTLFunction *, NSDictionary *, NSDictionary *, NSString **);
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
    NSArray *fields = nvmtl_argbuf_fields(self, index);
    if (!fields.count) { nvlog("newArgumentEncoderWithBufferIndex:%lu — %s declares no argument buffer there", (unsigned long)index, _fname.UTF8String); return nil; }
    NVMTLArgumentEncoder *e = [NVMTLArgumentEncoder new];
    e->_fields = fields; e->_fieldByIndex = nvmtl_index_argument_fields(fields);
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
- (id<MTLArgumentEncoder>)newArgumentEncoderWithBufferIndex:(NSUInteger)index reflection:(MTLAutoreleasedArgument *)reflection {
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
- (id)arrayType { return nil; }
- (id)textureReferenceType { return _texRef; }
- (id)pointerType { return _ptr; }
- (NSString *)description { return [NSString stringWithFormat:@"<member %@ id %lu off %lu type %lu>", _name, (unsigned long)_argIndex, (unsigned long)_offset, (unsigned long)_dataType]; }
@end
@implementation NVMTLArgument
- (NSString *)name { return _name ?: @""; }
- (MTLArgumentType)type { return _type; }
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
@implementation NVMTLBufferArgument @end
@interface NVMTLTextureArgument : NVMTLArgument <MTLTextureBinding> @end
@implementation NVMTLTextureArgument @end
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
static BOOL nvmtl_binding_active(NSDictionary *b) { id acc = b[@"access"]; return !([acc isKindOfClass:[NSString class]] && [acc isEqualToString:@"Unused"]); }
static NVMTLArgument *nvmtl_build_buffer_argument(NVMTLFunction *fn, NSUInteger index, NSDictionary *binding, NSDictionary *refl, NSDictionary *names) {
    NVMTLArgument *a = [NVMTLBufferArgument new];
    a->_type = MTLArgumentTypeBuffer; a->_index = index; a->_active = nvmtl_binding_active(binding); a->_arrayLength = 1; a->_bufAlign = 16;
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
            NSUInteger e = m->_offset + [am[@"size"] unsignedLongValue]; if (e > end) end = e;
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
                NVMTLArgument *a = [NVMTLTextureArgument new]; a->_type = MTLArgumentTypeTexture; a->_index = idx; a->_active = nvmtl_binding_active(b);
                a->_name = names[@"args"][[NSString stringWithFormat:@"texture:%lu", (unsigned long)idx]] ?: [NSString stringWithFormat:@"texture%lu", (unsigned long)idx];
                a->_access = [kind isEqualToString:@"StorageImage"] ? MTLBindingAccessReadWrite : nvmtl_access_for(b[@"access"]);
                a->_texType = nvmtl_textype_for_shape(b[@"texture_shape"]); a->_texDataType = MTLDataTypeFloat;
                id loc = b[@"descriptor"]; a->_arrayLength = [loc isKindOfClass:[NSDictionary class]] && [loc[@"count"] unsignedLongValue] > 1 ? [loc[@"count"] unsignedLongValue] : 1;
                [out addObject:a];
            } else if ([kind isEqualToString:@"Sampler"]) {
                NVMTLArgument *a = [NVMTLArgument new]; a->_type = MTLArgumentTypeSampler; a->_index = idx; a->_active = nvmtl_binding_active(b); a->_arrayLength = 1;
                a->_name = names[@"args"][[NSString stringWithFormat:@"sampler:%lu", (unsigned long)idx]] ?: [NSString stringWithFormat:@"sampler%lu", (unsigned long)idx];
                [out addObject:a];
            }
        }
        [out sortUsingComparator:^NSComparisonResult(NVMTLArgument *x, NVMTLArgument *y) {
            if (x->_type != y->_type) return x->_type < y->_type ? NSOrderedAscending : NSOrderedDescending;
            return x->_index < y->_index ? NSOrderedAscending : x->_index > y->_index ? NSOrderedDescending : NSOrderedSame; }];
        NSMutableString *s = [NSMutableString new];
        for (NVMTLArgument *a in out) [s appendFormat:@" %@(%lu)=%@%@", a->_type == MTLArgumentTypeBuffer ? @"buffer" : a->_type == MTLArgumentTypeTexture ? @"texture" : @"sampler",
                                       (unsigned long)a->_index, a->_name, a->_bufStruct ? [NSString stringWithFormat:@"[%lu members, %lu B]", (unsigned long)a->_bufStruct->_members.count, (unsigned long)a->_bufSize] : @""];
        nvlog("REFL %s (%s): %lu argument(s)%s", fn->_fname.UTF8String ?: "?", fn->_stage.UTF8String ?: "?", (unsigned long)out.count, s.UTF8String);
        fn->_arguments = out;
        return out;
    }
}
NVMTLArgument *nvmtl_argument_for_buffer(NVMTLFunction *fn, NSUInteger index)
{
    for (NVMTLArgument *a in nvmtl_arguments_for_function(fn)) if (a->_type == MTLArgumentTypeBuffer && a->_index == index) return a;
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
        a->_type = MTLArgumentTypeBuffer; a->_index = bi; a->_active = [live containsIndex:bi]; a->_arrayLength = 1; a->_bufAlign = 4;
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
    return end ?: 8;
}
- (NSUInteger)alignment { return 8; }
- (void)setArgumentBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off { _dst = (NVMTLBuffer *)b; _dstOffset = off; nvtrace("AE argbuf %p off %lu (%lu fields)", (__bridge void *)b, (unsigned long)off, (unsigned long)_fields.count); }
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
    NSString *why = nil;
    NSData *spv = nvmtl_compile_library_entry(air, ll, stage, name, &why);
    if (!spv) {
        if (error) *error = [NSError errorWithDomain:@"NVMTL" code:3
                                           userInfo:@{NSLocalizedDescriptionKey:why ?: @"entry translation failed"}];
        return nil;
    }

    @synchronized(self) {
        NSData *won = _compiledFns[name];
        if (won) return won;
        if (!_compiledFns) _compiledFns = [NSMutableDictionary new];
        _compiledFns[name] = spv;
    }
    return spv;
}
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (id<MTLFunction>)newFunctionWithName:(NSString *)name {
    NSData *spv = [_fns objectForKey:name];
    if (!spv && !_airs[name]) { nvlog("library: no function \"%s\" (this library holds %lu, externs %lu)", name.UTF8String, (unsigned long)(_fns.count + _airs.count), (unsigned long)_externs.count); return nil; }
    if (spv && ([_stages[name] isEqualToString:@"mesh"] || [_stages[name] isEqualToString:@"object"])) {
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = _stages[name];
        f->_air = _airs[name]; f->_lib = self; return f;
    }
    if (spv && [_airs[name] rangeOfString:@"@air.dyld_lib_table"].location != NSNotFound) {
        nvlog("library: \"%s\" was compiled against a dynamic library - DEFERRED to pipeline creation", name.UTF8String);
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"");
        f->_air = _airs[name]; f->_lib = self; f->_needsLink = YES; return f;
    }
    if (spv && [_airs[name] rangeOfString:@"!\"air.intersection_function_table\""].location != NSNotFound) {
        nvlog("library: \"%s\" takes an intersection function table - DEFERRED to pipeline creation", name.UTF8String);
        NVMTLFunction *f = [NVMTLFunction new]; f->_spirv = nil; f->_fname = name; f->_stage = (_stages[name] ?: @"");
        f->_air = _airs[name]; f->_lib = self; f->_needsLink = YES; return f;
    }
    if (spv) { NSError *te = nil; NSData *es = [self nvmtlEntrySpirv:name error:&te];
        if (!es && _airs[name] && ([te.localizedDescription rangeOfString:@"Metal visible function table"].location != NSNotFound
                                   || [te.localizedDescription rangeOfString:@"visible function reference"].location != NSNotFound)) {
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
    NSData *spec = nvmtl_translate_with_fc_cached(_airs[name], _stages[name], name, constants, idx, sz, pay, n, &e);
    if (!spec && ([e rangeOfString:@"visible function reference"].location != NSNotFound || [e rangeOfString:@"Metal visible function table"].location != NSNotFound) && _airs[name]) {
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
- (void)dealloc { nvmtl_vk_pipeline_destroy(&_p); }
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSUInteger)maxTotalThreadsPerThreadgroup { return 1024; }
- (NSUInteger)threadExecutionWidth { return 32; }
- (BOOL)threadgroupSizeMatchesTileSize { return NO; }
- (NSString *)label { return _descriptor.label; }
- (BOOL)supportIndirectCommandBuffers { return _descriptor.supportIndirectCommandBuffers; }
- (MTLResourceID)gpuResourceID {
    MTLResourceID r; r._impl = _descriptor.supportIndirectCommandBuffers ? nvmtl_resid_pipe(self, &_resid) : 0; return r; }
- (NSUInteger)imageblockSampleLength { return 0; }
- (NSUInteger)imageblockMemoryLengthForDimensions:(MTLSize)d { return 0; }
- (MTLShaderValidation)shaderValidation { return MTLShaderValidationDisabled; }
@end

#pragma mark - encoder
static void nvmtl_retain_resource(NVMTLCommandBuffer *cb, id resource) {
    if (!resource) return;
    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    [cb->_resources addObject:resource];
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
@implementation NVMTLRenderCommandEncoder
- (NSUInteger)tileWidth { return 0; }
- (NSUInteger)tileHeight { return 0; }
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
- (BOOL)nvmtlBindSamplerPipeline {
    if (!_ps) return NO;
    NVMTLRenderPipelineState *selected=_ps;
    if(_vertexSamplers.count || _fragmentSamplers.count) {
        NSDictionary *vS=nvmtl_pixel_sampler_norm(_vertexSamplers),*fS=nvmtl_pixel_sampler_norm(_fragmentSamplers);
        NSData *key=[NSJSONSerialization dataWithJSONObject:@[vS,fS] options:NSJSONWritingSortedKeys error:NULL];
        @synchronized(_ps) {
            if(!_ps->_samplerVariants) _ps->_samplerVariants=[NSMutableDictionary new];
            id cached=_ps->_samplerVariants[key];
            if(cached==[NSNull null]) return NO;
            selected=cached;
            if(!selected) {
                NVMTLFunction *vf=_ps->_linkedVert?:(id)_ps->_descriptor.vertexFunction,*ff=_ps->_linkedFrag?:(id)_ps->_descriptor.fragmentFunction;
                NSString *why=nil;
                NSString *gk=nvmtl_sampvar_key(_ps->_descriptor,vf,ff,key); selected=nvmtl_sampvar_get(gk);
                NSData *v=selected?nil:nvmtl_translate_samplers(vf,vS,&why),*f=(selected||!v)?nil:(ff?nvmtl_translate_samplers(ff,fS,&why):[NSData data]);
                if(v && f) {
                    NVMTLFunction *vs=[NVMTLFunction new],*fs=ff?[NVMTLFunction new]:nil;
                    vs->_spirv=v;vs->_fname=vf->_fname;vs->_stage=vf->_stage;
                    if(fs){fs->_spirv=f;fs->_fname=ff->_fname;fs->_stage=ff->_stage;}
                    MTLRenderPipelineDescriptor *desc=[_ps->_descriptor copy];desc.vertexFunction=vs;desc.fragmentFunction=fs;
                    NSError *error=nil;selected=(id)[(id<MTLDevice>)gNVMTLMainDevice newRenderPipelineStateWithDescriptor:desc error:&error];
                    if(!selected)why=error.description;
                    else nvmtl_sampvar_put(gk,selected,vf,ff);
                }
                if(!selected){nvlog("pixel sampler specialization REFUSED %s: %s",_ps->_fname.UTF8String,why.UTF8String);_ps->_samplerVariants[key]=[NSNull null];return NO;}
                _ps->_samplerVariants[key]=selected;
                nvlog("pixel sampler variant built: %s / %s",_ps->_vname.UTF8String,_ps->_fname.UTF8String);
            }
        }
    }
    if(!_cb->_resources)_cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:selected];
    nvmtl_vk_cmd_bind_pipeline(&_cb->_c,&selected->_p);
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX,   (NVMTLFunction *)_ps->_descriptor.vertexFunction,   _vbufAt);
    nvmtl_bind_embedded(_cb, NVMTL_SET_FRAGMENT, (NVMTLFunction *)_ps->_descriptor.fragmentFunction, _fbufAt);
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
    memset(&_pass, 0, sizeof _pass); cb->_c.depth_view = NULL;
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
        uint32_t zslice = [t nvi]->mtl_type == 7 ? (uint32_t)ca.depthPlane : (uint32_t)ca.slice;
        if (ca.depthPlane && [t nvi]->mtl_type != 7) { static int said;
            if (said++ < 4) nvlog("encoder: apps2 colour %u sets depthPlane %lu on a texture of type %u, which is not 3D — depthPlane IGNORED, its slice %lu is used", i, (unsigned long)ca.depthPlane, [t nvi]->mtl_type, (unsigned long)ca.slice); }
        int ta8 = 0; uint32_t tvk = nvmtl_tex_attach_vkfmt(t, &ta8);
        if (nvmtl_vk_cmd_attachment_view_n(&cb->_c, [t nvi], 0, t->_baseLevel + (uint32_t)ca.level, t->_baseSlice + zslice, NL, tvk, ta8, &_pass.colview[i], &cw, &ch)) return NO;
        _pass.col[i] = [t nvi]; _pass.colw[i] = cw; _pass.colh[i] = ch;
        _pass.col_level[i] = t->_baseLevel + (uint32_t)ca.level; _pass.col_layer[i] = t->_baseSlice + zslice;
        _pass.colfmt[i] = tvk;
        MTLClearColor cc = ca.clearColor;
        _pass.clear[i][0] = (float)cc.red; _pass.clear[i][1] = (float)cc.green; _pass.clear[i][2] = (float)cc.blue; _pass.clear[i][3] = (float)cc.alpha;
        if (nvmtl_load_keeps(ca.loadAction)) load |= NVMTL_LOAD_COLOUR(i);
        if (cw < w) w = cw;
        if (ch < h) h = ch;
        any = YES;
        if (i) {
            [cb->_resources addObject:t];
            NVMTLTexture *sr = t->_parent ? t->_parent : t;
            if (sr->_surf) {
                if (ca.loadAction == MTLLoadActionClear) { sr->_surfSeed = IOSurfaceGetSeed(sr->_surf); sr->_surfSynced = YES; sr->_surfGen = sr->_vramOn ? sr->_surfGen : nvmtl_vram_gen(sr->_surf, sr->_plane); }
                else [sr nvmtlSurfaceIn];
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
    if (!any) { nvlog("encoder: apps2 a render pass with no attachments (%lux%lu) is not carried yet — nothing is drawn",
                      (unsigned long)d.renderTargetWidth, (unsigned long)d.renderTargetHeight); return NO; }
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
    if (!_ps->_p.has_stencil) return;
    static const uint32_t off[6] = { 7, 0, 0, 0, 0xff, 0xff };
    nvmtl_vk_cmd_set_stencil(&_cb->_c, _ds && _ds->_stencil, _ds ? _ds->_sf : off, _ds ? _ds->_sb : off);
    if (!_hasRef) { nvmtl_vk_cmd_set_stencil_ref(&_cb->_c, 0, 0); _hasRef = YES; }
}
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)ps {
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
    if (_begun && !nvmtl_vk_pipeline_compatible(&_passSig, &p->_p)) {
        nvlog("encoder: incompatible render pipeline refused before changing draw state"); return;
    }
    _ps = p;
    nvmtl_retain_resource(_cb, p);
    if (!_begun) {
        if (_refused) return;
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
    if (!_vbufAt) _vbufAt = [NSMutableDictionary new];
    _vbufAt[@(i)] = @[nb, @(off)];
    nvtrace_floats("VB", (unsigned long)i, (unsigned long)off, nb->_b.map, 24);
    if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, off)) nvlog("setVertexBuffer: bind FAILED");
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
    if (!_fbufAt) _fbufAt = [NSMutableDictionary new];
    _fbufAt[@(i)] = @[nb, @(off)];
    nvtrace_floats("FB", (unsigned long)i, (unsigned long)off, nb->_b.map, 16);
    if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, &nb->_b, off)) nvlog("setFragmentBuffer: bind FAILED");
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
    if (nvmtl_vk_bind_buffer(&_cb->_c, set, (uint32_t)i, &nb->_b, roff)) nvlog("setBytes: bind FAILED");
    nvtrace_floats(set == NVMTL_SET_VERTEX ? "VBYTES" : "FBYTES", (unsigned long)i, 0, len >= 4 ? bytes : NULL, (int)(len / 4 > 24 ? 24 : len / 4));
    if (!ring) {
        if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
        [_cb->_scratch addObject:nb];
    }
    if (set == NVMTL_SET_VERTEX)        { if (!_vbufAt) _vbufAt = [NSMutableDictionary new]; _vbufAt[@(i)] = @[nb, @(roff)]; }
    else if (set == NVMTL_SET_FRAGMENT) { if (!_fbufAt) _fbufAt = [NSMutableDictionary new]; _fbufAt[@(i)] = @[nb, @(roff)]; }
}
- (void)setVertexBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlBindBytes:b length:l atIndex:i set:NVMTL_SET_VERTEX]; }
- (void)setFragmentBytes:(const void *)b length:(NSUInteger)l atIndex:(NSUInteger)i { [self nvmtlBindBytes:b length:l atIndex:i set:NVMTL_SET_FRAGMENT]; }
- (void)setVertexTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTexture *nt = (NVMTLTexture *)t;
    if (!nt) { nvlog("setVertexTexture: nil at index %lu", (unsigned long)i); return; }
    nvmtl_retain_resource(_cb, nt);
    [nt nvmtlSurfaceIn];
    if (nt->_tbView) nvmtl_vk_bind_texel_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_vk_bind_texture_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, [nt nvview])) nvlog("setVertexTexture: bind FAILED");
    nvmtl_vk_bind_storage_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, nt->_stex ? [nt nvview] : NULL);
    }
}
- (void)setFragmentSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)s;
    if (i >= 16 || nvmtl_vk_bind_sampler(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, ns ? &ns->_s : NULL)) {
        nvlog("setFragmentSamplerState: invalid index or bind failure"); return;
    }
    if (!_fragmentSamplers) _fragmentSamplers=[NSMutableDictionary new];
    NSString *key=[@(i) stringValue];
    if(ns && ns->_pixelState) _fragmentSamplers[key]=ns->_pixelState;else [_fragmentSamplers removeObjectForKey:key];
    nvtrace("FRAGMENT SAMPLER[%lu] pixel=%d bound=%d",(unsigned long)i,ns && ns->_pixelState!=nil,ns!=nil);
    if(ns) { if (!_cb->_resources) _cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:ns]; }
}
- (void)setVertexSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)s;
    if (i >= 16 || nvmtl_vk_bind_sampler(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, ns ? &ns->_s : NULL)) {
        nvlog("setVertexSamplerState: invalid index or bind failure"); return;
    }
    if (!_vertexSamplers) _vertexSamplers=[NSMutableDictionary new];
    NSString *key=[@(i) stringValue];
    if(ns && ns->_pixelState) _vertexSamplers[key]=ns->_pixelState;else [_vertexSamplers removeObjectForKey:key];
    nvtrace("VERTEX SAMPLER[%lu] pixel=%d bound=%d",(unsigned long)i,ns && ns->_pixelState!=nil,ns!=nil);
    if(ns) { if (!_cb->_resources) _cb->_resources=[NVMTLResSet new];[_cb->_resources addObject:ns]; }
}
- (void)setFragmentTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTexture *nt = (NVMTLTexture *)t;
    if (!nt) { nvlog("setFragmentTexture: nil at index %lu", (unsigned long)i); return; }
    nvmtl_retain_resource(_cb, nt);
    nvmtl_surface_cpu_dump(nt->_parent ? nt->_parent : nt);
    [nt nvmtlSurfaceIn];
    { NVMTLTexture *rt = nt->_parent ? nt->_parent : nt; nvtrace("FTEX[%lu] %ux%u surf %u view %d", (unsigned long)i, [nt nvi]->w, [nt nvi]->h,
        rt->_surf ? (unsigned)IOSurfaceGetID(rt->_surf) : 0u, nt->_parent != nil); }
    if (nt->_tbView) nvmtl_vk_bind_texel_view(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_vk_bind_texture_view(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, [nt nvview])) nvlog("setFragmentTexture: bind FAILED");
    nvmtl_vk_bind_storage_view(&_cb->_c, NVMTL_SET_FRAGMENT, (uint32_t)i, nt->_stex ? [nt nvview] : NULL);
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
    [self nvmtlCloseVisibility];
    if (_begun) nvmtl_vk_cmd_end_render(&_cb->_c);
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
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r {
    NVMTLIndirectCommandBuffer *b = (NVMTLIndirectCommandBuffer *)icb;
    if (!b) { nvlog("executeCommandsInBuffer: nil"); return; }
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
- (void)drawIndexedPrimitives:(MTLPrimitiveType)type indexCount:(NSUInteger)count indexType:(MTLIndexType)it
                  indexBuffer:(id<MTLBuffer>)ib indexBufferOffset:(NSUInteger)off
                instanceCount:(NSUInteger)instances baseVertex:(NSInteger)baseVertex baseInstance:(NSUInteger)baseInstance {
    if (!_begun) { nvlog("drawIndexedPrimitives with no pipeline set"); return; }
    NVMTLBuffer *b = (NVMTLBuffer *)ib;
    if (!b) { nvlog("drawIndexedPrimitives with no index buffer"); return; }
    nvmtl_retain_resource(_cb, b);
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
    { NSArray *b = _vbufAt[@(i)]; if (b) _vbufAt[@(i)] = @[b[0], @(off)]; }
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
    { NSArray *b = _fbufAt[@(i)]; if (b) _fbufAt[@(i)] = @[b[0], @(off)]; }
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
    NSString *key=[@(i) stringValue];
    if(_fragmentSamplers[key]) {
        NSMutableDictionary *state=[_fragmentSamplers[key] mutableCopy];
        state[@"lod_min_clamp"]=@(lo);state[@"lod_max_clamp"]=@(hi);_fragmentSamplers[key]=state;
    }
}
- (void)setVertexSamplerState:(id<MTLSamplerState>)ss lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i {
    [self setVertexSamplerState:ss atIndex:i];
    NSString *key=[@(i) stringValue];
    if(_vertexSamplers[key]) {
        NSMutableDictionary *state=[_vertexSamplers[key] mutableCopy];
        state[@"lod_min_clamp"]=@(lo);state[@"lod_max_clamp"]=@(hi);_vertexSamplers[key]=state;
    }
}
- (void)setFragmentSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for(NSUInteger k=0;k<r.length;k++) [self setFragmentSamplerState:ss[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location+k];
}
- (void)setVertexSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])ss lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for(NSUInteger k=0;k<r.length;k++) [self setVertexSamplerState:ss[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location+k];
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u { nvmtl_retain_resource(_cb, r); }
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n {}
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n stages:(MTLRenderStages)st { nvtrace("USEHEAPS %lu stages %lu", (unsigned long)n, (unsigned long)st); }
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u stages:(MTLRenderStages)st { nvmtl_retain_resource(_cb, r); nvtrace("USERES %s %p usage %lu stages %lu", object_getClassName(r), (__bridge void *)r, (unsigned long)u, (unsigned long)st); }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i=0;i<n;i++) nvmtl_retain_resource(_cb,r[i]); }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u stages:(MTLRenderStages)st { for (NSUInteger i=0;i<n;i++) nvmtl_retain_resource(_cb,r[i]); nvtrace("USERES x%lu usage %lu stages %lu", (unsigned long)n, (unsigned long)u, (unsigned long)st); }
- (void)useHeap:(id<MTLHeap>)h {}
- (void)useHeap:(id<MTLHeap>)h stages:(MTLRenderStages)st {}
- (void)sampleCountersInBuffer:(id)sb atSampleIndex:(NSUInteger)i withBarrier:(BOOL)b { nvmtl_sample_counter(_cb, sb, i, self, b); }
- (void)textureBarrier { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)updateFence:(id<MTLFence>)f afterStages:(MTLRenderStages)st {}
- (void)waitForFence:(id<MTLFence>)f beforeStages:(MTLRenderStages)st { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)memoryBarrierWithScope:(MTLBarrierScope)scope afterStages:(MTLRenderStages)a beforeStages:(MTLRenderStages)b { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)memoryBarrierWithResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n afterStages:(MTLRenderStages)a beforeStages:(MTLRenderStages)b { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)drawPrimitives:(MTLPrimitiveType)type indirectBuffer:(id<MTLBuffer>)buf indirectBufferOffset:(NSUInteger)off {
    NVMTLBuffer *nb = (NVMTLBuffer *)buf;
    if (!nb) { nvlog("drawPrimitives:indirectBuffer: nil"); return; }
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
- (void)pushDebugGroup:(NSString *)g {}
- (void)popDebugGroup {}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
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
        *out = p->img; *off = p->off; *len = p->len;
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
    if (parked) { memset(img, 0, sizeof *img); atomic_fetch_add(&gHTParked, 1); }
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
void nvmtl_heap_back(NVMTLHeap *h, NSUInteger size, int shared) {
    if (nvmtl_vk_heap_create(size, shared, &h->_hm)) {
        nvlog("heap alias: an automatic %s heap of %lu bytes got no memory of its own - its resources take the legacy fresh path",
              shared ? "shared" : "private", (unsigned long)size);
        return;
    }
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
    if (nvmtl_vk_buffer_create_placed(len, &_hm, off, &b->_b)) {
        const char *why = nvmtl_heap_give(self, off, charge);
        nvlog("heap alias: bind of %lu at %lu FAILED - legacy path%s%s", (unsigned long)len, (unsigned long)off, why ? "; range kept out: " : "", why ? why : "");
        return nil;
    }
    b->_subHeap = self; b->_subCharge = charge; b->_subPlaced = YES; b->_heapOffset = off; _used += charge;
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
                  nvmtl_vk_heap_destroy(&_hm); free(_fr); _fr = NULL; }
- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)len options:(MTLResourceOptions)opt offset:(NSUInteger)off {
    if (_type != MTLHeapTypePlacement) { nvlog("heap newBufferWithLength:offset: on a non-placement heap"); return nil; }
    NVMTLBuffer *b = [NVMTLBuffer new];
    b->_storage = (MTLStorageMode)((opt >> MTLResourceStorageModeShift) & 0xF);
    if (nvmtl_vk_buffer_create_placed(len, &_hm, off, &b->_b)) { nvlog("heap newBufferWithLength:%lu offset:%lu FAILED", (unsigned long)len, (unsigned long)off); return nil; }
    b->_heap = self; b->_heapOffset = off; if (off + len > _used) _used = off + len;
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
        nvlog("heap newTextureWithDescriptor: %lux%lu DEPTH offset:%lu -> %p", (unsigned long)d.width, (unsigned long)d.height, (unsigned long)off, (__bridge void *)t);
        return t;
    }
    uint32_t vkf = 0, bpp = 0; int a8 = 0;
    if (nvmtl_pixfmt_public(d.pixelFormat, &vkf, &bpp, &a8)) return nil;
    int wantStex = (d.usage & MTLTextureUsageShaderWrite) != 0;
    if (nvmtl_vk_image_create_typed_placed((uint32_t)d.width, (uint32_t)d.height, vkf, bpp, a8, (uint32_t)d.mipmapLevelCount, wantStex,
                                            (uint32_t)d.textureType, nvmtl_vk_layers_for_desc((uint32_t)d.textureType, (uint32_t)d.arrayLength, (uint32_t)d.depth), &_hm, off, &t->_i)) {
        nvlog("heap newTextureWithDescriptor: %lux%lu offset:%lu FAILED", (unsigned long)d.width, (unsigned long)d.height, (unsigned long)off); return nil; }
    t->_q = nvmtl_device_queue(); t->_stex = t->_i.storage != 0; t->_heap = self; t->_heapOffset = off;
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
    if (charge > [self maxAvailableSizeWithAlignment:0]) { nvlog("heap newBufferWithLength:%lu (charge %lu) — only %lu left of %lu", (unsigned long)len, (unsigned long)charge, (unsigned long)(_size > _used ? _size - _used : 0), (unsigned long)_size); return nil; }
    if (_backed) { id<MTLBuffer> pb = [self nvmtlPlacedBuffer:len options:opt charge:charge]; if (pb) return pb;
                   atomic_fetch_add(&gHAFallback, 1); nvmtl_heap_census("fallback"); }
    gZeroSkip = 1; id<MTLBuffer> b = [_dev newBufferWithLength:len options:opt]; gZeroSkip = 0;
    if (!b) return nil;
    if (![(id)b isKindOfClass:[NVMTLBuffer class]]) {
        static int said; if (said++ < 2) nvlog("heap sub-allocation is a %s, not ours — UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)b class]) UTF8String]);
        return b;
    }
    NVMTLBuffer *nb = (NVMTLBuffer *)b; nb->_subHeap = self; nb->_subCharge = charge; _used += charge;
    return b;
}
- (id<MTLAccelerationStructure>)newAccelerationStructureWithSize:(NSUInteger)size {
    NSUInteger charge = [_dev heapAccelerationStructureSizeAndAlignWithSize:size].size;
    if (charge < size) charge = size;
    if (charge > [self maxAvailableSizeWithAlignment:0]) { nvlog("heap newAccelerationStructureWithSize:%lu (charge %lu) - only %lu left of %lu", (unsigned long)size, (unsigned long)charge, (unsigned long)(_size > _used ? _size - _used : 0), (unsigned long)_size); return nil; }
    id<MTLAccelerationStructure> as = [_dev newAccelerationStructureWithSize:size];
    if (!as) return nil;
    if (![(id)as isKindOfClass:[NVMTLAccelerationStructure class]]) {
        static int said; if (said++ < 2) nvlog("heap structure is a %s, not ours - UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)as class]) UTF8String]);
        return as;
    }
    NVMTLAccelerationStructure *na = (NVMTLAccelerationStructure *)as; na->_subHeap = self; na->_subCharge = charge; _used += charge;
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
        if (nvmtl_vk_image_create_typed_placed(w, hh, vkf, bpp, a8, mips, wantStex, type, layers, &_hm, off, &t->_i)
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
    t->_q = nvmtl_device_queue(); t->_stex = t->_i.storage != 0;
    t->_subHeap = self; t->_subCharge = len; t->_subPlaced = YES; t->_heapOffset = off;
    t->_parkable = !noRecycle; memcpy(t->_pkey, key, sizeof key); t->_pkLen = len;
    _used += len;
    atomic_fetch_add(&gHAPlaced, 1); atomic_fetch_add(&gHALive, len);
    atomic_fetch_add(rec ? &gHTRecycled : &gHTPlaced, 1); nvmtl_heaptex_census(rec ? "recycled" : "placed");
    return t;
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d {
    if (!d) return nil;
    MTLSizeAndAlign sa = [_dev heapTextureSizeAndAlignWithDescriptor:d];
    NSUInteger charge = sa.size;
    if (_backed && charge) { id<MTLTexture> pt = [self nvmtlPlacedTexture:d charge:charge align:sa.align]; if (pt) return pt;
                             atomic_fetch_add(&gHTFallback, 1); nvmtl_heaptex_census("fallback"); }
    if (!charge) charge = d.width * d.height * 4;
    id<MTLTexture> t = [_dev newTextureWithDescriptor:d];
    if (!t) return nil;
    if (![(id)t isKindOfClass:[NVMTLTexture class]]) {
        static int said; if (said++ < 2) nvlog("heap sub-allocation is a %s, not ours — UNCHARGED, usedSize will under-report", [NSStringFromClass([(id)t class]) UTF8String]);
        return t;
    }
    NVMTLTexture *nt = (NVMTLTexture *)t; nt->_subHeap = self; nt->_subCharge = charge; _used += charge;
    return t;
}
@end

@implementation NVMTLDepthStencilState
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
- (NSUInteger)maxTotalThreadsPerThreadgroup { NSUInteger v = nvmtl_compute_maxt(), n = _attrT ?: _askT; return n && n < v ? n : v; }
- (NSUInteger)threadExecutionWidth { static NSUInteger v; if (!v) v = nvmtl_env_uint(@"NVMTL_TEW", 32); return v; }
- (NSUInteger)staticThreadgroupMemoryLength { return 0; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l {}
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
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = nvmtl_resid_pipe(self, &_resid); return r; }
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
    if (cb->_g79Owed) { nvmtl_vk_cmd_barrier(&cb->_c); cb->_g79Owed = NO; nvmtl_g79_count(G79C_OWEDPAID); }
    cb->_g79C2C = NO;
}
@interface NVMTLComputeCommandEncoder () {
@public
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
static id<MTLComputePipelineState> g_nvc_ps[7];
#define NVC_CMP 6
#define NVC_NVAR 5
static NSMutableDictionary *g_nvc_state;
static pthread_mutex_t g_nvc_lk = PTHREAD_MUTEX_INITIALIZER;
static _Atomic unsigned long g_nvc_ran, g_nvc_verified, g_nvc_refused, g_nvc_skips;
#define NVC_SKIP(why) do { const unsigned long k_ = ++g_nvc_skips; if (k_ <= 16) nvlog("nvconv: not taken - %s (%lu)", why, k_); return 0; } while (0)
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
            NSString *names[7] = { @"nvconv3_b", @"nvconv3_s", @"nvconv4_b", @"nvconv4_s", @"nvconv5_b", @"nvconv5_s", @"nvconv_cmp" };
            for (int i = 0; i < 7 && on; i++) {
                id<MTLFunction> f = [lib newFunctionWithName:names[i]];
                g_nvc_ps[i] = f ? [dev newComputePipelineStateWithFunction:f error:&err] : nil;
                if (!g_nvc_ps[i]) { on = 0; nvlog("nvconv: %s did not build (%s) - MPS's conv stays everywhere", names[i].UTF8String, err.description.UTF8String); }
            }
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
    for (uint32_t a = 0; a < nar; a++) if (kp[a * 20] | kp[a * 20 + 1] | kp[a * 20 + 2] | kp[a * 20 + 3]) NVC_SKIP("array offset");
    const uint32_t ob = (nar - 1) * 20;
    if (kp[4] != cin || kp[5] != sw || kp[6] != sh || kp[7] != batch || kp[8] != 1) NVC_SKIP("input descriptor");
    if (kp[24] != cout || kp[25] != cin || kp[26] != kw || kp[27] != kh || kp[28] != 1) NVC_SKIP("weights descriptor");
    if (kp[ob + 4] != cout || kp[ob + 5] != dw || kp[ob + 6] != dh || kp[ob + 7] != batch) NVC_SKIP("output sizes");
    if (kp[ob + 8] != 1 || (dw > 1 && kp[ob + 9] != cout) || (dh > 1 && kp[ob + 10] != cout * dw) || (batch > 1 && kp[ob + 11] != cout * dw * dh)) {
        char why[160]; snprintf(why, sizeof why, "output strides %u,%u,%u,%u for cout %u x %u x %u (sizes %u,%u,%u,%u)",
                                kp[ob + 8], kp[ob + 9], kp[ob + 10], kp[ob + 11], cout, dw, dh, kp[ob + 4], kp[ob + 5], kp[ob + 6], kp[ob + 7]);
        NVC_SKIP(why); }
    if (nar >= 4 && (kp[44] != cout || kp[48] != 1)) NVC_SKIP("bias descriptor");
    if (nar == 5 && (kp[64] != cout || kp[65] != dw || kp[66] != dh || kp[67] != batch || kp[68] != 1 || (dw > 1 && kp[69] != cout) || (dh > 1 && kp[70] != cout * dw)))
        NVC_SKIP("residual descriptor");
    NSArray *bi = e->_bufAt[@0], *bw = e->_bufAt[@1], *bo = e->_bufAt[@(nar - 1)], *bb = nar >= 4 ? e->_bufAt[@2] : nil, *br = nar == 5 ? e->_bufAt[@3] : nil;
    if (!bi || !bw || !bo || (nar >= 4 && !bb) || (nar == 5 && !br)) NVC_SKIP("buffer slot unbound");
    #define NVC_ROOM(arr, need) ([(id<MTLBuffer>)arr[0] length] >= [arr[1] unsignedLongValue] + (uint64_t)(need) * 4)
    const uint64_t nin = (uint64_t)(cin - 1) * kp[8] + (uint64_t)(sw - 1) * kp[9] + (uint64_t)(sh - 1) * kp[10] + (uint64_t)(batch - 1) * kp[11] + 1;
    const uint64_t nwt = (uint64_t)(cout - 1) * kp[28] + (uint64_t)(cin - 1) * kp[29] + (uint64_t)(kw - 1) * kp[30] + (uint64_t)(kh - 1) * kp[31] + 1;
    const uint64_t nout = (uint64_t)cout * dw * dh * batch;
    if (!NVC_ROOM(bi, nin) || !NVC_ROOM(bw, nwt) || !NVC_ROOM(bo, nout) || (bb && !NVC_ROOM(bb, cout)) || (br && !NVC_ROOM(br, nout)) || nout > 0xffffffffull) NVC_SKIP("buffer too short for f32");
    #undef NVC_ROOM
    const uint64_t M = (uint64_t)batch * dh * dw;
    const BOOL small = ((M + 127) / 128) * ((cout + 63) / 64) < 60;
    id<MTLComputePipelineState> ours = g_nvc_ps[(nar - 3) * 2 + (small ? 1 : 0)];
    const MTLSize og = small ? MTLSizeMake((M + 63) / 64, (cout + 31) / 32, 1) : MTLSizeMake((M + 127) / 128, (cout + 63) / 64, 1);
    NSMutableData *key = [NSMutableData dataWithBytes:&e->_ps length:sizeof(void *)];
    for (uint32_t a = 0; a < nar; a++) [key appendBytes:kp + a * 20 + 4 length:8 * 4];
    [key appendBytes:cp + 32 length:18 * 4];
    pthread_mutex_lock(&g_nvc_lk); NSNumber *st = g_nvc_state[key]; if (!st) g_nvc_state[key] = @0; pthread_mutex_unlock(&g_nvc_lk);
    if (st.intValue > 0) { nvmtl_nvconv_run(e, ours, og, (uint32_t)(st.intValue - 1)); g_nvc_ran++; return 1; }
    if (st) { static _Atomic unsigned long pend; if (st.intValue == 0 && ++pend <= 8) nvlog("nvconv: still pending (%lu)", (unsigned long)pend); return 0; }
    e->_nvcIn = YES; [e dispatchThreadgroups:grid threadsPerThreadgroup:tg]; e->_nvcIn = NO;
    id<MTLBuffer> outBuf = bo[0]; const NSUInteger outOff = [bo[1] unsignedLongValue];
    id<MTLBuffer> res = [dev newBufferWithLength:32 + 48 options:MTLResourceStorageModeShared];
    memset(res.contents, 0, 32 + 48);
    const uint32_t n = (uint32_t)nout;
    for (uint32_t v = 0; v < NVC_NVAR; v++) {
        id<MTLBuffer> scratch = [dev newBufferWithLength:(NSUInteger)nout * 4 options:MTLResourceStorageModePrivate];
        [e setBuffer:scratch offset:0 atIndex:nar - 1];
        nvmtl_nvconv_run(e, ours, og, v);
        NVMTLComputePipelineState *saved = e->_ps; NSMutableDictionary *savedTG = e->_tgLen;
        e->_ps = (NVMTLComputePipelineState *)g_nvc_ps[NVC_CMP]; e->_tgLen = nil; e->_nvcIn = YES;
        [e setBuffer:outBuf offset:outOff atIndex:5]; [e setBuffer:scratch offset:0 atIndex:6]; [e setBuffer:res offset:v * 4 atIndex:7];
        [e setBytes:&n length:4 atIndex:8]; [e setBuffer:res offset:32 atIndex:9];
        [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        e->_nvcIn = NO; e->_ps = saved; e->_tgLen = savedTG;
    }
    [e setBuffer:outBuf offset:outOff atIndex:nar - 1];
    char shape[96]; snprintf(shape, sizeof shape, "%ux%ux%u -> %u k%ux%u s%u,%u form %u", sw, sh, cin, cout, kw, kh, cp[46], cp[47], nar);
    NSString *sh_ = @(shape);
    { static _Atomic unsigned long issued; const unsigned long k = ++issued; if (k <= 96) nvlog("nvconv: verifying %s (%lu issued)", shape, k); }
    [e->_cb addCompletedHandler:^(id<MTLCommandBuffer> cb) {
        const uint32_t *r = (const uint32_t *)res.contents; int pick = 0;
        for (int v = 0; v < NVC_NVAR && !pick; v++) if (r[v] == 0) pick = v + 1;
        pthread_mutex_lock(&g_nvc_lk); g_nvc_state[key] = @(pick ? pick : -1); pthread_mutex_unlock(&g_nvc_lk);
        if (pick) { const unsigned long k = ++g_nvc_verified; if (k <= 64 || !(k & (k - 1))) nvlog("nvconv: VERIFIED %s epilogue %d (%lu so far)", sh_.UTF8String, pick - 1, k); }
        else { const unsigned long k = ++g_nvc_refused; const float *sp = (const float *)(r + 8);
            if (k <= 64 || !(k & (k - 1))) nvlog("nvconv: REFUSED %s - mismatches %u/%u/%u/%u/%u of %u, MPS keeps it (%lu so far); MPS/ours(prelu) %g/%g %g/%g %g/%g %g/%g", sh_.UTF8String,
                                                  r[0], r[1], r[2], r[3], r[4], n, k, sp[0], sp[1], sp[2], sp[3], sp[4], sp[5], sp[6], sp[7]); }
    }];
    return 1;
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
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])hs count:(NSUInteger)n { (void)hs; (void)n; }
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
    nvmtl_bind_embedded(_cb, NVMTL_SET_VERTEX, _ps->_function, _bufAt);
    return selected;
}
static int nvmtl_zprobe_on(void);
- (void)setComputePipelineState:(id<MTLComputePipelineState>)ps { _ps = (NVMTLComputePipelineState *)ps; nvmtl_retain_resource(_cb, ps);
    if (ps && nvmtl_zprobe_on())
        nvlog("ZENC setPipeline %s  tew=%lu maxT=%lu staticTG=%lu",
              _ps && _ps->_function ? _ps->_function->_fname.UTF8String : "?",
              (unsigned long)[ps threadExecutionWidth], (unsigned long)[ps maxTotalThreadsPerThreadgroup],
              (unsigned long)[ps staticThreadgroupMemoryLength]);
}
- (void)setBuffer:(id<MTLBuffer>)b offset:(NSUInteger)off atIndex:(NSUInteger)i {
    NVMTLBuffer *nb = (NVMTLBuffer *)b;
    if (!nb) {
        size_t zoff = 0;
        NVMTLBuffer *zb = nvmtl_bytes_ring(_cb, 16, &zoff);
        const BOOL zring = zb != nil;
        if (!zring) {
            zb = [NVMTLBuffer new];
            if (nvmtl_vk_buffer_create(16, 1, &zb->_b)) { nvlog("compute setBuffer: nil at %lu - zero buffer create FAILED", (unsigned long)i); return; }
        }
        if (zb->_b.map) memset((uint8_t *)zb->_b.map + zoff, 0, 16);
        if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, &zb->_b, zoff)) nvlog("compute setBuffer: nil at %lu - bind FAILED", (unsigned long)i);
        if (!zring) {
            if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
            [_cb->_scratch addObject:zb];
        }
        if (nvmtl_zprobe_on()) nvlog("ZENC setBuffer  idx %-3lu len 0        off 0  (nil -> zero-filled)", (unsigned long)i);
        return;
    }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:nb];
    if (!_bufAt) _bufAt = [NSMutableDictionary new];
    _bufAt[@(i)] = @[nb, @(off)];
    if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, off)) nvlog("compute setBuffer: bind FAILED");
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
        if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, &_cb->_g79BRing[i]->_b, _cb->_g79BOff[i])) nvlog("compute setBytes: bind FAILED");
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
    if (nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, &nb->_b, roff)) nvlog("compute setBytes: bind FAILED");
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
    if (!nt) { nvlog("compute setTexture: nil at %lu", (unsigned long)i); return; }
    nvmtl_zprobe_record(nt, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, (unsigned)i, nt->_stex ? 1u : 0u);
    nvmtl_retain_resource(_cb, nt);
    [nt nvmtlSurfaceIn]; if (nt->_stex) nvmtl_surf_dirty(_cb, nt);
    if (nt->_tbView) nvmtl_vk_bind_texel_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, nt->_tbView, nt->_tbStorage);
    else {
    if (nvmtl_vk_cmd_prepare_sampled_image(&_cb->_c, [nt nvi], nt->_stex)) { nvlog("compute setTexture: image layout preparation refused"); return; }
    if (nvmtl_vk_bind_texture_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, [nt nvview])) nvlog("compute setTexture: bind FAILED");
    nvmtl_vk_bind_storage_view(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, nt->_stex ? [nt nvview] : NULL);
    }
}
- (void)nvmtlE78Barrier:(BOOL)indirect {
    int kind;
    if (indirect) kind = 0;
    else if (!_e78NDisp) {
        kind = _cb->_g79C2C && nvmtl_g79_on(NVMTL_G79_ENC) ? 1 : 0;
        nvmtl_g79_count(kind ? G79C_ENCNARROW : G79C_ENCFULL);
    }
    else if (_e78Concurrent && nvmtl_enc78_on(NVMTL_E78_H7)) {
        kind = _g79Bar ? 1 : 2;
        if (_g79Bar) nvmtl_g79_count(G79C_BARFOLD);
    }
    else if (nvmtl_enc78_on(NVMTL_E78_P1)) kind = 1;
    else kind = 0;
    _g79Bar = NO;
    _e78NDisp++;
    if (kind == 0) nvmtl_vk_cmd_barrier(&_cb->_c);
    else if (kind == 1) nvmtl_vk_cmd_barrier_c2c(&_cb->_c);
    if (kind) _e78Loose = YES;
    nvmtl_enc78_count(E78C_DISP, 1); nvmtl_enc78_count(E78C_FULL + kind, 1);
    if (_e78Concurrent) nvmtl_enc78_count(E78C_CONC, 1);
}
- (void)dispatchThreadgroups:(MTLSize)grid threadsPerThreadgroup:(MTLSize)tg {
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
    if (nvmtl_vendor_dispatch_sass(selected, &_cb->_c, t, l) == 0) return;
    nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &selected->_p, t, l);
}
- (void)dispatchThreads:(MTLSize)threads threadsPerThreadgroup:(MTLSize)tg {
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
    if (nvmtl_vendor_dispatch_sass(selected, &_cb->_c, t, l) == 0) return;
    nvmtl_vk_cmd_dispatch_threads(&_cb->_c, &selected->_p, t, l);
}
- (void)setBufferOffset:(NSUInteger)off atIndex:(NSUInteger)i {
    { NSArray *b = _bufAt[@(i)]; if (b) _bufAt[@(i)] = @[b[0], @(off)]; }
    if (nvmtl_vk_bind_buffer_offset(&_cb->_c, NVMTL_SET_VERTEX, (uint32_t)i, off))
        nvlog("compute setBufferOffset: nothing bound at index %lu", (unsigned long)i);
}
- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i {
    if (!_tgLen) _tgLen = [NSMutableDictionary new];
    if (nvmtl_zprobe_on()) nvlog("ZENC tgMemory   idx %-3lu length %lu", (unsigned long)i, (unsigned long)len);
    NSUInteger was = [_tgLen[@(i).stringValue] unsignedIntegerValue];
    if (len) _tgLen[@(i).stringValue] = @(len);
    else     [_tgLen removeObjectForKey:@(i).stringValue];
    if (was != len) { _e78Key = nil; _e78VarSel = nil; }
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
    nvlog("compute executeCommandsInBuffer: replayed %lu of %lu slot(s)", (unsigned long)n, (unsigned long)r.length);
}
- (void)setSamplerState:(id<MTLSamplerState>)ss atIndex:(NSUInteger)i {
    NVMTLSamplerState *ns = (NVMTLSamplerState *)ss;
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
    NVMTLComputePipelineState *selected = [self nvmtlSamplerPipeline];
    if (!selected) { nvlog("indirect dispatch with no valid compute sampler pipeline"); return; }
    uint32_t l[3] = { (uint32_t)tg.width, (uint32_t)tg.height, (uint32_t)tg.depth };
    if (nvmtl_vk_tg_reject(l, NULL, _ps && _ps->_function ? _ps->_function->_fname.UTF8String : NULL, selected->_p.lin)) return;
    [self nvmtlE78Barrier:YES];
    nvmtl_vk_cmd_dispatch_indirect_tg(&_cb->_c, &selected->_p, &nb->_b, off, l);
}
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u { nvmtl_retain_resource(_cb, r); }
- (void)useResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n usage:(MTLResourceUsage)u { for (NSUInteger i=0;i<n;i++) nvmtl_retain_resource(_cb,r[i]); }
- (void)useHeap:(id<MTLHeap>)h {}
- (void)sampleCountersInBuffer:(id)sb atSampleIndex:(NSUInteger)i withBarrier:(BOOL)b { nvmtl_sample_counter(_cb, sb, i, self, b); }
- (void)memoryBarrierWithScope:(MTLBarrierScope)scope {
    if (nvmtl_g79_on(NVMTL_G79_BAR)) { _g79Bar = YES; nvmtl_g79_count(G79C_BARLAZY); } else nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)memoryBarrierWithResources:(const id<MTLResource> __unsafe_unretained [])r count:(NSUInteger)n {
    if (nvmtl_g79_on(NVMTL_G79_BAR)) { _g79Bar = YES; nvmtl_g79_count(G79C_BARLAZY); } else nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)pushDebugGroup:(NSString *)g {}
- (void)popDebugGroup {}
- (void)insertDebugSignpost:(NSString *)sp {}
- (void)endEncoding {
    if (nvmtl_g79_on(NVMTL_G79_ENC)) {
        if (_e78NDisp) { _cb->_g79C2C = YES; if (_e78Loose || _g79Bar) _cb->_g79Owed = YES; }
        _e78Loose = NO; _g79Bar = NO;
    } else if (_e78Loose || _g79Bar) { nvmtl_vk_cmd_barrier(&_cb->_c); _e78Loose = NO; _g79Bar = NO; nvmtl_enc78_count(E78C_ENDFULL, 1); }
    nvmtl_g79_census();
    _cb->_nEnd++; nvmtl_enc78_census();
}
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
@end

static NSUInteger nvmtl_texture_slice_count(NVMTLTexture *t) {
    MTLTextureType type = t.textureType;
    return type == MTLTextureTypeCube ? 6 : type == MTLTextureTypeCubeArray ? t.arrayLength * 6 : t.arrayLength;
}
@implementation NVMTLBlitCommandEncoder
- (unsigned long long)globalTraceObjectID { return nvmtl_global_trace_id(self); }
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so toBuffer:(id<MTLBuffer>)dst
     destinationOffset:(NSUInteger)dof size:(NSUInteger)size {
    NVMTLBuffer *s = (NVMTLBuffer *)src, *d = (NVMTLBuffer *)dst;
    if (!s || !d) { nvlog("blit copyFromBuffer: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s];
    [_cb->_resources addObject:d];
    if (nvmtl_vk_cmd_copy_buffer(&_cb->_c, &s->_b, so, &d->_b, dof, size)) nvlog("blit copyFromBuffer: FAILED");
}
- (void)fillBuffer:(id<MTLBuffer>)buf range:(NSRange)range value:(uint8_t)value {
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
    NVMTLTexture *s = (NVMTLTexture *)src, *d = (NVMTLTexture *)dst;
    if (!s || !d) { nvlog("blit copyFromTexture: nil"); return; }
    if (sl >= s.mipmapLevelCount || dl >= d.mipmapLevelCount || ss >= nvmtl_texture_slice_count(s) || ds >= nvmtl_texture_slice_count(d)) {
        nvlog("blit copyFromTexture: subresource outside view"); return;
    }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s];
    [_cb->_resources addObject:d];
    { int was = nvtrace_scope([d nvi]->w, [d nvi]->h);
      nvtrace("BLIT tex %ux%u surf %u (%lu,%lu %lux%lu) -> tex %ux%u surf %u (%lu,%lu)", [s nvi]->w, [s nvi]->h, nvmtl_surf_id(s), (unsigned long)so.x,
          (unsigned long)so.y, (unsigned long)size.width, (unsigned long)size.height, [d nvi]->w, [d nvi]->h, nvmtl_surf_id(d), (unsigned long)dof.x, (unsigned long)dof.y);
      g_trace_mute = was; }
    [s nvmtlSurfaceIn]; [d nvmtlSurfaceIn]; nvmtl_surf_dirty(_cb, d);
    if (nvmtl_vk_cmd_copy_image_sub(&_cb->_c, [s nvi], (uint32_t)sl + s->_baseLevel, (uint32_t)ss + s->_baseSlice, (uint32_t)so.x, (uint32_t)so.y,
                                [d nvi], (uint32_t)dl + d->_baseLevel, (uint32_t)ds + d->_baseSlice, (uint32_t)dof.x, (uint32_t)dof.y,
                                (uint32_t)size.width, (uint32_t)size.height))
        nvlog("blit copyFromTexture: FAILED");
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
    NVMTLTexture *t = (NVMTLTexture *)tex;
    if (!t) { nvlog("generateMipmapsForTexture: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:t];
    { int was = nvtrace_scope([t nvi]->w, [t nvi]->h); nvtrace("MIPS tex %ux%u surf %u", [t nvi]->w, [t nvi]->h, nvmtl_surf_id(t)); g_trace_mute = was; }
    [t nvmtlSurfaceIn]; nvmtl_surf_dirty(_cb, t);
    if (nvmtl_vk_cmd_image_gen_mipmaps(&_cb->_c, [t nvi])) nvlog("generateMipmapsForTexture: FAILED");
}
- (void)copyFromTexture:(id<MTLTexture>)src sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl toTexture:(id<MTLTexture>)dst
       destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl sliceCount:(NSUInteger)nslices levelCount:(NSUInteger)nlevels {
    NVMTLTexture *s = (NVMTLTexture *)src, *d = (NVMTLTexture *)dst;
    if (!s || !d) { nvlog("blit copyFromTexture:sliceCount: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:s]; [_cb->_resources addObject:d];
    for (NSUInteger k = 0; k < (nslices ? nslices : 1); k++)
        for (NSUInteger l = 0; l < (nlevels ? nlevels : 1); l++) {
            if (sl + l >= s.mipmapLevelCount || dl + l >= d.mipmapLevelCount) { nvlog("blit sliceCount: mip outside view"); return; }
            NSUInteger w = MAX((NSUInteger)1, s.width >> (sl + l)), h = MAX((NSUInteger)1, s.height >> (sl + l));
            [self copyFromTexture:s sourceSlice:ss + k sourceLevel:sl + l sourceOrigin:MTLOriginMake(0,0,0)
                      sourceSize:MTLSizeMake(w,h,1) toTexture:d destinationSlice:ds + k destinationLevel:dl + l destinationOrigin:MTLOriginMake(0,0,0)];
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
    NVMTLTexture *s = (NVMTLTexture *)src;
    if (!s) { nvlog("blit copyFromTexture:toTexture: nil"); return; }
    [self copyFromTexture:src sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0)
               sourceSize:MTLSizeMake(s.width, s.height, 1) toTexture:dst
         destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
}
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)row
   sourceBytesPerImage:(NSUInteger)img sourceSize:(MTLSize)size toTexture:(id<MTLTexture>)dst
      destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dof {
    NVMTLBuffer *b = (NVMTLBuffer *)src; NVMTLTexture *t = (NVMTLTexture *)dst;
    if (!b || !t) { nvlog("blit copyFromBuffer:toTexture: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:b];
    [_cb->_resources addObject:t];
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
    [t nvmtlSurfaceIn]; nvmtl_surf_dirty(_cb, t);
    { BOOL vol = [t nvi]->mtl_type == 7; NSUInteger nz = vol ? (size.depth ? size.depth : 1) : 1; NSUInteger per = img ? img : row * size.height; int rc = 0;
      for (NSUInteger zi = 0; zi < nz && !rc; zi++)
          rc = nvmtl_vk_cmd_copy_buffer_to_image_level_layer(&_cb->_c, &b->_b, so + zi * per, (uint32_t)row, [t nvi],
                                      (uint32_t)dof.x, (uint32_t)dof.y, (uint32_t)size.width, (uint32_t)size.height, (uint32_t)dl + t->_baseLevel, (uint32_t)(vol ? dof.z + zi : ds + t->_baseSlice));
      if (rc) nvlog("blit copyFromBuffer:toTexture: FAILED (level %lu slice %lu)", (unsigned long)dl, (unsigned long)ds); }
}
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)row
   sourceBytesPerImage:(NSUInteger)img sourceSize:(MTLSize)size toTexture:(id<MTLTexture>)dst
      destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dof
               options:(MTLBlitOption)opt {
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
    NVMTLTexture *t = (NVMTLTexture *)src; NVMTLBuffer *b = (NVMTLBuffer *)dst;
    if (!t || !b) { nvlog("blit copyFromTexture:toBuffer: nil"); return; }
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    [_cb->_resources addObject:t];
    [_cb->_resources addObject:b];
    { nvk_image *ti = [t nvi]; uint32_t tl = (uint32_t)nvmtl_texture_slice_count(t), tm = (uint32_t)t.mipmapLevelCount; BOOL vol = ti->mtl_type == 7;
      if (ss >= tl || sl >= tm || (vol ? (so.z + size.depth > MAX((NSUInteger)1, t.depth >> sl)) : (size.depth != 1 || so.z))) {
          nvlog("blit copyFromTexture:toBuffer: slice %lu/%u level %lu/%u depth %lu@z%lu — outside this texture", (unsigned long)ss, tl, (unsigned long)sl, tm, (unsigned long)size.depth, (unsigned long)so.z); return; } }
    [t nvmtlSurfaceIn];
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
    const uint32_t asp = (opt & MTLBlitOptionDepthFromDepthStencil) ? 2u : (opt & MTLBlitOptionStencilFromDepthStencil) ? 4u : 0u;
    if (opt & ~(MTLBlitOptionDepthFromDepthStencil | MTLBlitOptionStencilFromDepthStencil))
        nvlog("blit copyFromTexture:toBuffer:options:%lu — only the depth/stencil options are implemented", (unsigned long)opt);
    nvmtl_vk_copy_aspect_override(asp, asp == 2u ? 4u : asp == 4u ? 1u : 0u);
    [self copyFromTexture:src sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:size toBuffer:dst
        destinationOffset:dof destinationBytesPerRow:row destinationBytesPerImage:img];
    nvmtl_vk_copy_aspect_override(0, 0);
}
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)pushDebugGroup:(NSString *)g {}
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
- (void)endEncoding { _cb->_nEnd++; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (NSString *)label { return _label; }
- (id<MTLDevice>)device { id<MTLDevice> d = _cb ? [_cb device] : nil; return d ? d : (id<MTLDevice>)gNVMTLMainDevice; }
@end

#pragma mark - command buffer / queue
#import "NVMTLEventSync.m"
@implementation NVMTLCommandBuffer
- (void)nvmtlReleaseCapacity {
    @synchronized (self) {
        if (_capacityHeld) { _capacityHeld = NO; dispatch_semaphore_signal(_queue->_capacity); }
    }
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
        _callbackGroup = dispatch_group_create(); _scheduledGroup = dispatch_group_create();
    }
    return self;
}
- (void)useResidencySet:(id<MTLResidencySet>)r {}
- (void)useResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {}
- (NSMutableDictionary *)userDictionary {
    @synchronized (self) { if (!_userDictionary) _userDictionary = [NSMutableDictionary new]; return _userDictionary; }
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
            if (d.colorAttachments[0].loadAction == MTLLoadActionClear) { sr->_surfSeed = IOSurfaceGetSeed(sr->_surf); sr->_surfSynced = YES; sr->_surfGen = sr->_vramOn ? sr->_surfGen : nvmtl_vram_gen(sr->_surf, sr->_plane); }
            else [sr nvmtlSurfaceIn];
            nvmtl_surf_dirty(self, sr); } }
    const float defaultBlend[4] = { 0, 0, 0, 0 };
    nvmtl_vk_cmd_set_blend_color(&_c, defaultBlend);
    nvmtl_vk_cmd_set_line_width(&_c, 1.0f);
    (void)nvmtl_vk_cmd_set_polygon_mode(&_c, 0);
    (void)nvmtl_vk_cmd_set_depth_clip(&_c, 1);
    e->_visBuf = (NVMTLBuffer *)d.visibilityResultBuffer;
    MTLClearColor cc = d.colorAttachments[0].clearColor;
    e->_clear[0] = (float)cc.red; e->_clear[1] = (float)cc.green; e->_clear[2] = (float)cc.blue; e->_clear[3] = (float)cc.alpha;
    if (![e nvmtlBuildPass:d]) e->_refused = YES;
    return e;
}
- (void)nvmtlRecordDrawable:(id<MTLDrawable>)drawable presentation:(dispatch_block_t)action {
    if (!drawable || _committed) return;
    {
        extern void nvmtl_vk_submit_wait_stats(uint64_t *total_ns, uint64_t *waits);
        static int off = -1;
        if (off < 0) { const char *e = getenv("NVMTL_NO_FPS"); off = NVMTL_RELEASE ? 1 : (e && *e && *e != '0') ? 1 : 0; }
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

- (void)pushDebugGroup:(NSString *)g {}
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
    if (ws) [self waitUntilCompleted]; else [self waitUntilScheduled];
    return self.status != MTLCommandBufferStatusError;
}
- (void)commit { [self nvmtlSubmitWithEvents]; }
- (id<MTLComputeCommandEncoder>)computeCommandEncoder {
    NVMTLComputeCommandEncoder *e = [NVMTLComputeCommandEncoder new]; e->_cb = self; _nEnc++; return e;
}
- (id<MTLComputeCommandEncoder>)computeCommandEncoderWithDispatchType:(MTLDispatchType)t {
    NVMTLComputeCommandEncoder *e = (NVMTLComputeCommandEncoder *)[self computeCommandEncoder];
    e->_e78Concurrent = t == MTLDispatchTypeConcurrent; return e; }
- (id<MTLComputeCommandEncoder>)computeCommandEncoderWithDescriptor:(MTLComputePassDescriptor *)d {
    return [self computeCommandEncoderWithDispatchType:d ? d.dispatchType : MTLDispatchTypeSerial]; }
- (id<MTLBlitCommandEncoder>)blitCommandEncoderWithDescriptor:(MTLBlitPassDescriptor *)d { return [self blitCommandEncoder]; }
- (id<MTLResourceStateCommandEncoder>)resourceStateCommandEncoder { return nil; }
- (id<MTLResourceStateCommandEncoder>)resourceStateCommandEncoderWithDescriptor:(MTLResourceStatePassDescriptor *)d { return nil; }
- (id<MTLBlitCommandEncoder>)blitCommandEncoder {
    nvmtl_g79_flush(self);
    NVMTLBlitCommandEncoder *e = [NVMTLBlitCommandEncoder new]; e->_cb = self; _nEnc++; return e;
}
- (id<MTLAccelerationStructureCommandEncoder>)accelerationStructureCommandEncoder {
    nvmtl_g79_flush(self);
    NVMTLAccelerationStructureCommandEncoder *e = [NVMTLAccelerationStructureCommandEncoder new]; e->_cb = self; _nEnc++; return e;
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
    _nWait++; [_completionCondition lock];
    while (!_completionReady) [_completionCondition wait];
    [_completionCondition unlock];
    dispatch_group_wait(_callbackGroup, DISPATCH_TIME_FOREVER);
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
    nvlog("queue: GPU priority hint %lu unavailable; retaining fixed backend priority", (unsigned long)priority);
    return NO;
}
- (BOOL)setBackgroundGPUPriority:(NSUInteger)priority {
    nvlog("queue: background GPU priority hint %lu unavailable; retaining fixed backend priority", (unsigned long)priority);
    return NO;
}
- (void)addResidencySet:(id<MTLResidencySet>)r {}
- (void)addResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {}
- (void)removeResidencySet:(id<MTLResidencySet>)r {}
- (void)removeResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])r count:(NSUInteger)n {}
- (void)dealloc { nvmtl_vk_queue_destroy(&_q); }
- (id<MTLCommandBuffer>)commandBuffer {
    dispatch_semaphore_t cap = nil;
    if (nvmtl_async_commit_on()) {
        @synchronized (self) { if (!_capacity) _capacity = dispatch_semaphore_create(_capacityCount ? _capacityCount : 64); cap = _capacity; }
        dispatch_semaphore_wait(cap, DISPATCH_TIME_FOREVER);
    }
    NVMTLCommandBuffer *cb = [NVMTLCommandBuffer new];
    if (!cb) { if (cap) dispatch_semaphore_signal(cap); return nil; }
    cb->_queue = self; cb->_capacityHeld = cap != nil;
    if (nvmtl_vk_cmd_begin(&_q, &cb->_c)) { nvlog("queue: command buffer begin FAILED"); return nil; }
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
    NVP_VOFF, NVP_FOFF, NVP_DEPTHCLIP
} nvp_kind;
typedef struct {
    nvp_kind kind;
    uint32_t i0, i1, i2;
    uint64_t u0, u1, u2, u3, u4, u5;
    double   f0, f1, f2, f3, f4, f5;
    void    *o0;
    const void *p0;
} nvp_cmd;
@interface NVMTLDeferredRenderCommandEncoder : NSProxy { @public __weak NVMTLParallelRenderCommandEncoder *_owner; NSMutableArray *_inv, *_keep; BOOL _ended; NSString *_label; nvp_cmd *_cmds; uint32_t _ncmd, _capcmd; }
@end
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
    NSMethodSignature *m = [NVMTLRenderCommandEncoder instanceMethodSignatureForSelector:s];
    if (!m) { nvlog("parallel sub-encoder: %s is not a render-encoder selector — recorded as a no-op", sel_getName(s)); m = [NSMethodSignature signatureWithObjCTypes:"v@:"]; }
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
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n {}
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])h count:(NSUInteger)n stages:(MTLRenderStages)st {}
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
- (void)pushDebugGroup:(NSString *)g {}
- (void)popDebugGroup {}
@end

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
            if (!vb) { nvlog("acceleration structure: triangle geometry without a vertex buffer — dropped"); continue; }
            if (track) [track addObject:vb];
            o->vertices = nvmtl_vk_buffer_address(&vb->_b) + t.vertexBufferOffset; o->stride = (uint32_t)(t.vertexStride ? t.vertexStride : 12);
            o->count = (uint32_t)t.triangleCount;
            MTLAttributeFormat f = [t respondsToSelector:@selector(vertexFormat)] ? t.vertexFormat : MTLAttributeFormatFloat3;
            switch (f) {
            case MTLAttributeFormatFloat3: o->vertex_format = 106; break;
            case MTLAttributeFormatHalf3:  o->vertex_format = 90; break;
            case MTLAttributeFormatFloat2: o->vertex_format = 103; break;
            case MTLAttributeFormatHalf2:  o->vertex_format = 83; break;
            default: nvlog("acceleration structure: vertex format %lu is not carried — read as float3", (unsigned long)f); o->vertex_format = 106; break;
            }
            NVMTLBuffer *ib = (NVMTLBuffer *)t.indexBuffer;
            if (ib) { if (track) [track addObject:ib]; o->indices = nvmtl_vk_buffer_address(&ib->_b) + t.indexBufferOffset;
                      o->index_type = t.indexType == MTLIndexTypeUInt16 ? 0 : 1; o->max_vertex = 0xFFFFFF; }
            else { o->index_type = 1000165000u ; o->max_vertex = o->count ? o->count * 3 - 1 : 0; }
        } else if ([g isKindOfClass:[MTLAccelerationStructureBoundingBoxGeometryDescriptor class]]) {
            MTLAccelerationStructureBoundingBoxGeometryDescriptor *b = (MTLAccelerationStructureBoundingBoxGeometryDescriptor *)g;
            NVMTLBuffer *bb = (NVMTLBuffer *)b.boundingBoxBuffer;
            if (!bb) { nvlog("acceleration structure: box geometry without a buffer — dropped"); continue; }
            if (track) [track addObject:bb];
            o->is_boxes = 1; o->boxes = nvmtl_vk_buffer_address(&bb->_b) + b.boundingBoxBufferOffset;
            o->box_stride = (uint32_t)(b.boundingBoxStride ? b.boundingBoxStride : 24); o->count = (uint32_t)b.boundingBoxCount;
        } else {
            nvlog("acceleration structure: geometry %s is not carried (motion / curves) — dropped", NSStringFromClass([g class]).UTF8String); continue;
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
    nvmtl_vk_accel_destroy(&_as);
    if (_uid.buf) nvmtl_vk_buffer_destroy(&_uid);
#if !__has_feature(objc_arc)
    [super dealloc];
#endif
}
@end

@implementation NVMTLAccelerationStructureCommandEncoder
- (id<MTLDevice>)device { return _cb->_queue->_dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)endEncoding { _cb->_nEnd++; }
- (void)nvmtlTrack:(id)r { if (!r) return; if (!_cb->_resources) _cb->_resources = [NVMTLResSet new]; [_cb->_resources addObject:r]; }
- (void)buildAccelerationStructure:(id<MTLAccelerationStructure>)as descriptor:(MTLAccelerationStructureDescriptor *)desc
                     scratchBuffer:(id<MTLBuffer>)scratch scratchBufferOffset:(NSUInteger)soff {
    NVMTLAccelerationStructure *dst = (NVMTLAccelerationStructure *)as; NVMTLBuffer *sc = (NVMTLBuffer *)scratch;
    if (!dst || !sc) { nvlog("build acceleration structure: nil %s", dst ? "scratch" : "structure"); return; }
    [self nvmtlTrack:dst]; [self nvmtlTrack:sc];
    if (!_cb->_resources) _cb->_resources = [NVMTLResSet new];
    static nvmtl_geom geoms[NVMTL_MAX_GEOMS]; int instance = 0; uint32_t icount = 0;
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
    if (instance) {
        MTLInstanceAccelerationStructureDescriptor *idesc = (MTLInstanceAccelerationStructureDescriptor *)desc;
        NVMTLBuffer *ib = (NVMTLBuffer *)idesc.instanceDescriptorBuffer;
        if (!ib || !nvmtl_buf_cpu(ib)) { nvlog("build acceleration structure: instance descriptors %s", ib ? "not host-visible" : "nil"); return; }
        const MTLAccelerationStructureInstanceDescriptorType itype = idesc.instanceDescriptorType;
        if (itype != MTLAccelerationStructureInstanceDescriptorTypeDefault && itype != MTLAccelerationStructureInstanceDescriptorTypeUserID &&
            itype != MTLAccelerationStructureInstanceDescriptorTypeIndirect) {
            nvlog("build acceleration structure: instance descriptor type %lu is not carried (Default, UserID and Indirect are)", (unsigned long)itype); return; }
        const uint32_t natural = itype == MTLAccelerationStructureInstanceDescriptorTypeDefault ? 64 : itype == MTLAccelerationStructureInstanceDescriptorTypeUserID ? 68 : 72;
        NSArray *blases = idesc.instancedAccelerationStructures; static uint64_t addrs[4096]; uint32_t nas = 0;
        static uint32_t iftAdds[4096];
        for (NVMTLAccelerationStructure *b in blases) { if (nas < 4096) { iftAdds[nas] = b->_iftAdd; addrs[nas++] = b->_as.addr; } [self nvmtlTrack:b]; }
        NVMTLBuffer *vk = [NVMTLBuffer new];
        size_t bytes = (size_t)(icount ? icount : 1) * (64 + 4);
        if (nvmtl_vk_buffer_create(bytes, 1, &vk->_b) || !vk->_b.map) { nvlog("build acceleration structure: instance conversion buffer FAILED"); return; }
        uint32_t stride = (uint32_t)(idesc.instanceDescriptorStride ? idesc.instanceDescriptorStride : natural);
        if (stride < natural || idesc.instanceDescriptorBufferOffset + (size_t)(icount ? icount - 1 : 0) * stride + natural > ib->_b.size) {
            nvlog("build acceleration structure: %u instances of stride %u at %lu overrun the %zu-byte descriptor buffer - REFUSED", icount, stride,
                  (unsigned long)idesc.instanceDescriptorBufferOffset, ib->_b.size); return; }
        const size_t uidOff = (size_t)(icount ? icount : 1) * 64, uidBytes = (size_t)(icount ? icount : 1) * 4;
        memset((uint8_t *)vk->_b.map + uidOff, 0, uidBytes);
        nvmtl_vk_convert_instances((const uint8_t *)nvmtl_buf_cpu(ib) + idesc.instanceDescriptorBufferOffset, stride, icount, addrs, nas, vk->_b.map,
                                   itype == MTLAccelerationStructureInstanceDescriptorTypeIndirect ? 3u : (uint32_t)itype,
                                   (uint32_t *)((uint8_t *)vk->_b.map + uidOff), iftAdds);
        if (dst->_uid.size < uidBytes) {
            if (dst->_uid.buf) { NVMTLBuffer *old = [NVMTLBuffer new]; old->_b = dst->_uid; if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new]; [_cb->_scratch addObject:old]; }
            memset(&dst->_uid, 0, sizeof dst->_uid);
            if (nvmtl_vk_buffer_create(uidBytes, 0, &dst->_uid)) { nvlog("build acceleration structure: userID side table (%zu B) FAILED - user_instance_id reads 0", uidBytes); memset(&dst->_uid, 0, sizeof dst->_uid); }
        }
        if (dst->_uid.buf && nvmtl_vk_cmd_copy_buffer(&_cb->_c, &vk->_b, uidOff, &dst->_uid, 0, uidBytes))
            nvlog("build acceleration structure: userID side table copy FAILED");
        if (!_cb->_scratch) _cb->_scratch = [NSMutableArray new];
        [_cb->_scratch addObject:vk];
        [self nvmtlTrack:ib]; inst_addr = nvmtl_vk_buffer_address(&vk->_b);
    }
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
- (void)useHeap:(id<MTLHeap>)h {}
- (void)useHeaps:(const id<MTLHeap> __unsafe_unretained [])hs count:(NSUInteger)n {}
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f {}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)sampleCountersInBuffer:(id<MTLCounterSampleBuffer>)b atSampleIndex:(NSUInteger)i withBarrier:(BOOL)w { nvmtl_sample_counter(_cb, b, i, self, w); }
@end

static void nvmtl_buffer_sync_in(NVMTLBuffer *b)
{
    if (!b || !b->_hostPtr || !b->_b.map || !b->_hostLen) return;
    const uint64_t t0 = nvmtl_perf_now();
    memcpy(b->_b.map, b->_hostPtr, b->_hostLen);
    nvmtl_perf_note(NVP_SYNCIN, t0, b->_hostLen);
}

static void nvmtl_buffer_sync_out(NVMTLBuffer *b)
{
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
