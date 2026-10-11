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

`xkpi/` runs the AMD and Intel kernel drivers (amdgpu and i915 from drm-kmod, and Intel's xe from Linux 6.14, on one DRM core with TTM and
dma-fence) on macOS through FreeBSD's
LinuxKPI and a FreeBSD-kernel layer implemented on xnu (`xkpi/bsdcompat`, `xkpi/xnu`). `xkpi/kext` links it all
into `NMXGPU.kext`.

- **Build** (on the build Mac): `xkpi/all.sh` compiles the driver sources drm-kmod's Makefiles select, display core
  included (900), `xkpi/all-i915.sh` i915 (274 with the port's own), `xkpi/all-xe.sh` xe (193 with the port's own),
  `xkpi/rt.sh` the runtime (75 sources), `xkpi/kext/build.sh <out>` links the kext (15.4 MB).
- **xe** comes from Linux 6.14 (`linux-6.14`, the DRM core version drm-kmod tracks). `all-xe.sh` copies it to `xe-src`
  without its GPL-licensed files and KUnit tests and refuses to build if any GPL source is left; the page-table walker,
  resource cursor, module glue and trace/test headers are the port's own (`override/xe`). `fork_xe.py` holds the few
  macOS forms (BAR resize left to the firmware, drm-kmod's drm_file/TTM, worker copies into a process, Panther Lake
  firmware names). xe carries its own build of the Intel display code, so the kext links it as one object that exports
  nothing. It drives Lunar Lake and Battlemage, and Panther Lake by its forced device ids; i915 keeps every older part.
- **User pages**: `get_user_pages` pins a process's pages through an IOMemoryDescriptor wiring on its task; the last
  `put_page` releases it. xe userptr (`override/xe/nmx_xe_hmm.c`) and the other drivers' userptr paths use it.
- **Link check**: `kmutil create` against macOS 15.8.1's kernel collections resolves every symbol (test
  collection in `/tmp` only; nothing installed).
- **Device nodes**: each DRM minor is an xnu devfs clone node, so every open has its own file, as LinuxKPI expects.
- **Mapping GPU memory**: a kext cannot implement `mmap(2)` on a device, so mappings use the node ioctls in
  `xkpi/bsdcompat/include/nmx_drm_uapi.h`. The driver's own fault handler fills the page list; the range is mapped
  whole into the caller. TTM buffers stay pinned while mapped. libdrm's `drm_mmap`/`drm_munmap` use these ioctls on
  Darwin (`patches/libdrm-macos.patch`).
- **Opt-in only, per vendor**: the kext declines every AMD and Intel GPU unless the boot argument `nmxamd=1` /
  `nmxintel=1` or the device property `nullmoth-gpu` is set. Taking a GPU that drives the boot display resets its display engine.

## Metal on AMD and Intel GPUs

- **Accelerator**: `kexts/NVRM/accel/nvrm-accel.cpp` built with `-DNMX_ACCEL` (`build/nmxaccel_build.sh`) is NMXAccel: the
  same IOAcceleratorFamily2 accelerator under its own class names, matched to AMD/Intel display functions and started only
  on a device NMXGPU has claimed (`nmxgpu-claimed`). It runs the way an NVIDIA GPU without a display does: a Metal device
  for apps, with frames copied into the IOSurfaces WindowServer reads from NMXGPU's framebuffer.
- **Plugin**: the Metal plugin reads the vendor of the accelerator it was made on and loads RADV (AMD) or ANV (Intel, i915
  and xe) instead of NVK; NVIDIA-only paths (SASS dispatch, NVDEC/NVENC, family-VRAM surface sharing) stay off there.
  One process runs Metal on one vendor's GPU.
- **Userland**: Mesa RADV/ANV with libdrm for Darwin (`patches/`), installed beside NVK in `/Library/GPUBundles/nvmtl`
  only when the AMD/Intel driver is turned on. libdrm finds each GPU through `DRM_IOCTL_GET_PCIINFO`, which the port
  answers at the node (`override/src/nmx_drm_os.c`).

## NPUs: NMXNPU (Intel NPU, AMD Ryzen AI)

`xkpi/kext-npu` links NMXNPU.kext: Intel's NPU driver (Linux 6.14 ivpu: Meteor Lake, Arrow Lake, Lunar Lake, Panther
Lake) and AMD's Ryzen AI NPU driver (Linux 7.3 amdxdna) on the DRM accel core, each NPU published as
`/dev/accel/accelN`. Those drivers are GPL-2.0, so NMXNPU is its own GPL-2.0 kext with its own copy of the layer and a
DRM core built with accel minors (`all-npucore.sh`); NullMoth's layer files are licensed to it under GPL-2.0
(`kext-npu/NOTICE-GPL.txt`). `kext-npu/mksource.sh` packs its complete corresponding source from a compiler dependency
pass, and every release that carries NMXNPU publishes that archive beside the package.

- **Build** (on the build Mac): `all-npucore.sh` (75), `all-npu.sh` (ivpu, accel core, shmem helper, amdxdna and the glue:
  47), `kext-npu/build.sh <out>` (1.2 MB). `fork_npu.py` holds the macOS forms (BAR mapping, module registration,
  firmware names, the 7.x scheduler interface on the 6.14 scheduler).
- **AMD without IOMMU**: macOS gives no AMD IOMMU domain or PASID, so amdxdna runs on carveout memory - one physically
  contiguous region (512 MB down to 64 MB) allocated at probe, which Linux would set through debugfs.
- **Firmware**: `intel/vpu/vpu_*_v1.bin` (API 3.37/3.41, the same major as the driver) and `amdnpu/*/npu*.sbin`, with
  linux-firmware's links made by setup after the checksums pass.
- **Opt-in**: `nmxnpu=1` or the `nullmoth-npu` device property; setup --gpu-test installs it when an NPU is present.

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

- i915 covers Gen9 through Arrow Lake/Meteor Lake (display included); xe covers Lunar Lake, Battlemage and Panther Lake.
  Panther Lake runs 6.14's early support with newer firmware (no GSC, so HuC stays unauthenticated there).
  Intel stolen memory is read from the IGD as FreeBSD's early quirks do; the legacy whole-object mmap ioctl is refused
  (Mesa uses the offset path).
