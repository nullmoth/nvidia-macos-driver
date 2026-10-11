/*
 * FreeBSD <sys/time.h> on xnu: xnu's timespec/timeval, plus FreeBSD's kernel time API that LinuxKPI's jiffies,
 * ktime and timers are written against. sbintime_t is a 32.32 fixed-point count of seconds; uptime comes from
 * mach_absolute_time, wall time from the xnu calendar clock.
 */
#ifndef NMXKPI_SYS_TIME_H
#define NMXKPI_SYS_TIME_H
#include_next <sys/time.h>
#include <sys/types.h>
/* clock ids LinuxKPI's hrtimers take (FreeBSD's values; xnu's kernel headers have none) */
#ifndef CLOCK_REALTIME
#define CLOCK_REALTIME 0
#endif
#ifndef CLOCK_MONOTONIC
#define CLOCK_MONOTONIC 4
#endif
#define SBT_1S ((sbintime_t)1 << 32)
#define SBT_1M (SBT_1S * 60)
#define SBT_1MS (SBT_1S / 1000)
#define SBT_1US (SBT_1S / 1000000)
#define SBT_1NS (SBT_1S / 1000000000)
#define SBT_MAX 0x7fffffffffffffffLL
extern sbintime_t tick_sbt;
/* conversions split whole seconds from the remainder so no intermediate product can overflow 64 bits */
static inline sbintime_t nstosbt(int64_t ns) { return ((sbintime_t)(ns / 1000000000) << 32) + (sbintime_t)((((uint64_t)(ns % 1000000000)) << 32) / 1000000000); }
static inline sbintime_t ustosbt(int64_t us) { return ((sbintime_t)(us / 1000000) << 32) + (sbintime_t)((((uint64_t)(us % 1000000)) << 32) / 1000000); }
static inline sbintime_t mstosbt(int64_t ms) { return ((sbintime_t)(ms / 1000) << 32) + (sbintime_t)((((uint64_t)(ms % 1000)) << 32) / 1000); }
static inline int64_t sbttons(sbintime_t sbt) { return (int64_t)((sbt >> 32) * 1000000000LL) + (int64_t)(((uint64_t)(sbt & 0xffffffffLL) * 1000000000ULL) >> 32); }
static inline int64_t sbttous(sbintime_t sbt) { return (int64_t)((sbt >> 32) * 1000000LL) + (int64_t)(((uint64_t)(sbt & 0xffffffffLL) * 1000000ULL) >> 32); }
static inline int64_t sbttoms(sbintime_t sbt) { return (int64_t)((sbt >> 32) * 1000LL) + (int64_t)(((uint64_t)(sbt & 0xffffffffLL) * 1000ULL) >> 32); }
static inline sbintime_t tstosbt(struct timespec ts) { return ((sbintime_t)ts.tv_sec << 32) + nstosbt(ts.tv_nsec); }
static inline struct timespec sbttots(sbintime_t sbt) { struct timespec ts; ts.tv_sec = sbt >> 32; ts.tv_nsec = sbttons(sbt & 0xffffffffLL); return ts; }
static inline sbintime_t tvtosbt(struct timeval tv) { return ((sbintime_t)tv.tv_sec << 32) + ustosbt(tv.tv_usec); }
sbintime_t sbinuptime(void);
#define getsbinuptime() sbinuptime()
void nanouptime(struct timespec *ts);
void microuptime(struct timeval *tv);
#define getnanouptime(ts) nanouptime(ts)
#define getmicrouptime(tv) microuptime(tv)
void nanotime(struct timespec *ts);
void nmx_microtime(struct timeval *tv);
#undef microtime
#define microtime(tv) nmx_microtime(tv)
#define getnanotime(ts) nanotime(ts)
#define getmicrotime(tv) nmx_microtime(tv)
extern volatile time_t time_second;
time_t nmx_time_uptime(void);
#define time_uptime nmx_time_uptime()
int ppsratecheck(struct timeval *lasttime, int *curpps, int maxpps);
int ratecheck(struct timeval *lasttime, const struct timeval *mininterval);
int tvtohz(struct timeval *tv);
#ifndef timespecadd
#define timespecadd(tsp, usp, vsp) do { (vsp)->tv_sec = (tsp)->tv_sec + (usp)->tv_sec; \
    (vsp)->tv_nsec = (tsp)->tv_nsec + (usp)->tv_nsec; if ((vsp)->tv_nsec >= 1000000000L) { (vsp)->tv_sec++; (vsp)->tv_nsec -= 1000000000L; } } while (0)
#endif
#ifndef timespecsub
#define timespecsub(tsp, usp, vsp) do { (vsp)->tv_sec = (tsp)->tv_sec - (usp)->tv_sec; \
    (vsp)->tv_nsec = (tsp)->tv_nsec - (usp)->tv_nsec; if ((vsp)->tv_nsec < 0) { (vsp)->tv_sec--; (vsp)->tv_nsec += 1000000000L; } } while (0)
#endif
#ifndef timespeccmp
#define timespeccmp(tvp, uvp, cmp) (((tvp)->tv_sec == (uvp)->tv_sec) ? ((tvp)->tv_nsec cmp (uvp)->tv_nsec) : ((tvp)->tv_sec cmp (uvp)->tv_sec))
#endif
#ifndef timespecclear
#define timespecclear(tvp) ((tvp)->tv_sec = (tvp)->tv_nsec = 0)
#define timespecisset(tvp) ((tvp)->tv_sec || (tvp)->tv_nsec)
#endif
/* timevaladd/timevalsub: xnu declares them */
#ifndef timevalcmp
#define timevalcmp(tvp, uvp, cmp) (((tvp)->tv_sec == (uvp)->tv_sec) ? ((tvp)->tv_usec cmp (uvp)->tv_usec) : ((tvp)->tv_sec cmp (uvp)->tv_sec))
#endif
#endif
