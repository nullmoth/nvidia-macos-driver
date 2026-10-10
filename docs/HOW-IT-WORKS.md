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
GeForce RTX GPU  (GSP firmware from /Users/Shared/nvfw/nvidia/610.57.04)
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
3. on its own thread, loads the GSP firmware from `/Users/Shared/nvfw/nvidia/610.57.04/` and boots the GPU with
   `rm_init_adapter()`;
4. brings up NVKMS (NVIDIA's mode-setting) and releases the hold once the display is armed.

The boot argument `-nvoff` makes NVRM leave the card alone. The recovery path below uses it.

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
on macOS against NVRM instead of the Linux DRM interface (`nvk/nvk-macos.patch` on Mesa `17ca6174`).

**NVIDIAShared.bundle** carries NVIDIA's NVVM compiler libraries and `air2nvvm.py`, the AIR to NVVM path. The
plugin's MPS convolution fast path is `plugin/nvconv.metal`.

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

Installing macOS itself needs different values. The macOS installer has no NVIDIA driver, so it runs on the firmware's
screen, which only survives macOS's PCI setup with a small BAR: `ResizeAppleGpuBars = 0`, `ResizeGpuBars = -1`, and
`IONDRVSupport` not excluded. 1401 builds the installer that way. The 1401 Mac app switches to the table above when it
installs the driver.

## 5. The 1401 Mac app (installs the driver)

`app/` builds **1401.app** (Swift + WebKit). The window is HTML/JS (`app/Resources/`). Every change to the system is
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
`NullMothSafe.efi` (`app/efi-safe/`, Rust UEFI). It adds `-nvoff` to the boot arguments for the next start and sets the
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

See the README, *Build from source*. The kexts need NVIDIA's `open-gpu-kernel-modules` at tag `610.57.04`; NVK needs
Mesa `17ca6174` with `nvk/nvk-macos.patch`.
