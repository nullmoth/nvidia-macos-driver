# OpenCore configuration for the NVIDIA driver

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
| `-nvrmnopm` | turns off system sleep support: NVRM stays out of the power tree, as before it had one. A Mac that sleeps then comes back without the GPU. See [docs/SLEEP-WAKE.md](SLEEP-WAKE.md). |
| `nvrmtgp=1` | experimental laptop-only TGP budget on mains power, reset on battery; off by default. Requires battery detection and a mobile NVIDIA GPU. See [setup, telemetry and rollback](NOTEBOOK-TGP.md). |

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

