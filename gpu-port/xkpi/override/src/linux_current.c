/*-
 * Copyright (c) 2017 Hans Petter Selasky
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice unmodified, this list of conditions, and the following
 *    disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#include <sys/cdefs.h>
#ifdef __amd64__
#define	DEV_APIC
#elif defined(__i386__)
#include "opt_apic.h"
#endif

#include <linux/compat.h>
#include <linux/completion.h>
#include <linux/mm.h>
#include <linux/kthread.h>
#include <linux/moduleparam.h>

#include <sys/kernel.h>
#include <sys/eventhandler.h>
#include <sys/malloc.h>
#include <sys/sysctl.h>
#include <vm/uma.h>

#ifdef DEV_APIC
extern u_int first_msi_irq, num_msi_irqs;
#endif


static uma_zone_t linux_current_zone;
static uma_zone_t linux_mm_zone;

/* macOS port: the mm -> Mach task record (nmx_mm.cpp), declared here because this file sees no IOKit headers */
void nmx_xnu_mm_note_task(void *mm);
void nmx_xnu_mm_forget(void *mm);

/*
 * macOS port: one mm_struct per process, kept on the port's process record (FreeBSD finds it by walking the
 * process's threads, which xnu does not let a kext do).
 */
static struct mm_struct *
find_other_mm(struct proc *p)
{
	struct mm_struct *mm;

	mm = p->p_lkpi_mm;
	if (mm != NULL && atomic_inc_not_zero(&mm->mm_users))
		return (mm);
	return (NULL);
}

int
linux_alloc_current(struct thread *td, int flags)
{
	struct proc *proc;
	struct task_struct *ts;
	struct mm_struct *mm, *mm_other;

	MPASS(td->td_lkpi_task == NULL);

	if ((td->td_pflags & TDP_ITHREAD) != 0 || !THREAD_CAN_SLEEP()) {
		flags &= ~M_WAITOK;
		flags |= M_NOWAIT | M_USE_RESERVE;
	}

	ts = uma_zalloc(linux_current_zone, flags | M_ZERO);
	if (ts == NULL)
		return (ENOMEM);
	mm = NULL;

	/* setup new task structure */
	atomic_set(&ts->kthread_flags, 0);
	ts->task_thread = td;
	ts->comm = td->td_name;
	ts->pid = td->td_tid;
	ts->group_leader = ts;
	atomic_set(&ts->usage, 1);
	atomic_set(&ts->state, TASK_RUNNING);
	init_completion(&ts->parked);
	init_completion(&ts->exited);

	proc = td->td_proc;

	PROC_LOCK(proc);
	mm_other = find_other_mm(proc);

	/* use allocated mm_struct as a fallback */
	if (mm_other == NULL) {
		PROC_UNLOCK(proc);
		mm = uma_zalloc(linux_mm_zone, flags | M_ZERO);
		if (mm == NULL) {
			uma_zfree(linux_current_zone, ts);
			return (ENOMEM);
		}

		PROC_LOCK(proc);
		mm_other = find_other_mm(proc);
		if (mm_other == NULL) {
			/* setup new mm_struct */
			init_rwsem(&mm->mmap_sem);
			atomic_set(&mm->mm_count, 1);
			atomic_set(&mm->mm_users, 1);
			/* set mm_struct pointer */
			ts->mm = mm;
			/* macOS port: later threads of this process share it */
			proc->p_lkpi_mm = mm;
			/* macOS port: remember the Mach task, for driver workers that copy to this process */
			nmx_xnu_mm_note_task(mm);
			/* clear pointer to not free memory */
			mm = NULL;
		} else {
			ts->mm = mm_other;
		}
	} else {
		ts->mm = mm_other;
	}

	/* store pointer to task struct */
	td->td_lkpi_task = ts;
	PROC_UNLOCK(proc);

	/* free mm_struct pointer, if any */
	uma_zfree(linux_mm_zone, mm);

	return (0);
}

