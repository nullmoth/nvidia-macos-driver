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
    uint32_t flags;     /* in: 0 */
    uint64_t addr;      /* out: the address in the calling process */
};

struct nmx_drm_munmap {
    uint64_t addr;      /* an address returned by NMX_DRM_IOCTL_MMAP */
    uint64_t size;      /* the size it was mapped with */
};

#define NMX_DRM_IOCTL_MMAP _IOWR('N', 0x40, struct nmx_drm_mmap)
#define NMX_DRM_IOCTL_MUNMAP _IOW('N', 0x41, struct nmx_drm_munmap)
#endif
