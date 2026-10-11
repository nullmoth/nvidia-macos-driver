/* FreeBSD <sys/sched.h> on xnu: preemption control and priorities as LinuxKPI uses them. */
#pragma once
#include <sys/types.h>
struct thread;
/* xnu has its own critical_enter/critical_exit symbols; the FreeBSD forms map to the port's (preemption off/on) */
void nmx_critical_enter(void);
void nmx_critical_exit(void);
#define critical_enter() nmx_critical_enter()
#define critical_exit() nmx_critical_exit()
int nmx_in_critical(void);
void sched_pin(void);
void sched_unpin(void);
void sched_prio(struct thread *td, u_char prio);
void sched_add(struct thread *td, int flags);
int sched_is_bound(struct thread *td);
void sched_bind(struct thread *td, int cpu);
void sched_unbind(struct thread *td);
void kern_yield(int prio);
/* FreeBSD's voluntary context switch: here, a yield of the CPU to any other runnable thread */
void mi_switch(int flags);
#define SW_VOL 0x0100
#define SW_INVOL 0x0200
#define SWT_RELINQUISH 23
void maybe_yield(void);
#define SRQ_BORING 0x0000
#define PRI_USER -2
#define PRI_UNCHANGED -1
#define THREAD_NO_SLEEPING() do { } while (0)
#define THREAD_SLEEPING_OK() do { } while (0)
#define THREAD_CAN_SLEEP() (!nmx_in_critical())
