/*
 * FreeBSD <sys/lock.h> on xnu.
 *
 * Every FreeBSD lock begins with a struct lock_object. Here it carries the lock's class, which is what lets a single
 * sleep or condition-variable wait release and re-take whichever kind of lock the caller holds (sleep mutex, spin
 * mutex, sx, rwlock) - the job FreeBSD's struct lock_class does. Witness (lock-order checking) does not exist on macOS;
 * its macros compile to nothing.
 */
#ifndef NMXKPI_SYS_LOCK_H
#define NMXKPI_SYS_LOCK_H

#include_next <sys/lock.h>
#include <sys/types.h>
#include <kern/locks.h>

enum nmx_lock_class {
    NMX_LC_NONE = 0,
    NMX_LC_MTX,     /* sleep mutex: lck_mtx */
    NMX_LC_SPIN,    /* spin mutex: lck_spin, interrupts off while held */
    NMX_LC_SX,      /* shared/exclusive, sleepable: lck_rw */
    NMX_LC_RW,      /* reader/writer: lck_rw */
};

struct lock_object {
    const char *lo_name;
    u_int lo_flags;
    u_int lo_class;     /* enum nmx_lock_class */
    u_int lo_data;
};

/* lo_flags, as FreeBSD defines them (only the ones LinuxKPI tests) */
#define LO_CLASSFLAGS   0x0000ffff
#define LO_INITIALIZED  0x00010000
#define LO_WITNESS      0x00020000
#define LO_QUIET        0x00040000
#define LO_RECURSABLE   0x00080000
#define LO_SLEEPABLE    0x00100000
#define LO_UPGRADABLE   0x00200000
#define LO_DUPOK        0x00400000
#define LO_IS_VNODE     0x00800000
#define LO_NEW          0x02000000

#define lock_initialized(lo) ((lo)->lo_flags & LO_INITIALIZED)

/* lock assertions (sys/lock.h LA_*), shared by mtx_assert/sx_assert/rw_assert */
#define LA_MASKASSERT   0x000000ff
#define LA_UNLOCKED     0x00000000
#define LA_SLOCKED      0x00000001
#define LA_XLOCKED      0x00000002
#define LA_LOCKED       (LA_SLOCKED | LA_XLOCKED)
#define LA_RECURSED     0x00000004
#define LA_NOTRECURSED  0x00000008

/* release/re-acquire whatever lock 'lo' is, in the mode the current thread holds it; used by sleep and cv waits */
uintptr_t nmx_lock_release(struct lock_object *lo);
void nmx_lock_reacquire(struct lock_object *lo, uintptr_t how);
/* take / drop any lock once, in shared or exclusive mode (locked callouts) */
void nmx_lock_acquire(struct lock_object *lo, int shared);
void nmx_lock_drop(struct lock_object *lo);

/* xnu lock group every compatibility lock is allocated in (one group, so lock statistics stay in one place) */
extern lck_grp_t *nmx_lck_grp;

#define WITNESS_WARN(flags, lock, fmt, ...) do { } while (0)
#define WITNESS_SAVE_DECL(n)
#define WITNESS_SAVE(lock, n) do { } while (0)
#define WITNESS_RESTORE(lock, n) do { } while (0)
#define WARN_GIANTOK 0x01
#define WARN_PANIC   0x02
#define WARN_SLEEPOK 0x04

#endif
