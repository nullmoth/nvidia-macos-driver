# fork_npu.py <linux-6.14> <npu-src>: the NPU sources NMXNPU builds (GPL-2.0): Intel's ivpu, the DRM accel core and the
# GEM shmem helper from Linux 6.14, copied fresh each build with the few macOS forms applied. Anchors are exact and
# asserted; files are rewritten atomically.
import os, shutil, sys
src, dst = sys.argv[1], sys.argv[2]
if os.path.exists(dst):
    shutil.rmtree(dst)
# Intel's ivpu from the newer tree when given (argv[3]): Linux 6.18+ boots the current linux-firmware NPU image on
# Arrow Lake (boot API 3.37) where 6.14's driver (expects 3.26) timed out waiting for the ready message
IVPU_SRC = sys.argv[3] if len(sys.argv) > 3 and os.environ.get("NMX_IVPU_614") != "1" else src
shutil.copytree(os.path.join(IVPU_SRC, "drivers/accel/ivpu"), os.path.join(dst, "ivpu"))
# its UAPI header, found as <uapi/drm/ivpu_accel.h> through cc-npu.sh's -I npu-src/uapi ahead of the 6.14 copy
os.makedirs(os.path.join(dst, "uapi/uapi/drm"), exist_ok=True)
shutil.copy2(os.path.join(IVPU_SRC, "include/uapi/drm/ivpu_accel.h"), os.path.join(dst, "uapi/uapi/drm"))
shutil.copy2(os.path.join(src, "drivers/accel/drm_accel.c"), dst)
shutil.copy2(os.path.join(src, "drivers/gpu/drm/drm_gem_shmem_helper.c"), dst)
# AMD XDNA from the newer Intel/AMD tree (argv[3], Linux 7.3): only it can run without IOMMU SVA (carveout memory)
if len(sys.argv) > 3:
    shutil.copytree(os.path.join(sys.argv[3], "drivers/accel/amdxdna"), os.path.join(dst, "amdxdna"))
    os.makedirs(os.path.join(dst, "uapi/drm"), exist_ok=True)
    shutil.copy2(os.path.join(sys.argv[3], "include/uapi/drm/amdxdna_accel.h"), os.path.join(dst, "uapi/drm"))

def edit(name, pairs):
    p = os.path.join(dst, name)
    t = open(p).read()
    for old, new in pairs:
        assert t.count(old) == 1, (name, old[:60], t.count(old))
        t = t.replace(old, new)
    open(p + ".tmp", "w").write(t)
    os.replace(p + ".tmp", p)

