// test_heap_pin.m - the heap pin's lifetime and cost rules, modelled without a GPU.
//
// nvmtl_use_heap (plugin/NVMTLObjects.m) pins every member of a used heap for the submission: the pin holds each
// member strongly until the last submission naming it finishes, so a member the app releases is not destroyed under
// the GPU. This model copies the membership / pin / keep logic of that function line for line onto plain objects
// (a member is an object that counts its deallocs and its residency references; a command buffer is an array that
// holds pins until "completion"), with a fake clock so the keep window is deterministic. Each arm runs the NEW logic
// and, where it means something, the LEGACY logic (NVMTL_HEAP_PIN_LEGACY=1) or a deliberately broken variant that the
// arm must catch.
//
//   clang -fobjc-arc -O1 -framework Foundation tools/test_heap_pin.m -o /tmp/test_heap_pin && /tmp/test_heap_pin
#import <Foundation/Foundation.h>
#include <stdio.h>

typedef struct { BOOL legacy; uint64_t keepNs; BOOL noExpiry, noPrewalkDrop, noTimer, noUnkeep; const char *name; } Policy;
static Policy P;
static uint64_t gNow;                                   // fake clock, ns
static uint64_t gWalks, gWalked, gDeltas, gReuses, gRefsTaken, gRefsSkipped;
static NSMutableArray *gTimers;                         // deferred keep drops: {members (weak box), gen, deadline}

@class Heap;
@interface Member : NSObject { @public BOOL _heapBacked; int _resRefs; Heap *_heap; int *_freed; } @end
@implementation Member - (void)dealloc { if (_freed) (*_freed)++; } @end

static BOOL needs_ref(Member *m) { return P.legacy ? YES : !m->_heapBacked; }

@interface Pin : NSObject { @public NSArray *_members, *_own; Pin *_base; unsigned _depth; } @end
@implementation Pin
- (instancetype)initWithMembers:(NSArray *)members {
    if ((self = [super init])) {
        _members = members; NSMutableArray *own = nil;
        for (Member *m in members) {
            if (needs_ref(m)) { m->_resRefs++; gRefsTaken++; if (!own) own = [NSMutableArray new]; [own addObject:m]; }
            else gRefsSkipped++;
        }
        _own = [own copy];
    }
    return self;
}
- (instancetype)initWithBase:(Pin *)base added:(NSArray *)added {
    if ((self = [self initWithMembers:added])) { _base = base; _depth = base->_depth + 1; }
    return self;
}
- (BOOL)holds:(id)o { for (Pin *p = self; p; p = p->_base) if ([p->_members containsObject:o]) return YES; return NO; }
- (void)dealloc { for (Member *m in _own) m->_resRefs--; }
@end

@interface Members : NSObject { @public NSHashTable *_members; uint64_t _version; __weak Pin *_pin; uint64_t _pinVersion; NSPointerArray *_added;
                                       Pin *_keep; uint64_t _keepUntil, _keepGen; } @end
@implementation Members
- (instancetype)init { if ((self = [super init])) { _members = [NSHashTable weakObjectsHashTable]; _added = [NSPointerArray weakObjectsPointerArray]; } return self; }
@end
@interface Heap : NSObject { @public Members *_m; int *_freed; } @end
@implementation Heap - (instancetype)init { if ((self = [super init])) _m = [Members new]; return self; } - (void)dealloc { if (_freed) (*_freed)++; } @end
@interface CB : NSObject { @public NSMutableArray *_held; NSMutableDictionary *_versions; } @end
@implementation CB - (instancetype)init { if ((self = [super init])) { _held = [NSMutableArray new]; _versions = [NSMutableDictionary new]; } return self; } @end

