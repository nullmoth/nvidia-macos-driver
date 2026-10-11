// drmlist: enumerates DRM devices through libdrm exactly as Mesa does (drmGetDevices2), then opens each render node
// and asks for its version and bus id. On macOS this proves the libdrm Darwin port and the NMDRM kext agree.
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include <xf86drm.h>

int main(void)
{
    drmDevicePtr devs[16];
    int n = drmGetDevices2(0, devs, 16);
    if (n < 0) { printf("drmGetDevices2 failed: %d\n", n); return 1; }
    printf("drmGetDevices2: %d device(s)\n", n);
    int bad = 0;
    for (int i = 0; i < n; i++) {
        drmDevicePtr d = devs[i];
        if (d->bustype != DRM_BUS_PCI) { printf("  [%d] bus type %d (not PCI)\n", i, d->bustype); bad++; continue; }
        printf("  [%d] pci %04x:%02x:%02x.%u  %04x:%04x  sub %04x:%04x  rev %02x\n", i,
               d->businfo.pci->domain, d->businfo.pci->bus, d->businfo.pci->dev, d->businfo.pci->func,
               d->deviceinfo.pci->vendor_id, d->deviceinfo.pci->device_id, d->deviceinfo.pci->subvendor_id,
               d->deviceinfo.pci->subdevice_id, d->deviceinfo.pci->revision_id);
        if (!(d->available_nodes & (1 << DRM_NODE_RENDER))) { printf("      no render node\n"); bad++; continue; }
        const char *path = d->nodes[DRM_NODE_RENDER];
        int fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd < 0) { printf("      open %s failed\n", path); bad++; continue; }
        drmVersionPtr v = drmGetVersion(fd);
        char *busid = drmGetBusid(fd);
        printf("      %s: driver \"%s\" %d.%d.%d (%s), busid \"%s\"\n", path, v ? v->name : "?", v ? v->version_major : -1,
               v ? v->version_minor : -1, v ? v->version_patchlevel : -1, v ? v->desc : "?", busid ? busid : "?");
        if (!v || !busid) bad++;
        if (v) drmFreeVersion(v);
        if (busid) drmFreeBusid(busid);
        close(fd);
    }
    drmFreeDevices(devs, n);
    return bad ? 2 : (n ? 0 : 3);
}
