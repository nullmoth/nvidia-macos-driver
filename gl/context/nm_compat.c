/*
 * nm_compat.c: two C library calls Mesa uses that 10.11 does not have. The linker records them as weak imports
 * (they are newer than the deployment target), which on 10.11 would be null pointers at the call. Defining them
 * here makes every reference inside the bundle resolve to these instead; nothing is exported.
 *   clock_gettime   arrived in 10.12
 *   open_memstream  arrived in 10.13
 */
#include <errno.h>
#include <mach/mach_time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

int clock_gettime(clockid_t id, struct timespec *ts)
{
    if (!ts) { errno = EINVAL; return -1; }
    if (id == CLOCK_REALTIME) {
        struct timeval tv;
        if (gettimeofday(&tv, NULL) != 0) return -1;
        ts->tv_sec = tv.tv_sec; ts->tv_nsec = tv.tv_usec * 1000;
        return 0;
    }
    /* every other clock Mesa asks for is a monotonic one */
    static mach_timebase_info_data_t tb;
    if (tb.denom == 0) mach_timebase_info(&tb);
    uint64_t t = mach_absolute_time();
    /* split before scaling so the multiply cannot overflow 64 bits */
    uint64_t ns = (t / tb.denom) * tb.numer + (t % tb.denom) * tb.numer / tb.denom;
    ts->tv_sec = (time_t)(ns / 1000000000ull); ts->tv_nsec = (long)(ns % 1000000000ull);
    return 0;
}

/* A write-only stream that grows a buffer and publishes its address and length when flushed or closed. */
struct nm_memstream { char **out; size_t *len; char *buf; size_t used, cap; };

static int nm_ms_write(void *cookie, const char *data, int n)
{
    struct nm_memstream *m = cookie;
    if (n < 0) return -1;
    if (m->used + (size_t)n + 1 > m->cap) {
        size_t cap = m->cap ? m->cap : 256;
        while (cap < m->used + (size_t)n + 1) cap *= 2;
        char *b = realloc(m->buf, cap);
        if (!b) { errno = ENOMEM; return -1; }
        m->buf = b; m->cap = cap;
    }
    memcpy(m->buf + m->used, data, (size_t)n);
    m->used += (size_t)n;
    m->buf[m->used] = 0;
    *m->out = m->buf; *m->len = m->used;
    return n;
}
static int nm_ms_close(void *cookie)
{
    struct nm_memstream *m = cookie;
    *m->out = m->buf; *m->len = m->used;   /* the caller owns the buffer from here, as with the real call */
    free(m);
    return 0;
}

FILE *open_memstream(char **bufp, size_t *sizep)
{
    if (!bufp || !sizep) { errno = EINVAL; return NULL; }
    struct nm_memstream *m = calloc(1, sizeof *m);
    if (!m) return NULL;
    m->out = bufp; m->len = sizep;
    m->buf = malloc(1);
    if (!m->buf) { free(m); return NULL; }
    m->buf[0] = 0; m->cap = 1;
    *bufp = m->buf; *sizep = 0;
    FILE *f = funopen(m, NULL, nm_ms_write, NULL, nm_ms_close);
    if (!f) { free(m->buf); free(m); return NULL; }
    return f;
}
