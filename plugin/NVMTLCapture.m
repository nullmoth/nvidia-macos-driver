/*
 * NullMoth NVIDIA driver for macOS
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Both the texture cache and every recording that uses it own the imported pages. Replacing the cache cannot free an
// import still used by the GPU; timed-out submissions keep it through the existing submission quarantine.
@interface NVMTLCapturePages : NSObject {
@public nvk_buffer buffer; IOSurfaceRef surface; uint64_t used;
}
@end
@implementation NVMTLCapturePages
- (void)dealloc {
    nvmtl_vk_host_pages_drop(&buffer);
    if (surface) CFRelease(surface);
}
@end

// Shared host-page leases. Keep the current host-copy implementation and its budget;
// only the cache lifetime changes from one texture wrapper to one surface allocation.
static NSMutableDictionary *nvmtl_page_cache;
static dispatch_source_t nvmtl_page_timer;
static void nvmtl_page_cache_start(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        nvmtl_page_cache = [NSMutableDictionary new];
        dispatch_queue_t queue = dispatch_queue_create("nvmtl.page-cache", DISPATCH_QUEUE_SERIAL);
        dispatch_source_t timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, queue);
        if (!timer) return;
        dispatch_source_set_timer(timer, dispatch_time(DISPATCH_TIME_NOW, 500 * NSEC_PER_MSEC),
                                  500 * NSEC_PER_MSEC, 50 * NSEC_PER_MSEC);
        dispatch_source_set_event_handler(timer, ^{
            @autoreleasepool {
                NSMutableArray *released = [NSMutableArray new];
                const uint64_t now = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
                @synchronized ([NVMTLCapturePages class]) {
                    for (id key in [nvmtl_page_cache allKeys]) {
                        NVMTLCapturePages *pages = nvmtl_page_cache[key];
                        if (now >= pages->used && now - pages->used > 2000000000ull) {
                            [released addObject:pages]; [nvmtl_page_cache removeObjectForKey:key];
                        }
                    }
                }
                [released removeAllObjects]; // release imported pages outside the cache lock
            }
        });
        nvmtl_page_timer = timer;
        dispatch_resume(timer);
    });
}
static nvk_buffer *nvmtl_surface_host_pages(NVMTLTexture *r, void *base, size_t bytes) {
    static BOOL on; static dispatch_once_t once;
    dispatch_once(&once, ^{ const char *e = getenv("NVMTL_SURFIN_PAGE_CACHE"); on = !(e && e[0] == '0'); });
    const size_t alignment = nvmtl_vk_host_import_align();
    if (!on || !r->_surf || !base || !bytes || !alignment || (alignment & (alignment - 1))
        || (uintptr_t)base % alignment || bytes % alignment) return &r->_hostPages;
    nvmtl_page_cache_start();
    NSArray *key = @[@(IOSurfaceGetID(r->_surf)), @((uintptr_t)base), @(bytes)];
    NVMTLCapturePages *pages = nil;
    __attribute__((objc_precise_lifetime)) NVMTLCapturePages *old = nil, *discard = nil;
    @synchronized ([NVMTLCapturePages class]) { pages = nvmtl_page_cache[key]; }
    if (!pages) {
        NVMTLCapturePages *current = r->_hostPageLease;
        if (current && current->buffer.map == base && current->buffer.size == bytes) pages = current;
        else {
            pages = [NVMTLCapturePages new];
            if (nvmtl_vk_host_pages_hold(base, bytes, &pages->buffer)) return &r->_hostPages;
            pages->surface = (IOSurfaceRef)CFRetain(r->_surf);
        }
        @synchronized ([NVMTLCapturePages class]) {
            NVMTLCapturePages *existing = nvmtl_page_cache[key];
            if (existing) { discard = pages; pages = existing; }
            else {
                if (nvmtl_page_cache.count >= 64) {
                    id oldest = nil; uint64_t time = UINT64_MAX;
                    for (id k in nvmtl_page_cache) {
                        NVMTLCapturePages *entry = nvmtl_page_cache[k];
                        if (entry->used < time) { time = entry->used; oldest = k; }
                    }
                    if (oldest) { old = nvmtl_page_cache[oldest]; [nvmtl_page_cache removeObjectForKey:oldest]; }
                }
                nvmtl_page_cache[key] = pages;
            }
            pages->used = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
        }
    } else {
        @synchronized ([NVMTLCapturePages class]) { pages->used = clock_gettime_nsec_np(CLOCK_UPTIME_RAW); }
    }
    old = nil;
    discard = nil;
    r->_hostPageLease = pages;
    return &pages->buffer;
}

@implementation NVMTLTexture (CaptureCopyout)
- (id)nvmtlRecordCaptureCopy:(nvk_cmdbuf *)commands {
    NVMTLTexture *r = _parent ? _parent : self;
    if (!r->_surf || r->_vramOn || IOSurfaceGetPlaneCount(r->_surf) != 2 || !nvmtl_surfout_import()) return nil;
    @synchronized (nvmtl_surface_sync(r)) {
        if (![r nvmtlSurfaceCopyable]) return nil;
        const uintptr_t base = (uintptr_t)IOSurfaceGetBaseAddress(r->_surf);
        const uintptr_t plane = (uintptr_t)IOSurfaceGetBaseAddressOfPlane(r->_surf, (uint32_t)r->_plane);
        const size_t alignment = nvmtl_vk_host_import_align();
        size_t bytes = IOSurfaceGetAllocSize(r->_surf);
        if (!base || plane < base || !alignment || (alignment & (alignment - 1))) return nil;
        bytes &= ~(alignment - 1);  // never import beyond the surface allocation
        if (!bytes || plane - base >= bytes) return nil;
        NVMTLCapturePages *pages = r->_capturePages;
        if (!pages || pages->buffer.map != (void *)base || pages->buffer.size != bytes) {
            r->_capturePages = nil;
            pages = [NVMTLCapturePages new];
            if (nvmtl_vk_host_pages_hold((void *)base, bytes, &pages->buffer)) return nil;
            pages->surface = (IOSurfaceRef)CFRetain(r->_surf);
        }
        const size_t row = IOSurfaceGetBytesPerRowOfPlane(r->_surf, (uint32_t)r->_plane);
        if (nvmtl_vk_record_image_to_host(commands, &r->_i, &pages->buffer, (size_t)(plane - base), row)) return nil;
        r->_capturePages = pages;
        return pages;
    }
}
@end
