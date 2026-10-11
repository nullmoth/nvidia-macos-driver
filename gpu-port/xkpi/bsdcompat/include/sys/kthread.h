/* FreeBSD kthread(9) on xnu: kernel threads are xnu threads started with kernel_thread_start. */
#pragma once
#include <sys/types.h>
struct proc;
struct thread;
int kthread_add(void (*func)(void *), void *arg, struct proc *p, struct thread **newtdp, int flags, int pages, const char *fmt, ...) __printflike(7, 8);
int kproc_create(void (*func)(void *), void *arg, struct proc **newpp, int flags, int pages, const char *fmt, ...) __printflike(6, 7);
int kproc_kthread_add(void (*func)(void *), void *arg, struct proc **procptr, struct thread **tdptr, int flags, int pages, const char *procname, const char *fmt, ...) __printflike(8, 9);
void kthread_exit(void) __dead2;
int kthread_suspend(struct thread *td, int timo);
void kthread_suspend_check(void);
int kthread_resume(struct thread *td);
extern struct proc *proc0;
#define RFSTOPPED 0x20000
#define RFHIGHPID 0x40000
