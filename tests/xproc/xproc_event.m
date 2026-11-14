// Cross-process MTLSharedEvent oracle.
//
// One binary, two roles. The server is started by launchd (MachServices) and exports an
// NSXPCListener; the client creates a shared event, sends its MTLSharedEventHandle through a real
// NSXPCConnection (the only coder Apple's handle accepts), and the server rebuilds the event with
// -[MTLDevice newSharedEventWithHandle:]. Run xproc_run.sh, which installs and removes the
// transient launchd job.
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#include <mach/mach_time.h>
#include <mach/mach.h>
#import <objc/message.h>

#define SVC "org.nullmoth.xproc-oracle"

// Default: the system Metal device (Apple's driver on a Mac, the plugin on the NVIDIA box).
// -DXPROC_MOCK: the plugin's NVMTLSharedEventXProc.m on a stand-in event class, so the same arms run on any Mac.
#ifdef XPROC_MOCK
#include "xproc_mock.h"
#define NVXP_NEW_EVENT() mock_new_event()
#define NVXP_FROM_HANDLE(h) mock_event_from_handle(h)
#else
#define NVXP_NEW_EVENT() [gDev newSharedEvent]
#define NVXP_FROM_HANDLE(h) [gDev newSharedEventWithHandle:(h)]
#endif

@protocol XPOracle
- (void)waitOnHandle:(MTLSharedEventHandle *)h value:(uint64_t)v timeoutMS:(uint64_t)ms
               reply:(void (^)(BOOL ok, uint64_t seen, NSString *cls, NSString *label))reply;
- (void)signalHandle:(MTLSharedEventHandle *)h value:(uint64_t)v reply:(void (^)(BOOL ok))reply;
- (void)notifyOnHandle:(MTLSharedEventHandle *)h value:(uint64_t)v reply:(void (^)(BOOL fired, uint64_t seen))reply;
- (void)echoHandle:(MTLSharedEventHandle *)h reply:(void (^)(MTLSharedEventHandle *back))reply;
- (void)pingPong:(MTLSharedEventHandle *)ping pong:(MTLSharedEventHandle *)pong rounds:(uint64_t)n reply:(void (^)(BOOL ok))reply;
@end

static NSXPCInterface *iface(void) {
    NSXPCInterface *i = [NSXPCInterface interfaceWithProtocol:@protocol(XPOracle)];
    NSSet *hs = [NSSet setWithObject:[MTLSharedEventHandle class]];
    [i setClasses:hs forSelector:@selector(waitOnHandle:value:timeoutMS:reply:) argumentIndex:0 ofReply:NO];
    [i setClasses:hs forSelector:@selector(signalHandle:value:reply:) argumentIndex:0 ofReply:NO];
    [i setClasses:hs forSelector:@selector(notifyOnHandle:value:reply:) argumentIndex:0 ofReply:NO];
    [i setClasses:hs forSelector:@selector(echoHandle:reply:) argumentIndex:0 ofReply:NO];
    [i setClasses:hs forSelector:@selector(echoHandle:reply:) argumentIndex:0 ofReply:YES];
    [i setClasses:hs forSelector:@selector(pingPong:pong:rounds:reply:) argumentIndex:0 ofReply:NO];
    [i setClasses:hs forSelector:@selector(pingPong:pong:rounds:reply:) argumentIndex:1 ofReply:NO];
    return i;
}

static id<MTLDevice> gDev;

