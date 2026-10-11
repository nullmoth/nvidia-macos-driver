/* FreeBSD <machine/clock.h>: the TSC frequency in Hz (read from the kernel at start, nmx_kernel.c) */
#pragma once
#include <stdint.h>
extern uint64_t tsc_freq;
