// nm_era.h: force-included (-include) when a kext is built for a macOS older than the one the sources are written
// for. NM_ERA is the target release as MMmm (10.6 = 1006, 10.13 = 1013, 11 = 1100). Everything here stands in for a
// name the older kernel headers do not have, so the driver sources stay one copy.
// The graphics TYPES (structs, attribute codes, mode flags) come from the current IOGraphicsTypes.h, placed ahead of
// the era's copy on the include path by the era build script: Apple grew those structs inside their reserved fields,
// so their size and the offsets of the old fields are the same, and an older system ignores the newer fields.
#ifndef NM_ERA_H
#define NM_ERA_H
#if defined(NM_ERA) && defined(__cplusplus)

#include <IOKit/IOLib.h>
#include <kern/thread_call.h>
#include <libkern/OSAtomic.h>
#include <string.h>

// Method-override annotations: older headers do not define them.
#ifndef APPLE_KEXT_OVERRIDE
#define APPLE_KEXT_OVERRIDE override
#endif

// IOMallocZero arrived in 10.15: allocate, then clear.
#if NM_ERA < 1015
static inline void *nm_era_malloc_zero(vm_size_t size)
{
    void *p = IOMalloc(size);
    if (p) bzero(p, size);
    return p;
}
#define IOMallocZero(size) nm_era_malloc_zero(size)
#endif

// thread_call_allocate_with_options arrived in 10.12 and its run-once option in 10.13. The plain allocator gives the same callout without the
// run-once guarantee: a callout entered twice in a row runs twice, which every user of it here tolerates (each one
// re-reads its state under its own lock).
#if NM_ERA < 1013
#ifndef THREAD_CALL_PRIORITY_HIGH
#define THREAD_CALL_PRIORITY_HIGH 0
#define THREAD_CALL_PRIORITY_KERNEL 1
#define THREAD_CALL_PRIORITY_USER 2
#define THREAD_CALL_PRIORITY_LOW 3
#endif
#ifndef THREAD_CALL_OPTIONS_ONCE
#define THREAD_CALL_OPTIONS_ONCE 1
#endif
#define thread_call_allocate_with_options(func, param, pri, options) thread_call_allocate((func), (param))
#endif

// The NVRAM "write now" request: the key string is the one IODTNVRAM has always matched.
#ifndef kIONVRAMSyncNowPropertyKey
#define kIONVRAMSyncNowPropertyKey "IONVRAM-SYNCNOW-PROPERTY"
#endif

// Console "the framebuffer moved" operation (value from the current platform-expert header). An older kernel that
// does not know it leaves the console where it was.
#if NM_ERA < 1012
#define kPEBaseAddressChange 8
#endif

// The clock that keeps counting through sleep arrived in 10.12. Before it, the uptime clock stands in: the only use
// is the age of a utilisation sample, which is discarded across sleep anyway.
#if NM_ERA < 1012
#define mach_continuous_time mach_absolute_time
#endif

// IOFramebuffer's connection member was renamed.
#if NM_ERA < 1013
#define fServerConnect serverConnect
#endif

// Linear brightness is a separate display parameter only on newer systems; older ones have the one brightness key.
#if NM_ERA < 1013
#define gIODisplayLinearBrightnessKey gIODisplayBrightnessKey
#endif

// IOFramebuffer::setGammaTable with the sync flag (five arguments) exists from 10.13.
#if NM_ERA >= 1013
#define NM_ERA_HAS_GAMMA_SYNC 1
#endif

#endif // NM_ERA && C++
#ifndef NM_ERA
#define NM_ERA_HAS_GAMMA_SYNC 1
#endif
#endif
