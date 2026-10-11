/* FreeBSD <sys/_callout.h> on xnu: callouts run on the port's softclock thread (src/nmx_task.c). */
#pragma once
#include <sys/types.h>
struct lock_object;
struct callout {
    struct callout *c_next;             /* next callout on the softclock list, ordered by deadline */
    void (*c_func)(void *);
    void *c_arg;
    struct lock_object *c_lock;         /* taken around c_func when set (callout_init_mtx/rw) */
    volatile int c_flags;               /* CALLOUT_* */
    volatile uint32_t c_gen;            /* bumped by stop/reset so a stale firing does nothing */
    volatile int c_running;             /* c_func executing now */
    int c_cpu;
    uint64_t c_deadline;                /* mach absolute time */
};
