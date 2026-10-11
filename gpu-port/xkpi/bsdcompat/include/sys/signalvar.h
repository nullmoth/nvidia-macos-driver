/*
 * FreeBSD signal-set helpers on xnu's sigset_t (a 32-bit mask, bit n-1 for signal n). Pending signals come from xnu
 * (proc_issignal) when LinuxKPI locks the process; see nmx_proc_refresh_signals.
 */
#pragma once
#include <sys/signal.h>
#define SIGISEMPTY(set) ((set) == 0)
#define SIGNOTEMPTY(set) ((set) != 0)
#define SIGISMEMBER(set, signo) (((set) & sigmask(signo)) != 0)
#define SIGADDSET(set, signo) ((set) |= sigmask(signo))
#define SIGDELSET(set, signo) ((set) &= ~sigmask(signo))
#define SIGEMPTYSET(set) ((set) = 0)
#define SIGFILLSET(set) ((set) = ~(sigset_t)0)
#define SIGSETOR(set1, set2) ((set1) |= (set2))
#define SIGSETAND(set1, set2) ((set1) &= (set2))
#define SIGSETNAND(set1, set2) ((set1) &= ~(set2))
struct thread;
void tdsignal(struct thread *td, int sig);
