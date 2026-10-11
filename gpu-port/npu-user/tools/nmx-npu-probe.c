/*
 * nmx-npu-probe: what the kernel says about every NPU node, for "Send logs".
 *
 * For each /dev/accel/accelN: the DRM driver's name, version and description, the PCI function it drives, and each
 * driver attribute Linux would show under /sys/bus/pci/devices/<bdf>/ (read through the node, since macOS has no
 * sysfs). A missing attribute is printed with its error, so the report shows what the driver did and did not answer.
 * Read-only; needs no privileges beyond opening the node.
 */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "nmx_drm_uapi.h"

/* include/uapi/drm/drm.h */
struct drm_version_arg {
    int version_major, version_minor, version_patchlevel;
    size_t name_len;
    char *name;
    size_t date_len;
    char *date;
    size_t desc_len;
    char *desc;
};
struct drm_pciinfo {
    uint16_t domain;
    uint8_t bus, dev, func;
    uint16_t vendor_id, device_id, subvendor_id, subdevice_id;
    uint8_t revision_id;
};
#define IOCTL_VERSION _IOWR('d', 0x00, struct drm_version_arg)
#define IOCTL_PCIINFO _IOR('d', 0x15, struct drm_pciinfo)

/* every attribute the two NPU drivers in NMXNPU publish (amdxdna_sysfs.c, ivpu_sysfs.c), plus one that must be
   missing: a node that answers it too would mean the lookup is broken */
static const char *const amd_attrs[] = { "device_type", "fw_version", "vbnv", "nmx_not_an_attribute", NULL };
static const char *const intel_attrs[] = { "npu_busy_time_us", "sched_mode", "nmx_not_an_attribute", NULL };

static void attr(int fd, const char *name)
{
    struct nmx_drm_sysfs_get *sg = calloc(1, sizeof(*sg));
    if (sg == NULL)
        return;
    snprintf(sg->name, sizeof(sg->name), "%s", name);
    if (ioctl(fd, NMX_DRM_IOCTL_SYSFS_GET, sg) == 0) {
        size_t n = sg->len < sizeof(sg->value) ? sg->len : sizeof(sg->value) - 1;
        while (n > 0 && sg->value[n - 1] == '\n')
            n--;
        printf("  %-28s %.*s\n", name, (int)n, sg->value);
    } else {
        printf("  %-28s (error %d: %s)\n", name, errno, strerror(errno));
    }
    free(sg);
}

static void probe(const char *path)
{
    printf("== %s\n", path);
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("  open failed: error %d: %s\n", errno, strerror(errno));
        return;
    }
    char name[64] = "", date[64] = "", desc[256] = "";
    struct drm_version_arg v = { 0 };
    v.name = name, v.name_len = sizeof(name) - 1;
    v.date = date, v.date_len = sizeof(date) - 1;
    v.desc = desc, v.desc_len = sizeof(desc) - 1;
    if (ioctl(fd, IOCTL_VERSION, &v) == 0)
        printf("  driver %s %d.%d.%d (%s) %s\n", name, v.version_major, v.version_minor, v.version_patchlevel, date, desc);
    else
        printf("  DRM_IOCTL_VERSION failed: error %d: %s\n", errno, strerror(errno));
    struct drm_pciinfo pi = { 0 };
    if (ioctl(fd, IOCTL_PCIINFO, &pi) == 0)
        printf("  pci %04x:%02x:%02x.%x  id %04x:%04x  subsystem %04x:%04x  rev %02x\n", pi.domain, pi.bus, pi.dev,
               pi.func, pi.vendor_id, pi.device_id, pi.subvendor_id, pi.subdevice_id, pi.revision_id);
    else
        printf("  DRM_IOCTL_GET_PCIINFO failed: error %d: %s\n", errno, strerror(errno));
    const char *const *list = strcmp(name, "amdxdna") == 0 ? amd_attrs : intel_attrs;
    for (int i = 0; list[i] != NULL; i++)
        attr(fd, list[i]);
    close(fd);
}

int main(void)
{
    DIR *d = opendir("/dev/accel");
    if (d == NULL) {
        printf("no /dev/accel: no NPU driver attached (error %d: %s)\n", errno, strerror(errno));
        return 1;
    }
    int found = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strncmp(e->d_name, "accel", 5) != 0 || e->d_name[5] == '\0')
            continue;
        char path[300];
        snprintf(path, sizeof(path), "/dev/accel/%s", e->d_name);
        probe(path);
        found++;
    }
    closedir(d);
    if (found == 0)
        printf("/dev/accel has no accelN nodes\n");
    return found ? 0 : 1;
}
