# NullMoth NVIDIA Driver for macOS

A Metal driver for NVIDIA GeForce cards on macOS 15 Sequoia (Intel Macs and OpenCore PCs). It is open source; the
kernel extensions, the Metal plugin and the shader translator are in this repository. The 1401 app that installs it
(Mac and Windows) is its own project: [1401](https://github.com/nullmoth/1401).

Supported cards are Turing and newer: GTX 16, RTX 20, 30, 40 and 50, TITAN RTX, Quadro RTX and the T-series. Most
testing is done on an RTX 5060, so other cards may have problems that haven't been found yet. See
[card support](docs/CARD-SUPPORT.md) for what has been reported working.

- Changes: [CHANGELOG.md](CHANGELOG.md), downloads on the [releases page](https://github.com/nullmoth/nvidia-macos-driver/releases)
- Problems: [troubleshooting](docs/TROUBLESHOOTING.md), then Send logs from the 1401 app
- Installing macOS from Windows: [1401](https://github.com/nullmoth/1401)
- How it works: [docs/HOW-IT-WORKS.md](docs/HOW-IT-WORKS.md)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)

If your PC doesn't boot macOS with OpenCore yet, set that up first with the
[OpenCore Install Guide](https://dortania.github.io/OpenCore-Install-Guide/), then install the driver.

Support the work: https://buymeacoffee.com/nullmoth

| | |
|---|---|
| macOS | 15.5 Sequoia or later (tested 15.7.x and 15.8.1), or 26 Tahoe, x86_64 |
| GPUs | NVIDIA Turing and later; developed on an RTX 5060 |
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

## Install — by hand

Use the 1401 Mac app below unless you manage your OpenCore config yourself. `install.sh` installs the driver files but
does not change OpenCore, and the driver does not start until OpenCore has these settings:

- `boot-args` include `nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80
  ipc_control_port_options=0`, and not `nv_disable=1` or `-wegnoegpu` (both hide the NVIDIA card)
- `csr-active-config` includes the bits `0x0A43`
- `Misc > Security > SecureBootModel` is `Disabled`
- `Lilu.kext` and `AMFIPass.kext` are in `Kernel > Add` and enabled

Download `nullmoth-nvidia-<version>.tar.gz` from **Releases**, then:

```bash
tar -xzf nullmoth-nvidia-*.tar.gz && cd pkgroot
shasum -a 256 -c SHA256SUMS
sudo ./install.sh
sudo shutdown -r now
```

Every file must say OK in the check. `install.sh` copies the files and rebuilds the Auxiliary Kernel Collection; the
restart is required, because logging out does not load the driver. (Paste the lines without anything after them: the
Terminal on macOS does not treat `#` as a comment, so a pasted comment becomes part of the command.)

macOS asks you to **allow the extensions** in System Settings → Privacy & Security the first time. Allow, then reboot again.

## 1401 Mac app (recommended)

Download the latest `1401-Mac-<version>.dmg` from **Releases**, open it, and run **1401** (the driver package is inside the disk
image, so nothing else to download). Follow its steps. A supported setup must use a verified or explicitly selected OpenCore partition. The app finds the OpenCore that started your Mac (in `EFI/OC` or `EFI/BOOT`, on an
EFI or FAT32 partition), shows every change before making it, backs the config up, installs the driver, and adds
**1401: Remove NVIDIA driver** to the OpenCore boot picker. Choosing that entry removes the driver at the next start and
puts the Mac back exactly as it was before the install, OpenCore config included, then restarts by itself. The app also
maps your USB ports and, if the driver ever crashes the Mac, offers to make a crash report you can upload yourself
(it never sends anything on its own).

If something goes wrong, open 1401 > Crash report > Send logs to NullMoth. It uploads what 1401 did, the driver's
state, crash reports and OpenCore's startup log, with names, serial numbers and addresses removed, and gives you a
report ID to put in your issue or in Discord.

Keep the USB stick or disk that OpenCore started from plugged in while the app runs, and for every restart after:
that is the config the app changes. The app never copies OpenCore to your internal disk. The first start with the
driver pauses for up to a minute at "PCI configuration end" while the GPU comes up.

## Wiring into an existing OpenCore setup

The kexts install into `/Library/Extensions` and load from the Auxiliary Kernel Collection — **do not** also inject
them from `EFI/OC/Kexts`. OpenCore only needs to set SIP and boot-args.

**`NVRAM → Add → 7C436110-AB2A-4BBB-A880-FE41995C9F82`**

| Key | Value | Why |
|---|---|---|
| `csr-active-config` | `<430A0000>` (Data) | the tested value: unsigned kexts, plus what root patches need |
| `boot-args` | `nvfb=1 nvaccel=1 nvfbheads=4 -nvkmsnosmooth amfi_get_out_of_my_way=0x1 amfi=0x80 ipc_control_port_options=0` | framebuffer + accelerator, 4 display heads; the AMFI args let WindowServer load the driver bundle |

Optional boot arguments, for troubleshooting:

| boot-arg | Effect |
|---|---|
| `-nvrmnoflip` | keeps the composited frame on the copy path: NVRM never turns on zero-copy scan-out (`debug.nvaccel_iop_flip`), so the display reads a finished copy instead of the surface the GPU may still be drawing. Try it first when the desktop flashes or tears while the cursor moves. |
| `-nvrmnogo` | the driver idles until `nvrmctl go <pass>` instead of starting the GPU itself. |
| `-nvrmnobootscreen` | do not snapshot the boot screen. |
| `-nvrmnopm` | turns off system sleep support: NVRM stays out of the power tree, as before it had one. A Mac that sleeps then comes back without the GPU. See [docs/SLEEP-WAKE.md](docs/SLEEP-WAKE.md). |

Add every key you set to `NVRAM → Delete` as well, so the values are rewritten each boot.

| Setting | Value | Why |
|---|---|---|
| `UEFI → Quirks → ResizeGpuBars` | keep the existing value | Forced resizing can stop firmware before the picker; `-1` preserves firmware sizing |
| `Booter → Quirks → ResizeAppleGpuBars` | keep the existing value | Large firmware BARs can stop PCI configuration; `0` is the installer-safe small-BAR setting |
| `Kernel → Block` | `com.apple.iokit.IONDRVSupport`, Strategy `Exclude` | otherwise the firmware framebuffer takes display index 0 from NVRMFB |
| `Misc → Security → SecureBootModel` | `Disabled` | Apple Secure Boot refuses kexts Apple did not sign |

The macOS **installer** uses `ResizeAppleGpuBars` `0`, `ResizeGpuBars` `-1`, and IONDRVSupport
not excluded: it runs on the firmware framebuffer. The Mac app keeps your existing BAR settings when it installs or
updates the driver, and the driver sets up the BAR itself after macOS starts, so don't copy BAR values from someone
else's board.

Also required:
- **BIOS:** Above 4G Decoding ON (the card maps memory above 4 GB), CSM OFF.
- **SMBIOS:** a Mac model that runs macOS 15 with a discrete GPU (tested: `iMacPro1,1`, which 1401 uses).
- **Remove** any `nv_disable=1`, WhateverGreen NVIDIA patches, or `agdpmod=pikera` for this card.
- **With WhateverGreen installed** (for an Intel or AMD GPU), add `gfxrst=4` to boot-args. Without it, WhateverGreen's
  boot screen copy can read freed memory when the NVIDIA display opens, and the Mac panics.
- **No `DeviceProperties`** are needed for the NVIDIA card.

Verify after the reboot:

```bash
kmutil showloaded --list-only | grep nullmoth
system_profiler SPDisplaysDataType | head -20
```

The first shows 4 lines; the second shows your GeForce with Metal: supported.

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
build/build_xlate.sh
RELEASE=1 build/build_plugin.sh
build/build263.sh
build/accel_build.sh <src> <out>
build/accel_build_all.sh kexts/NVRM <out>
```

In order: the translator (`libnvmtl_translate.dylib`); the plugin (`NVMTLDriver.bundle`; `RELEASE=1` strips every
diagnostic); NVK (apply `nvk/nvk-macos.patch` to Mesa `17ca6174` first); the kexts; and NVAccel for macOS 15 and 26
(`NM_TAHOE=1`), written to `<out>/15` and `<out>/26`.

Rebuild both accelerators whenever `nvrm_vram_abi.h` or `nvrm_gpuva_abi.h` changes: an accelerator built against an
older ABI loads, but NVRMFB refuses all its VRAM requests and the desktop stays black.

## License

Free of charge, source code included. Nobody may sell it or use it to make money.

- `plugin/`, `kexts/`, `build/`, `package/`: PolyForm Noncommercial 1.0.0 (see `LICENSE`), © NullMoth Systems.
  Personal, research, educational and non-profit use is allowed; any commercial use is not.
- `translator/`: LGPL-3.0-or-later (see `translator/LICENSE`): it is based on metal2vulkan, whose licence carries over.
- `nvk/`: changes to Mesa (MIT).
- GSP firmware: NVIDIA's redistributable firmware licence.

See `NOTICE` for third-party credits.
