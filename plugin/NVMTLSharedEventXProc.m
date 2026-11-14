// Cross-process MTLSharedEvent.
//
// Apple's MTLSharedEventHandle is an NSXPCCoder-only value holding one mach port send right plus a label
// (encodeWithCoder: writes "Port" and "Label"; it throws for any other coder). We reuse that class unchanged: it is
// built from an event that answers -eventPort, and the port it carries is what the receiving process hands back to
// -[MTLDevice newSharedEventWithHandle:].
//
// Our port is a plain mach port owned by the exporting process:
//   - exporting an event allocates one page of anonymous shared memory (a memory entry) that holds the 64-bit value,
//     plus a receive right whose send right travels in the handle;
//   - one service thread per process answers a "map" request on those ports with the memory entry;
//   - the importer maps the page, so both processes read and write the same 64-bit word.
// Wake-up across processes is os_sync_wait_on_address / wake with the SHARED flag on that word, which keys on the
// physical page, so a signal in one process wakes a waiter in another. The value only moves forward, as Apple's does.
//
// An event that is never exported keeps its value in the ivar and pays nothing. After export the page is the single
// source of truth, so the GPU-side signal and wait of our command buffers (setSignaledValue: / signaledValue) work
// unchanged on a shared event. Remote signals do not pass through this process, so a command buffer waiting on one is
// re-pumped by a watcher thread (nvmtlXpWatchPump:), and pending notifyListener blocks are fired by the same thread.
//
// If the exporting process exits, an importer's mapping stays valid but nothing will signal it again. Apple's event
// has the same dependence on the creating client.

#import <mach/mach.h>
#import <os/os_sync_wait_on_address.h>
#include <stdatomic.h>

#define NVXP_MAGIC       0x3150584Eu     // 'NXP1'
#define NVXP_MAP_ID      0x4E585031
#define NVXP_REPLY_ID    (0x4E585031 + 100)
#define NVXP_TIMEOUT_MS  2000
#define NVXP_MAX_SERVICE 4096

typedef struct { uint32_t magic, version; _Atomic uint64_t value; } nvxp_shared;

typedef struct nvxp {
    nvxp_shared *sh;
    vm_size_t size;
    mach_port_t port;       // exporter: receive right plus the send right we made; importer: a send right
    mach_port_t entry;      // exporter only
    int owner;
    int watcher;            // a watcher thread is running; guarded by the event's condition
    uint64_t pumpWant;      // 0 = no command buffer waits; guarded by the event's condition
} nvxp;

typedef struct {
    mach_msg_header_t h;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t d;
    uint32_t magic, version;
    uint64_t size;
} nvxp_reply;

@interface MTLSharedEventHandle (NVXP)
- (instancetype)initWithSharedEvent:(id)event;
- (mach_port_t)eventPort;
@end

static _Atomic uint64_t gxp_handles, gxp_imports, gxp_import_fail, gxp_timeouts;

// ---- the per-process service thread ----------------------------------------------------------------------------

static struct { mach_port_t port, entry; } gxp_svc[NVXP_MAX_SERVICE];
static int gxp_svc_n;
static pthread_mutex_t gxp_svc_lock = PTHREAD_MUTEX_INITIALIZER;
static mach_port_t gxp_set;

