/* SPDX-License-Identifier: Apache-2.0 */
/*
 * Force-included into AMD's XRT on macOS: the Linux userland interfaces XRT's POSIX code uses that macOS lacks.
 */
#ifndef NMX_XRT_COMPAT_H
#define NMX_XRT_COMPAT_H
#ifdef __APPLE__
#include <pthread.h>
#include <string.h>
/* glibc gives byte-order helpers (be32toh...) through its common headers; here they come from compat endian.h */
#include <endian.h>
/* Linux errno for a rejected key (xclbin signature check); macOS has no such errno, and nothing here returns it */
/* Linux mmap flag asking for the pages to stay resident; device buffers on macOS are wired by the kernel anyway */
#ifndef MAP_LOCKED
#define MAP_LOCKED 0
#endif
#ifndef EKEYREJECTED
#define EKEYREJECTED 129
#endif
/*
 * CPU affinity: macOS offers no hard affinity (thread affinity tags are hints the scheduler may ignore), so a
 * configured Runtime.cpu_affinity is accepted and has no effect; XRT's threads run where the scheduler puts them.
 */
typedef struct { unsigned long bits[16]; } cpu_set_t;
#define CPU_ZERO(s) memset((s), 0, sizeof(*(s)))
#define CPU_SET(c, s) ((s)->bits[(c) / (8 * sizeof(unsigned long))] |= 1UL << ((c) % (8 * sizeof(unsigned long))))
#define CPU_ISSET(c, s) (((s)->bits[(c) / (8 * sizeof(unsigned long))] >> ((c) % (8 * sizeof(unsigned long)))) & 1UL)
static inline int pthread_setaffinity_np(pthread_t t, size_t n, const cpu_set_t *s) { (void)t; (void)n; (void)s; return 0; }
#endif
#endif
