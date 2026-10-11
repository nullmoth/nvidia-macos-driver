/* SPDX-License-Identifier: Apache-2.0 */
/*
 * Linux's <linux/const.h> for userland on macOS. AMD's XDNA ioctl header includes it; these are the constant helpers
 * Linux UAPI headers use.
 */
#ifndef NMX_COMPAT_LINUX_CONST_H
#define NMX_COMPAT_LINUX_CONST_H
#define _AC(X, Y) (X##Y)
#define _UL(x) (_AC(x, UL))
#define _ULL(x) (_AC(x, ULL))
#define _BITUL(x) (_UL(1) << (x))
#define _BITULL(x) (_ULL(1) << (x))
#define __ALIGN_KERNEL_MASK(x, mask) (((x) + (mask)) & ~(mask))
#define __ALIGN_KERNEL(x, a) __ALIGN_KERNEL_MASK(x, (__typeof__(x))(a) - 1)
#define __KERNEL_DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#endif
