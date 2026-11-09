# What a real Mac has, and what a NullMoth Mac has

This is the 2.9 audit for PCs that run macOS 15 Sequoia or macOS 26 Tahoe through 1401 and the NullMoth driver (NVIDIA
GTX 16 and RTX 20 to 50; AMD and Intel graphics through the NMXGPU test driver; hybrid laptops). Each row names a
feature of a real Apple-silicon Mac or a real Intel Mac, what a NullMoth Mac has today, and the gap. NVIDIA's own
Windows features are in [WINDOWS-PARITY.md](WINDOWS-PARITY.md) and [WINDOWS-PARITY-MEDIA.md](WINDOWS-PARITY-MEDIA.md);
this page is about the Mac side.

How to read the status column:

| Status | Meaning |
|---|---|
| Have | in the released stack and expected to work; the file that provides it is named |
| Built, off | written and tested offline, behind a switch that is off until a Mac that has the hardware confirms it |
| Partial | some of it works; the missing part is named |
| Not built | buildable from public sources and not yet written; the next step is named |
| Blocked | cannot be made real on a PC; the reason is one of: needs Apple silicon, needs the T2 or a Secure Enclave, Apple's own gate, a licence, or a missing physical interface |

"Real Mac" statements come from Apple's published requirements and from what macOS itself reports on a real Mac; where a
requirement is not something we could confirm, the row says so. Nothing on this page is a promise about a specific
machine: most rows are proven offline and wait for a tester's Mac.

## Measuring it on your own Mac

`tools/mac_parity_probe.sh` prints one line per feature, taken from macOS's own reports (`system_profiler`, `fdesetup`,
`pmset`, `csrutil`). Run it on a real Mac and on a NullMoth Mac and compare: a row that is `yes` on the real Mac and `no`
on the NullMoth Mac is a gap, with the evidence beside it. `nullmoth-mac-parity.sh identity` (in the Mac app) checks the
serial number, the ROM against the primary network adapter, and en0 on the installed system.

The two limits that shape most rows:

1. A PC has no T2 chip and no Secure Enclave. Anything Apple gates on them (Touch ID, Apple Pay with the Secure
   Element, Activation Lock, offline Find My, iPhone Mirroring, hardware FairPlay 4K) cannot be made real.
2. Continuity (AirDrop, Handoff, Universal Control, Sidecar over Wi-Fi, AirPlay to Mac, Continuity Camera, iPhone
   Mirroring, Instant Hotspot, Auto Unlock) runs over Apple Wireless Direct Link, which macOS exposes as the `awdl0`
   interface. Only a Wi-Fi driver written for Apple's IO80211 family raises it. The features stand or fall with the Wi-Fi
   card and its driver, not with the GPU.

