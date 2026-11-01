# Research: feeding NVENC from video memory (capture surfaces in VRAM)

Question (2026-10-18): T5.06 found a GPU tiling pass slower than the CPU (1.9 ms vs 0.87 ms at 4K) because the test
surface lived in system RAM. What would it take for capture frames to stay in VRAM, so NVENC reads them without a CPU
pass?

Sources: nullmoth's driver source (`vendor/nvidia-macos-driver`: `kexts/NVRM/accel/nvrm-accel.cpp`,
`kexts/NVRM/nvrm_surfshare*.{h,cpp}`, `kexts/NVRMFB/fb/nvrm-fb.cpp`, `plugin/NVMTLDevice.m`, `plugin/nvmtl_vk.c`,
`nvk/nvk-macos.patch`), runtime counters in the guest, and two probes (`tools/surfvram.m`, `tools/vramtile.m`).

## How IOSurfaces are stored on this driver today

- An IOSurface's own memory is always **pageable system RAM** (IOSurface.kext).
- **NVAccel keeps a VRAM copy** of every IOSurface the GPU uses: `NVVidMemory::allocPhysical` → NVRMFB `vramGrant`
  (NVKMS-KAPI `allocateMemory`, video memory, scanout type, pitch layout, mapped into the kernel through BAR1). It
  stays attached across page-offs ("sticky"). The runtime counters showed 6.8 GB allocated this way since boot
  (`debug.nvaccel_vm_alloc_bytes`); the name says "vm", but the memory is video memory.
- The **Metal plugin imports that VRAM** for `newTextureWithDescriptor:iosurface:plane:` (escape
  `NVRM_ESC_SURF_VRAM` 0xE2 → `NV04_SHARE` of the KAPI memory into the caller's RM client → `NV_ESC_RM_DUP_OBJECT`),
  draws into it, and marks it dirty (`NVRM_ESC_SURF_DIRTY` 0xE3).
- A **CPU lock pages it off**: `NVResource::pageoff` copies VRAM → the system-RAM pages with the copy engine.
- The plugin imports only **one-plane surfaces with a row pitch that's a multiple of 32**. Anything else (multi-plane NV12,
  odd pitches) goes through a private image, copied in and out by the CPU (`nvmtlSurfaceIn/Out`).
- Other CPU-mappable allocations (`NVK` "LOCAL+mapped", Shared MTLBuffers) are put in system RAM unless BAR1 covers all
  of VRAM (`nvrm_local_map_stays_vram`: needs `NVK_LOCAL_MAP_SYSMEM=0` **and** BAR1 ≥ VRAM; here 4 GB < 24 GB).
- BAR1 budget for the VRAM copies: half of BAR1 (2 GB with a 4 GB BAR1).
- Display: with flips off (`-nvrmnoflip`, our setting, since zero-copy scan-out makes capture black), WindowServer's final
  frame is paged off and then **copied by the CPU word by word into the scanout through BAR1**.

## What the probes measured (RTX 4090, Tahoe 26.7.1)

`tools/surfvram.m`: render a pattern into an IOSurface with Metal, then take its VRAM copy into **our own** RM client.

| surface | VRAM copy after the Metal render | our client maps it | CPU lock (page-off) |
|---|---|---|---|
| BGRA 1920x1080 | correct (0 of 384 samples wrong) | yes: share + dup **0.06-0.1 ms** | 2.3 ms |
| BGRA 3840x2160 | correct (0 of 1449) | yes | **7.8 ms** |
| NV12 (420f), two planes | **stale**: each plane has a VRAM allocation, but the pixels went through the plugin's private-image fallback | yes (both planes) | ~0 ms (already copied out by the CPU) |

`tools/vramtile.m`: the whole GPU-resident path, BGRA only. Metal converts a GPU-rendered BGRA surface into NV12 in
NVENC's layout, written into a second IOSurface. Our RM client takes that surface's VRAM, and NVENC encodes straight from
it (`Enc.extInput`).

| | 1920x1080 | 3840x2160 |
|---|---|---|
| Metal convert + tile, VRAM → VRAM | **0.029 ms GPU** | **0.063 ms GPU** (0.34 ms wall incl. submit + wait) |
| bytes vs a CPU reference (same integer math) | identical | identical |
| NVENC from that VRAM | 1.16 ms, 49.6 dB | 4.17 ms, 49.4 dB |

