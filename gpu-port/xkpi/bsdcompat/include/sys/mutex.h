/*
 * FreeBSD mutex(9) on xnu.
 *
 * MTX_DEF (a sleep mutex) is an xnu lck_mtx; MTX_SPIN is an xnu lck_spin taken with interrupts disabled, which is what
 * FreeBSD's spin mutexes guarantee and what code called from interrupt filters relies on. xnu locks are opaque to
 * kexts, so the mutex holds a pointer to one allocated in mtx_init(). The owner is recorded so mtx_owned() and
 * recursion (MTX_RECURSE) work as on FreeBSD.
 */
#ifndef NMXKPI_SYS_MUTEX_H
#define NMXKPI_SYS_MUTEX_H

#include <sys/types.h>
#include <sys/lock.h>
#include <sys/_mutex.h>

/* mtx_init() options */
#define MTX_DEF         0x00000000
#define MTX_SPIN        0x00000001
#define MTX_RECURSE     0x00000004
#define MTX_NOWITNESS   0x00000008
#define MTX_NOPROFILE   0x00000020
#define MTX_NEW         0x00000040
#define MTX_QUIET       0x00040000
#define MTX_DUPOK       0x00400000

/* mtx_assert() */
#define MA_OWNED        LA_XLOCKED
#define MA_NOTOWNED     LA_UNLOCKED
#define MA_RECURSED     LA_RECURSED
#define MA_NOTRECURSED  LA_NOTRECURSED

void mtx_init(struct mtx *m, const char *name, const char *type, int opts);
void mtx_destroy(struct mtx *m);
void nmx_mtx_lock(struct mtx *m);
void nmx_mtx_unlock(struct mtx *m);
int nmx_mtx_trylock(struct mtx *m);
void nmx_mtx_assert(const struct mtx *m, int what, const char *file, int line);
int nmx_mtx_owned(const struct mtx *m);
int nmx_mtx_recursed(const struct mtx *m);

#define mtx_lock(m)             nmx_mtx_lock(m)
#define mtx_unlock(m)           nmx_mtx_unlock(m)
#define mtx_trylock(m)          nmx_mtx_trylock(m)
#define mtx_lock_spin(m)        nmx_mtx_lock(m)
#define mtx_unlock_spin(m)      nmx_mtx_unlock(m)
#define mtx_trylock_spin(m)     nmx_mtx_trylock(m)
#define mtx_lock_flags(m, f)    nmx_mtx_lock(m)
#define mtx_unlock_flags(m, f)  nmx_mtx_unlock(m)
#define mtx_owned(m)            nmx_mtx_owned(m)
#define mtx_recursed(m)         nmx_mtx_recursed(m)
#define mtx_initialized(m)      lock_initialized(&(m)->lock_object)
#define mtx_name(m)             ((m)->lock_object.lo_name)
#ifdef INVARIANTS
#define mtx_assert(m, what)     nmx_mtx_assert((m), (what), __FILE__, __LINE__)
#else
#define mtx_assert(m, what)     do { (void)(m); } while (0)
#endif

/* FreeBSD's msleep/mtx_sleep take a struct mtx and a timeout in ticks; xnu's own msleep() has another signature. */
int nmx_sleep(const void *chan, struct lock_object *lock, int pri, const char *wmesg, int timo);
#undef msleep
#define msleep(chan, mtx, pri, wmesg, timo) nmx_sleep((chan), ((mtx) ? &(mtx)->lock_object : NULL), (pri), (wmesg), (timo))
#define mtx_sleep(chan, mtx, pri, wmesg, timo) msleep(chan, mtx, pri, wmesg, timo)
#define msleep_spin(chan, mtx, wmesg, timo) msleep(chan, mtx, 0, wmesg, timo)

/* statically declared mutexes are initialised at kext start, before any LinuxKPI code runs */
#include <sys/kernel.h>
struct mtx_args { void *ma_mtx; const char *ma_desc; int ma_opts; };
void mtx_sysinit(void *arg);
#define MTX_SYSINIT(name, mtx, desc, opts)                                          \
    static struct mtx_args name##_args = { (mtx), (desc), (opts) };                 \
    SYSINIT(name##_mtx_sysinit, SI_SUB_LOCK, SI_ORDER_MIDDLE, mtx_sysinit, &name##_args)

/* Giant does not exist on macOS; code that asserts it is not held is always right. */
#define GIANT_REQUIRED do { } while (0)
#define DROP_GIANT() do {
#define PICKUP_GIANT() } while (0)

#endif