static Pin *unkeep_locked(Members *m) { Pin *k = m->_keep; m->_keep = nil; m->_keepUntil = 0; return k; }
static void reg(Heap *h, Member *r) {   // nvmtl_heap_register; its own pool, the weak table's lookups autorelease
    @autoreleasepool { Members *m = h->_m;
        if (![m->_members containsObject:r]) { [m->_members addObject:r]; [m->_added addPointer:(__bridge void *)r]; m->_version++; } }
}
static BOOL unkeep(Heap *h) { if (P.noUnkeep) return NO; Pin *k = unkeep_locked(h->_m); BOOL had = k != nil; k = nil; return had; }
static void tick(void) {   // the dispatch_after drops whose time has come
    NSMutableArray *left = [NSMutableArray new];
    for (NSArray *t in gTimers) {
        if (gNow < [t[2] unsignedLongLongValue]) { [left addObject:t]; continue; }
        Members *m = ((NSPointerArray *)t[0]).allObjects.firstObject; if (!m) continue;
        Pin *k = nil; if (m->_keepGen == [t[1] unsignedLongLongValue]) k = unkeep_locked(m); k = nil; (void)k;
    }
    gTimers = left;
}

// nvmtl_use_heap, minus residency/hazard calls and the locks (the model is single-threaded).
static void use_heap(CB *cb, Heap *heap) {
    Members *members = heap->_m;
    NSArray *resources = nil, *added = nil; Pin *pin = nil, *base = nil; uint64_t version;
    NSValue *key = [NSValue valueWithNonretainedObject:heap];
    const uint64_t keepNs = P.legacy ? 0 : P.keepNs;
    for (int attempt = 0;; attempt++) {
        Pin *dropped = nil; BOOL again = NO;
        if (!P.noExpiry && members->_keep && gNow >= members->_keepUntil) dropped = unkeep_locked(members);
        version = members->_version;
        NSNumber *prior = cb->_versions[key];
        if (!prior || prior.unsignedLongLongValue != version) {
            if (members->_pinVersion == version) pin = members->_pin;
            if (pin) gReuses++;
            else {
                base = members->_pin;
                if (base && base->_depth < 16) { [members->_added compact]; added = members->_added.allObjects; }
                else if (!P.noPrewalkDrop && (members->_keep || dropped) && attempt == 0) { base = nil; if (!dropped) dropped = unkeep_locked(members); again = YES; }
                else { base = nil; resources = members->_members.allObjects; }
            }
        }
        dropped = nil;
        if (!again) break;
        base = nil; pin = nil; added = nil; resources = nil;
    }
    if (resources) { gWalks++; gWalked += resources.count; }
    if (added) gDeltas++;
    if (resources || added) {
        pin = added ? [[Pin alloc] initWithBase:base added:added] : [[Pin alloc] initWithMembers:resources];
        Pin *replaced = nil; uint64_t keepGen = 0;
        if (members->_version == version) { members->_pin = pin; members->_pinVersion = version; members->_added = [NSPointerArray weakObjectsPointerArray];
            if (keepNs && resources) { replaced = members->_keep; members->_keep = pin; members->_keepUntil = gNow + keepNs; keepGen = ++members->_keepGen; }
            else if (members->_keep) { replaced = members->_keep; members->_keep = pin; } }
        replaced = nil; (void)replaced;
        if (keepGen && !P.noTimer) {
            NSPointerArray *w = [NSPointerArray weakObjectsPointerArray]; [w addPointer:(__bridge void *)members];
            [gTimers addObject:@[w, @(keepGen), @(gNow + keepNs)]];
        }
    }
    if (pin) { [cb->_held addObject:heap]; [cb->_held addObject:pin]; cb->_versions[key] = @(version); }
}

// The app's per-frame autorelease pool: -allObjects hands back an autoreleased array that holds every member until the
// pool drains, in the driver as here, so every use runs inside one.
#define USE(cb, h) do { @autoreleasepool { use_heap((cb), (h)); } } while (0)
static int gFail;
static void arm(BOOL ok, BOOL wantOk, const char *name, NSString *detail) {
    BOOL good = ok == wantOk; if (!good) gFail++;
    printf("  %s [%s] %s%s  (%s)\n", good ? "ok  " : "FAIL", P.name, name, wantOk ? "" : "  <- must NOT hold here", detail.UTF8String);
}
static void reset(Policy p) { P = p; gNow = 0; gWalks = gWalked = gDeltas = gReuses = gRefsTaken = gRefsSkipped = 0; gTimers = [NSMutableArray new]; }
static const uint64_t MS = 1000000;

