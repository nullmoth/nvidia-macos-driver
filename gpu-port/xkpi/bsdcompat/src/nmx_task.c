/*
 * FreeBSD callout(9) and taskqueue(9) on xnu.
 *
 * Callouts: one softclock thread runs them in deadline order (see below). Callouts initialised with a lock run their
 * function with that lock held, and a stop or reset made under that lock always wins: a stopped locked callout
 * never runs afterwards. callout_drain returns only after a running function has finished.
 *
 * Taskqueues: a queue owns kernel threads (taskqueue_start_threads) that run tasks in priority, then FIFO, order.
 * Enqueueing a pending task only bumps its pending count (FreeBSD's coalescing). Drain waits until the task is
 * neither queued nor running; cancel removes a queued task and reports EBUSY for a running one.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/kernel.h>
#include <sys/lock.h>
#include <sys/mutex.h>
#include <sys/callout.h>
#include <sys/taskqueue.h>
#include <sys/proc.h>
#include <sys/malloc.h>
#include <sys/time.h>
#include <kern/clock.h>

MALLOC_DEFINE(M_TASKQUEUE, "taskqueue", "Task queues");

void nmx_thread_exit_cleanup(void);

/* ---- callout ------------------------------------------------------------------------------------------------------ */

/*
 * One softclock thread runs every callout, as on FreeBSD. Callouts wait on a list ordered by deadline; the thread
 * sleeps until the earliest one (assert_wait_deadline) or until a new earlier one wakes it. "Pending", "running" and
 * "drained" are all decided under the one spin lock, so callout_drain returning means the function has finished
 * and will not start again, with no window in which the softclock thread still touches the callout. xnu's
 * thread_call cannot give that guarantee to a kext (thread_call_cancel_wait is not exported).
 */
static lck_spin_t *cc_lock;
static struct callout *cc_head;             /* singly linked, ordered by c_deadline */
static struct callout *cc_running;          /* the callout whose function runs now */
static thread_t cc_thread;
static volatile int cc_exiting;
static int cc_wakeup_chan;

#define C_NEXT(c) ((c)->c_next)

static void cc_unlink(struct callout *c)
{
    for (struct callout **pp = &cc_head; *pp; pp = &C_NEXT(*pp)) {
        if (*pp == c) {
            *pp = C_NEXT(c);
            C_NEXT(c) = NULL;
            return;
        }
    }
}

static void cc_insert(struct callout *c)
{
    struct callout **pp = &cc_head;
    while (*pp && (*pp)->c_deadline <= c->c_deadline)
        pp = &C_NEXT(*pp);
    C_NEXT(c) = *pp;
    *pp = c;
}

static void softclock_thread(void *arg, wait_result_t wr)
{
    lck_spin_lock(cc_lock);
    while (!cc_exiting) {
        uint64_t now = mach_absolute_time();
        struct callout *c = cc_head;
        if (c != NULL && c->c_deadline <= now) {
            cc_head = C_NEXT(c);
            C_NEXT(c) = NULL;
            c->c_flags &= ~CALLOUT_PENDING;
            cc_running = c;
            c->c_running = 1;
            void (*fn)(void *) = c->c_func;
            void *a = c->c_arg;
            struct lock_object *lk = c->c_lock;
            int shared = (c->c_flags & CALLOUT_SHAREDLOCK) != 0;
            int retunlocked = (c->c_flags & CALLOUT_RETURNUNLOCKED) != 0;
            uint32_t gen = c->c_gen;
            lck_spin_unlock(cc_lock);
            int run = 1;
            if (lk) {
                nmx_lock_acquire(lk, shared);
                /* a stop or reset made under the callout's lock after we picked it up wins */
                if (c->c_gen != gen)
                    run = 0;
            }
            if (run)
                fn(a);
            if (lk && (!run || !retunlocked))
                nmx_lock_drop(lk);
            lck_spin_lock(cc_lock);
            cc_running = NULL;
            c->c_running = 0;
            thread_wakeup_prim((event_t)&c->c_running, FALSE, THREAD_AWAKENED);
            continue;
        }
        if (c != NULL)
            assert_wait_deadline((event_t)&cc_wakeup_chan, THREAD_UNINT, c->c_deadline);
        else
            assert_wait((event_t)&cc_wakeup_chan, THREAD_UNINT);
        lck_spin_unlock(cc_lock);
        thread_block(THREAD_CONTINUE_NULL);
        lck_spin_lock(cc_lock);
    }
    cc_thread = NULL;
    thread_wakeup_prim((event_t)&cc_thread, FALSE, THREAD_AWAKENED);
    lck_spin_unlock(cc_lock);
    thread_terminate(current_thread());
}