static void *nvxp_service_main(void *unused) {
    pthread_setname_np("nvmtl-xproc-events");
    for (;;) {
        union { mach_msg_header_t h; char raw[512]; } in;
        mach_msg_return_t kr = mach_msg(&in.h, MACH_RCV_MSG, 0, sizeof in, gxp_set, MACH_MSG_TIMEOUT_NONE, MACH_PORT_NULL);
        if (kr == MACH_RCV_TOO_LARGE) continue;     // the kernel destroyed the message
        if (kr != KERN_SUCCESS) { nvlog("xproc-event: service thread stopped, receive %d (pid %d) cannot answer imports", kr, getpid()); return NULL; }
        mach_port_t reply = in.h.msgh_remote_port;
        mach_port_t on = in.h.msgh_local_port;
        BOOL ok = in.h.msgh_id == NVXP_MAP_ID && !(in.h.msgh_bits & MACH_MSGH_BITS_COMPLEX) && reply != MACH_PORT_NULL
            && MACH_MSGH_BITS_REMOTE(in.h.msgh_bits) == MACH_MSG_TYPE_MOVE_SEND_ONCE;
        if (!ok) { mach_msg_destroy(&in.h); continue; }
        nvxp_reply r; memset(&r, 0, sizeof r);
        r.h.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0) | MACH_MSGH_BITS_COMPLEX;
        r.h.msgh_size = sizeof r; r.h.msgh_remote_port = reply; r.h.msgh_id = NVXP_REPLY_ID;
        r.body.msgh_descriptor_count = 1;
        r.d.disposition = MACH_MSG_TYPE_COPY_SEND; r.d.type = MACH_MSG_PORT_DESCRIPTOR;
        r.magic = NVXP_MAGIC; r.version = 1; r.size = 0;
        pthread_mutex_lock(&gxp_svc_lock);
        for (int i = 0; i < gxp_svc_n; i++) if (gxp_svc[i].port == on) { r.d.name = gxp_svc[i].entry; r.size = vm_page_size; break; }
        if (r.d.name == MACH_PORT_NULL) {
            // the event is gone: reply with no descriptor so the importer fails at once instead of timing out
            r.h.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0); r.h.msgh_size = sizeof(mach_msg_header_t); r.h.msgh_id = NVXP_REPLY_ID + 1;
        }
        mach_msg(&r.h, MACH_SEND_MSG | MACH_SEND_TIMEOUT, r.h.msgh_size, 0, MACH_PORT_NULL, 1000, MACH_PORT_NULL);
        pthread_mutex_unlock(&gxp_svc_lock);
    }
}

static int nvxp_service_start(void) {
    static dispatch_once_t once; static int ok;
    dispatch_once(&once, ^{
        if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_PORT_SET, &gxp_set) != KERN_SUCCESS) return;
        pthread_t t; pthread_attr_t a; pthread_attr_init(&a); pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
        pthread_attr_setstacksize(&a, 256 * 1024);
        ok = pthread_create(&t, &a, nvxp_service_main, NULL) == 0;
    });
    return ok;
}

// ---- export and import -----------------------------------------------------------------------------------------

static void nvxp_free(nvxp *x) {
    if (!x) return;
    if (x->owner) {
        pthread_mutex_lock(&gxp_svc_lock);
        for (int i = 0; i < gxp_svc_n; i++) if (gxp_svc[i].port == x->port) { gxp_svc[i] = gxp_svc[--gxp_svc_n]; break; }
        pthread_mutex_unlock(&gxp_svc_lock);
        if (x->port) mach_port_destruct(mach_task_self(), x->port, -1, 0);
        if (x->entry) mach_port_deallocate(mach_task_self(), x->entry);
    } else if (x->port) mach_port_deallocate(mach_task_self(), x->port);
    if (x->sh) vm_deallocate(mach_task_self(), (vm_address_t)x->sh, x->size);
    free(x);
}

