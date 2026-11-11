// nvmtl_avail.h: uses of Apple objects and symbols that an older macOS does not have.
//
// The plugin is one source for every macOS floor it is built for. A descriptor an app hands the driver belongs to the
// Metal of the running system: a property that Metal gained in a later release does not exist on it, and sending it
// raises "unrecognized selector". A data symbol or C function that arrived later is a null address there, and reading or
// calling it faults.
//
// NVMTL_SINCE(version, type, expression, fallback) evaluates the expression only on a system that has it and yields the
// fallback elsewhere. The fallback at each use is the value the newer Metal documents as that property's default, so the
// driver takes the path an app gets when it never sets the property. In a build whose floor is at or above the version
// the check is constant and the generated code is what it was without the macro.
#ifndef NVMTL_AVAIL_H
#define NVMTL_AVAIL_H
#include <mach/mach.h>

#define NVMTL_SINCE(version, type, expression, fallback) \
    ({ type nvmtl_since_value; \
       if (__builtin_available(macOS version, *)) nvmtl_since_value = (expression); else nvmtl_since_value = (fallback); \
       nvmtl_since_value; })

// The I/O Kit calls take a null port to mean the default main port; kIOMainPortDefault is the named form of that null
// and is a data symbol only macOS 12 and later export.
#define NVMTL_IO_MAIN_PORT ((mach_port_t)MACH_PORT_NULL)

// A build whose floor is OS X 10.11: the unfair lock and the nanosecond clock arrive with macOS 10.12, and on 10.11
// both are null addresses. Below, the lock falls back to the spin lock (the same four bytes, zero when free, so every
// initialiser stays as written) and the clock to the Mach absolute time, which is what the newer call reads for the
// monotonic and uptime clocks; the raw monotonic clock also counts sleep on newer systems and does not here. A build
// for 10.12 or later compiles none of this.
#if defined(__MAC_OS_X_VERSION_MIN_REQUIRED) && __MAC_OS_X_VERSION_MIN_REQUIRED < 101200
#include <os/lock.h>
#include <time.h>
#include <sys/time.h>
#include <stdbool.h>
#include <libkern/OSAtomic.h>
#include <mach/mach_time.h>
static inline void nvmtl_compat_unfair_lock(os_unfair_lock_t l) {
    if (__builtin_available(macOS 10.12, *)) os_unfair_lock_lock(l); else OSSpinLockLock((OSSpinLock *)l);
}
static inline void nvmtl_compat_unfair_unlock(os_unfair_lock_t l) {
    if (__builtin_available(macOS 10.12, *)) os_unfair_lock_unlock(l); else OSSpinLockUnlock((OSSpinLock *)l);
}
static inline bool nvmtl_compat_unfair_trylock(os_unfair_lock_t l) {
    if (__builtin_available(macOS 10.12, *)) return os_unfair_lock_trylock(l);
    return OSSpinLockTry((OSSpinLock *)l);
}
static inline __uint64_t nvmtl_compat_clock_ns(clockid_t id) {
    if (__builtin_available(macOS 10.12, *)) return clock_gettime_nsec_np(id);
    if (id == _CLOCK_REALTIME) { struct timeval tv; gettimeofday(&tv, NULL); return (__uint64_t)tv.tv_sec * 1000000000ull + (__uint64_t)tv.tv_usec * 1000ull; }
    static mach_timebase_info_data_t tb;
    if (!tb.denom) mach_timebase_info(&tb);
    return mach_absolute_time() * tb.numer / tb.denom;
}
static inline int nvmtl_compat_clock_gettime(clockid_t id, struct timespec *ts) {
    if (__builtin_available(macOS 10.12, *)) return clock_gettime(id, ts);
    const __uint64_t ns = nvmtl_compat_clock_ns(id == _CLOCK_REALTIME ? id : _CLOCK_UPTIME_RAW);
    ts->tv_sec = (time_t)(ns / 1000000000ull); ts->tv_nsec = (long)(ns % 1000000000ull); return 0;
}
#define os_unfair_lock_lock(l) nvmtl_compat_unfair_lock(l)
#define os_unfair_lock_unlock(l) nvmtl_compat_unfair_unlock(l)
#define os_unfair_lock_trylock(l) nvmtl_compat_unfair_trylock(l)
#define clock_gettime_nsec_np(id) nvmtl_compat_clock_ns(id)
#define clock_gettime(id, ts) nvmtl_compat_clock_gettime(id, ts)
#endif

#endif
