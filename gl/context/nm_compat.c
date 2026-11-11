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

/*
 * Releases before 10.10 also lack the directory-relative file calls, and releases before 10.12 mkostemp. Mesa
 * uses them for its on-disk shader cache only. Each is rebuilt from the older call: a directory descriptor is
 * turned into its path with F_GETPATH.
 */
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <unistd.h>

static int nm_at_path(int dirfd, const char *path, char *out)
{
    if (!path) { errno = EFAULT; return -1; }
    if (path[0] == '/' || dirfd == AT_FDCWD) {
        if (strlen(path) >= PATH_MAX) { errno = ENAMETOOLONG; return -1; }
        strcpy(out, path);
        return 0;
    }
    char dir[PATH_MAX];
    if (fcntl(dirfd, F_GETPATH, dir) != 0) return -1;
    if (strlen(dir) + 1 + strlen(path) >= PATH_MAX) { errno = ENAMETOOLONG; return -1; }
    snprintf(out, PATH_MAX, "%s/%s", dir, path);
    return 0;
}

int openat(int dirfd, const char *path, int flags, ...)
{
    char full[PATH_MAX];
    mode_t mode = 0;
    if (flags & O_CREAT) { va_list ap; va_start(ap, flags); mode = (mode_t)va_arg(ap, int); va_end(ap); }
    if (nm_at_path(dirfd, path, full) != 0) return -1;
    return open(full, flags, mode);
}

int unlinkat(int dirfd, const char *path, int flags)
{
    char full[PATH_MAX];
    if (nm_at_path(dirfd, path, full) != 0) return -1;
    return (flags & AT_REMOVEDIR) ? rmdir(full) : unlink(full);
}

int fstatat(int dirfd, const char *path, struct stat *st, int flags)
{
    char full[PATH_MAX];
    if (nm_at_path(dirfd, path, full) != 0) return -1;
    return (flags & AT_SYMLINK_NOFOLLOW) ? lstat(full, st) : stat(full, st);
}

int mkostemp(char *tmpl, int flags)
{
    int fd = mkstemp(tmpl);
    if (fd >= 0 && (flags & O_CLOEXEC)) (void)fcntl(fd, F_SETFD, FD_CLOEXEC);
    return fd;
}

/* 10.6 lacks these two string calls (they arrived in 10.7). */
size_t strnlen(const char *s, size_t max)
{
    size_t n = 0;
    while (n < max && s[n]) n++;
    return n;
}

char *strndup(const char *s, size_t max)
{
    size_t n = strnlen(s, max);
    char *d = malloc(n + 1);
    if (!d) return NULL;
    memcpy(d, s, n);
    d[n] = 0;
    return d;
}
