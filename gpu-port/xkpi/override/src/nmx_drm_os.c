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

#include <linux/kernel.h>
#include <linux/err.h>
#include <linux/debugfs.h>

#include <drm/drm_device.h>
#include <drm/drm_gem.h>
#include <drm/drm_gem_ttm_helper.h>
#include <drm/drm_writeback.h>
#include <drm/ttm/ttm_bo.h>
#include <drm_os_freebsd.h>

#include "nmx_vma.h"

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
 * FreeBSD publishes each DRM minor as hw.dri.N (PCI id, type) for its libdrm. The port's libdrm asks the node
 * itself (DRM_IOCTL_GET_PCIINFO), so there is no alias to make.
 */
int drm_dev_alias(struct device *ldev, struct drm_minor *minor, const char *minor_str)
{
    return 0;
}

void drm_dev_alias_free(struct drm_minor *minor)
{
}

/* ---- GEM objects backed by TTM buffers ------------------------------------------------------------------------------ */

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

/* ---- writeback connectors: none are created on macOS, so no job ever reaches these ---------------------------------- */

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

void debugfs_create_atomic_t(const char *name, umode_t mode, struct dentry *parent, atomic_t *value)
{
}

void debugfs_remove(struct dentry *dentry)
{
}
