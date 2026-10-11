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

## Kernel driver: NMXGPU (AMD and Intel)

`xkpi/` runs the AMD and Intel kernel drivers (amdgpu and i915 from drm-kmod, on one DRM core with TTM and dma-fence) on macOS through FreeBSD's
LinuxKPI and a FreeBSD-kernel layer implemented on xnu (`xkpi/bsdcompat`, `xkpi/xnu`). `xkpi/kext` links it all
into `NMXGPU.kext`.

- **Build** (on the build Mac): `xkpi/all.sh` compiles the driver sources drm-kmod's Makefiles select, display core
  included (900), `xkpi/all-i915.sh` i915 (275 with the port's own), `xkpi/rt.sh` the runtime (73 sources),
  `xkpi/kext/build.sh <out>` links the kext (13.6 MB).
- **Link check**: `kmutil create` against macOS 15.8.1's kernel collections resolves every symbol (test
  collection in `/tmp` only; nothing installed).
- **Device nodes**: each DRM minor is an xnu devfs clone node, so every open has its own file, as LinuxKPI expects.
- **Mapping GPU memory**: a kext cannot implement `mmap(2)` on a device, so mappings use the node ioctls in
  `xkpi/bsdcompat/include/nmx_drm_uapi.h`. The driver's own fault handler fills the page list; the range is mapped
  whole into the caller. TTM buffers stay pinned while mapped. libdrm's `drm_mmap`/`drm_munmap` use these ioctls on
  Darwin (`patches/libdrm-macos.patch`).
- **Opt-in only, per vendor**: the kext declines every AMD and Intel GPU unless the boot argument `nmxamd=1` /
  `nmxintel=1` or the device property `nullmoth-gpu` is set. Taking a GPU that drives the boot display resets its display engine.

Not done yet:

- It has not been loaded on hardware. Start-up runs all SYSINITs, then the driver probes the device.
- Display: the bridge (`override/src/nmx_fb_bridge.c`, `xnu/nmx_fb.cpp`) is an in-kernel DRM client that lights every
  connected output with one XRGB8888 buffer in contiguous VRAM and publishes it as an IOFramebuffer (no copy). One
  mode, the largest output's; smaller outputs mirror the top-left. Mode switching and per-output framebuffers are not
  done yet.
- HDCP, writeback connectors and the panel brightness quirk table are not offered (their DRM helpers are GPL).
- There are no file descriptors for dma-buf or sync_file (a kext cannot create them), so PRIME export and sync
  file import/export report EOPNOTSUPP.
- DMI quirks see the SMBIOS the bootloader presents (the Mac model), not the PC's board.
- Kernel FPU sections run with interrupts off.

- i915 covers Gen9 through Meteor Lake (display included); Lunar Lake and later need xe, which drm-kmod does not carry.
  Intel stolen memory is read from the IGD as FreeBSD's early quirks do; the legacy whole-object mmap ioctl is refused
  (Mesa uses the offset path).
