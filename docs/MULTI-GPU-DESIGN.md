# Multi-GPU acceleration: what it takes, measured on this tree

Today one NVIDIA card is accelerated per boot. [AUTO-PRIMARY-GPU.md](AUTO-PRIMARY-GPU.md) picks which one, and the others stay
visible on the PCI bus. This page is the inventory of what stands between that and one Metal device per card, so the work can
be done in slices that each keep the single-card path unchanged. Status after the 2.9 work is in the last section.

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

## Status in 2.9

Everything below is off unless the boot argument `nvmultigpu=1` is present. Without it, one card and several cards run the code
they ran before.

| Slice | State |
|---|---|
| 1 NVRM device table | built: `kexts/NVRM/nvrm_devtab.h`; `nv_xnu_gpu_state()` and `nv_xnu_gpu_os_device()` return the primary, `nv_xnu_gpu_state_at(i)`, `nv_xnu_gpu_os_device_at(i)`, `nv_xnu_gpu_count()` enumerate. |
| 2 NVAccel per-object resolution | built: every object the accelerator makes (video memory, resource, memory map, display pipe) holds its accelerator and uses `NVAccel::resolve(fAccel)`; RM handles, the NVRM service lookup and the accelerator lock owner are per accelerator. Display, scanout and flip code uses `NVAccel::forDisplay()`, the primary, because the primary is the only card with a display. `tools/check_accel_resolution.py` fails on any other read of the global (15 arms fail on the pre-change source). |
| 3 NVKMS | NVKMS binds the primary only; no change was needed. |
| 4 Matching and re-offer | built: a one-at-a-time start gate (atomic) with the unmarked cards waiting for the marked one; a refused card is queued and, when a start finishes, the oldest waiting card is offered again with `registerService(kIOServiceAsynchronous)`, at most 3 times per card. Sequencing is proven on the host (four cards offered in the worst order start 0, 3, 2, 1 with never more than one in flight). |
| 5 Plugin | built as card binding, not as several cards in one process: see below. |

### Slice 5: one card per process

The plugin's Vulkan layer (`nvmtl_vk.c`) is one device per process: 184 file-level globals, about 500 call sites in the Metal
object code, 371 uses of the instance, device and physical-device globals. Making that a context passed to every call cannot
be proven without a second card, so the plugin keeps one device per process and chooses which card:

- the kernel publishes `NVAccelPrimary` on every accelerator;
- the plugin (`nvmtl_card_wanted`, rules in `plugin/nvmtl_card_select.h`) accepts the accelerator the user assigned to this
  executable (`NVMTL_CARD` in the conf: `1`, or `Game=1,*=0`; 0 is the primary, 1 the second in registry order), else the
  primary, and refuses the others, so `MTLCreateSystemDefaultDevice` is the primary card and an assigned application gets its card;
- it hands the choice to NVK as `NVK_ACCEL_ID` (the registry entry id), which `nvk/nvk-card-select.patch` makes the RM
  connection open, and never falls back to another card;
- the accelerator's user client already comes from the NVRM that attached to the same PCI device (`NVAccel::locateNVRM`).

So each card is a Metal device in the processes that use it, and `MTLCopyAllDevices` in a process lists the card that process is
bound to; one process does not drive two cards at once.

### Proof

- `tools/test_nvrm_devtab.cpp` (host, 26 arms): table and gate rules against a model of the old code, the four-card overlapping
  start, the re-offer sequencing, `resolve()` for one and two cards. Two mutants of the header fail it.
- `tools/check_accel_resolution.py`, `tools/test_card_select.c` (11 arms), `tools/test_multigpu_wiring.py` (8 arms).
- Builds on the mini: NVRM 43 warnings before and after; NVAccel 24 and 24 for macOS 15 and 26; the plugin 330 and 330; NVK with
  both window and card patches, 2274 exports, the same set as before.
- Not proven, because it needs two cards: that IOKit offers a re-registered card again exactly as modelled, and a real second
  card's Metal and Vulkan work.