int nmx_callout_init(void)
{
    cc_lock = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    if (cc_lock == NULL)
        return ENOMEM;
    thread_t th;
    if (kernel_thread_start(softclock_thread, NULL, &th) != KERN_SUCCESS)
        return ENOMEM;
    thread_set_thread_name(th, "nmx softclock");
    cc_thread = th;
    thread_deallocate(th);
    return 0;
}

void nmx_callout_fini(void)
{
    if (cc_lock == NULL)
        return;
    lck_spin_lock(cc_lock);
    cc_exiting = 1;
    thread_wakeup_prim((event_t)&cc_wakeup_chan, FALSE, THREAD_AWAKENED);
    while (cc_thread != NULL) {
        assert_wait((event_t)&cc_thread, THREAD_UNINT);
        lck_spin_unlock(cc_lock);
        thread_block(THREAD_CONTINUE_NULL);
        lck_spin_lock(cc_lock);
    }
    lck_spin_unlock(cc_lock);
    lck_spin_free(cc_lock, nmx_lck_grp);
    cc_lock = NULL;
}

void callout_init(struct callout *c, int mpsafe)
{
    bzero(c, sizeof(*c));
    c->c_flags = mpsafe ? CALLOUT_MPSAFE : 0;
}

void _callout_init_lock(struct callout *c, struct lock_object *lock, int flags)
{
    callout_init(c, 1);
    c->c_lock = lock;
    c->c_flags |= flags & (CALLOUT_RETURNUNLOCKED | CALLOUT_SHAREDLOCK);
}

int callout_reset_sbt_on(struct callout *c, sbintime_t sbt, sbintime_t prec, void (*func)(void *), void *arg, int cpu,
                         int flags)
{
    uint64_t interval = 0;
    if (flags & C_ABSOLUTE) {
        sbintime_t now = sbinuptime();
        sbt = sbt > now ? sbt - now : 0;
    }
    if (sbt < 0)
        sbt = 0;
    nanoseconds_to_absolutetime((uint64_t)sbttons(sbt), &interval);

    lck_spin_lock(cc_lock);
    int was = (c->c_flags & CALLOUT_PENDING) != 0;
    if (was)
        cc_unlink(c);
    c->c_gen++;
    c->c_func = func;
    c->c_arg = arg;
    c->c_deadline = mach_absolute_time() + interval;
    c->c_flags |= CALLOUT_PENDING | CALLOUT_ACTIVE;
    cc_insert(c);
    int first = (cc_head == c);
    lck_spin_unlock(cc_lock);
    if (first)
        thread_wakeup_prim((event_t)&cc_wakeup_chan, FALSE, THREAD_AWAKENED);
    return was;
}

int _callout_stop_safe(struct callout *c, int flags)
{
    if (cc_lock == NULL)
        return 0;
    lck_spin_lock(cc_lock);
    int was = (c->c_flags & CALLOUT_PENDING) != 0;
    if (was)
        cc_unlink(c);
    c->c_gen++;
    c->c_flags &= ~(CALLOUT_PENDING | CALLOUT_ACTIVE);
    if (flags & CS_DRAIN) {
        while (cc_running == c) {
            assert_wait((event_t)&c->c_running, THREAD_UNINT);
            lck_spin_unlock(cc_lock);
            thread_block(THREAD_CONTINUE_NULL);
            lck_spin_lock(cc_lock);
        }
    }
    lck_spin_unlock(cc_lock);
    return was;
}

int callout_async_drain(struct callout *c, void (*drain)(void *))
{
    int was = _callout_stop_safe(c, CS_DRAIN);
    if (drain)
        drain(c->c_arg);
    return was;
}

/* ---- taskqueue ---------------------------------------------------------------------------------------------------- */

struct taskqueue {
    STAILQ_HEAD(, task) tq_queue;
    struct mtx tq_mutex;
    const char *tq_name;
    struct task *tq_running;        /* task whose function is executing now (one per thread; see tq_running_n) */
    int tq_running_n;
    int tq_tcount;                  /* threads started */
    int tq_alive;                   /* threads still running */
    int tq_flags;
    int tq_blocked;
    taskqueue_enqueue_fn tq_enqueue;
    void *tq_context;
    thread_t tq_threads[16];
    char tq_tdname[32];
};
#define TQ_EXITING 0x1