For comparison, today's Parsec path at 4K: NV12 frames from ScreenCaptureKit in system RAM, our CPU tiling 0.87 ms +
engine 3.8 ms = 4.7 ms. Before that, WindowServer has already copied the frame on the CPU: a `sample` of WindowServer
during a Parsec session shows `-[NVMTLTexture nvmtlSurfaceOut/In]` → `memmove` (issue #93 measured WindowServer at
60-100 % of a core in that stack). The probes don't show how much of that is the capture frames.

## What it would take

**Already there (no driver change):**
- Any process that can look up an IOSurface ID can get the surface's VRAM into its own RM client (0xE2 + DUP), and
  NVENC can read it. No privilege needed.
- For one-plane surfaces rendered by the GPU (BGRA), the VRAM copy is current. A frame can go GPU → our Metal kernel →
  NVENC with no CPU pass, as `vramtile` shows.

**Driver changes (nullmoth's plugin / NVK), for Parsec's NV12 path:**
1. **Import multi-plane IOSurfaces into VRAM in the plugin.** The kernel already gives each plane its own VRAM allocation
   (`SURF_VRAM` takes a plane index); the plugin's `nvmtl_vk_surface_vram()` refuses anything but one plane with a
   32-aligned pitch and falls back to CPU copies. Supporting per-plane import (an R8 luma texture and an RG8 chroma texture
   on two VRAM allocations) would keep WindowServer's NV12 capture output in VRAM, and also remove WindowServer's CPU
   copies. It's the larger change: NVK images over imported per-plane memory, pitch rules, and the dirty/page-off
   bookkeeping per plane.
2. **A way for a consumer to know the VRAM copy is current** before reading it: a query escape (VRAM newest? pending GPU
   mark?) or a wait on WindowServer's completion mark (`nvSurfWaitReady` exists in the kernel, but only the display path
   uses it). Without it, an encoder can't tell a GPU-written surface from a CPU-written one, apart from trusting the
   producer.
3. Optional: zero-copy scan-out (`nvaccel_iop_flip`) breaks capture today because nothing pages off the scanned-out
   composite. If consumers read VRAM directly, capture and flips could coexist: flips also save WindowServer's CPU copy
   into the scanout.

**On our side (libnvencvt), once the driver keeps capture frames in VRAM:**
1. From the `CVPixelBuffer`, get the IOSurface ID; `SURF_VRAM` per plane + DUP + GPU map, cached per surface ID
   (ScreenCaptureKit cycles through a small pool), unmapped when the surface goes away.
2. Convert/tile on the GPU from VRAM into NVENC's layout: a Metal compute kernel (≈0.06 ms at 4K, as measured), writing an
   IOSurface whose VRAM our client maps. NVENC can't read pitch-linear input, and the copy engine can't produce NVENC's
   layout, so a shader pass stays necessary.
3. Order it: wait for the Metal command buffer (≈0.3 ms wall today), or better, have the NVENC channel wait on a semaphore
   the Metal work releases.
4. Never lock the source surface on the CPU, so no page-off happens.

## Numbers (2026-10-18, RTX 4090 in a KVM guest, 4K desktop, Parsec streaming)

### Per-frame costs of today's surface paths (`tools/surfcost.m`)

