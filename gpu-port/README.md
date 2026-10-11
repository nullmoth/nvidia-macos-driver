# AMD and Intel GPUs: the Vulkan side

Mesa's Vulkan drivers, RADV (AMD) and ANV (Intel), built for x86_64 macOS against libdrm. They reach the GPU through DRM
device nodes in `/dev/dri`, as on Linux and the BSDs. `kexts/NMDRM` provides those nodes.

## Sources

| Tree | Revision | Patch |
|---|---|---|
| Mesa | bffd919 (26.3.0-devel) | `patches/mesa-macos.patch` |
| libdrm | b97cbde | `patches/libdrm-macos.patch` |

The patches cover:

- **libdrm**
  - Builds on Darwin.
  - Finds a DRM node by its device number among the entries of `/dev/dri`. Darwin assigns character majors at run time, and its `devname_r` searches only the top level of `/dev`.
  - Reads PCI identity with `DRM_IOCTL_GET_PCIINFO`, the OpenBSD interface.
- **Mesa**
  - Treats Darwin as a KMS/DRM system.
  - Uses `os_sync_wait_on_address` for futexes.
  - Adds the `vk_image` Apple guards.
  - Allows undefined weak entrypoints at link time, as lavapipe already does.
  - Takes addrlib's portable path (`HAVE_TSERVER`).
  - Adds a Darwin front end for drm-shim (`src/drm-shim/drm_shim_darwin.c`, dyld interposing) and POSIX shared memory in place of memfd.

## Build

Build on the build Mac (Apple silicon, cross-compiling to x86_64) with the cross file `tools/x86_64-macos.ini`. Configure options: `-Dvulkan-drivers=amd,intel -Dgallium-drivers= -Dplatforms= -Dtools=drm-shim`.

## Tests

- **`tools/vk_chip_matrix.sh`** runs `vkprobe` once per GPU through Mesa's noop drm-shim kernels: 20 AMD chips (GCN1 to RDNA4, every APU) and 21 Intel chips (Gen9 to Xe3). Each must create a device and compile a compute pipeline (`tools/fill.comp`). Result on 2026-10-11: 41 of 41.
- **`tools/drmlist`** enumerates devices through libdrm, the same way Mesa does, then queries each render node. On studio with NMDRM loaded, it reports `10de:2d05` at `pci:0000:02:00.0` from `/dev/dri/renderD128`.

## Next

The amdgpu and xe kernel interfaces go behind NMDRM one family at a time. Until a family's ioctls exist, NMDRM reports the driver name `nmdrm`, and RADV/ANV skip the device.
