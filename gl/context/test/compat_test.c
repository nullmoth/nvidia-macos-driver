/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Host test of three shims in nm_compat.c. The shim source and this file are both built with the three names
 * prefixed (build line in build_context.sh), so the calls below reach the shims and not the host's own. Works in a fresh private directory under the build output.
 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef AT_FDCWD
#define AT_FDCWD -2
#endif
#ifndef AT_SYMLINK_NOFOLLOW
#define AT_SYMLINK_NOFOLLOW 0x0020
#endif

int nm_fchmodat(int dirfd, const char *path, mode_t mode, int flags);
/* the stream call is declared by the system header under the prefixed name (same -D on this file's build line),
 * which keeps whatever symbol suffix the header gives it */
int nm_renamex_np(const char *from, const char *to, unsigned int flags);

static int failed, arms;
static void arm(const char *name, int ok, long a, long b)
{
    arms++; if (!ok) failed++;
    printf("%s  %s  [%ld, %ld]\n", ok ? "PASS" : "FAIL", name, a, b);
}
static void put(const char *p, const char *text) { FILE *f = fopen(p, "w"); fputs(text, f); fclose(f); }
static int holds(const char *p, const char *text) { char b[32] = {0}; FILE *f = fopen(p, "r"); if (!f) return 0; fread(b, 1, 31, f); fclose(f); return strcmp(b, text) == 0; }

int main(int argc, char **argv)
{
    if (argc < 2 || chdir(argv[1]) != 0) { printf("usage: compat_test <empty work dir>\n"); return 2; }
    struct stat st;

    put("a", "A"); put("b", "B");
    errno = 0; int r = nm_renamex_np("a", "b", 0x4); int e = errno;
    arm("exclusive rename onto an existing name fails and leaves both files as they were", r == -1 && e == EEXIST && holds("a", "A") && holds("b", "B"), r, e);

    r = nm_renamex_np("a", "c", 0x4);
    arm("exclusive rename onto a free name moves the file", r == 0 && holds("c", "A") && lstat("a", &st) != 0, r, 0);

    mkdir("d1", 0700); errno = 0; r = nm_renamex_np("d1", "d2", 0x4); e = errno;
    arm("exclusive rename of a directory reports not-supported instead of a check-then-rename", r == -1 && e == ENOTSUP && lstat("d1", &st) == 0 && lstat("d2", &st) != 0, r, e);

    r = nm_renamex_np("c", "b", 0);
    arm("a plain rename still replaces", r == 0 && holds("b", "A"), r, 0);

    errno = 0; r = nm_renamex_np("b", "e", 0x2); e = errno;
    arm("a flag other than exclusive is refused", r == -1 && e == EINVAL && lstat("b", &st) == 0, r, e);

    put("target", "T"); chmod("target", 0600); symlink("target", "ln");
    errno = 0; r = nm_fchmodat(AT_FDCWD, "ln", 0644, AT_SYMLINK_NOFOLLOW); e = errno; stat("target", &st);
    arm("mode change with the no-follow flag is refused and the link's target keeps its mode", r == -1 && e == EOPNOTSUPP && (st.st_mode & 0777) == 0600, e, st.st_mode & 0777);

    int dfd = open(".", O_RDONLY); r = nm_fchmodat(dfd, "target", 0640, 0); stat("target", &st);
    arm("mode change relative to a directory descriptor, no flags, applies", r == 0 && (st.st_mode & 0777) == 0640, r, st.st_mode & 0777);
    close(dfd);

    int fd = open("d1", O_RDONLY); struct stat want; fstat(fd, &want);
    DIR *d = nm_fdopendir(fd); struct stat got; memset(&got, 0, sizeof got);
    if (d) fstat(dirfd(d), &got);
    arm("a directory stream from a descriptor is the same directory, and the descriptor is consumed", d && got.st_ino == want.st_ino && got.st_dev == want.st_dev && fcntl(fd, F_GETFD) == -1, d != NULL, (long)got.st_ino);
    if (d) closedir(d);

    fd = open("target", O_RDONLY); errno = 0; d = nm_fdopendir(fd); e = errno;
    arm("a descriptor that is not a directory gives no stream and stays open", d == NULL && fcntl(fd, F_GETFD) != -1, d != NULL, e);
    close(fd);

    printf("%d arms, %d failed\n", arms, failed);
    return failed ? 1 : 0;
}