@interface Server : NSObject <XPOracle, NSXPCListenerDelegate> @end
@implementation Server
- (BOOL)listener:(NSXPCListener *)l shouldAcceptNewConnection:(NSXPCConnection *)c {
    c.exportedInterface = iface(); c.exportedObject = self; [c resume]; return YES;
}
- (void)waitOnHandle:(MTLSharedEventHandle *)h value:(uint64_t)v timeoutMS:(uint64_t)ms
               reply:(void (^)(BOOL, uint64_t, NSString *, NSString *))reply {
    id<MTLSharedEvent> e = NVXP_FROM_HANDLE(h);
    if (!e) { reply(NO, 0, @"(nil)", @""); return; }
    BOOL ok = [e waitUntilSignaledValue:v timeoutMS:ms];
    reply(ok, e.signaledValue, NSStringFromClass(object_getClass(e)), e.label ?: @"");
}
- (void)signalHandle:(MTLSharedEventHandle *)h value:(uint64_t)v reply:(void (^)(BOOL))reply {
    id<MTLSharedEvent> e = NVXP_FROM_HANDLE(h);
    if (e) e.signaledValue = v;
    reply(e != nil);
}
- (void)notifyOnHandle:(MTLSharedEventHandle *)h value:(uint64_t)v reply:(void (^)(BOOL, uint64_t))reply {
    id<MTLSharedEvent> e = NVXP_FROM_HANDLE(h);
    if (!e) { reply(NO, 0); return; }
    dispatch_semaphore_t s = dispatch_semaphore_create(0);
    MTLSharedEventListener *l = [[MTLSharedEventListener alloc] initWithDispatchQueue:dispatch_queue_create("xp.l", 0)];
    __block uint64_t seen = 0;
    [e notifyListener:l atValue:v block:^(id<MTLSharedEvent> ev, uint64_t val) { seen = val; dispatch_semaphore_signal(s); }];
    BOOL fired = dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) == 0;
    reply(fired, seen);
}
- (void)echoHandle:(MTLSharedEventHandle *)h reply:(void (^)(MTLSharedEventHandle *))reply {
    id<MTLSharedEvent> e = NVXP_FROM_HANDLE(h);
    reply(e ? [e newSharedEventHandle] : nil);
}
- (void)pingPong:(MTLSharedEventHandle *)ping pong:(MTLSharedEventHandle *)pong rounds:(uint64_t)n reply:(void (^)(BOOL))reply {
    id<MTLSharedEvent> a = NVXP_FROM_HANDLE(ping), b = NVXP_FROM_HANDLE(pong);
    BOOL ok = a && b;
    for (uint64_t i = 1; ok && i <= n; i++) {
        ok = [a waitUntilSignaledValue:i timeoutMS:5000];
        if (ok) b.signaledValue = i;
    }
    reply(ok);
}
@end

static int fails = 0, negs = 0;
static void arm(const char *name, BOOL ok, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    printf("%s %s: %s\n", ok ? "PASS" : "FAIL", name, buf);
    if (!ok) fails++;
}
static double nowms(void) {
    static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb);
    return (double)mach_absolute_time() * tb.numer / tb.denom / 1e6;
}
#define BLK(var, call) ({ __block id _r; dispatch_semaphore_t _s = dispatch_semaphore_create(0); call; \
    BOOL _ok = dispatch_semaphore_wait(_s, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC)) == 0; if (!_ok) printf("FAIL xpc call timed out\n"); _ok; })