# LinuxKPI's class devnode callback takes a non-const device; drm-kmod's files carry no address_space for the inode
edit("drm_accel.c", [
    ("static char *accel_devnode(const struct device *dev, umode_t *mode)", "static char *accel_devnode(struct device *dev, umode_t *mode)"),
    ("\tfilp->f_mapping = dev->anon_inode->i_mapping;\n", ""),
])
# shmem objects on FreeBSD LinuxKPI: pages already come from the kernel's wired pool (no gfp mask on a mapping), the
# mmap handle is the GEM object (as drm-kmod's TTM passes the BO), and truncation acts on the shmem vm_object
edit("drm_gem_shmem_helper.c", [
    ("\t\tmapping_set_gfp_mask(obj->filp->f_mapping, GFP_HIGHUSER |\n\t\t\t\t     __GFP_RETRY_MAYFAIL | __GFP_NOWARN);\n", "\t\t;\n"),
    ("\tdrm_vma_node_unmap(&obj->vma_node, dev->anon_inode->i_mapping);\n", "\tdrm_vma_node_unmap(&obj->vma_node, obj);\n"),
    ("\tshmem_truncate_range(file_inode(obj->filp), 0, (loff_t)-1);\n\n\tinvalidate_mapping_pages(file_inode(obj->filp)->i_mapping, 0, (loff_t)-1);\n",
     "\tshmem_truncate_range(obj->filp->f_shmem, 0, (loff_t)-1);\n"),
])
# ivpu: BARs through LinuxKPI's PCI mapping (no struct resource array on its pci_dev); IOPCIFamily owns D3 timing
edit("ivpu/ivpu_drv.c", [
    ("\tstruct resource *bar0 = &pdev->resource[0];\n\tstruct resource *bar4 = &pdev->resource[4];\n", ""),
    ("\tivpu_dbg(vdev, MISC, \"Mapping BAR0 (RegV) %pR\\n\", bar0);\n\tvdev->regv = devm_ioremap_resource(vdev->drm.dev, bar0);\n",
     "\tvdev->regv = pci_iomap(pdev, 0, 0) ?: ERR_PTR(-ENOMEM);\n"),
    ("\tpdev->d3hot_delay = 0;\n", ""),
    ("\tivpu_dbg(vdev, MISC, \"Mapping BAR4 (RegB) %pR\\n\", bar4);\n\tvdev->regb = devm_ioremap_resource(vdev->drm.dev, bar4);\n",
     "\tvdev->regb = pci_iomap(pdev, 4, 0) ?: ERR_PTR(-ENOMEM);\n"),
    # the port's module registration (SYSINIT through LinuxKPI), as drm-kmod's drivers do on FreeBSD
    ("module_pci_driver(ivpu_pci_driver);\n",
     "static int ivpu_module_init(void) { return pci_register_driver(&ivpu_pci_driver); }\n"
     "static void ivpu_module_exit(void) { pci_unregister_driver(&ivpu_pci_driver); }\n"
     "LKPI_DRIVER_MODULE(intel_vpu, ivpu_module_init, ivpu_module_exit);\n"
     "MODULE_DEPEND(intel_vpu, drmn, 2, 2, 2);\n"
     "MODULE_DEPEND(intel_vpu, linuxkpi, 1, 1, 1);\n"),
    # Linux's one-argument MODULE_VERSION; the BSD macro of that name takes (module, version)
    ("MODULE_VERSION(DRIVER_VERSION_STR);\n", ""),
])
# NPU firmware: linux-firmware now ships only the _v1 files (boot API 3.37, JSM 3.41 - the same major 3 this driver
# speaks; newer minors are accepted). Later Intel trees look for _v1 first and fall back to _v0.0; so does this one.
edit("ivpu/ivpu_fw.c", [
    ('\t{ IVPU_HW_IP_37XX, "intel/vpu/vpu_37xx_v0.0.bin" },\n', '\t{ IVPU_HW_IP_37XX, "intel/vpu/vpu_37xx_v1.bin" },\n\t{ IVPU_HW_IP_37XX, "intel/vpu/vpu_37xx_v0.0.bin" },\n'),
    ('\t{ IVPU_HW_IP_40XX, "intel/vpu/vpu_40xx_v0.0.bin" },\n', '\t{ IVPU_HW_IP_40XX, "intel/vpu/vpu_40xx_v1.bin" },\n\t{ IVPU_HW_IP_40XX, "intel/vpu/vpu_40xx_v0.0.bin" },\n'),
    ('\t{ IVPU_HW_IP_50XX, "intel/vpu/vpu_50xx_v0.0.bin" },\n', '\t{ IVPU_HW_IP_50XX, "intel/vpu/vpu_50xx_v1.bin" },\n\t{ IVPU_HW_IP_50XX, "intel/vpu/vpu_50xx_v0.0.bin" },\n'),
])
# First Arrow Lake attach: the SHAVE firmware buffer found no room in context 0 (-ENOSPC) although the range
# is 2 GB (SZ_2G was -2 GB; see override/include/linux/sizes.h). A failed placement prints the range, the size and
# every hole of the context.
edit("ivpu/ivpu_mmu_context.c", [
    ("""	ret = drm_mm_insert_node_in_range(&ctx->mm, node, size, IVPU_MMU_PAGE_SIZE, 0,
					  range->start, range->end, DRM_MM_INSERT_BEST);
unlock:
""", """	ret = drm_mm_insert_node_in_range(&ctx->mm, node, size, IVPU_MMU_PAGE_SIZE, 0,
					  range->start, range->end, DRM_MM_INSERT_BEST);
	if (ret) {
		struct drm_mm_node *h;
		u64 hs, he;
		int n = 0;

		pr_err("NullMoth ivpu: ctx %u: no room for 0x%llx bytes in [0x%llx, 0x%llx) (%d); mm [0x%llx, +0x%llx)\\n",
		       ctx->id, (unsigned long long)size, (unsigned long long)range->start,
		       (unsigned long long)range->end, ret, (unsigned long long)ctx->mm.head_node.start + ctx->mm.head_node.size,
		       (unsigned long long)-ctx->mm.head_node.size);
		drm_mm_for_each_hole(h, &ctx->mm, hs, he) {
			pr_err("NullMoth ivpu:   hole [0x%llx, 0x%llx) size 0x%llx\\n", (unsigned long long)hs,
			       (unsigned long long)he, (unsigned long long)(he - hs));
			if (++n == 24)
				break;
		}
		pr_err("NullMoth ivpu:   %d holes listed\\n", n);
	}
unlock:
"""),
])
# A failed firmware boot (the NPU never sent its ready message, 10-11) prints the firmware's own log buffers.
edit("ivpu/ivpu_drv.c", [
    ("""err_diagnose_failure:
	ivpu_hw_diagnose_failure(vdev);
	ivpu_mmu_evtq_dump(vdev);
""", """err_diagnose_failure:
	ivpu_hw_diagnose_failure(vdev);
	ivpu_mmu_evtq_dump(vdev);
	{
		struct drm_printer p = drm_info_printer(vdev->drm.dev);

		ivpu_fw_log_print(vdev, false, &p);
	}
"""),
])
# NMX_IVPU_TRACE=1 also checks, after a failed firmware boot, that the NPU sees what the CPU wrote: for the boot
# parameters page and the firmware entry page, the NPU page-table entry against the page's physical address, and the
# CPU's write-combined view against an uncached read of the same RAM.
if os.environ.get("NMX_IVPU_TRACE") == "1":
    p = os.path.join(dst, "ivpu/ivpu_mmu_context.c")
    t = open(p).read() + r'''
#include "ivpu_gem.h"
void nmx_ivpu_check(struct ivpu_device *vdev, struct ivpu_bo *bo, u64 off)
{
	struct ivpu_mmu_context *ctx = &vdev->gctx;
	u64 va = bo->vpu_addr + off, e = 0, a, b, c, d;
	int pgd = FIELD_GET(IVPU_MMU_PGD_INDEX_MASK, va), pud = FIELD_GET(IVPU_MMU_PUD_INDEX_MASK, va);
	int pmd = FIELD_GET(IVPU_MMU_PMD_INDEX_MASK, va), pte = FIELD_GET(IVPU_MMU_PTE_INDEX_MASK, va);
	struct page *pg = bo->base.pages ? bo->base.pages[off >> PAGE_SHIFT] : NULL;
	phys_addr_t pa = pg ? page_to_phys(pg) : 0;
	u8 *cpu = (u8 *)ivpu_bo_vaddr(bo);
	void __iomem *uc;
	int i, diff = 0;

	if (ctx->pgtable.pte_ptrs[pgd] && ctx->pgtable.pte_ptrs[pgd][pud] && ctx->pgtable.pte_ptrs[pgd][pud][pmd])
		e = ctx->pgtable.pte_ptrs[pgd][pud][pmd][pte];
	if (!cpu || !pa) {
		pr_err("NullMoth ivpu check: va 0x%llx pte 0x%llx: no CPU map or page\n", va, e);
		return;
	}
	cpu += off;
	uc = ioremap(pa, PAGE_SIZE);
	if (!uc) {
		pr_err("NullMoth ivpu check: va 0x%llx: ioremap failed\n", va);
		return;
	}
	for (i = 0; i < PAGE_SIZE; i += 8)
		if (readq(uc + i) != *(u64 *)(cpu + i))
			diff++;
	a = *(u64 *)cpu; b = *(u64 *)(cpu + 8); c = readq(uc); d = readq(uc + 8);
	pr_err("NullMoth ivpu check: va 0x%llx pte 0x%llx (page 0x%llx) phys 0x%llx | cpu %016llx %016llx ram %016llx %016llx | %d/512 qwords differ\n",
	       va, e, e & ~(u64)0xfff & ~BIT_ULL(52), (u64)pa, a, b, c, d, diff);
	iounmap(uc);
}
'''
    open(p + ".tmp", "w").write(t); os.replace(p + ".tmp", p)
    edit("ivpu/ivpu_drv.c", [("""		ivpu_fw_log_print(vdev, false, &p);
	}
""", """		ivpu_fw_log_print(vdev, false, &p);
	}
	{
		extern void nmx_ivpu_check(struct ivpu_device *, struct ivpu_bo *, u64);

		nmx_ivpu_check(vdev, vdev->fw->mem, 0);
		nmx_ivpu_check(vdev, vdev->fw->mem, 0x3000);
		nmx_ivpu_check(vdev, vdev->fw->mem, 0x10000);
	}
""")])
# NMX_IVPU_TRACE=1 (bring-up builds only): Intel's debug trace for firmware boot, power, MMU, IPC and misc goes to the
# kernel log (LinuxKPI's dev_dbg prints nothing).
if os.environ.get("NMX_IVPU_TRACE") == "1":
    edit("ivpu/ivpu_drv.h", [("\t\tdev_dbg((vdev)->drm.dev, \"[%s] \" fmt, #type, ##args);",
                              "\t\tdev_info((vdev)->drm.dev, \"[%s] \" fmt, #type, ##args);")])
    edit("ivpu/ivpu_drv.c", [("int ivpu_dbg_mask;\n",
                              "int ivpu_dbg_mask = IVPU_DBG_FW_BOOT | IVPU_DBG_PM | IVPU_DBG_MMU | IVPU_DBG_IPC | IVPU_DBG_MISC" + (" | IVPU_DBG_REG" if os.environ.get("NMX_IVPU_REG") == "1" else "") + ";\n")])
