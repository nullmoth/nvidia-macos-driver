# NVIDIA native NVK driver

Complete driver for Intel **Sequoia 15.5+ and Tahoe 26**, including native
Zink OpenGL, Minecraft/Wine runtimes and additional video codecs. Normal OpenGL
apps use Zink automatically; Minecraft does not need the companion mod for
this path. OBS retains its automatic SDR capture compatibility mode.

Keep your previous working driver archive. Existing OpenCore/SIP/AMFI setup
requirements still apply; see the [OpenCore setup guide](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/OPENCORE-SETUP.md).

## Install

Double-click `Install.command`, or run from this folder:

```sh
sudo ./install.sh
```

Follow the approval and reboot instructions. Optional Intel laptop panel
support defaults to **No**; an existing opt-in is retained. See
[the Intel panel guide](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/INTEL-PANEL.md) before enabling it.

Native MetalFX DLSS has its own installer prompt, default **No**. It downloads
NVIDIA files and compiles kernels on the target Mac. Wine/CrossOver DLSS uses
the separate Wine/NVK setup and the game's `nvngx_dlss.dll`.

The optional Safari WebGL helper offers **Manual**, **Automatic**, or **Skip**.
Manual is the default and prints the bundled kext's location and OpenCore setup
steps. Automatic detects EFI partitions and selects a unique compatible OpenCore
installation; multiple candidates require your choice. It backs up changes first.
See [Safari WebGL setup and recovery](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/SAFARI-WEBGL.md).

With a compatible NVIDIA driver already running, `Verify.command` checks
the package and runs GPU/OpenGL probes. Diagnostic windows briefly appear.
These checks do not establish that a new package boots successfully.

## Setup guides

- [Minecraft native Vulkan](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/nvk/native-macos/INSTALL.md)
- [MPV and other Vulkan applications](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/nvk/native-macos/APPLICATIONS.md)
- [Wine / CrossOver, DXVK, D3D12 and DLSS](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/nvk/wine-macos/INSTALL.md)
- [Optional MetalFX DLSS](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/METALFX-DLSS.md)
- [Optional Activity Monitor GPU usage (off by default)](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/GPU-UTILIZATION.md)
- [Video codecs](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/video/INSTALL.md)
- [Zink OpenGL and OBS compatibility](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/ZINK-OPENGL.md)

`PACKAGE-IDENTITY.json` identifies this package. Detailed build and validation
records are retained with the build output, outside the installable archive.

## Rollback

Reinstall the previous complete driver package and reboot.
`Uninstall.command` removes the entire NVIDIA driver.
Disable/remove the optional OpenCore WebGL helper from EFI separately.

## Recovery removal

If the desktop cannot start:

1. Boot macOS Recovery, open Terminal, run
   `nvram "7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove=1"`, then reboot.
2. Run `sudo ./uninstall.sh` from the extracted `pkgroot`.
3. Before rebooting again, clear the flag with
   `nvram -d "7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove=1"`.
4. Reinstall the previous working driver package.

Optional HD Audio on Tahoe defaults to **No**, separately from the Intel panel.
Use `sudo bash install.sh --audio=yes` to opt in or `--audio=no` to remove audio
previously installed by this fork. Independent audio installations are preserved.
See [optional audio](https://github.com/HighDelay/nvidia-macos-driver-nvk/blob/main/docs/OPTIONAL-AUDIO.md).
