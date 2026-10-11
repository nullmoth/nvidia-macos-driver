/*
 * The DRM pieces the macOS port provides itself, written for this port:
 *   - what drm-kmod's drm_os_freebsd.c gives FreeBSD (malloc type, sysctl root, device aliases, VRAM page range);
 *   - the GEM-on-TTM helpers (drm-kmod's drm_gem_ttm_helper.c is GPL and is not used), plus the pin hook that keeps
 *     a TTM buffer in place while it is mapped into a process (see override/src/nmx_linux_cdev.c);
 *   - writeback connectors, which the port does not create, and debugfs, which macOS does not have.
 */
#include <sys/param.h>
#include <sys/systm.h>
#include <sys/malloc.h>
#include <sys/sysctl.h>
#include <sys/priv.h>

#include <linux/kernel.h>
#include <linux/err.h>
#include <linux/debugfs.h>

#include <drm/drm.h>
#include <drm/drm_device.h>
#include <drm/drm_file.h>
#include <linux/pci.h>
#include <linux/cdev.h>
#undef cdev                     /* LinuxKPI renames struct cdev; the node below is the BSD one (as drm_os_freebsd.c) */
#include <sys/conf.h>
#include <drm/drm_ioctl.h>
#include <drm/drm_gem.h>
#ifndef NMX_NO_TTM
#include <drm/drm_gem_ttm_helper.h>
#endif
#include <drm/drm_writeback.h>
#include <drm/drm_utils.h>
#include <drm/display/drm_hdcp_helper.h>
#ifndef NMX_NO_TTM
#include <drm/ttm/ttm_bo.h>
#endif
#include <drm_os_freebsd.h>

#include "nmx_vma.h"
#include "nmx_drm_uapi.h"

MALLOC_DEFINE(DRM_MEM_DRIVER, "drm_driver", "DRM driver data");

/* hw.dri in the port's own sysctl tree (drm_sysctl_freebsd.c hangs its nodes here) */
struct sysctl_oid_list sysctl__hw_dri_children;

/*
 * FreeBSD registers the VRAM aperture as fictitious pages so its pager can find a vm_page for any pfn there. The
 * port's user mappings take physical addresses directly (nmx_xnu_map_user), so nothing needs those pages.
 */
int register_fictitious_range(struct drm_device *ddev, vm_paddr_t start, vm_paddr_t end)
{
    return 0;
}

void unregister_fictitious_range(struct drm_device *ddev, vm_paddr_t start, vm_paddr_t end)
{
}

/*
 * Each DRM and accel minor's device node, under the name Linux gives it: /dev/dri/card%d, /dev/dri/renderD%d,
 * /dev/accel/accel%d. drm-kmod makes the /dev/dri names here too (as aliases of 256 nodes created up front); the port
 * creates the one node itself when the minor registers (linux_compat.c records the stub file operations of the two
 * majors instead of making nodes). The PCI identity FreeBSD publishes as hw.dri.N is asked of the node instead
 * (DRM_IOCTL_GET_PCIINFO).
 *
 * Modes follow what Linux distributions give these nodes: render and accel nodes are meant for any process (Mesa's
 * Vulkan drivers, Level Zero, XRT run inside apps and WindowServer), so 0666; the primary node can change what the
 * screens show, so root only.
 */
#ifndef ACCEL_MAJOR
#define ACCEL_MAJOR 261         /* include/drm/drm_accel.h */
#endif
const struct file_operations *nmx_chrdev_fops(unsigned int major);   /* linux_compat.c */

int drm_dev_alias(struct device *ldev, struct drm_minor *minor, const char *minor_str)
{
	const struct file_operations *fops;
	struct make_dev_args args;
	struct linux_cdev *lc;
	struct cdev *node;
	const char *dir;
	char name[32];
	unsigned int major;
	int mode, err;

	switch (minor->type) {
	case DRM_MINOR_ACCEL:
		major = ACCEL_MAJOR, dir = "accel", mode = 0666;
		break;
	case DRM_MINOR_RENDER:
		major = DRM_MAJOR, dir = "dri", mode = 0666;
		break;
	default:
		major = DRM_MAJOR, dir = "dri", mode = 0600;
		break;
	}
	fops = nmx_chrdev_fops(major);
	if (fops == NULL)
		return (-ENXIO);
	snprintf(name, sizeof(name), minor_str, minor->index);

	lc = cdev_alloc();
	if (lc == NULL)
		return (-ENOMEM);
	lc->ops = fops;
	lc->dev = MKDEV(major, minor->index);       /* iminor() of an open is minor->index, as on Linux */
	kobject_set_name(&lc->kobj, "%s", major == ACCEL_MAJOR ? "accel" : "drm");

	make_dev_args_init(&args);
	args.mda_devsw = &linuxcdevsw;
	args.mda_uid = UID_ROOT;
	args.mda_gid = GID_WHEEL;
	args.mda_mode = mode;
	args.mda_si_drv1 = lc;
	err = make_dev_s(&args, &node, "%s/%s", dir, name);
	if (err != 0) {
		printf("NullMoth drm: cannot create /dev/%s/%s (%d)\n", dir, name, err);
		kobject_put(&lc->kobj);
		return (-err);
	}
	lc->cdev = node;
	minor->bsd_device = node;
	minor->bsd_alias = node;
	return (0);
}

