/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Linux IOMMU API for the NPU drivers. macOS gives a kext no IOMMU domain, no groups and no shared virtual addressing
 * (PASID), so every operation reports that: no domain, no SVA, mapping refused. AMD's XDNA then runs on carveout memory
 * (physical addresses, the AMD IOMMU off as macOS on PCs requires). A superset of LinuxKPI's iommu.h, which this replaces.
 */
#ifndef _NMX_NPU_LINUX_IOMMU_H_
#define _NMX_NPU_LINUX_IOMMU_H_
#define _LINUXKPI_LINUX_IOMMU_H_	/* LinuxKPI's version must not also be included */
#include <linux/device.h>
#include <linux/err.h>
#include <linux/types.h>

#define __IOMMU_DOMAIN_PAGING	(1U << 0)
#define __IOMMU_DOMAIN_DMA_API	(1U << 1)
#define __IOMMU_DOMAIN_PT	(1U << 2)
#define __IOMMU_DOMAIN_DMA_FQ	(1U << 3)
#define IOMMU_DOMAIN_BLOCKED	(0U)
#define IOMMU_DOMAIN_IDENTITY	(__IOMMU_DOMAIN_PT)
#define IOMMU_DOMAIN_UNMANAGED	(__IOMMU_DOMAIN_PAGING)
#define IOMMU_DOMAIN_DMA	(__IOMMU_DOMAIN_PAGING | __IOMMU_DOMAIN_DMA_API)
#define IOMMU_DOMAIN_DMA_FQ	(__IOMMU_DOMAIN_PAGING | __IOMMU_DOMAIN_DMA_API | __IOMMU_DOMAIN_DMA_FQ)

#define IOMMU_READ	(1 << 0)
#define IOMMU_WRITE	(1 << 1)
#define IOMMU_PASID_INVALID	((u32)-1)
#define IOMMU_HWPT_ALLOC_PASID	(1 << 1)

struct iommu_domain_geometry {
	dma_addr_t aperture_start;
	dma_addr_t aperture_end;
	bool force_aperture;
};

struct iommu_domain {
	unsigned int type;
	unsigned long pgsize_bitmap;
	struct iommu_domain_geometry geometry;
};

struct iommu_group;
struct iommu_sva;
struct mm_struct;
struct sg_table;

static inline struct iommu_domain *iommu_get_domain_for_dev(struct device *dev) { return NULL; }
static inline struct iommu_group *iommu_group_get(struct device *dev) { return NULL; }
static inline void iommu_group_put(struct iommu_group *group) { }
static inline struct iommu_domain *iommu_paging_domain_alloc_flags(struct device *dev, unsigned int flags) { return ERR_PTR(-ENODEV); }
static inline void iommu_domain_free(struct iommu_domain *domain) { }
static inline int iommu_attach_group(struct iommu_domain *domain, struct iommu_group *group) { return -ENODEV; }
static inline void iommu_detach_group(struct iommu_domain *domain, struct iommu_group *group) { }
static inline int iommu_map(struct iommu_domain *domain, unsigned long iova, phys_addr_t paddr, size_t size, int prot, gfp_t gfp) { return -ENODEV; }
static inline size_t iommu_unmap(struct iommu_domain *domain, unsigned long iova, size_t size) { return 0; }
static inline ssize_t iommu_map_sgtable(struct iommu_domain *domain, unsigned long iova, struct sg_table *sgt, int prot) { return -ENODEV; }
static inline struct iommu_sva *iommu_sva_bind_device(struct device *dev, struct mm_struct *mm) { return ERR_PTR(-ENODEV); }
static inline void iommu_sva_unbind_device(struct iommu_sva *handle) { }
static inline u32 iommu_sva_get_pasid(struct iommu_sva *handle) { return IOMMU_PASID_INVALID; }
#endif
