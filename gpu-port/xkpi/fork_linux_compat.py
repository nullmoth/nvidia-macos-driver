# fork_linux_compat.py <override/src/linux_compat.c>: macOS forms of the FreeBSD-specific parts of LinuxKPI's
# linux_compat.c. Everything else (devices, classes, kobjects, timers, completions, files, ioctls) stays LinuxKPI's.
import pathlib, sys
p = pathlib.Path(sys.argv[1]); s = p.read_text()

def once(old):
    assert s.count(old) == 1, (old[:70], s.count(old))
    return s.index(old)

def replace_region(start_marker, end_marker_after, new):
    """replaces from start_marker up to and including the function that begins at end_marker_after"""
    global s
    i = once(start_marker)
    j = once(end_marker_after)
    k = s.index("\n}\n", j) + 3
    s = s[:i] + new.strip("\n") + "\n" + s[k:]

def replace_once(old, new):
    global s
    once(old)
    s = s.replace(old, new)

# networking is not part of the GPU driver build on macOS
replace_once("#include <linux/netdevice.h>\n", "#include <linux/notifier.h>\n")

# FreeBSD VM pager for device mmap: user mappings are made by the device-node glue's mmap ioctl instead
replace_region("static int\nlinux_cdev_pager_fault(", "zap_vma_ptes(struct vm_area_struct *vma, unsigned long address,\n", r"""
/*
 * macOS port: GPU memory is mapped into a process whole, by the device-node glue's mmap ioctl (an IOKit mapping
 * in the caller), and the buffer stays resident while mapped. There is no FreeBSD VM pager behind a mapping, so
 * there are no page table entries to zap here.
 */
int
zap_vma_ptes(struct vm_area_struct *vma, unsigned long address,
    unsigned long size)
{
	return (0);
}
""")

# kqueue on a Linux file: xnu does not let kexts attach kqueue filters to device nodes
replace_region("static void\nlinux_file_kqfilter_detach(struct knote *kn)\n", "linux_file_kqfilter(struct file *file, struct knote *kn)\n", r"""
/* macOS port: no kqueue filters on device nodes for kexts; readiness is reported through select (nmx_devfs.cpp) */
static void
linux_file_kqfilter_poll(struct linux_file *filp, int kqflags)
{
}

static int
linux_file_kqfilter(struct file *file, struct knote *kn)
{
	return (EINVAL);
}
""")

replace_region("static int\nlinux_file_mmap_single(struct file *fp, const struct file_operations *fop,\n",
               "linux_file_mmap_single(struct file *fp, const struct file_operations *fop,\n", r"""
/* macOS port: see zap_vma_ptes above; mmap goes through the device-node glue */
static int
linux_file_mmap_single(struct file *fp, const struct file_operations *fop,
    vm_ooffset_t *offset, vm_size_t size, struct vm_object **object,
    int nprot, bool is_shared, struct thread *td)
{
	return (ENODEV);
}
""")

replace_region("static int\nlinux_file_mmap_sub(struct thread *td, vm_size_t objsize, vm_prot_t prot,\n",
               "linux_file_mmap(struct file *fp, vm_map_t map, vm_offset_t *addr, vm_size_t size,\n", r"""
static int
linux_file_mmap(struct file *fp, vm_map_t map, vm_offset_t *addr, vm_size_t size,
    vm_prot_t prot, vm_prot_t cap_maxprot, int flags, vm_ooffset_t foff,
    struct thread *td)
{
	return (ENODEV);
}
""")

replace_region("static void\nlinux_handle_ifnet_link_event(", "unregister_inetaddr_notifier(struct notifier_block *nb)\n", r"""
/* macOS port: no network devices are driven through LinuxKPI here; network notifiers never fire */
int
register_netdevice_notifier(struct notifier_block *nb)
{
	return (0);
}

int
register_inetaddr_notifier(struct notifier_block *nb)
{
	return (0);
}

int
unregister_netdevice_notifier(struct notifier_block *nb)
{
	return (0);
}

int
unregister_inetaddr_notifier(struct notifier_block *nb)
{
	return (0);
}
""")

# the FreeBSD vnode and cdev records must be declared before <linux/cdev.h> renames "cdev" to "linux_cdev"
replace_once("#include <linux/cdev.h>\n", "#include <sys/vnode.h>\n#include <sys/conf.h>\n#include <linux/cdev.h>\n")

# init and uninit still create and destroy the vma lock that the removed pager code used
replace_once("int\nzap_vma_ptes(struct vm_area_struct *vma, unsigned long address,\n",
             "static struct rwlock linux_vma_lock;\n\nint\nzap_vma_ptes(struct vm_area_struct *vma, unsigned long address,\n")

# list_sort: the comparator takes const list heads (LinuxKPI's own header, LINUXKPI_VERSION >= 51300)
replace_once("\tint (*cmp)(void *, struct list_head *, struct list_head *);\n",
             "\tint (*cmp)(void *, const struct list_head *, const struct list_head *);\n")
replace_once("list_sort(void *priv, struct list_head *head, int (*cmp)(void *priv,\n    struct list_head *a, struct list_head *b))\n",
             "list_sort(void *priv, struct list_head *head, int (*cmp)(void *priv,\n    const struct list_head *a, const struct list_head *b))\n")

# fstat of a DRM node is answered by xnu's devfs; the port's file never reaches a stat or kinfo request
replace_region("static int\nlinux_file_stat(struct file *fp, struct stat *sb, struct ucred *active_cred)\n",
               "linux_file_fill_kinfo(struct file *fp, struct kinfo_file *kif,\n", r"""
/* macOS port: fstat and process file listings of a DRM node are answered by xnu's devfs, never through here */
static int
linux_file_stat(struct file *fp, struct stat *sb, struct ucred *active_cred)
{
	return (EOPNOTSUPP);
}

static int
linux_file_fill_kinfo(struct file *fp, struct kinfo_file *kif,
    struct filedesc *fdp)
{
	return (EOPNOTSUPP);
}
""")

# kcmp(2)'s ordering of two pointers (FreeBSD kern_descrip.c kcmp_cmp)
replace_once("\treturn (kcmp_cmp((uintptr_t)filp1->f_cdev, (uintptr_t)filp2->f_cdev));\n",
             "\tif ((uintptr_t)filp1->f_cdev == (uintptr_t)filp2->f_cdev)\n\t\treturn (0);\n"
             "\treturn ((uintptr_t)filp1->f_cdev < (uintptr_t)filp2->f_cdev ? 1 : 2);\n")

p.write_text(s)
print("linux_compat.c fork applied")