int main(void) { @autoreleasepool {
    const Policy NEW = { .keepNs = 200 * MS, .name = "new" }, LEGACY = { .legacy = YES, .name = "legacy" };
    Policy NOEXP = NEW; NOEXP.noExpiry = YES; NOEXP.noTimer = YES; NOEXP.name = "broken: keep never expires";
    Policy NOUNK = NEW; NOUNK.noUnkeep = YES; NOUNK.name = "broken: no unkeep when out of room";
    Policy NOTIM = NEW; NOTIM.noTimer = YES; NOTIM.name = "broken: no keep timer";

    printf("A1 lifetime: a member the app releases while a submission that used its heap is in flight stays alive\n");
    for (int k = 0; k < 2; k++) { reset(k ? LEGACY : NEW);
        int freed = 0; Heap *h = [Heap new]; Member *x = [Member new]; x->_heapBacked = YES; x->_freed = &freed; reg(h, x);
        CB *cb = [CB new]; USE(cb, h); x = nil;                         // app drops it, GPU still running
        gNow += 300 * MS; tick();                                            // the keep window passes too
        const int before = freed; cb = nil;                                  // completion
        arm(before == 0 && freed == 1, YES, "released member alive until its submission completes, freed at completion",
            [NSString stringWithFormat:@"freed before completion %d, after %d", before, freed]); }

    printf("A2 release: once its submissions finish and the keep window passes, a released member is freed\n");
    Policy a2[] = { NEW, LEGACY, NOEXP };
    for (int k = 0; k < 3; k++) { reset(a2[k]);
        int freed = 0; Heap *h = [Heap new];
        NSMutableArray *keep = [NSMutableArray new];                         // the app's references
        for (int i = 0; i < 50; i++) { Member *m = [Member new]; m->_heapBacked = YES; [keep addObject:m]; reg(h, m); }
        Member *x = keep.lastObject; x->_freed = &freed; x = nil;
        for (int f = 0; f < 30; f++) {                                       // 30 frames at 16 ms, GPU drains each frame
            if (f == 3) [keep removeLastObject];                             // app drops one member in frame 3
            CB *cb = [CB new]; USE(cb, h); cb = nil; gNow += 16 * MS; tick(); }
        arm(freed == 1, k < 2, "released member freed within 30 frames (480 ms, keep 200 ms)", [NSString stringWithFormat:@"freed %d", freed]); }

    printf("A3 cost: 600 frames of a 4000-member heap that drains every frame, a member added every 10th frame\n");
    uint64_t walkedNew = 0, walkedOld = 0;
    for (int k = 0; k < 2; k++) { reset(k ? LEGACY : NEW);
        Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        for (int i = 0; i < 4000; i++) { Member *m = [Member new]; m->_heapBacked = YES; [app addObject:m]; reg(h, m); }
        for (int f = 0; f < 600; f++) {
            if (f % 10 == 0) { Member *m = [Member new]; m->_heapBacked = YES; [app addObject:m]; reg(h, m); }
            for (int e = 0; e < 3; e++) { CB *cb = [CB new]; USE(cb, h); }   // three command buffers per frame, all done by frame end
            gNow += 16 * MS; tick(); }
        *(k ? &walkedOld : &walkedNew) = gWalked;
        // 600 frames * 16 ms = 9.6 s; one walk per 200 ms window -> ~48, plus a collapse every 16 deltas
        arm(gWalks <= 60, k == 0, "at most one full walk per keep window",
            [NSString stringWithFormat:@"%llu full walks over %llu members, %llu deltas, %llu reuses, refs taken %llu",
             gWalks, gWalked, gDeltas, gReuses, gRefsTaken]); }
    printf("     members walked: legacy %llu -> new %llu (%.1fx fewer)\n", walkedOld, walkedNew, walkedNew ? (double)walkedOld / walkedNew : 0.0);

    printf("A4 refs: heap-backed members take no residency reference; a member that is not heap-backed still does\n");
    for (int k = 0; k < 2; k++) { reset(k ? LEGACY : NEW);
        Heap *h = [Heap new]; Member *a = [Member new]; a->_heapBacked = YES; Member *b = [Member new]; b->_heapBacked = NO; reg(h, a); reg(h, b);
        CB *cb = [CB new]; USE(cb, h);
        arm(a->_resRefs == 0, k == 0, "heap-backed member: no reference taken", [NSString stringWithFormat:@"refs %d", a->_resRefs]);
        BOOL took = b->_resRefs == 1; cb = nil; Pin *k2 = unkeep_locked(h->_m); k2 = nil; (void)k2;
        arm(took && b->_resRefs == 0, YES, "plain member: reference taken and given back", [NSString stringWithFormat:@"refs after release %d", b->_resRefs]); }

    printf("A5 room: an out-of-room heap gets a released member's space back at once when no submission holds it\n");
    Policy a5[] = { NEW, NOUNK };
    for (int k = 0; k < 2; k++) { reset(a5[k]);
        int freed = 0; Heap *h = [Heap new]; Member *x = [Member new]; x->_heapBacked = YES; x->_freed = &freed; reg(h, x);
        { CB *cb = [CB new]; USE(cb, h); }                              // submitted and completed, pin kept
        x = nil;                                                             // app releases, inside the keep window
        int before = freed; unkeep(h);                                       // newBufferWithLength: finds no room
        arm(before == 0 && freed == 1, k == 0, "kept-only member freed by the out-of-room unkeep", [NSString stringWithFormat:@"freed before %d after %d", before, freed]); }

    printf("A6 cycle: member -> heap -> membership -> kept pin -> member; the timer breaks it after the app drops everything\n");
    Policy a6[] = { NEW, NOTIM };
    for (int k = 0; k < 2; k++) { reset(a6[k]);
        int mfreed = 0, hfreed = 0;
        @autoreleasepool { Heap *h = [Heap new]; h->_freed = &hfreed; Member *x = [Member new]; x->_heapBacked = YES; x->_heap = h; x->_freed = &mfreed; reg(h, x);
            CB *cb = [CB new]; USE(cb, h); cb = nil; }                  // done; the app holds nothing now
        gNow += 250 * MS; tick();                                            // the heap is never used again
        arm(mfreed == 1 && hfreed == 1, k == 0, "member and heap freed after the window", [NSString stringWithFormat:@"member freed %d heap freed %d", mfreed, hfreed]); }

    printf("A7 coverage: a member added after the kept pin is in the next submission's pin\n");
    { reset(NEW); Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
      for (int i = 0; i < 10; i++) { Member *m = [Member new]; m->_heapBacked = YES; [app addObject:m]; reg(h, m); }
      { CB *cb = [CB new]; USE(cb, h); }
      Member *late = [Member new]; late->_heapBacked = YES; [app addObject:late]; reg(h, late); gNow += 16 * MS;
      CB *cb = [CB new]; USE(cb, h); Pin *p = cb->_held.lastObject;
      BOOL all = YES; for (Member *m in app) all = all && [p holds:m];
      arm(all && p->_depth == 1, YES, "every live member covered, as a delta (no walk)", [NSString stringWithFormat:@"depth %u, walks %llu", p->_depth, gWalks]); }

    printf("A8 walk hygiene: a depth-16 collapse inside the keep window does not carry a member the app released\n");
    Policy a8b = NEW; a8b.noPrewalkDrop = YES; a8b.name = "broken: no drop before walk";
    Policy a8[] = { NEW, a8b };
    for (int k = 0; k < 2; k++) { reset(a8[k]);
        int freed = 0; Heap *h = [Heap new]; NSMutableArray *app = [NSMutableArray new];
        Member *x = [Member new]; x->_heapBacked = YES; x->_freed = &freed; reg(h, x);
        { CB *cb = [CB new]; USE(cb, h); } x = nil;                     // app drops x inside the window
        for (int i = 0; i < 17; i++) { Member *m = [Member new]; m->_heapBacked = YES; [app addObject:m]; reg(h, m); CB *cb = [CB new]; USE(cb, h); gNow += MS; }
        arm(freed == 1, k == 0, "released member gone after the collapse walk", [NSString stringWithFormat:@"freed %d, walks %llu, deltas %llu", freed, gWalks, gDeltas]); }

    printf("%s: %d arm(s) off\n", gFail ? "FAIL" : "PASS", gFail);
    return gFail ? 1 : 0;
} }
