/* FreeBSD priv(9) on xnu: a privileged operation requires the caller to be the superuser (kauth). */
#pragma once
struct thread;
struct ucred;
int priv_check(struct thread *td, int priv);
int priv_check_cred(struct ucred *cred, int priv);
#define PRIV_DRIVER 14
#define PRIV_SCHED_SETPRIORITY 203
#define PRIV_IO 15
#define PRIV_KMEM_READ 16
#define PRIV_KMEM_WRITE 17
