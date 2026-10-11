/*
 * FreeBSD sleeping on xnu: msleep/tsleep/pause/wakeup, condition variables, sleepqueue(9) and sema(9).
 *
 * Every wait is xnu's assert_wait(_deadline) on the channel address followed by thread_block, and every wakeup is
 * thread_wakeup on the same address. The thread asserts its wait before it releases the caller's lock (or the
 * sleepqueue chain lock), so a wakeup issued after that point always finds it: no lost wakeups.
 *
 * Timeouts are FreeBSD ticks (hz = 1000, one millisecond each) or sbintime_t. Results follow FreeBSD: 0 when woken,
 * EWOULDBLOCK on timeout, EINTR or ERESTART when a signal interrupted an interruptible sleep.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/condvar.h>
#include <sys/sleepqueue.h>
#include <sys/sema.h>
#include <sys/proc.h>
#include <sys/smp.h>
#include <sys/time.h>
#include <kern/locks.h>
#include <kern/clock.h>

extern void _disable_preemption(void);
extern void _enable_preemption(void);
extern int cpu_number(void);

static int result_to_errno(wait_result_t wr)
{
    switch (wr) {
    case THREAD_AWAKENED: return 0;
    case THREAD_TIMED_OUT: return EWOULDBLOCK;
    case THREAD_INTERRUPTED: return EINTR;
    case THREAD_RESTART: return ERESTART;
    default: return 0;
    }
}

/* ticks -> absolute deadline; 0 = no deadline */
static uint64_t ticks_deadline(int timo)
{
    uint64_t d = 0;
    if (timo > 0)
        clock_interval_to_deadline((uint32_t)timo, NSEC_PER_SEC / (uint64_t)hz, &d);
    return d;
}

static uint64_t sbt_deadline(sbintime_t sbt, int flags)
{
    uint64_t d = 0;
    if (sbt <= 0)
        return 0;
    if (flags & C_ABSOLUTE) {
        /* absolute sbintime on the uptime clock: convert the remaining interval */
        sbintime_t now = sbinuptime();
        sbt = sbt > now ? sbt - now : 1;
    }
    nanoseconds_to_absolutetime((uint64_t)sbttons(sbt), &d);
    return mach_absolute_time() + d;
}

int nmx_sleep(const void *chan, struct lock_object *lock, int pri, const char *wmesg, int timo)
{
    wait_interrupt_t intr = (pri & PCATCH) ? THREAD_ABORTSAFE : THREAD_UNINT;
    uint64_t deadline = ticks_deadline(timo);
    event_t ev = (event_t)(uintptr_t)(chan ? chan : (const void *)&ev);   /* NULL channel: sleep until timeout */

    if (deadline)
        assert_wait_deadline(ev, intr, deadline);
    else
        assert_wait(ev, intr);
    uintptr_t how = 0;
    if (lock)
        how = nmx_lock_release(lock);
    wait_result_t wr = thread_block(THREAD_CONTINUE_NULL);
    if (lock && !(pri & PDROP))
        nmx_lock_reacquire(lock, how);
    return result_to_errno(wr);
}

int tsleep(const void *chan, int pri, const char *wmesg, int timo)
{
    return nmx_sleep(chan, NULL, pri, wmesg, timo);
}

int pause(const char *wmesg, int timo)
{
    if (timo < 1)
        timo = 1;
    int err = nmx_sleep(NULL, NULL, 0, wmesg, timo);
    return err == EWOULDBLOCK ? 0 : err;
}

int pause_sbt(const char *wmesg, sbintime_t sbt, sbintime_t pr, int flags)
{
    uint64_t deadline = sbt_deadline(sbt, flags);
    event_t ev = (event_t)&deadline;
    if (deadline == 0)
        return 0;
    assert_wait_deadline(ev, (flags & C_CATCH) ? THREAD_ABORTSAFE : THREAD_UNINT, deadline);
    wait_result_t wr = thread_block(THREAD_CONTINUE_NULL);
    return wr == THREAD_TIMED_OUT ? 0 : result_to_errno(wr);
}

void nmx_wakeup(const void *chan)
{
    thread_wakeup_prim((event_t)(uintptr_t)chan, FALSE, THREAD_AWAKENED);
}

void nmx_wakeup_one(const void *chan)
{
    thread_wakeup_prim((event_t)(uintptr_t)chan, TRUE, THREAD_AWAKENED);
}

/* ---- condition variables ------------------------------------------------------------------------------------------ */

void cv_init(struct cv *cvp, const char *desc)
{
    cvp->cv_description = desc;
    cvp->cv_waiters = 0;
}

void cv_destroy(struct cv *cvp)
{
}

