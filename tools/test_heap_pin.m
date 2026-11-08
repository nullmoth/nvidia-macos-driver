// test_heap_pin.m - the heap pin's lifetime and cost rules, modelled without a GPU.
//
// nvmtl_use_heap (plugin/NVMTLObjects.m) pins every member of a used heap for the submission: the pin holds each
// member strongly until the last submission naming it finishes, so a member the app releases is not destroyed under
// the GPU. This model copies the membership / pin / keep logic of that function onto plain objects (a member counts
// its deallocs and residency references; a command buffer is an array that holds pins until "completion"), with a
// fake clock so the keep window is deterministic. A use is split into its two locked phases - decide, then (after the
// pin is built outside the lock) install - so the arms can interleave several command buffers and a registration that
// lands while a pin is being built, which is what the game does on its encoding threads.
//
// Policies: "new" is the shipped logic. "v1" is the first kept-pin version (install only if the version did not move,
// no single build, walk on collapse); "legacy" is the original (v1 without keep, every member referenced). The
// "broken:" variants each remove one rule an arm depends on, and that arm must catch it.
//
// Every arm must give the same verdict at any optimisation level:
//   for o in 0 1 2; do clang -fobjc-arc -O$o -Wall -framework Foundation tools/test_heap_pin.m -o /tmp/test_heap_pin \
//                      && /tmp/test_heap_pin; done
// What makes that true: at -O0 ARC does not elide the autorelease handshake, so an object returned by a function or
// a Foundation getter sits in the innermost autorelease pool and outlives every "release point" in the arm. So: every
// helper here that returns an object is NS_RETURNS_RETAINED (no autorelease at all), every Foundation call that hands
// back a member, a pin or a command buffer runs inside its own @autoreleasepool (STEP below, or inside the helper), and
// no arm reads a returned object into a strong local that is still alive at the point where the arm expects a free.
#import <Foundation/Foundation.h>
#include <stdio.h>

typedef struct { const char *name; BOOL v1, legacy; uint64_t keepNs;
                 BOOL noExpiry, noTimer, noUnkeep, renewKeep, noSingleFlight, clearAddedAtInstall; } Policy;
static Policy P;
static uint64_t gNow;                                                       // fake clock, ns
static uint64_t gWalks, gWalked, gDeltas, gFlats, gReuses, gWaits, gRefsTaken, gRefsSkipped;
static NSMutableArray *gTimers;                                             // deferred keep drops

@class Heap;
@interface Member : NSObject { @public BOOL _heapBacked; int _resRefs; Heap *_heap; int *_freed; } @end
@implementation Member - (void)dealloc { if (_freed) (*_freed)++; } @end
static BOOL needs_ref(Member *m) { return P.legacy ? YES : !m->_heapBacked; }

@interface Pin : NSObject { @public NSArray *_members, *_own; Pin *_base; unsigned _depth; } @end
@implementation Pin
static void take(NSArray *ms, NSMutableArray **own) {
    for (Member *m in ms) { if (needs_ref(m)) { m->_resRefs++; gRefsTaken++; if (!*own) *own = [NSMutableArray new]; [*own addObject:m]; } else gRefsSkipped++; }
}
- (instancetype)initWithMembers:(NSArray *)ms { if ((self = [super init])) { _members = ms; NSMutableArray *own = nil; take(ms, &own); _own = [own copy]; } return self; }
- (instancetype)initWithBase:(Pin *)base added:(NSArray *)added {
    if ((self = [self initWithMembers:added])) { _base = base; _depth = base->_depth + 1; }
    return self;
}
- (NSArray *)allMembers { return _base ? [[_base allMembers] arrayByAddingObjectsFromArray:_members] : _members; }
- (instancetype)initFlatFromBase:(Pin *)base added:(NSArray *)added {
    if ((self = [super init])) {
        NSMutableArray *own = nil, *chainOwn = [NSMutableArray new];
        for (Pin *p = base; p; p = p->_base) if (p->_own.count) [chainOwn addObjectsFromArray:p->_own];
        take(chainOwn, &own); take(added, &own);
        _members = [[base allMembers] arrayByAddingObjectsFromArray:added]; _own = [own copy];
    }
    return self;
}
- (BOOL)holds:(id)o { return [[self allMembers] containsObject:o]; }
- (void)dealloc { for (Member *m in _own) m->_resRefs--; }
@end

