/* SPDX-License-Identifier: MIT */
/*
 * sysinfo(2) on macOS: total memory from hw.memsize, free memory from the VM statistics (free + inactive pages), in
 * bytes (mem_unit 1). Other fields are zero. The NPU driver sizes its default memory limits from these.
 */
#ifndef NMX_COMPAT_SYS_SYSINFO_H
#define NMX_COMPAT_SYS_SYSINFO_H
#include <string.h>
#include <stdint.h>
#include <sys/sysctl.h>
#include <mach/mach.h>
#include <linux/sysinfo.h>
static inline int sysinfo(struct sysinfo *info)
{
	uint64_t mem = 0;
	size_t len = sizeof(mem);
	vm_statistics64_data_t vm;
	mach_msg_type_number_t n = HOST_VM_INFO64_COUNT;

	memset(info, 0, sizeof(*info));
	if (sysctlbyname("hw.memsize", &mem, &len, NULL, 0) != 0)
		return -1;
	info->totalram = (unsigned long)mem;
	if (host_statistics64(mach_host_self(), HOST_VM_INFO64, (host_info64_t)&vm, &n) == KERN_SUCCESS)
		info->freeram = (unsigned long)(vm.free_count + vm.inactive_count) * (unsigned long)vm_page_size;
	info->mem_unit = 1;
	return 0;
}
#endif
