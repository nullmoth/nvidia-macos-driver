/*
 * LinuxKPI's sizes.h computes SZ_2G..SZ_32G in int arithmetic ((1024 * 1024 * 1024 * 2) overflows to -2 GB, SZ_4G to 0).
 * Linux defines them as unsigned long long. The first Arrow Lake NPU attach failed on exactly this: ivpu's
 * SHAVE range [0x180000000, +SZ_2G) came out as [0x180000000, 0x100000000), end below start, so its firmware buffer
 * found no room (-ENOSPC). Every range of 2 GB or more in amdgpu, i915, xe and the NPU drivers had the same error.
 * Here they take Linux's values and type.
 */
#ifndef _NMX_LINUX_SIZES_H_
#define _NMX_LINUX_SIZES_H_
#include_next <linux/sizes.h>

#undef SZ_2G
#undef SZ_4G
#undef SZ_8G
#undef SZ_16G
#undef SZ_32G
#undef SZ_64G
#undef SZ_128G
#undef SZ_256G
#define SZ_2G	0x80000000ULL
#define SZ_4G	0x100000000ULL
#define SZ_8G	0x200000000ULL
#define SZ_16G	0x400000000ULL
#define SZ_32G	0x800000000ULL
#define SZ_64G	0x1000000000ULL
#define SZ_128G	0x2000000000ULL
#define SZ_256G	0x4000000000ULL

#endif
