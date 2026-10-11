/*
 * Kernel basics for the FreeBSD-on-xnu layer: per-thread records (struct thread), the current process record,
 * SYSINIT/SYSUNINIT, the tick clock, CPU identification, preemption control and the lock group.
 *
 * Per-thread records: LinuxKPI hangs Linux's task_struct off FreeBSD's struct thread, and DRM's locks record their
 * owner as a struct thread pointer. xnu has no per-thread storage for kexts, so records live in a hash table keyed by
 * thread_t, created on first use. User threads only pass through the driver inside a system call; the entry points
 * release a record the call created (nmx_thread_leave), so records do not accumulate. Kernel threads the port starts
 * keep theirs until they exit.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/kernel.h>
#include <sys/lock.h>
#include <sys/proc.h>
#include <sys/sched.h>
#include <sys/smp.h>
#include <sys/time.h>
#include <machine/md_var.h>
#include <machine/cpufunc.h>
#include <kern/locks.h>
#include <kern/clock.h>
#include <mach/mach_time.h>

#include <sys/module.h>
#include <sys/sysctl.h>

/* xnu calls a kext may link (checked against macOS 15 with tools/kpi_probe.sh); _disable_preemption and
 * ml_get_max_cpus are not among them */
extern boolean_t ml_at_interrupt_context(void);
extern boolean_t ml_set_interrupts_enabled(boolean_t enable);
extern int cpu_number(void);
extern int nmx_xnu_ncpus(void);
/* exported to kexts but not declared in the SDK; the xnu proc_t is used only as an opaque key */
extern void *current_proc(void);

lck_grp_t *nmx_lck_grp;
int hz = 1000;
int tick = 1000000 / 1000;
sbintime_t tick_sbt = SBT_1S / 1000;
int cold = 0;
int rebooting = 0;
int mp_ncpus = 1;
int mp_ncores = 1;
int mp_maxid = 0;
int smp_started = 1;
cpuset_t all_cpus;
bool efi_boot = true;
struct proc *pageproc = NULL;
int (*lkpi_alloc_current)(struct thread *, int);
char osrelease[OSRELEASELEN] = "24.0.0";

/* ---- tick clock: FreeBSD 'ticks' counts hz per second since kext start ------------------------------------------- */

static uint64_t nmx_boot_abs;
static mach_timebase_info_data_t nmx_tb;

static uint64_t abs_to_ns(uint64_t abs)
{
    return abs * nmx_tb.numer / nmx_tb.denom;
}

int nmx_ticks(void)
{
    return (int)(abs_to_ns(mach_absolute_time() - nmx_boot_abs) / (1000000000ULL / (uint64_t)hz));
}

/* LinuxKPI's jiffies: the same clock, 64 bits wide so it never wraps */
unsigned long nmx_jiffies(void)
{
    return (unsigned long)(abs_to_ns(mach_absolute_time() - nmx_boot_abs) / (1000000000ULL / (uint64_t)hz));
}

/* ---- per-thread and per-process records ---------------------------------------------------------------------------- */

#define NMX_THREAD_BUCKETS 256
struct nmx_thread_rec {
    struct thread td;               /* first member: a struct thread * is a struct nmx_thread_rec * */
    struct nmx_thread_rec *next;
    int transient;                  /* created by a system-call entry; freed when that entry returns */
};
static struct nmx_thread_rec *nmx_threads[NMX_THREAD_BUCKETS];
static lck_spin_t *nmx_thread_lock;
static volatile int nmx_thread_count;

#define NMX_PROC_BUCKETS 64
struct nmx_proc_rec {
    struct proc p;
    void *xnu;
    struct nmx_proc_rec *next;
};
static struct nmx_proc_rec *nmx_procs[NMX_PROC_BUCKETS];
static struct proc nmx_kernproc_rec = { .p_pid = 0, .p_comm = "kernel_task" };
struct proc *nmx_kernproc = &nmx_kernproc_rec;
struct proc *proc0 = &nmx_kernproc_rec;

