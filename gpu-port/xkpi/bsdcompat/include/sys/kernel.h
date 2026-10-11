/*
 * FreeBSD <sys/kernel.h> on xnu: SYSINIT/SYSUNINIT, ticks and hz.
 *
 * FreeBSD runs SYSINIT functions at boot or module load in (subsystem, order) order. Here each SYSINIT records an entry
 * in the __DATA,nmx_sysinit section of the kext; nmx_sysinit_run() sorts and runs them at kext start and
 * nmx_sysuninit_run() runs the SYSUNINITs in reverse at stop. That is how LinuxKPI's module_init(), static mutexes and
 * DEFINE_* objects come to life on macOS.
 */
#ifndef NMXKPI_SYS_KERNEL_H
#define NMXKPI_SYS_KERNEL_H

#include <sys/types.h>

enum sysinit_sub_id {
    SI_SUB_DUMMY = 0x0000000,
    SI_SUB_TUNABLES = 0x0700000,
    SI_SUB_LOCK = 0x1B00000,
    SI_SUB_EVENTHANDLER = 0x1C00000,
    SI_SUB_KLD = 0x2000000,
    SI_SUB_CPU = 0x2100000,
    SI_SUB_INTRINSIC = 0x2800000,
    SI_SUB_DRIVERS = 0x3100000,
    SI_SUB_ROOT_CONF = 0xb000000,
    SI_SUB_CONFIGURE = 0x3800000,
    SI_SUB_TASKQ = 0x8800000,
    SI_SUB_SMP = 0xf000000,
    SI_SUB_RUN_SCHEDULER = 0xfffffff,
};
enum sysinit_elem_order {
    SI_ORDER_FIRST = 0x0000000,
    SI_ORDER_SECOND = 0x0000001,
    SI_ORDER_THIRD = 0x0000002,
    SI_ORDER_FOURTH = 0x0000003,
    SI_ORDER_FIFTH = 0x0000004,
    SI_ORDER_SIXTH = 0x0000005,
    SI_ORDER_SEVENTH = 0x0000006,
    SI_ORDER_EIGHTH = 0x0000007,
    SI_ORDER_MIDDLE = 0x1000000,
    SI_ORDER_ANY = 0xfffffff,
    SI_ORDER_LAST = 0xfffffff,
};

typedef void (*sysinit_cfunc_t)(const void *);
typedef void (*sysinit_nfunc_t)(void *);

struct sysinit {
    enum sysinit_sub_id subsystem;
    enum sysinit_elem_order order;
    sysinit_cfunc_t func;
    const void *udata;
    int uninit;     /* 1 = SYSUNINIT */
};

#define NMX_SYSINIT_SECTION "__DATA,nmx_sysinit"
#define C_SYSINIT(uniquifier, subsystem, order, func, ident)                                         \
    static const struct sysinit uniquifier##_sys_init = { subsystem, order, (sysinit_cfunc_t)(func), \
        (const void *)(ident), 0 };                                                                    \
    static const struct sysinit *const uniquifier##_sys_init_ptr                                     \
        __attribute__((__used__, __section__(NMX_SYSINIT_SECTION))) = &uniquifier##_sys_init
#undef SYSINIT
#define NMX_SYSINIT(uniquifier, subsystem, order, func, ident) \
    C_SYSINIT(uniquifier, subsystem, order, (sysinit_cfunc_t)(sysinit_nfunc_t)(func), (void *)(ident))
#define C_SYSUNINIT(uniquifier, subsystem, order, func, ident)                                       \
    static const struct sysinit uniquifier##_sys_uninit = { subsystem, order, (sysinit_cfunc_t)(func), \
        (const void *)(ident), 1 };                                                                    \
    static const struct sysinit *const uniquifier##_sys_uninit_ptr                                   \
        __attribute__((__used__, __section__(NMX_SYSINIT_SECTION))) = &uniquifier##_sys_uninit
#define SYSUNINIT(uniquifier, subsystem, order, func, ident) \
    C_SYSUNINIT(uniquifier, subsystem, order, (sysinit_cfunc_t)(sysinit_nfunc_t)(func), (void *)(ident))

#define SYSINIT(u, s, o, f, i) NMX_SYSINIT(u, s, o, f, i)
int nmx_sysinit_run(void);
void nmx_sysuninit_run(void);

/* the system clock: 1000 ticks per second, counted from kext start (FreeBSD's default hz) */
extern int hz;
extern int tick;
extern volatile int ticks;
int nmx_ticks(void);
#define ticks nmx_ticks()

/* tunables: there is no loader.conf on macOS; a tunable keeps its compiled-in default */
#define TUNABLE_INT(path, var)
#define TUNABLE_LONG(path, var)
#define TUNABLE_ULONG(path, var)
#define TUNABLE_INT_FETCH(path, var) 0
#define TUNABLE_LONG_FETCH(path, var) 0
#define TUNABLE_ULONG_FETCH(path, var) 0
#define TUNABLE_STR_FETCH(path, var, size) 0
#define TUNABLE_BOOL_FETCH(path, var) 0

extern int cold;            /* 0: the system is running (a kext never loads before the scheduler) */
extern int rebooting;

#endif
