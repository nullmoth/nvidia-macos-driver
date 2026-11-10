# What NVIDIA's Windows driver offers, and what this driver offers on macOS

This is the 2.9 audit for GTX 16 / RTX 20 to RTX 50 cards on macOS 15 and 26. Each row names the Windows feature, the
state in this tree (file, switch or document), and what is missing. "Blocked" always carries the reason. Media rows are
detailed in [WINDOWS-PARITY-MEDIA.md](WINDOWS-PARITY-MEDIA.md); card coverage in [CARD-SUPPORT.md](CARD-SUPPORT.md).

Settings that are boot arguments or `nvrm610.conf` lines are all listed, validated, and changed safely by
`tools/nullmoth_settings.py` (`list`, `get`, `set`, `--selftest`). It never touches a boot argument or conf line it was not
asked about, and writes conf settings only into the block the installer keeps across updates.

## Hardware coverage

| Item | State | Notes |
|---|---|---|
| Device table | 251 ids, `SUPPORTED_IDS` in `package/install.sh`, mirrored in the app's `nvidia_gsp_ids.json` | Proven against NVIDIA's 620.70.06 list by `tools/check_device_table.py <ogkm README.md> [app json]`: every id is in NVIDIA's list, every id left out (48) is a headless data-centre or mining part, every id falls in exactly one generation row, names match NVIDIA's. 2.9 added A40 (2235) and L40 (26B5), the two data-centre cards with DisplayPort outputs. |
| Turing to Blackwell, desktop, laptop, Max-Q, workstation, T-series, MX with Turing | all present | GTX 16, RTX 20, RTX 30 (GA10x), RTX 40, RTX 50, RTX A, RTX Ada, RTX PRO Blackwell, Quadro RTX, T400 to T1200, MX450/MX550 (Turing), MX570 (GA107) |
| Unknown future id | still installs | the installer accepts any device id from 1E00 up; only the name is missing from the Mac app until its list is updated |
| Per-generation paths | one row per generation in `plugin/nvmtl_dlss.c` (DLSS architecture), SM targets in `plugin/NVMTLVendorCompiler.m`, video library choice in `docs/WINDOWS-PARITY-MEDIA.md` | the device-table check proves every id reaches exactly one generation row |
| Pre-Turing | out of scope | no GSP firmware; see CARD-SUPPORT.md |
| eGPU over Thunderbolt | supported with limits | the BAR bridge-window code handles a card behind a downstream port (`kexts/NVRM/NVRM.cpp`, an RTX 3070 eGPU case); hot-unplug runs the device-removed path (`docs/NVRMFB-HOTPLUG-LIFECYCLE.md`, device-lost recovery in the plugin); `tools/nvmtl_gpus.py` marks a card as external. The tunnel carries PCIe x4 at most, so a link below x16 is normal there, not a fault. Not proven on hardware. |
| PCIe generation and link width | reported by `tools/nvmtl_gpus.py` | reads `IOPCIExpressLinkStatus` against `IOPCIExpressLinkCapabilities`; a narrower link is flagged LOW, a lower speed at full width is reported as idle step-down (cards drop their link speed at idle). `--selftest` has 10 named arms. |
| Multi-GPU boxes | off by default (`nvmultigpu=1`) | every NVIDIA card can be accelerated, one start-up at a time, refused cards are offered again; a process drives one card (the primary, or `NVMTL_CARD` per application). Several cards in one process are not built. [MULTI-GPU-DESIGN.md](MULTI-GPU-DESIGN.md) has the status and proof; a second card was not available to test with. |

## Display

| Windows feature | State in 2.9 | Gap |
|---|---|---|
| HDR10 | boot-arg `nvfbhdr=1` (opt-in, `kexts/NVRMFB/fb/nvrm-fb.cpp`, `tools/test_fb_hdr.py`) | studio verification pending |
| 10 / 12-bit output | `nvfb10bit=1`, `nvfbbpc` 6/8/10/12 | none |
| Colour format and range | `nvfbformat` (RGB, 4:2:2, 4:4:4), `nvfbrange` | none |
| Digital vibrance | `nvfbvib` 0 to 200, live through `debug.nvrmfb_color_*` | none |
| Variable refresh (G-Sync Compatible, Adaptive-Sync) | `nvfbvrr=1` | G-Sync with a hardware module needs NVIDIA's closed module protocol: blocked |
| DSR / DLDSR | `nvfbdsr=1`, `nvfbhidpi=1` give desktops above the panel's size scaled down by the head scaler | DLDSR's neural filter is a closed NVIDIA model: blocked; the plain scaler path is built |
| Scaling modes | head scaler, `nvfbrotscale=1` for rotated heads | none |
| Rotation | supported, scaled modes opt-in as above | none |
| Surround / multi-monitor | `nvfbsurround=<mask>` with hot-plug and audio (`tools/test_fb_surround_wiring.py`) | none |
| Laptop panel brightness | NVKMS path, eDP AUX path (`nvfbedpbl`) | none |
| HDMI/DP audio | ELD mapping (`docs/DISPLAY-AUDIO-MAPPING.md`) | none |
| Control panel | `tools/nullmoth_settings.py` (this release): one catalogue of 17 settings with validation. | The Mac app has a Driver settings window over the same catalogue (`mac/Sources/SettingsPanel.swift`, branch w29-panel). |

## Video and compute