static unsigned bucket_of(const void *p, unsigned n)
{
    uintptr_t v = (uintptr_t)p;
    return (unsigned)((v >> 4) ^ (v >> 12)) & (n - 1);
}

static struct proc *nmx_proc_for(void *xnu)
{
    if (xnu == NULL)
        return nmx_kernproc;
    unsigned b = bucket_of(xnu, NMX_PROC_BUCKETS);
    lck_spin_lock(nmx_thread_lock);
    for (struct nmx_proc_rec *r = nmx_procs[b]; r; r = r->next)
        if (r->xnu == xnu) { lck_spin_unlock(nmx_thread_lock); return &r->p; }
    lck_spin_unlock(nmx_thread_lock);

    struct nmx_proc_rec *nr = IOMallocZero(sizeof(*nr));
    if (nr == NULL)
        return nmx_kernproc;
    nr->xnu = xnu;
    nr->p.nmx_xnu = xnu;
    nr->p.p_pid = proc_pid(xnu);
    proc_name(nr->p.p_pid, nr->p.p_comm, sizeof(nr->p.p_comm));

    lck_spin_lock(nmx_thread_lock);
    for (struct nmx_proc_rec *r = nmx_procs[b]; r; r = r->next)
        if (r->xnu == xnu) { lck_spin_unlock(nmx_thread_lock); IOFree(nr, sizeof(*nr)); return &r->p; }
    nr->next = nmx_procs[b];
    nmx_procs[b] = nr;
    lck_spin_unlock(nmx_thread_lock);
    return &nr->p;
}

struct proc *nmx_curproc(void)
{
    return nmx_proc_for(current_proc());
}

pid_t nmx_proc_pid(struct proc *p)
{
    return p ? p->p_pid : 0;
}

static struct nmx_thread_rec *nmx_thread_find(thread_t xt)
{
    unsigned b = bucket_of(xt, NMX_THREAD_BUCKETS);
    for (struct nmx_thread_rec *r = nmx_threads[b]; r; r = r->next)
        if (r->td.td_xnu == xt)
            return r;
    return NULL;
}

/* the record for the calling thread, created on first use; 'transient' marks one made by a system-call entry */
static struct thread *nmx_thread_get(int transient, int *created)
{
    thread_t xt = current_thread();
    *created = 0;
    lck_spin_lock(nmx_thread_lock);
    struct nmx_thread_rec *r = nmx_thread_find(xt);
    lck_spin_unlock(nmx_thread_lock);
    if (r)
        return &r->td;

    struct nmx_thread_rec *nr = IOMallocZero(sizeof(*nr));
    if (nr == NULL)
        panic("NullMoth xkpi: no memory for a thread record");
    nr->td.td_xnu = xt;
    nr->td.td_proc = nmx_curproc();
    nr->td.td_tid = (int)(thread_tid(xt) & 0x7fffffff);
    nr->td.td_priority = PZERO;
    nr->td.td_base_pri = PZERO;
    nr->transient = transient;

    lck_spin_lock(nmx_thread_lock);
    r = nmx_thread_find(xt);
    if (r == NULL) {
        unsigned b = bucket_of(xt, NMX_THREAD_BUCKETS);
        nr->next = nmx_threads[b];
        nmx_threads[b] = nr;
        nmx_thread_count++;
        r = nr;
        nr = NULL;
        *created = 1;
    }
    lck_spin_unlock(nmx_thread_lock);
    if (nr)
        IOFree(nr, sizeof(*nr));
    return &r->td;
}

struct thread *nmx_curthread(void)
{
    int created;
    struct thread *td = nmx_thread_get(0, &created);
    td->td_intr_nesting_level = ml_at_interrupt_context() ? 1 : 0;
    return td;
}

