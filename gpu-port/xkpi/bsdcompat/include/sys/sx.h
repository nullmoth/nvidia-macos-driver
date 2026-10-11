/*
 * FreeBSD sx(9) (shared/exclusive lock that may be held across a sleep) on xnu, as an xnu lck_rw.
 * The exclusive holder is recorded so sx_xlocked(), sx_unlock() and recursion (SX_RECURSE) work.
 */
#ifndef NMXKPI_SYS_SX_H
#define NMXKPI_SYS_SX_H

#include <sys/types.h>
#include <sys/lock.h>
#include <sys/kernel.h>

struct sx {
    struct lock_object lock_object;
    void *sx_lk;                    /* lck_rw_t * */
    volatile uintptr_t sx_lock;     /* struct thread * of the exclusive holder, 0 otherwise (FreeBSD's owner word) */
    int sx_recurse;
    volatile int sx_shared;         /* number of shared holders (for sx_assert / sx_unlock) */
    volatile int sx_waiters;        /* threads blocked acquiring it (sx_has_waiters) */
};

#define SX_DUPOK        0x01
#define SX_NOPROFILE    0x02
#define SX_NOWITNESS    0x04
#define SX_QUIET        0x08
#define SX_NOADAPTIVE   0x10
#define SX_RECURSE      0x20
#define SX_NEW          0x40

#define SA_LOCKED       LA_LOCKED
#define SA_SLOCKED      LA_SLOCKED
#define SA_XLOCKED      LA_XLOCKED
#define SA_UNLOCKED     LA_UNLOCKED
#define SA_RECURSED     LA_RECURSED
#define SA_NOTRECURSED  LA_NOTRECURSED

void sx_init_flags(struct sx *sx, const char *description, int opts);
void sx_destroy(struct sx *sx);
void nmx_sx_xlock(struct sx *sx);
int nmx_sx_xlock_sig(struct sx *sx);
void nmx_sx_xunlock(struct sx *sx);
void nmx_sx_slock(struct sx *sx);
int nmx_sx_slock_sig(struct sx *sx);
void nmx_sx_sunlock(struct sx *sx);
int nmx_sx_try_xlock(struct sx *sx);
int nmx_sx_try_slock(struct sx *sx);
int nmx_sx_try_upgrade(struct sx *sx);
void nmx_sx_downgrade(struct sx *sx);
int nmx_sx_xlocked(const struct sx *sx);
int nmx_sx_has_waiters(const struct sx *sx);
void nmx_sx_unlock(struct sx *sx);
void nmx_sx_assert(const struct sx *sx, int what, const char *file, int line);

#define sx_init(sx, desc)       sx_init_flags((sx), (desc), 0)
#define sx_xlock(sx)            nmx_sx_xlock(sx)
#define sx_xlock_sig(sx)        nmx_sx_xlock_sig(sx)
#define sx_xunlock(sx)          nmx_sx_xunlock(sx)
#define sx_slock(sx)            nmx_sx_slock(sx)
#define sx_slock_sig(sx)        nmx_sx_slock_sig(sx)
#define sx_sunlock(sx)          nmx_sx_sunlock(sx)
#define sx_try_xlock(sx)        nmx_sx_try_xlock(sx)
#define sx_try_slock(sx)        nmx_sx_try_slock(sx)
#define sx_try_upgrade(sx)      nmx_sx_try_upgrade(sx)
#define sx_downgrade(sx)        nmx_sx_downgrade(sx)
#define sx_xlocked(sx)          nmx_sx_xlocked(sx)
#define sx_has_waiters(sx)      nmx_sx_has_waiters(sx)
#define sx_unlock(sx)           nmx_sx_unlock(sx)
#define sx_xholder(sx)          ((struct thread *)(sx)->sx_lock)
#define SX_OWNER(x)             ((uintptr_t)(x))
#define SX_LOCK_UNLOCKED        0
#ifdef INVARIANTS
#define sx_assert(sx, what)     nmx_sx_assert((sx), (what), __FILE__, __LINE__)
#else
#define sx_assert(sx, what)     do { (void)(sx); } while (0)
#endif
#define sx_sleep(chan, sx, pri, wmesg, timo) nmx_sleep((chan), &(sx)->lock_object, (pri), (wmesg), (timo))

struct sx_args { struct sx *sa_sx; const char *sa_desc; int sa_flags; };
void sx_sysinit(void *arg);
#define SX_SYSINIT_FLAGS(name, sxa, desc, flags)                                    \
    static struct sx_args name##_args = { (sxa), (desc), (flags) };                 \
    SYSINIT(name##_sx_sysinit, SI_SUB_LOCK, SI_ORDER_MIDDLE, sx_sysinit, &name##_args)
#define SX_SYSINIT(name, sxa, desc) SX_SYSINIT_FLAGS(name, sxa, desc, 0)

int nmx_sleep(const void *chan, struct lock_object *lock, int pri, const char *wmesg, int timo);

#endif