// Called with the event's condition held and _xp still NULL. Returns NULL (and logs) when the kernel refuses.
static nvxp *nvxp_export(uint64_t value) {
    if (!nvxp_service_start()) { nvlog("xproc-event: cannot export, the service thread did not start (pid %d)", getpid()); return NULL; }
    nvxp *x = calloc(1, sizeof *x); if (!x) return NULL;
    x->owner = 1; x->size = vm_page_size;
    memory_object_size_t sz = x->size; kern_return_t kr;
    if ((kr = mach_make_memory_entry_64(mach_task_self(), &sz, 0, MAP_MEM_NAMED_CREATE | VM_PROT_DEFAULT, &x->entry, MACH_PORT_NULL)) != KERN_SUCCESS) {
        nvlog("xproc-event: cannot export, memory entry %d (pid %d)", kr, getpid()); nvxp_free(x); return NULL; }
    vm_address_t addr = 0;
    if ((kr = vm_map(mach_task_self(), &addr, x->size, 0, VM_FLAGS_ANYWHERE, x->entry, 0, FALSE, VM_PROT_DEFAULT, VM_PROT_DEFAULT, VM_INHERIT_NONE)) != KERN_SUCCESS) {
        nvlog("xproc-event: cannot export, vm_map %d (pid %d)", kr, getpid()); nvxp_free(x); return NULL; }
    x->sh = (nvxp_shared *)addr;
    x->sh->magic = NVXP_MAGIC; x->sh->version = 1; atomic_store(&x->sh->value, value);
    if ((kr = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &x->port)) != KERN_SUCCESS
        || (kr = mach_port_insert_right(mach_task_self(), x->port, x->port, MACH_MSG_TYPE_MAKE_SEND)) != KERN_SUCCESS) {
        nvlog("xproc-event: cannot export, port %d (pid %d)", kr, getpid());
        if (x->port) { mach_port_t p = x->port; x->port = MACH_PORT_NULL; mach_port_mod_refs(mach_task_self(), p, MACH_PORT_RIGHT_RECEIVE, -1); }
        nvxp_free(x); return NULL; }
    pthread_mutex_lock(&gxp_svc_lock);
    int full = gxp_svc_n == NVXP_MAX_SERVICE;
    if (!full) { gxp_svc[gxp_svc_n].port = x->port; gxp_svc[gxp_svc_n].entry = x->entry; gxp_svc_n++; }
    pthread_mutex_unlock(&gxp_svc_lock);
    if (full || (kr = mach_port_move_member(mach_task_self(), x->port, gxp_set)) != KERN_SUCCESS) {
        nvlog("xproc-event: cannot export, %s (pid %d)", full ? "service table full" : "port set", getpid());
        nvxp_free(x); return NULL; }
    return x;
}

// Asks the exporter for its memory entry and maps it. `port` stays owned by the caller on failure.
static nvxp *nvxp_map(mach_port_t port) {
    mach_port_t rp = MACH_PORT_NULL;
    if (mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &rp) != KERN_SUCCESS) return NULL;
    union { mach_msg_header_t h; char raw[512]; } buf; memset(&buf, 0, sizeof buf);
    buf.h.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE);
    buf.h.msgh_size = sizeof(mach_msg_header_t); buf.h.msgh_remote_port = port; buf.h.msgh_local_port = rp; buf.h.msgh_id = NVXP_MAP_ID;
    mach_msg_return_t kr = mach_msg(&buf.h, MACH_SEND_MSG | MACH_RCV_MSG | MACH_SEND_TIMEOUT | MACH_RCV_TIMEOUT,
                                    sizeof(mach_msg_header_t), sizeof buf, rp, NVXP_TIMEOUT_MS, MACH_PORT_NULL);
    mach_port_mod_refs(mach_task_self(), rp, MACH_PORT_RIGHT_RECEIVE, -1);
    if (kr != KERN_SUCCESS) { nvlog("xproc-event: import cannot reach the owner, mach_msg %d (pid %d)", kr, getpid()); return NULL; }
    nvxp_reply *r = (nvxp_reply *)&buf;
    if (buf.h.msgh_id != NVXP_REPLY_ID || !(buf.h.msgh_bits & MACH_MSGH_BITS_COMPLEX) || r->body.msgh_descriptor_count != 1
        || r->d.type != MACH_MSG_PORT_DESCRIPTOR || r->magic != NVXP_MAGIC || r->version != 1 || r->size == 0) {
        nvlog("xproc-event: import refused by the owner, reply id %d (pid %d)", buf.h.msgh_id, getpid());
        if (buf.h.msgh_bits & MACH_MSGH_BITS_COMPLEX) mach_msg_destroy(&buf.h);
        return NULL; }
    vm_address_t addr = 0;
    kr = vm_map(mach_task_self(), &addr, (vm_size_t)r->size, 0, VM_FLAGS_ANYWHERE, r->d.name, 0, FALSE, VM_PROT_DEFAULT, VM_PROT_DEFAULT, VM_INHERIT_NONE);
    mach_port_deallocate(mach_task_self(), r->d.name);
    if (kr != KERN_SUCCESS) { nvlog("xproc-event: import cannot map the value page, vm_map %d (pid %d)", kr, getpid()); return NULL; }
    nvxp *x = calloc(1, sizeof *x); if (!x) { vm_deallocate(mach_task_self(), addr, (vm_size_t)r->size); return NULL; }
    x->sh = (nvxp_shared *)addr; x->size = (vm_size_t)r->size;
    if (x->sh->magic != NVXP_MAGIC) { nvlog("xproc-event: import refused, value page has the wrong magic (pid %d)", getpid()); nvxp_free(x); return NULL; }
    return x;
}