struct taskqueue *taskqueue_thread;
struct taskqueue *taskqueue_fast;
struct taskqueue *taskqueue_swi;
struct taskqueue *taskqueue_swi_giant;

struct taskqueue *taskqueue_create(const char *name, int mflags, taskqueue_enqueue_fn enqueue, void *context)
{
    struct taskqueue *q = malloc(sizeof(*q), M_TASKQUEUE, mflags | M_ZERO);
    if (q == NULL)
        return NULL;
    STAILQ_INIT(&q->tq_queue);
    mtx_init(&q->tq_mutex, name ? name : "taskqueue", NULL, MTX_DEF);
    q->tq_name = name;
    q->tq_enqueue = enqueue;
    q->tq_context = context;
    return q;
}

struct taskqueue *taskqueue_create_fast(const char *name, int mflags, taskqueue_enqueue_fn enqueue, void *context)
{
    return taskqueue_create(name, mflags, enqueue, context);
}

/* the "enqueue" hook FreeBSD queues use when they own threads: wake one of them */
void taskqueue_thread_enqueue(void *context)
{
    struct taskqueue **qp = context;
    wakeup_one(*qp);
}

static void taskqueue_run_locked(struct taskqueue *q)
{
    struct task *t;
    while (!q->tq_blocked && (t = STAILQ_FIRST(&q->tq_queue)) != NULL) {
        STAILQ_REMOVE_HEAD(&q->tq_queue, ta_link);
        int pending = t->ta_pending;
        t->ta_pending = 0;
        t->ta_flags &= ~TASK_ENQUEUED;
        q->tq_running = t;
        q->tq_running_n++;
        mtx_unlock(&q->tq_mutex);
        t->ta_func(t->ta_context, pending);
        mtx_lock(&q->tq_mutex);
        q->tq_running_n--;
        q->tq_running = NULL;
        wakeup(t);
    }
}

void taskqueue_run(struct taskqueue *q)
{
    mtx_lock(&q->tq_mutex);
    taskqueue_run_locked(q);
    mtx_unlock(&q->tq_mutex);
}

static void taskqueue_thread_loop(void *arg, wait_result_t wr)
{
    struct taskqueue *q = arg;
    mtx_lock(&q->tq_mutex);
    while (!(q->tq_flags & TQ_EXITING)) {
        taskqueue_run_locked(q);
        if (q->tq_flags & TQ_EXITING)
            break;
        msleep(q, &q->tq_mutex, 0, "-", 0);
    }
    taskqueue_run_locked(q);
    q->tq_alive--;
    wakeup(&q->tq_alive);
    mtx_unlock(&q->tq_mutex);
    nmx_thread_exit_cleanup();
    thread_terminate(current_thread());
}

int taskqueue_start_threads(struct taskqueue **tqp, int count, int pri, const char *name, ...)
{
    struct taskqueue *q = *tqp;
    va_list ap;
    va_start(ap, name);
    vsnprintf(q->tq_tdname, sizeof(q->tq_tdname), name, ap);
    va_end(ap);
    if (count > (int)nitems(q->tq_threads))
        count = (int)nitems(q->tq_threads);
    for (int i = 0; i < count; i++) {
        thread_t th;
        if (kernel_thread_start(taskqueue_thread_loop, q, &th) != KERN_SUCCESS)
            return ENOMEM;
        thread_set_thread_name(th, q->tq_tdname);
        q->tq_threads[i] = th;
        mtx_lock(&q->tq_mutex);
        q->tq_tcount++;
        q->tq_alive++;
        mtx_unlock(&q->tq_mutex);
        thread_deallocate(th);
    }
    return 0;
}

static int enqueue_locked(struct taskqueue *q, struct task *t, int flags)
{
    if (t->ta_flags & TASK_ENQUEUED) {
        if (flags & TASKQUEUE_FAIL_IF_PENDING)
            return EEXIST;
        if (t->ta_pending < UINT16_MAX)
            t->ta_pending++;
        return 0;
    }
    if ((flags & TASKQUEUE_FAIL_IF_CANCELING) && (t->ta_flags & TASK_NOENQUEUE))
        return ECANCELED;
    /* insert after the last task of the same or higher priority */
    struct task *prev = NULL, *ins;
    STAILQ_FOREACH(ins, &q->tq_queue, ta_link) {
        if (ins->ta_priority < t->ta_priority)
            break;
        prev = ins;
    }
    if (prev == NULL)
        STAILQ_INSERT_HEAD(&q->tq_queue, t, ta_link);
    else
        STAILQ_INSERT_AFTER(&q->tq_queue, prev, t, ta_link);
    t->ta_pending = 1;
    t->ta_flags |= TASK_ENQUEUED;
    if (q->tq_enqueue)
        q->tq_enqueue(q->tq_context);
    return 0;
}

