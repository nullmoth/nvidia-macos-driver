/* nvmtl_fps_limit.h - a frame-rate limiter for applications, the equivalent of the Windows driver's "Max Frame Rate".
   The limit is applied where the application hands a drawable to the driver: the call sleeps just long enough that
   presents are at least one interval apart, so the application's own render loop slows down and the GPU idles in
   between (lower power and heat, steadier pacing). It never changes what is drawn.

   The setting is NVMTL_FPS_LIMIT, from the user's block of nvrm610.conf. Its value is either one number for every
   application ("60"), or a list of per-application entries ("Game=60,Other=144,*=0"). An entry names an executable
   (getprogname) exactly; "*" is the default for applications not named. 0 means unlimited. The window server and
   any process that is not an application with a window never reach the limiter (the caller passes the executable name
   and the limiter refuses system names). Values outside 10..1000 are rejected as a typing mistake, not clamped.

   Pure over its inputs (names, nanosecond clock), so tools/test_fps_limit.c proves it without a GPU. */
#ifndef NVMTL_FPS_LIMIT_H
#define NVMTL_FPS_LIMIT_H
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <ctype.h>

#define NVMTL_FPS_MIN 10
#define NVMTL_FPS_MAX 1000

/* Parse one number: 0 (unlimited) or NVMTL_FPS_MIN..NVMTL_FPS_MAX. Returns -1 for anything else (empty, text after the
   number, a negative, a value out of range). */
static inline int nvmtl_fps_parse_number(const char *s, size_t n)
{
    if (!s || !n) return -1;
    long v = 0;
    for (size_t i = 0; i < n; i++) {
        if (s[i] < '0' || s[i] > '9') return -1;
        v = v * 10 + (s[i] - '0');
        if (v > 100000) return -1;
    }
    if (v == 0) return 0;
    return (v >= NVMTL_FPS_MIN && v <= NVMTL_FPS_MAX) ? (int)v : -1;
}

/* Processes the limiter never touches, whatever the setting says. */
static inline int nvmtl_fps_system_process(const char *exe)
{
    static const char *const names[] = { "WindowServer", "loginwindow", "Dock", "SystemUIServer", "launchd", "coreaudiod", "mediaanalysisd", 0 };
    if (!exe || !*exe) return 1;
    for (int i = 0; names[i]; i++) if (!strcmp(exe, names[i])) return 1;
    return 0;
}

/* The limit for one executable, from a setting string. 0 = unlimited. A malformed entry is skipped (so a typo costs
   that entry only); a malformed single number is unlimited. */
static inline int nvmtl_fps_limit_for(const char *spec, const char *exe)
{
    if (!spec || !*spec || nvmtl_fps_system_process(exe)) return 0;
    if (!strchr(spec, '=')) { int v = nvmtl_fps_parse_number(spec, strlen(spec)); return v > 0 ? v : 0; }
    int dflt = 0; size_t elen = strlen(exe);
    const char *p = spec;
    while (*p) {
        const char *end = strchr(p, ','); size_t len = end ? (size_t)(end - p) : strlen(p);
        const char *eq = memchr(p, '=', len);
        if (eq) {
            size_t klen = (size_t)(eq - p); int v = nvmtl_fps_parse_number(eq + 1, len - klen - 1);
            if (v >= 0) {
                if (klen == elen && !memcmp(p, exe, elen)) return v;
                if (klen == 1 && p[0] == '*') dflt = v;
            }
        }
        if (!end) break;
        p = end + 1;
    }
    return dflt;
}

typedef struct { uint64_t interval_ns; uint64_t next_ns; } nvmtl_fps_pacer;

static inline void nvmtl_fps_pacer_init(nvmtl_fps_pacer *p, int fps)
{
    p->interval_ns = fps > 0 ? 1000000000ull / (uint64_t)fps : 0; p->next_ns = 0;
}

/* Called at each present with the current monotonic time. Returns how long the caller sleeps before presenting (0 when
   no wait is needed). The schedule advances by exactly one interval per frame, so the average rate is the limit even
   when single frames jitter; after a stall longer than one interval (loading screen, window drag) it restarts from now
   instead of firing a burst of frames to catch up. */
static inline uint64_t nvmtl_fps_pace(nvmtl_fps_pacer *p, uint64_t now_ns)
{
    if (!p->interval_ns) return 0;
    if (!p->next_ns || now_ns >= p->next_ns + p->interval_ns) { p->next_ns = now_ns + p->interval_ns; return 0; }
    uint64_t wait = now_ns < p->next_ns ? p->next_ns - now_ns : 0;
    p->next_ns += p->interval_ns;
    return wait;
}
#endif