// ---- value, wait, notify ---------------------------------------------------------------------------------------

static inline uint64_t nvxp_now_ns(void) { return clock_gettime_nsec_np(CLOCK_MONOTONIC); }

static uint64_t nvmtl_sev_read(NVMTLSharedEvent *e) {
    [e->_condition lock];
    nvxp *x = (nvxp *)e->_xp;
    uint64_t v = x ? atomic_load(&x->sh->value) : e->_value;
    [e->_condition unlock];
    return v;
}

static void nvxp_watch_locked(NVMTLSharedEvent *e);

static void nvmtl_sev_signal(NVMTLSharedEvent *e, uint64_t value) {
    NSMutableArray *ready = [NSMutableArray new];
    BOOL grew = NO; nvxp *x;
    [e->_condition lock];
    x = (nvxp *)e->_xp;
    if (x) {
        uint64_t cur = atomic_load(&x->sh->value);
        while (value > cur && !atomic_compare_exchange_weak(&x->sh->value, &cur, value)) {}
        grew = value > cur; if (!grew) value = cur;
    } else {
        grew = value > e->_value; if (grew) e->_value = value; else value = e->_value;
    }
    for (NSArray *n in e->_notifications) if ([n[1] unsignedLongLongValue] <= value) [ready addObject:n];
    [e->_notifications removeObjectsInArray:ready];
    [e->_condition broadcast];
    [e->_condition unlock];
    if (x && grew) os_sync_wake_by_address_all(&x->sh->value, sizeof(uint64_t), OS_SYNC_WAKE_BY_ADDRESS_SHARED);
    for (NSArray *n in ready) {
        MTLSharedEventListener *l = n[0]; MTLSharedEventNotificationBlock b = n[2];
        dispatch_async(l.dispatchQueue, ^{ b(e, value); });
    }
    nvmtl_pump_buffers();
}

static void nvmtl_sev_notify(NVMTLSharedEvent *e, MTLSharedEventListener *l, uint64_t wanted, MTLSharedEventNotificationBlock b) {
    if (!l || !b) return;
    [e->_condition lock];
    nvxp *x = (nvxp *)e->_xp;
    uint64_t value = x ? atomic_load(&x->sh->value) : e->_value;
    if (value < wanted) { [e->_notifications addObject:@[l, @(wanted), [b copy]]]; if (x) nvxp_watch_locked(e); }
    [e->_condition unlock];
    if (value >= wanted) dispatch_async(l.dispatchQueue, ^{ b(e, value); });
}