| path, 3840x2160 | wall | CPU | 1920x1080 wall |
|---|---|---|---|
| NV12 rendered by Metal into a **two-plane** IOSurface (plugin fallback: private image + CPU copy-out) | **5.94 ms** | **5.24 ms** | 1.41 ms |
| same planes as two **one-plane** IOSurfaces (VRAM import) | 0.24 ms | 0.25 ms | 0.66 ms |
| CPU lock of a GPU-rendered BGRA surface (page-off: copy engine to a staging buffer, then a CPU copy into the pages) | **7.59 ms** | **7.17 ms** | 1.97 ms |
| Metal convert + tile BGRA → NVENC-layout NV12, VRAM → VRAM (`vramtile`) | 0.063 ms GPU / 0.34 ms wall | ~0 | 0.029 ms GPU |
| CPU copy of a BGRA frame into VRAM through BAR1, `memcpy` | **22.8 ms** | 22.8 ms | 5.5 ms |
| same, 32-bit word loop (what NVAccel's display copy does) | **83.6 ms** | 83.6 ms | 21.1 ms |
| CPU read back through BAR1 | ~7 s | | ~1.5 s |

**BAR1 was effectively uncached in this VM** (1.4 GB/s with `memcpy`, 0.37 GB/s with 32-bit stores), although RM maps it
write-combined in the guest (caching type 2). The cause turned out to be in the guest, not on the host: the AMD_Vanilla
kernel patch that OpenCore applies to XNU sets PAT entry 6 to uncached-minus, and XNU maps write-combined memory through
PAT entry 6. See "BAR1 caching: cause and fix" below. With the fix, the same copy takes 3.1-5.5 ms instead of 86 ms.

### Live: WindowServer and Parsec during a session (20 s windows)

| | static desktop | full-screen motion (CSS-animated layer in Safari) |
|---|---|---|
| Parsec frame rate (new desktop frames) | 20-30 fps | **~10 fps** |
| our encoder, entry → emit | 4.7 ms | 5.0 ms |
| WindowServer CPU (user / kernel) | 16 % / 3.5 % of a core | 23 % / **17 %** of a core |
| display transactions (`debug.nvaccel_iop_ok`) | 0.15 per s | **4.8 per s** |
| WindowServer GPU-mark waits that hit the 100 ms timeout | | **69 in 20 s** |
| parsecd CPU | 16 % | 10 % |
| screensharingd (a macOS Screen Sharing session open at the same time) | | 43 % |

A `sample` of WindowServer during the motion test: the plugin's `-[NVMTLTexture nvmtlSurfaceOut]` was on the stack for
~2860 samples in 8 s. Almost all of it (1343 samples under `nvmtlAdvancePart` alone) was **blocked in the kernel in
`NVRM_ESC_SURF_DIRTY`**, not copying (`memmove` at the top of the stack: 136 samples). The escape takes NVAccel's
accelerator family lock (`nvFamLock`, blocking), and the display transaction's CPU copy through BAR1 very likely runs
under that same lock. That last link is inferred from the code, not measured.

## What this means

1. **Capture in VRAM saves little in our encoder:** 0.87 ms CPU tiling → ~0.06 ms GPU + ~0.3 ms submit/wait. Parsec's
   encode latency would go 4.7 → ~4.1 ms (~3.9 ms with a GPU-side wait instead of a CPU wait).
2. **For the capture producer it's a 24x difference** when NV12 is rendered by Metal: 5.9 ms → 0.24 ms per 4K frame,
   if the plugin imported multi-plane surfaces into VRAM. In the live test, though, WindowServer's copy-outs were mostly
   blocked rather than copying, so the CPU copy isn't the main cost there. How ScreenCaptureKit produces Parsec's 420f
   frames (which process, which path) isn't pinned down yet.
3. **CPU readers of GPU-rendered BGRA surfaces** (screenshots, BGRA capture, VideoToolbox conversions) pay a 7.6 ms
   page-off per 4K frame today. Reading the VRAM copy on the GPU removes it.
4. **The biggest cost on this machine is the display copy path, not capture.** With zero-copy scan-out off (it makes
   capture black), every desktop update is copied by the CPU into the scanout through BAR1, word by word. Here that's
   ~35-85 ms of WindowServer kernel time per full 4K update, about 17 % of a core under motion. It holds the accelerator
   lock, the desktop runs at ~5-10 updates per second under motion, and Parsec can't deliver more frames than that.
   **Enabling zero-copy scan-out would remove that copy**, but capture then has to read the composite from VRAM rather
   than from the IOSurface's system pages: the same mechanism as above, on the producer side (WindowServer/SkyLight
   and the plugin), plus a page-off or GPU read where capture needs the pixels. That's driver work.
5. **Independent of the driver, write-combining for BAR1 in the VM** makes every CPU write through BAR1 much cheaper.
   The fix is a guest boot setting (OpenCore), not a host change; see "BAR1 caching: cause and fix" below.

## Expected improvements (summary)

| change | who does it | gain (4K) | basis |
|---|---|---|---|
| encoder reads VRAM, GPU tiling | us (needs 2 below for NV12) | encode 4.7 → ~4.1 ms; ~0.9 ms CPU/frame saved | measured parts |
| plugin imports multi-plane IOSurfaces into VRAM | nullmoth | NV12 producer 5.9 → 0.24 ms per frame; removes CPU copy-outs | measured (synthetic) |
| consumers read VRAM instead of locking | us / apps | −7.6 ms per BGRA frame read | measured |
| zero-copy scan-out compatible with capture | nullmoth (+ the above) | removes ~35-85 ms CPU per desktop update here; desktop under motion ~10 fps → up to the GPU's compositing rate; WindowServer −~17 % of a core | measured components, end result estimated |
| write-combined BAR1 in the VM (PA6 = WC in the AMD_Vanilla PAT patch) | us (OpenCore config) | CPU → BAR1 writes 86 → 3-5.5 ms per 4K frame; desktop under motion 4.3-4.8 → 12-17 display transactions/s | **measured** (below) |

