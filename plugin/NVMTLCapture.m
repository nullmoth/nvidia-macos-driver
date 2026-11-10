/*
 * NullMoth NVIDIA driver for macOS
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Both the texture cache and every recording that uses it own the imported pages. Replacing the cache cannot free an
// import still used by the GPU; timed-out submissions keep it through the existing submission quarantine.
@interface NVMTLCapturePages : NSObject {
@public nvk_buffer buffer; IOSurfaceRef surface;
}
@end
@implementation NVMTLCapturePages
- (void)dealloc {
    nvmtl_vk_host_pages_drop(&buffer);
    if (surface) CFRelease(surface);
}
@end

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