static int client(void) {
    NSXPCConnection *c = [[NSXPCConnection alloc] initWithMachServiceName:@SVC options:0];
    c.remoteObjectInterface = iface();
    c.invalidationHandler = ^{ printf("note: connection invalidated\n"); };
    [c resume];
    __block int errs = 0;
    id<XPOracle> srv = [c remoteObjectProxyWithErrorHandler:^(NSError *e) { errs++; printf("xpc error: %s\n", e.description.UTF8String); }];

    id<MTLSharedEvent> ev = NVXP_NEW_EVENT();
    ev.label = @"xproc-oracle";
    MTLSharedEventHandle *h = [ev newSharedEventHandle];
    arm("handle exists", h != nil, "class=%s label=%s", h ? class_getName(object_getClass(h)) : "(nil)", h.label.UTF8String ?: "(nil)");
    if (!h) return 1;
    arm("handle is secure-coding", [[h class] supportsSecureCoding], "supportsSecureCoding=%d", [[h class] supportsSecureCoding]);

    // Apple's handle refuses a non-XPC coder.
    BOOL threw = NO;
    @try { (void)[NSKeyedArchiver archivedDataWithRootObject:h requiringSecureCoding:YES error:nil]; }
    @catch (NSException *x) { threw = YES; }
    NSError *er = nil;
    NSData *d = [NSKeyedArchiver archivedDataWithRootObject:h requiringSecureCoding:YES error:&er];
    arm("keyed archiver refuses handle", d == nil, "data=%lu err=%s", (unsigned long)d.length, er ? "yes" : "none");
    (void)threw;

    // 1. value crosses process: client signals, server (separate process) wakes.
    {
        __block BOOL ok = NO; __block uint64_t seen = 0; __block NSString *cls = nil, *lbl = nil;
        dispatch_semaphore_t s = dispatch_semaphore_create(0);
        double t0 = nowms();
        [srv waitOnHandle:h value:7 timeoutMS:5000 reply:^(BOOL o, uint64_t v, NSString *k, NSString *l) { ok = o; seen = v; cls = k; lbl = l; dispatch_semaphore_signal(s); }];
        usleep(300000);
        ev.signaledValue = 7;
        BOOL got = dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) == 0;
        double dt = nowms() - t0;
        arm("parent signal -> child wake", got && ok && seen >= 7, "ok=%d seen=%llu class=%s total=%.0fms (300ms sleep incl.)", ok, seen, cls.UTF8String, dt);
        arm("wake latency bound", got && dt < 1500, "%.0fms", dt);
        arm("label survives the handle", [lbl isEqualToString:@"xproc-oracle"], "label=%s", lbl.UTF8String);
    }
    // 2. child signals, parent waits.
    {
        dispatch_semaphore_t s = dispatch_semaphore_create(0); __block BOOL ok = NO;
        [srv signalHandle:h value:11 reply:^(BOOL o) { ok = o; dispatch_semaphore_signal(s); }];
        BOOL w = [ev waitUntilSignaledValue:11 timeoutMS:5000];
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
        arm("child signal -> parent wait", w && ok && ev.signaledValue == 11, "wait=%d value=%llu", w, ev.signaledValue);
    }
    // 3. notifyListener in the other process.
    {
        __block BOOL fired = NO; __block uint64_t seen = 0; dispatch_semaphore_t s = dispatch_semaphore_create(0);
        [srv notifyOnHandle:h value:20 reply:^(BOOL f, uint64_t v) { fired = f; seen = v; dispatch_semaphore_signal(s); }];
        usleep(300000); ev.signaledValue = 20;
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
        arm("notifyListener fires in child", fired && seen == 20, "fired=%d seen=%llu", fired, seen);
    }
    // 4. timeout arm: nothing signals 500, wait must return NO after about the timeout.
    {
        __block BOOL ok = YES; dispatch_semaphore_t s = dispatch_semaphore_create(0); double t0 = nowms();
        [srv waitOnHandle:h value:500 timeoutMS:300 reply:^(BOOL o, uint64_t v, NSString *k, NSString *l) { ok = o; dispatch_semaphore_signal(s); }];
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
        double dt = nowms() - t0;
        arm("child timeout returns NO", !ok && dt >= 250 && dt < 2000, "ok=%d after %.0fms (asked 300)", ok, dt);
        t0 = nowms(); BOOL lw = [ev waitUntilSignaledValue:500 timeoutMS:200]; dt = nowms() - t0;
        arm("parent timeout returns NO", !lw && dt >= 150 && dt < 1500, "ok=%d after %.0fms (asked 200)", lw, dt);
    }
    // 5. a handle survives a round trip and names the same event.
    {
        __block MTLSharedEventHandle *back = nil; dispatch_semaphore_t s = dispatch_semaphore_create(0);
        [srv echoHandle:h reply:^(MTLSharedEventHandle *b) { back = b; dispatch_semaphore_signal(s); }];
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
        id<MTLSharedEvent> e2 = back ? NVXP_FROM_HANDLE(back) : nil;
        BOOL same = NO;
        if (e2) { e2.signaledValue = 33; same = ev.signaledValue == 33; }
        arm("handle round trip names same event", same, "back=%d e2=%d value=%llu", back != nil, e2 != nil, ev.signaledValue);
        // value only moves forward? probe what Apple does when set lower.
        ev.signaledValue = 5;
        printf("INFO set lower: value after setSignaledValue(5) from 33 = %llu\n", ev.signaledValue);
    }
    // 6. ping-pong latency across processes.
    {
        id<MTLSharedEvent> ping = NVXP_NEW_EVENT(), pong = NVXP_NEW_EVENT();
        dispatch_semaphore_t s = dispatch_semaphore_create(0); __block BOOL ok = NO; const uint64_t N = 200;
        [srv pingPong:[ping newSharedEventHandle] pong:[pong newSharedEventHandle] rounds:N reply:^(BOOL o) { ok = o; dispatch_semaphore_signal(s); }];
        usleep(200000);
        double t0 = nowms(); BOOL all = YES;
        for (uint64_t i = 1; i <= N && all; i++) { ping.signaledValue = i; all = [pong waitUntilSignaledValue:i timeoutMS:5000]; }
        double dt = nowms() - t0;
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
        arm("ping-pong 200 rounds", all && ok, "%.1fus per round trip", dt * 1000 / N);
    }