static int cv_wait_common(struct cv *cvp, struct lock_object *lock, int intr, uint64_t deadline, int relock)
{
    __atomic_fetch_add(&cvp->cv_waiters, 1, __ATOMIC_RELAXED);
    if (deadline)
        assert_wait_deadline((event_t)cvp, intr ? THREAD_ABORTSAFE : THREAD_UNINT, deadline);
    else
        assert_wait((event_t)cvp, intr ? THREAD_ABORTSAFE : THREAD_UNINT);
    uintptr_t how = nmx_lock_release(lock);
    wait_result_t wr = thread_block(THREAD_CONTINUE_NULL);
    __atomic_fetch_sub(&cvp->cv_waiters, 1, __ATOMIC_RELAXED);
    if (relock)
        nmx_lock_reacquire(lock, how);
    return result_to_errno(wr);
}

void _cv_wait(struct cv *cvp, struct lock_object *lock)
{
    (void)cv_wait_common(cvp, lock, 0, 0, 1);
}

void _cv_wait_unlock(struct cv *cvp, struct lock_object *lock)
{
    (void)cv_wait_common(cvp, lock, 0, 0, 0);
}

int _cv_wait_sig(struct cv *cvp, struct lock_object *lock)
{
    return cv_wait_common(cvp, lock, 1, 0, 1);
}

int _cv_timedwait_sbt(struct cv *cvp, struct lock_object *lock, sbintime_t sbt, sbintime_t pr, int flags)
{
    return cv_wait_common(cvp, lock, 0, sbt_deadline(sbt, flags), 1);
}

int _cv_timedwait_sig_sbt(struct cv *cvp, struct lock_object *lock, sbintime_t sbt, sbintime_t pr, int flags)
{
    return cv_wait_common(cvp, lock, 1, sbt_deadline(sbt, flags), 1);
}

void cv_signal(struct cv *cvp)
{
    if (__atomic_load_n(&cvp->cv_waiters, __ATOMIC_RELAXED) > 0)
        thread_wakeup_prim((event_t)cvp, TRUE, THREAD_AWAKENED);
}

void cv_broadcastpri(struct cv *cvp, int pri)
{
    if (__atomic_load_n(&cvp->cv_waiters, __ATOMIC_RELAXED) > 0)
        thread_wakeup_prim((event_t)cvp, FALSE, THREAD_AWAKENED);
}

/* ---- sleepqueue(9) ------------------------------------------------------------------------------------------------ */

/*
 * Chains: a spin lock per hash bucket guards "about to sleep / waking" for every channel in it, plus a count of the
 * waiters per channel for sleepq_sleepcnt. Between sleepq_add and the wait call the thread holds the chain lock with
 * preemption disabled, so its pending wait (channel, flags, deadline) is kept in a per-CPU slot.
 */
#define SC_TABLESIZE 256
#define SC_WAITERS 8
struct sleepq_chain {
    lck_spin_t *lock;
    struct { const void *wchan; u_int count; } waiters[SC_WAITERS];
};
static struct sleepq_chain sc_table[SC_TABLESIZE];

struct sleepq_pending {
    const void *wchan;
    int flags;
    uint64_t deadline;
};
static struct sleepq_pending sq_pending[MAXCPU];

static struct sleepq_chain *sc_lookup(const void *wchan)
{
    uintptr_t v = (uintptr_t)wchan;
    return &sc_table[((v >> 7) ^ (v >> 15)) & (SC_TABLESIZE - 1)];
}

int nmx_sleepq_init(void)
{
    for (int i = 0; i < SC_TABLESIZE; i++) {
        sc_table[i].lock = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
        if (sc_table[i].lock == NULL)
            return ENOMEM;
    }
    return 0;
}

void nmx_sleepq_fini(void)
{
    for (int i = 0; i < SC_TABLESIZE; i++) {
        if (sc_table[i].lock)
            lck_spin_free(sc_table[i].lock, nmx_lck_grp);
        sc_table[i].lock = NULL;
    }
}

void sleepq_lock(const void *wchan)
{
    lck_spin_lock(sc_lookup(wchan)->lock);
}

void sleepq_release(const void *wchan)
{
    lck_spin_unlock(sc_lookup(wchan)->lock);
}

static void sc_count(struct sleepq_chain *sc, const void *wchan, int delta)
{
    int freeslot = -1;
    for (int i = 0; i < SC_WAITERS; i++) {
        if (sc->waiters[i].wchan == wchan) {
            sc->waiters[i].count += delta;
            if (sc->waiters[i].count == 0)
                sc->waiters[i].wchan = NULL;
            return;
        }
        if (sc->waiters[i].wchan == NULL && freeslot < 0)
            freeslot = i;
    }
    /* more distinct sleeping channels in one bucket than slots: the count is only a hint (see sleepq_sleepcnt) */
    if (delta > 0 && freeslot >= 0) {
        sc->waiters[freeslot].wchan = wchan;
        sc->waiters[freeslot].count = (u_int)delta;
    }
}

void sleepq_add(const void *wchan, struct lock_object *lock, const char *wmesg, int flags, int queue)
{
    struct sleepq_pending *p = &sq_pending[cpu_number()];
    p->wchan = wchan;
    p->flags = flags;
    p->deadline = 0;
}

