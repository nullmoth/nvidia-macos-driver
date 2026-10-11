/*
 * FreeBSD mutex(9), sx(9) and rwlock(9) on xnu locks.
 *
 * mtx (MTX_DEF) = lck_mtx; mtx (MTX_SPIN) = lck_spin with interrupts disabled while held; sx and rwlock = lck_rw.
 * Owners are recorded so ownership tests, assertions and recursion behave as on FreeBSD:
 *   mtx and rwlock record the xnu thread_t; sx records the struct thread * (LinuxKPI's mutex_is_owned compares
 *   the sx owner word with curthread).
 * Using a lock that was never initialised is a bug in the caller; it panics with the lock's address instead of
 * silently corrupting memory.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/sx.h>
#include <sys/rwlock.h>
#include <sys/proc.h>
#include <kern/locks.h>

extern boolean_t ml_set_interrupts_enabled(boolean_t enable);
/* exported to kexts (verified by tools/kpi_probe.sh) but not declared in the SDK headers */
extern boolean_t lck_mtx_try_lock(lck_mtx_t *lck);
extern boolean_t lck_spin_try_lock(lck_spin_t *lck);

#define CUR_XNU ((uintptr_t)current_thread())

static void lock_object_init(struct lock_object *lo, const char *name, u_int cls, u_int flags)
{
    lo->lo_name = name;
    lo->lo_class = cls;
    lo->lo_flags = LO_INITIALIZED | flags;
}

/* ---- mutex -------------------------------------------------------------------------------------------------------- */

void mtx_init(struct mtx *m, const char *name, const char *type, int opts)
{
    bzero(m, sizeof(*m));
    if (opts & MTX_SPIN) {
        m->mtx_lk = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
        lock_object_init(&m->lock_object, name, NMX_LC_SPIN, (opts & MTX_RECURSE) ? LO_RECURSABLE : 0);
    } else {
        m->mtx_lk = lck_mtx_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
        lock_object_init(&m->lock_object, name, NMX_LC_MTX, LO_SLEEPABLE | ((opts & MTX_RECURSE) ? LO_RECURSABLE : 0));
    }
    if (m->mtx_lk == NULL)
        panic("NullMoth xkpi: mtx_init(%s): no lock memory", name ? name : "?");
}

void mtx_destroy(struct mtx *m)
{
    if (m->mtx_lk == NULL)
        return;
    if (m->lock_object.lo_class == NMX_LC_SPIN)
        lck_spin_free(m->mtx_lk, nmx_lck_grp);
    else
        lck_mtx_free(m->mtx_lk, nmx_lck_grp);
    m->mtx_lk = NULL;
    m->lock_object.lo_flags &= ~LO_INITIALIZED;
}

static void mtx_check(const struct mtx *m)
{
    if (__builtin_expect(m->mtx_lk == NULL, 0))
        panic("NullMoth xkpi: mutex %p (%s) used before mtx_init", m, m->lock_object.lo_name ? m->lock_object.lo_name : "?");
}

void nmx_mtx_lock(struct mtx *m)
{
    mtx_check(m);
    if (m->mtx_owner == CUR_XNU) {
        if (!(m->lock_object.lo_flags & LO_RECURSABLE))
            panic("NullMoth xkpi: recursion on non-recursive mutex %s", m->lock_object.lo_name);
        m->mtx_recurse++;
        return;
    }
    if (m->lock_object.lo_class == NMX_LC_SPIN) {
        boolean_t was = ml_set_interrupts_enabled(FALSE);
        lck_spin_lock(m->mtx_lk);
        m->mtx_saved_intr = was;
    } else {
        lck_mtx_lock(m->mtx_lk);
    }
    m->mtx_owner = CUR_XNU;
}

int nmx_mtx_trylock(struct mtx *m)
{
    mtx_check(m);
    if (m->mtx_owner == CUR_XNU) {
        if (!(m->lock_object.lo_flags & LO_RECURSABLE))
            return 0;
        m->mtx_recurse++;
        return 1;
    }
    if (m->lock_object.lo_class == NMX_LC_SPIN) {
        boolean_t was = ml_set_interrupts_enabled(FALSE);
        if (!lck_spin_try_lock(m->mtx_lk)) {
            ml_set_interrupts_enabled(was);
            return 0;
        }
        m->mtx_saved_intr = was;
    } else if (!lck_mtx_try_lock(m->mtx_lk)) {
        return 0;
    }
    m->mtx_owner = CUR_XNU;
    return 1;
}