int taskqueue_enqueue_flags(struct taskqueue *q, struct task *t, int flags)
{
    mtx_lock(&q->tq_mutex);
    int err = enqueue_locked(q, t, flags);
    mtx_unlock(&q->tq_mutex);
    return err;
}

int taskqueue_enqueue(struct taskqueue *q, struct task *t)
{
    return taskqueue_enqueue_flags(q, t, 0);
}

static void timeout_task_fire(void *arg)
{
    struct timeout_task *tt = arg;
    taskqueue_enqueue(tt->q, &tt->t);
}

void _timeout_task_init(struct taskqueue *queue, struct timeout_task *tt, int priority, task_fn_t func, void *context)
{
    TASK_INIT(&tt->t, priority, func, context);
    callout_init(&tt->c, 1);
    tt->q = queue;
    tt->f = 0;
}

int taskqueue_enqueue_timeout_sbt(struct taskqueue *q, struct timeout_task *tt, sbintime_t sbt, sbintime_t pr, int flags)
{
    tt->q = q;
    if (sbt <= 0) {
        _callout_stop_safe(&tt->c, 0);
        return taskqueue_enqueue(q, &tt->t);
    }
    return callout_reset_sbt(&tt->c, sbt, pr, timeout_task_fire, tt, flags);
}

int taskqueue_enqueue_timeout(struct taskqueue *q, struct timeout_task *tt, int nticks)
{
    return taskqueue_enqueue_timeout_sbt(q, tt, tick_sbt * nticks, 0, C_HARDCLOCK);
}

static int task_is_running(struct taskqueue *q, struct task *t)
{
    return q->tq_running == t;
}

int taskqueue_poll_is_busy(struct taskqueue *q, struct task *t)
{
    mtx_lock(&q->tq_mutex);
    int busy = (t->ta_flags & TASK_ENQUEUED) || task_is_running(q, t);
    mtx_unlock(&q->tq_mutex);
    return busy;
}

int taskqueue_cancel(struct taskqueue *q, struct task *t, u_int *pendp)
{
    mtx_lock(&q->tq_mutex);
    u_int pending = 0;
    if (t->ta_flags & TASK_ENQUEUED) {
        STAILQ_REMOVE(&q->tq_queue, t, task, ta_link);
        pending = t->ta_pending;
        t->ta_pending = 0;
        t->ta_flags &= ~TASK_ENQUEUED;
    }
    int busy = task_is_running(q, t);
    mtx_unlock(&q->tq_mutex);
    if (pendp)
        *pendp = pending;
    return busy ? EBUSY : 0;
}

int taskqueue_cancel_timeout(struct taskqueue *q, struct timeout_task *tt, u_int *pendp)
{
    int was = _callout_stop_safe(&tt->c, 0);
    u_int pending = 0;
    int err = taskqueue_cancel(q, &tt->t, &pending);
    if (pendp)
        *pendp = pending + (was ? 1 : 0);
    return err;
}

void taskqueue_drain(struct taskqueue *q, struct task *t)
{
    mtx_lock(&q->tq_mutex);
    while ((t->ta_flags & TASK_ENQUEUED) || task_is_running(q, t))
        msleep(t, &q->tq_mutex, 0, "tqdrain", 0);
    mtx_unlock(&q->tq_mutex);
}

void taskqueue_drain_timeout(struct taskqueue *q, struct timeout_task *tt)
{
    _callout_stop_safe(&tt->c, CS_DRAIN);
    taskqueue_drain(q, &tt->t);
}

void taskqueue_drain_all(struct taskqueue *q)
{
    mtx_lock(&q->tq_mutex);
    while (!STAILQ_EMPTY(&q->tq_queue) || q->tq_running_n > 0) {
        struct task *last = STAILQ_LAST(&q->tq_queue, task, ta_link);
        if (last)
            msleep(last, &q->tq_mutex, 0, "tqdrainall", 0);
        else
            msleep(q->tq_running, &q->tq_mutex, 0, "tqdrainall", 1);
    }
    mtx_unlock(&q->tq_mutex);
}

