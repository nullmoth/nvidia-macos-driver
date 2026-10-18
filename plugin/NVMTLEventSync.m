/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

@interface NVMTLQueueReservation : NSObject { @public __weak NVMTLCommandBuffer *buffer; } @end
@implementation NVMTLQueueReservation @end
@interface NVMTLSubmissionPart : NSObject {
@public
    nvk_cmdbuf commands;
    NSSet *resources, *surfaces;
    NSArray *scratch;
    id waitEvent, signalEvent;
    uint64_t value;
    unsigned encoders;
    NSArray *signals;
    nvmtl_inflight *inflight;
    int aheadFailed;
    BOOL waitFolded;
    BOOL aheadEmpty;
    BOOL claimed;
    NSArray<NVMTLBuffer *> *copyInputs;
    BOOL copyInputsPrepared, hasHostCopies;
}
@end
static _Atomic unsigned gsub_inflight;
static _Atomic unsigned long long gsub_ahead, gsub_peak;
static NSMutableArray *nvmtl_quarantined_parts;
static int nvmtl_gsub_finish(NVMTLSubmissionPart *part) {
    nvmtl_inflight *const f = part->inflight;
    part->inflight = NULL;
    const int r = nvmtl_vk_submit_finish(f);
    atomic_fetch_sub(&gsub_inflight, 1);
    if (r == 0) nvmtl_vk_inflight_free(f);
    return r;
}
@interface NVMTLPendingSurfaceWrite : NSObject {
@public __weak NVMTLCommandBuffer *buffer; __weak NVMTLTexture *texture; uint32_t seed;
}
@end
@implementation NVMTLPendingSurfaceWrite @end
// Queue keys and command-buffer references are weak. An abandoned recording is
// never registered; completion removes committed entries without retaining VRAM.
static NSMapTable *nvmtl_surface_writers;
static NSString *nvmtl_surface_writer_key(NVMTLTexture *t) {
    return [NSString stringWithFormat:@"%u:%lu", (unsigned)IOSurfaceGetID(t->_surf), (unsigned long)t->_plane];
}
static BOOL nvmtl_surface_pending_write(NVMTLCommandBuffer *cb, NVMTLTexture *t) {
    if (!cb || !t || !t->_surf) return NO;
    @synchronized ([NVMTLCommandBuffer class]) {
        NSString *key = nvmtl_surface_writer_key(t);
        for (id queue in nvmtl_surface_writers.keyEnumerator) {
            NSDictionary *writes = [nvmtl_surface_writers objectForKey:queue];
            NVMTLPendingSurfaceWrite *write = writes[key];
            NVMTLCommandBuffer *writer = write ? write->buffer : nil;
            NVMTLTexture *backing = write ? write->texture : nil;
            const BOOL sameBacking = backing && (backing == t || (backing->_vramOn && t->_vramOn));
            if (writer && writer != cb && sameBacking && write->seed == IOSurfaceGetSeed(t->_surf)) return YES;
        }
        return NO;
    }
}
static void nvmtl_surface_register_writes(NVMTLCommandBuffer *cb) {
    // The same lock as pending_write and finish_writes, taken here too: the map is shared with the completion thread, and
    // its one caller holding this (recursive) lock today must not be what keeps WindowServer's heap intact.
    @synchronized ([NVMTLCommandBuffer class]) {
        if (!nvmtl_surface_writers) nvmtl_surface_writers = [NSMapTable weakToStrongObjectsMapTable];
        NSMutableDictionary *writes = [nvmtl_surface_writers objectForKey:cb->_queue];
        for (NVMTLSubmissionPart *part in cb->_parts) for (NVMTLTexture *t in part->surfaces) {
            if (!t->_surf) continue;
            if (!writes) { writes = [NSMutableDictionary new]; [nvmtl_surface_writers setObject:writes forKey:cb->_queue]; }
            NVMTLPendingSurfaceWrite *write = [NVMTLPendingSurfaceWrite new];
            write->buffer = cb; write->texture = t; write->seed = IOSurfaceGetSeed(t->_surf);
            writes[nvmtl_surface_writer_key(t)] = write;
        }
    }
}
// WindowServer frame completion, with NVAccel's nvSurfGpuEscape: each surface a WindowServer command buffer renders is
// marked pending when it commits and ready when the GPU has finished it, and the swap waits for that in the kernel, off
// WindowServer's thread (NVIDIA completes flips on the display's flip event, never in the compositor). The marks go
// through NVK's surface escape because WindowServer's sandbox denies sysctl writes. A mark the kernel does not take (an
// older NVAccel, debug.nvaccel_surface_gpu_on=0) keeps WindowServer's own wait; that answer is re-read every second.
static _Atomic int nvmtl_ws_marks_on = -1;
static _Atomic uint64_t nvmtl_ws_marks_checked;
static BOOL nvmtl_ws_kernel_waits(void) {
    static int ws = -1;
    if (ws < 0) ws = getprogname() && !strcmp(getprogname(), "WindowServer") && !getenv("NVMTL_WS_WAIT_COMPLETION");
    if (!ws) return NO;
    const uint64_t now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
    if (atomic_load(&nvmtl_ws_marks_on) < 0 || now - atomic_load(&nvmtl_ws_marks_checked) > 1000000000ull) {
        const int on = nvmtl_vk_surface_gpu_mark(0, 0) == 0;   // surface 0: a probe the kernel answers and ignores
        if (on != atomic_exchange(&nvmtl_ws_marks_on, on))
            nvlog("WindowServer frame completion: %s", on ? "the swap waits in NVAccel (no composite wait here)" : "waiting for each composite here");
        atomic_store(&nvmtl_ws_marks_checked, now);
    }
    return atomic_load(&nvmtl_ws_marks_on) == 1;
}
static const void *const nvmtl_ws_marks_key = &nvmtl_ws_marks_key;
static void nvmtl_ws_mark(uint32_t sid, BOOL pending) { (void)nvmtl_vk_surface_gpu_mark(sid, pending); }
// Marking WindowServer's surfaces newest in VRAM at commit is only safe when NVAccel's page-off waits for a pending
// surface (NVAccelSurfaceWait). Without that wait a CPU lock or a copied present pages the surface off while the GPU
// is still drawing it: blank window panes, a torn band, a Dock without its icons. Such an accelerator (a package that
// pairs this plugin with an older one) keeps the ordering it was built for: the surface is marked newest after the GPU
// has finished, and the pending marks are released when the whole command buffer has. Read once per process.
static BOOL nvmtl_ws_surface_wait(void) {
    static int on = -1;
    if (on < 0) {
        on = nvmtl_vk_accel_surface_wait() ? 1 : 0;
        nvlog("WindowServer surfaces: %s", on ? "marked newest in VRAM at commit (NVAccel's page-off waits for the GPU)"
                                              : "marked newest after the GPU finishes (NVAccel's page-off does not wait)");
    }
    return on == 1;
}
// What one WindowServer command buffer holds: the surfaces it marked pending in the kernel, and the textures whose shared
// VRAM its commit marked newest. early is fixed at commit (nvmtl_ws_surface_wait): the marks are released part by part.
@interface NVMTLSurfaceHolds : NSObject { @public NSMutableSet *ids; NSSet *dirtied; BOOL early; } @end
@implementation NVMTLSurfaceHolds @end
// A texture whose part has landed while its surface is still held, published by the holder that releases it last.
// dirtied: the commit marked its VRAM newest, so publication is bookkeeping only (nvmtlSurfaceOutDirtied:).
@interface NVMTLDeferredPublication : NSObject { @public NVMTLTexture *texture; BOOL dirtied; __weak NVMTLCommandBuffer *owner; } @end
@implementation NVMTLDeferredPublication @end
// The kernel keeps one pending bit per surface, with no owner. WindowServer commits overlapping command buffers that
// write the same surface, so the holders are counted here and the kernel mark is released only when the last of them has
// finished with it; an older command buffer finishing first would otherwise release a surface the newer one still writes.
// With the early ordering nothing is published while its surface is still held. A dirtied texture's bookkeeping would
// call the surface current with another write in flight; a full publication (a private copy, or a texture whose commit
// mark was refused) locks the surface, and that lock's page-off would wait for a mark only this completion thread can
// release. Those textures wait in nvmtl_ws_deferred for the holder that releases the surface last.
// The lock is its own, never the command-buffer class lock, and nothing that can wait for the accelerator runs under it:
// a page-off waiting for a mark holds the accelerator lock, and the release it waits for must always get through.
static NSCountedSet *nvmtl_ws_pending_holds;
static NSMutableDictionary<NSNumber *, NSMutableArray<NVMTLDeferredPublication *> *> *nvmtl_ws_deferred;
static NSObject *nvmtl_ws_lock(void) {
    static NSObject *lock; static dispatch_once_t once;
    dispatch_once(&once, ^{ lock = [NSObject new]; });
    return lock;
}
static void nvmtl_ws_defer(NVMTLCommandBuffer *owner, NVMTLTexture *t, BOOL dirtied, uint32_t sid) {
    @synchronized (nvmtl_ws_lock()) {
        if (!nvmtl_ws_deferred) nvmtl_ws_deferred = [NSMutableDictionary new];
        NSMutableArray *waiting = nvmtl_ws_deferred[@(sid)];
        if (!waiting) nvmtl_ws_deferred[@(sid)] = waiting = [NSMutableArray new];
        // a texture written again keeps only its latest place in the order: published at its earlier place, a private
        // copy written between the two would be copied out after it and leave the older pixels current
        for (NSUInteger i = 0; i < waiting.count; i++) if (((NVMTLDeferredPublication *)waiting[i])->texture == t) { [waiting removeObjectAtIndex:i]; break; }
        NVMTLDeferredPublication *d = [NVMTLDeferredPublication new];
        d->texture = t; d->dirtied = dirtied; d->owner = owner;
        [waiting addObject:d];
    }
}
// One holder lets go of a surface. The last one releases the kernel mark and moves the textures waiting for it to settled.
static void nvmtl_ws_release(NVMTLSurfaceHolds *holds, uint32_t sid, NSMutableArray *settled) {
    NSNumber *n = @(sid);
    @synchronized (nvmtl_ws_lock()) {
        if (![holds->ids containsObject:n]) return;
        [holds->ids removeObject:n];
        [nvmtl_ws_pending_holds removeObject:n];
        if ([nvmtl_ws_pending_holds countForObject:n]) return;
        nvmtl_ws_mark(sid, NO);
        NSArray *waiting = nvmtl_ws_deferred[n];
        if (waiting) { [settled addObjectsFromArray:waiting]; [nvmtl_ws_deferred removeObjectForKey:n]; }
    }
}
// Publishes what was waiting, after the marks are released. Returns NO when one of cb's own textures could not be
// published (its command buffer fails, as before); another command buffer's has already completed, so that is logged.
static BOOL nvmtl_ws_publish_settled(NSArray *settled, NVMTLCommandBuffer *cb) {
    BOOL ok = YES;
    for (NVMTLDeferredPublication *d in settled) {
        if (d->dirtied ? [d->texture nvmtlSurfaceOutDirtied:YES] : [d->texture nvmtlSurfaceOut]) continue;
        if (cb && d->owner == cb) ok = NO;
        else nvlog("WindowServer surface %u: publication after its last write refused", (unsigned)IOSurfaceGetID(d->texture->_surf));
    }
    return ok;
}
// Commit, OUTSIDE the command-buffer class lock and before the pending-write records are stamped: the surfaces this
// command buffer writes are marked pending in the kernel before commit returns, so WindowServer's swap that follows finds
// the marks; with the early ordering their shared VRAM is first marked newest.
// A normal IOSurfaceLock waits for GPU work still in flight on the surface (only kIOSurfaceLockAvoidSync skips that),
// and WindowServer relies on it: a window capture locks the surface it is still drawing (SkyLight's
// do_finish_and_clear_capture_state). The VRAM copy was marked newest only after the GPU finished, so a lock that came
// first found the system copy current and read zeros (black screenshots, blank Dock thumbnails, black lock screen).
// Marked here, the lock pages the surface off, and the page-off waits for the pending mark to clear.
// The VRAM mark takes the accelerator lock, which a page-off waiting for a pending surface holds until the completion
// thread releases that surface. So it runs outside the class lock (the completion thread takes that one between command
// buffers), and before this command buffer's own pending marks: with those in first, a page-off of a surface this
// command buffer writes would wait for it, and it cannot complete while its commit waits here. Either way only the
// kernel's 100 ms bound would end the wait, and the page-off would then copy unfinished pixels.
// The VRAM mark moves the IOSurface seed, so the pending-write records are stamped after it
// (nvmtl_surface_register_writes): a record stamped before the move matched nothing, every frame's read of the surface
// still in flight took the CPU path (page-off, wait, 8 MB upload) on the compositor's thread, and a 144 Hz desktop ran
// at 90 frames a second.
static void nvmtl_ws_mark_pending(NVMTLCommandBuffer *cb) {
    if (!nvmtl_ws_kernel_waits()) return;
    NSMutableSet *ids = nil;
    for (NVMTLSubmissionPart *part in cb->_parts) {
        if (![part isKindOfClass:[NVMTLSubmissionPart class]]) continue;
        for (NVMTLTexture *t in part->surfaces) if (t->_surf) {
            if (!ids) ids = [NSMutableSet new];
            [ids addObject:@(IOSurfaceGetID(t->_surf))];
        }
    }
    if (!ids) return;
    NVMTLSurfaceHolds *holds = [NVMTLSurfaceHolds new];
    holds->ids = ids; holds->early = nvmtl_ws_surface_wait();
    if (holds->early) {
        NSMutableSet *dirtied = [NSMutableSet new];
        for (NVMTLSubmissionPart *part in cb->_parts) {
            if (![part isKindOfClass:[NVMTLSubmissionPart class]]) continue;
            for (NVMTLTexture *t in part->surfaces)
                if (t->_surf && t->_vramOn && !nvmtl_vk_surface_dirty((uint32_t)IOSurfaceGetID(t->_surf), (uint32_t)t->_plane)) [dirtied addObject:t];
        }
        holds->dirtied = dirtied;
    }
    @synchronized (nvmtl_ws_lock()) {
        if (!nvmtl_ws_pending_holds) nvmtl_ws_pending_holds = [NSCountedSet new];
        for (NSNumber *n in ids) { [nvmtl_ws_pending_holds addObject:n]; nvmtl_ws_mark(n.unsignedIntValue, YES); }
    }
    objc_setAssociatedObject(cb, nvmtl_ws_marks_key, holds, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}
// The GPU has finished part index of cb. Without the early ordering, returns every texture of the part for nvmtlSurfaceOut
// and keeps the marks until the end of the command buffer (nvmtl_ws_mark_ready), as before. With it, returns nothing:
// this command buffer's mark on a surface is released after the last of its parts that writes the surface, and settled
// receives what the releases freed, to be published after them (a CPU lock waiting in the page-off for a mark holds the
// surface's lock, which publication takes). Released earlier, the swap or a capture would read the surface with a later
// part's write still in flight.
static NSArray *nvmtl_ws_part_landed(NVMTLCommandBuffer *cb, NSUInteger index, NSMutableArray *settled) {
    NVMTLSubmissionPart *part = cb->_parts[index];
    NVMTLSurfaceHolds *holds = objc_getAssociatedObject(cb, nvmtl_ws_marks_key);
    NSMutableArray *publish = [NSMutableArray new];
    if (!holds || !holds->early) { for (NVMTLTexture *t in part->surfaces) [publish addObject:t]; return publish; }
    NSMutableArray *last = [NSMutableArray new];
    for (NVMTLTexture *t in part->surfaces) {
        if (!t->_surf) { [publish addObject:t]; continue; }
        const uint32_t sid = (uint32_t)IOSurfaceGetID(t->_surf);
        // deferred before the release below, so the release that turns out to be the last one publishes it
        nvmtl_ws_defer(cb, t, [holds->dirtied containsObject:t], sid);
        BOOL later = NO;
        for (NSUInteger i = index + 1; i < cb->_parts.count && !later; i++) {
            NVMTLSubmissionPart *next = cb->_parts[i];
            if (![next isKindOfClass:[NVMTLSubmissionPart class]]) continue;
            for (NVMTLTexture *other in next->surfaces) if (other->_surf && (uint32_t)IOSurfaceGetID(other->_surf) == sid) { later = YES; break; }
        }
        if (!later) [last addObject:@(sid)];
    }
    for (NSNumber *n in last) nvmtl_ws_release(holds, n.unsignedIntValue, settled);
    return publish;
}
// The command buffer has finished (or failed): every hold it still has is released, and what was waiting is published.
static void nvmtl_ws_mark_ready(NVMTLCommandBuffer *cb) {
    NVMTLSurfaceHolds *holds = objc_getAssociatedObject(cb, nvmtl_ws_marks_key);
    if (!holds) return;
    NSArray *ids; @synchronized (nvmtl_ws_lock()) { ids = holds->ids.allObjects; }
    NSMutableArray *settled = [NSMutableArray new];
    for (NSNumber *n in ids) nvmtl_ws_release(holds, n.unsignedIntValue, settled);
    objc_setAssociatedObject(cb, nvmtl_ws_marks_key, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    (void)nvmtl_ws_publish_settled(settled, nil);
}
static void nvmtl_surface_finish_writes(NVMTLCommandBuffer *cb) {
    nvmtl_ws_mark_ready(cb);
    @synchronized ([NVMTLCommandBuffer class]) {
        NSMutableDictionary *writes = [nvmtl_surface_writers objectForKey:cb->_queue];
        for (NSString *key in [writes.allKeys copy]) {
            NVMTLPendingSurfaceWrite *write = writes[key];
            if (!write->buffer || write->buffer == cb) [writes removeObjectForKey:key];
        }
        if (!writes.count) [nvmtl_surface_writers removeObjectForKey:cb->_queue];
    }
}

@implementation NVMTLSubmissionPart
- (void)dealloc {
    if (inflight) {
        nvlog("commit: G-SUB BUG - a part submitted ahead reached dealloc unfinished; finishing it now");
        if (nvmtl_gsub_finish(self) != 0) {
            (void)CFBridgingRetain(resources); (void)CFBridgingRetain(surfaces); (void)CFBridgingRetain(scratch);
            memset(&commands, 0, sizeof commands);
            return;
        }
    }
    if (commands.cb) nvmtl_vk_cmd_abandon(&commands);
}
@end

static NSMutableArray<NVMTLCommandBuffer *> *nvmtl_pending_buffers;
static BOOL nvmtl_pumping;
static void nvmtl_pump_buffers(void);
static BOOL nvmtl_async_commit_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_ASYNC_COMMIT"); on = !(e && e[0] == '0');
        nvlog("commit: %s (NVMTL_ASYNC_COMMIT=%s)", on ? "ASYNC (default) - execution worker, submit ahead, queue capacity enforced" : "synchronous (NVMTL_ASYNC_COMMIT=0)", e ? e : "unset"); }
    return on;
}
static _Atomic unsigned long long gSkipEmpty;
static int nvmtl_skip_empty_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_SKIP_EMPTY"); on = !(e && e[0] == '0');
        nvlog("commit: empty submission parts %s (NVMTL_SKIP_EMPTY=%s)", on ? "are NOT submitted" : "are submitted as before b78", e ? e : "unset"); }
    return on;
}