#ifdef XPROC_MOCK
    // 6b. A command buffer waiting on an exported event must be re-pumped when another process signals it.
    {
        id<MTLSharedEvent> pe = NVXP_NEW_EVENT();
        MTLSharedEventHandle *ph = [pe newSharedEventHandle];
        int before = atomic_load(&gMockPumps);
        ((void (*)(id, SEL, uint64_t))objc_msgSend)(pe, @selector(nvmtlXpWatchPump:), 5);
        dispatch_semaphore_t s = dispatch_semaphore_create(0);
        [srv signalHandle:ph value:5 reply:^(BOOL o) { dispatch_semaphore_signal(s); }];
        dispatch_semaphore_wait(s, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
        usleep(200000);
        arm("remote signal re-pumps a waiting command buffer", atomic_load(&gMockPumps) > before, "pumps %d -> %d", before, atomic_load(&gMockPumps));
    }
    // 6c. Export and import must not leak ports or mappings.
    {
        mach_port_name_array_t names; mach_msg_type_number_t nn, nt; mach_port_type_array_t types; 
        mach_port_names(mach_task_self(), &names, &nn, &types, &nt); int n0 = nn;
        vm_deallocate(mach_task_self(), (vm_address_t)names, nn * sizeof *names); vm_deallocate(mach_task_self(), (vm_address_t)types, nt * sizeof *types);
        for (int i = 0; i < 300; i++) @autoreleasepool {
            id<MTLSharedEvent> a = NVXP_NEW_EVENT(); MTLSharedEventHandle *hh = [a newSharedEventHandle];
            id<MTLSharedEvent> b = NVXP_FROM_HANDLE(hh); b.signaledValue = 3;
            if (a.signaledValue != 3) { fails++; printf("FAIL import did not share value\n"); break; }
        }
        usleep(100000);
        mach_port_names(mach_task_self(), &names, &nn, &types, &nt); int n1 = nn;
        vm_deallocate(mach_task_self(), (vm_address_t)names, nn * sizeof *names); vm_deallocate(mach_task_self(), (vm_address_t)types, nt * sizeof *types);
        arm("300 export/import cycles leak no port names", n1 - n0 < 10, "port names %d -> %d", n0, n1);
    }
#endif
    // 7. NEGATIVE arm: expects the wrong thing on purpose (a never-signalled value completes). Must FAIL.
    {
        BOOL w = [ev waitUntilSignaledValue:9999 timeoutMS:100];
        BOOL fired = (w == YES);
        printf("%s NEG never-signalled wait reports done (must FAIL): wait=%d\n", fired ? "PASS" : "FAIL", w);
        if (fired) { printf("UNEXPECTED: negative arm passed\n"); fails++; } else negs++;
    }
    arm("no xpc errors", errs == 0, "errors=%d", errs);
    printf("%s: %d unexpected failures, %d negative arm(s) failed as designed\n", fails ? "RESULT FAIL" : "RESULT OK", fails, negs);
    return fails ? 1 : 0;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        gDev = MTLCreateSystemDefaultDevice();
        if (!gDev) { printf("no device\n"); return 2; }
        printf("device class %s\n", class_getName(object_getClass(gDev)));
        if (argc > 1 && !strcmp(argv[1], "server")) {
            static Server *s; s = [Server new];
            NSXPCListener *l = [[NSXPCListener alloc] initWithMachServiceName:@SVC];
            l.delegate = s; [l resume];
            [[NSRunLoop currentRunLoop] run];
            return 0;
        }
        return client();
    }
}
