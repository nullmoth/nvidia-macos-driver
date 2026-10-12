# Real Macs (Mac Pro 2019, Mac Pro 2010-2012, Intel MacBooks with an eGPU)

Status: design (10-12). Nothing here ships yet.

## Why setup cannot "just skip OpenCore"
WindowServer is a platform binary. It maps our ad hoc signed Metal plugin only when AMFIPass (a Lilu plugin) is loaded:
with the same SIP bits and `amfi` boot arguments but no AMFIPass, 1.9.0 logs showed "mapping process is a platform
binary, but mapped file is not", no Metal device and a black screen. Lilu and AMFIPass are injected by OpenCore, so a
Mac without OpenCore cannot load the driver's userland, whatever NVRAM says.

## The route: a minimal OpenCore on the Mac's own EFI partition (what OpenCore Legacy Patcher does for real Macs)
No model spoofing (the Mac keeps its own SMBIOS), only:
- `Kernel > Add`: Lilu, AMFIPass (pinned files setup already fetches and checks)
- `NVRAM > Add`: `boot-args` with the driver's arguments, `csr-active-config` with the SIP bits the driver needs
- `Misc > Security > SecureBootModel Disabled` (Apple Secure Boot refuses kexts Apple did not sign)
- the boot picker tool (Start once without NVIDIA driver / Remove NVIDIA driver)
Everything after that is today's setup, unchanged: it finds this OpenCore as "the one that started the Mac".

## Per Mac
| Mac | Firmware | Extra user step | Card path |
|---|---|---|---|
| Mac Pro 2019 (MacPro7,1) | T2 | Recovery > Startup Security Utility: No Security, allow kexts; `csrutil disable` | PCIe/MPX slot (needs the auxiliary power cables for big cards) |
| Mac Pro 2010-2012 (MacPro5,1) | no T2, runs OCLP already | none beyond OCLP | PCIe x16; boot screen only on cards with a Mac EFI ROM (none for RTX) -> the picker is blind until macOS loads |
| Mac Pro 2013 (MacPro6,1) | no T2, OCLP for macOS 15 | none beyond OCLP | Thunderbolt 2 only -> eGPU path |
| Intel MacBook Pro / iMac / mini 2018-2020 | T2 | as Mac Pro 2019 | Thunderbolt 3 eGPU |

## eGPU (Thunderbolt) work in the driver
- NVRM must attach to a GPU behind a Thunderbolt bridge: check probe matching does not require a root-port parent.
- BAR1 behind Thunderbolt is usually 256 MB (no Resizable BAR): the small-BAR path from 1.14 applies.
- Hot unplug: RM must survive surprise removal (NVRM's fallen-off-the-bus path; no flips into a gone device).
- 10-11 report (leetftw, iGPU + eGPU RTX 3070, NM-PMDXQX4Y / NM-H364M4RV): NVRM claimed the card, but BAR1 behind
  Thunderbolt was 32 MB and 0 heads published. The kernel log shows the general limit: NVRMFB maps EVERY VRAM
  allocation through BAR1, and once that window is full each nvAllocVram is "REFUSED - BAR1 budget spent" (192 MB budget
  exhausted on that machine's other boot).
- **The change that unlocks eGPUs and every no-ReBAR board: VRAM without a BAR1 mapping.** GPU-only allocations
  (render targets, textures the CPU never touches, NVK's device-local heaps) get VRAM with no CPU mapping; only
  CPU-visible memory (staging, the shared pairs, scanout reads by the CPU copy path) spends BAR1. NVK already separates
  DEVICE_LOCAL from HOST_VISIBLE memory types, so the userland mostly asks correctly; the kernel ABI (nvAllocVram /
  NVRMVramRequest) needs a "no CPU mapping" flag, and the accelerator's paths that read `fKva` must handle its absence.
  Test machine: any desktop with Resizable BAR turned off in the BIOS shows the same refusals (no new hardware needed);
  the Thunderbolt case needs an enclosure.

## What blocks testing
- A real Mac to try it on: the available MacPro6,1 with OCLP has only Thunderbolt 2 and authorization is needed before
  rebooting it; a T2 Mac or a 2019 Mac Pro has to be borrowed or bought.
- An NVIDIA card in a Thunderbolt enclosure for the eGPU path.