struct mm_struct *
linux_get_task_mm(struct task_struct *task)
{
	struct mm_struct *mm;

	mm = task->mm;
	if (mm != NULL) {
		atomic_inc(&mm->mm_users);
		return (mm);
	}
	return (NULL);
}

void
linux_mm_dtor(struct mm_struct *mm)
{
	/* macOS port: forget the process's shared mm before it goes */
	struct proc *p = curthread->td_proc;

	if (p != NULL && p->p_lkpi_mm == mm)
		p->p_lkpi_mm = NULL;
	nmx_xnu_mm_forget(mm);
	uma_zfree(linux_mm_zone, mm);
}

void
linux_free_current(struct task_struct *ts)
{
	mmput(ts->mm);
	uma_zfree(linux_current_zone, ts);
}

static void
linuxkpi_thread_dtor_hook(struct thread *td)
{
	struct task_struct *ts;

	ts = td->td_lkpi_task;
	if (ts == NULL)
		return;

	td->td_lkpi_task = NULL;
	put_task_struct(ts);
}

/* macOS port: threads and processes are found through the port's records (nmx_kernel.c) */
static struct task_struct *
linux_get_pid_task_int(pid_t pid, const bool do_get)
{
	struct thread *td;
	struct task_struct *ts;
	struct proc *p;

	if (pid > PID_MAX) {
		td = tdfind(pid, -1);
		if (td == NULL)
			return (NULL);
		ts = td->td_lkpi_task;
		if (do_get && ts != NULL)
			get_task_struct(ts);
		return (ts);
	}
	p = nmx_proc_find(pid);
	if (p == NULL)
		return (NULL);
	return (nmx_proc_any_task(p, do_get));
}

struct task_struct *
linux_pid_task(pid_t pid)
{
	return (linux_get_pid_task_int(pid, false));
}

struct task_struct *
linux_get_pid_task(pid_t pid)
{
	return (linux_get_pid_task_int(pid, true));
}

/* macOS port: whether the task's process is exiting comes from xnu (proc_exiting) */
bool
linux_task_exiting(struct task_struct *task)
{
	struct thread *td;
	struct proc *p;

	td = tdfind(task->pid, -1);
	p = (td != NULL) ? td->td_proc : nmx_proc_find(task->pid);
	return (p != NULL && nmx_proc_exiting(p));
}

/*
 * macOS port: task and mm structures come from plain zones (xnu has no reserve to pre-fill), and FreeBSD's
 * thread-destructor event is the port's release hook, run when a thread's record is dropped (end of a system call,
 * exit of a kernel thread the port started).
 */
static void
linux_current_init(void *arg __unused)
{
	linux_current_zone = uma_zcreate("lkpicurr",
	    sizeof(struct task_struct), NULL, NULL, NULL, NULL,
	    UMA_ALIGN_PTR, 0);
	linux_mm_zone = uma_zcreate("lkpimm",
	    sizeof(struct mm_struct), NULL, NULL, NULL, NULL,
	    UMA_ALIGN_PTR, 0);

	atomic_thread_fence_seq_cst();

	nmx_thread_release_hook = linuxkpi_thread_dtor_hook;
	lkpi_alloc_current = linux_alloc_current;
}
SYSINIT(linux_current, SI_SUB_EVENTHANDLER, SI_ORDER_LAST, linux_current_init,
    NULL);

static void
linux_current_uninit(void *arg __unused)
{
	lkpi_alloc_current = linux_alloc_current_noop;
	nmx_thread_release_hook = NULL;

	atomic_thread_fence_seq_cst();

	nmx_threads_release_all(linuxkpi_thread_dtor_hook);

	uma_zdestroy(linux_current_zone);
	uma_zdestroy(linux_mm_zone);
}
SYSUNINIT(linux_current, SI_SUB_EVENTHANDLER, SI_ORDER_LAST,
    linux_current_uninit, NULL);

/* macOS port: reference a task from the port's registry walk (nmx_proc_any_task) */
void nmx_task_get(struct task_struct *ts);
void
nmx_task_get(struct task_struct *ts)
{
	get_task_struct(ts);
}
