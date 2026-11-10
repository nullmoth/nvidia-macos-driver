# How the NullMoth NVIDIA driver works

This page follows a frame from a Metal app down to the GPU, then covers how the driver is installed, removed, and
recovered. Paths are the ones on an installed Mac; the source folder for each piece is in brackets.

> The driver is new. It is tested on an RTX 5060 under macOS 15.7.x, and it may not work on every card, board or
> OpenCore setup. If your Mac does not boot macOS with OpenCore yet, set that up first with the
> [OpenCore Install Guide](https://dortania.github.io/OpenCore-Install-Guide/), then install the driver.

## 1. The layers

```
Metal app / WindowServer / Core Image / MPS / OpenGL (Apple's GL-on-Metal)
        |
        v
NVMTLDriver.bundle  (Metal driver plugin)                         [plugin/]
        |  shaders: Apple AIR  -> SPIR-V                            [translator/]
        v
NVK  (Mesa's Vulkan driver for NVIDIA, with the NAK compiler)      [nvk/nvk-macos.patch]
        |  SPIR-V -> NVIDIA machine code, command buffers
        v
NVRM.kext  (NVIDIA's open GPU kernel modules, r610, ported to XNU) [kexts/NVRM]
        |  talks to the GPU's GSP firmware
        v
GeForce RTX GPU  (GSP firmware from /Library/NullMoth/nvfw/nvidia/620.70.06)
```

Display and compositing run beside that path:

```
WindowServer -> IOFramebuffer = NVRMFB.kext            (NVKMS heads, modes, vblank)       [kexts/NVRMFB]
             -> IOAccelerator = NVAccel.kext           (surfaces WindowServer composites)
             -> AppleGraphicsDeviceControl policy = NVRMAGDC.kext                           [kexts/NVRMAGDC]
```

## 2. Kernel side

**NVRM.kext.** NVIDIA's open-source resource manager, built for macOS with an XNU OS layer (`os-xnu*.cpp`) in place of
Linux's. At start it:
1. claims the PCI device, maps its BARs, and places BAR1 (the GPU-visible window into video memory) outside the firmware
   console;
2. holds the I/O registry busy so WindowServer does not start on the firmware screen;
3. on its own thread, loads the GSP firmware from `/Library/NullMoth/nvfw/nvidia/620.70.06/` and boots the GPU with
   `rm_init_adapter()`;
4. brings up NVKMS (NVIDIA's mode-setting) and releases the hold once the display is armed.

The boot argument `-nvoff` makes NVRM leave the card alone. The recovery path below uses it.

NVRM is also a power child of the GPU's PCI device: on system sleep it suspends NVKMS and puts RM in standby with all of
video memory preserved, and on wake it restores the PCI state and resumes both. NVRMFB then does the wake work
IOGraphics expects. [SLEEP-WAKE.md](SLEEP-WAKE.md) has the details.

**NVRMFB.kext** is the macOS framebuffer (an `IOFramebuffer`). It publishes each NVKMS head as a display, reads real
mode timings and EDIDs, and drives vblank from the GPU. **NVAccel.kext** is the `IOAccelerator` that WindowServer and
IOSurface use for the shared surfaces the plugin renders into. **NVRMAGDC.kext** answers Apple's graphics-device-control
policy queries for the card.

All four kexts install into `/Library/Extensions` and load from the Auxiliary Kernel Collection that `install.sh`
rebuilds with `kmutil`. They are not injected by OpenCore: the display kexts link against Apple frameworks
(IOGraphicsFamily, IOAcceleratorFamily2) that only exist in macOS's own kernel collections.

## 3. User space

**NVMTLDriver.bundle** (`/Library/GPUBundles/`) is the Metal driver Apple's Metal.framework loads for the card. It
implements Metal's device, queues, command buffers, encoders, resources, argument buffers, ray tracing and mesh
pipelines on top of Vulkan (`nvmtl_vk.c`), with contract notes for Apple-specific behavior (sample positions, sampler
descriptors, read-only metadata, physical bounds) in `plugin/nvmtl_*.h`.

**Shader translation** (`libnvmtl_translate.dylib`, `translator/`) converts Apple AIR, the bitcode Metal shaders compile
to, into SPIR-V that NVK can compile. It is based on metal2vulkan (LGPL-3.0).

**NVK** (`/Library/GPUBundles/nvmtl/`) is Mesa's Vulkan driver for NVIDIA plus the NAK shader compiler, patched to run
on macOS against NVRM instead of the Linux DRM interface (`nvk/nvk-macos.patch` and the rest of the chain on Mesa `90a959eb`, 26.3.0-rc3).

**NVIDIAShared.bundle** is no longer in the package. Packages up to 2.8.1 carried it with two of NVIDIA's closed
compiler libraries; the plugin used it only when a developer switch (`NVMTL_VENDOR_COMPILER`) was set. The installer
removes an old copy on update and the package gate refuses a tree that holds one. The plugin's MPS convolution fast path
is `plugin/nvconv.metal`.

## 4. OpenCore settings the driver needs

| Setting | Value | Why |
|---|---|---|
| `NVRAM > Add > 7C436110-…-FE41995C9F82 > boot-args` | `nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80 ipc_control_port_options=0` | turns on the framebuffer and accelerator, 4 display heads; the AMFI arguments let WindowServer load the driver bundle |
| `NVRAM > Add > … > csr-active-config` | `<430A0000>` | the SIP value the driver was tested with (its kexts are not Apple-signed) |
| `Misc > Security > SecureBootModel` | `Disabled` | Apple Secure Boot refuses kexts Apple did not sign |
| `UEFI > Quirks > ResizeGpuBars` | `13` | 8 GB BAR1 for full memory bandwidth; NVRM then moves BAR1 away from the boot screen (tested on the RTX 5060) |
| `Booter > Quirks > ResizeAppleGpuBars` | `-1` | macOS sees the full BAR |
| `Kernel > Block` | `com.apple.iokit.IONDRVSupport`, `Exclude` | otherwise the firmware framebuffer takes display index 0 from NVRMFB |
| BIOS | Above 4G Decoding on, CSM off | the card maps memory above 4 GB |

Optional boot arguments, for troubleshooting: `-nvrmnoflip` keeps NVAccel on the copy path (no zero-copy scan-out
of the composited surface) and is the first thing to try when the desktop flashes or tears while the cursor moves;
`-nvrmnogo` leaves the GPU idle until `nvrmctl go <pass>`; `-nvrmnobootscreen` skips the boot-screen snapshot.

Installing macOS itself needs different values. The macOS installer has no NVIDIA driver, so it runs on the firmware's
screen, which only survives macOS's PCI setup with a small BAR: `ResizeAppleGpuBars = 0`, `ResizeGpuBars = -1`, and
`IONDRVSupport` not excluded. 1401 builds the installer that way. The 1401 Mac app switches to the table above when it
installs the driver.

## 5. The 1401 Mac app (installs the driver)

**1401.app** lives in the [1401](https://github.com/nullmoth/1401) repository (`mac/`, Swift + WebKit; the window is
HTML/JS in `mac/Resources/`). It installs a driver package with that package's own `install.sh`. Every change to the system is
made by one root script, `nullmoth-setup.sh`, which the app runs through macOS's administrator prompt.

**Finding OpenCore.** The script reads OpenCore's `boot-path` NVRAM variable, which names the partition OpenCore started
from, and uses that partition. If the variable is missing, it searches every EFI and FAT32 partition and keeps only a config whose SMBIOS model matches this Mac, so a rescue stick or another machine's EFI is never edited; if none matches it stops and asks for the boot disk or stick to be plugged in. OpenCore may live
in `EFI/OC` or, when `OpenCore.efi` is the firmware's `BOOTx64.efi`, in `EFI/BOOT`; both are found. If more than one
OpenCore is found, the app asks which one.

**Install** (`--dry` lists the same changes and makes none):
1. checks the driver package's SHA-256;
2. test-builds the Auxiliary Kernel Collection before changing anything, so a Mac that cannot take the driver is left
   as it was;
3. backs up `config.plist` next to it, then makes only the changes in section 4 that are missing, and lints the result
   (any failure restores the backup);
4. adds **1401: Remove NVIDIA driver** to the boot picker (`Misc > Tools`, the `NullMothSafe.efi` tool);
5. runs the package's `install.sh`: copies the files and builds the Auxiliary Kernel Collection (if it fails, the
   config backup is put back);
6. writes an install record to `/Library/NullMoth/state` and installs the recovery daemon.

If SIP is still fully on in the running system, the first run sets only SIP, Secure Boot, the boot arguments and the
boot picker entry, and asks for a restart; the second run installs the driver and switches the BAR.

**Remove.** If the config is unchanged since the install, the backup is copied back. Otherwise only the driver's own
edits are undone. Then `uninstall.sh` removes the files and rebuilds the kernel collection.

**The way back if the driver stops macOS starting.** Choosing *1401: Remove NVIDIA driver* in the boot picker runs
`NullMothSafe.efi` (1401's `mac/efi-safe/`, Rust UEFI). It adds `-nvoff` to the boot arguments for the next start and sets the
NVRAM flag `nullmoth-remove=1`. macOS then starts without the driver, the LaunchDaemon `com.nullmoth.recover` sees the
flag, runs `nullmoth-setup.sh --remove`, and restarts.

**Verbose startup.** *Turn verbose on/off* adds or removes the boot argument `-v` in the OpenCore config and in NVRAM,
so the next start shows macOS's startup text instead of the Apple logo.

**USB map.** The app watches the USB ports while you plug a device into each one, then writes `UTBMap.kext` (for
USBToolBox) into the OpenCore `Kexts` folder.

**Crash reports.** If the driver panics the Mac, the app offers at the next login to write a text report to the
Desktop: hardware, macOS version, and what failed, with names, serial numbers, network addresses and paths removed.
Nothing is sent; you can upload the file on the site yourself.

## 6. The package

`package/install.sh` and `uninstall.sh` work without the app (README, *Install — prebuilt*). The tarball holds
`pkgroot/` with every file at its install path plus `SHA256SUMS`.

## 7. Building

See the README, *Build from source*. The kexts need NVIDIA's `open-gpu-kernel-modules` at tag `620.70.06`; NVK needs
Mesa `90a959eb` (the mesa-26.3.0-rc3 tag; 2.8 and 2.8.1 used `17ca6174`) with, in order, `nvk/nvk-macos.patch`, `nvk/nvk-fixes-after-def16.patch`, `nvk/nvk-620-kernel.patch`,
`nvk/nvk-recycle.patch`, `nvk/nvk-slab.patch`, `nvk/nvk-slabdef.patch`, `nvk/nvk-hubfix.patch`, `nvk/nvk-perf-boost-burst.patch`,
`nvk/nvk-rated-tdp.patch`, `nvk/nvk-gps-maxpower.patch`, `nvk/nvk-metal-wsi.patch`, `nvk/nvk-card-select.patch`, `nvk/hwrt/ttu-nak.patch`, `nvk/hwrt/ttu-rq.patch`, `nvk/curves/curves.patch`, `nvk/hwrt/wide.patch` and `nvk/nvk-warnings.patch` (`patch -p1` from the Mesa root; the last two are the ray-query part for the RT cores, off unless `NVK_HWRT_QUERY=1`, see `docs/HWRT.md`; `nvk/hwrt/nvk_hwrt.h` and `nvk_hwrt_layout.h` are copied into `src/nouveau/vulkan` after them), then
`nvk/dlss/patch_dlss_cache.py <mesa tree>`. NVK compiles against the `open-gpu-kernel-modules` 610.57.04 headers with
`nvk/ogkm-nvos-620.patch` and `nvk/ogkm-warnings.patch` applied to that tree; 620's `nv.h` does not compile in user space, so the rest stays 610.

`nvk/nvk-metal-wsi.patch` turns on `VK_EXT_metal_surface`: it adds the extension to NVK's instance table, counts the Metal platform as a window system, and runs Mesa's Metal swapchain in its software mode (a linear host-visible image copied into the `CAMetalLayer` drawable by Mesa's layer helper, because the hardware mode needs a driver that can bind a Metal texture to a `VkImage`). `nvk/nvk-card-select.patch` (last in the chain) makes the RM connection honour `NVK_ACCEL_ID`, the registry entry id of one accelerator, so a process bound to one of several cards opens that card (unset, the first accelerator, as before). The build uses `-Dplatforms=macos` for the NVK build and still `-Dplatforms=` for the native `mesa_clc` tool build.

`build/build_nvk.sh <mesa git> <610.57.04 tree> <out> [2.9|2.8]` does all of it: it exports Mesa `90a959eb` (26.3.0-rc3; `2.8` exports `17ca6174`) with `git archive`,
applies the header patch and the chain above (and copies the two hwrt headers), builds `mesa_clc` and `vtn_bindgen2` from that same tree (tools from another Mesa
revision deserialize NVK's internal shaders as garbage), cross-builds `libvulkan_nouveau.dylib` for x86_64 and prints its
LC_UUID and export count. The `2.8` argument builds the set the 2.8 / 2.8.1 package was made from (no hubfix, boost, rated-tdp
or gps-maxpower patches; `nvk/release-2.8/nvk-fixes-after-def16.patch`). Rebuilding 2.8 gives the shipped library byte for byte
except the 16 LC_UUID bytes: the linker derives the UUID from the unstripped file, which records the absolute path of every
object, so a different build directory gives a different UUID. `build/nvk_compare.py` states exactly which bytes differ.
The NAK changes under `nvk/speed267` are already contained in `nvk-macos.patch`; the scripts there record how they were made.

The video codecs in `libnvencvt.dylib` are built from `vendor/nvenc` (mickys/nvenc, MIT) by `build/build_nvencvt.sh`. The
package puts the library beside the plugin in `NVMTLDriver.bundle/Contents/MacOS/`; the plugin loads it only when it and
its folder are root-owned. `NVMTL_NVENCVT` in `nvrm610.conf` decides where it registers: unset is automatic on Ada and
Ampere, `0` is never, `1` is every process on any card the library accepts, and a list of process names limits it to those.
The GPU's boost policy is `NVRM_610_BOOST`: unset is a short lease renewed while work is submitted, `hold` keeps one boost
for the device's life, `0` never boosts.
`NVRM_610_MAXPOWER_MW=<mW>` (off unless set) asks the GPU performance-scaling code for that maximum power through the public root-client GPS control and reads it back.
`NVRM_610_RATEDTDP` is off unless set: `l` holds the card at its rated TDP performance state (a power and clock cap), `k` locks it, `x` allows the turbo ceiling.

`package/stage_nvencvt.sh <pkgroot> <libnvencvt.dylib>` puts the video codec library into a package tree, signs it and refreshes `SHA256SUMS`; `install.sh` stops before changing anything if a package lacks it.
`tools/nvmtl-smi` (build with `tools/nvmtl-smi/build.sh <patched Mesa tree> <out>`) prints the GPU's temperature, power, clocks, pstate, utilisation, throttle reasons and fan from the resource manager's shared-data block.
The opt-in D3cold code is `kexts/NVRM/nv_rtd3.cpp` (`nvrtd3=1` and `nvrtd3res=<ACPI path>` on the boot line).

Kernel log volume: start-up, mode changes, sleep and wake, errors and warnings always log. The per-query lines of NVRMFB are on with the boot argument `nvfblog=1`, those of NVAccel with `-nvaccelverbose`. NVRM's lines are all boot, sleep and wake events. The plugin's log lines are rate-limited per call site.
Opt-in D3cold: `nvrtd3=1` and `nvrtd3res=<ACPI path of the GPU's power resource>` on the boot line. After 60 idle seconds (no framebuffer, no user client) the driver runs its sleep path and then the resource's `_OFF`; the first client or framebuffer runs `_ON` and then the wake path; system wake runs `_ON` before the resume.
`package/finish_package.sh <pkgroot> --nvencvt <lib> --air-dis <nvmtl-air-dis> --nmsmn <kext> <ryzenadj> <sha256> [--boost <bin>] [--vulkan-loader <dir>]` is the last stage of a package build: it stages those parts in one order, adds `sums_complete.sh`, and runs `package/check_package.sh <pkgroot>`, which fails if `libnvencvt.dylib` is missing, unsigned or not in `SHA256SUMS`, if the Ryzen power parts are missing, if any file in the tree has no line in `SHA256SUMS`, if a program in it is signed by Apple, or if a library in it loads from outside the system and the folders the package installs. `nvmtl-air-dis` (`plugin/air_dis.c`, built by `build/build_air_dis.sh`) is the plugin's out-of-process AIR decoder, on the same upstream LLVM as `air-runtime`; packages up to 2.8.1 carried the Metal toolchain's `air-opt` for this instead, which is Apple's program and is no longer shipped. `install.sh` runs the same completeness check (`sums_complete.sh`) right after `shasum -c`, because `shasum -c` reads only the files the list names.