@implementation NVMTLCommandBuffer (EventSubmission)
- (void)nvmtlReserveQueue {
    if (_reservation) return;
    NVMTLQueueReservation *r = [NVMTLQueueReservation new]; r->buffer = self;
    _reservation = r;
    if (!_queue->_reservations) _queue->_reservations = [NSMutableArray new];
    [_queue->_reservations addObject:r];
}
- (void)nvmtlReleaseReservation {
    if (_reservation) [_queue->_reservations removeObjectIdenticalTo:_reservation];
    _reservation = nil;
}
- (void)nvmtlSealPartWithSignal:(id)event value:(uint64_t)value restart:(BOOL)restart {
    if (!_parts) _parts = [NSMutableArray new];
    nvmtl_g79_flush(self);
    NVMTLSubmissionPart *part = [NVMTLSubmissionPart new];
    part->commands = _c;
    memset(&_c, 0, sizeof(_c));
    part->resources = [(NVMTLResSet *)_resources nvmtlTakeAll]; part->surfaces = [_surfDirty copy];
    // Another process may read an IOSurface once its writer is scheduled, and nothing orders its queue after ours.
    static int after = -1; if (after < 0) { const char *e = getenv("NVMTL_SCHEDULE_AFTER_SURFACE_WRITES"); after = e && e[0] == '1'; }
    NVMTLTexture *drawable = _present ? (_present->_parent ?: _present) : nil;
    for (NVMTLTexture *s in part->surfaces) if (after && s != drawable) _scheduleAfterPublish = YES;
    part->scratch = [_scratch copy]; part->signalEvent = event; part->value = value;
    part->encoders = _nEnc - _nEncSealed; _nEncSealed = _nEnc;
    if (_g79Sig.count) { part->signals = [_g79Sig copy]; [_g79Sig removeAllObjects]; }
    [_parts addObject:part];
    [_surfDirty removeAllObjects]; [_scratch removeAllObjects];
    // The repeated-setBytes cache points at ring chunks without owning them; the part now owns those chunks and frees
    // them when it completes, which can happen while this command buffer is still being encoded. A cache entry that
    // outlived its chunk was bound and retained again by the next identical setBytes, and the compute encoder's binding
    // table later released a freed object (mediaanalysisd under MPS convolution, endEncoding / .cxx_destruct).
    memset(_g79BRing, 0, sizeof _g79BRing); memset(_g79BLen, 0, sizeof _g79BLen);
    // Recording goes on after this seal, and the encoders' binding tables still name these resources by raw Vulkan
    // handle: a later dispatch snapshots them again without rebinding. The command buffer keeps holding them until it
    // completes, as Metal's do. WAS: only the part held them, so a texture the app had released died when that part
    // completed and the next dispatch wrote its destroyed image view (NVK abort in get_sampled_image_view_desc under
    // nvk_UpdateDescriptorSets: mediaanalysisd, Safari, wallpapers).
    if (restart) for (id o in (NVMTLResSet *)part->resources) [(NVMTLResSet *)_resources addObject:o];
    if (restart && nvmtl_vk_cmd_begin(&_queue->_q, &_c)) {
        _cbError = [NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorOutOfMemory
            userInfo:@{NSLocalizedDescriptionKey:@"Could not allocate recording after event boundary"}];
    }
}
- (void)nvmtlRecordWait:(id)event value:(uint64_t)value {
    if (!event || _committed) return;
    if (nvmtl_g79_on(NVMTL_G79_EV)) {
        BOOL self_ = NO;
        for (NSArray *p in _g79Promised) if (p[0] == event && [p[1] unsignedLongLongValue] >= value) { self_ = YES; break; }
        BOOL held = !self_ && [event respondsToSelector:@selector(signaledValue)]
            && ((uint64_t(*)(id,SEL))objc_msgSend)(event, @selector(signaledValue)) >= value;
        if (self_ || held) {
            if (_g79C2C && nvmtl_g79_on(NVMTL_G79_WFOLD)) { _g79Owed = YES; nvmtl_g79_count(G79C_WAITFOLD); }
            else if (_c.cb) { nvmtl_vk_cmd_barrier(&_c); _g79Owed = NO; _g79C2C = NO; }
            nvmtl_g79_count(self_ ? G79C_WAITSELF : G79C_WAITHELD);
            return;
        }
        nvmtl_g79_count(G79C_WAITSPLIT);
    }
    [self nvmtlSealPartWithSignal:nil value:0 restart:YES];
    NVMTLSubmissionPart *part = [NVMTLSubmissionPart new];
    part->waitEvent = event; part->value = value; [_parts addObject:part];
}
- (void)nvmtlRecordSignal:(id)event value:(uint64_t)value {
    if (!event || _committed) return;
    if (nvmtl_g79_on(NVMTL_G79_EV)) {
        if (!_g79Sig) _g79Sig = [NSMutableArray new];
        if (!_g79Promised) _g79Promised = [NSMutableArray new];
        [_g79Sig addObject:@[event, @(value)]]; [_g79Promised addObject:@[event, @(value)]];
        nvmtl_g79_count(G79C_SIGRIDE);
        return;
    }
    [self nvmtlSealPartWithSignal:event value:value restart:YES];
}
- (void)nvmtlDispatchHandlers:(NSArray *)handlers scheduled:(BOOL)scheduled {
    if (!handlers.count) return;
    dispatch_queue_t order;
    dispatch_queue_t target;
    // the class, not the queue: apps may hold @synchronized(queue) while they wait for this command buffer
    @synchronized ([NVMTLCommandQueue class]) {
        if (!_queue->_callbackOrderQueue)
            _queue->_callbackOrderQueue = dispatch_queue_create("com.nvmtl.command-callbacks", DISPATCH_QUEUE_SERIAL);
        order = _queue->_callbackOrderQueue;
        target = _queue->_completionQueue ?: dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0);
    }
    for (NSUInteger i = 0; i < handlers.count; ++i) {
        dispatch_group_enter(_callbackGroup);
        if (scheduled) dispatch_group_enter(_scheduledGroup);
    }
    dispatch_async(order, ^{
        dispatch_sync(target, ^{
            for (MTLCommandBufferHandler handler in handlers) {
                handler(self);
                if (scheduled) dispatch_group_leave(self->_scheduledGroup);
                dispatch_group_leave(self->_callbackGroup);
            }
        });
    });
}
- (void)nvmtlMarkScheduled {
    if (_scheduleAfterPublish) return;
    [_completionCondition lock];
    if (!_wasScheduled) {
        _wasScheduled = YES; _cbstatus = MTLCommandBufferStatusScheduled;
        NSArray *handlers = [_scheduledHandlers copy]; [_scheduledHandlers removeAllObjects];
        [self nvmtlDispatchHandlers:handlers scheduled:YES];
        [_completionCondition broadcast];
    }
    [_completionCondition unlock];
}
- (void)nvmtlCollectDeviceTiming:(nvk_cmdbuf *)commands {
    if (!commands->timing.valid) { _gpuTimingInvalid = YES; return; }
    if (!_gpuTimingSeen) { _gpuTiming = commands->timing; _gpuTimingSeen = YES; }
    else {
        _gpuTiming.end_tick = commands->timing.end_tick;
        _gpuTiming.host_after_ns = commands->timing.host_after_ns;
    }
}
- (void)nvmtlFinishExecution:(NSError *)error {
    if (_scheduleAfterPublish) { _scheduleAfterPublish = NO; [self nvmtlMarkScheduled]; }
    for (NSUInteger i = _nextPart; i < _parts.count; i++) {
        NVMTLSubmissionPart *p = _parts[i];
        if (![p isKindOfClass:[NVMTLSubmissionPart class]] || !p->inflight) continue;
        int result = nvmtl_gsub_finish(p);
        if (!result) [self nvmtlCollectDeviceTiming:&p->commands];
        else _gpuTimingInvalid = YES;
        if (result == -2) {
            if (!nvmtl_quarantined_parts) nvmtl_quarantined_parts = [NSMutableArray new];
            [nvmtl_quarantined_parts addObject:p];
        }
    }
    if (!error) {
        for (dispatch_block_t action in _presentations) action();
    }
    [_presentations removeAllObjects]; _present = nil;
    double deviceStart = 0, deviceEnd = 0;
    if (!error && _gpuTimingSeen && !_gpuTimingInvalid)
        (void)nvmtl_vk_interval_seconds(&_gpuTiming, &deviceStart, &deviceEnd);
    nvmtl_surface_finish_writes(self);
    [_completionCondition lock];
    _gpuStart = deviceStart; _gpuEnd = deviceEnd;
    _legacyKernelEnd = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e9;
    _cbError = error; _cbstatus = error ? MTLCommandBufferStatusError : MTLCommandBufferStatusCompleted;
    _executionDone = YES;
    [_completionCondition unlock];
    [_parts removeAllObjects];
    // A finished command buffer lets go of everything only its GPU work needed: the setBytes ring and the scratch
    // buffers it filled, and any resources still listed on it. Apple's command buffers do the same. An app's
    // autorelease pool can keep thousands of finished command buffers alive during a long loop (Geekbench 7 does),
    // and holding these until the command buffer itself died filled VRAM until allocations were refused.
    [_scratch removeAllObjects]; _e78Ring = nil; _e78RingOff = 0; memset(_g79BRing, 0, sizeof _g79BRing);
    [(NVMTLResSet *)_resources removeAllObjects]; [_stTwins removeAllObjects];
    // Applications may retain completed command buffers. Pending overwrite
    // bookkeeping must not keep their IOSurfaces and VRAM alive indefinitely.
    [_surfOverwrites removeAllObjects]; _surfOverwrites = nil;
    if (!error) nvmtl_res2_cb_done(self);
    [self nvmtlReleaseCapacity];
}
- (void)nvmtlFinishCallbacks {
    [_completionCondition lock];
    NSArray *handlers = [_handlers copy]; [_handlers removeAllObjects];
    [self nvmtlDispatchHandlers:handlers scheduled:NO];
    _completionReady = YES;
    [_completionCondition broadcast]; [_completionCondition unlock];
}
static unsigned nvmtl_gsub_cap(void) {
    static int cap = -1;
    if (cap < 0) { const char *e = getenv("NVMTL_GSUB_INFLIGHT"); int v = e ? atoi(e) : 16;
        cap = v < 1 ? 1 : v > 32 ? 32 : v;
        nvlog("commit: G-SUB submit-ahead %s, at most %d parts in flight (NVMTL_GSUB_INFLIGHT=%s)",
              nvmtl_async_commit_on() && nvmtl_g79_on(NVMTL_G79_SUB) ? "ON" : "off", cap, e ? e : "unset"); }
    return (unsigned)cap;
}
static BOOL nvmtl_gsub_ahead_on(void) { return nvmtl_async_commit_on() && nvmtl_g79_on(NVMTL_G79_SUB) && nvmtl_gsub_cap(); }
static NSMapTable *nvmtl_evfold_submitted;
static _Atomic unsigned long long gEvFold, gEvFoldEmpty;
static BOOL nvmtl_evfold_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_EVFOLD"); on = !(e && e[0] == '0');
        nvlog("commit: sub1 EVFOLD %s - an event wait whose signal is already in the channel %s (NVMTL_EVFOLD=%s)",
              on ? "ON" : "off", on ? "is ordered by the channel, no CPU hold" : "is held until the CPU reads the signal", e ? e : "unset"); }
    return on;
}
static uint64_t nvmtl_evfold_promised(id event) {
    return event ? [[nvmtl_evfold_submitted objectForKey:event] unsignedLongLongValue] : 0;
}
static void nvmtl_evfold_promise(id event, uint64_t value) {
    if (!event || !nvmtl_evfold_on()) return;
    if (!nvmtl_evfold_submitted) nvmtl_evfold_submitted = [NSMapTable weakToStrongObjectsMapTable];
    if (value > nvmtl_evfold_promised(event)) [nvmtl_evfold_submitted setObject:@(value) forKey:event];
}
static void nvmtl_evfold_promise_part(NVMTLSubmissionPart *part) {
    if (part->signalEvent) nvmtl_evfold_promise(part->signalEvent, part->value);
    for (NSArray *s in part->signals) nvmtl_evfold_promise(s[0], [s[1] unsignedLongLongValue]);
}
static void nvmtl_evfold_census(_Atomic unsigned long long *c, const char *what) {
    const unsigned long long k = atomic_fetch_add(c, 1) + 1;
    if (!(k & (k - 1))) nvlog("commit: sub1 EVFOLD %llu %s", k, what);
}
static void nvmtl_gsub_census(void) {
    const unsigned long long k = atomic_load(&gsub_ahead);
    if (k & (k - 1)) return;
    uint64_t s[9]; nvmtl_vk_gsub_nvk_stats(s, 9);
    nvlog("commit: G-SUB %llu parts submitted ahead, peak %llu in flight | NVK %s submits %llu acquires %llu cpuwaits %llu slotwaits %llu fifowaits %llu deferred %llu reaped %llu waits %llu",
          k, (unsigned long long)atomic_load(&gsub_peak), s[0] == ~0ull ? "G-SUB ABSENT" : s[0] ? "async" : "SYNC (NVRM_ASYNC=0)",
          (unsigned long long)s[1], (unsigned long long)s[2], (unsigned long long)s[3], (unsigned long long)s[4],
          (unsigned long long)s[5], (unsigned long long)s[6], (unsigned long long)s[7], (unsigned long long)s[8]);
}
// A texture over a copy-backed buffer must refresh the same bytes as a direct
// buffer binding. Keep direct managed-buffer writeback and copy each backing once.
static NSArray<NVMTLBuffer *> *nvmtl_submission_copies(NSSet *resources, BOOL writtenOnly) {
    NSMutableSet<NVMTLBuffer *> *copies = nil;
    for (id resource in resources) {
        NVMTLBuffer *buffer = nil;
        if ([resource isKindOfClass:[NVMTLBuffer class]]) buffer = resource;
        else if ([resource isKindOfClass:[NVMTLTexture class]]) {
            NVMTLTexture *texture = resource;
            if (writtenOnly && texture->_usage != MTLTextureUsageUnknown &&
                !(texture->_usage & (MTLTextureUsageShaderWrite | MTLTextureUsageRenderTarget))) continue;
            buffer = texture->_backBuf ?: (texture->_parent ? texture->_parent->_backBuf : nil);
        }
        if (!buffer || (!buffer->_hostPtr && !buffer->_shadow.map)) continue;
        if (!copies) copies = [NSMutableSet new];
        [copies addObject:buffer];
    }
    return copies.allObjects;
}
static void nvmtl_prepare_submission_copies(NVMTLSubmissionPart *part) {
    if (part->copyInputsPrepared) return;
    // A sealed part's membership and its buffers' backing modes do not change.
    // The ahead pass can retry a held part many times: discover its upload
    // backings once, but refresh their bytes only when it actually submits.
    part->copyInputs = nvmtl_submission_copies(part->resources, NO);
    for (NVMTLBuffer *buffer in part->copyInputs) {
        if (buffer->_hostPtr) { part->hasHostCopies = YES; break; }
    }
    part->copyInputsPrepared = YES;
}
- (BOOL)nvmtlSubmitAhead {
    // a commit still putting its kernel marks in (nvmtlSubmitWithEvents) must not reach the GPU before them
    if (_executionDone || _cbError || !_committed || _commitMarking) return NO;
    if (_nextSubmit < _nextPart) _nextSubmit = _nextPart;
    while (_nextSubmit < _parts.count) {
        if (atomic_load(&gsub_inflight) >= nvmtl_gsub_cap()) return NO;
        NVMTLSubmissionPart *part = _parts[_nextSubmit];
        if (![part isKindOfClass:[NVMTLSubmissionPart class]]) return NO;
        if (part->claimed) return NO;
        if (part->waitEvent) {
            if (!nvmtl_evfold_on() || nvmtl_evfold_promised(part->waitEvent) < part->value) return NO;
            part->waitFolded = YES; _nextSubmit++; nvmtl_evfold_census(&gEvFold, "event waits ordered by the channel (no CPU hold)");
            continue;
        }
        if (!part->commands.cb) return NO;
        if (!part->encoders && !part->resources.count && !part->surfaces.count && !part->scratch.count && nvmtl_skip_empty_on()) {
            if (!nvmtl_evfold_on() || _nextSubmit + 1 >= _parts.count) return NO;
            [self nvmtlMarkScheduled];
        if (!_legacyKernelStart) _legacyKernelStart = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e9;
            nvmtl_vk_cmd_abandon(&part->commands);
            part->aheadEmpty = YES; _nextSubmit++;
            nvmtl_evfold_promise_part(part);
            nvmtl_evfold_census(&gEvFoldEmpty, "empty split parts retired by the ahead pass");
            continue;
        }
        nvmtl_prepare_submission_copies(part);
        {
            static int hold = -1;
            if (hold < 0) { const char *e = getenv("NVMTL_GSUB_HOSTPTR_LIFT"); hold = (e && *e && *e != '0') ? 0 : 1; }
            if (hold && part->hasHostCopies) return NO;
        }
        nvmtl_inflight *f = nvmtl_vk_inflight_new();
        if (!f) return NO;
        for (NVMTLBuffer *buffer in part->copyInputs) nvmtl_buffer_sync_in(buffer);
        [self nvmtlMarkScheduled];
        if (!_legacyKernelStart) _legacyKernelStart = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e9;
        const int r = nvmtl_vk_submit_begin(&part->commands, f);
        _nextSubmit++;
        if (r) {
            nvmtl_vk_inflight_free(f);
            part->aheadFailed = r;
            return NO;
        }
        part->inflight = f;
        nvmtl_evfold_promise_part(part);
        const unsigned long long n = atomic_fetch_add(&gsub_inflight, 1) + 1;
        if (n > atomic_load(&gsub_peak)) atomic_store(&gsub_peak, n);
        atomic_fetch_add(&gsub_ahead, 1);
        nvmtl_gsub_census();
    }
    return YES;
}
static void nvmtl_gsub_ahead_pass1(void);
static void nvmtl_gsub_ahead_pass(void) {
    for (int round = 0; round < 64; round++) {
        const unsigned long long before = atomic_load(&gEvFold);
        nvmtl_gsub_ahead_pass1();
        if (atomic_load(&gEvFold) == before) break;
    }
}
static void nvmtl_gsub_ahead_pass1(void) {
    NSMutableSet *queues = [NSMutableSet new];
    for (NVMTLCommandBuffer *cb in nvmtl_pending_buffers) {
        id q = cb->_queue;
        if (!q || [queues containsObject:q]) continue;
        [queues addObject:q];
        for (NVMTLQueueReservation *r in [cb->_queue->_reservations copy]) {
            NVMTLCommandBuffer *b = r->buffer;
            if (!b) continue;
            if (![b nvmtlSubmitAhead]) break;
        }
    }
}
- (BOOL)nvmtlAdvancePart {
    if (_executionDone) return NO;
    if (_cbError) { [self nvmtlFinishExecution:_cbError]; return YES; }
    if (_nextPart >= _parts.count) { [self nvmtlFinishExecution:nil]; return YES; }
    NVMTLSubmissionPart *part = _parts[_nextPart];
    BOOL ahead;   // claimed under the class lock: the commit-time pass stops at a claimed part
    @synchronized ([NVMTLCommandBuffer class]) { ahead = part->inflight || part->aheadFailed; part->claimed = YES; }
    if (part->waitEvent && !ahead && !part->waitFolded) {
        if (![part->waitEvent respondsToSelector:@selector(signaledValue)]) {
            [self nvmtlFinishExecution:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:@"Cannot wait on an event from another backend"}]];
            return YES;
        }
        uint64_t have = ((uint64_t(*)(id,SEL))objc_msgSend)(part->waitEvent,@selector(signaledValue));
        if (have < part->value) { @synchronized ([NVMTLCommandBuffer class]) { part->claimed = NO; } return NO; }
    }
    if (!ahead && part->commands.cb && !part->encoders && !part->resources.count && !part->surfaces.count && !part->scratch.count && nvmtl_skip_empty_on()) {
        [self nvmtlMarkScheduled];
        if (!_legacyKernelStart) _legacyKernelStart = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e9;
        nvmtl_vk_cmd_abandon(&part->commands);
        const unsigned long long k = atomic_fetch_add(&gSkipEmpty, 1);
        if (!k || ((k + 1) & 0xFFF) == 0) nvlog("commit: %llu empty submission parts NOT submitted (no encoder, no resources) - each WAS a full submit + wait", k + 1);
    }
    if (part->commands.cb || ahead) {
        [self nvmtlMarkScheduled];
        if (!_legacyKernelStart) _legacyKernelStart = clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e9;
        int result;
        if (part->inflight) result = nvmtl_gsub_finish(part);
        else if (part->aheadFailed) result = part->aheadFailed;
        else {
            nvmtl_prepare_submission_copies(part);
            for (NVMTLBuffer *buffer in part->copyInputs) nvmtl_buffer_sync_in(buffer);
            result = nvmtl_vk_submit_wait(&part->commands);
        }
        if (result == -2) {
            if (!nvmtl_quarantined_parts) nvmtl_quarantined_parts = [NSMutableArray new];
            [nvmtl_quarantined_parts addObject:part];
        }
        if (result == 0) [self nvmtlCollectDeviceTiming:&part->commands];
        BOOL published = result == 0;
        if (published) {
            // the GPU has finished this part: its surfaces' kernel marks are released (nvmtl_ws_part_landed) before the
            // surface work below, which takes the surface's own lock
            NSMutableArray *settled = [NSMutableArray new];
            for (NVMTLTexture *texture in nvmtl_ws_part_landed(self, _nextPart, settled)) if (![texture nvmtlSurfaceOut]) published = NO;
            if (!nvmtl_ws_publish_settled(settled, self)) published = NO;
            for (NVMTLBuffer *buffer in nvmtl_submission_copies(part->resources, YES)) nvmtl_buffer_sync_out(buffer);
        }
        if (!published) {
            [self nvmtlFinishExecution:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:result ? @"GPU event segment submission failed" : @"Could not publish event segment writes"}]];
            return YES;
        }
    }
    if (part->signalEvent) {
        if (![part->signalEvent respondsToSelector:@selector(setSignaledValue:)]) {
            [self nvmtlFinishExecution:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:@"Cannot signal an event from another backend"}]];
            return YES;
        }
        ((void(*)(id,SEL,uint64_t))objc_msgSend)(part->signalEvent,@selector(setSignaledValue:),part->value);
    }
    for (NSArray *s in part->signals) {
        id event = s[0];
        if (![event respondsToSelector:@selector(setSignaledValue:)]) {
            [self nvmtlFinishExecution:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
                userInfo:@{NSLocalizedDescriptionKey:@"Cannot signal an event from another backend"}]];
            return YES;
        }
        ((void(*)(id,SEL,uint64_t))objc_msgSend)(event, @selector(setSignaledValue:), [s[1] unsignedLongLongValue]);
    }
    _parts[_nextPart++] = [NSNull null];
    if (_nextPart == _parts.count) { nvtrace("COMPLETE cb %p t=%llu", (__bridge void *)self, (unsigned long long)(clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1000));
        [self nvmtlFinishExecution:nil]; }
    return YES;
}
- (void)nvmtlSubmitWithEvents {
    @synchronized ([NVMTLCommandBuffer class]) {
        if (_committed) return;
        [self nvmtlReserveQueue];
        _committed = YES; _commitMarking = YES;
        if (!__atomic_load_n(&_rep, __ATOMIC_ACQUIRE)) __atomic_store_n(&_rep, nvmtl_ep_enter(), __ATOMIC_RELEASE);
        [_completionCondition lock]; _cbstatus = MTLCommandBufferStatusCommitted; [_completionCondition unlock];
        [self nvmtlSealPartWithSignal:nil value:0 restart:NO];
    }
    // the commit's kernel marks can wait for the accelerator lock, so they go in outside the class lock (see
    // nvmtl_ws_mark_pending). This command buffer is committed and holds its queue reservation, but nothing submits it
    // (_commitMarking stops the submit-ahead pass) or completes it (it is not pending yet) before its marks are in.
    nvmtl_ws_mark_pending(self);
    @synchronized ([NVMTLCommandBuffer class]) {
        _commitMarking = NO;
        nvmtl_surface_register_writes(self);
        if (!nvmtl_pending_buffers) nvmtl_pending_buffers = [NSMutableArray new];
        [nvmtl_pending_buffers addObject:self];
        nvmtl_pump_buffers();
    }
}
@end

