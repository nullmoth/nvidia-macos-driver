/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Linux HMM for the NPU drivers. XDNA faults user buffers in with it only when the process is bound to the NPU through
 * PASID, which macOS cannot offer; so a range fault reports "not supported" and XDNA's carveout buffers are used instead.
 */
#ifndef _NMX_NPU_LINUX_HMM_H_
#define _NMX_NPU_LINUX_HMM_H_
#include <linux/types.h>
#include <linux/errno.h>
#include <linux/mm.h>
#include <linux/mmu_notifier.h>

enum hmm_pfn_flags {
	HMM_PFN_VALID = 1UL << 63,
	HMM_PFN_WRITE = 1UL << 62,
	HMM_PFN_ERROR = 1UL << 61,
	HMM_PFN_REQ_FAULT = HMM_PFN_VALID,
	HMM_PFN_REQ_WRITE = HMM_PFN_WRITE,
	HMM_PFN_FLAGS = HMM_PFN_VALID | HMM_PFN_WRITE | HMM_PFN_ERROR,
};

struct hmm_range {
	struct mmu_interval_notifier *notifier;
	unsigned long notifier_seq;
	unsigned long start;
	unsigned long end;
	unsigned long *hmm_pfns;
	unsigned long default_flags;
	unsigned long pfn_flags_mask;
	void *dev_private_owner;
};

#define HMM_RANGE_DEFAULT_TIMEOUT 1000

static inline int hmm_range_fault(struct hmm_range *range) { return -EOPNOTSUPP; }
static inline struct page *hmm_pfn_to_page(unsigned long hmm_pfn) { return pfn_to_page(hmm_pfn & ~HMM_PFN_FLAGS); }
#define hmm_range_fault_unlocked_timeout(...) (-EOPNOTSUPP)
#endif