void nmx_mtx_unlock(struct mtx *m)
{
    mtx_check(m);
    if (m->mtx_recurse > 0) {
        m->mtx_recurse--;
        return;
    }
    m->mtx_owner = 0;
    if (m->lock_object.lo_class == NMX_LC_SPIN) {
        boolean_t was = (boolean_t)m->mtx_saved_intr;
        lck_spin_unlock(m->mtx_lk);
        ml_set_interrupts_enabled(was);
    } else {
        lck_mtx_unlock(m->mtx_lk);
    }
}

int nmx_mtx_owned(const struct mtx *m)
{
    return m->mtx_owner == CUR_XNU;
}

int nmx_mtx_recursed(const struct mtx *m)
{
    return m->mtx_owner == CUR_XNU && m->mtx_recurse > 0;
}

void nmx_mtx_assert(const struct mtx *m, int what, const char *file, int line)
{
    int owned = nmx_mtx_owned(m);
    if ((what & MA_OWNED) && !owned)
        panic("NullMoth xkpi: mutex %s not owned at %s:%d", m->lock_object.lo_name, file, line);
    if (what == MA_NOTOWNED && owned)
        panic("NullMoth xkpi: mutex %s owned at %s:%d", m->lock_object.lo_name, file, line);
}

void mtx_sysinit(void *arg)
{
    struct mtx_args *a = arg;
    mtx_init(a->ma_mtx, a->ma_desc, NULL, a->ma_opts);
}

/* ---- sx ----------------------------------------------------------------------------------------------------------- */

void sx_init_flags(struct sx *sx, const char *description, int opts)
{
    bzero(sx, sizeof(*sx));
    sx->sx_lk = lck_rw_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    if (sx->sx_lk == NULL)
        panic("NullMoth xkpi: sx_init(%s): no lock memory", description ? description : "?");
    lock_object_init(&sx->lock_object, description, NMX_LC_SX,
                     LO_SLEEPABLE | LO_UPGRADABLE | ((opts & SX_RECURSE) ? LO_RECURSABLE : 0));
}

void sx_destroy(struct sx *sx)
{
    if (sx->sx_lk)
        lck_rw_free(sx->sx_lk, nmx_lck_grp);
    sx->sx_lk = NULL;
    sx->lock_object.lo_flags &= ~LO_INITIALIZED;
}

static void sx_check(const struct sx *sx)
{
    if (__builtin_expect(sx->sx_lk == NULL, 0))
        panic("NullMoth xkpi: sx %p (%s) used before sx_init", sx, sx->lock_object.lo_name ? sx->lock_object.lo_name : "?");
}

void nmx_sx_xlock(struct sx *sx)
{
    sx_check(sx);
    uintptr_t me = (uintptr_t)curthread;
    if (sx->sx_lock == me) {
        if (!(sx->lock_object.lo_flags & LO_RECURSABLE))
            panic("NullMoth xkpi: recursion on non-recursive sx %s", sx->lock_object.lo_name);
        sx->sx_recurse++;
        return;
    }
    __atomic_fetch_add(&sx->sx_waiters, 1, __ATOMIC_RELAXED);
    lck_rw_lock_exclusive(sx->sx_lk);
    __atomic_fetch_sub(&sx->sx_waiters, 1, __ATOMIC_RELAXED);
    sx->sx_lock = me;
}

/* lck_rw has no interruptible acquire: the wait is not cut short by a signal, which is a valid outcome of the _sig forms */
int nmx_sx_xlock_sig(struct sx *sx)
{
    nmx_sx_xlock(sx);
    return 0;
}

void nmx_sx_xunlock(struct sx *sx)
{
    sx_check(sx);
    if (sx->sx_recurse > 0) {
        sx->sx_recurse--;
        return;
    }
    sx->sx_lock = 0;
    lck_rw_unlock_exclusive(sx->sx_lk);
}

