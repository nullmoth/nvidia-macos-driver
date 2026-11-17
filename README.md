# NullMoth NVIDIA Driver for macOS — NVK fork

Metal driver with native Zink OpenGL and Vulkan runtimes for **Intel macOS Sequoia 15.5+
and Tahoe 26**, with **NVIDIA Turing or newer** GPUs (GTX 16, RTX 20–50,
Quadro RTX). Based on [NullMoth's driver](https://github.com/nullmoth/nvidia-macos-driver).

**[Driver downloads](https://github.com/HighDelay/nvidia-macos-driver-nvk/releases)** ·
[Card support](docs/CARD-SUPPORT.md) · [Troubleshooting](docs/TROUBLESHOOTING.md)

## Install — by hand

1. Configure [OpenCore](docs/OPENCORE-SETUP.md): required boot arguments, SIP,
   SecureBootModel, Lilu/AMFIPass and BIOS settings are listed there.
2. Download this fork's driver archive and extract its `pkgroot/` folder.
3. Run:

```sh
cd ~/Downloads/pkgroot
shasum -a 256 --strict -c SHA256SUMS --quiet
sudo ./install.sh
sudo shutdown -r now
```

Double-clicking `Install.command` is an alternative to running `install.sh`.
Follow any extension-approval instructions in **Privacy & Security** and reboot
when requested. The installer backs up the previous driver and rebuilds the
kernel collection. Do not also inject these kexts through `EFI/OC/Kexts`.

`Verify.command` checks the complete package and runs GPU/CGL diagnostics.
See the [package guide](docs/COMBINED-INSTALL.md).

The installer offers experimental **Intel laptop panel support** (`NMIntelFB`),
default **No**, for internal screens wired to an unsupported Intel GPU. Existing
opt-ins survive updates. See [when to enable it](docs/INTEL-PANEL.md).

Experimental [notebook TGP support](docs/NOTEBOOK-TGP.md) is available with
`nvrmtgp=1`; it stays off by default and uses the laptop firmware's power budget.

Optional [NMXAudio HD Audio](docs/OPTIONAL-AUDIO.md) is offered separately on
Tahoe, also defaulting to **No**. Existing independent audio drivers are preserved.

## 1401 Mac app

Download the upstream [1401 Mac app](https://github.com/nullmoth/1401/releases),
open its disk image and run **1401**, then follow its installer. It automates OpenCore
configuration, driver installation and boot-menu recovery. Keep the OpenCore
boot disk connected while using it. This fork's Zink and native gaming runtimes
require **this fork's package**, installed using the method above.

For ARM-only apps without 1401, use the [separate ARM scripts](tools/arm-apps/README.md).
They install the independent translator and leave the GPU driver/OpenCore alone.

## Uninstall / recovery

```sh
cd ~/Downloads/pkgroot
sudo ./uninstall.sh
sudo shutdown -r now
```

If macOS cannot start, follow the [Recovery removal steps](docs/COMBINED-INSTALL.md#recovery-removal)
using `nullmoth-remove=1`, then reinstall your previous working package.

## Minecraft native Vulkan

[Aetherium — optional Vulkan shader mod](https://github.com/HighDelay/aetherium)

| Renderer | Application shader path |
| --- | --- |
| Apple OpenGL | OpenGL → Apple GL-on-Metal → NVMTL → NVK → GPU |
| Zink OpenGL | OpenGL → Mesa/Zink → native NVK → GPU |
| Vulkan / MoltenVK | Vulkan → MoltenVK → Metal → NVMTL → NVK → GPU |
| Vulkan / NVK Bridge | Vulkan → native NVK → GPU |

The native paths bypass Metal shader translation. Presentation still uses GPU
copies into Metal drawables and macOS composition; no controlled FPS gain is claimed.

<a id="native-opengl-with-zink"></a>
<a id="opengl-through-zink-tahoe-only"></a>

### OpenGL through Zink

Install the Zink-enabled package for **Sequoia 15.5+ or Tahoe**, then launch
Minecraft with **OpenGL**. No companion mod, mod loader or JVM argument is
needed. F3/logs should show **zink** and **NVK**; Minecraft's VSync toggle works.

Minecraft gameplay is verified on Tahoe and Sequoia 15.8.1 / RTX 2070 SUPER
with 256 MiB BAR1, including the small-BAR crash and black-window repair.
[Compatibility, builds and test results](docs/ZINK-OPENGL.md).

### Official Vulkan renderer with NVK Bridge

Requires **Minecraft 26.3** and **Intel Java 25**. Vanilla Minecraft can select
the native provider with one saved JVM argument; no companion mod is required.

1. Install this fork's driver with its Minecraft runtime and reboot.
2. Append this to the launcher's **JVM arguments**:

   ```text
   -Dorg.lwjgl.vulkan.libname=/Library/GPUBundles/nvmtl/minecraft/lib/libnvk_minecraft.dylib
   ```

3. Select **Vulkan**, restart Minecraft, and check F3 for **NVK Mesa**.

[Prism/Minecraft Launcher instructions, migration and rollback](nvk/native-macos/INSTALL.md).
Fabric/NeoForge are optional for other mods. Minecraft's default macOS library
selection still prefers MoltenVK when the argument is absent.

## Other Vulkan applications

MPV and applications with custom Vulkan-provider selection can use the same bridge.
See the [MPV and custom-library guide](nvk/native-macos/APPLICATIONS.md), including
the required loader-compatible runtime update after `v2.7.1-nvk`.

## Wine / CrossOver native Vulkan

The driver bundles native NVK, **DXVK for D3D11**, **vkd3d-proton for D3D12**,
DLSS Super Resolution support and an FPS HUD. This bypasses MoltenVK and NVMTL's
shader translation; Wine and DXVK/vkd3d remain.

**[Start with the complete Wine / CrossOver setup guide](nvk/wine-macos/INSTALL.md)**
for a compatible Wine build, prefix/bottle creation, DXVK installation and launch
commands. D3D12/DLSS are optional steps **after** that setup. The provider lives at
`/Library/GPUBundles/nvmtl/wine`; Wine itself is installed separately.

For **ZZZ, Genshin Impact or Honkai: Star Rail**, use
[YAAGL NVK](https://github.com/HighDelay/yaagl-nvk/releases) and its
[setup guide](https://github.com/HighDelay/yaagl-nvk/blob/main/docs/NVK.md).
It downloads Wine and uses this driver's installed graphics runtime.
ZZZ/HSR Global gameplay, GI in-game rendering and ZZZ D3D12/DLSS SR are verified.
For GI, enable **Steam Patch** and **Timeout fix**. Commercial CrossOver, DLSSG
and ray tracing remain unqualified.

## OBS display capture

OBS compatibility is **enabled automatically for OBS only**: Apple's OpenGL
renderer plus **8-bit BGRA SDR capture**. Other apps continue using Zink.
Select your display in **macOS Screen Capture**.
[Details and opt-out](docs/ZINK-OPENGL.md#obs-display-capture-compatibility).

## Activity Monitor GPU usage

Per-process **GPU %** and **GPU Time** for Metal applications are optional and
**off by default**. [Enable or disable accounting](docs/GPU-UTILIZATION.md#per-process-metal-accounting-experimental).

## Additional video codecs

The codec addon adds **VP8/VP9 hardware decoding on RTX 20**, retaining the
existing H.264/HEVC encoders. VP9 supports 8-bit and 10-bit VideoToolbox output.
VP8/VP9 encoding uses software; Turing has no AV1 hardware support.
Existing Ada/Ampere codec paths are retained.
[Setup, application limits and builds](video/INSTALL.md).

## Build from source

Requires Xcode command line tools, Rust (stable), Meson/Ninja, and NVIDIA's
`open-gpu-kernel-modules` at **620.70.06** for the kernel components. NVK uses
**610.57.04** headers with the recorded NVOS compatibility patch.

```sh
build/build_xlate.sh
RELEASE=1 build/build_plugin.sh
build/build_nvk.sh /path/to/mesa-git /path/to/ogkm-610.57.04 /path/to/nvk-output
build/accel_build.sh <src> <out>
build/accel_build_all.sh kexts/NVRM <out>
```

These build the translator, Metal plugin, NVK, kexts and both OS accelerators.
The NVK builder exports official Mesa **26.3.0** from `nvk/MESA_BASE` and applies
the complete 620-compatible patch chain, including matched native compiler tools.
[Mesa update, dependencies and validation limits](docs/MESA-26.3.0-UPDATE.md).
Rebuild **both accelerators** whenever `nvrm_vram_abi.h` or `nvrm_gpuva_abi.h`
changes. [Zink build and packaging instructions](docs/ZINK-OPENGL.md#building-locally).

[Architecture](docs/HOW-IT-WORKS.md) · [Display rotation](docs/DISPLAY-ROTATION.md) ·
[Contributing](CONTRIBUTING.md) · [Upstream support](https://buymeacoffee.com/nullmoth)

Driver code: [PolyForm Noncommercial](LICENSE); translator: [LGPL-3.0](translator/LICENSE);
Mesa/NVK/Zink: MIT; firmware carries NVIDIA's license. See [NOTICE](NOTICE).