void drm_dev_alias_free(struct drm_minor *minor)
{
	struct cdev *node = minor->bsd_device;

	if (node == NULL)
		return;
	minor->bsd_device = NULL;
	minor->bsd_alias = NULL;
	/* the linux_cdev's release destroys the node (draining opens in flight) and frees it */
	kobject_put(&((struct linux_cdev *)node->si_drv1)->kobj);
}

/* ---- GEM objects backed by TTM buffers ------------------------------------------------------------------------------ */

/* NMX_NO_TTM: NMXNPU builds this file without TTM (its drivers use GEM shmem objects only) */
#ifndef NMX_NO_TTM
static struct ttm_buffer_object *bo_of(struct drm_gem_object *gem)
{
    return container_of(gem, struct ttm_buffer_object, base);
}

int drm_gem_ttm_vmap(struct drm_gem_object *gem, struct iosys_map *map)
{
    return ttm_bo_vmap(bo_of(gem), map);
}

void drm_gem_ttm_vunmap(struct drm_gem_object *gem, struct iosys_map *map)
{
    ttm_bo_vunmap(bo_of(gem), map);
}

/* the user-mapping pin hook: a pinned buffer is never evicted or moved, so the pages mapped stay its pages */
static int nmx_ttm_pin(void *arg, bool on)
{
    struct ttm_buffer_object *bo = arg;
    int r = ttm_bo_reserve(bo, on /* interruptible only when pinning; unpinning must happen */, false, NULL);
    if (r)
        return r;
    if (on)
        ttm_bo_pin(bo);
    else
        ttm_bo_unpin(bo);
    ttm_bo_unreserve(bo);
    return 0;
}

/*
 * drm_gem_mmap_obj took a GEM reference for the mapping; ttm_bo_mmap_obj takes the buffer's own, which the TTM
 * vm close callback drops at unmap, so the GEM reference is returned here.
 */
int drm_gem_ttm_mmap(struct drm_gem_object *gem, struct vm_area_struct *vma)
{
    struct ttm_buffer_object *bo = bo_of(gem);
    int r = ttm_bo_mmap_obj(vma, bo);
    if (r < 0)
        return r;
    nmx_vma_set_pin(vma, nmx_ttm_pin, bo);
    drm_gem_object_put(gem);
    return 0;
}

#endif

/* ---- writeback connectors: none are created on macOS (amdgpu logs the refusal and continues) ------------------------ */

int drm_writeback_connector_init(struct drm_device *dev, struct drm_writeback_connector *wb_connector,
                                 const struct drm_connector_funcs *con_funcs,
                                 const struct drm_encoder_helper_funcs *enc_helper_funcs,
                                 const u32 *formats, int n_formats, u32 possible_crtcs)
{
    return -ENODEV;
}

void drm_writeback_queue_job(struct drm_writeback_connector *wb_connector, struct drm_connector_state *conn_state)
{
}

void drm_writeback_signal_completion(struct drm_writeback_connector *wb_connector, int status)
{
}

/*
 * Panel quirk table for minimum backlight (drm_panel_backlight_quirks.c is GPL and is not used): no panel has a
 * quirk, which amdgpu reads as "keep the panel's own minimum" (a negative answer).
 */
int drm_get_panel_min_brightness_quirk(const struct drm_edid *edid)
{
    return -ENODATA;
}

/*
 * HDCP (drm_hdcp_helper.c is GPL and is not used): the content-protection property is not offered, so no client can
 * ask for protected output and the driver never has a state change to report.
 */
int drm_connector_attach_content_protection_property(struct drm_connector *connector, bool hdcp_content_type)
{
    return 0;
}

void drm_hdcp_update_content_protection(struct drm_connector *connector, u64 val)
{
}

/* the revocation list check of an HDCP handshake, which never starts without the property: nothing is revoked */
int drm_hdcp_check_ksvs_revoked(struct drm_device *dev, u8 *ksvs, u32 ksv_count)
{
    return 0;
}


int drm_writeback_set_fb(struct drm_connector_state *cs, struct drm_framebuffer *fb)
{
    return fb == NULL ? 0 : -EINVAL;
}

