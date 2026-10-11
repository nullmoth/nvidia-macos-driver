/*
 * Device-node ioctls the port adds to every DRM node on macOS (shared with the libdrm and Mesa patches).
 *
 * A kext cannot implement mmap(2) for a character device on macOS, so user mappings of GPU memory are requested
 * with NMX_DRM_IOCTL_MMAP using the same offset mmap(2) would have taken (the GEM fake offset), and released with
 * NMX_DRM_IOCTL_MUNMAP. Closing the node releases every mapping still made through it.
 */
#ifndef NMX_DRM_UAPI_H
#define NMX_DRM_UAPI_H
#include <stdint.h>
#include <sys/ioccom.h>

struct nmx_drm_mmap {
    uint64_t offset;    /* in: the mmap offset (page aligned) */
    uint64_t size;      /* in: bytes (rounded up to pages) */
    uint32_t prot;      /* in: PROT_READ | PROT_WRITE */
    uint32_t flags;     /* in: 0 or NMX_DRM_MMAP_FIXED */
    uint64_t addr;      /* in (NMX_DRM_MMAP_FIXED): the address to map at; out: the address in the calling process */
};

struct nmx_drm_munmap {
    uint64_t addr;      /* an address returned by NMX_DRM_IOCTL_MMAP */
    uint64_t size;      /* the size it was mapped with */
};

/*
 * Reads one sysfs attribute of the node's device, as Linux userland reads /sys/bus/pci/devices/<bdf>/<name>. The port
 * keeps a driver's attributes in its own tree (not xnu's sysctl), so runtimes that discover a device through sysfs
 * (AMD's XRT reads "device_type" to pick its queue model) ask the node instead. 'name' may be "group/attr".
 */
struct nmx_drm_sysfs_get {
    char name[64];      /* in: attribute name, NUL-terminated */
    uint32_t len;       /* GET out: bytes in value, without the NUL; PUT in: bytes to write */
    uint32_t pad;
    char value[4096];   /* GET out: the text the show handler wrote; PUT in: the new value (Linux's PAGE_SIZE limit) */
};

/* map at req.addr (page aligned), replacing the caller's own mapping there, as mmap(MAP_FIXED) does */
#define NMX_DRM_MMAP_FIXED 0x1

#define NMX_DRM_IOCTL_MMAP _IOWR('N', 0x40, struct nmx_drm_mmap)
#define NMX_DRM_IOCTL_MUNMAP _IOW('N', 0x41, struct nmx_drm_munmap)
#define NMX_DRM_IOCTL_SYSFS_GET _IOWR('N', 0x42, struct nmx_drm_sysfs_get)
/* writes 'len' bytes of 'value' to the attribute (root only, and only a writable attribute, as on Linux) */
#define NMX_DRM_IOCTL_SYSFS_PUT _IOW('N', 0x43, struct nmx_drm_sysfs_get)
#endif
