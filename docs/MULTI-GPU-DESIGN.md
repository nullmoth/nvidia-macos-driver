# Multi-GPU acceleration: what it takes, measured on this tree

Today one NVIDIA card is accelerated per boot. [AUTO-PRIMARY-GPU.md](AUTO-PRIMARY-GPU.md) picks which one, and the others stay
visible on the PCI bus. This page is the inventory of what stands between that and one Metal device per card, so the work can
be done in slices that each keep the single-card path unchanged. It describes the code as of 2.9; none of it is built.

## Why it is not a small change

The accelerator, the display side and the resource-manager glue all assume exactly one GPU.

| Layer | Single-card assumption | Measured |
|---|---|---|
| NVRM service | one global service object returned by `nv_xnu_gpu_state()` and `nv_xnu_gpu_os_device()` (`kexts/NVRM/NVRM.cpp`, `gNVRMService`) | 4 uses of the global; 19 file-level statics in `NVRM.cpp`; the two accessors have 7 callers in `os-nvkms-xnu.cpp`, `os-xnu2.cpp`, `nvrm_ddc.cpp` |
| NVKMS glue | `nvkms_open_gpu` and the GPU list are built from `gpu_info[0]` only (`kexts/NVRM/os-nvkms-xnu.cpp`) | 11 file-level statics; the GPU table has one slot |
| NVAccel | one global accelerator object (`NVAccel::gAccel`) read by every allocation, surface and framebuffer path | 34 uses of `gAccel` in `kexts/NVRM/accel/nvrm-accel.cpp`, 64 file-level statics |
| NVRMFB | one framebuffer family bound to that accelerator | 20 file-level statics in `kexts/NVRM/fb/nvrm-fb.cpp` |
| Plugin | the device is found with a first-match registry lookup (`IOServiceGetMatchingService`, 3 sites in `plugin/`) and the process creates one Metal device | `MTLCopyAllDevices` can list only what the plugin creates |

## Slices, each provable before the next

1. **Device table, one entry.** Replace `gNVRMService` with a table indexed by device, with a "primary" entry that is whatever
   AUTO-PRIMARY-GPU picked (or the only card). `nv_xnu_gpu_state()` returns the primary. With one card the call returns the same
   pointer as before, so the test is: for one registered device every accessor returns what the old global returned; for two, the
   primary does not change when the second registers or leaves. Host-testable, no GPU.
2. **Per-device accelerator.** Same treatment for `gAccel`: `NVAccel::forDevice(i)` plus `primary()`; the 34 uses are converted to
   take the device from the object they already hold (surfaces, user clients and memory objects know their accelerator) and only
   fall back to `primary()` where there is no context.
3. **NVKMS table with N slots**, display bound to the card that owns the console; the other cards expose compute and render only.
4. **Matching.** Drop the primary-selector requirement on NVAccel's personality behind `nvmulti=1` (default off).
5. **Plugin.** One Metal device per accelerator; `MTLCopyAllDevices` lists each; the default device stays the primary; the second
   card is opt-in until measured.

## What is needed to prove it

Slices 1 and 2 can be proven on the host (table behaviour, equivalence for one card) and by kext builds for macOS 15 and 26 with
no new warnings. Slices 3 to 5 need a machine with two NVIDIA cards: boot, WindowServer on the primary, a Metal workload on the
second. The earlier four-card report (MULTI-GPU.md) shows that unrestricted matching produced overlapping start-up attempts and
a display failure, so slice 4 has to be measured before it is allowed to default on.
