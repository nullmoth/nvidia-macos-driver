/* FreeBSD <sys/priority.h>: priority values are accepted and mapped onto xnu thread priorities where it matters. */
#pragma once
#define PRI_MIN 0
#define PRI_MAX 255
#define PRI_MIN_ITHD 0
#define PRI_MAX_ITHD 47
#define PI_REALTIME 0
#define PI_INTR 4
#define PI_AV 4
#define PI_NET 8
#define PI_DISK 12
#define PI_TTY 16
#define PI_DULL 20
#define PI_SOFT 24
#define PI_SWI(x) PI_SOFT
#define PRI_MIN_REALTIME 48
#define PRI_MIN_KERN 80
#define PRI_MAX_KERN 119
#define PSWP (PRI_MIN_KERN + 0)
#define PVM (PRI_MIN_KERN + 4)
#define PINOD (PRI_MIN_KERN + 8)
#define PRIBIO (PRI_MIN_KERN + 12)
#define PVFS (PRI_MIN_KERN + 16)
#define PZERO (PRI_MIN_KERN + 20)
#define PSOCK (PRI_MIN_KERN + 24)
#define PWAIT (PRI_MIN_KERN + 28)
#define PLOCK (PRI_MIN_KERN + 32)
#define PPAUSE (PRI_MIN_KERN + 36)
#define PRI_MIN_TIMESHARE 120
#define PRI_MAX_TIMESHARE 223
#define PUSER PRI_MIN_TIMESHARE
#define PRI_MIN_IDLE 224
#define PRI_MAX_IDLE 255
#define PCATCH 0x100
#define PDROP 0x200
#define PNOLOCK 0x400
#define PRIMASK 0x0ff
