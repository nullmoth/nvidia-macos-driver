/* FreeBSD rwlock(9) on xnu, as an xnu lck_rw. The write holder is recorded for rw_wowned() and rw_assert(). */
#ifndef NMXKPI_SYS_RWLOCK_H
#define NMXKPI_SYS_RWLOCK_H

#include <sys/types.h>
#include <sys/lock.h>
#include <sys/kernel.h>

struct rwlock {
    struct lock_object lock_object;
    void *rw_lk;                    /* lck_rw_t * */
    volatile uintptr_t rw_owner;    /* thread_t of the write holder */
    int rw_recurse;
    volatile int rw_readers;
};

#define RW_DUPOK        0x01
#define RW_NOPROFILE    0x02
#define RW_NOWITNESS    0x04
#define RW_QUIET        0x08
#define RW_RECURSE      0x10
#define RW_NEW          0x20

#define RA_LOCKED       LA_LOCKED
#define RA_RLOCKED      LA_SLOCKED
#define RA_WLOCKED      LA_XLOCKED
#define RA_UNLOCKED     LA_UNLOCKED
#define RA_RECURSED     LA_RECURSED
#define RA_NOTRECURSED  LA_NOTRECURSED

void rw_init_flags(struct rwlock *rw, const char *name, int opts);
void rw_destroy(struct rwlock *rw);
void nmx_rw_rlock(struct rwlock *rw);
void nmx_rw_runlock(struct rwlock *rw);
void nmx_rw_wlock(struct rwlock *rw);
void nmx_rw_wunlock(struct rwlock *rw);
int nmx_rw_try_rlock(struct rwlock *rw);
int nmx_rw_try_wlock(struct rwlock *rw);
int nmx_rw_try_upgrade(struct rwlock *rw);
void nmx_rw_downgrade(struct rwlock *rw);
int nmx_rw_wowned(const struct rwlock *rw);
void nmx_rw_unlock(struct rwlock *rw);
void nmx_rw_assert(const struct rwlock *rw, int what, const char *file, int line);

#define rw_init(rw, name)       rw_init_flags((rw), (name), 0)
#define rw_rlock(rw)            nmx_rw_rlock(rw)
#define rw_runlock(rw)          nmx_rw_runlock(rw)
#define rw_wlock(rw)            nmx_rw_wlock(rw)
#define rw_wunlock(rw)          nmx_rw_wunlock(rw)
#define rw_try_rlock(rw)        nmx_rw_try_rlock(rw)
#define rw_try_wlock(rw)        nmx_rw_try_wlock(rw)
#define rw_try_upgrade(rw)      nmx_rw_try_upgrade(rw)
#define rw_downgrade(rw)        nmx_rw_downgrade(rw)
#define rw_wowned(rw)           nmx_rw_wowned(rw)
#define rw_unlock(rw)           nmx_rw_unlock(rw)
#define rw_initialized(rw)      lock_initialized(&(rw)->lock_object)
#ifdef INVARIANTS
#define rw_assert(rw, what)     nmx_rw_assert((rw), (what), __FILE__, __LINE__)
#else
#define rw_assert(rw, what)     do { (void)(rw); } while (0)
#endif
#define rw_sleep(chan, rw, pri, wmesg, timo) nmx_sleep((chan), &(rw)->lock_object, (pri), (wmesg), (timo))

struct rw_args { void *ra_rw; const char *ra_desc; int ra_flags; };
void rw_sysinit(void *arg);
#define RW_SYSINIT_FLAGS(name, rw, desc, flags)                                     \
    static struct rw_args name##_args = { (rw), (desc), (flags) };                  \
    SYSINIT(name##_rw_sysinit, SI_SUB_LOCK, SI_ORDER_MIDDLE, rw_sysinit, &name##_args)
#define RW_SYSINIT(name, rw, desc) RW_SYSINIT_FLAGS(name, rw, desc, 0)

int nmx_sleep(const void *chan, struct lock_object *lock, int pri, const char *wmesg, int timo);

#endif
