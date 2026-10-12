#!/usr/bin/env python3
"""Exercise the production handle fallback with real owned descriptors."""
import pathlib
import shutil
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
compiler = shutil.which('clang') or shutil.which('cc')
if not compiler:
    raise SystemExit('A native C compiler is required for the descriptor regression.')
source = r'''
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include <stdarg.h>
static int denial = EPERM, calls, permission;
static int test_open(const char *path, int flags, ...) {
    calls++;
    if (flags & O_CREAT) {
        va_list a; va_start(a, flags); permission = va_arg(a, int); va_end(a);
    }
    errno = denial; return -1;
}
static int pipe_failure;
static int test_pipe(int p[2]) {
    if (pipe_failure) { errno = EMFILE; return -1; }
    return pipe(p);
}
#define open test_open
#define pipe test_pipe
#include "nvmtl_fd_token.h"
#undef open
#undef pipe
static int descriptors(void) {
    int n=0; for (int fd=0;fd<256;fd++) if (fcntl(fd,F_GETFD)>=0) n++; return n;
}
int main(void) {
    int before=descriptors();
    for (int n=0;n<200;n++) {
        denial = n%2 ? EACCES : EPERM;
        int fd = nvmtl_icd_open("/dev/null",O_RDONLY|O_CLOEXEC);
        assert(fd>=0 && (fcntl(fd,F_GETFD)&FD_CLOEXEC));
        char c; assert(read(fd,&c,1)==0);
        assert(write(fd,"x",1)==-1 && errno==EBADF);
        close(fd);
    }
    assert(descriptors()==before && calls==200);
    denial=ENOENT;
    assert(nvmtl_icd_open("/dev/null",O_RDONLY|O_CLOEXEC)==-1 && errno==ENOENT);
    denial=EPERM;
    assert(nvmtl_icd_open("/another/path",O_RDONLY)==-1 && errno==EPERM);
    assert(nvmtl_icd_open("/dev/null",O_RDWR)==-1 && errno==EPERM);
    assert(nvmtl_icd_open("/dev/null",O_RDONLY|O_NONBLOCK)==-1 && errno==EPERM);
    assert(nvmtl_icd_open("/dev/null",O_CREAT|O_RDWR,0600)==-1 && permission==0600);
    pipe_failure=1;
    assert(nvmtl_icd_open("/dev/null",O_RDONLY|O_CLOEXEC)==-1 && errno==EMFILE);
    assert(descriptors()==before);
    puts("Owned descriptors: 200 permission fallbacks, EOF, close-on-exec, no leaks; unrelated failures preserved.");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path/'test.c').write_text(source)
    subprocess.run([compiler, '-std=c11', '-D_GNU_SOURCE', '-I'+str(root/'plugin'),
                    str(path/'test.c'), '-o', str(path/'test')], check=True)
    subprocess.run([str(path/'test')], check=True)
