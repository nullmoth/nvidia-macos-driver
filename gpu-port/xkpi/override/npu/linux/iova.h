/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Linux IOVA allocator, for XDNA's IOMMU-domain mode. macOS gives a kext no IOMMU domain, so that mode is never entered
 * here (XDNA runs on carveout memory instead); these declarations let the file that holds both modes compile, and the
 * allocator answers "nothing" if it were ever reached.
 */
#ifndef _NMX_LINUX_IOVA_H_
#define _NMX_LINUX_IOVA_H_
#include <linux/types.h>
struct iova {
	unsigned long pfn_hi;
	unsigned long pfn_lo;
};
struct iova_domain {
	unsigned long granule;
	unsigned long start_pfn;
};
static inline unsigned long iova_shift(struct iova_domain *d) { return __builtin_ctzl(d->granule ? d->granule : 4096); }
static inline size_t iova_align(struct iova_domain *d, size_t size) { size_t g = d->granule ? d->granule : 4096; return (size + g - 1) & ~(g - 1); }
static inline dma_addr_t iova_dma_addr(struct iova_domain *d, struct iova *i) { return (dma_addr_t)i->pfn_lo << iova_shift(d); }
static inline unsigned long iova_pfn(struct iova_domain *d, dma_addr_t a) { return a >> iova_shift(d); }
static inline struct iova *alloc_iova(struct iova_domain *d, unsigned long size, unsigned long limit_pfn, bool size_aligned) { return NULL; }
static inline void free_iova(struct iova_domain *d, unsigned long pfn) { }
static inline void init_iova_domain(struct iova_domain *d, unsigned long granule, unsigned long start_pfn) { d->granule = granule; d->start_pfn = start_pfn; }
static inline void put_iova_domain(struct iova_domain *d) { }
static inline int iova_cache_get(void) { return 0; }
static inline void iova_cache_put(void) { }
static inline void __free_iova(struct iova_domain *d, struct iova *i) { }
#endif