/* removes and frees the calling thread's record; LinuxKPI's task_struct must already be released by the caller */
static void nmx_thread_drop_current(void)
{
    thread_t xt = current_thread();
    unsigned b = bucket_of(xt, NMX_THREAD_BUCKETS);
    struct nmx_thread_rec *found = NULL;
    lck_spin_lock(nmx_thread_lock);
    for (struct nmx_thread_rec **pp = &nmx_threads[b]; *pp; pp = &(*pp)->next) {
        if ((*pp)->td.td_xnu == xt) {
            found = *pp;
            *pp = found->next;
            nmx_thread_count--;
            break;
        }
    }
    lck_spin_unlock(nmx_thread_lock);
    if (found)
        IOFree(found, sizeof(*found));
}

/*
 * System-call entry and exit for the device-node glue: enter returns non-zero when it created the record, and leave
 * frees it again (after LinuxKPI's per-thread state is released through the hook), unless the thread is one of ours.
 */
void (*nmx_thread_release_hook)(struct thread *td);
int nmx_thread_enter(void)
{
    int created;
    (void)nmx_thread_get(1, &created);
    return created;
}

void nmx_thread_leave(int created)
{
    if (!created)
        return;
    int c2;
    struct thread *td = nmx_thread_get(1, &c2);
    if (nmx_thread_release_hook && td->td_lkpi_task)
        nmx_thread_release_hook(td);
    nmx_thread_drop_current();
}

void nmx_thread_exit_cleanup(void)
{
    nmx_thread_leave(1);
}

struct thread *tdfind(lwpid_t tid, pid_t pid)
{
    struct thread *found = NULL;
    lck_spin_lock(nmx_thread_lock);
    for (unsigned b = 0; b < NMX_THREAD_BUCKETS && !found; b++)
        for (struct nmx_thread_rec *r = nmx_threads[b]; r; r = r->next)
            if (r->td.td_tid == tid && (pid == -1 || nmx_proc_pid(r->td.td_proc) == pid)) { found = &r->td; break; }
    lck_spin_unlock(nmx_thread_lock);
    return found;
}

/* ---- preemption and CPU ------------------------------------------------------------------------------------------- */

/*
 * Critical sections: xnu does not let kexts disable preemption, so a critical section runs with interrupts off on
 * this CPU, which also rules out preemption and migration. Nesting is counted per CPU; the interrupt state from the
 * outermost entry is restored at the outermost exit. LinuxKPI's critical sections are short and never sleep.
 */
static struct { int nest; boolean_t saved; } nmx_crit[MAXCPU];

void nmx_critical_enter(void)
{
    boolean_t was = ml_set_interrupts_enabled(FALSE);
    int c = cpu_number();
    if (nmx_crit[c].nest++ == 0)
        nmx_crit[c].saved = was;
}

void nmx_critical_exit(void)
{
    int c = cpu_number();
    if (--nmx_crit[c].nest == 0)
        ml_set_interrupts_enabled(nmx_crit[c].saved);
}

int nmx_in_critical(void)
{
    return (ml_at_interrupt_context() || nmx_crit[cpu_number()].nest > 0) ? 1 : 0;
}

int nmx_curcpu(void)
{
    return cpu_number();
}

void sched_pin(void)
{
    nmx_critical_enter();
}

void sched_unpin(void)
{
    nmx_critical_exit();
}

void sched_prio(struct thread *td, u_char prio) { if (td) td->td_priority = prio; }
void sched_add(struct thread *td, int flags) { }
int sched_is_bound(struct thread *td) { return 0; }
void sched_bind(struct thread *td, int cpu) { }
void sched_unbind(struct thread *td) { }

void sched_relinquish(struct thread *td)
{
    mi_switch(SW_VOL);
}

void kern_yield(int prio)
{
    mi_switch(0);
}