static void nvmtl_pump_buffers_sync(void) {
    @synchronized ([NVMTLCommandBuffer class]) {
        if (nvmtl_pumping) return;
        nvmtl_pumping = YES;
        BOOL progress;
        do {
            progress = NO;
            for (NVMTLCommandBuffer *cb in [nvmtl_pending_buffers copy]) {
                NSMutableArray *reservations = cb->_queue->_reservations;
                while (reservations.count && !((NVMTLQueueReservation *)reservations.firstObject)->buffer)
                    [reservations removeObjectAtIndex:0];
                if (reservations.firstObject != cb->_reservation) continue;
                if ([cb nvmtlAdvancePart]) progress = YES;
                if (cb->_executionDone) {
                    [cb nvmtlReleaseReservation];
                    [nvmtl_pending_buffers removeObjectIdenticalTo:cb];
                    [cb nvmtlFinishCallbacks];
                }
            }
        } while (progress);
        nvmtl_pumping = NO;
    }
}

static uint64_t nvmtl_pump_generation;
static BOOL nvmtl_commit_ahead_on(void) {
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_COMMIT_AHEAD"); on = !(e && e[0] == '0');
        nvlog("commit: submit-ahead %s (NVMTL_COMMIT_AHEAD=%s)", on ? "also runs at commit while the worker waits" : "runs on the worker only", e ? e : "unset"); }
    return on;
}
static void nvmtl_pump_buffers(void) {
    static dispatch_once_t once; static dispatch_queue_t worker;
    const BOOL async = nvmtl_async_commit_on();
    if (async) dispatch_once(&once, ^{ worker = dispatch_queue_create("nvmtl.execution", DISPATCH_QUEUE_SERIAL); });
    if (!async) { nvmtl_vk_clock_refresh(); nvmtl_pump_buffers_sync(); return; }
    @synchronized ([NVMTLCommandBuffer class]) {
        if (!nvmtl_pending_buffers.count) return;
        ++nvmtl_pump_generation;
        if (nvmtl_pumping) {
            // the worker may be blocked on an earlier part; without this the commit is scheduled only after that wait
            if (nvmtl_commit_ahead_on() && nvmtl_gsub_ahead_on()) nvmtl_gsub_ahead_pass();
            return;
        }
        nvmtl_pumping = YES;
        dispatch_async(worker, ^{
            for (;;) { @autoreleasepool {
                nvmtl_vk_clock_refresh();
                NSArray *batch; uint64_t generation;
                @synchronized ([NVMTLCommandBuffer class]) {
                    if (nvmtl_gsub_ahead_on()) nvmtl_gsub_ahead_pass();
                    batch = [nvmtl_pending_buffers copy]; generation = nvmtl_pump_generation;
                }
                BOOL progress = NO;
                for (NVMTLCommandBuffer *cb in batch) {
                    BOOL eligible;
                    @synchronized ([NVMTLCommandBuffer class]) {
                        NSMutableArray *reservations = cb->_queue->_reservations;
                        while (reservations.count && !((NVMTLQueueReservation *)reservations.firstObject)->buffer)
                            [reservations removeObjectAtIndex:0];
                        eligible = reservations.firstObject == cb->_reservation;
                    }
                    if (!eligible) continue;
                    if ([cb nvmtlAdvancePart]) progress = YES;
                    @synchronized ([NVMTLCommandBuffer class]) {
                        if (cb->_executionDone) {
                            [cb nvmtlReleaseReservation];
                            [nvmtl_pending_buffers removeObjectIdenticalTo:cb];
                            [cb nvmtlFinishCallbacks];
                        }
                    }
                }
                @synchronized ([NVMTLCommandBuffer class]) {
                    if (!progress && generation == nvmtl_pump_generation) {
                        nvmtl_pumping = NO; return;
                    }
                }
            } }
        });
    }
}