void nmx_sx_slock(struct sx *sx)
{
    sx_check(sx);
    __atomic_fetch_add(&sx->sx_waiters, 1, __ATOMIC_RELAXED);
    lck_rw_lock_shared(sx->sx_lk);
    __atomic_fetch_sub(&sx->sx_waiters, 1, __ATOMIC_RELAXED);
    __atomic_fetch_add(&sx->sx_shared, 1, __ATOMIC_RELAXED);
}

int nmx_sx_slock_sig(struct sx *sx)
{
    nmx_sx_slock(sx);
    return 0;
}

void nmx_sx_sunlock(struct sx *sx)
{
    sx_check(sx);
    __atomic_fetch_sub(&sx->sx_shared, 1, __ATOMIC_RELAXED);
    lck_rw_unlock_shared(sx->sx_lk);
}

int nmx_sx_try_xlock(struct sx *sx)
{
    sx_check(sx);
    uintptr_t me = (uintptr_t)curthread;
    if (sx->sx_lock == me) {
        if (!(sx->lock_object.lo_flags & LO_RECURSABLE))
            return 0;
        sx->sx_recurse++;
        return 1;
    }
    if (!lck_rw_try_lock(sx->sx_lk, LCK_RW_TYPE_EXCLUSIVE))
        return 0;
    sx->sx_lock = me;
    return 1;
}

int nmx_sx_try_slock(struct sx *sx)
{
    sx_check(sx);
    if (!lck_rw_try_lock(sx->sx_lk, LCK_RW_TYPE_SHARED))
        return 0;
    __atomic_fetch_add(&sx->sx_shared, 1, __ATOMIC_RELAXED);
    return 1;
}

/*
 * xnu's shared-to-exclusive upgrade drops the shared hold when it fails; FreeBSD's sx_try_upgrade must keep it. The
 * only safe answer that keeps the caller's shared hold is "could not upgrade", which sx_try_upgrade may always give.
 */
int nmx_sx_try_upgrade(struct sx *sx)
{
    return 0;
}

void nmx_sx_downgrade(struct sx *sx)
{
    sx_check(sx);
    sx->sx_lock = 0;
    lck_rw_lock_exclusive_to_shared(sx->sx_lk);
    __atomic_fetch_add(&sx->sx_shared, 1, __ATOMIC_RELAXED);
}

int nmx_sx_xlocked(const struct sx *sx)
{
    return sx->sx_lock == (uintptr_t)curthread;
}

int nmx_sx_has_waiters(const struct sx *sx)
{
    return __atomic_load_n(&sx->sx_waiters, __ATOMIC_RELAXED) > 0;
}

void nmx_sx_unlock(struct sx *sx)
{
    if (sx->sx_lock == (uintptr_t)curthread)
        nmx_sx_xunlock(sx);
    else
        nmx_sx_sunlock(sx);
}

void nmx_sx_assert(const struct sx *sx, int what, const char *file, int line)
{
    int x = nmx_sx_xlocked(sx);
    if ((what & SA_XLOCKED) && !(what & SA_SLOCKED) && !x)
        panic("NullMoth xkpi: sx %s not exclusively held at %s:%d", sx->lock_object.lo_name, file, line);
    if (what == SA_UNLOCKED && x)
        panic("NullMoth xkpi: sx %s held at %s:%d", sx->lock_object.lo_name, file, line);
}

void sx_sysinit(void *arg)
{
    struct sx_args *a = arg;
    sx_init_flags(a->sa_sx, a->sa_desc, a->sa_flags);
}

/* ---- rwlock ------------------------------------------------------------------------------------------------------- */

void rw_init_flags(struct rwlock *rw, const char *name, int opts)
{
    bzero(rw, sizeof(*rw));
    rw->rw_lk = lck_rw_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    if (rw->rw_lk == NULL)
        panic("NullMoth xkpi: rw_init(%s): no lock memory", name ? name : "?");
    lock_object_init(&rw->lock_object, name, NMX_LC_RW, (opts & RW_RECURSE) ? LO_RECURSABLE : 0);
}