/* yield: a one-microsecond timed wait always gives the CPU to another runnable thread */
void mi_switch(int flags)
{
    uint64_t deadline;
    clock_interval_to_deadline(1, NSEC_PER_USEC, &deadline);
    assert_wait_deadline((event_t)&deadline, THREAD_UNINT, deadline);
    thread_block(THREAD_CONTINUE_NULL);
}

void maybe_yield(void)
{
    mi_switch(SW_VOL);
}

int rtp_to_pri(struct rtprio *rtp, struct thread *td)
{
    /* Linux's sched_set_fifo: the request is recorded; xnu schedules kernel threads at kernel priority already */
    if (td)
        td->td_priority = (u_char)(PRI_MIN_REALTIME + (rtp ? rtp->prio : 0));
    return 0;
}

unsigned cpu_feature, cpu_feature2, cpu_stdext_feature, cpu_stdext_feature2, cpu_high, cpu_id, cpu_vendor_id;
unsigned amd_feature, amd_feature2;
char cpu_vendor[20];
int cpu_clflush_line_size = 64;

void nmx_cpu_identify(void)
{
    unsigned r[4];
    do_cpuid(0, r);
    cpu_high = r[0];
    memcpy(cpu_vendor, &r[1], 4);
    memcpy(cpu_vendor + 4, &r[3], 4);
    memcpy(cpu_vendor + 8, &r[2], 4);
    cpu_vendor[12] = 0;
    cpu_vendor_id = strcmp(cpu_vendor, "GenuineIntel") == 0 ? CPU_VENDOR_INTEL :
                    strcmp(cpu_vendor, "AuthenticAMD") == 0 ? CPU_VENDOR_AMD :
                    strcmp(cpu_vendor, "HygonGenuine") == 0 ? CPU_VENDOR_HYGON : 0;
    do_cpuid(1, r);
    cpu_id = r[0];
    cpu_feature = r[3];
    cpu_feature2 = r[2];
    cpu_clflush_line_size = (int)(((r[1] >> 8) & 0xff) * 8);
    if (cpu_high >= 7) {
        cpuid_count(7, 0, r);
        cpu_stdext_feature = r[1];
        cpu_stdext_feature2 = r[2];
    }
    do_cpuid(0x80000000, r);
    if (r[0] >= 0x80000001) {
        do_cpuid(0x80000001, r);
        amd_feature = r[3];
        amd_feature2 = r[2];
    }
}

/* ---- SYSINIT / SYSUNINIT ------------------------------------------------------------------------------------------ */

extern const struct sysinit *const nmx_sysinit_start __asm("section$start$__DATA$nmx_sysinit");
extern const struct sysinit *const nmx_sysinit_end __asm("section$end$__DATA$nmx_sysinit");

static int sysinit_count(void)
{
    return (int)(&nmx_sysinit_end - &nmx_sysinit_start);
}

static void sysinit_sorted(const struct sysinit **out, int n, int uninit)
{
    int k = 0;
    for (const struct sysinit *const *p = &nmx_sysinit_start; p < &nmx_sysinit_end; p++)
        if ((*p)->uninit == uninit)
            out[k++] = *p;
    /* insertion sort by (subsystem, order): a few hundred entries at most, run once */
    for (int i = 1; i < k; i++) {
        const struct sysinit *v = out[i];
        int j = i - 1;
        while (j >= 0 && (out[j]->subsystem > v->subsystem ||
                          (out[j]->subsystem == v->subsystem && out[j]->order > v->order))) {
            out[j + 1] = out[j];
            j--;
        }
        out[j + 1] = v;
    }
    for (int i = k; i < n; i++)
        out[i] = NULL;
}

int nmx_sysinit_ran;

