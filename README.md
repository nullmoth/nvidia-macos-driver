# NullMoth NVIDIA Driver for macOS

A Metal driver for NVIDIA Turing-and-later cards on Intel Macs and OpenCore systems running **macOS 15 Sequoia**.
The device table includes GTX 16, RTX 20/30/40/50, TITAN RTX, and supported Quadro/RTX workstation cards.
The driver implements NVIDIA-backed display and Metal interfaces. Feature availability and application behavior depend on the card, operating-system version and installed components; device-table coverage is not runtime qualification.

Made by **NullMoth Systems**.

**Latest update:** [1401 and NVIDIA driver 1.3.0](docs/RELEASE-1.3.0.md). See the changes, validation and qualification scope before updating.

Installing macOS from Windows? Use **1401**: https://github.com/nullmoth/1401

> **This driver is new and may not work on every PC.** It is tested on an RTX 5060 under macOS 15.7.x and 15.8.1. Device-table coverage is broader than physical hardware validation; see [card support](docs/CARD-SUPPORT.md). If your PC does
> not boot macOS with OpenCore yet, set that up first with the
> [OpenCore Install Guide](https://dortania.github.io/OpenCore-Install-Guide/)
> ([OpenCore releases](https://github.com/acidanthera/OpenCorePkg/releases)), then install the driver.
> The full write-up of how the driver works is in [`docs/HOW-IT-WORKS.md`](docs/HOW-IT-WORKS.md).

Support the work: https://buymeacoffee.com/nullmoth

| | |
|---|---|
| macOS | 15 Sequoia (tested 15.7.x and 15.8.1), x86_64 |
| GPUs | NVIDIA Turing and later from the GSP device table; physical validation: RTX 5060 |
| Metal | Metal 3: argument buffers tier 2, ray tracing, mesh shaders, MPS, MetalFX path |
| Also | OpenGL (through Apple's GL-on-Metal), OpenCL, Core Image, Core ML |

## How it works

```
Metal app ─► NVMTLDriver.bundle ─► translator (Apple AIR → SPIR-V) ─► NVK (Mesa Vulkan + NAK compiler) ─► kexts ─► GPU
```

| Component | Path on disk | Source |
|---|---|---|
| Metal driver plugin | `/Library/GPUBundles/NVMTLDriver.bundle` | `plugin/` |
| Shader translator | inside the plugin (`libnvmtl_translate.dylib`) | `translator/` (LGPL-3.0, based on metal2vulkan) |
| Vulkan back end (NVK) | `/Library/GPUBundles/nvmtl/` | `nvk/nvk-macos.patch` on Mesa `17ca6174` |
| Kernel extensions | `/Library/Extensions/NVRM, NVAccel, NVRMFB, NVRMAGDC` | `kexts/` |
| GPU firmware (GSP) | `/Users/Shared/nvfw/nvidia/610.57.04` | NVIDIA, unmodified |

The kernel side runs NVIDIA's own open GPU kernel modules (r610) under macOS. NVRMFB is the display framebuffer,
NVAccel the accelerator WindowServer composites through, NVRMAGDC the display-policy shim.

## Install — prebuilt (recommended)

Download `nullmoth-nvidia-<version>.tar.gz` from **Releases**, then:

```bash
tar -xzf nullmoth-nvidia-*.tar.gz && cd pkgroot
shasum -a 256 -c SHA256SUMS          # every file must say OK
sudo ./install.sh                    # copies the files, rebuilds the Auxiliary Kernel Collection
sudo shutdown -r now                 # a reboot is required: logout does not load the driver
```

macOS asks you to **allow the extensions** in System Settings → Privacy & Security the first time. Allow, then reboot again.

## 1401 Mac app (easiest)

Download the latest `1401-Mac-<version>.dmg` from **Releases**, open it, and run **1401** (the driver package is inside the disk
image, so nothing else to download). Follow its steps. A supported setup must use a verified or explicitly selected OpenCore partition. The app finds the OpenCore that started your Mac (in `EFI/OC` or `EFI/BOOT`, on an
EFI or FAT32 partition), shows every change before making it, backs the config up, installs the driver, and adds
**1401: Remove NVIDIA driver** to the OpenCore boot picker. Choosing that entry removes the driver at the next start and
puts the Mac back exactly as it was before the install, OpenCore config included, then restarts by itself. The app also
maps your USB ports and, if the driver ever crashes the Mac, offers to make a crash report you can upload yourself
(it never sends anything on its own).

**Something not working?** Open 1401 > Crash report > **Send logs to NullMoth**. It sends what 1401 did, the driver's
state, driver crash reports, recent WindowServer crash reports and OpenCore's startup logs (names, serial numbers and addresses removed), each
with a SHA-256 the site checks, and shows a report ID to quote in the NullMoth Discord.

**Keep the USB stick or disk OpenCore started your Mac from plugged in** while the app runs: that is the config it
changes. A shared SMBIOS model alone does not establish the startup partition. Automatic USB-to-internal copying is disabled, preserving Windows and vendor boot files. Driver installation uses the selected startup partition. Keep the OpenCore stick attached for every restart until the internal boot setup is reviewed. After the install,
restart; the first start with the driver pauses for up to a minute at "PCI configuration end" while the GPU comes up.

## Wiring into an existing OpenCore setup

The kexts install into `/Library/Extensions` and load from the Auxiliary Kernel Collection — **do not** also inject
them from `EFI/OC/Kexts`. OpenCore only needs to set SIP and boot-args.

**`NVRAM → Add → 7C436110-AB2A-4BBB-A880-FE41995C9F82`**

| Key | Value | Why |
|---|---|---|
| `csr-active-config` | `<430A0000>` (Data) | the tested value: unsigned kexts, plus what root patches need |
| `boot-args` | `nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80 ipc_control_port_options=0` | framebuffer + accelerator, 4 display heads; the AMFI args let WindowServer load the driver bundle |

Add every key you set to `NVRAM → Delete` as well, so the values are rewritten each boot.

| Setting | Value | Why |
|---|---|---|
| `UEFI → Quirks → ResizeGpuBars` | `13` | 8 GB BAR: full memory bandwidth (tested on the RTX 5060) |
| `Booter → Quirks → ResizeAppleGpuBars` | `-1` | macOS sees the full BAR |
| `Kernel → Block` | `com.apple.iokit.IONDRVSupport`, Strategy `Exclude` | otherwise the firmware framebuffer takes display index 0 from NVRMFB |
| `Misc → Security → SecureBootModel` | `Disabled` | Apple Secure Boot refuses kexts Apple did not sign |

The macOS **installer** needs the opposite BAR settings (`ResizeAppleGpuBars` `0`, `ResizeGpuBars` `-1`, IONDRVSupport
not excluded): it has no NVIDIA driver and runs on the firmware's screen. 1401 builds installers that way, and the 1401
Mac app switches to the values above when it installs the driver.

Also required:
- **BIOS:** Above 4G Decoding ON (the card maps memory above 4 GB), CSM OFF.
- **SMBIOS:** a Mac model that runs macOS 15 with a discrete GPU (tested: `iMacPro1,1`, which 1401 uses).
- **Remove** any `nv_disable=1`, WhateverGreen NVIDIA patches, or `agdpmod=pikera` for this card.
- **No `DeviceProperties`** are needed for the NVIDIA card.

Verify after the reboot:

```bash
kmutil showloaded --list-only | grep nullmoth      # 4 lines
system_profiler SPDisplaysDataType | head -20      # your GeForce, Metal: supported
```

## Dual boot

Nothing in the driver touches other disks. OpenCore's picker boots Windows (`ScanPolicy` 0 or including NTFS) and Linux
(`OpenLinuxBoot.efi` in `UEFI → Drivers`, `LauncherOption = Full`) alongside macOS.

## Uninstall

```bash
sudo ./uninstall.sh && sudo shutdown -r now
```

## Build from source

Requires Xcode 16, Rust (stable), Meson/Ninja, and NVIDIA's `open-gpu-kernel-modules` at tag `610.57.04`.

```bash
build/build_xlate.sh       # translator  -> libnvmtl_translate.dylib
RELEASE=1 build/build_plugin.sh   # plugin -> NVMTLDriver.bundle (RELEASE=1 strips every diagnostic)
build/build263.sh          # NVK: apply nvk/nvk-macos.patch to Mesa 17ca6174 first
build/accel_build.sh <src> <out>  # kexts
```

## License

Free of charge, source code included. Nobody may sell it or use it to make money.

- `plugin/`, `kexts/`, `build/`, `package/`: PolyForm Noncommercial 1.0.0 (see `LICENSE`), © NullMoth Systems.
  Personal, research, educational and non-profit use is allowed; any commercial use is not.
- `translator/`: LGPL-3.0-or-later (see `translator/LICENSE`): it is based on metal2vulkan, whose licence carries over.
- `nvk/`: changes to Mesa (MIT).
- GSP firmware: NVIDIA's redistributable firmware licence.

See `NOTICE` for third-party credits.