static uint64_t nvmtl_event_read(NSCondition *condition, uint64_t *storage) {
    [condition lock]; uint64_t value = *storage; [condition unlock]; return value;
}
static void nvmtl_event_signal(id event, NSCondition *condition, uint64_t *storage,
                              NSMutableArray *notifications, uint64_t value) {
    NSMutableArray *ready = [NSMutableArray new];
    [condition lock];
    if (value > *storage) *storage = value;
    value = *storage;
    for (NSArray *notification in notifications)
        if ([notification[1] unsignedLongLongValue] <= value) [ready addObject:notification];
    [notifications removeObjectsInArray:ready];
    [condition broadcast]; [condition unlock];
    for (NSArray *notification in ready) {
        MTLSharedEventListener *listener = notification[0];
        MTLSharedEventNotificationBlock block = notification[2];
        dispatch_async(listener.dispatchQueue, ^{ block(event, value); });
    }
    nvmtl_pump_buffers();
}
static void nvmtl_event_notify(id event, NSCondition *condition, uint64_t *storage,
                              NSMutableArray *notifications, MTLSharedEventListener *listener,
                              uint64_t wanted, MTLSharedEventNotificationBlock block) {
    if (!listener || !block) return;
    [condition lock]; uint64_t value = *storage;
    if (value < wanted) [notifications addObject:@[listener, @(wanted), [block copy]]];
    [condition unlock];
    if (value >= wanted) dispatch_async(listener.dispatchQueue, ^{ block(event, value); });
}
static BOOL nvmtl_event_wait(NSCondition *condition, uint64_t *storage, uint64_t wanted, uint64_t milliseconds) {
    NSDate *deadline = milliseconds == UINT64_MAX ? nil : [NSDate dateWithTimeIntervalSinceNow:(double)milliseconds / 1000.0];
    [condition lock];
    while (*storage < wanted) {
        if (!milliseconds) break;
        if (deadline) { if (![condition waitUntilDate:deadline]) break; }
        else [condition wait];
    }
    BOOL done = *storage >= wanted; [condition unlock]; return done;
}