int nmx_sysinit_run(void)
{
    int n = sysinit_count();
    if (n <= 0) {
        /* a missing set means every module_init and static lock silently did nothing: refuse to start */
        printf("NullMoth xkpi: SYSINIT set is empty (%d) - the build lost its initialisers; not starting\n", n);
        return ENOEXEC;
    }
    const struct sysinit **v = IOMallocZero(sizeof(*v) * (size_t)n);
    if (v == NULL)
        return ENOMEM;
    sysinit_sorted(v, n, 0);
    for (int i = 0; i < n && v[i]; i++) {
        v[i]->func(v[i]->udata);
        nmx_sysinit_ran++;
    }
    IOFree(v, sizeof(*v) * (size_t)n);
    printf("NullMoth xkpi: ran %d SYSINITs\n", nmx_sysinit_ran);
    return 0;
}

void nmx_sysuninit_run(void)
{
    int n = sysinit_count();
    if (n <= 0)
        return;
    const struct sysinit **v = IOMallocZero(sizeof(*v) * (size_t)n);
    if (v == NULL)
        return;
    sysinit_sorted(v, n, 1);
    int k = 0;
    while (k < n && v[k])
        k++;
    for (int i = k - 1; i >= 0; i--)
        v[i]->func(v[i]->udata);
    IOFree(v, sizeof(*v) * (size_t)n);
}

void nmx_module_event(void *arg)
{
    moduledata_t *md = arg;
    if (md && md->evhand) {
        int err = md->evhand(NULL, MOD_LOAD, md->priv);
        if (err)
            printf("NullMoth xkpi: module %s failed to load: %d\n", md->name ? md->name : "?", err);
    }
}

/* ---- start/stop of the whole layer -------------------------------------------------------------------------------- */

/* FreeBSD's tsc_freq (machine/clock.h): drivers scale clocks by it */
uint64_t tsc_freq;
extern int sysctlbyname(const char *, void *, size_t *, void *, size_t);

int nmx_kernel_init(void)
{
    clock_timebase_info(&nmx_tb);
    nmx_boot_abs = mach_absolute_time();
    nmx_lck_grp = lck_grp_alloc_init("com.nullmoth.xkpi", LCK_GRP_ATTR_NULL);
    if (nmx_lck_grp == NULL)
        return ENOMEM;
    nmx_thread_lock = lck_spin_alloc_init(nmx_lck_grp, LCK_ATTR_NULL);
    if (nmx_thread_lock == NULL)
        return ENOMEM;
    mp_ncpus = nmx_xnu_ncpus();
    mp_ncores = mp_ncpus;
    mp_maxid = mp_ncpus - 1;
    CPU_ZERO(&all_cpus);
    for (int i = 0; i < mp_ncpus && i < CPU_SETSIZE; i++)
        CPU_SET(i, &all_cpus);
    nmx_cpu_identify();
    size_t len = sizeof(tsc_freq);
    if (sysctlbyname("machdep.tsc.frequency", &tsc_freq, &len, NULL, 0) != 0)
        tsc_freq = 0;
    return 0;
}

void nmx_kernel_fini(void)
{
    if (nmx_thread_lock) {
        for (unsigned b = 0; b < NMX_THREAD_BUCKETS; b++) {
            struct nmx_thread_rec *r = nmx_threads[b];
            while (r) { struct nmx_thread_rec *n = r->next; IOFree(r, sizeof(*r)); r = n; }
            nmx_threads[b] = NULL;
        }
        for (unsigned b = 0; b < NMX_PROC_BUCKETS; b++) {
            struct nmx_proc_rec *r = nmx_procs[b];
            while (r) { struct nmx_proc_rec *n = r->next; IOFree(r, sizeof(*r)); r = n; }
            nmx_procs[b] = NULL;
        }
        lck_spin_free(nmx_thread_lock, nmx_lck_grp);
        nmx_thread_lock = NULL;
    }
    if (nmx_lck_grp) {
        lck_grp_free(nmx_lck_grp);
        nmx_lck_grp = NULL;
    }
}

