/* FreeBSD <sys/filio.h>: xnu's generic fd ioctls plus FIODGNAME (the device name of an fd), which xnu lacks */
#ifndef NMXKPI_SYS_FILIO_H
#define NMXKPI_SYS_FILIO_H
#include_next <sys/filio.h>
#include <sys/ioccom.h>
struct fiodgname_arg {
    int len;
    void *buf;
};
#define FIODGNAME _IOW('f', 120, struct fiodgname_arg)
static inline void *fiodgname_buf_get_ptr(void *fgnp, u_long com)
{
    (void)com;
    return ((struct fiodgname_arg *)fgnp)->buf;
}
#endif
