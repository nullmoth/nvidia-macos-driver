// nv_floor.h: uses of system symbols that an older macOS does not have.
//
// The library is one source for every macOS floor it is built for. A VideoToolbox or CoreVideo key that arrived in a
// later release is a null address on an older one (a null dictionary key or a null argument to CFEqual faults), and a
// function that arrived later cannot be called there. Each macro below names the release that introduced the symbol:
// in a build whose floor is at or above that release it expands to the plain call, so that build is byte for byte what
// it was; in a build for an older floor it checks the running system first and leaves the feature out where the
// system has none.
#ifndef NV_FLOOR_H
#define NV_FLOOR_H
#include <Availability.h>
#include <stdlib.h>
#include <CoreFoundation/CoreFoundation.h>
#include <CoreVideo/CoreVideo.h>
#include <IOKit/IOKitLib.h>
#include <mach/mach.h>

#define NV_FLOOR __MAC_OS_X_VERSION_MIN_REQUIRED
#define NV_AT_LEAST(v) (defined(__MAC_OS_X_VERSION_MIN_REQUIRED) && __MAC_OS_X_VERSION_MIN_REQUIRED >= (v))
#if NV_AT_LEAST(101400)
#define NV_FLOOR_HAS_10_14 1
#else
#define NV_FLOOR_HAS_10_14 0
#endif
#if NV_AT_LEAST(101500)
#define NV_FLOOR_HAS_10_15 1
#else
#define NV_FLOOR_HAS_10_15 0
#endif
#if NV_AT_LEAST(110000)
#define NV_FLOOR_HAS_11_0 1
#else
#define NV_FLOOR_HAS_11_0 0
#endif
#if NV_AT_LEAST(110300)
#define NV_FLOOR_HAS_11_3 1
#else
#define NV_FLOOR_HAS_11_3 0
#endif
#if NV_AT_LEAST(120000)
#define NV_FLOOR_HAS_12_0 1
#else
#define NV_FLOOR_HAS_12_0 0
#endif
#if NV_AT_LEAST(130000)
#define NV_FLOOR_HAS_13_0 1
#else
#define NV_FLOOR_HAS_13_0 0
#endif
#define NV_CAT_(a, b) a##b
#define NV_CAT(a, b) NV_CAT_(a, b)

// NV_SET(tag, version, dictionary, key, value): the key is listed only where the system has it.
#define NV_SET(tag, ver, d, k, v) NV_CAT(NV_SET_, NV_FLOOR_HAS_##tag)(ver, d, k, v)
#define NV_SET_1(ver, d, k, v) CFDictionarySetValue(d, k, v)
#define NV_SET_0(ver, d, k, v) do { if (__builtin_available(macOS ver, *)) CFDictionarySetValue(d, k, v); } while (0)

// NV_IS(tag, version, a, key): CFEqual(a, key), false where the system has no such key.
#define NV_IS(tag, ver, a, k) NV_CAT(NV_IS_, NV_FLOOR_HAS_##tag)(ver, a, k)
#define NV_IS_1(ver, a, k) CFEqual(a, k)
#define NV_IS_0(ver, a, k) ({ Boolean nv_is_ = false; if (__builtin_available(macOS ver, *)) nv_is_ = CFEqual(a, k); nv_is_; })

// NV_SYM(tag, version, symbol): the symbol, NULL where the system has none.
#define NV_SYM(tag, ver, s) NV_CAT(NV_SYM_, NV_FLOOR_HAS_##tag)(ver, s)
#define NV_SYM_1(ver, s) (s)
#define NV_SYM_0(ver, s) ({ const void *nv_sym_ = NULL; if (__builtin_available(macOS ver, *)) nv_sym_ = (s); nv_sym_; })

// NV_PICK(tag, version, with, without): one of two values by whether the system is at least that release.
#define NV_PICK(tag, ver, a, b) NV_CAT(NV_PICK_, NV_FLOOR_HAS_##tag)(ver, a, b)
#define NV_PICK_1(ver, a, b) (a)
#define NV_PICK_0(ver, a, b) ({ __typeof__(a) nv_pick_ = (b); if (__builtin_available(macOS ver, *)) nv_pick_ = (a); nv_pick_; })

// The I/O Kit calls take a null port to mean the default main port; kIOMainPortDefault is the named form of that null
// and is a data symbol only macOS 12 and later export.
#if NV_FLOOR_HAS_12_0
#define NV_IO_MAIN_PORT kIOMainPortDefault
#else
#define NV_IO_MAIN_PORT ((mach_port_t)MACH_PORT_NULL)
#endif

// Copying an attachment arrives with macOS 12; before it the getter returns a borrowed value, retained here so the
// caller releases it the same way.
#if NV_FLOOR_HAS_12_0
#define NV_COPY_ATTACHMENT(buffer, key, mode) CVBufferCopyAttachment(buffer, key, mode)
#else
static inline CFTypeRef nv_copy_attachment(CVBufferRef buffer, CFStringRef key, CVAttachmentMode *mode) {
    if (__builtin_available(macOS 12.0, *)) return CVBufferCopyAttachment(buffer, key, mode);
    CFTypeRef v = CVBufferGetAttachment(buffer, key, mode);
    return v ? CFRetain(v) : NULL;
}
#define NV_COPY_ATTACHMENT(buffer, key, mode) nv_copy_attachment(buffer, key, mode)
#endif

// aligned_alloc arrives with macOS 10.15; posix_memalign gives the same block and is freed the same way.
#if NV_FLOOR_HAS_10_15
#define NV_ALIGNED_ALLOC(alignment, size) aligned_alloc(alignment, size)
#else
static inline void *nv_aligned_alloc(size_t alignment, size_t size) {
    if (__builtin_available(macOS 10.15, *)) return aligned_alloc(alignment, size);
    void *p = NULL;
    return posix_memalign(&p, alignment, size) == 0 ? p : NULL;
}
#define NV_ALIGNED_ALLOC(alignment, size) nv_aligned_alloc(alignment, size)
#endif

// A table entry that holds the address of a symbol newer than the floor holds NULL on an older system.
#if NV_FLOOR_HAS_10_14
#define NV_ENTRY_PRESENT(p) 1
#else
#define NV_ENTRY_PRESENT(p) ((p) != NULL)
#endif

#endif