void rw_destroy(struct rwlock *rw)
{
    if (rw->rw_lk)
        lck_rw_free(rw->rw_lk, nmx_lck_grp);
    rw->rw_lk = NULL;
    rw->lock_object.lo_flags &= ~LO_INITIALIZED;
}

static void rw_check(const struct rwlock *rw)
{
    if (__builtin_expect(rw->rw_lk == NULL, 0))
        panic("NullMoth xkpi: rwlock %p (%s) used before rw_init", rw, rw->lock_object.lo_name ? rw->lock_object.lo_name : "?");
}

void nmx_rw_rlock(struct rwlock *rw)
{
    rw_check(rw);
    lck_rw_lock_shared(rw->rw_lk);
    __atomic_fetch_add(&rw->rw_readers, 1, __ATOMIC_RELAXED);
}

void nmx_rw_runlock(struct rwlock *rw)
{
    __atomic_fetch_sub(&rw->rw_readers, 1, __ATOMIC_RELAXED);
    lck_rw_unlock_shared(rw->rw_lk);
}

void nmx_rw_wlock(struct rwlock *rw)
{
    rw_check(rw);
    if (rw->rw_owner == CUR_XNU) {
        if (!(rw->lock_object.lo_flags & LO_RECURSABLE))
            panic("NullMoth xkpi: recursion on non-recursive rwlock %s", rw->lock_object.lo_name);
        rw->rw_recurse++;
        return;
    }
    lck_rw_lock_exclusive(rw->rw_lk);
    rw->rw_owner = CUR_XNU;
}

void nmx_rw_wunlock(struct rwlock *rw)
{
    if (rw->rw_recurse > 0) {
        rw->rw_recurse--;
        return;
    }
    rw->rw_owner = 0;
    lck_rw_unlock_exclusive(rw->rw_lk);
}

int nmx_rw_try_rlock(struct rwlock *rw)
{
    rw_check(rw);
    if (!lck_rw_try_lock(rw->rw_lk, LCK_RW_TYPE_SHARED))
        return 0;
    __atomic_fetch_add(&rw->rw_readers, 1, __ATOMIC_RELAXED);
    return 1;
}

int nmx_rw_try_wlock(struct rwlock *rw)
{
    rw_check(rw);
    if (rw->rw_owner == CUR_XNU) {
        if (!(rw->lock_object.lo_flags & LO_RECURSABLE))
            return 0;
        rw->rw_recurse++;
        return 1;
    }
    if (!lck_rw_try_lock(rw->rw_lk, LCK_RW_TYPE_EXCLUSIVE))
        return 0;
    rw->rw_owner = CUR_XNU;
    return 1;
}

/* see nmx_sx_try_upgrade: xnu's upgrade drops the read hold on failure, so this keeps it and reports failure */
int nmx_rw_try_upgrade(struct rwlock *rw)
{
    return 0;
}

void nmx_rw_downgrade(struct rwlock *rw)
{
    rw->rw_owner = 0;
    lck_rw_lock_exclusive_to_shared(rw->rw_lk);
    __atomic_fetch_add(&rw->rw_readers, 1, __ATOMIC_RELAXED);
}

int nmx_rw_wowned(const struct rwlock *rw)
{
    return rw->rw_owner == CUR_XNU;
}

void nmx_rw_unlock(struct rwlock *rw)
{
    if (rw->rw_owner == CUR_XNU)
        nmx_rw_wunlock(rw);
    else
        nmx_rw_runlock(rw);
}

void nmx_rw_assert(const struct rwlock *rw, int what, const char *file, int line)
{
    if ((what & RA_WLOCKED) && !(what & RA_RLOCKED) && !nmx_rw_wowned(rw))
        panic("NullMoth xkpi: rwlock %s not write-held at %s:%d", rw->lock_object.lo_name, file, line);
}

void rw_sysinit(void *arg)
{
    struct rw_args *a = arg;
    rw_init_flags(a->ra_rw, a->ra_desc, a->ra_flags);
}

/* ---- take / drop any lock once (locked callouts) ------------------------------------------------------------------ */

