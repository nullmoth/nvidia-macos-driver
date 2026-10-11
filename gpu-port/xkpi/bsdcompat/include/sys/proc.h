/*
 * FreeBSD <sys/proc.h> on xnu. LinuxKPI keeps per-thread Linux state (struct task_struct) hanging off FreeBSD's
 * struct thread. Here struct thread is a small record the port keeps for each xnu thread that runs LinuxKPI code,
 * created on first use and found by thread_t (see src/nmx_thread.c).
 */
#ifndef NMXKPI_SYS_PROC_H
#define NMXKPI_SYS_PROC_H
#include_next <sys/proc.h>
#include <sys/types.h>
#include <sys/priority.h>
#include <sys/rtprio.h>
#include <sys/pcpu.h>
#include <kern/thread.h>
struct task_struct;
/* FreeBSD's process record as LinuxKPI and DRM read it; one per xnu process that uses the driver, kept by the port.
 * xnu's own struct proc is opaque to kexts, so the name is free; xnu calls take the nmx_xnu pointer instead. */
struct proc {
    pid_t p_pid;
    char p_comm[MAXCOMLEN + 1];
    void *p_vmspace;
    sigset_t p_siglist;             /* pending signals, refreshed from xnu by PROC_LOCK */
    void *p_lkpi_mm;                /* the process's Linux mm_struct, shared by its threads (linux_current fork) */
    void *nmx_xnu;                  /* the xnu proc_t */
};
struct thread {
    thread_t td_xnu;                    /* the xnu thread */
    struct proc *td_proc;               /* its BSD process (the kernel's for kernel threads) */
    struct task_struct *td_lkpi_task;   /* LinuxKPI's per-thread state */
    int td_tid;
    u_char td_priority;
    u_char td_base_pri;
    int td_pinned;
    int td_critnest;
    int td_intr_nesting_level;      /* refreshed from ml_at_interrupt_context() by curthread */
    int td_flags;
    int td_owepreempt;              /* FreeBSD's deferred-preemption flag; xnu preempts on its own, so always 0 */
    int td_inhibitors;
    struct { int code; register_t args[8]; } td_sa;   /* FreeBSD syscall arguments: never filled here */
    struct file *td_fpop;           /* the file whose operation runs now (LinuxKPI's OPW) */
    struct nmx_open *td_nmx_open;   /* the device-node open an entry point runs for (devfs_get_cdevpriv) */
    struct ucred *td_ucred;
    sigset_t td_siglist;            /* signals pending for this thread (filled with the process's, see PROC_LOCK) */
    sigset_t td_sigmask;
    int td_pflags;
    char td_name[32];
    void *td_wchan;
    const char *td_wmesg;
};
#define TDF_KTH_SUSP 0x00000100
#define TDP_KTHREAD 0x00200000
#define TDP_NOFAULTING 0x00000080
#define TDP_ITHREAD 0x00000400
#define PID_MAX 99999
#define TD_IS_RUNNING(td) 1
#define TD_ON_LOCK(td) 0
#define thread_lock(td) do { } while (0)
#define thread_unlock(td) do { } while (0)
/* LinuxKPI takes PROC_LOCK right before it reads a process's pending signals, so that is where they are refreshed */
void nmx_proc_refresh_signals(struct proc *p);
#define PROC_LOCK(p) nmx_proc_refresh_signals(p)
#define PROC_UNLOCK(p) do { } while (0)
#define PROC_LOCK_ASSERT(p, w) do { } while (0)
#define FIRST_THREAD_IN_PROC(p) curthread
#define TD_IS_SLEEPING(td) 0
#define TDA_SCHED 1
#define td_ast_pending(td, a) 0
/* yield the CPU to other runnable threads */
void sched_relinquish(struct thread *td);
int pause(const char *wmesg, int timo);
int pause_sbt(const char *wmesg, sbintime_t sbt, sbintime_t pr, int flags);
#define pause_sig(wmesg, timo) pause((wmesg), (timo))
/* xnu declares wakeup(void *); FreeBSD callers pass const channels, so the FreeBSD names map to the port's own. */
void nmx_wakeup(const void *chan);
void nmx_wakeup_one(const void *chan);
#define wakeup(chan) nmx_wakeup(chan)
#define wakeup_one(chan) nmx_wakeup_one(chan)
#define wakeup_any(chan) nmx_wakeup_one(chan)
int tsleep(const void *chan, int pri, const char *wmesg, int timo);
#define tsleep_sbt(chan, pri, wmesg, sbt, pr, flags) tsleep((chan), (pri), (wmesg), (int)((sbt) / tick_sbt))
pid_t nmx_proc_pid(struct proc *p);
struct thread *tdfind(lwpid_t tid, pid_t pid);
/* LinuxKPI installs this hook at load; it creates a thread's Linux task_struct on first use */
extern int (*lkpi_alloc_current)(struct thread *, int);
int linux_alloc_current_noop(struct thread *td, int flags);
/* port helpers for the linux_current fork */
struct proc *nmx_proc_find(pid_t pid);
bool nmx_proc_exiting(struct proc *p);
struct task_struct *nmx_proc_any_task(struct proc *p, bool get);
void nmx_threads_release_all(void (*release)(struct thread *));
extern void (*nmx_thread_release_hook)(struct thread *td);
extern struct proc *nmx_kernproc;
struct proc *nmx_curproc(void);
#define curproc nmx_curproc()
extern struct proc *pageproc;   /* FreeBSD's page daemon: no such process here, so never equal to curproc */
#define SIGPENDING(td) nmx_signal_pending(td)
int nmx_signal_pending(struct thread *td);
#endif
