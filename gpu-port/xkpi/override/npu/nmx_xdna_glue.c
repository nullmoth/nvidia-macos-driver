// SPDX-License-Identifier: GPL-2.0
/*
 * AMD XDNA on macOS: the carveout memory it runs from. Without IOMMU shared virtual addressing (macOS has no AMD IOMMU
 * driver, and macOS on PCs runs with the IOMMU off), the NPU reaches memory by physical address, so XDNA allocates its
 * buffers from one physically contiguous region. Linux sets that region through debugfs; here it is allocated at probe,
 * the largest of 512, 256, 128 or 64 MB that the system can give in one piece, and kept for the device's life.
 */
#include <linux/kernel.h>
#include <linux/printk.h>

#include "amdxdna_pci_drv.h"
#include "amdxdna_cbuf.h"
#include "nmx_xnu.h"

void nmx_xdna_carveout_setup(struct amdxdna_dev *xdna)
{
	static const uint64_t sizes[] = { 512ULL << 20, 256ULL << 20, 128ULL << 20, 64ULL << 20 };
	void *handle = NULL;

	for (unsigned i = 0; i < ARRAY_SIZE(sizes); i++) {
		void *kva = nmx_xnu_mem_alloc(sizes[i], 1, ~0ULL, 2 << 20, &handle);
		if (kva == NULL)
			continue;
		uint64_t phys = nmx_xnu_kva_to_phys(kva);
		if (phys == 0 || amdxdna_carveout_init(xdna, phys, sizes[i]) != 0) {
			nmx_xnu_mem_free(handle);
			pr_err("amdxdna: carveout of %llu MB could not be set up\n", (unsigned long long)(sizes[i] >> 20));
			return;
		}
		pr_info("amdxdna: %llu MB carveout at 0x%llx for the NPU\n", (unsigned long long)(sizes[i] >> 20),
			(unsigned long long)phys);
		return;		/* held for the device's life, as Linux's debugfs-configured carveout is */
	}
	pr_err("amdxdna: no physically contiguous memory for the NPU carveout - buffers cannot be allocated\n");
}
