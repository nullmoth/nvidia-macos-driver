/* FreeBSD <sys/vmmeter.h> on xnu: memory totals, as LinuxKPI's si_meminfo reports them. */
#pragma once
#include <stdint.h>
uint64_t nmx_physmem_pages(void);
uint64_t nmx_free_pages(void);
#define vm_free_count() ((unsigned long)nmx_free_pages())
#define physmem ((unsigned long)nmx_physmem_pages())
