/* FreeBSD <sys/errno.h> on xnu: xnu's errno values, plus FreeBSD's kernel-internal pseudo-errors LinuxKPI uses. */
#ifndef NMXKPI_SYS_ERRNO_H
#define NMXKPI_SYS_ERRNO_H
#include_next <sys/errno.h>
#ifndef ENOIOCTL
#define ENOIOCTL (-3)       /* ioctl not handled by this layer */
#endif
#ifndef EDIRIOCTL
#define EDIRIOCTL (-4)
#endif
#ifndef ERELOOKUP
#define ERELOOKUP (-5)
#endif
#ifndef ECAPMODE
#define ECAPMODE 94
#endif
#ifndef ENOTCAPABLE
#define ENOTCAPABLE 93
#endif
#ifndef EINTEGRITY
#define EINTEGRITY 97
#endif
#endif
