/* FreeBSD <machine/md_var.h> on xnu: CPU identification read once at kext start (cpuid). */
#pragma once
#include <stdint.h>
extern unsigned cpu_feature, cpu_feature2, cpu_stdext_feature, cpu_stdext_feature2, cpu_high, cpu_id, cpu_vendor_id;
extern unsigned amd_feature, amd_feature2;
extern char cpu_vendor[];
extern int cpu_clflush_line_size;
void nmx_cpu_identify(void);
#include <machine/specialreg.h>
#define CPUID_CLFSH_UNUSED 0
#include <machine/cputypes.h>