void taskqueue_quiesce(struct taskqueue *q)
{
    taskqueue_drain_all(q);
}

void taskqueue_block(struct taskqueue *q)
{
    mtx_lock(&q->tq_mutex);
    q->tq_blocked = 1;
    mtx_unlock(&q->tq_mutex);
}

void taskqueue_unblock(struct taskqueue *q)
{
    mtx_lock(&q->tq_mutex);
    q->tq_blocked = 0;
    if (!STAILQ_EMPTY(&q->tq_queue) && q->tq_enqueue)
        q->tq_enqueue(q->tq_context);
    mtx_unlock(&q->tq_mutex);
}

int taskqueue_member(struct taskqueue *q, struct thread *td)
{
    for (int i = 0; i < q->tq_tcount; i++)
        if (q->tq_threads[i] == td->td_xnu)
            return 1;
    return 0;
}

void taskqueue_set_callback(struct taskqueue *q, enum taskqueue_callback_type cb_type, taskqueue_callback_fn callback,
                            void *context)
{
}

void taskqueue_free(struct taskqueue *q)
{
    mtx_lock(&q->tq_mutex);
    q->tq_flags |= TQ_EXITING;
    wakeup(q);
    while (q->tq_alive > 0) {
        wakeup(q);
        msleep(&q->tq_alive, &q->tq_mutex, 0, "tqfree", 0);
    }
    mtx_unlock(&q->tq_mutex);
    mtx_destroy(&q->tq_mutex);
    free(q, M_TASKQUEUE);
}

/* the system queues FreeBSD provides: one general thread queue, and the "fast" queue sharing it */
static struct taskqueue *nmx_tq_thread_self;
int nmx_taskqueue_init(void)
{
    taskqueue_thread = taskqueue_create("thread taskq", M_WAITOK, taskqueue_thread_enqueue, &nmx_tq_thread_self);
    if (taskqueue_thread == NULL)
        return ENOMEM;
    nmx_tq_thread_self = taskqueue_thread;
    int err = taskqueue_start_threads(&taskqueue_thread, 1, PWAIT, "nmx taskq");
    taskqueue_fast = taskqueue_thread;
    taskqueue_swi = taskqueue_thread;
    taskqueue_swi_giant = taskqueue_thread;
    return err;
}

void nmx_taskqueue_fini(void)
{
    if (taskqueue_thread)
        taskqueue_free(taskqueue_thread);
    taskqueue_thread = taskqueue_fast = taskqueue_swi = taskqueue_swi_giant = NULL;
}

/* ---- grouptasks (LinuxKPI tasklets): tasks on the system taskqueue ------------------------------------------------- */

#include <sys/gtaskqueue.h>

struct taskqgroup *qgroup_softirq;

static void gtask_trampoline(void *ctx, int pending)
{
    struct gtask *g = ctx;
    g->gt_fn(g->gt_arg);
}

void nmx_grouptask_init(struct grouptask *gt, int prio, gtask_fn_t *fn, void *arg)
{
    bzero(gt, sizeof(*gt));
    gt->gtask.gt_fn = fn;
    gt->gtask.gt_arg = arg;
    TASK_INIT(&gt->gtask.gt_task, prio, gtask_trampoline, &gt->gtask);
}

void nmx_grouptask_enqueue(struct grouptask *gt)
{
    taskqueue_enqueue(taskqueue_thread, &gt->gtask.gt_task);
}

int taskqgroup_attach_cpu(struct taskqgroup *qgroup, struct grouptask *grptask, void *uniq, int cpu, void *dev,
                          void *irq, const char *name)
{
    grptask->gt_cpu = cpu;
    if (name)
        strlcpy(grptask->gt_name, name, sizeof(grptask->gt_name));
    return 0;
}

void taskqgroup_attach(struct taskqgroup *qgroup, struct grouptask *grptask, void *uniq, void *dev, void *irq,
                       const char *name)
{
    (void)taskqgroup_attach_cpu(qgroup, grptask, uniq, -1, dev, irq, name);
}

void taskqgroup_detach(struct taskqgroup *qgroup, struct grouptask *gtask)
{
    taskqueue_drain(taskqueue_thread, &gtask->gtask.gt_task);
}

void taskqgroup_drain_all(struct taskqgroup *qgroup)
{
    taskqueue_drain_all(taskqueue_thread);
}
