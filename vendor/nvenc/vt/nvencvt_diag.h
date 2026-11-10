// Diagnostic flag files and the files diagnostics write. This library is loaded into every app that plays or encodes
// video, and /tmp is writable by every local user: a flag file there used to switch on logging, statistics and a dump of
// the encoded stream and raw input frames in other users' apps, written to predictable names that a link could redirect.
// A flag file now counts only when root made it (root-owned, a regular file, not writable by others), and every
// diagnostic output is created without following a link, readable by its owner alone.
#pragma once
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
// 0 when the flag file is honoured (the value access() gave), -1 otherwise
static inline int nvencvt_flag(const char *path)
{
    struct stat st;
    return (lstat(path, &st) == 0 && S_ISREG(st.st_mode) && st.st_uid == 0 && !(st.st_mode & 022)) ? 0 : -1;
}
static inline FILE *nvencvt_flag_open(const char *path) { return nvencvt_flag(path) == 0 ? fopen(path, "r") : NULL; }
// mode "a" appends, anything else starts the file again
static inline FILE *nvencvt_out(const char *path, const char *mode)
{
    const int fd = open(path, O_WRONLY | O_CREAT | O_NOFOLLOW | O_CLOEXEC | (mode[0] == 'a' ? O_APPEND : O_TRUNC), 0600);
    if (fd < 0) return NULL;
    FILE *f = fdopen(fd, mode[0] == 'a' ? "a" : "w");
    if (!f) close(fd);
    return f;
}
