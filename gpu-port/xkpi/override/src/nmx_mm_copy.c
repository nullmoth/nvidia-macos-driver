/*
 * copy_to_user / copy_from_user against another process's mm, for driver worker threads (the port's form of
 * Linux's kthread_use_mm + copy_*_user; see nmx_mm.cpp). Same return convention as copy_*_user: the number of bytes
 * NOT copied, so 0 is success.
 */
#include <linux/types.h>
#include <linux/mm_types.h>
#include <linux/uaccess.h>

#include "nmx_xnu.h"
#include "nmx_port.h"

unsigned long nmx_mm_copy_to_user(struct mm_struct *mm, void __user *to, const void *from, unsigned long n)
{
	return nmx_xnu_mm_copy(mm, (uint64_t)(uintptr_t)to, __DECONST(void *, from), n, 1) == 0 ? 0 : n;
}

unsigned long nmx_mm_copy_from_user(struct mm_struct *mm, void *to, const void __user *from, unsigned long n)
{
	return nmx_xnu_mm_copy(mm, (uint64_t)(uintptr_t)from, to, n, 0) == 0 ? 0 : n;
}