@interface Members : NSObject { @public NSHashTable *_members; uint64_t _version; __weak Pin *_pin; uint64_t _pinVersion; NSPointerArray *_added;
                                       Pin *_keep; uint64_t _keepUntil, _keepGen; BOOL _building; } @end
@implementation Members
- (instancetype)init { if ((self = [super init])) { _members = [NSHashTable weakObjectsHashTable]; _added = [NSPointerArray weakObjectsPointerArray]; } return self; }
@end
@interface Heap : NSObject { @public Members *_m; int *_freed; } @end
@implementation Heap - (instancetype)init { if ((self = [super init])) _m = [Members new]; return self; } - (void)dealloc { if (_freed) (*_freed)++; } @end
@interface CB : NSObject { @public NSMutableArray *_held; NSMutableDictionary *_versions; } @end
@implementation CB - (instancetype)init { if ((self = [super init])) { _held = [NSMutableArray new]; _versions = [NSMutableDictionary new]; } return self; } @end

static Pin *unkeep_locked(Members *m) NS_RETURNS_RETAINED;
static Pin *unkeep_locked(Members *m) { Pin *k = m->_keep; m->_keep = nil; m->_keepUntil = 0; return k; }
// One step of an arm inside its own pool, so nothing it autoreleases outlives it.
#define STEP(...) do { @autoreleasepool { __VA_ARGS__; } } while (0)
static void reg(Heap *h, Member *r) {   // nvmtl_heap_register; its own pool, the weak table's lookups autorelease
    @autoreleasepool { Members *m = h->_m;
        if (![m->_members containsObject:r]) { [m->_members addObject:r]; [m->_added addPointer:(__bridge void *)r]; m->_version++; } }
}
static BOOL unkeep(Heap *h) { if (P.noUnkeep) return NO; BOOL had; @autoreleasepool { Pin *k = unkeep_locked(h->_m); had = k != nil; k = nil; (void)k; } return had; }
static void tick(void) { @autoreleasepool {   // the dispatch_after drops whose time has come
    NSMutableArray *left = [NSMutableArray new];
    for (NSArray *t in gTimers) {
        if (gNow < [t[2] unsignedLongLongValue]) { [left addObject:t]; continue; }
        Members *m = ((NSPointerArray *)t[0]).allObjects.firstObject; if (!m) continue;
        Pin *k = nil; if (m->_keepGen == [t[1] unsignedLongLongValue]) k = unkeep_locked(m); k = nil; (void)k;
    }
    gTimers = left;
} }
static void arm_timer(Members *members, uint64_t gen, uint64_t keepNs) {
    if (P.noTimer) return;
    NSPointerArray *w = [NSPointerArray weakObjectsPointerArray]; [w addPointer:(__bridge void *)members];
    [gTimers addObject:@[w, @(gen), @(gNow + keepNs)]];
}

// One use in flight between its decide and install phases.
enum { kNone, kReuse, kDelta, kFlat, kWalk, kWait };
@interface Use : NSObject { @public CB *cb; Heap *heap; int kind; NSArray *resources, *added; Pin *pin, *base; uint64_t version; BOOL install, again; } @end
@implementation Use @end