## Graphics and Metal

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| Metal 3 GPU family, argument buffers tier 2, mesh shaders, function pointers, barycentrics | all supported Macs | NVIDIA plugin (`plugin/`); features in `CHANGELOG.md` 2.7 to 2.8 | Have on NVIDIA. AMD and Intel: partial, NMXGPU test driver (`nmx/README.md`) |
| Metal 4 | the Macs Apple lists | the device reports the Metal 4 family and Metal 4 queues, buffers, encoders and compiler run over Metal 3 objects (`CHANGELOG.md` 2.8) | Have on NVIDIA |
| MetalFX temporal upscaling and frame interpolation | the Macs Apple lists | served by DLSS on RTX cards (`plugin/NVMTLMetalFXDLSS.m`, `dlss/`; Blackwell default, Turing, Ampere and Ada opt-in) | Partial. GTX 16 has no tensor cores, so there is no DLSS path there |
| MetalFX spatial upscaler on cards without DLSS | same | no source for it in `plugin/` | Not built. A compute-shader upscaler from a permissively licensed algorithm is buildable and needs a card to prove; tracked with the DLSS work |
| Ray tracing (Metal intersection and acceleration structures) | the Macs Apple lists | software path; the hardware path is the ray-tracing area's own BVH builder (`docs/WINDOWS-PARITY.md`, Gaming) | Partial |
| Game Mode | Apple lists Apple silicon Macs | NullMoth's own, because Apple's daemon on an Intel Mac says no (a fixed answer for Game Mode, and the kernel setting Game Mode relies on, `kern.console_mode`, is not on this PC's kernel). 1401's Tools tab has a Game Mode card; turning it on installs a root daemon (`/Library/NullMoth/gamemode`) and a menu-bar indicator. While a game (games category, or opted in to Game Mode) is in front and full screen the daemon gives it nice -5 and moves background indexing and analysis jobs to the background class; the plugin raises the game's own GPU channel group (interleave HIGH, a longer timeslice) and every other ordinary Metal client lowers its own (interleave LOW, a shorter timeslice) while the game is active; leaving full screen, quitting the game, or turning it off undoes it from a record | Built, off until the user turns it on. One state drives the CPU side, the GPU side and the indicator. Nothing of Apple's daemon is patched or its preferences changed. GPU changes are made when a client makes its Metal device, and an app that is already running is re-prioritised live: the NVK driver exports `nvmtl_nvk_tsg_reprioritise_v1`, which re-applies the interleave level and timeslice on that process's existing channel groups (and restores what they had), and the plugin calls it each time the Game Mode state changes (never for system processes, and never when the user set `NVRM_TSG_Q0_HIGH` or `NVRM_TSG_TIMESLICE_US` themselves). Unset `NVRM_TSG_Q0_HIGH` behaves exactly as before; `0` is the new interleave-LOW value (`tools/test_nvk_tsg_reprioritise.py`) Not run on a game, a hybrid laptop or an NVIDIA card |
| Game Porting Toolkit (D3DMetal) | Apple silicon only | none | Blocked: Apple distributes it for Apple silicon and the licence is Apple's. Wine with DXVK or MoltenVK over the Vulkan driver is separate and unproven |
| Multiple displays, hot-plug, high refresh, HiDPI | yes | `nvfbheads`, hot-plug re-read, `nvfbhidpi`, refresh above 60 Hz (`docs/WINDOWS-PARITY.md`, Display) | Have |
| HDR10 / EDR output | XDR and HDR displays | `nvfbhdr=1`, opt-in (`kexts/NVRMFB/fb/nvrm-fb.cpp`, `tools/test_fb_hdr.py`) | Built, off. Needs an HDR display to confirm |
| Display brightness (laptop panel) | yes | NVRMFB backlight path with persistence (`docs/NVRMFB-BACKLIGHT-POWER.md`); on a hybrid laptop the integrated GPU owns the panel | Have where the NVIDIA GPU drives the panel; hybrid: see Power |
| GPU switching (dual-GPU laptops) | automatic graphics switching | opt-in render offload with the integrated GPU keeping the panel (`docs/HYBRID-LAPTOPS.md`, `nmhybrid`) | Built, off. Needs a hybrid laptop |
| External GPU over Thunderbolt | Intel Macs | BAR bridge-window code for a card behind a downstream port (`docs/WINDOWS-PARITY.md`, Hardware coverage) | Partial |
| OpenGL, OpenCL, Core Image | yes | OpenGL over Metal (`plugin/NVMTLGL.m`); OpenCL in the compute area; Core Image on Metal | Have for OpenGL and Core Image |

## Media

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| H.264 and HEVC hardware decode | all | NVDEC decoders registered with VideoToolbox; Ada and Ampere also through the vendored NVEncVT (`docs/WINDOWS-PARITY-MEDIA.md`) | Have |
| AV1 and VP9 decode | M3 and later (AV1) | Ada and Ampere through NVEncVT; Blackwell needs its own decode class | Partial |
| H.264 and HEVC hardware encode through VideoToolbox | all | Turing and Ampere built-in libraries, Ada through NVEncVT, Blackwell built-in; `EnableLowLatencyRateControl` accepted by NVEncVT (`tools/test_nvencvt_lowlatency.py`) | Have for the generations in the media page |
| What Sidecar, AirPlay to Mac and Screen Sharing ask of the encoder | a hardware HEVC or H.264 encoder that accepts real-time, low-latency settings | the encoder list above | Partial. The encoder side is present; whether those three services pick our encoder is only known from a run with an iPad or a second Mac, which the probe's `wifi.awdl0` row must pass first |
| ProRes decode and encode | Apple silicon has media engines; Intel Macs use the CPU | macOS's CPU path, which does not depend on the GPU | Have (Apple's own software path) |
| Dolby Vision, spatial audio with head tracking | depends on the display, speakers and AirPods | depends on display output and the audio driver | Unverified: Apple's requirements per feature were not confirmed here |
| Screen Sharing high-performance mode | Apple documents it for Apple-silicon hosts | The viewer asks the local HEVC decoder for `MaxDecoderPixelRate` (a stock Mac answers 7,365,427,200 pixels per second) and falls back to 60 frames per second without an answer. The host asks its HEVC encoder for `MaxEncoderPixelRate` and sets the profile `HEVC_Main444_AutoLevel`. NVEncVT decoders and encoders answer the rates, and with `NVMTL_NVENC_HEVC_444=1` the encoder lists HEVC Main 4:4:4 and 4:4:4 10 and takes only 4:4:4 input under them | Built for both sides (`tools/test_nvencvt_pixel_rate.py`, `tools/test_nvencvt_hevc444.py`; library builds with the same warning count and exports). The 4:4:4 stream itself comes from the encoder's existing 4:4:4 path, whose output has not been decoded by a second Mac here. Neither side has run with a second Mac |
| FairPlay streaming in Safari and the TV app (up to 1080p) | needs a GPU path macOS accepts | none documented for NVIDIA | Unverified. The measurement is to play protected content on a NullMoth Mac and read the decoder report; we do not bypass or redistribute anything |
| FairPlay 4K / HDR streaming | needs the T2 or Apple silicon | none | Blocked: T2 or Secure Enclave |
| Audio output and input (onboard, HDMI and DisplayPort) | yes | NMXAudio on macOS 26 (macOS 15 has Apple's HDA driver) (`nmx/README.md`) | Have |
| Camera | built-in camera works through the USB video class driver | USB cameras work with macOS's own driver; MIPI cameras have no driver | Partial. The 1401 scan does not yet record the camera model |

## Continuity, Apple ID and services

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| iCloud, App Store, iMessage, FaceTime sign-in | valid serial, board serial, hardware UUID, ROM, working NVRAM, en0 built-in | 1401 generates the identity, keeps it across rebuilds, marks PCI Ethernet built-in, and (new) checks the whole identity at build time and on the installed Mac | Have, with new checks. `p1401/identity_ready.py` flags placeholder, sample and malformed values and a multicast ROM; fresh builds write a unicast ROM; `Policy.rom_mac` writes the primary adapter's address as the ROM (off); `nullmoth-mac-parity.sh identity` compares the ROM with en0 on the installed system. Whether Apple accepts the sign-in is Apple's decision |
| AirDrop, Handoff, Universal Clipboard | AWDL + Bluetooth LE | awdl0 only with Apple-compatible Wi-Fi: Broadcom cards through Apple's driver; the RTW89 driver raises awdl0 on RTL8922AE with transmit unproven (the RTW89 driver is its own project) | Partial. The build now says which of these the chosen Wi-Fi driver gives (`p1401/continuity.py`) |
| Universal Control | AWDL (or USB), a model Apple allows | model gate passes for every model 1401 picks; wireless needs awdl0 | Partial; USB works without awdl0 |
| Sidecar | AWDL (or USB), a hardware HEVC encoder | model gate passes; encoder: see Media | Partial; USB works without awdl0 |
| AirPlay to Mac | AWDL | needs awdl0 | Partial |
| Continuity Camera | AWDL (or USB) | needs awdl0 or a cable | Partial |
| Instant Hotspot, Auto Unlock with Apple Watch | Bluetooth LE + Wi-Fi | needs awdl0 and a working Bluetooth stack | Partial. Unverified which part of each depends on awdl0 |
| Model gate for Sidecar, AirPlay to Mac, Universal Control, Continuity Camera | the Mac model identifier | FeatureUnlock's README lists only models up to MacPro6,1, iMac16 and MacBookPro12 as excluded; 1401 presents MacPro7,1, iMac20,x or MacBookPro16,x | Have; `continuity.model_needs_unlock()` keeps it true for a hand-made EFI |
| iPhone Mirroring | Apple lists Apple silicon or a T2 Mac | none | Blocked: needs the T2 or Apple silicon |
| Phone calls and SMS relay | Wi-Fi + Bluetooth + iCloud | same as Handoff | Partial |
| Find My (this Mac) | Apple ID + location; offline finding needs the T2 or Apple silicon | Apple ID side as above | Partial. Offline finding is Blocked: T2 or Secure Enclave |
| Activation Lock | T2 or Apple silicon | none | Blocked: T2 or Secure Enclave |
| Apple Pay on this Mac | Secure Element (Touch ID Macs); on any Mac with Handoff, payment is approved on the iPhone | none directly | Blocked for the Secure Element. The iPhone route follows Handoff |
| Wi-Fi and Bluetooth chipsets | Apple's Broadcom parts | Intel through itlwm / AirportItlwm and IntelBluetoothFirmware; MediaTek (MT7921/7922) through SimpleMtkWlan; RTL8922AE through the RTW89 driver; Qualcomm Bluetooth through a community loader (`p1401/netsupport.py`, `p1401/community.py`) | Partial: Wi-Fi and Bluetooth join and work for the listed chips; awdl0 only where noted above |

## Apple Intelligence and machine learning

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| Apple Intelligence (writing tools, Image Playground, Siri models) | Apple silicon only | none | Blocked: needs Apple silicon |
| Core ML on the GPU, Metal Performance Shaders, Vision | yes | MPS and Core ML run on the Metal path (`docs/WINDOWS-PARITY.md`, Video and compute) | Have for the operators proven so far; the compute area owns the rest |
| Neural Engine | Apple silicon | Intel NPUs and AMD Ryzen AI through NMXNPU (`nmx/README.md`, test) | Partial; it is not the Apple Neural Engine and Core ML does not schedule onto it |
| Live Text, Visual Look Up | work through Vision on the GPU or CPU on Intel Macs | same Vision path | Unverified on a NullMoth Mac |

## Security and boot

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| FileVault | APFS volume encryption; no T2 needed | VirtualSMC is in the builds (`upstream/OpCore-Simplify` kext selection) | Have; confirmed by `mac_parity_probe.sh` (`security.filevault_usable`) |
| Secure Boot, Apple signing | T2 or Apple silicon | `SecureBootModel` Disabled; SIP lowered only as far as the driver needs (`p1401/policy.py`) | Blocked: T2. The design keeps SIP as high as each feature allows |
| Touch ID | Secure Enclave | none (`NoTouchID` is selected for models that expect it) | Blocked: Secure Enclave |
| Recovery, Migration Assistant, Time Machine | built into macOS | the OpenCore picker lists Recovery; the others are macOS's own | Have |
| Software updates | installer + Secure Boot | `revpatch=sbvmm` lets macOS 26 update on an unsupported model (`p1401/policy.py`) | Have |

## Power and input

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| Sleep and wake | yes | system sleep and wake with GPU resume (`docs/SLEEP-WAKE.md`) | Have on NVIDIA; the board's BIOS wake sources still matter |
| Power Nap, standby, hibernate | Intel Macs support them | PC firmware often mishandles the wake they cause | Built, off: `nullmoth-mac-parity.sh power check` shows the settings, `power apply` writes a profile after saving the previous values, `power restore` undoes it |
| Battery health, Low Power Mode, battery charge | SMC battery keys | SMCBatteryManager is selected for laptops by the engine | Have where the embedded controller is standard |
| Display brightness keys, keyboard backlight | HID and SMC | BrightnessKeys is selected by the engine for laptops; `kexts/NMKbdBacklight` (opt-in `nmkbl=1`) serves both routes macOS uses: the SMC key `LKSB` (non-T2 Macs) and a virtual built-in HID keyboard with the vendor element 0xFF00:0x0F, feature report 0xBF and output report 0xB0 (T2-era Macs, which is every Intel Mac macOS 26 supports). It forwards to ASUS WMI, Lenovo IdeaPad/Legion/LOQ (`KBLC`, `HALS`/`SALS`) and ThinkPad (`MLCG`/`MLCS`) | Brightness keys Have. Keyboard backlight: Built, off, compiled against the real SDK headers and host-tested, never run. Of 648 uploaded tables 33 carry the ASUS interface and 98 the Lenovo IdeaPad ones. Dell is built from the public Dell SMBIOS call encodings, with the buffer size read from Dell's descriptor (never run). MSI is built on the 22 firmware strings the public msi-ec table gives an address for, through ACPI's embedded-controller address space; any other firmware is refused (never run). HP and Acer: the firmware changes the light itself and the public Linux drivers ignore the notification, so there is no call to drive. Razer: a user-space setter (`tools/razer`) from OpenRazer's report layout, never run on a Razer |
| Trackpad gestures | Apple multitouch | VoodooI2C / VoodooPS2 by the engine | Partial: gestures depend on the touchpad's HID report |
| Thunderbolt, USB | yes | USB port mapping by UTBDefault; Thunderbolt hot-plug depends on the board | Partial; not measured |
| NFC | none on a Mac | n/a | n/a |

## Apps and system

| Feature | Real Mac | NullMoth Mac today | Status and gap |
|---|---|---|---|
| Apps built only for Apple silicon on an Intel Mac | Rosetta runs Intel apps on Apple silicon; the reverse does not exist | the ARM apps translator (opt-in, 2.7): command-line tools identical to Apple silicon, windowed apps experimental (`CHANGELOG.md` 2.7) | Partial |
| iPhone and iPad apps on the Mac | Apple silicon only | `ipa2mac` (ARM translator tree, `ipa/`) and the Mac app's ARM apps picker: an unencrypted .ipa becomes a Mac Catalyst bundle (platform stamp, `/System/iOSSupport` dependency paths, Mac layout, ad hoc signature). The translator (`xlat2.py`) now links a Catalyst program as a Catalyst x86_64 image, because dyld will not load iOSSupport frameworks into a macOS-platform image | Built, opt-in. Proven on a test app built for iOS: after conversion it runs natively on Apple silicon, and after translation the x86_64 Catalyst image starts and prints its line (run under Rosetta on an Apple-silicon Mac, not on an Intel Mac). Apps from the App Store are FairPlay-encrypted and are refused; this tool will not remove that. App extensions are refused for now |
| Xcode simulators | Intel and Apple-silicon runtimes | Apple's own | Unverified for macOS 26 on an Intel Mac |
| Universal apps | yes | yes (Intel slice) | Have |

## Which Apple-silicon Mac a translated program sees

`identity/profiles.py` in the ARM translator tree (branch `w29-identity`) maps the PC's NVIDIA GPU, by PCI device id and
architecture rather than marketing number, to the Apple-silicon chip a translated program is told it runs on:

| NVIDIA die | Chip | Reason |
|---|---|---|
| Turing without RT cores (GTX 16, GTX 1630/1650, MX450, MX550, T400 to T1200) | M1 | no hardware ray tracing, no AV1 decode |
| Turing with RT cores (RTX 20, TITAN RTX, Quadro RTX) | M2 | RT cores but no AV1 decode, as the M2 |
| Ampere GA10x (RTX 30, RTX 2050, MX570, RTX A) | M3 | first Apple chip with hardware RT and AV1 decode |
| Ada AD10x (RTX 40, RTX Ada, L4, L40; also the RTX 3050 A) | M4 | AV1 encode and newer RT cores |
| Blackwell GB20x (RTX 50, RTX PRO Blackwell) | M5 | fifth-generation tensor cores, as the M5's GPU neural accelerators |

Corrections to the name-based guess: the MX550 is Turing (device ids 1F9F and 1FA0), not Ampere; the RTX 2050 and MX570 are
Ampere; the "RTX 3050 A" (ids 2822 and 28A3) is an Ada part. An id outside the table (datacenter, mining, non-NVIDIA) changes
nothing. A chip the running macOS does not know is stepped down to the newest one it does (macOS 15 stops at the M4). The
Metal family claimed is capped at what the plugin implements (Apple6), whatever the chip's real family. Fields without a
verified source (the M5's core family) are left at the base answer. Off unless `ARM2X86_IDENTITY_PROFILE=1`.

## What this branch adds

| Item | Where | Proof |
|---|---|---|
| Identity readiness check for Apple services | 1401 `p1401/identity_ready.py`, run in `engine.py` after the policy pass and in `efidoctor.py` | `tests/test_identity_ready.py` |
| Fresh ROM is a valid unicast address; ROM from the primary adapter's address behind `Policy.rom_mac` | 1401 `engine._identity_rom_pass` | same test file |
| Continuity expectations from the Wi-Fi driver and Mac model | 1401 `p1401/continuity.py`, one line in the build notices | `tests/test_continuity.py` |
| Installed-Mac identity check and opt-in sleep profile | Mac app `Resources/nullmoth-mac-parity.sh` | `mac/tools/test_mac_parity_script.py` |
| Capability probe comparing a real Mac with a NullMoth Mac | `tools/mac_parity_probe.sh` | `tools/test_mac_parity_probe.py`, run on an Apple-silicon Mac for the reference output |