int drm_writeback_prepare_job(struct drm_writeback_job *j)
{
    return 0;
}

void drm_writeback_cleanup_job(struct drm_writeback_job *j)
{
}

struct dma_fence *drm_writeback_get_out_fence(struct drm_writeback_connector *connector)
{
    return NULL;
}

/* ---- debugfs: macOS has none; Linux's own answer when it is compiled out is ERR_PTR(-ENODEV) ---------------------- */

struct dentry *debugfs_create_dir(const char *name, struct dentry *parent)
{
    return ERR_PTR(-ENODEV);
}

struct dentry *debugfs_create_file(const char *name, umode_t mode, struct dentry *parent, void *data,
                                   const struct file_operations *fops)
{
    return ERR_PTR(-ENODEV);
}

void debugfs_create_bool(const char *name, umode_t mode, struct dentry *parent, bool *value)
{
}

void debugfs_create_atomic_t(const char *name, umode_t mode, struct dentry *parent, atomic_t *value)
{
}

struct dentry *debugfs_create_file_unsafe(const char *name, umode_t mode, struct dentry *parent, void *data,
                                          const struct file_operations *fops)
{
    return ERR_PTR(-ENODEV);
}

void debugfs_remove(struct dentry *dentry)
{
}

/*
 * DRM_IOCTL_GET_PCIINFO, the BSD way for libdrm to learn which PCI device a DRM node is (Linux reads sysfs instead).
 * The port's libdrm uses it to enumerate GPUs (drmGetDevices2), which is how Mesa's RADV and ANV find their device;
 * drm-kmod leaves it to FreeBSD's own drm code, so the port answers it at the node. Returns -1 for any other ioctl.
 */
int nmx_drm_node_ioctl(struct linux_file *filp, unsigned long cmd, void *data)
{
	struct drm_file *df;
	struct drm_pciinfo *pi = data;
	struct pci_dev *pdev;

	if (cmd != DRM_IOCTL_GET_PCIINFO && cmd != NMX_DRM_IOCTL_SYSFS_GET && cmd != NMX_DRM_IOCTL_SYSFS_PUT)
		return -1;
	df = filp != NULL ? filp->private_data : NULL;
	if (cmd == NMX_DRM_IOCTL_SYSFS_GET || cmd == NMX_DRM_IOCTL_SYSFS_PUT) {
		/* an attribute of the node's device (the PCI function), through its show/store handler */
		struct nmx_drm_sysfs_get *sg = data;
		struct sysctl_oid *oid;
		struct device *ldev;
		size_t len;
		int err;

		if (df == NULL || df->minor == NULL || df->minor->dev == NULL || df->minor->dev->dev == NULL)
			return ENODEV;
		ldev = df->minor->dev->dev;
		sg->name[sizeof(sg->name) - 1] = '\0';
		oid = ldev->kobj.oidp != NULL ? nmx_sysctl_find(SYSCTL_CHILDREN(ldev->kobj.oidp), sg->name) : NULL;
		if (oid == NULL)
			return ENOENT;
		if (cmd == NMX_DRM_IOCTL_SYSFS_PUT) {
			/*
			 * Linux lets only root write an attribute, and only one whose mode has a write bit. Every leaf under a
			 * device's kobject is a LinuxKPI sysfs attribute, whose arg2 is the struct attribute (a bin_attribute
			 * starts with one).
			 */
			const struct attribute *attr = (const struct attribute *)(uintptr_t)oid->oid_arg2;
			if (attr == NULL || (attr->mode & 0222) == 0)
				return EACCES;
			err = priv_check(curthread, PRIV_DRIVER);
			if (err)
				return err;
			if (sg->len == 0 || sg->len > sizeof(sg->value))
				return EINVAL;
			return nmx_sysctl_write(oid, sg->value, sg->len);
		}
		err = nmx_sysctl_read(oid, sg->value, sizeof(sg->value), &len);
		sg->len = err ? 0 : (uint32_t)len;
		return err;
	}
	if (df == NULL || df->minor == NULL || df->minor->dev == NULL || !dev_is_pci(df->minor->dev->dev))
		return ENODEV;
	pdev = to_pci_dev(df->minor->dev->dev);
	memset(pi, 0, sizeof(*pi));
	pi->domain = pci_domain_nr(pdev->bus);
	pi->bus = pdev->bus->number;
	pi->dev = PCI_SLOT(pdev->devfn);
	pi->func = PCI_FUNC(pdev->devfn);
	pi->vendor_id = pdev->vendor;
	pi->device_id = pdev->device;
	pi->subvendor_id = pdev->subsystem_vendor;
	pi->subdevice_id = pdev->subsystem_device;
	pi->revision_id = pdev->revision;
	return 0;
}
