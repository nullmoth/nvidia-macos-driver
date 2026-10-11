/*
 * FreeBSD struct uio on xnu. xnu keeps struct uio opaque to kexts (its uio_t is reached only through accessors), so
 * LinuxKPI code completes it with FreeBSD's layout. The device-node glue builds these for kernel buffers only
 * (UIO_SYSSPACE) and moves the data to the caller's xnu uio itself.
 */
#ifndef NMXKPI_SYS_UIO_H
#define NMXKPI_SYS_UIO_H
#include_next <sys/uio.h>
#include <sys/types.h>
struct thread;
struct uio {
    struct iovec *uio_iov;
    int uio_iovcnt;
    off_t uio_offset;
    ssize_t uio_resid;
    enum uio_seg uio_segflg;
    enum uio_rw uio_rw;
    struct thread *uio_td;
};
int nmx_uiomove(void *cp, int n, struct uio *uio);
#undef uiomove
#define uiomove(cp, n, uio) nmx_uiomove((void *)(cp), (n), (uio))
#endif