static BOOL nvmtl_sev_wait(NVMTLSharedEvent *e, uint64_t wanted, uint64_t ms) {
    uint64_t t0 = nvxp_now_ns();
    uint64_t deadline = ms == UINT64_MAX ? 0 : t0 + ms * 1000000ull;
    [e->_condition lock];
    for (;;) {
        nvxp *x = (nvxp *)e->_xp;
        uint64_t cur = x ? atomic_load(&x->sh->value) : e->_value;
        if (cur >= wanted) { [e->_condition unlock]; return YES; }
        uint64_t now = nvxp_now_ns();
        if (!ms || (deadline && now >= deadline)) {
            [e->_condition unlock];
            if (ms) {
                uint64_t n = atomic_fetch_add(&gxp_timeouts, 1) + 1;
                // a shared event that nobody signalled in time: the signature of a client whose partner never got its handle
                nvlog("xproc-event: wait timeout #%llu on %s event, wanted %llu have %llu after %llu ms (pid %d)",
                      (unsigned long long)n, x ? "an exported" : "a local", (unsigned long long)wanted, (unsigned long long)cur,
                      (unsigned long long)((now - t0) / 1000000ull), getpid());
            }
            return NO;
        }
        if (!x) {
            // a later export broadcasts the condition, so this wait moves to the shared word
            if (deadline) [e->_condition waitUntilDate:[NSDate dateWithTimeIntervalSinceNow:(double)(deadline - now) / 1e9]];
            else [e->_condition wait];
            continue;
        }
        [e->_condition unlock];
        if (deadline) os_sync_wait_on_address_with_timeout(&x->sh->value, cur, sizeof(uint64_t), OS_SYNC_WAIT_ON_ADDRESS_SHARED,
            OS_CLOCK_MACH_ABSOLUTE_TIME, deadline - now);
        else os_sync_wait_on_address(&x->sh->value, cur, sizeof(uint64_t), OS_SYNC_WAIT_ON_ADDRESS_SHARED);
        [e->_condition lock];
    }
}

// ---- watcher: fires notifications and re-pumps command buffers for signals made by another process ---------------

static void *nvxp_watch_main(void *arg) {
    NVMTLSharedEvent *e = (__bridge_transfer NVMTLSharedEvent *)arg;
    pthread_setname_np("nvmtl-xproc-watch");
    for (;;) {
        @autoreleasepool {
            NSMutableArray *ready = [NSMutableArray new];
            BOOL pump = NO; uint64_t seen; nvxp *x;
            [e->_condition lock];
            x = (nvxp *)e->_xp;
            seen = atomic_load(&x->sh->value);
            for (NSArray *n in e->_notifications) if ([n[1] unsignedLongLongValue] <= seen) [ready addObject:n];
            [e->_notifications removeObjectsInArray:ready];
            if (x->pumpWant && seen >= x->pumpWant) { x->pumpWant = 0; pump = YES; }
            if (!e->_notifications.count && !x->pumpWant) x->watcher = 0;
            BOOL done = !x->watcher;
            [e->_condition unlock];
            for (NSArray *n in ready) {
                MTLSharedEventListener *l = n[0]; MTLSharedEventNotificationBlock b = n[2];
                dispatch_async(l.dispatchQueue, ^{ b(e, seen); });
            }
            if (pump || ready.count) nvmtl_pump_buffers();
            if (done) return NULL;
            os_sync_wait_on_address_with_timeout(&x->sh->value, seen, sizeof(uint64_t), OS_SYNC_WAIT_ON_ADDRESS_SHARED,
                OS_CLOCK_MACH_ABSOLUTE_TIME, 5ull * NSEC_PER_SEC);
        }
    }
}

// The event's condition is held; _xp is set.
static void nvxp_watch_locked(NVMTLSharedEvent *e) {
    nvxp *x = (nvxp *)e->_xp;
    if (!x || x->watcher) return;
    pthread_t t; pthread_attr_t a; pthread_attr_init(&a); pthread_attr_setdetachstate(&a, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&a, 256 * 1024);
    x->watcher = 1;
    if (pthread_create(&t, &a, nvxp_watch_main, (__bridge_retained void *)e) != 0) {
        x->watcher = 0; CFBridgingRelease((__bridge CFTypeRef)e);
        nvlog("xproc-event: cannot start the watcher thread, remote signals will not wake notifications (pid %d)", getpid());
    }
}

