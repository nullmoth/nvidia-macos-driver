/* SPDX-License-Identifier: MIT */
/* <linux/kernel.h> as Linux exports it to userland: only the sysinfo struct header and alignment helpers. */
#ifndef NMX_COMPAT_LINUX_KERNEL_H
#define NMX_COMPAT_LINUX_KERNEL_H
#include <linux/sysinfo.h>
#ifndef __ALIGN_KERNEL
#define __ALIGN_KERNEL(x, a) __ALIGN_KERNEL_MASK(x, (__typeof__(x))(a) - 1)
#define __ALIGN_KERNEL_MASK(x, mask) (((x) + (mask)) & ~(mask))
#endif
#endif
