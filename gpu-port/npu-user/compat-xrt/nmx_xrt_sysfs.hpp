// SPDX-License-Identifier: Apache-2.0
/*
 * XRT on macOS: what Linux userland learns from sysfs and mmap(2), asked of the device node instead.
 *
 * Linux XRT finds an NPU by listing /sys/bus/pci/drivers/amdxdna, reads its attributes from
 * /sys/bus/pci/devices/<bdf>/, and maps buffers by mmap(2) on /dev/accel/accelN. macOS has no sysfs, and a kext cannot
 * back mmap(2) on a character device. NullMoth's kernel port answers on every /dev/accel/accelN node instead:
 *   DRM_IOCTL_VERSION           the driver's name ("amdxdna", "intel_vpu")
 *   DRM_IOCTL_GET_PCIINFO       its PCI address and ids
 *   NMX_DRM_IOCTL_SYSFS_GET/PUT one attribute, through the driver's own show/store handler
 *   NMX_DRM_IOCTL_MMAP/MUNMAP   buffer mappings (NMX_DRM_MMAP_FIXED for MAP_FIXED)
 * The standard PCI attributes Linux's PCI core adds (vendor, device, subsystem_*, revision) are formatted from
 * GET_PCIINFO exactly as Linux prints them. Anything else the kernel does not have reads as missing (ENOENT), which
 * XRT reports the way it reports a missing sysfs file.
 */
#ifndef NMX_XRT_SYSFS_HPP
#define NMX_XRT_SYSFS_HPP
#ifdef __APPLE__

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>
#include <vector>
#include <dirent.h>
#include <fcntl.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "nmx_drm_uapi.h"

