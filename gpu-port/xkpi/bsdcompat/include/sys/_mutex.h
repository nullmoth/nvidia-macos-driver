/* FreeBSD <sys/_mutex.h> on xnu: the mutex structure (see sys/mutex.h). */
#ifndef NMXKPI_SYS__MUTEX_H
#define NMXKPI_SYS__MUTEX_H

#include <sys/types.h>
#include <sys/lock.h>

struct mtx {
    struct lock_object lock_object;
    void *mtx_lk;                   /* lck_mtx_t * (MTX_DEF) or lck_spin_t * (MTX_SPIN) */
    volatile uintptr_t mtx_owner;   /* thread_t of the holder, 0 when free */
    int mtx_recurse;                /* extra acquisitions by the holder (MTX_RECURSE) */
    int mtx_saved_intr;             /* interrupt state before a spin mutex was taken */
};

/* FreeBSD's padded variant; cache-line padding has no functional effect */
struct mtx_padalign {
    struct mtx m;
} __attribute__((__aligned__(64)));

#endif
