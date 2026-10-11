/* FreeBSD <sys/sysent.h>: system-call tables; macOS processes are all native 64-bit. */
#pragma once
#define SV_ILP32 0x000100
#define SV_LP64 0x000200
#define SV_CURPROC_FLAG(x) ((x) == SV_LP64)
/* no FreeBSD system-call ABI exists here, so LinuxKPI always takes its kernel-buffer ioctl path */
#define SV_ABI_FREEBSD 0
#define SV_ABI_LINUX 3
#define SV_PROC_ABI(p) (-1)
/* the calling process's ABI: never Linux's, so drm_prime keeps FreeBSD's (and macOS's) fd flag values */
#define SV_CURPROC_ABI() SV_ABI_FREEBSD
#define SYS_ioctl 54