// nvmtl_use_heap, first locked section (the shipped one), and its v1 / legacy counterpart.
static Use *use_decide(CB *cb, Heap *heap) NS_RETURNS_RETAINED;
static Use *use_decide(CB *cb, Heap *heap) {
    Members *members = heap->_m; Use *u = [Use new]; u->cb = cb; u->heap = heap; u->kind = kNone;
    NSValue *key = [NSValue valueWithNonretainedObject:heap]; NSNumber *prior = cb->_versions[key];
    const uint64_t keepNs = P.legacy ? 0 : P.keepNs;
    if (P.v1) {   // 4f9c3de / legacy
        for (int attempt = 0;; attempt++) {
            Pin *dropped = nil; BOOL again = NO;
            if (!P.noExpiry && members->_keep && gNow >= members->_keepUntil) dropped = unkeep_locked(members);
            u->version = members->_version;
            if (!prior || prior.unsignedLongLongValue != u->version) {
                if (members->_pinVersion == u->version) u->pin = members->_pin;
                if (u->pin) { gReuses++; u->kind = kReuse; }
                else {
                    u->base = members->_pin;
                    if (u->base && u->base->_depth < 16) { [members->_added compact]; u->added = members->_added.allObjects; u->kind = kDelta; }
                    else if ((members->_keep || dropped) && attempt == 0) { u->base = nil; if (!dropped) dropped = unkeep_locked(members); again = YES; }
                    else { u->base = nil; u->resources = members->_members.allObjects; u->kind = kWalk; }
                }
            }
            dropped = nil;
            if (!again) break;
            u->base = nil; u->pin = nil; u->added = nil; u->resources = nil; u->kind = kNone;
        }
        (void)keepNs; return u;
    }
    NSMutableArray *dropped = nil;
    if (members->_keep && !P.noExpiry && gNow >= members->_keepUntil) { dropped = [NSMutableArray new]; [dropped addObject:unkeep_locked(members)]; }
    u->version = members->_version;
    if (prior && prior.unsignedLongLongValue == u->version) return u;
    Pin *cur = members->_pin;
    if (cur && members->_pinVersion == u->version) { u->pin = cur; u->kind = kReuse; gReuses++; return u; }
    if (members->_building && !P.noSingleFlight) { u->kind = kWait; gWaits++; return u; }   // the driver sleeps on _cv here
    [members->_added compact];
    if (cur && cur->_depth < 16) { u->base = cur; u->added = members->_added.allObjects; u->kind = kDelta; }
    else if (cur) { u->base = cur; u->added = members->_added.allObjects; u->kind = kFlat; }
    else { u->resources = members->_members.allObjects; u->kind = kWalk; }
    if (!P.clearAddedAtInstall) members->_added = [NSPointerArray weakObjectsPointerArray];
    members->_building = YES; u->install = YES;
    return u;
}
// The pin build (outside the lock) and the second locked section.
static void use_install(Use *u) {
    Members *members = u->heap->_m; const uint64_t keepNs = P.legacy ? 0 : P.keepNs;
    if (u->kind == kWalk) { gWalks++; gWalked += u->resources.count; }
    if (u->kind == kDelta) gDeltas++;
    if (u->kind == kFlat) gFlats++;
    if (u->kind == kDelta || u->kind == kFlat || u->kind == kWalk) {
        u->pin = u->kind == kDelta ? [[Pin alloc] initWithBase:u->base added:u->added]
               : u->kind == kFlat  ? [[Pin alloc] initFlatFromBase:u->base added:u->added] : [[Pin alloc] initWithMembers:u->resources];
        Pin *replaced = nil; uint64_t keepGen = 0;
        if (P.v1) {
            if (members->_version == u->version) { members->_pin = u->pin; members->_pinVersion = u->version; members->_added = [NSPointerArray weakObjectsPointerArray];
                if (keepNs && u->kind == kWalk) { replaced = members->_keep; members->_keep = u->pin; members->_keepUntil = gNow + keepNs; keepGen = ++members->_keepGen; }
                else if (members->_keep) { replaced = members->_keep; members->_keep = u->pin; } }
        } else if (u->install) {
            members->_pin = u->pin; members->_pinVersion = u->version; members->_building = NO;
            if (P.clearAddedAtInstall) members->_added = [NSPointerArray weakObjectsPointerArray];
            if (keepNs && (u->kind == kWalk || (P.renewKeep && members->_keep))) {
                replaced = members->_keep; members->_keep = u->pin; members->_keepUntil = gNow + keepNs; keepGen = ++members->_keepGen; }
            else if (members->_keep) { replaced = members->_keep; members->_keep = u->pin; }
        }
        replaced = nil; (void)replaced;
        if (keepGen) arm_timer(members, keepGen, keepNs);
    }
    u->base = nil;
    if (u->pin) { [u->cb->_held addObject:u->heap]; [u->cb->_held addObject:u->pin]; u->cb->_versions[[NSValue valueWithNonretainedObject:u->heap]] = @(u->version); }
}
// A whole use. A use that would wait on another build in the driver is retried here after the others install.
static void use_heap(CB *cb, Heap *heap) { @autoreleasepool { Use *u = use_decide(cb, heap); if (u->kind != kWait) use_install(u); } }
// Several command buffers encoded at once: every decide first, then `during` (e.g. a registration from a streaming
// thread while the pins are built), then every install; waiters decide again after the builder installed.
static void use_together(NSArray *cbs, Heap *heap, void (^during)(void)) {
    @autoreleasepool {
        NSMutableArray *us = [NSMutableArray new], *waiting = [NSMutableArray new];
        for (CB *cb in cbs) { Use *u = use_decide(cb, heap); [(u->kind == kWait ? waiting : us) addObject:u]; }
        if (during) during();
        for (Use *u in us) use_install(u);
        while (waiting.count) { NSArray *w = [waiting copy]; [waiting removeAllObjects];
            for (Use *x in w) { Use *u = use_decide(x->cb, heap); if (u->kind == kWait) [waiting addObject:u]; else use_install(u); } }
    }
}

