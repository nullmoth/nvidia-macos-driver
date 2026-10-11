/* SPDX-License-Identifier: Apache-2.0 */
/*
 * Linux's extra mmap flags (huge pages), which XRT's Alveo DMA path asks for. macOS has no MAP_HUGETLB: the flags are
 * zero, so such a mapping is an ordinary one (the NPU never takes that path).
 */
#ifndef NMX_COMPAT_ASM_MMAN_H
#define NMX_COMPAT_ASM_MMAN_H
#include <sys/mman.h>
#ifndef MAP_HUGETLB
#define MAP_HUGETLB 0
#endif
#ifndef MAP_HUGE_SHIFT
#define MAP_HUGE_SHIFT 26
#endif
#endif