// ---- handle and device entry points ----------------------------------------------------------------------------

static MTLSharedEventHandle *nvmtl_sev_handle(NVMTLSharedEvent *e) {
    [e->_condition lock];
    if (!e->_xp) {
        nvxp *x = nvxp_export(e->_value);
        if (x) { e->_xp = x; [e->_condition broadcast]; if (e->_notifications.count) nvxp_watch_locked(e); }
    }
    BOOL ok = e->_xp != NULL;
    [e->_condition unlock];
    if (!ok) { nvlog("newSharedEventHandle: cannot share this event across processes, returning nil (pid %d)", getpid()); return nil; }
    if (![MTLSharedEventHandle instancesRespondToSelector:@selector(initWithSharedEvent:)]) {
        nvlog("newSharedEventHandle: unsupported, this Metal has no MTLSharedEventHandle initWithSharedEvent: (pid %d)", getpid());
        return nil; }
    MTLSharedEventHandle *h = [[MTLSharedEventHandle alloc] initWithSharedEvent:e];
    uint64_t n = atomic_fetch_add(&gxp_handles, 1) + 1;
    if (!h) nvlog("newSharedEventHandle: cannot build the handle, the port send right was refused (pid %d)", getpid());
    else nvlog("xproc-event: handle #%llu created (imports %llu, import failures %llu, wait timeouts %llu, pid %d)",
               (unsigned long long)n, (unsigned long long)atomic_load(&gxp_imports), (unsigned long long)atomic_load(&gxp_import_fail),
               (unsigned long long)atomic_load(&gxp_timeouts), getpid());
    return h;
}

static id nvmtl_sev_import(id<MTLDevice> dev, MTLSharedEventHandle *h) {
    mach_port_t port = [h respondsToSelector:@selector(eventPort)] ? [h eventPort] : MACH_PORT_NULL;
    NVMTLSharedEvent *e = nil;
    if (port != MACH_PORT_NULL && mach_port_mod_refs(mach_task_self(), port, MACH_PORT_RIGHT_SEND, 1) == KERN_SUCCESS) {
        nvxp *x = nvxp_map(port);
        if (x) {
            x->port = port;
            e = [NVMTLSharedEvent new]; e->_dev = dev; e->_xp = x; e->_label = [h.label copy];
        } else mach_port_deallocate(mach_task_self(), port);
    }
    uint64_t n = e ? atomic_fetch_add(&gxp_imports, 1) + 1 : 0, f = e ? 0 : atomic_fetch_add(&gxp_import_fail, 1) + 1;
    if (e) nvlog("xproc-event: import #%llu rebuilt a shared event from a handle (pid %d)", (unsigned long long)n, getpid());
    else nvlog("newSharedEventWithHandle: FAIL #%llu, no event rebuilt, handle %p port %u -> nil (pid %d)", (unsigned long long)f, (__bridge void *)h, port, getpid());
    return e;
}

static void nvmtl_sev_teardown(NVMTLSharedEvent *e) {
    nvxp *x = (nvxp *)e->_xp; e->_xp = NULL;
    nvxp_free(x);
}

@implementation NVMTLSharedEvent (XProc)
- (mach_port_t)eventPort { nvxp *x = (nvxp *)_xp; return x ? x->port : MACH_PORT_NULL; }
- (uint64_t)labelTraceID { return 0; }
// A command buffer waits for `value` on this event: if another process may signal it, make sure someone re-pumps.
- (void)nvmtlXpWatchPump:(uint64_t)value {
    [_condition lock];
    nvxp *x = (nvxp *)_xp;
    if (x) { if (!x->pumpWant || value < x->pumpWant) x->pumpWant = value; nvxp_watch_locked(self); }
    [_condition unlock];
}
@end
