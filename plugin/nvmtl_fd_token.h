/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#ifndef NVMTL_FD_TOKEN_H
#define NVMTL_FD_TOKEN_H
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <string.h>
#include <unistd.h>

/* The RM bridge uses a read-only null descriptor as an owned handle number.
 * If the host sandbox forbids that path, an anonymous pipe at EOF has the
 * same read-only behavior. No inherited descriptor or filesystem access is
 * borrowed. Other paths, access modes and failures retain normal semantics. */
static int nvmtl_icd_open(const char *path, int flags, ...)
{
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args; va_start(args, flags); mode = (mode_t)va_arg(args, int); va_end(args);
    }
    int fd = open(path, flags, mode);
    if (fd >= 0 || !path || strcmp(path, "/dev/null") ||
        (flags & ~O_CLOEXEC) != O_RDONLY ||
        (errno != EACCES && errno != EPERM)) return fd;
    int ends[2];
    if (pipe(ends)) return -1;
    close(ends[1]);
    if ((flags & O_CLOEXEC) && fcntl(ends[0], F_SETFD, FD_CLOEXEC) < 0) {
        int error = errno; close(ends[0]); errno = error; return -1;
    }
    return ends[0];
}
#endif
