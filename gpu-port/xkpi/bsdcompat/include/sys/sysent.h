/* FreeBSD <sys/sysent.h>: system-call tables; macOS processes are all native 64-bit. */
#pragma once
#define SV_ILP32 0x000100
#define SV_LP64 0x000200
#define SV_CURPROC_FLAG(x) ((x) == SV_LP64)