static int gFail;
static void arm(BOOL ok, BOOL wantOk, const char *name, NSString *detail) {
    BOOL good = ok == wantOk; if (!good) gFail++;
    printf("  %s [%s] %s%s  (%s)\n", good ? "ok  " : "FAIL", P.name, name, wantOk ? "" : "  <- must NOT hold here", detail.UTF8String);
}
static void reset(Policy p) { P = p; gNow = 0; gWalks = gWalked = gDeltas = gFlats = gReuses = gWaits = gRefsTaken = gRefsSkipped = 0; gTimers = [NSMutableArray new]; }
static const uint64_t MS = 1000000;
static Member *mk(Heap *h, NSMutableArray *app, int *freed) NS_RETURNS_RETAINED;
static Member *mk(Heap *h, NSMutableArray *app, int *freed) {
    Member *m = [Member new]; m->_heapBacked = YES; m->_freed = freed; if (app) [app addObject:m]; reg(h, m); return m; }
// The newest pin a command buffer holds, retained (not autoreleased) so the caller decides when it goes.
static Pin *last_pin(CB *cb) NS_RETURNS_RETAINED;
static Pin *last_pin(CB *cb) { Pin *p; @autoreleasepool { p = cb->_held.lastObject; } return p; }
// Does the pin's chain hold o? allMembers builds autoreleased arrays that hold every member, so in a pool of its own.
static BOOL pin_holds(Pin *p, id o) { BOOL r; @autoreleasepool { r = p && [p holds:o]; } return r; }
static BOOL pin_holds_all(Pin *p, NSArray *ms) { BOOL r = YES; @autoreleasepool { for (id m in ms) r = r && [p holds:m]; } return r; }

