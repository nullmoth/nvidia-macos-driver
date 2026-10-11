# fork_npu.py <linux-6.14> <npu-src>: the NPU sources NMXNPU builds (GPL-2.0): Intel's ivpu, the DRM accel core and the
# GEM shmem helper from Linux 6.14, copied fresh each build with the few macOS forms applied. Anchors are exact and
# asserted; files are rewritten atomically.
import os, shutil, sys
src, dst = sys.argv[1], sys.argv[2]
if os.path.exists(dst):
    shutil.rmtree(dst)
shutil.copytree(os.path.join(src, "drivers/accel/ivpu"), os.path.join(dst, "ivpu"))
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
