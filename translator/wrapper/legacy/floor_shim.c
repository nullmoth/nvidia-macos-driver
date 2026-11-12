/* floor_shim.c: one libSystem call that arrives with macOS 10.12, for a translator library meant to load on OS X 10.11.
 *
 * The Rust standard library is built for 10.12 and binds clock_gettime as a required symbol; 10.11 does not export it,
 * so the library would not load there. Linked into the library, this definition makes the name resolve inside it. On a
 * system that has the real call it is used (looked up in the next image); on 10.11 the clock is the Mach absolute time
 * (wall time for the real-time clock). It is linked only by build/legacy/userland/xlate_floor.sh; the release build
 * does not use this file. */
#include <dlfcn.h>
#include <errno.h>
#include <stdint.h>
#include <time.h>
#include <sys/time.h>
#include <mach/mach_time.h>

__attribute__((visibility("hidden"))) int clock_gettime(clockid_t id, struct timespec *ts)
{
    static int (*real)(clockid_t, struct timespec *); static int looked;
    if (!looked) { real = (int (*)(clockid_t, struct timespec *))dlsym(RTLD_NEXT, "clock_gettime"); looked = 1; }
    if (real) return real(id, ts);
    if (!ts) { errno = EINVAL; return -1; }
    if (id == _CLOCK_REALTIME) {
        struct timeval tv; gettimeofday(&tv, NULL);
        ts->tv_sec = tv.tv_sec; ts->tv_nsec = (long)tv.tv_usec * 1000; return 0;
    }
    static mach_timebase_info_data_t tb;
    if (!tb.denom) mach_timebase_info(&tb);
    const uint64_t ns = mach_absolute_time() * tb.numer / tb.denom;
    ts->tv_sec = (time_t)(ns / 1000000000ull); ts->tv_nsec = (long)(ns % 1000000000ull);
    return 0;
}