if os.path.isdir(os.path.join(dst, "amdxdna")):
    edit("amdxdna/aie2_pci.h", [('#include "amdxdna_mailbox.h"\n', '#include "amdxdna_mailbox.h"\n#include "nmx_xdna_sched.h"\n')])
    edit("amdxdna/aie2_ctx.c", [
        ("\tret = drm_sched_init(sched, &args);\n", "\tret = nmx_drm_sched_init(sched, &args);\n"),
        # LinuxKPI's down_interruptible is "sema_wait(...), 0" without parentheses: the assignment needs them
        ("\tret = down_interruptible(&hwctx->priv->job_sem);\n", "\tret = (down_interruptible(&hwctx->priv->job_sem));\n"),
        ("\tret = drm_sched_job_init(&job->base, &hwctx->priv->entity, 1, hwctx,\n",
         "\tret = nmx_drm_sched_job_init(&job->base, &hwctx->priv->entity, 1, hwctx,\n"),
    ])
    edit("amdxdna/amdxdna_pci_drv.c", [
        # drm-kmod keeps the opener's pid as a pid_t
        ("\tclient->pid = pid_nr(rcu_access_pointer(filp->pid));\n", "\tclient->pid = filp->pid;\n"),
        # no SR-IOV under macOS (LinuxKPI's pci_driver has no sriov_configure)
        ("\t.sriov_configure = amdxdna_sriov_configure,\n", ""),
        # Linux sets the carveout through debugfs; macOS allocates it at probe (nmx_xdna_glue.c)
        ("\tamdxdna_debugfs_init(xdna);\n\treturn 0;\n",
         "\tamdxdna_debugfs_init(xdna);\n\tnmx_xdna_carveout_setup(xdna);\n\treturn 0;\n"),
        ("module_pci_driver(amdxdna_pci_driver);\n",
         "static int amdxdna_module_init(void) { return pci_register_driver(&amdxdna_pci_driver); }\n"
         "static void amdxdna_module_exit(void) { pci_unregister_driver(&amdxdna_pci_driver); }\n"
         "LKPI_DRIVER_MODULE(amdxdna, amdxdna_module_init, amdxdna_module_exit);\n"
         "MODULE_DEPEND(amdxdna, drmn, 2, 2, 2);\n"
         "MODULE_DEPEND(amdxdna, linuxkpi, 1, 1, 1);\n"),
    ])
print("npu fork applied")