Not measured yet (would need the user's OK, since it affects the live desktop and the Parsec stream): desktop frame rate
and WindowServer CPU with zero-copy scan-out switched on (`sysctl debug.nvaccel_iop_flip=1`; capture goes black while
it's on).

**Risks and limits:**
- The VRAM copies count against the BAR1 budget (2 GB with a 4 GB BAR1).
- The share/dup path has no access check beyond IOSurface lookup. Fine for us, but nullmoth may want to restrict it.
- It ties our encoder to undocumented driver escapes (0xE2/0xE3, ABI version 2) that can change between releases.
  Check the version and fall back to the CPU path.

## BAR1 caching: cause and fix (2026-10-18)

### What was tried, and what the numbers said

`tools/surfcost` part C writes one 4K BGRA frame (31.6 MB) into VRAM through a BAR1 mapping. `RM_MAP_CACHE=1..4`
(`src/rm.c`) forces the guest mapping to uncached, write-through, copy-back or write-combined.

| configuration | word loop | memcpy |
|---|---|---|
| stock guest, any `RM_MAP_CACHE` mode (1080p: 21.3-21.8 ms / 5.5-5.8 ms, all the same) | 86 ms | 23 ms |
| + host MTRR write-combining over BAR1 on the host (`/proc/mtrr`) | 86 ms | 23 ms |
| **guest PAT fixed (PA6 = WC)**, BAR1 32 GB, with or without the host MTRR | **3.1 ms** | **4.9 ms** |
| guest PAT fixed, BAR1 4 GB | 5.5 ms | 6.2 ms |

Every guest cache mode giving the same speed, even copy-back over a host write-combining MTRR, meant the guest page
table bits weren't reaching the hardware as requested. The host side was already right: KVM's NPT entries use PAT
entry 0 (write-back) and rely on host MTRRs (Linux 6.1 `arch/x86/kvm/mmu/spte.c`), the WC MTRR covered BAR1 exactly,
and KVM copies the guest's IA32_PAT into the VMCB (`svm.c`, `save.g_pat`).

### Cause

Tracing the guest's IA32_PAT writes on the host (`kvm:kvm_msr` with filter `ecx == 0x277`, during boot) showed 16 writes
(one per vCPU), all `0x0007010600070106`: PA0 WB, PA1 WC, PA2 UC-, PA3 UC, PA4 WB, PA5 WC, **PA6 UC-**, PA7 UC.

- XNU (`osfmk/i386/mtrr.c`, `mtrr_update_action`) normally sets **PA6 = WC**. Its pmap encodes write-combined pages as
  PAT|PCD = entry 6, and kIOMapWriteCombineCache mappings use it.
- The enabled AMD_Vanilla patch **"Algrey / Zormeister | _mtrr_update_action | Fix PAT | 15.0+"** replaces that code
  with `mov eax, 0x00070106; mov edx, 0x00070106; wrmsr`, a fixed PAT whose entry 6 is UC-. The 10.13+ version does
  the same; the "Shaneee" variants (disabled) make every entry write-back.
- So in this guest every write-combined mapping (RM's BAR1 mappings, kIOMapWriteCombineCache) ran uncached-minus.
  The earlier explanation (the host's PAT maps BAR1 uncached-minus for VFIO) was wrong: that is QEMU's host mapping, which
  the guest's accesses don't use.

### Fix

`vm/tahoe/make-config.py --pat-wc` (with `--cpu amd`) rewrites the patch's Replace data to `edx = 0x00010106`
(PA4 WB, PA5 WC, **PA6 WC**, PA7 UC), keeping PA0-PA3. The image is `OpenCore-amd-patwc-gpu-debug.img`. After boot
the trace shows `0x0001010600070106` on all 16 vCPUs. No host change is needed: the host MTRR made no difference with
the fix in place, so it was removed again.

### Effect on the desktop (Safari, full-screen CSS gradient animation, `vm/tahoe/bench/desk-sample.sh`, 20 s)

| | display transactions/s | WindowServer user / system (% of a core) |
|---|---|---|
| stock PAT, BAR1 32 GB | 4.3-4.8 | 24 / 14 |
| PA6 = WC, BAR1 32 GB | 8.8 | 46 / 32 |
| **PA6 = WC, BAR1 4 GB** | **12.3-16.8** | 44-52 / 21-33 |

The desktop update rate under motion goes up about 3x at the standard BAR1 size. Kernel time per transaction is still
~20-35 ms, so nullmoth's in-kernel display copy most likely still writes through an uncached mapping. NVKMS asks for
kernel mappings with `flags = 0` (caching type "cached"), and what RM makes of that for video memory isn't verified.
Mapping the scanout write-combined there would be a driver change.

### BAR1 size retest (same day, root port + `phys-bits=40`, AMD profile)

| BAR1 | boots, display | NVRM placement | driver policy |
|---|---|---|---|
| 4 GB | yes | 0xce00000000 | scanout by-handle, surface budget 2 GB |
| 8 GB | yes | 0xcc00000000 | by-handle, budget 4 GB |
| 16 GB | yes | 0xc800000000 | by-handle, budget 8 GB |
| 32 GB | yes | 0xc000000000 | **static-bar1** (BAR1 ≥ VRAM), budget 16 GB |

The earlier "32 GB: macOS drops the card" predates the root port and `phys-bits=40` setup. The plugin still places
LOCAL+mapped allocations in system RAM at every size (`b80 … default`). surfcost's B paths didn't change with BAR1 size
(B1 4.7-5.0 ms, B2 0.26-0.31 ms, page-off 6.9-8.6 ms). With the PAT fix, 4 GB gave the best desktop rate of the
sizes measured, so 4 GB stays the default.

The macOS Screen Sharing frame grab (`vnc-shot.py`) was black with only the cursor after every reboot in this series,
stock image and 4 GB included, so that is unrelated to BAR1 size or PAT. Not investigated yet.

Raw results: `notes/bar1-tests/` (project folder) and `notes/bar1-cycle-*.log`, made by `vm/tahoe/bar1-cycle.sh`.

### Where the desktop's time goes after the PAT fix (spindump, 2026-10-18)

`spindump` of the whole system for 10 s during the Safari motion test (PA6 = WC, BAR1 4 GB; 12-17 display
transactions/s). Kext frames resolved with `atos -l 0` against `/Library/Extensions/NV*.kext`. Raw:
`notes/bar1-tests/ws-motion-patwc.spindump` (project folder).

- **The display copy is no longer the bottleneck.** `NVDisplayPipe::performTransaction` runs on a kernel_task thread
  (`debug.nvaccel_iop_async=1`, every transaction from an IOSurface: `iop_src_surf`). It costs ~76 samples in 10 s
  (~8 % of a core): ~75 in `nvAccelSurfCopyReady` → `NVResource::pageoff` → `nvrm_ce_copy_to_pages` (copy engine into
  the surface's pages; about half of that is `os_delay_us` busy-polling for the copy engine), and ~30 in the word
  copy into the scanout (about 2 ms per transaction, down from tens of ms).
- **WindowServer's main thread is the limit:** 6.0 s CPU in 10 s. Of its 1001 samples, 809 are in NVMTLDriver and
  **403 in `-[NVMTLTexture nvmtlSurfaceIn]`** (306 in `nvmtl_vk_image_write`, 122 in page-fault traps). That's the
  compositor copying client surfaces (Safari's layer) from their system pages into VRAM on the CPU for every frame,
  after the producer's frame was paged off from VRAM into those pages: a VRAM → RAM → VRAM round trip per frame.
- The plugin has a path that avoids this ("item 7": single-plane IOSurfaces with `pitch % 32 == 0` alias the
  family's VRAM copy, and surface-in is skipped while the VRAM tag is current; on by default). Why Safari's layers
  don't take it here (multi-plane, pitch, BAR1 surface budget, or a stale tag) isn't known yet: the release plugin
  only logs failures. Next step: a plugin built with logging (`NVMTL_RELEASE=0`) to count skipped/copied surface-ins
  and see which surfaces fall back.

### Surface statistics from the logging plugin (branch `plugin-logging`, 2026-10-18)

Fork branch `plugin-logging` (from `nvenc-pre-blackwell`): the plugin sources the guest runs (v2.3.1 + our NVENC
commits) plus per-process statistics in non-release builds, written every 2 s to `$TMPDIR/nvmtl-surfstat-<pid>.log`
(WindowServer's: `/private/var/folders/zz/*/T/WindowServer/`, root only). Built with
`PLUGIN_REF=plugin-logging RELEASE=0 guest/nvenc/build-plugin.sh` (kvm-tahoe-nvidia).

Motion test, WindowServer, per 2 s:

| | |
|---|---|
| textures created for IOSurfaces | ~300-360, all shared VRAM ("item 7"), 2-18 private (multi-plane / plane > 0) |
| surface-ins with a copy | 36-46 (the frame rate), **0 skipped**, ~2,000-2,400 unchanged |
| copied per surface-in | Safari's layer, 3158x1816 BGRA, pitch 12672, cache mode default: ~22 MB in ~17-20 ms |
| of which the CPU copy from the surface's pages | ~14-17 ms |

So Safari's layers *are* in shared VRAM, but each frame's pixels arrive in the surface's system pages and WindowServer
uploads them: 0 skips means the VRAM tag is never current, because the producer didn't write through the GPU.

**Why the producer doesn't use the GPU:** WebKit's GPU process can't load the plugin. Its sandbox
(`com.apple.WebKit.GPUProcess.sb`) allows `file-read*` on `/Library/GPUBundles` but `file-map-executable` only on system
paths (`/System/Library/Extensions`, the frameworks), and the log shows
`com.apple.WebKit.GPU: Error loading /Library/GPUBundles/NVMTLDriver.bundle/... (file system sandbox blocked mmap())` at
every Safari start (nullmoth's shipped plugin is also in `/Library/GPUBundles` and ad-hoc signed). Without Metal,
WebKit paints its layers with the CPU, and every frame takes the upload path. This affects every WebKit view, not just
this VM. Not mentioned in nullmoth's repository.

**Why the upload is slow:** `tools/surfread.m` reads a 3158x1816 BGRA IOSurface the way the plugin does. In one process
the pages read at 5-8 GB/s (2.6-4.5 ms per 22 MB); only the first read after the surface is mapped costs 14-17 ms
(~5,400 page faults, ~2.6 us each in this VM). WindowServer pays that first-read cost every frame (14-17 ms), so its
mapping of Safari's surfaces is new each time (consistent with ~150 new texture wrappers per second).

**What would remove it:** an upload that doesn't touch the pages with WindowServer's CPU: the copy engine pulling the
surface's system pages into its shared VRAM, the reverse of the kernel's existing page-off
(`nvrm_surfshare_pageoff` → `nvrm_ce_copy_to_pages`). No "page-on" exists in the kexts today; it would be new code in
NVRM/NVAccel plus a plugin change. Separately, a plugin installed where WebKit's GPU process may map code would let
Safari render with Metal, but `/System/Library/Extensions` is on the sealed system volume.

### Fix: surface-in through the GPU (fork branch `surface-upload` → `dev`, 2026-10-18)

A kernel "page-on" can't be built from the published sources (NVRM needs nullmoth's unpublished RM build; its copy
helper `nvrm_ce_copy_to_pages` only copies VRAM → pages). The plugin does it instead, in user space:

- **GPU upload:** `nvmtl_vk_image_write_host` imports the locked surface pages as host memory
  (`VK_EXT_external_memory_host`, which this driver supports) and lets the GPU copy them into the image. No CPU copy,
  and the CPU never touches the pages. Default in every process; `NVMTL_SURFIN_UPLOAD=cpu` turns it off.
- **Keep surfaces mapped:** WindowServer releases a client's surface every frame, and the pages were faulted in again
  at the next upload (same address every time; "mapped at a new address 0 x" in the statistics). Retaining the last 32
  uploaded surfaces (at most 384 MB) keeps them mapped. Default in WindowServer only; `NVMTL_SURFIN_KEEP=1/0`.

Same WindowServer session, Safari motion test, 3158x1816 layer, switched with `/tmp/nvmtl-surfin-mode`:

| surface-in | per upload | display transactions/s | WindowServer user / system |
|---|---|---|---|
| CPU copy (before) | 15.0 ms | 23.7 | 128 % / 36 % |
| keep mapped | 5.8 ms (CPU copy ~2.7 ms) | 31.9 | 133 % / 19 % |
| GPU upload | 5.7 ms | 29.5 | 45 % / 22 % |
| **both (the default now)** | **5.3 ms** | **30.1** | **30 % / 21 %** |

`tools/surfincheck.m` (CPU-written pattern → IOSurface texture → GPU blit → compare): pixel-exact in every mode, at
3158x1740, 1920x1080 and 333x211, over 100 rounds. With the defaults installed, the limit in this test is now the
producer: WebKit paints the animation on the CPU at ~180 % CPU, because its GPU process can't load the plugin
(see above).

## Driver 2.3.7: is the fork's surface-in still worth it? (2026-10-19)

NullMoth's plugin against the fork's (GPU upload + keep uploaded surfaces mapped), Safari `motion.html` (full-window CSS
gradient animation), RTX 4090, macOS 26.7.1, PAT fix, BAR1 4 GB. `vm/tahoe/bench/desk-ab.sh` (dev VM) installs each
plugin, logs out and in so WindowServer loads it, quits OBS/Parsec/Terminal (OBS's preview alone keeps ~30 display
transactions/s going), opens Safari in a fixed window and samples 3 x 20 s with `desk-run.sh` / `desk-sample.sh`.
Release builds alternate stock, ours, stock, ours; the diagnostic build switches `/tmp/nvmtl-surfin-mode` in one session
and reports the cost per upload. Raw: `notes/desk-ab-20261019-0417.log`, `-0434.log`, `-0439.log`.

| window (pixels) | plugin | display transactions/s | WindowServer user / system | WebKit GPU process | per upload |
|---|---|---|---|---|---|
| 3200x1800 | NullMoth 2.3.7 (2 x 3 samples) | 34.9-40.2 (avg 38.6) | 45 % / 14 % | 222 % | 6.7-7.6 ms (diag, mode 0) |
| 3200x1800 | fork (2 x 3 samples) | 39.6-42.4 (avg 41.1) | 33 % / 22 % | 199 % | 6.3-6.6 ms (diag, mode 3) |
| 3840x2110 | NullMoth 2.3.7 | 20.8-21.2 | 33 % / 40 % | 469 % | 19.4-19.7 ms |
| 3840x2110 | fork | 33.7-34.5 | 29 % / 26 % | 265 % | 7.8-7.9 ms |
| 3840x2110 | GPU upload only (mode 2) | 32.8-33.9 | 28 % / 28 % | 285 % | 7.7 ms |
| 3840x2110 | keep mapped only (mode 1) | 31.2-32.3 | 40 % / 18 % | 294 % | 8.5-8.8 ms |

- **Up to about 3200x1800 the fork's plugin is barely better on 2.3.7** (+6 % display rate, a few points less
  WindowServer CPU). NullMoth's own copy got much faster than in 2.3.6 (15.0 → ~7 ms per upload for this window), which
  closed most of the gap measured then.
- **A full-screen 4K window is where it still matters:** NullMoth's CPU copy takes ~19.7 ms per upload (nearly 3x the
  smaller window for 1.4x the pixels: the pages are faulted in again every frame), the display falls to ~21 updates/s,
  and WebKit's GPU process spends ~470 % of a core. The fork's plugin: 7.8 ms, ~34 updates/s (+63 %), WebKit ~265 %.
- **Either part alone gives most of it** (GPU upload 33.4/s, keep mapped 31.8/s, both 34/s). GPU upload holds no extra
  memory and takes the most user CPU off WindowServer; keeping surfaces mapped retains up to 384 MB of client surfaces
  in WindowServer.
- Display rates are compositor updates (`debug.nvaccel_iop_ok`), not Safari's frame rate. The idle desktop is 0 in all
  variants.

### Smaller windows: 1920x1080 and 1280x720 pixels (minimal patch 4868217, 2026-10-19)

Same harness, release builds, Safari window 960x540 / 640x360 pt on the HiDPI 3840x2160@50 desktop (the upload a
full-screen window makes on a 1080p / 720p display). Raw: `notes/desk-ab-20261019-0603.log`, `-0608.log`.

| window (pixels) | plugin | updates/s | WindowServer user / system | WebKit GPU process |
|---|---|---|---|---|
| 1920x1080 | NullMoth 2.3.7 | 50.0 (the 50 Hz cap) | 24-25 % / 6 % | 91 % |
| 1920x1080 | + patch | 50.0 | 22 % / 11 % | 88 % |
| 1280x720 | NullMoth 2.3.7 | 50.0 | 25-26 % / 8 % | 44 % |
| 1280x720 | + patch | 50.0 | 22 % / 11 % | 42 % |

No difference at these sizes: both hold the display rate, and WindowServer's total CPU is within a few points (the
patch moves some of it from user to system time). The gain is specific to large (4K-class) CPU-drawn windows. A
native 1080p@60 desktop would lift the 50 Hz cap; NVRMFB only offers rasters at or above the firmware's (4K here),
hence the fork-only `nvfbsmall=1` experiment.

## Driver 2.4.3: still worth it? (2026-10-20)

Yes, for large windows: NullMoth 2.4.x changed nothing on the surface-in path. `vm/tahoe/bench/desk-ab.sh`, stock 2.4.3 plugin
against the release build of fork `dev` (2.4.3 + 4868217, overlay `v2.4.3-mickys.1 --desktop`), passes alternating stock /
ours, 3 x 20 s each, Safari `motion.html`; logs `notes/desk-ab-20261020-1650.log` (full screen) and `-1700.log` (3200x1800).

| window | plugin | updates/s (6 samples) | WindowServer CPU | WebKit GPU process |
|---|---|---|---|---|
| 3840x2110 (full screen) | stock 2.4.3 | 20.6 (20.5-20.9) | 73 % | 459 % |
| 3840x2110 (full screen) | ours | 34.6 (33.9-35.6), +67 % | 55 % | 255 % |
| 3200x1800 | stock 2.4.3 | 40.3 (39.0-42.5) | 58 % | 215 % |
| 3200x1800 | ours | 42.9 (42.4-43.6), +6 % | 55 % | 198 % |

The same as on 2.3.7 (20.9 -> 34.3 full screen, 40.5 -> 40.9 at 3200x1800). Measurement traps found on the way: after a
relogin the guest session now comes back locked (`CGSSessionScreenIsLocked`; the lock screen's animated wallpaper then
gives ~41 updates/s with Safari idle, and the display sleeps after a while, reading 0); `desk-run.sh` keeps the display
awake and `desk-ab.sh` unlocks through QMP when needed.

## GTX 1650: the same patch on a low-end card (2026-10-21)

Same benchmark on a GTX 1650 (TU117, 4 GB, BAR1 256 MB) on NullMoth 2.4.3: the stock plugin against `v2.4.3-mickys.2`'s
`--desktop` plugin (= 2.4.3 + 4868217), 2 x 3 x 20 s each. Logs: `notes/desk-ab-20261021-0123.log` (4K) and `-0133.log`
(3200x1800).

| Safari window | stock (updates/s) | patch (updates/s) | WindowServer, user + system (% of a core) | WebKit GPU process |
|---|---:|---:|---|---|
| 3200x1800 | 23.2 (21.6-23.9) | 31.7 (31.2-32.5), **+37 %** | 68 → 48 | 200-270 % → 192-200 % |
| full-screen 4K | 16.5 (18.7 / 14.3 by login) | 18.6 (18.1-18.9), +13 % | 80 / 73 → 61 / 63 | 350-420 % → 240-420 % |

- **3200x1800:** the gain the 4090 did not show there (+6 %), because on this card the CPU copy still limited that size.
- **Full-screen 4K:** the card itself caps both builds at about 19 updates per second, so the gain is smaller. Stock varied
  between logins (18.7 then 14.3), the patch did not. WindowServer's own CPU time drops by about two thirds (user 47 % /
  32 % → 16-18 %).
- **On both cards** the patch lowers WindowServer's CPU and raises the update rate where the CPU copy is the limit.

### Parsec on the GTX 1650 with and without the patch (2026-10-21)

Parsec streams the 4K HiDPI desktop to the user's Mac (H.265 8-bit), Safari shows motion.html, and the frame rate comes from
Parsec's host log over 35 s each. The patch raises Parsec's capture rate:

| Safari | stock 2.4.3 plugin | patch (`--desktop`) |
|---|---:|---:|
| window | 7.9 fps | 11.4 fps (+44 %) |
| full screen | 10.0 fps | 13.1 fps (+31 %) |
| idle desktop | ~10 fps | ~29 fps |

The encode latency is the same (13-14 ms), so the capture is what improves: it waits on WindowServer, and the patch takes the
per-frame window upload off WindowServer's CPU.
