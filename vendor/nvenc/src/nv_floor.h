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
#if NV_AT_LEAST(101200)
#define NV_FLOOR_HAS_10_12 1
#else
#define NV_FLOOR_HAS_10_12 0
#endif
#if NV_AT_LEAST(101300)
#define NV_FLOOR_HAS_10_13 1
#else
#define NV_FLOOR_HAS_10_13 0
#endif
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

// HEVC and HDR metadata arrive with macOS 10.13 (the parameter-set calls, the HEVC profile levels, the mastering-display
// and content-light keys, the sRGB, PQ and HLG transfer tags). A build for 10.12 or 10.11 serves only those two
// systems, which have none of it, so it is compiled out there: no HEVC encoder or decoder is registered and the HDR
// keys are not carried. NV13(with, without) picks an expression, NV13_DO(...) keeps or drops statements or table rows.
#if NV_FLOOR_HAS_10_13
#define NV_HEVC_HDR 1
#define NV13(with, without) (with)
#define NV13_DO(...) __VA_ARGS__
#else
#define NV_HEVC_HDR 0
#define NV13(with, without) (without)
#define NV13_DO(...)
#endif
// one transfer tag arrives with macOS 10.12
#if NV_FLOOR_HAS_10_12
#define NV12_DO(...) __VA_ARGS__
#else
#define NV12_DO(...)
#endif

// The POSIX clocks arrive with macOS 10.12; a build for 10.11 reads the Mach absolute time, which is what the monotonic
// clocks report there.
#if NV_FLOOR_HAS_10_12
#define NV_CLOCK_GETTIME(id, ts) clock_gettime(id, ts)
#define NV_CLOCK_NS(id) clock_gettime_nsec_np(id)
#else
#include <time.h>
#include <mach/mach_time.h>
static inline unsigned long long nv_mach_ns(void) {
    static mach_timebase_info_data_t tb;
    if (!tb.denom) mach_timebase_info(&tb);
    return mach_absolute_time() * tb.numer / tb.denom;
}
static inline unsigned long long nv_clock_ns(void) {
    if (__builtin_available(macOS 10.12, *)) return clock_gettime_nsec_np(_CLOCK_MONOTONIC_RAW);
    return nv_mach_ns();
}
static inline int nv_clock_gettime(struct timespec *ts) {
    if (__builtin_available(macOS 10.12, *)) return clock_gettime(_CLOCK_MONOTONIC, ts);
    const unsigned long long ns = nv_mach_ns();
    ts->tv_sec = (time_t)(ns / 1000000000ull); ts->tv_nsec = (long)(ns % 1000000000ull); return 0;
}
#define NV_CLOCK_GETTIME(id, ts) nv_clock_gettime(ts)
#define NV_CLOCK_NS(id) nv_clock_ns()
#endif

// A table entry that holds the address of a symbol newer than the floor holds NULL on an older system.
#if NV_FLOOR_HAS_10_14
#define NV_ENTRY_PRESENT(p) 1
#else
#define NV_ENTRY_PRESENT(p) ((p) != NULL)
#endif

#endif
