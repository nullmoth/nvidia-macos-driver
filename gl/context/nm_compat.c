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

/*
 * The Vulkan driver's own gaps on the older releases (it is linked with this file too): a random-bytes call, a
 * power-of-ten helper the compiler emits, and four more directory calls. Each is rebuilt from what 10.6 has.
 */
#include <dirent.h>
#include <math.h>

int CCRandomGenerateBytes(void *bytes, size_t count)
{
    unsigned char *p = bytes;
    for (size_t i = 0; i < count; i += 4) {
        uint32_t r = arc4random();
        for (size_t k = 0; k < 4 && i + k < count; k++) p[i + k] = (unsigned char)(r >> (8 * k));
    }
    return 0;   /* kCCSuccess */
}

float __exp10f(float x) { return powf(10.0f, x); }
double __exp10(double x) { return pow(10.0, x); }

int linkat(int fd1, const char *path1, int fd2, const char *path2, int flags)
{
    char a[PATH_MAX], b[PATH_MAX];
    (void)flags;
    if (nm_at_path(fd1, path1, a) != 0 || nm_at_path(fd2, path2, b) != 0) return -1;
    return link(a, b);
}

/* Cloning files is a later file-system feature; callers fall back to copying when it is refused. */
int fclonefileat(int srcfd, int dst_dirfd, const char *dst, int flags)
{
    (void)srcfd; (void)dst_dirfd; (void)dst; (void)flags;
    errno = ENOTSUP;
    return -1;
}

#undef dirfd
int dirfd(DIR *d) { return d ? d->__dd_fd : -1; }

DIR *fdopendir(int fd)
{
    char path[PATH_MAX];
    if (fcntl(fd, F_GETPATH, path) != 0) return NULL;
    DIR *d = opendir(path);
    if (!d) return NULL;
    /* The path was read back from the descriptor and could have been renamed or replaced since: the directory
     * opened by name must be the very one the caller holds, or the call fails. */
    struct stat a, b;
    if (fstat(fd, &a) != 0 || fstat(d->__dd_fd, &b) != 0 || a.st_dev != b.st_dev || a.st_ino != b.st_ino) {
        closedir(d);
        errno = EBADF;
        return NULL;
    }
    close(fd);          /* the stream owns a descriptor of its own; the caller's is consumed as the real call does */
    return d;
}

/*
 * Calls the Rust standard library of the 10.6 driver build makes that 10.6 lacks: three more directory-relative
 * calls, the rename with flags (10.12), and the standard-directory enumeration (10.12).
 */
int fchmodat(int dirfd, const char *path, mode_t mode, int flags)
{
    char full[PATH_MAX];
    /* No flag can be honoured through a path: "do not follow a link" is refused rather than silently followed. */
    if (flags != 0) { errno = EOPNOTSUPP; return -1; }
    if (nm_at_path(dirfd, path, full) != 0) return -1;
    return chmod(full, mode);
}

int mkdirat(int dirfd, const char *path, mode_t mode)
{
    char full[PATH_MAX];
    if (nm_at_path(dirfd, path, full) != 0) return -1;
    return mkdir(full, mode);
}

int renameat(int fd1, const char *from, int fd2, const char *to)
{
    char a[PATH_MAX], b[PATH_MAX];
    if (nm_at_path(fd1, from, a) != 0 || nm_at_path(fd2, to, b) != 0) return -1;
    return rename(a, b);
}

/* Flag 0x4 asks that an existing destination is not replaced. That is done with a hard link, which the system
 * creates only if the name is free, followed by removing the old name: no window in which another process's
 * file can be replaced. Where a link cannot be made (a directory, a volume without links) the call reports
 * "not supported" and the caller takes its own fallback. Other flags are refused. */
int renamex_np(const char *from, const char *to, unsigned int flags)
{
    if (flags & ~0x4u) { errno = EINVAL; return -1; }
    if (!(flags & 0x4u)) return rename(from, to);
    if (link(from, to) != 0) {
        if (errno != EEXIST && errno != ENOENT && errno != EACCES && errno != ENAMETOOLONG) errno = ENOTSUP;
        return -1;
    }
    return unlink(from);
}

/* An enumeration that is empty from the start: callers fall back to their defaults. */
unsigned int sysdir_start_search_path_enumeration(int dir, int domain_mask) { (void)dir; (void)domain_mask; return 0; }
unsigned int sysdir_get_next_search_path_enumeration(unsigned int state, char *path) { (void)state; if (path) path[0] = 0; return 0; }
