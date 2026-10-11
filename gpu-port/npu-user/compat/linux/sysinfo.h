/* SPDX-License-Identifier: MIT */
/* Linux's struct sysinfo; macOS answers the fields the NPU driver reads (memory sizes) through sys/sysinfo.h. */
#ifndef NMX_COMPAT_LINUX_SYSINFO_H
#define NMX_COMPAT_LINUX_SYSINFO_H
struct sysinfo {
	long uptime;
	unsigned long loads[3];
	unsigned long totalram;
	unsigned long freeram;
	unsigned long sharedram;
	unsigned long bufferram;
	unsigned long totalswap;
	unsigned long freeswap;
	unsigned short procs;
	unsigned long totalhigh;
	unsigned long freehigh;
	unsigned int mem_unit;
};
#endif