| Windows feature | State in 2.9 | Gap |
|---|---|---|
| NVENC / NVDEC, AV1, VP9 | per generation in WINDOWS-PARITY-MEDIA.md | Blackwell decode class and 4:2:2 need a Blackwell card |
| RTX Video Super Resolution / HDR | none | closed NVIDIA models: blocked |
| CUDA | **blocked.** libcuda and the CUDA driver API are NVIDIA's closed code (licence 2.2/2.3 forbids modifying or disassembling it) and there is no open CUDA runtime for NVK. | The open route is not CUDA: OpenCL on NVK compute (Rusticl) and Vulkan compute, both tracked in the compute/OpenCL area. |
| OpenCL | owned by the compute/OpenCL area (`rel/driver-opencl`) | see that area's report |
| DirectML-equivalent | Metal Performance Shaders and Core ML run on the Metal path | none |
| Vulkan for applications | **built:** `package/vulkan-for-apps.sh --install` writes a loader manifest for the driver's NVK library, off by default (`tools/test_vulkan_for_apps.sh`, 12 arms). `nvk/nvk-metal-wsi.patch` adds `VK_EXT_metal_surface` windows (copy-based present into a `CAMetalLayer`). | Window presentation is proven to build only (see the report); the copy path is slower than a native one. The Khronos loader (Apache-2.0) can be installed with `--with-loader`; the package does not carry one yet. |
| OpenGL | translation layer `plugin/NVMTLGL.m` over the Metal path | no native OpenGL driver; version is whatever Apple's OpenGL front end reports |

## Gaming

| Windows feature | State in 2.9 | Gap |
|---|---|---|
| DLSS Super Resolution, Frame Generation, Ray Reconstruction | `plugin/nvmtl_dlss.c`, `plugin/NVMTLMetalFXDLSS.m`, `dlss/` (Blackwell default; Turing, Ampere, Ada opt-in) | verification needs studio |
| Max frame rate limiter | **built:** `NVMTL_FPS_LIMIT` (`plugin/nvmtl_fps_limit.h`, `tools/test_fps_limit.c`, 17 arms). One number or `Game=60,*=0`, 10 to 1000, never WindowServer. Kept across updates. | applied on the application's present call; proven offline only, behaviour on a real game is a studio check |
| Low-latency mode | `NVMTL_GSUB_INFLIGHT` (1 to 32, queue depth) | NVIDIA Reflex needs the game's own SDK calls and NVIDIA's closed runtime: blocked |
| Per-application profiles | built-in profiles by executable and bundle id (`plugin/nvmtl_app_profile.h`); a user's own settings block in `nvrm610.conf`; per-application GPU on hybrid laptops (`nvmtl-gpu-policy.txt`); per-application frame limits | a user-defined per-application profile for arbitrary knobs is not built; the frame limit is the one per-application user setting |
| Shader cache size control | `NVMTL_SPVCACHE_MAX_MB` (default 2048; 0 turns trimming off) | none |
| Ray tracing | software path; hardware RT blocked, see the RT area | |

## Power, thermal, system

| Windows feature | State in 2.9 | Gap |
|---|---|---|
| Idle power states | NVK burst boost, RM standby; ASPM L1 on request (nvrmaspm=1), the link otherwise stays as the firmware set it | measured idle clocks need hardware |
| Laptop dGPU power-off | opt-in D3cold monitor (`nvrtd3=1`, `nvrtd3res=`) | needs a hybrid laptop to prove |
| Power limit, clock offsets, fan curve | none | the public 620 headers carry no write control for them; desktop fans are VBIOS-controlled: blocked |
| Notebook power budget (Dynamic Boost's TGP step) | opt-in `nvrmtgp=1`: the GPU firmware is told to run at the platform limit while on mains power and released on battery, live switch `debug.nvrm_tgp_enable`, state in `debug.nvrm_tgp_*` and `nvmtl-smi` (`kexts/NVRM/nvrm_power.cpp`, policy `nvrm_power_policy.h`) | needs a notebook to measure the enforced limit; the CPU/GPU power split (NVPCF) is `nvrmnvpcf=1` and the platform request handler (GPS) is `nvrmgps=1`, both served from the firmware's own ACPI methods (`nvrm_dsm_route.h`) |
| Run-time GPU boost switch | opt-in `sudo env NULLMOTH_GPU_BOOST=1 ./install.sh` installs `nvmtl-boost` as a LaunchDaemon; `package/boost_daemon.sh set on\|off` (the Mac app's toggle) edits `/Library/NullMoth/gpuboost` and the daemon follows within two seconds (`tools/nvmtl-boost`, `tools/test_boost_daemon.py`) | needs a GPU run to measure the clocks |
| Hybrid laptop whose panel is on the integrated GPU | opt-in `nvfbigpu=1`: the framebuffer starts as a proxy head for the firmware console and the accelerator copies frames into it (`kexts/NVRMFB/nvrm_igpu_proxy.h`) | needs a BIOS Hybrid notebook to prove the copy path; see `docs/HYBRID-LAPTOPS.md` |
| Telemetry overlay | `tools/nvmtl-smi` reads power, clocks, temperature, pstate, utilisation, throttle reasons and fan from RUSD | a graphical overlay is not built |
| Crash recovery (TDR equivalent) | GPU fault handling and device-lost recovery in the plugin; boot guard switches the driver off after a failed boot | none |
| Driver control panel | CLI above and the Mac app's Driver settings window | none |

## How to prove these locally

```
python3 tools/check_device_table.py <open-gpu-kernel-modules README.md> p1401/nvidia_gsp_ids.json
python3 tools/nvmtl_gpus.py --selftest
python3 tools/nullmoth_settings.py --selftest
cc -I plugin tools/test_fps_limit.c -o /tmp/t && /tmp/t
bash tools/test_vulkan_for_apps.sh
python3 tools/test_install_keeps_my_settings.py
```
