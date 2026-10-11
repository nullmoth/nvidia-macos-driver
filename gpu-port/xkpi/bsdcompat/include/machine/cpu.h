/* FreeBSD <machine/cpu.h> on xnu (x86_64) */
#pragma once
#include <stdint.h>
#define cpu_spinwait() __builtin_ia32_pause()
static inline uint64_t get_cyclecount(void) { return __builtin_ia32_rdtsc(); }
#define cpu_getmaxphyaddr() (1ULL << 46)