void sleepq_set_timeout_sbt(const void *wchan, sbintime_t sbt, sbintime_t pr, int flags)
{
    sq_pending[cpu_number()].deadline = sbt_deadline(sbt, flags);
}

static int sleepq_block(const void *wchan, int want_timeout)
{
    struct sleepq_chain *sc = sc_lookup(wchan);
    struct sleepq_pending p = sq_pending[cpu_number()];
    wait_interrupt_t intr = (p.flags & SLEEPQ_INTERRUPTIBLE) ? THREAD_ABORTSAFE : THREAD_UNINT;

    if (want_timeout && p.deadline)
        assert_wait_deadline((event_t)(uintptr_t)wchan, intr, p.deadline);
    else
        assert_wait((event_t)(uintptr_t)wchan, intr);
    sc_count(sc, wchan, 1);
    lck_spin_unlock(sc->lock);
    wait_result_t wr = thread_block(THREAD_CONTINUE_NULL);
    lck_spin_lock(sc->lock);
    sc_count(sc, wchan, -1);
    lck_spin_unlock(sc->lock);
    return result_to_errno(wr);
}

void sleepq_wait(const void *wchan, int pri)
{
    (void)sleepq_block(wchan, 0);
}

int sleepq_wait_sig(const void *wchan, int pri)
{
    return sleepq_block(wchan, 0);
}

int sleepq_timedwait(const void *wchan, int pri)
{
    return sleepq_block(wchan, 1);
}

int sleepq_timedwait_sig(const void *wchan, int pri)
{
    return sleepq_block(wchan, 1);
}

u_int sleepq_sleepcnt(const void *wchan, int queue)
{
    struct sleepq_chain *sc = sc_lookup(wchan);
    for (int i = 0; i < SC_WAITERS; i++)
        if (sc->waiters[i].wchan == wchan)
            return sc->waiters[i].count;
    return 0;
}

int sleepq_signal(const void *wchan, int flags, int pri, int queue)
{
    thread_wakeup_prim((event_t)(uintptr_t)wchan, TRUE, THREAD_AWAKENED);
    return 0;
}

int sleepq_broadcast(const void *wchan, int flags, int pri, int queue)
{
    thread_wakeup_prim((event_t)(uintptr_t)wchan, FALSE, THREAD_AWAKENED);
    return 0;
}

/* waking one specific thread: every waiter on the channel re-checks its condition, which LinuxKPI's waits all do */
void sleepq_remove(struct thread *td, const void *wchan)
{
    thread_wakeup_prim((event_t)(uintptr_t)wchan, FALSE, THREAD_AWAKENED);
}

int sleepq_type(const void *wchan)
{
    return SLEEPQ_SLEEP;
}

/* ---- semaphores ---------------------------------------------------------------------------------------------------- */

void sema_init(struct sema *sema, int value, const char *description)
{
    bzero(sema, sizeof(*sema));
    mtx_init(&sema->sema_mtx, description, "sema backing lock", MTX_DEF);
    cv_init(&sema->sema_cv, description);
    sema->sema_value = value;
}

void sema_destroy(struct sema *sema)
{
    cv_destroy(&sema->sema_cv);
    mtx_destroy(&sema->sema_mtx);
}

void _sema_post(struct sema *sema, const char *file, int line)
{
    mtx_lock(&sema->sema_mtx);
    sema->sema_value++;
    if (sema->sema_waiters && sema->sema_value > 0)
        cv_signal(&sema->sema_cv);
    mtx_unlock(&sema->sema_mtx);
}

void _sema_wait(struct sema *sema, const char *file, int line)
{
    mtx_lock(&sema->sema_mtx);
    while (sema->sema_value == 0) {
        sema->sema_waiters++;
        cv_wait(&sema->sema_cv, &sema->sema_mtx);
        sema->sema_waiters--;
    }
    sema->sema_value--;
    mtx_unlock(&sema->sema_mtx);
}

int _sema_timedwait(struct sema *sema, int timo, const char *file, int line)
{
    int err = 0;
    mtx_lock(&sema->sema_mtx);
    while (sema->sema_value == 0 && err == 0) {
        sema->sema_waiters++;
        err = cv_timedwait(&sema->sema_cv, &sema->sema_mtx, timo);
        sema->sema_waiters--;
    }
    if (sema->sema_value > 0) {
        sema->sema_value--;
        err = 0;
    }
    mtx_unlock(&sema->sema_mtx);
    return err;
}

int _sema_trywait(struct sema *sema, const char *file, int line)
{
    int ok = 0;
    mtx_lock(&sema->sema_mtx);
    if (sema->sema_value > 0) {
        sema->sema_value--;
        ok = 1;
    }
    mtx_unlock(&sema->sema_mtx);
    return ok;
}

int sema_value(struct sema *sema)
{
    return sema->sema_value;
}