namespace nmx_xrt {

// include/uapi/drm/drm.h (the port's DRM core uses Linux's layout and numbers)
struct drm_version_arg {
  int version_major;
  int version_minor;
  int version_patchlevel;
  size_t name_len;
  char* name;
  size_t date_len;
  char* date;
  size_t desc_len;
  char* desc;
};
struct drm_pciinfo {
  uint16_t domain;
  uint8_t bus;
  uint8_t dev;
  uint8_t func;
  uint16_t vendor_id;
  uint16_t device_id;
  uint16_t subvendor_id;
  uint16_t subdevice_id;
  uint8_t revision_id;
};
constexpr unsigned long ioctl_version = _IOWR('d', 0x00, drm_version_arg);
constexpr unsigned long ioctl_pciinfo = _IOR('d', 0x15, drm_pciinfo);

constexpr const char* sys_devices = "/sys/bus/pci/devices/";

struct node {
  std::string bdf;      // "0000:c4:00.1", the name Linux gives the device under /sys/bus/pci/devices
  std::string path;     // /dev/accel/accelN
  std::string driver;   // the DRM driver's name
  int instance;         // N
  drm_pciinfo pci;
};

inline std::string
bdf_of(const drm_pciinfo& pi)
{
  char b[32];
  snprintf(b, sizeof(b), "%04x:%02x:%02x.%x", pi.domain, pi.bus, pi.dev, pi.func);
  return b;
}

// every /dev/accel/accelN with its driver and PCI address, by instance
inline std::vector<node>
nodes()
{
  std::vector<node> v;
  std::unique_ptr<DIR, int (*)(DIR*)> d(opendir("/dev/accel"), closedir);
  if (!d)
    return v;
  while (auto e = readdir(d.get())) {
    const std::string n = e->d_name;
    if (n.size() <= 5 || n.compare(0, 5, "accel") != 0 ||
        !std::all_of(n.begin() + 5, n.end(), [](char c) { return c >= '0' && c <= '9'; }))
      continue;
    node nd;
    nd.path = "/dev/accel/" + n;
    nd.instance = std::stoi(n.substr(5));
    int fd = ::open(nd.path.c_str(), O_RDWR | O_CLOEXEC);
    if (fd < 0)
      continue;
    char name[64] = {};
    drm_version_arg ver = {};
    ver.name_len = sizeof(name) - 1;
    ver.name = name;
    nd.pci = {};
    bool ok = ioctl(fd, ioctl_version, &ver) == 0 && ioctl(fd, ioctl_pciinfo, &nd.pci) == 0;
    ::close(fd);
    if (!ok)
      continue;
    nd.driver = name;
    nd.bdf = bdf_of(nd.pci);
    v.push_back(std::move(nd));
  }
  std::sort(v.begin(), v.end(), [](const node& a, const node& b) { return a.instance < b.instance; });
  return v;
}

inline bool
find(const std::string& bdf, node& out)
{
  for (auto& n : nodes()) {
    if (n.bdf == bdf) {
      out = n;
      return true;
    }
  }
  return false;
}

// "/sys/bus/pci/devices/<bdf>[/<attribute path>]" -> bdf and attribute path; false for any other path
inline bool
split_sys_path(const std::string& path, std::string& bdf, std::string& attr)
{
  const std::string root = sys_devices;
  if (path.compare(0, root.size(), root) != 0)
    return false;
  auto rest = path.substr(root.size());
  auto slash = rest.find('/');
  bdf = rest.substr(0, slash);
  attr.clear();
  // XRT joins "<dev>/<subdir>/<entry>" with an empty subdir for device attributes: empty segments are dropped
  if (slash != std::string::npos) {
    for (char c : rest.substr(slash + 1)) {
      if (c == '/' && (attr.empty() || attr.back() == '/'))
        continue;
      attr += c;
    }
    while (!attr.empty() && attr.back() == '/')
      attr.pop_back();
  }
  return !bdf.empty();
}

// the PCI core's own attributes, as Linux formats them (drivers/pci/pci-sysfs.c)
inline bool
pci_attr(const drm_pciinfo& pi, const std::string& attr, std::string& out)
{
  char b[16];
  if (attr == "vendor")
    snprintf(b, sizeof(b), "0x%04x\n", pi.vendor_id);
  else if (attr == "device")
    snprintf(b, sizeof(b), "0x%04x\n", pi.device_id);
  else if (attr == "subsystem_vendor")
    snprintf(b, sizeof(b), "0x%04x\n", pi.subvendor_id);
  else if (attr == "subsystem_device")
    snprintf(b, sizeof(b), "0x%04x\n", pi.subdevice_id);
  else if (attr == "revision")
    snprintf(b, sizeof(b), "0x%02x\n", pi.revision_id);
  else
    return false;
  out = b;
  return true;
}

// one attribute of the device at bdf: 0 or an errno (ENODEV: no such device, ENOENT: no such attribute)
inline int
read_attr(const std::string& bdf, const std::string& attr, std::string& out)
{
  out.clear();
  node n;
  if (!find(bdf, n))
    return ENODEV;
  if (pci_attr(n.pci, attr, out))
    return 0;
  auto sg = std::make_unique<nmx_drm_sysfs_get>();
  if (attr.empty() || attr.size() >= sizeof(sg->name))
    return ENOENT;
  std::memset(sg.get(), 0, sizeof(*sg));
  std::memcpy(sg->name, attr.data(), attr.size());
  int fd = ::open(n.path.c_str(), O_RDWR | O_CLOEXEC);
  if (fd < 0)
    return errno;
  int r = ioctl(fd, NMX_DRM_IOCTL_SYSFS_GET, sg.get());
  int e = errno;
  ::close(fd);
  if (r != 0)
    return e;
  out.assign(sg->value, std::min<size_t>(sg->len, sizeof(sg->value)));
  return 0;
}

inline int
write_attr(const std::string& bdf, const std::string& attr, const void* data, size_t len)
{
  node n;
  if (!find(bdf, n))
    return ENODEV;
  auto sg = std::make_unique<nmx_drm_sysfs_get>();
  if (attr.empty() || attr.size() >= sizeof(sg->name))
    return ENOENT;
  if (len == 0 || len > sizeof(sg->value))
    return EINVAL;
  std::memset(sg.get(), 0, sizeof(*sg));
  std::memcpy(sg->name, attr.data(), attr.size());
  std::memcpy(sg->value, data, len);
  sg->len = static_cast<uint32_t>(len);
  int fd = ::open(n.path.c_str(), O_RDWR | O_CLOEXEC);
  if (fd < 0)
    return errno;
  int r = ioctl(fd, NMX_DRM_IOCTL_SYSFS_PUT, sg.get());
  int e = errno;
  ::close(fd);
  return r == 0 ? 0 : e;
}

// a Linux sysfs path under /sys/bus/pci/devices: 0 or an errno (ENOENT for any other path)
inline int
read_sys(const std::string& path, std::string& out)
{
  std::string bdf, attr;
  if (!split_sys_path(path, bdf, attr))
    return ENOENT;
  return read_attr(bdf, attr, out);
}

inline int
write_sys(const std::string& path, const void* data, size_t len)
{
  std::string bdf, attr;
  if (!split_sys_path(path, bdf, attr))
    return ENOENT;
  return write_attr(bdf, attr, data, len);
}

// mmap(2) on a device node: the kernel maps the buffer at 'offset' (MAP_FIXED: at addr, replacing what is there)
inline void*
map(int fd, void* addr, size_t len, int prot, int flags, off_t offset)
{
  nmx_drm_mmap r = {};
  r.offset = static_cast<uint64_t>(offset);
  r.size = len;
  r.prot = static_cast<uint32_t>(prot & (PROT_READ | PROT_WRITE));
  if (flags & MAP_FIXED) {
    r.flags = NMX_DRM_MMAP_FIXED;
    r.addr = reinterpret_cast<uint64_t>(addr);
  }
  if (ioctl(fd, NMX_DRM_IOCTL_MMAP, &r) != 0)
    return MAP_FAILED;
  return reinterpret_cast<void*>(r.addr);
}

inline int
unmap(int fd, void* addr, size_t len)
{
  nmx_drm_munmap r = { reinterpret_cast<uint64_t>(addr), len };
  return ioctl(fd, NMX_DRM_IOCTL_MUNMAP, &r);
}

} // namespace nmx_xrt

#endif // __APPLE__
#endif
