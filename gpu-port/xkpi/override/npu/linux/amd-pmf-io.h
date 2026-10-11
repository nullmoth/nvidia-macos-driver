/* SPDX-License-Identifier: GPL-2.0 */
/* AMD PMF (platform management) interface the XDNA driver can read NPU metrics from. macOS has no PMF driver, so the
 * metrics are unavailable; XDNA treats that as "no counters". */
#ifndef _NMX_LINUX_AMD_PMF_IO_H_
#define _NMX_LINUX_AMD_PMF_IO_H_
#include <linux/types.h>
#include <linux/errno.h>
struct amd_pmf_npu_metrics {
	u16 npuclk_freq;
	u16 npu_busy[8];
	u16 npu_power;
	u16 mpnpuclk_freq;
	u16 npu_reads;
	u16 npu_writes;
	u16 npu_temp;
};
static inline int amd_pmf_get_npu_data(struct amd_pmf_npu_metrics *info) { return -ENODEV; }
#endif
