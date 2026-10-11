/* SPDX-License-Identifier: MIT */
/*
 * Force-included into Intel's NPU user driver on macOS: the Linux userland names it uses that macOS spells differently.
 */
#ifndef NMX_UMD_COMPAT_H
#define NMX_UMD_COMPAT_H
#include <stdint.h>
#include <pthread.h>
#include <sys/types.h>
/* the kernel's annotation for user pointers, which Linux's exported uapi headers strip */
#ifndef __user
#define __user
#endif
/* Linux's thread id; macOS gives a 64-bit one per thread */
static inline pid_t gettid(void)
{
	uint64_t tid = 0;
	pthread_threadid_np(NULL, &tid);
	return (pid_t)tid;
}
/* struct stat's access time: st_atimespec on macOS */
#define st_atim st_atimespec
/* Linux's "file descriptor in bad state" errno, as the kernel driver (NMXNPU) returns it: the same number */
#ifndef EBADFD
#define EBADFD 77
#endif
#endif