void nmx_lock_acquire(struct lock_object *lo, int shared)
{
    switch (lo->lo_class) {
    case NMX_LC_MTX:
    case NMX_LC_SPIN: nmx_mtx_lock(__containerof(lo, struct mtx, lock_object)); return;
    case NMX_LC_SX:
        if (shared) nmx_sx_slock(__containerof(lo, struct sx, lock_object));
        else nmx_sx_xlock(__containerof(lo, struct sx, lock_object));
        return;
    case NMX_LC_RW:
        if (shared) nmx_rw_rlock(__containerof(lo, struct rwlock, lock_object));
        else nmx_rw_wlock(__containerof(lo, struct rwlock, lock_object));
        return;
    }
    panic("NullMoth xkpi: acquire of a lock of unknown class %u", lo->lo_class);
}

void nmx_lock_drop(struct lock_object *lo)
{
    switch (lo->lo_class) {
    case NMX_LC_MTX:
    case NMX_LC_SPIN: nmx_mtx_unlock(__containerof(lo, struct mtx, lock_object)); return;
    case NMX_LC_SX: nmx_sx_unlock(__containerof(lo, struct sx, lock_object)); return;
    case NMX_LC_RW: nmx_rw_unlock(__containerof(lo, struct rwlock, lock_object)); return;
    }
    panic("NullMoth xkpi: drop of a lock of unknown class %u", lo->lo_class);
}

/* ---- release / re-acquire any lock (sleep and condition-variable waits) ------------------------------------------- */

/* 'how' encodes the mode and the recursion depth released, so the lock comes back exactly as it was */
#define HOW_SHARED (1UL << 31)

uintptr_t nmx_lock_release(struct lock_object *lo)
{
    switch (lo->lo_class) {
    case NMX_LC_MTX:
    case NMX_LC_SPIN: {
        struct mtx *m = __containerof(lo, struct mtx, lock_object);
        uintptr_t depth = (uintptr_t)m->mtx_recurse;
        m->mtx_recurse = 0;
        nmx_mtx_unlock(m);
        return depth;
    }
    case NMX_LC_SX: {
        struct sx *sx = __containerof(lo, struct sx, lock_object);
        if (nmx_sx_xlocked(sx)) {
            uintptr_t depth = (uintptr_t)sx->sx_recurse;
            sx->sx_recurse = 0;
            nmx_sx_xunlock(sx);
            return depth;
        }
        nmx_sx_sunlock(sx);
        return HOW_SHARED;
    }
    case NMX_LC_RW: {
        struct rwlock *rw = __containerof(lo, struct rwlock, lock_object);
        if (nmx_rw_wowned(rw)) {
            uintptr_t depth = (uintptr_t)rw->rw_recurse;
            rw->rw_recurse = 0;
            nmx_rw_wunlock(rw);
            return depth;
        }
        nmx_rw_runlock(rw);
        return HOW_SHARED;
    }
    }
    panic("NullMoth xkpi: release of a lock of unknown class %u", lo->lo_class);
}

void nmx_lock_reacquire(struct lock_object *lo, uintptr_t how)
{
    switch (lo->lo_class) {
    case NMX_LC_MTX:
    case NMX_LC_SPIN: {
        struct mtx *m = __containerof(lo, struct mtx, lock_object);
        nmx_mtx_lock(m);
        m->mtx_recurse = (int)how;
        return;
    }
    case NMX_LC_SX: {
        struct sx *sx = __containerof(lo, struct sx, lock_object);
        if (how == HOW_SHARED) { nmx_sx_slock(sx); return; }
        nmx_sx_xlock(sx);
        sx->sx_recurse = (int)how;
        return;
    }
    case NMX_LC_RW: {
        struct rwlock *rw = __containerof(lo, struct rwlock, lock_object);
        if (how == HOW_SHARED) { nmx_rw_rlock(rw); return; }
        nmx_rw_wlock(rw);
        rw->rw_recurse = (int)how;
        return;
    }
    }
    panic("NullMoth xkpi: re-acquire of a lock of unknown class %u", lo->lo_class);
}