int main(void) { @autoreleasepool {
    const Policy NEW = { .name = "new", .keepNs = 200 * MS };
    const Policy V1 = { .name = "v1 (4f9c3de)", .v1 = YES, .keepNs = 200 * MS };
    const Policy LEGACY = { .name = "legacy", .v1 = YES, .legacy = YES };
    Policy NOEXP = NEW; NOEXP.noExpiry = YES; NOEXP.noTimer = YES; NOEXP.name = "broken: keep never expires";
    Policy RENEW = NEW; RENEW.renewKeep = YES; RENEW.name = "broken: deltas renew the keep";
    Policy NOUNK = NEW; NOUNK.noUnkeep = YES; NOUNK.name = "broken: no unkeep when out of room";
    Policy NOTIM = NEW; NOTIM.noTimer = YES; NOTIM.name = "broken: no keep timer";
    Policy NOSF = NEW; NOSF.noSingleFlight = YES; NOSF.name = "broken: no single build";
    Policy CLR = NEW; CLR.clearAddedAtInstall = YES; CLR.name = "broken: added list cleared at install";

    printf("A1 lifetime: a member the app releases while a submission that used its heap is in flight stays alive\n");
    Policy a1[] = { NEW, LEGACY };
    for (int k = 0; k < 2; k++) { reset(a1[k]);
        int freed = 0; Heap *h = [Heap new]; Member *x = mk(h, nil, &freed); (void)x;
        CB *cb = [CB new]; use_heap(cb, h); x = nil;                         // app drops it, GPU still running
        gNow += 300 * MS; tick();                                            // the keep window passes too
        const int before = freed; cb = nil;                                  // completion
        arm(before == 0 && freed == 1, YES, "released member alive until its submission completes, freed at completion",
            [NSString stringWithFormat:@"freed before completion %d, after %d", before, freed]); }

    printf("A2 release: once its submissions finish and the keep window passes, a released member is freed\n");
    Policy a2[] = { NEW, LEGACY, NOEXP, RENEW };
    for (int k = 0; k < 4; k++) { reset(a2[k]);
        int freed = 0; Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        for (int i = 0; i < 50; i++) mk(h, app, NULL);
        STEP(((Member *)app[49])->_freed = &freed);
        for (int f = 0; f < 60; f++) {                                       // 60 frames at 16 ms, GPU drains each frame
            if (f == 3) [app removeObjectAtIndex:49];                        // app drops one member in frame 3
            if (f % 2 == 0) mk(h, app, NULL);                                // and keeps adding (deltas every other frame)
            CB *cb = [CB new]; use_heap(cb, h); cb = nil; gNow += 16 * MS; tick(); }
        arm(freed == 1, k < 2, "released member freed within 60 frames (960 ms, keep 200 ms)",
            [NSString stringWithFormat:@"freed %d, walks %llu, deltas %llu, flats %llu", freed, gWalks, gDeltas, gFlats]); }

    printf("A3 cost: 600 frames of a 4000-member heap that drains every frame, a member added every 10th frame\n");
    uint64_t walkedNew = 0, walkedOld = 0;
    Policy a3[] = { NEW, LEGACY };
    for (int k = 0; k < 2; k++) { reset(a3[k]);
        Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        for (int i = 0; i < 4000; i++) mk(h, app, NULL);
        for (int f = 0; f < 600; f++) {
            if (f % 10 == 0) mk(h, app, NULL);
            for (int e = 0; e < 3; e++) { CB *cb = [CB new]; use_heap(cb, h); }
            gNow += 16 * MS; tick(); }
        *(k ? &walkedOld : &walkedNew) = gWalked;
        arm(gWalks <= 50, k == 0, "at most one full walk per keep window (9.6 s / 200 ms = 48)",
            [NSString stringWithFormat:@"%llu walks over %llu members, %llu deltas, %llu flats, %llu reuses", gWalks, gWalked, gDeltas, gFlats, gReuses]); }
    printf("     members walked: legacy %llu -> new %llu\n", walkedOld, walkedNew);

    printf("A4 refs: heap-backed members take no residency reference; a member that is not heap-backed still does\n");
    for (int k = 0; k < 2; k++) { reset(k ? LEGACY : NEW);
        Heap *h = [Heap new]; Member *a = mk(h, nil, NULL); Member *b = [Member new]; b->_heapBacked = NO; reg(h, b);
        for (int i = 0; i < 20; i++) { mk(h, nil, NULL); CB *cb = [CB new]; use_heap(cb, h); }   // through a flat collapse too
        CB *cb = [CB new]; use_heap(cb, h);
        arm(a->_resRefs == 0, k == 0, "heap-backed member: no reference taken", [NSString stringWithFormat:@"refs %d", a->_resRefs]);
        BOOL took = b->_resRefs >= 1; cb = nil; unkeep(h);
        arm(took && b->_resRefs == 0, YES, "plain member: references taken and all given back (incl. the flat pin's)",
            [NSString stringWithFormat:@"refs after release %d, flats %llu", b->_resRefs, gFlats]); }

    printf("A5 room: an out-of-room heap gets a released member's space back at once when no submission holds it\n");
    Policy a5[] = { NEW, NOUNK };
    for (int k = 0; k < 2; k++) { reset(a5[k]);
        int freed = 0; Heap *h = [Heap new]; Member *x = mk(h, nil, &freed); (void)x;
        { CB *cb = [CB new]; use_heap(cb, h); }
        x = nil; int before = freed; unkeep(h);
        arm(before == 0 && freed == 1, k == 0, "kept-only member freed by the out-of-room unkeep", [NSString stringWithFormat:@"freed before %d after %d", before, freed]); }

    printf("A6 cycle: member -> heap -> membership -> kept pin -> member; the timer breaks it after the app drops everything\n");
    Policy a6[] = { NEW, NOTIM };
    for (int k = 0; k < 2; k++) { reset(a6[k]);
        int mfreed = 0, hfreed = 0;
        @autoreleasepool { Heap *h = [Heap new]; h->_freed = &hfreed; Member *x = mk(h, nil, &mfreed); x->_heap = h;
            CB *cb = [CB new]; use_heap(cb, h); cb = nil; }
        gNow += 250 * MS; tick();
        arm(mfreed == 1 && hfreed == 1, k == 0, "member and heap freed after the window", [NSString stringWithFormat:@"member freed %d heap freed %d", mfreed, hfreed]); }

    printf("A7 coverage: a member added after the kept pin is in the next submission's pin\n");
    { reset(NEW); Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
      for (int i = 0; i < 10; i++) mk(h, app, NULL);
      { CB *cb = [CB new]; use_heap(cb, h); }
      mk(h, app, NULL); gNow += 16 * MS;
      CB *cb = [CB new]; use_heap(cb, h); Pin *p = last_pin(cb);
      BOOL all = pin_holds_all(p, app);
      arm(all && p->_depth == 1, YES, "every live member covered, as a delta (no walk)", [NSString stringWithFormat:@"depth %u, walks %llu", p->_depth, gWalks]); }

    printf("A8 collapse: a 16-link chain collapses flat without a walk, covers every member, and a released member it\n"
           "   carries is still freed once the keep expires and the GPU drains\n");
    Policy a8[] = { NEW, RENEW };
    for (int k = 0; k < 2; k++) { reset(a8[k]);
        int freed = 0; Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        Member *x = mk(h, nil, &freed); (void)x;
        { CB *cb = [CB new]; use_heap(cb, h); } x = nil;
        Pin *last = nil;
        for (int i = 0; i < 20; i++) { mk(h, app, NULL); CB *cb = [CB new]; use_heap(cb, h); last = last_pin(cb); gNow += MS; }
        BOOL all = pin_holds_all(last, app);
        const uint64_t walksAtCollapse = gWalks, flats = gFlats; last = nil;
        for (int f = 0; f < 30; f++) { mk(h, app, NULL); CB *cb = [CB new]; use_heap(cb, h); cb = nil; gNow += 16 * MS; tick(); }
        if (k == 0) arm(walksAtCollapse == 1 && flats >= 1 && all, YES, "collapse is flat (1 walk total) and covers all",
                        [NSString stringWithFormat:@"walks %llu, flats %llu, covered %d", walksAtCollapse, flats, all]);
        arm(freed == 1, k == 0, "carried member freed after the window", [NSString stringWithFormat:@"freed %d, walks %llu", freed, gWalks]); }

    printf("A9 measured pattern (Cyberpunk 11-07): one 6000-member heap, 3 command buffers per frame encoded together, a\n"
           "   member registered while their pins are built, GPU drained each frame, 1000 frames at 65 fps (15.4 s)\n");
    Policy a9[] = { NEW, V1, LEGACY, NOSF };
    uint64_t w9[4];
    for (int k = 0; k < 4; k++) { reset(a9[k]);
        Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        for (int i = 0; i < 6000; i++) mk(h, app, NULL);
        for (int f = 0; f < 1000; f++) {
            STEP(use_together(@[[CB new], [CB new], [CB new]], h, ^{ mk(h, app, NULL); }));   // the array literal autoreleases
            gNow += 15385000; tick(); }
        w9[k] = gWalks;
        // one walk per 200 ms keep window = 77, plus the first
        arm(gWalks <= 80, k == 0, "at most one walk per keep window",
            [NSString stringWithFormat:@"%llu walks over %llu members, %llu deltas, %llu flats, %llu reuses, %llu waits", gWalks, gWalked, gDeltas, gFlats, gReuses, gWaits]); }
    printf("     walks: legacy %llu, v1 %llu, new %llu\n", w9[2], w9[1], w9[0]);

    printf("A10 coverage under the race: a member registered while a pin is being built is in the next use's pin\n");
    Policy a10[] = { NEW, CLR };
    for (int k = 0; k < 2; k++) { reset(a10[k]);
        Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new]; for (int i = 0; i < 10; i++) mk(h, app, NULL);
        __block Member *late = nil;
        STEP(use_together(@[[CB new]], h, ^{ late = mk(h, app, NULL); }));
        CB *cb = [CB new]; use_heap(cb, h); Pin *p = last_pin(cb); const BOOL held = pin_holds(p, late);
        arm(held, k == 0, "late member pinned (else the GPU reads an unpinned resource)", [NSString stringWithFormat:@"held %d", held]); }

    printf("A11 single build: two command buffers that miss together walk the heap once\n");
    Policy a11[] = { NEW, NOSF };
    for (int k = 0; k < 2; k++) { reset(a11[k]);
        Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new]; for (int i = 0; i < 100; i++) mk(h, app, NULL);
        CB *a = [CB new], *b = [CB new]; STEP(use_together(@[a, b], h, nil));
        Pin *pb = last_pin(b); Member *lastM; STEP(lastM = app.lastObject); const BOOL held = pin_holds(pb, lastM);
        arm(gWalks == 1 && held, k == 0, "one walk, the second reuses it",
            [NSString stringWithFormat:@"walks %llu, waits %llu, reuses %llu", gWalks, gWaits, gReuses]); }

    printf("%s: %d arm(s) off\n", gFail ? "FAIL" : "PASS", gFail);
    return gFail ? 1 : 0;
} }