/* CPU count from the kernel's own sysctl (kexts may call sysctlbyname) */
int nmx_xnu_ncpus(void)
{
    extern int sysctlbyname(const char *, void *, size_t *, void *, size_t);
    int n = 0;
    size_t len = sizeof(n);
    if (sysctlbyname("hw.logicalcpu_max", &n, &len, NULL, 0) != 0 || n <= 0)
        n = 1;
    return n > MAXCPU ? MAXCPU : n;
}

int nmx_thread_records(void)
{
    return nmx_thread_count;
}

/* ---- signals: LinuxKPI reads pending signals right after PROC_LOCK, which refreshes them from xnu ------------------ */

extern int proc_issignal(int pid, sigset_t mask);
extern void proc_signal(int pid, int signum);

void nmx_proc_refresh_signals(struct proc *p)
{
    if (p == NULL || p->nmx_xnu == NULL) {
        if (p)
            p->p_siglist = 0;
        return;
    }
    sigset_t pending = 0;
    if (proc_issignal(p->p_pid, sigmask(SIGKILL)))
        pending |= sigmask(SIGKILL);
    /* any other deliverable signal: one representative bit is all signal_pending() needs */
    if (proc_issignal(p->p_pid, ~sigmask(SIGKILL)))
        pending |= sigmask(SIGINT);
    p->p_siglist = pending;
}

void tdsignal(struct thread *td, int sig)
{
    if (td && td->td_proc && td->td_proc->nmx_xnu)
        proc_signal(td->td_proc->p_pid, sig);
}

/* ---- helpers for LinuxKPI's per-thread state (the linux_current fork) ---------------------------------------------- */

/* xnu's proc_find/proc_exiting/proc_rele (declared by xnu with its own proc_t); the pointer is only a key here */

int linux_alloc_current_noop(struct thread *td, int flags)
{
    return 0;
}

/* the port's record for a running process, or NULL; looked up through xnu so a stale pid finds nothing */
struct proc *nmx_proc_find(pid_t pid)
{
    void *xp = (void *)proc_find(pid);
    if (xp == NULL)
        return NULL;
    struct proc *p = nmx_proc_for(xp);
    proc_rele((proc_t)xp);
    return p;
}

bool nmx_proc_exiting(struct proc *p)
{
    if (p == NULL || p->nmx_xnu == NULL)
        return false;
    void *xp = (void *)proc_find(p->p_pid);
    if (xp == NULL)
        return true;            /* already gone */
    bool exiting = proc_exiting((proc_t)xp) != 0;
    proc_rele((proc_t)xp);
    return exiting;
}

/* any thread of the process that has Linux state; with 'get' its task_struct gains a reference under the lock */
struct task_struct *nmx_proc_any_task(struct proc *p, bool get)
{
    extern void nmx_task_get(struct task_struct *ts);
    struct task_struct *ts = NULL;
    lck_spin_lock(nmx_thread_lock);
    for (unsigned b = 0; b < NMX_THREAD_BUCKETS && ts == NULL; b++)
        for (struct nmx_thread_rec *r = nmx_threads[b]; r; r = r->next)
            if (r->td.td_proc == p && r->td.td_lkpi_task != NULL) {
                ts = r->td.td_lkpi_task;
                if (get)
                    nmx_task_get(ts);
                break;
            }
    lck_spin_unlock(nmx_thread_lock);
    return ts;
}

/* detaches every thread's Linux state and releases it outside the registry lock (unload) */
void nmx_threads_release_all(void (*release)(struct thread *))
{
    for (;;) {
        struct thread *td = NULL;
        lck_spin_lock(nmx_thread_lock);
        for (unsigned b = 0; b < NMX_THREAD_BUCKETS && td == NULL; b++)
            for (struct nmx_thread_rec *r = nmx_threads[b]; r; r = r->next)
                if (r->td.td_lkpi_task != NULL) { td = &r->td; break; }
        lck_spin_unlock(nmx_thread_lock);
        if (td == NULL)
            return;
        release(td);            /* clears td_lkpi_task */
    }
}
