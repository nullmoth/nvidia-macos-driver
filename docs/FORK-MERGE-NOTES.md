# Retained fork changes for upstream merges

## Hardware vblank after partial modesets, 2026-11-17

Retain `620-vblank-restore-unchanged-heads.patch`. NVIDIA 620.70.06 disables
RG semaphore interrupts on all API heads before applying a committed modeset,
but restores only changed heads. Restore active unchanged heads through the
existing guarded enable helper in the same modeset update. Preserve inactive
heads, callback ownership, timing, mappings and the ordinary changed-head path.
The macOS framebuffer otherwise retains a registered callback that stops firing
and uses its software-timer fallback. There is no per-frame change.

The production disable loop, enable helper and unchanged-head branch pass
ASan/UBSan checks for partial changes across two display engines, preserved
callbacks/timings, reconnects, absent capabilities and 200 repeated modesets.
The unpatched control loses unchanged-head interrupts as expected. This defect
also exists in the NVIDIA core used by v2.9 and current driver upstream; it
does not establish the cause of a reported playback version regression.
On RTX 2060 SUPER / Tahoe 26.7.1 / 8 GiB BAR1, the trial UUID is verified
loaded after reboot. With two 1080p75 displays, the user reports smooth desktop
behavior and flip-timeout/late counters are zero. However, NVKMS explicitly
reports RG semaphore interrupts unsupported on this platform: the existing
timer fallback remains active, so this does not exercise the repaired hardware
interrupt path or establish the cause of the perceived improvement.
The user also reports smooth behavior after the requested single-display
refresh-change check. The repaired interrupt path and remote RTX 4070 Laptop
playback remain unqualified on hardware.

The private NVRM builds and links isolated Tahoe collections
with the current machine's eight extensions and with the package's core and
optional drivers. Install, rollback and injected-failure restoration pass on
an isolated filesystem fixture; the link preflights left installed files and
collections unchanged. The user subsequently approved the private NVRM install;
the original NVRM and auxiliary collection are backed up for rollback.
Sequoia execution remains untested. Keep the private trial separate from the
held release assets until explicitly staged for a package test.

## Rejected convolution diagnostics, 2026-11-17

Keep rejected-convolution diagnostic formatting, string allocation and locking
out of ordinary release dispatches. `NVMTL_PROCESS_ENV=1` retains those logs,
and development builds retain their existing diagnostics. Rejection still
runs the original shader; kernel selection, output verification and timing
gates are unchanged.

On RTX 2060 SUPER / Tahoe 26.7.1 / 8 GiB BAR1, 2,000 rejected linked dispatches
drop from about 6.0 ms to 2.3 ms of recording CPU with exact integer output.
Ordinary dispatches and opted-in logging retain their preceding timings.
Binding, descriptor reuse, partial IOSurface, render-format and sampler GPU
checks pass. This targeted CPU saving is not an application FPS claim.
The combined hash-reuse and diagnostic-guard build scores
[73,543](https://browser.geekbench.com/v7/gpu/409935) in a clean Geekbench run.

## Linked compute shader hash reuse, 2026-11-17

Mark copied linker, tile-lowering and mesh-lowering output as immutable AIR,
as for decoded modules. The metadata-cache change in `d04dd86` left this text
unmarked, causing every compute dispatch to hash the whole module again.
Preserve fresh analysis for external mutable text, the existing cache keys and
shader bytes, and the WindowServer image-pooling optimization in `87f5cd9`.

On RTX 2060 SUPER / Tahoe 26.7.1 / 8 GiB BAR1, the isolated linked-dispatch
probe drops from about 55 ms to 1.1 ms per 1,000 dispatches with identical
integer output. Reverting only `87f5cd9` retains the slowdown; restoring only
the old function-key memoization removes it. Direct and indirect dispatch,
binding, descriptor and sanitized cache/lifetime checks pass, including a
missing-marker negative control. The clean Geekbench run scores
[73,075](https://browser.geekbench.com/v7/gpu/409848), versus
[59,616](https://browser.geekbench.com/v7/gpu/409737) before the repair and
[74,840](https://browser.geekbench.com/v7/gpu/409717) on v3.0.
Other GPUs and Sequoia execution remain untested for this repair.

## Wine hybrid window binding, 2026-11-16

Keep the Wine-only initial-layer repair in `nvk/native-macos/bridge.mm`. Wine
creates its Metal layer with the system-default device, which can be Intel or
AMD even though DXVK selected NVIDIA. At new surface creation, defer a foreign
binding only after complete NVAccel registry enumeration. Preserve NVIDIA-bound
layers, already-owned layers, enumeration failures, the exact PCI/Metal match
and all later device/drawable mismatch checks. Native Minecraft and Zink retain
their existing paths. Rebuild the Wine frontend for future full packages rather
than reusing the original v3.0.1 frontend. Both provider assembly and full-driver
packaging require current frontend source hashes, a Wine build record and matching
binary hashes; keep these checks when changing the release tooling.

`surface_device_test.mm` checks production binding with simulated adapters;
existing host checks retain small-BAR, async/reuse and lifetime coverage. On
Intel UHD 770 + CMP 40HX, the original Wine-default layer failed while explicit
NVIDIA and deferred layers each displayed 180 changing-color GPU-copy frames.
The affected user confirmed Star Rail rendering after installing the update.
AMD hybrid systems and muxless laptops remain untested.

The v3.0.1-only builder, updater implementation and transaction tests were
removed from `main` after that confirmation. Their original source remains in
[commit 0805a9d](https://github.com/HighDelay/nvidia-macos-driver-nvk/commit/0805a9d).
Retain the production bridge fix and `surface_device_test.mm`, and keep local
v3.0.1 recovery archives and the linked
[setup/rollback guide](../tools/bridge-update/README.md). Keep the v3.0.1 GitHub
release available as a prerelease.
Do not make this a recurring separate release asset. Full driver packages from
v3.0.2 include the repaired frontend normally.

## Small-BAR video-encoder headroom, 2026-11-16

Keep the display-aware framebuffer surface budget. For known BAR1 apertures
of at most 256 MiB, reserve one quarter of the aperture, with a 16 MiB minimum,
for other RM mappings and fragmentation.
The previous 16 MiB margin could not accommodate the retained Turing H.264
encoder's 33.5 MiB of CPU-mapped video memory at 1080p. If scanout and the
reservation fill the aperture, refuse new surface grants so the existing
system-memory fallback can handle them. Preserve larger and unknown apertures'
previous budget and fallback decisions, including the half-window budget at
8 GiB, and the capture completion repair.
This changes NVRMFB budgeting; codec binaries and OBS settings are retained.

On RTX 2060 SUPER / Tahoe 26.7.1 / 256 MiB BAR1, the loaded candidate was
verified after reboot and the user confirmed OBS recording works. The OBS log
records 2,039 frames at 1080p60 with hardware H.264; isolated 1080p hardware
H.264 and HEVC encode/decode roundtrips also pass. The budget regression fails
on the preceding implementation. Smaller apertures have host checks; larger
and unknown apertures, including 8 GiB, are checked against their preceding
policy. Other GPUs, Sequoia execution, higher resolutions and concurrent
encoding sessions remain unqualified for this repair.

## Metal metadata and binding caches, 2026-11-16

Keep the shared AIR digest and function-constant results restricted to text
marked immutable by the loader. Preserve literal function-table detection,
unrecognized constant-row counts and fresh analysis for external mutable text.
Surface cache keys must retain all seven identity fields. The resource-set
member cache contains only retained members and is invalidated on removal,
clear and transfer; identity membership and sealing ownership remain required.

Descriptor snapshots walk named mask bits in ascending order and preserve the
null-mask, storage-image, texel-buffer, descriptor-copy and GL-uniform paths.
Address snapshots update changed texture entries independently in each stage,
while keeping every GPU snapshot immutable. With `NVMTL_PROCESS_ENV=1`,
`NVMTL_SNAP_WALK=0`, `NVMTL_ADDR_TEX_INC=0` and `NVMTL_RESSET_CACHE=0` individually
restore the corresponding previous CPU path. `NVMTL_ADDR_TEX_VERIFY=1` compares
cached texture entries with a full rebuild. Keep diagnostic formatting and
empty visibility-group allocation out of ordinary render work.

A partial IOSurface wrapper must use private backing: its upload or GPU write
cannot validate untouched pixels in the family's VRAM. Whole-surface imports,
padded rows and the sRGB tiled publication bridge retain their existing paths.
Allow bounded 1D/1D-array texture views, with exactly one slice for a 1D view.

The private macOS 15-targeted plugin builds and passes 22 focused host suites,
including sanitizer coverage for cache lifetime, concurrent sealing and sparse
descriptor equivalence. On RTX 2060 SUPER / Tahoe 26.7.1, the partial-surface and
1D-view regressions fail on the preceding build and pass on the candidate.
The sRGB, render-feedback, capture-format, sampler-transition and command-binding
GPU checks also pass. These results do not qualify desktop installation,
Sequoia, other GPUs or application FPS. Kernel, Mesa, video-encoding and presentation
changes are outside this update; package and release preparation remain held.

## Optional notebook TGP manager, 2026-11-16

Retain the narrow backport of upstream
[`dfe54e0`](https://github.com/nullmoth/nvidia-macos-driver/commit/dfe54e0329308a8acd8de5e1246354454fcb557a).
It requires explicit `nvrmtgp=1`, battery detection and enabled system PM;
the RM helper also rejects non-mobile GPUs. It sends only the firmware's
TGP-mode enable/reset command under API-then-GPU locks, never a wattage or
generic control. The existing single-adapter telemetry contract is retained.

Keep the dedicated work-loop timer, stable per-service owner, gated lifecycle
transitions and terminal stop. Drain before closing the PM gate or tearing
down RM; resume only after successful RM/NVKMS recovery and PM gate reopening.
Cancel/disable queued work under the same gate. Preserve allocation-failure
cleanup, module-stop sysctl unregistering and atomic telemetry/live settings.
Unknown source readings cannot enable a budget and reset one already applied;
live disable works on unknown readings too. Bound failed enable attempts but
retain reset attempts for an applied budget. See [setup and rollback](NOTEBOOK-TGP.md).

The production manager and RM helper pass 49 checks under both memory and
thread sanitizers,
including charger decisions, missing readings, mobile/full-power refusal,
failure bounds, concurrent live settings and suspend/stop callback races.
Negative controls remove the mobile guard, drain or terminal stop and fail.
The private NVRM links against the unchanged, source-verified NVIDIA 620.70.06
archives; an isolated Tahoe collection includes the candidate and all eight
existing extension identities. Laptop wattage, charger/battery transitions,
thermal behavior, sleep/resume and Sequoia execution remain unqualified.
The historical display-sleep fixture fails identically on the unchanged base;
the other 11 selected existing host suites pass, with PERFMON tested separately.
Packaging, installation and release preparation stay held for this backport.
The stable Mesa, 2.8 NVK patch chain and broader power/token/display boundary
remain unchanged.

## Capture completion dispatch and sRGB imports, 2026-11-16

Keep the stateless surface GPU-mark path ahead of the NVRM user-client gate.
Capture pageoff can hold the accelerator lock while waiting for a ready mark;
WindowServer's next dirty escape can hold the client gate while waiting for
that accelerator lock. Serializing the ready mark behind the same gate closes
the cycle and forces capture to reach its 100 ms deadline.

Only an exact-size, current-version `NVRM_ESC_SURF_DIRTY` GPU mark bypasses
the client gate, mapping scope and PM accounting. It accesses only NVAccel's
separately locked pending table, so it needs no initialized RM adapter or GPU
power transition. Preserve NVAccel's WindowServer authorization, disabled-mark
fallback and readiness waits. Refuse new marks after client cleanup starts;
in-flight marks use no client RM/mapping state. Keep the accelerator service
reference retained and atomically published, releasing concurrent losers.
Ordinary surface and RM operations retain their existing gate and PM handling.

When a renderable linear IOSurface import lacks requested storage support,
retain it as `_surfaceLinear` and use an optimal image with native or compatible
UNORM storage views. Preserve the existing GPU import/publication ordering,
local backing cache and three-word generation tags. A compatible storage view
does not make the base sRGB view writable. Keep the renderability refusal,
private fallback on allocation/capability failures and direct linear imports
when tiled storage is unnecessary. CPU-modified or stale-tagged surfaces still
require the existing upload path; this does not eliminate every CPU copy.

`tools/test_surface_mark_dispatch.py` exercises production dispatch, completion
waits, authorization, concurrent service discovery and client cleanup with
ASan/UBSan and TSan. Its gated negative control reproduces the timeout.
Drawable/import, publication, queue and storage-view host regressions cover
the tiled bridge and retained fallbacks. RTX 2060 SUPER / Tahoe 26.7.1 / OBS
33.0.1 stacks establish the original lock cycle. Private NVRM/plugin builds
pass; the plugin passes eight sRGB bridge cases, 96 render/publication cases,
four existing storage-write cases and 16 capture-format readback cases on
that GPU. These GPU tests use the previously loaded kernel. Isolated and
installed Tahoe collections link successfully, retaining external extensions
at permanent paths. After reboot, the candidate NVRM is verified loaded and
the user confirms OBS capture stutter is resolved on that setup. Sequoia
execution and other BAR1/GPU setups remain unqualified. Package/release
preparation remains held.

## Login-window l10r frames, 2026-11-16

Retain the narrow backport of upstream
[`1285e17`](https://github.com/nullmoth/nvidia-macos-driver/commit/1285e179d2593bd6700b035215c8ffcea552a765).
Tahoe can present login/lock frames as `l10r` (ARGB2101010). The earlier selective
integration imported its conversion helper but retained a BGRA-only accelerator
copy path, leaving these frames refused and the panel black with a live cursor.
This matches the reported RTX 2060 / Tahoe 26.7 / 256 MiB BAR1 symptom; it is a
format-handling defect, not a reason to change BAR1 sizing or allocation policy.

Convert only copied l10r pixels into the existing 8-bit scanout. Preserve BGRA
copies, the BGRA-only zero-copy flip, GPU/source-readiness ordering, mapping
bounds and the existing NVIDIA 620 interface. Intel-panel l10r frames must bypass
raw copies, retain the frame lease through conversion, and finish or cancel it
once. Convert the visible width only, validate destination pitch and retain
`debug.nvaccel_iop_l10r`. Broader HDR/30-bit scanout changes are not dependencies.

`tools/test_iop_login_frame.py` runs the production transaction, source mapping
and frame lease: 34 sanitizer cases pass, with refusal/raw-copy negative controls.
Existing BAR1, console parking, flip-validity, Intel-panel and gamma-guard checks
also pass. These host checks do not qualify a real login or either accelerator
binary. Driver builds, installation and release preparation remain held.

## WindowServer small-image pooling, 2026-11-16

Keep optimal-image pooling enabled by default only for the exact `WindowServer`
process on NVIDIA. Other applications retain their existing opt-in behavior;
`NVMTL_IMAGE_POOL=0` restores individual allocations. Preserve the 4 MiB image
limit, 32 MiB blocks, two retained idle blocks, ordinary-allocation fallback and
the separate external, linear, placed, large and dedicated-image paths.

Publish fresh VRAM pool blocks only after their GPU clear succeeds. Heap creation,
clearing and destruction must stay outside the pool lock because retirement can
take that lock. Recheck for a usable block after clearing and discard redundant
private blocks; a failed clear must release its block and fall back. Retain the
two-thread publication/failure fixtures in `tools/test_image_pool.py` and the
concurrent texture/corner-readback coverage in `tools/bench_texture_churn.m`.

On RTX 2060 SUPER with macOS 26.7.1, isolated texture-creation CPU fell about 46%.
GPU correctness checks passed, and the user reported normal rendering and lower
CPU after reboot. Manual interaction captures averaged about 60% then 34% of one
core, but the scripted desktop phases showed no consistent reduction. These
results do not establish a general desktop gain or qualify other GPUs/OS versions.

## Native NVK D3D12 descriptor sets, 2026-11-14

Keep the matched vkd3d-nvk `3.1-nvk.2` pair, source patch, licenses and build
provenance in the driver-owned Wine provider. The patch gates descriptor sets
on both `VK_DRIVER_ID_MESA_NVK` and Wine's Darwin host before device creation;
other drivers/hosts retain their existing paths. Preserve the upstream
`skip_driver_workarounds` diagnostic opt-out and explicit extension disabling.
The DXVK pair and launcher contract remain unchanged. Review the
[VKD3D notes](https://github.com/HighDelay/vkd3d-nvk/blob/master/docs/NATIVE-NVK.md)
before changing the binding policy or replacing the source-identified pair.

The user confirmed that the descriptor-set configuration trial restores ZZZ's
missing player character. The compiled pair passed automatic default/opt-out,
DXIL compute/readback, rendering, presentation and resize checks. Full staged
package and compiled-pair game testing remain pending; publication stays held.

## Native NVK limited-BAR mapped memory, 2026-11-14

Preserve the [native mapped-memory repair](NATIVE-NVK-SMALL-BAR.md). On Darwin,
expose host-visible device-local VRAM only when BAR1 covers VRAM; limited-BAR
devices use the coherent system-memory type for CPU uploads. Do not advertise
an unused BAR heap. Preserve the original memory layouts and placement for
full-BAR devices, device-only rendering memory, non-Darwin backends and the
GPU-copy presentation path. Keep the patch in the matched `2.8` release chain,
the actual-initializer topology comparison and the LWJGL/VMA GPU readback test.
Physical small-BAR and full-BAR application results must remain separately stated.

Retain the limited-BAR drawable fallback for exact invalid-external-handle
refusals: GPU-copy into imported coherent IOSurface host pages, with checked
alignment/size, transfer visibility, lock/seed publication, cache re-lock and
timeout quarantine. Full-BAR devices skip its capability query and extension
addition and keep the original import/publication path. Preserve both the
sanitizer lifecycle fixtures and the real GPU forced-import pixel/resize test.
Minecraft startup, world rendering and resize were user-confirmed on the
physical 256 MiB BAR1 desktop after restarting the launcher. This does not
qualify the complete staged package or replace a physical full-BAR rerun.

## Modern upstream 3.0 integration, 2026-11-14

The merge of `87e524ab36f12d17c04626d6cf7fc3bde1cc63e4` uses
`7412cdde81447f78da0ebb64216dc258d8beaedb` as its common ancestor.
Follow the [scope, equivalence review and validation limits](UPSTREAM-3.0-INTEGRATION.md).
Legacy macOS support is excluded at the user's request. Keep stable Mesa 26.3.0,
the 620.70.06 mapping ABI and the default/release `2.8` patch chain; append only
the compatible uncached-stream patch. Deferred 2.9/3.0 power/token/display
implementations are not qualified by the merge ancestry.

Preserve full surface-ID/plane keys and linear-image exclusions in dirty-mark
deduplication, overflow-safe scanout reservation with allocation fallbacks,
the deferred-connect registration recheck, and worker-first failed-start cleanup
with the fork's Intel-panel controller grouping and quarantine. A slow utilization
query disables later queries without bypassing power accounting or locking.
Keep the stronger existing bounded AIR caches and object-identity HiDPI ownership
instead of duplicating equivalent upstream fixes. Preserve all translator,
native OpenGL/Minecraft, Wine/async/reuse, MoltenVK, codec and Safari repairs below.
WhateverGreen boot-argument ownership must fail closed on an unreadable NVRAM
store, preserve user choices and retain recovery state after failed removal.

Automatic Safari WebGL helper setup now detects EFI partitions by structured
diskutil type and validates OpenCore/Lilu on each. Select only a unique compatible
candidate; multiple candidates require a choice or explicit override. Manual
remains the default. Keep mount cleanup, vault/signature checks, guarded
backup/restore and no-op handling; restore requires the original explicit target.

Thirty focused host entrypoints, production scale ownership, private Metal
readbacks, all 43 native Zink cases and an isolated kernel-collection link passed.
The broad snapshot has no new failures against the fork baseline, with 76
historical failures still present. The changed RM source compiled as an object;
the isolated link reused the recorded installed RM, so a complete new RM link
and boot remain untested. Full-driver build/release remains held.

## Zink/Gallium 26.3.0, 2026-11-14

The [Zink stable update](ZINK-MESA-26.3.0.md) advances the independently checked
frontend from `17ca6174` to `0a6fa60abb737d26ac8a316a4aad7c9ddf64f9e4`.
Use the new zero-initialized `broken_invalidate` field instead of the removed
`get_param` callback. Rebase the native overlay against this exact Mesa tree;
never change only the reported version string.

Upstream now covers the locked submission broadcasts, predicate wait loops,
monotonic timed waits and additional fence/reset synchronization. Preserve its
implementation instead of replaying the old submission hunks. Retain the locked
usage/unflushed predicate update, the batch generation guard in both wait loops
and the Darwin barrier teardown repair.
Keep native Vulkan selection, descriptor sets, staging/GPU texture uploads,
bounded texture-worker payloads, coherent system-memory preference on small BARs,
failed-map handling/diagnostics and IOSurface write-generation tracking.

The frontend, CGL adapter and native Vulkan bridge must retain matching build
records. Native GPU-copy presentation, capability filtering, shared-context/HDR
interop, OBS compatibility, the macOS 15.5 minimum and the no-exact-build-gate
policy remain. The full-driver build/release hold still applies; prior Sequoia
and small-BAR application results do not qualify the new frontend automatically.

## Mesa/NVK 26.3.0, 2026-11-14

The [stable Mesa update](MESA-26.3.0-UPDATE.md) advances NVK to official
`0a6fa60abb737d26ac8a316a4aad7c9ddf64f9e4` while retaining the 620.70.06
kernel interface, compatible macOS backend, slab/recycler defaults and DLSS
view-lifetime repair. Keep the default/release-input patch chain on `2.8`;
Mesa version selection comes from `nvk/MESA_BASE`. The upstream 2.9 token,
hub and power changes remain deferred. Keep Zink's independent frontend ABI
pin and the Wine provider/DXVK contract, MSync and presentation-reuse fallback.

Use upstream's equivalent CoreGraphics dependency fix from
`nvk/nvk-metal-wsi.patch` independently of its software-present policy. Native
applications retain the fork's GPU-copy bridge. Both native compiler tools need
matching patched-source identities; keep the no-fuzz patch and system-library
dependency checks. Do not apply the older optional NAK patch wholesale to stable.

Minecraft 26.3 can select the existing bridge using the documented
`org.lwjgl.vulkan.libname` JVM argument. Preserve vanilla feature checks and
SDL's provider alignment. The companion selector mod is optional; an ICD
manifest does not override LWJGL's bundled MoltenVK preference by itself.

The stable renderer is installed in the global/native/Zink/Wine paths and passed
installed-path GPU probes. The separate async Wine trial passed automated tests
and reached ZZZ gameplay. The user compared its synchronous fallback and final
async repeat, preferred async and approved source promotion to main. The full
driver build/release remains held. Automated RTX
2060 SUPER/Tahoe checks do not qualify WindowServer, a new boot, Sequoia,
Blackwell, or every application.

## Bounded Wine presentation worker, 2026-11-14

Preserve the separate lifecycle gate and bounded presentation worker described
in [the async presentation record](WINE-ASYNC-PRESENTATION.md). Drawable acquisition
and the rendering/copy fence wait must not hold the application submission lock
in async mode. Keep the separately reserved copy-queue requirement, synchronous
fallback/opt-out, copied caller arrays, source-image reservations, fenced rejected
handoff drain, late-error reporting, idle/resize/teardown drains, process-exit join,
HUD lifetime and timeout quarantine. Native Minecraft and Zink stay synchronous.
Record the worker header in every frontend's source identity and the Zink
packaging guard. User application qualification and the full-driver release hold
remain separate from host fixtures and private GPU correctness checks.

## Safari WebGPU argument-buffer textures, 2026-11-13

Preserve recognition of sole-use i64-loaded texture handles before raw-buffer
emission, independently of BDA. Raw sampler retries must retain the required
address model for dereferenced buffer pointers nested in argument groups,
while keeping directly bound word-buffer/atomic cursor behavior. Keep the
separate texture-slot and nonzero-uniform readbacks; successful shader
compilation can still hide zero-valued resource reads. Preserve multisample
typing through embedded texture discovery, reflection, loaded values and
descriptor heaps: sample indices must lower to `Sample`, not mip `Lod`.
Query-only shaders still need embedded texture descriptors. Keep the distinct
four-sample resolve and dimension/mip/sample-count readbacks through both
argument encoder paths. See the
[Safari WebGPU repair record](SAFARI-WEBGPU-TEXTURES.md) for the reproduction, source
constraints, private GPU results, and user-confirmed Safari result on RTX 2060
SUPER / Tahoe 26.7.1. Full driver build/release remains held at the user's request.

## Repaired upstream 2.9 integration, 2026-11-10

After the selective backports below, the user approved the broader integration
and local test packages. Follow the [integration decisions](UPSTREAM-2.9-INTEGRATION.md)
when reconciling future changes. This supersedes the earlier hold on merging
`7412cdd`; it does not authorize replacing protected fork behavior or publishing
before local testing. Keep the matched kernel/Mesa interface and existing
shutdown/backlight paths while integrating repaired 2.9 loader/translator work.
Preserve the exact validated local archives for a later release.

## Selective upstream 2.9 backports, 2026-11-10

The user approved the narrow scope in [the 2.9 review](UPSTREAM-2.9-REVIEW.md):
take BAR allocation/address preflight from `3889d54` and package manifest
completeness from `28afa39`. Keep the NVIDIA 620.70.06 / upstream 2.8 base;
do not mark the entire `7412cdd` range merged. Unsafe or redundant shader,
surface, shutdown, backlight, codec and Safari alternatives remain excluded.
All existing NVMTL optimizations and fork implementations/repairs are retained.
Do not resolve future overlapping code by replacing it wholesale with upstream.

The root-owned private installer copy verifies every digest before executing
its covered `sums_complete.sh`; the checker rejects unlisted regular payload
files and understands the fork's escaped checksum names. The build verifier
also requires completeness. Wine provider input directories retain their
verified-file allowlist: local Wine/cache extras are never copied and need
not be hashed as installable files. Complete delivered package manifests remain
mandatory. Preserve the Intel/audio/DLSS/WebKit prompts and rollback behavior.

BAR1 sizing and display/startup policy stay unchanged. Allocate and populate
the full replacement aperture list before touching PCI configuration, clip
physical placement to CPU/IOKit limits, and drop stale BAR1 descriptors using
the full old 64-bit address. Keep allocation-failure cleanup and PCI readback
rollback. The [NVRM build recipe](NVRM-BUILD.md) rebuilds the same 620 core with
the fork's five existing patches; two patch contexts are mechanically updated
for 620, whose FSP cleanup already clears its poll pointer. Do not add the 2.9
YUV/color/power patch chain while rebuilding this backport.

## Native Zink HDR IOSurface export, 2026-11-08

The shared-lock repair below alone still left Stremio laggy on the local host
and the affected laptop. Actual `CAOpenGLLayer` output is RGBA16F, 1920×943,
with a 15,360-byte row; the earlier libmpv probe exported BGRA8. Retain the
GPU-local conversion buffer followed by a buffer/DMA copy into imported
IOSurface pages. Direct compute stores into those host pages dominated the
remaining publication wait. `PIPE_USAGE_IMMUTABLE` selects device-local staging
even when the small-BAR policy puts ordinary mappable buffers in system memory.

Reuse one growable buffer per context, release it on normal context teardown,
and retain it with that context after uncertain GPU completion. Keep the direct
copy as an allocation-failure fallback. Preserve row padding, plane offsets,
the completion fence, surface lock/seed ordering, and the shared-context repair.
There is no CPU pixel copy and no codec or presenter policy change.

On the RTX 2060 SUPER / 8 GiB BAR1 host, the real Stremio layer's mean wait fell
from 34.7 ms to about 1.7 ms; the user confirmed substantially smoother playback
with Zink enabled. Keep the HDR/padding/resize transfer probe and existing
interop/concurrency tests. This is not an Apple-OpenGL parity result or a
confirmed RTX 5070 Laptop result; that machine uses the same path but needs its
own playback test. Detailed evidence is in the synchronization record below.

## Stremio shared-IOSurface synchronization trial, 2026-11-08

Retain native Zink and the target-filter fast path for ordinary non-rectangle
texture binds. Keep the short share-group list lock, refcounted binding snapshots
and per-IOSurface transfer locks, shared by separately imported aliases/planes.
An unrelated shared context's uploads, publication and teardown must not wait
behind a different surface's GPU fence. Same-surface consumers still wait for
completion, with exact pixels, seeds and external refresh ordering preserved.
Retain deleted/replaced bindings through queued writes and active snapshots.

Keep the pre-flush before reading backend write generations: deferred clears
acquire their writer/generation during that flush. Uncertain copies need their
own idempotent quarantine reference even if another snapshot has retired their
normal list entry. Never free imported pages, a locked surface or its image
merely because the last snapshot was released after a timeout.

The sanitizer, forced-fence negative controls, shared-context/alias/lifetime
probes and real-libmpv harness are described in
[the trial record](ZINK-STREMIO-SYNCHRONIZATION.md). Local RTX 2060 SUPER / full
BAR1 tests are not Blackwell-laptop playback or a reproduced fix for the old
query/descriptor crashes. Codecs, kernels, NVMTL, native Minecraft/Wine runtimes
and existing Zink presentation/compatibility policies remain unchanged.
No driver installation, draft, tag or release is authorized by this local trial.

## Upstream 2.8 integration, 2026-11-08

Merge `60fb7255e18c31fe0747091fd9a961f30c1cd82a` onto fork `02027c1`,
using `18ba05f` as the common ancestor. Resolve four textual conflicts in the
plugin builder, installer, uninstaller and GL descriptor-cache fixture. Keep
release `-O2`, both required frameworks, optional audio ownership, native GLD
removal and the extended high-binding/fetch-mask tests. Use the production
slot guard in that fixture and give its fake buffers plausible aligned handles.

Take NVIDIA 620.70.06, its mapping-node/escape hardening, colour-format selection,
firmware identities/fallback and updated card list. Use the signed official
**2.8 RM, firmware and renderer** together. Rebuild the fork framebuffer and both
accelerators against NVIDIA commit `e6a2e01579fbbd1ca2ed43fef2ee2589e9bd8bd9`;
older NVKMS layouts are incompatible. Pin the Intel-panel builder and record
actual included headers too. The public source still lacks Darwin RM/KMS link
archives; the official RM sources match except for the fork's build-path default.

Take the bounded per-translation parser cache, typed null-resource slots,
Metal 4 wrappers, compatible texture copies, channel-order pipeline variants,
lazy heap backing, heap-pin reuse and native MetalFX DLSS integration. Preserve
the fork's bounded address cache, recording/binding/resource caches, surface
publication/capture, Photo Booth storage/orientation, Spotlight half-float mips,
Safari renderer repair, all-app ANGLE and exact completion-witness policy.
Native Zink/CGL/IOSurface/AnyDesk/resize/small-BAR, Wine/DXVK/vkd3d/NGX/NVAPI and
role-scoped codec/sandbox repairs remain; their frontends use the matched 620
renderer. The staged IOSurface record remains ABI 3 and the public bridge exports
are unchanged. Do not apply the separate NVRM mapping proposal as an actual patch.

Correct two upstream defects. A lazy backing object must keep its address after
its metadata lock is released: store separately allocated backings behind the
growable pointer index, and release them with the heap. The original array's
`realloc` could invalidate a concurrent binder's pointer. Bound slot arithmetic
and preserve allocation-failure fallback. DLSS views must remain tracked until
their image dies: grow the initial cache to a bounded 4,096 entries, reuse holes,
and refuse an evaluation on allocation/capacity failure instead of leaking an
untracked Vulkan view. Both production-snippet sanitizer tests pass and reproduce
the respective defects on unmodified upstream as negative controls.

Native MetalFX DLSS has a separate installer prompt, default **No**. Keep
`--dlss=yes|no`, the upstream `NULLMOTH_DLSS=1` alias, noninteractive default No,
checked root-owned tools and pinned NVIDIA downloads. Explicit arguments win over
the environment. Explain that Wine/CrossOver uses its independent provider setup
and the game's `nvngx_dlss.dll`; declining the native download keeps existing
files and does not modify Wine. Include signed local helper builds, their
licenses and the [native DLSS guide](METALFX-DLSS.md) in the installable package.
Keep audio and Intel-panel choices independent and default No.

The DLSS coverage guard must accept both `./relative` paths from the fork's
packager and bare paths in standard text/binary checksum records. Anchor the
whole record so a different filename ending in a helper's name cannot count as
coverage. Exercise the actual guarded DLSS step with `stage_package.write_manifest`
output: testing the prompt function alone missed a silently skipped prompt.

The stager carries release versions and dependency minima onto rebuilt/reused
core kexts while retaining fork personalities and additional dependencies. Re-sign
a changed plist. Do not ship the accelerator template version `0.1` or an old
AGDC release version in a new package; packaging alone needs no recompilation.

Validation on RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1 includes 2,247 translator
library tests and 44 wrapper tests (two existing ignores and two unavailable
private fixtures excluded), the retained driver host suite, 15 installer-choice
and payload-coverage tests, DLSS worker/download/OS-version fixtures, sanitizer
checks and offscreen GPU readbacks. Update the callback fixture for existing timing diagnostics and
the OS-version fixture for a real host older than its application's profile floor.
Keep original assertions and never weaken missing-validator failures into passes.
Package signatures, manifests, OS minima, core/optional kernel-collection links
and asset digests are recorded beside the local packages. These are isolated
tests against the current kernel, not a new 620 boot, laptop/audio qualification,
or native DLSS SR/RR/FG gameplay test. Other GPUs and Sequoia remain unqualified
for this combined build. The installed driver is unchanged; preserve the exact
local archives for user testing and later release approval.

## Gecko sandbox media and texture storage, PR #8, 2026-11-07

Merge `bf38d7affa37810a3728f81e6aa7329453aa272a` onto `e126221`.
Resolve the video-registration and test conflicts by retaining role-scoped
addon registration, configuration-before-bootstrap ordering, disable settings,
native sandbox delegation and generation selection. Recognize Gecko RDD/GPU
children from bounded original argument strings despite pointer-array removal;
reject invalid parent PIDs and argument counts. RDD registers synchronously
after configuration; encoder services retain their separate asynchronous path.
RDD receives only decoder factories, and denied bridge access grants no exception.

Preserve the narrow `/dev/null` token fallback in codec libraries and the native
Vulkan frontend. Rotate codec descriptors within a bounded window, advancing
beyond the actual `F_DUPFD` result under a lock: the original PR could immediately
reuse a result when lower slots were occupied. Keep errno, access modes, close-on-exec
and low-limit/exhaustion behavior. This reduces immediate teardown reuse without
changing the kernel connection ABI. Keep import rebinding local to pinned images,
validate Mach-O tables, restore actual VM protection and release VM object ports.
The renderer and already-loaded sibling Zink library share the same helper;
their shared-header hashes are part of frontend build/package provenance.

Include `gl3ext.h` in CGL generation, excluding comments and multiline macros.
This exposes public texture-storage slots and `GL_ARB_texture_storage` without
changing existing slot offsets. Rebuild CGL and the native/Zink/Wine frontends
for packaging; Mesa, codecs, kexts and their source overlays are unchanged.

On RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1, host/sanitizer checks cover 37
registration scenarios, sandbox delegation, descriptor contention, 384 threaded
token opens and 512 repeated import binds per C/C++ harness without port leaks.
GPU checks pass 2D/3D/array immutable storage and mip readback with denied null
opens/page-size queries; the old adapter fails this probe. VP8/VP9 each decode
12 pixel-checked frames synchronously and asynchronously in both service and
shuffled-argv RDD-style probes. H.264/HEVC encode eight packets each; H.264 hardware
session creation and 16 concurrent HEVC-teardown/VP9-startup rounds also pass.
Spotlight mip/A8/Unknown-usage, Photo Booth, Safari and surface regressions pass.
Sandbox GPU probes isolate the relevant denied operations; they do not reproduce
the complete Firefox/Zen profile. Browser-level playback, Sequoia and other GPUs
remain untested. Firefox's first H.264 capability query can still precede plugin
registration; do not report its startup status column as repaired by this merge.

## Spotlight half-float icons and texture correctness, 2026-11-07

Preserve automatic mip generation for eligible full level-zero uploads of
`R16Float`, `RG16Float` and `RGBA16Float` client-storage images. CoreAnimation
requests mipmapped half-float app icons but supplies only level zero; leaving
the smaller levels undefined makes Spotlight icons disappear when minified.
Use float-aware GPU blits to retain extended-range colors. Keep the existing
size/region/view guards and stop automatic generation after an explicit higher
mip write. Do not ship the diagnostic CGImage conversion or Spotlight hooks.

Also retain the tested format/storage corrections: publish the format-query
function with the instance dispatch's `pthread_once`, allocate writable backing
for valid `MTLTextureUsageUnknown` textures before making mip/slice views, and
use identity Vulkan attachment views for A8. Remap A8 framebuffer reads using
the pipeline's attachment format; sampled-view swizzles remain separate.

On RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1, the user confirmed Spotlight app
icons after restarting the session. GPU probes cover half-float checker
averaging, odd dimensions, extended-range values, explicit mip ownership,
Unknown-usage writes and A8 framebuffer reads. The preceding binary fails the
half-float mip probe; the repaired binary passes. Preserve PR #5 caches, PR #7
surface publication, Photo Booth, Safari and codec behavior. Other GPUs and
Sequoia have not been qualified by this icon test.

## Codec registration by process role, 2026-11-06

Keep decoder/encoder service registration independent of Metal device creation.
Start it only after `nvmtl_conf_settle()` completes; the asynchronous worker must
not race configuration initialization. Built-in generation-specific libraries
and the additional codec library register only the service's own role.

Retain `NVEncVT_RegisterCodecs(rating, scope)` (1 decoder, 2 encoder, 3 both), the
legacy combined wrapper, codec-disable settings and native sandboxed decoder
delegation. The addon must use the same client policy as the built-in decoders;
do not let its Ada/TU117 decoder factories bypass the QuickTime repair. Require
the matching version-2 API in build/package records. Keep MX450 IDs `1f97`, `1f98`
and `1f9c` excluded from both codec paths without excluding GTX 1650/MX550.

Policy tests cover all existing addon families, service roles, disabled codecs,
invalid scopes and retained codec sets. Isolated RTX 2060 SUPER / Tahoe tests
register through the plugin constructor with one plugin image and no Vulkan
renderer: H.264/HEVC hardware encoders return eight packets each, and VP8/VP9
hardware decoders return twelve pixel-checked frames each in synchronous and
asynchronous mode. The probe-only Metal-enumeration shim prevents the installed
plugin loading alongside the candidate; never ship or apply it to applications.
These are process-role probes, not tests inside Apple's actual XPC sandbox.

Do not equate smooth Safari playback with hardware decode: the live GPU-process
sample used `AppleH264SW`, and the sandbox denied executable mapping of
`/Library/GPUBundles/NVMTLDriver.bundle`. Service registration does not override
client codec discovery. No Safari sandbox change or general cross-GPU playback
qualification is included. Retain the separate Safari renderer-lifetime repair.
Local evidence is under `out/records/video-audit-20261106/`.

## Upstream 2.7.2 integration, 2026-11-06

Integrate `18ba05f38b5eadea8b5ae05834bb51c61fb4c5c3` on top of fork `90b42b2`.
Resolve three textual conflicts: keep both depth-bounds state and the fork's
GPU feedback attachments in `nvk_cmdbuf`, retain vertex-stride invalidation
alongside depth-state restoration after a pipeline bind, and combine the Intel
panel's kernel includes. Keep the per-recording pipeline/descriptor/resource
caches, bounded lowering cache, submit-ahead, surface publication and quarantine.

Upstream removes kernel pointers from IORegistry. Port the fork's Intel-panel
`kapiInit` to the versioned `nvDisplayPointers` kernel call, reject failed requests,
and copy only the GPU ID when publishing panel nubs. Preserve its leases, RAM
scanout opt-in, brightness, parent claim, cancellation/drain and rollback. Apply
upstream's supported-kernel check to the fork's additional claim class too, before
it can take the firmware display category. Audit the five root-only panel/copy
sysctls explicitly; retain the audit's unlisted-key and `CTLFLAG_ANYBODY` controls.

Take the new Metal required-size getters, depth bounds, descriptor-based views,
Metal 3 attachment-map behavior, capability checks, inherited-environment/library
policy, mapping locks/snapshots/retains and profiling access restrictions. These
do not replace the fork's existing repairs. Zink/CGL/Minecraft, native Vulkan,
Wine/DXVK/vkd3d/DLSS, codec overlay, translator, optional-component installers,
Safari adapter, Photo Booth adapter and submission/callback implementations are
byte-identical to `90b42b2`. The removed exact-build Zink guard stays removed;
upstream's major-OS support policy does not add a minor-version CGL whitelist.

The release plugin builds for macOS 15.0, both accelerator variants build, and
framebuffer, Intel panel, primary selector, DRM and changed RM sources compile.
The focused host suite passes 39 entrypoints, plus capability and environment
tests. On RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1, isolated GPU tests pass 20
new-feature cases, 96 IOSurface cases, 15 sampler cases, 128 surface handoffs
without stale pixels, 32 Core Image frames and 64 custom-drawable presentations.
The preceding plugin fails six new-feature assertions as the negative control.
Core-only and optional-panel/audio kernel collections both link separately.

Future packaging must use the verified official **2.7.2 NVRM**: an older RM does
not provide the new display-pointer API. The official archive's SHA-256 and pinned
Ed25519 signature were checked; its RM sources match this integration. Rebuild
the fork framebuffer, both accelerators, panel and plugin rather than substituting
stock binaries. This validation does not install anything or qualify a new boot,
Sequoia execution, physical laptop/RAM scanout, or AMD/Intel rendering. The current
driver and kernel collection remain unchanged. Source/build records are under
`out/records/upstream-2.7.2-20261106/`. Package installation and boot
testing are separate from these source-integration checks.

## Experimental WebKit GPU loading helper (2026-11-10)

`kexts/NVWebKitFixup` is a separate, opt-in Lilu plugin for the local Safari boot
test. Keep its exact kernel/prologue gates, Apple platform/signature/identity
checks, root-owned/restricted runtime checks, private X/RX bounds and read-only
null-device check. It routes only Sandbox's open/mmap callbacks; do not turn it
into a global sandbox/AMFI bypass or raise prior maximum mapping protections.
The 0.2.0 candidate admits Darwin 24 (Sequoia) and 25 (Tahoe) only, then checks
the required symbols, exact frame-setup/register-save layouts, bounded variable
stack-frame sizes and Lilu's decoded instruction lengths. It no longer pins one
kernel version string. Retain the range/complete-instruction checks; never accept
arbitrary instructions merely because they can be decoded. Unknown layouts stay
inactive. The OpenCore entry must also span `24.0.0` through `25.99.99`.
Preserve its disable argument and diagnostics.

The matched `nvmtl_sysconf` repair handles denied page-size and physical-page
queries using the cached Mach page size, `hw.memsize` or host information. It
preserves normal results/errors, bounds the result and releases acquired host
ports. Do not restore the old `getpagesize()` fallback, which WebKit also denies.

`nvmtl_icd_open` also handles WebKit denying both a null-device open and `pipe()`
by duplicating its inherited read-only null stdin. Validate the owned duplicate
against the root-owned null device, preserve close-on-exec and never consume or
close the original stdin. Do not substitute an unverified inherited descriptor
or change ordinary open/error behavior.

Policy/callback ASan/UBSan tests, page/memory tests, GPU texture regressions and
the isolated kernel link check pass. Version 0.1.0 subsequently booted with Lilu
1.7.3 on RTX 2060 SUPER / Tahoe 26.7.1. A real WebKit GPU process created a context,
compiled shaders and returned the expected pixels without a GL error; normal
Safari rendered Aquarium with 500 fish at a page-reported approximately 73–75 FPS
on a 75 Hz display. The user confirmed visible, moving fish. Safari retained its
normal private message-filter mode. The 0.2.0 compatibility extension has matching
MAC/signing-query declarations across 13 published Apple XNU tags, OS/initialization
gate tests and an isolated Tahoe link check; it has not yet booted on either OS.
No Sequoia runtime or kernel-link test has been performed. Other kernels/GPUs and
extended use remain untested. Keep helper activation optional and retain the
release approval requirement and existing Safari video, icon, codec
and Zink behavior. Future packaging bundles it under `Optional/NVWebKitFixup/`,
with Manual (default), Automatic and Skip choices. Automatic installation changes
only its Kernel/Add entry and kext on an explicitly selected EFI, checks Lilu and
vault state, and retains a guarded recovery backup. Never add this Lilu plugin
to the normal system extension repository or kernel collection.

## Safari navigation frame-conversion latency (2026-11-14)

Preserve the immutable-AIR analysis caches in `NVMTLLibraryLoad.m`. VideoToolbox
recreates functions and pipelines for transfers, so caching only on those objects
repeats large visible-function-table and grid-only scans. Keep successful positive
and negative link-query results on driver-produced immutable text; query failures
must still use the current error fallback and permit recovery. Grid-only results
also include the entry name, with at most 32 cached entries per text. External or
mutable text must remain uncached. Keep the existing grouping and linking rules.

Retain the translator's shared replacement-token map across blocks during static
initializer folding, and the per-call parse-cache scope around sampler
specialization. These avoid repeated work without changing shader output or
retaining parsed modules across calls. Preserve the Safari video/filter policies,
WebKit feature settings, decoder selection and sandbox behavior.

On RTX 2060 SUPER / Tahoe 26.7.1 / Safari 27.0, captured page transitions waited
for GPU-process video-frame conversion. Three isolated NV12-to-BGRA conversion
cases improved from 190–315 ms to 35–48 ms on repeated calls, with identical
pixel fingerprints. First conversions with empty caches improved from 5.1–7.3 s
to 3.3–5.0 s; first-seen variants still require compilation. Forty-one translator
comparisons matched output bytes or refusal results, and 12 sampler GPU recordings
matched expected pixels. Analysis-cache and retained WebKit/sampler/cache checks
pass. The compiler suite passes 2,266 cases; its existing derivative-validator
negative control fails equally on unchanged source with SPIRV-Tools 2026.4.
The wrapper suite passes 48 self-contained cases; two require unavailable local
captures and two are explicitly ignored. After installing the signed userland
components and relaunching Safari, the user confirmed clearly faster YouTube
navigation with normal playback. Other GPUs and operating systems remain
untested for this optimization. First conversions can still compile shaders.

## Safari filtered-icon compatibility trial (2026-11-09)

`NVMTLWebKit.m` also selects WebKit's software CSS/SVG filter implementation
when `GraphicsContextFiltersEnabled` exists. The exact Facebook WebP assets and
`invert/sepia/hue-rotate` filter reproduce missing onscreen icons in a separate
WebKit window; the alternate implementation renders them. This is a filter
fallback, with possible CPU cost for large filtered elements, not a hardware
WebGL or Safari codec fix. The user confirmed reaction and profile-popup icons
render correctly after a normal Safari relaunch on RTX 2060 SUPER / Tahoe
26.7.1. Other GPUs and operating systems have not been tested for this policy.

Keep the exact Safari/NVIDIA scope, ephemeral updates, API checks and existing
video policy. Do not change GPU-process, WebGL, canvas, DOM-rendering, protected
content or persistent browser settings. `NVMTL_WEBKIT_FILTERS=0` disables only
the filter policy; `NVMTL_WEBKIT_STABLE_VIDEO=0` disables only the video policy.
The real WebKit API test covers both policies and all four opt-out combinations
under ASan/UBSan against system WebKit and Safari's staged framework.

Safari's GPU subprocess separately reports a sandbox denial of executable
mapping for `/Library/GPUBundles/NVMTLDriver.bundle`. Apple's staged ANGLE can
compile shaders, draw and read correct pixels after preloading the driver in
a standalone process before a matching mapping restriction. That is a loading
reproducer, not a repaired Safari GPU process: the plugin cannot initialize in
a process that blocks loading it. Preserve this distinction and do not disable
the sandbox or modify Apple's binaries/profiles as a driver-side repair.

## Safari MediaSource visibility repair (2026-11-06)

Retain `NVMTLWebKit.m` and its call after successful NVIDIA NVMTL device creation.
Intel Safari can use a software VP9 decompression session while its renderer
preferences predict otherwise. Visibility changes then flush queued video and
stop the shared audio/video timebase until frames are re-enqueued. This reproduced
with local, fully buffered MediaSource fixtures as well as YouTube; 360p also paused.

The adapter selects WebKit's existing `MediaSourcePrefersDecompressionSession`
mode through its ephemeral preference-update API. Keep the exact Safari process
and bundle check, main-thread initialization, runtime feature/signature checks,
same-image API lookup and pinned callback lifetime. Apply it to existing views and
new/replaced configurations. Do not replace this with video-frame JavaScript,
persistent browser defaults, private C++ object offsets or suppression of real
flush/error notifications. `NVMTL_WEBKIT_STABLE_VIDEO=0` in `nvrm610.conf` disables
the video policy after relaunching Safari.

Preserve native decoder-service delegation, Chromium's sandbox/bridge checks,
codec ratings, generation selection and the separate protected-content setting.
This changes renderer lifetime, not NVDEC/NVENC registration or codec binaries;
Safari VP9 hardware-decoder discovery remains separately unqualified.

On RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1 / Safari 27.0, the clock-only VP9
scrolling test improved from 3.220 to 7.800 seconds of media progress over 7.800
seconds of wall time. H.264 stayed at 7.800 seconds. Renderer flush requests fell
from 26 to zero; both clips had over 49 seconds buffered during scrolling. The
user also confirmed continuous YouTube playback while scrolling, switching tabs
and minimizing an overlapping Edge window after a normal Safari launch.

The real WebKit API harness passes with ASan/UBSan against both system WebKit
and Safari's staged WebKit, including autosave-named preferences without writes
to user defaults, existing/new/nil/replaced configurations and unchanged protected
content flags. Hardware VP8/VP9 pixel comparisons and H.264/HEVC encoder/decoder
roundtrips pass. Codec payloads are byte-identical to the preceding installation.
Other GPUs and Sequoia playback have not been tested for this repair; an absent
feature or unfamiliar API leaves WebKit unchanged. Local records and rollback
instructions are under `out/records/safari-video-repair-20261106/`.

## Merge policy

Keep the fork's Tahoe fixes, existing performance optimizations, OpenGL/Minecraft changes and
MoltenVK optimizations when merging upstream. This policy was confirmed on 2026-10-17.

Before each merge:

1. Fetch the requested upstream revision and inspect its changes since the previous merge.
2. Check for both textual conflicts and changes to the behavior listed below. A clean Git merge
   does not establish compatibility.
3. Check whether upstream has already implemented each fix. Use its implementation when it fully
   covers the retained behavior; keep any remaining fork-specific repair. Avoid applying equivalent
   fixes twice, and inspect overlapping code before resolving a conflict.
4. Build the affected components and check their behavior, resource lifetime and synchronization.
   Record which fork changes upstream now covers, what remains local and the validation limits.

The current upstream ancestry is **3.0 at `87e524ab36f12d17c04626d6cf7fc3bde1cc63e4`**,
integrated from the repaired 2.9 base `7412cdde81447f78da0ebb64216dc258d8beaedb`.
This is a selective implementation merge: legacy support and the incompatible
power/token/display chain remain excluded. The optional notebook TGP manager
above is an independent, later backport. The matched kernel/NVK patch chain
still follows 2.8 at `60fb7255e18c31fe0747091fd9a961f30c1cd82a`, integrated
on 2026-11-08 with `18ba05f38b5eadea8b5ae05834bb51c61fb4c5c3` (2.7.2) as
the common ancestor. The preceding 2.7.2 integration used
`6b4752c51b0719862fb46471d383317ef7ecc6f0` (2.7.1); the 2.7.1 integration
used `470e391` (2.6.6). Earlier upstream history was rewritten: old 2.6.5
`ceba63c` maps to `c6f5aa0`; do not replay already integrated history. The
preceding 2.6.4 base was `f95f4d2`.

## Multi-display flip and cursor repair, 2026-11-06

Keep the per-head primary transaction monitor in `nvrm-head-gate.h`. The global
display-management gate still protects shared state and NVKMS calls, but
`waitFlipDone` releases all its recursive acquisitions while sleeping and
restores them before polling completion. A reserved head serializes primary
flips, modesets, gamma/LUT changes and power transitions; another head, cursor
movement and allocation management can progress during a primary-flip wait.
Cursor geometry/image requests wait for mode transitions, releasing management
while they wait. Retain nested same-thread transactions and the framebuffer
reference held by an accepted flip.

Do not replace a completion wait with an early success. Keep the scanout
memory duplicates, allocation cookies/epochs, prior-completion tests, fault
retention and asynchronous NVAccel completion. Stop first prevents new primary
work, drains its accepted head reservation, then unregisters callbacks and
drains workers with no management/head reservation held. Preserve the cursor's
`THREAD_CALL_OPTIONS_ONCE`, accepted-work retains and cancellation/drain code.

The observed defect was reproduced on RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1
with two 1080p 75 Hz displays. Kernel stacks show flip workers blocking one
another and cursor workers behind the same global gate. The local candidate
passes ASan/UBSan and TSan concurrency cases, two negative controls, 18 existing
display regression entrypoints, transform/resolution checks and an isolated
Tahoe kernel-collection link. The direct install preserves all eight existing
collection extensions and their permanent paths, including macFUSE/HighPoint.
After reboot, the candidate UUID was verified in the loaded framebuffer and the
user confirmed normal desktop movement and cursor behavior on both displays.
Sequoia execution and physical modeset/sleep/hotplug are not newly qualified.
This is a source update with a direct local install; no package or release is
prepared under the user's hold.

Hardware VBlank registration and the existing timer fallback policy are unchanged.
The head monitor uses the full-recursion drop/restore contract of
[IORecursiveLockSleep](https://github.com/apple-oss-distributions/xnu/blob/main/iokit/Kernel/IOLocks.cpp).
Local build, stack traces and installation/rollback records are under
`out/records/multidisplay-20261106/`.

## Capture destination reads, 2026-11-05

Core Animation has no framebuffer fetch here (`fc_framebuffer_fetch=0`). It reads its
destination by sampling the current attachment behind a render-target barrier.
NVK draws a linear attachment through a tiled shadow when its pitch or offset is
not 128-byte aligned, or the pass mixes tilings. Sampled views name the linear
image, which the shadow only reaches at pass end, so that read returned the pass's
starting contents and a transparent layer tile overwrote what was under it.

WindowServer keeps exact IOSurface usage and the linear family import, so capture
targets hit this for most widths; 64-byte row alignment makes roughly half of them
unaligned. Applications widen usage onto a private image and miss it. Live
1920-wide framebuffers are aligned, consistent with only captures looking wrong.

Render-target `memoryBarrierWithScope:`, `textureBarrier` and an attachment's
`memoryBarrierWithResources:` now restart a shadowed pass with unpublished writes.
Both attachment clears and draws count as writes. Retain this until NVK can sample its shadow, and share
`nvmtl_vk_pass_render_shadowed` with the framebuffer-fetch shadow test. Do not
replace it with `_surfaceLinear` for WindowServer targets: that path moves
publication off the kernel-marked GPU write.

`tools/test_surface_render_feedback.m` uses the WindowServer name to select the
import path, and repeats each size on a buffer texture of the same pitch. A surface
only takes the family import while the kernel can wire its VRAM into BAR1. With a
256 MB BAR1 nearly full (`debug.nvrmfb_vram_mapped_bytes` 241.6 of 242.2 MB), imports
were refused with `VK_ERROR_INVALID_EXTERNAL_HANDLE`, surfaces fell back to private
images and passed unfixed. The surface rows therefore prove nothing on their own:
read the path each row prints. Buffer rows always draw through the shadow.

RTX 2060 / macOS 26.7.2 (25G332), 2.7.2 kexts, release-flag build merged with 2.7.2,
three runs: every row on a shadow failed before (12 to 15 of 60) and none after, covering
three barrier forms, BGRA8 and RGBA16F. Aligned pitches keep their timing. Each shadowed
barrier cost 0.13-0.22 ms at 936x541 to 1292x892 before the merge. Screenshots taken on
the unfixed driver agree: 363, 936 and 1292 wide have holes, 535 and 562 do not. No fixed
WindowServer session or real capture has been tested, and blur loss is unproven.

Fork integration of PR #7, 2026-11-06: the PR merges cleanly with `82cfa4f`.
Keep its three barrier forms and narrow shadow predicate. Mark a draw after
hazard checks and successful binding: a hazard restart clears the old pass's
flag, so marking before it could miss the following write. Seed the new pass's
flag for colour clears, even with no draw. Replayed indirect commands go through
the same draw binding path; an empty replay is not itself a write. Repeated
barriers on a loaded, untouched pass need no additional restart. Preserve pass
loads, visibility-query handling, binding/resource caches and kernel publication.

On RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1, the original 60-case GPU oracle
fails 24 cases on the installed 2.7.2 plugin. Expanding it to cover clear-only
passes finds 24 failures in the unmodified PR; the revised plugin passes all
120 cases, with 48 shadow cases and all 60 surfaces using the family import.
Use per-case autorelease pools and the one-plugin-image check to keep the
negative controls meaningful. `test_render_feedback_barriers.py` exercises the
production methods with ASan/UBSan, including the internal-restart case, clear-only
passes, repeated barriers, texture views, unrelated resources and direct targets.
Refresh the submission-copy fixture's `_surfaceLinear` field so the existing
submission/publication checks compile against the retained production code.

These are isolated userland tests. A real WindowServer/capture session remains
untested for this revision; do not claim all blur loss is fixed. NVENC/NVDEC,
Zink/CGL, Safari, Photo Booth, kexts and optional installers remain unchanged.
The user requested source merge/push only: no driver installation, release
package staging, draft or publication. Local records are under
`out/records/pr7-20261106/`.

## Managed IOSurface icon rendering, 2026-11-05

Retain the stable tiled GPU backing for shader-writable Managed IOSurfaces,
shared by their texture wrappers, plus GPU copies to/from the family's linear
publication image. Track GPU generations as well as CPU seeds; do not tag a
CPU upload as updating the separate publication image. Keep failed copies
unpublished, the weak backing cache and destruction order. Plain read-only
imports retain the direct VRAM path and existing WindowServer optimizations.

Keep the tiling-specific format queries and render/storage fallback checks.
Mixed linear/tiled framebuffer-fetch passes use explicit tiled attachment views
for both writes and reads, with GPU copy-back and command-buffer-owned lifetime.
This prevents previous contents surviving in layered icons; clearing caches alone
does not repair rendering. Existing corrupted icon entries need one regeneration
after the repaired plugin is loaded by the icon services and Spotlight.

Validation on RTX 2060 SUPER / 8 GiB BAR1 / Tahoe 26.7.1: 96 public-Metal GPU
cases, 128 Shared/Managed scheduled handoffs and four Unknown-usage storage cases passed.
Apple's fresh icon renderer and the normal Applications grid rendered correctly
for the reproduced icons. No Sequoia or other-GPU result is implied. Run
`tools/test_surface_render_formats.m` and the drawable-usage, buffer-view,
publication, queue, schedule and import-lifetime harnesses before replacing this
with any upstream equivalent. This is a local userland test; no release package
or kext change accompanies it.

## Native Vulkan loader support, 2026-11-05

Keep the three `vk_icd*` entry points, loader-interface negotiation at versions
3–5, and the frontend-owned Metal surface/swapchain path. The generated
`nvk_bridge_icd.json` selects the presentation frontend, not the raw NVK renderer.
Preserve it when staging runtimes and keep application selection per process.
Legacy verified Minecraft runtimes without that manifest remain stageable.

The shared bridge source also feeds Wine and Zink: verify their source identities
before reusing binaries in a future build. This integration is source-only;
no binaries, installed runtime or release assets were rebuilt or replaced.
See [application setup](../nvk/native-macos/APPLICATIONS.md).

## Upstream 2.7.1 integration, 2026-11-04

Resolve eight textual conflicts: both required local frameworks plus MetalFX,
optional-component uninstall, fixed-array compute bindings, worker submission,
blit fence/copy barriers, cached buffer addresses, submission-copy tests and the
shader emitter's pointer-address sidecar. Preserve release `-O2`, the metadata
empty fast path, sampler restoration, per-recording bindings and resource caches.
Use the new compute encoder arrays instead of restoring the obsolete dictionary.

Take repeated-render-pipeline elision, compatible render-pass transitions,
compute/pass ordering, retained useHeap/residency members, delayed buffer/image/
view destruction, bounded command-pool retention, GPU instance conversion,
BGR10A2/storage-image support, shader changes, application profiles and complete
shader-cache mode names. Worker acceptance may release later queue parts before
its fence wait, while preserving the fork's scheduled-after-acceptance rule,
publication policy and owned inflight/quarantine path. Keep exact completion
witness matching: lifetime sequence allocation is ordered with queue submission,
but witness allocation still happens earlier and is a separate contract.

The fork's Photo Booth writable storage/Unknown usage and preview orientation,
ANGLE terminal/sampler repairs, surface handoff/capture, bounded address cache,
Parallels callbacks/cache I/O, native Zink/CGL/IOSurface/AnyDesk/resize/small-BAR,
video codecs and Wine/DXVK/vkd3d/DLSS remain. Zink, native frontends, codec overlay,
CGL, NMIntelFB, both accelerator sources, framebuffer and wrapper source are
byte-identical to the preceding fork commit. Recheck producer hashes before any
future binary reuse; these identities do not authorize reusing a stock accelerator
without the Intel frame ABI 2 or a stock GLD without the fork's interop repairs.

**Diagnostic retention:** upstream's release switch omitted several recorders.
Its allocation, image and view histories grow for the life of the process and
probe a temporary Vulkan buffer per allocation. Gate the histories, slot records,
barrier/copy histories and image-death lookups with `NVMTL_LOSS_RECORDS` too.
Ordinary release clients keep no catalogs or address probes; explicit diagnostics
and developer builds retain them. Keep real memory accounting, hazard/resource
lifetime and delayed destruction active. This does not change our cache budgets,
binding/resource optimizations or replace them with broader eviction.

**Compute binding follow-up:** GPU instance conversion must use
`nvmtl_bind_compute_pipeline`, just like app compute dispatches. A direct Vulkan
bind leaves the per-recording cache naming the previous app pipeline; resuming
that same app pipeline then incorrectly skips its bind. Keep repeated-binding
elision and descriptor reuse, while tracking the internal conversion pipeline.
`tools/test_compute_bindings.py` covers app/conversion/app transitions, consecutive
conversions and failed requests. The `conversion-resume` GPU readback overwrites
app output with two real conversions, then verifies the resumed app kernel and
the conversion's remaining matrix, flags, UserID and address output. Its stale
cache guard rejects an unfixed backend before GPU submission.

**Optional audio:** the user requests a separate default-No installer choice.
Move the verified upstream NMXAudio payload to `Optional/`, retain its license
and record its untouched executable hash. Permit `--audio=yes|no` on Tahoe with a
PCI HD Audio controller. Retain an opt-in only when a root-owned ownership record
matches the installed binary; independent or replaced NMXAudio remains external.
Pair the record with backup, immediate/stale-install rollback, boot fallback and
uninstall. Removal checks exact selected IDs, never every `com.nullmoth.*` product.
Intel-panel selection remains independent. See [audio setup](OPTIONAL-AUDIO.md).

The ARM app translator is an independent signed package, installed by 1401 through
`/Library/NullMoth/arm/bin/nm-arm`. Keep it outside the driver archive and use its
CLI as the user. The merged driver preserves `/Library/NullMoth` traversal so the
CLI is reachable. Do not extract an executable from 1401 or replace this fork with
its stock driver; separate script packaging is handled independently.

**Validation:** release plugin and matching shader translator compile. The core
suite passes 2,241 tests, five retained integrations pass 33 (including eight
Resolve layout cases), and wrapper tests
pass 44 with its two existing ignores and two unavailable private fixtures
explicitly excluded. GPU tests on RTX 2060 SUPER / Tahoe 26.7.1 / 8 GiB BAR1 pass
27 shader cases, 32 Core Image frames and 64 IOSurface handoffs (zero stale reads).
The new catalog policy runs 4,096 allocations/images per configuration under
ASan/UBSan and fails on unmodified upstream as a negative control. The actual RM
spinlock adapter passes 64,000 concurrent nested lock pairs; the old adapter
fails. Seven upstream pure-C suites, hazard/lifetime/descriptor checks and the
retained fork host regressions pass. Adapt stale fixtures to fixed arrays,
ordered lifetime helpers and the actual embedded instance kernel: 240 random
matrix/type cases agree byte-for-byte, and the generator now reproduces the
embedded runtime bytes instead of its older row-only kernel.

The isolated Tahoe collection links the verified official 2.7.1 RM, retained fork
framebuffer/AGDC, matched Tahoe accelerator and both optional kexts. A separate repository containing only
the four core kexts links and contains exactly those four IDs too. Public Darwin RM build archives are not
available, so a future package must use the verified official 2.7.1 RM, with
provenance, instead of the old 2.6.6 binary. NAK speed267 source patches are
retained; their presence is not proof that an existing native Zink/Wine renderer
was rebuilt with them. Preserve the matching private renderer identities and
small-BAR overlay when refreshing those runtimes.

This is isolated source/build/regression validation, not a reboot, physical audio
or laptop test, Sequoia kernel link, or ARM GUI/game qualification. Installed files
and the live collection remain unchanged. The user requested merging first and
a separate ARM script package; no driver release or installable driver archive
is prepared by this integration. Compact validation records are retained under
`out/records/`.

## Separate ARM app scripts, 2026-11-04

`build/package_arm_apps.py` creates an independent scripts archive, never a driver
payload. Pin the current signed NullMoth ARM 2.7.0 archive and the upstream 1401
release key in `tools/arm-apps/`; do not require 1401.app or let its stock-driver
installer replace this fork. The packaging and install paths authenticate the
same byte buffer with SHA-256 and Ed25519 before extraction. Reject traversal,
links, devices, duplicate names and Finder metadata. Privileged extraction uses a
fresh root-only folder; install with umask 022 and keep `/Library/NullMoth`
traversable. Translation/list/remove run as the user through `nm-arm` with direct
arguments. Runtime uninstall never passes a user-writable translated-app manifest
to root: it keeps translated copies, which the user can remove with `nm-arm remove`.

Validate the wrapper with the macOS CLT's Python 3.9 as well as the host Python.
Thirty-four host cases cover authentication, archive boundaries, opt-in privilege,
literal app paths and the actual privileged shell body (including archive swaps,
parent ownership/symlinks and its installer umask). The unchanged upstream runtime installs and passes its
CPU/CLT/LLVM check in a user-owned test prefix on i5-11400F / Tahoe 26.7.1. The
synthetic ARM-only app reaches the wrapper correctly but upstream refuses to
translate it because this volume has 6 GiB free and requires at least 8 GiB.
Keep that safeguard: this is not a successful translated-app or GUI/game test.
No system translator, driver, OpenCore or application was installed by these checks.

## Optional Intel panel bridge and RAM copies, 2026-11-04

Adapt `kitsuneislife/nmx-intel` at `b4bb3a0c653248ce125ce7d36b920d216e53c16f`:
panel series 1–21 and the v2.6.4 accelerator series. Source hashes and credit
are in `kexts/NMIntelFB/NMX-PROVENANCE.json`. Its v2.5.4 panel base predates
our brightness code; retain that handler, add inverted PWM handling and preserve
its cancellation/drain. Resolve the old start/stop overlap by configuring the
controller, primary RM handles, scanout proxy and VBL worker before registration.

Keep NMIntelFB optional, default No, and preserve existing opt-ins. NVIDIA-parent
publication, a primary head on headless laptops, matching NVKMS allocation and
AGDC enable ordinary copies after startup for the selected panel; retain the
bundled per-boot inspection/retry command and automatic-activation opt-out. Preserve
the source's 45-second delay unless explicitly overridden. Failed parent/panel
startup must release the claim and allow firmware matching. Honor NVIDIA removal,
boot-guard and other-renderer opt-outs; do not take an unsupported inherited mode.
Retain and drain workers, interrupt callbacks, PCI references and AGDC ownership.
Keep diagnostic NVRAM writes off the frame path; there is no heartbeat writer.

The user explicitly requested experimental fast-copy support too. Keep it off by
default. Frame ABI 2 pairs both NVAccel variants with NMIntelFB: every Intel frame,
including CPU fallback, has an exclusive lease and explicit completion. Keep
IOSurface pending marks and the existing plugin-wrote/source-freshness checks.
RAM scanout requires matching frame traffic, display version 12/13, the known
16 MiB register layout and no other Intel accelerator. Reserve only zero GGTT
entries for the second buffer, verify ownership and readback, and require a
previous flip to latch before reusing its old front. Mode changes cannot race a
lease. Uncertain DMA or scanout ownership quarantines the destination and source
until reboot; never fall back into that same buffer or free prepared pages.
The one-colour hold filter remains an explicit option, default zero.

Do not import the source project's persistent auto-enable/WindowServer-kill job,
fixed-address GGTT overwrite, automatic wake rewrite or NVRM binary-offset patch.
The existing NVRM copy-engine API is reused. Sleep/wake and physical laptop
boot/brightness/acceleration remain unverified for this port. Keep current
NVMTL/cache/browser/capture, cursor teardown, Zink, gaming and small-BAR fixes.
The isolated Tahoe collection links all five kexts without installing them.
Laptop testing remains pending. After these checks the user requested no further
local build work and cleanup of old build outputs; no release is requested.

## Resolve wrapped uniform loads, upstream PR #490, 2026-11-04

Backport [upstream PR #490](https://github.com/nullmoth/nvidia-macos-driver/pull/490),
commit `ca2eecdf57009ed0c9879f3d7a74cbd3c3a33a2a`, credited to WatchAndyTW.
Its base is the same integrated `470e391`; the backport applies without textual
conflicts and does not merge unrelated upstream changes.

Resolve's display vertex shader reads float4 values through a pointer to nested
header structs containing `[4 x i32]`. Preserve the existing successful-load
paths, then permit the zero-offset leading scalar array only when element count
and lane width agree. Load that array through a correctly typed access chain
and reinterpret individual lane bits. Do not skip a leading member, widen an
array read, numerically convert integer bits to floats, or bitcast aggregate
pointers. Keep the common predicate equivalent to the preceding bare-array path.

The installed translator reproduces the reported rejection. The backport's
Resolve-shaped vertex fixture translates and passes `spirv-val` for Vulkan 1.2.
Eight focused wrapper/layout tests and 18 retained GL uniform/sampler/pointer
tests pass, as do all 2,232 core library tests. Six GPU readback cases perform
18 dispatches covering nested/single structs, trailing fields, arrays of arrays,
matching float lanes, dynamic slot indices and exact signed-zero/NaN/infinity
bits. All 21 retained GPU readbacks also pass on RTX 2060 SUPER / Tahoe 26.7.1.
Across a 426-entry local shader sample, all 415 successful outputs are
byte-identical and the 11 failures remain identical; this is not a complete cache
sweep. The wrapper ABI retains all 20 exports. No plugin/kernel, binding/resource,
cache-budget, capture, Zink or gaming-provider source changes are needed.

The author reports testing Resolve 21.1.2 on RTX 3060 Laptop / Tahoe 26.7.2;
local validation above uses synthetic shaders and GPU readback, not Resolve.
On 2026-11-04 the user confirmed that Resolve is working on the other machine;
its GPU, OS and Resolve versions were not supplied. Preserve the content-hashed
translator cache identity; restart Resolve after updating, without manually
deleting shader caches. The user does not need another local Resolve build.

## Native resize probe correction, 2026-11-04

The three previously retained large-resize failures were false positives in
`tools/zink_native_present_probe.mm`, reproduced with unchanged installed driver
binaries. AppKit constrains the requested 1920x960 window to 1920x939 on this
desktop. The green scissor starts at `height/4` with extent `height/2`; its
exclusive end is their sum. The old expected end `height*3/4` included an extra
row when the height modulo four was three. A separate expectation also failed
to give the last blue clear priority where the two rectangles overlap.

Keep exact origin-plus-extent bounds and blue-over-green ordering, require the
native event dimensions to match the rendered frame, and retain byte-for-byte
pixel checks. Do not loosen tolerances or alter presentation code to satisfy
an incorrect expected image. Explicit, automatic GLD and forced system-page
large-resize cases now pass. New odd/tiny resize cases cover both backings and
transparent output. The correction changes diagnostics only; the driver
libraries, capture policy, synchronization and GPU-copy paths remain unchanged.
All 43 runtime cases pass without the optional screen-capture tests. A separate
negative control with a deliberately shortened scissor still fails pixel checks.
Validation is on RTX 2060 SUPER / Tahoe 26.7.1 / 8 GiB BAR1; forced family-VRAM
refusal tests the host-page fallback, not a physical small-BAR reconfiguration.

## Native Zink IOSurface texture interop and CGL capabilities, 2026-11-04

Keep `CGLTexImageIOSurface2D` on Zink for supported rectangle/color/plane
bindings. Clone the validated context-local GLI callback allocation and replace
only integer parameter 910 handling, preserving the original callback for all
other parameters and restoring its vector for Apple presentation and teardown.
Do not globally interpose a loader, modify Apple's framework, or disable Zink
for Avalonia/Ryujinx. The private callback layout still needs validation on
other OS versions; no exact-build activation guard is added.

Import the IOSurface host pages and copy to/from the private Zink image on the
GPU. Keep locks, resources, context sharing, row/plane offsets, seeds and GPU
fences paired. Flush, finish and GL sync creation publish this context's tracked
writes. Texture/framebuffer rebinding refreshes externally changed storage;
reject conflicting unflushed ownership. Retain deleted textures through queued
writes, including Avalonia's delete-before-flush order. Rebinding/redefinition
must invalidate framebuffer attachments and sampler views. Noncurrent teardown
temporarily binds the correct Mesa context, drains writes and releases its
texture references before restoring the previous binding. Completion uncertainty
retains the imports and quarantines the screen rather than freeing busy pages.
Only IOSurface images enable Zink's write-generation tracking; normal resources
retain their existing path and no-interop flush/bind calls return immediately.

Keep the CGL capability filter paired with this interop. On the affected host,
Skia rejects Mesa's advertised 4.6 interface because Apple's public loader
cannot provide required functions. Use the pinned Khronos registry and recovered
public dispatch to compute version/extension coverage, then intersect that with
each Mesa context. All typed version/count queries and indexed/legacy extension
strings must agree, including GL errors for invalid queries. The tested framework
covers 4.1 core / 3.2 compatibility. This changes CGL reporting, not the standalone
Mesa API or its rendering backend. Preserve the existing OBS compatibility policy.

Ryujinx 1.3.3 now imports its UI surfaces on a normal launch with the installed
Zink libraries. The user confirms rendering and response on RTX 2060 SUPER /
Tahoe 26.7.1 / 8 GiB BAR1, while reporting remaining UI quirks with Apple's
renderer as well. Its own Skia library fails interface validation with the
preceding capabilities and passes validation/context creation after filtering.
Focused GPU tests cover input/output, padding, orientation, external updates,
shared/noncurrent contexts, deleted/replaced textures, FBO/sampler invalidation,
R8/RG8 NV12 planes, caller state, GL fences and noncurrent teardown. The initially
retained large-resize failures are resolved by the probe correction above. Compositor capture
rechecks are skipped when this session lacks screen-recording permission.
Sequoia, other GPUs, game emulation and OBS through native interop are untested.
No release is prepared for this local trial.

## Zink window presentation and late view attachment, 2026-11-04

Honor `CGLFlushDrawable`/Cocoa `flushBuffer` when the target context is not
current. The CGL swap dispatch argument identifies that target; it must not be
looked up as though it were the calling thread's current context. Lock and
temporarily bind it for presentation, then restore both the CGL and Mesa
bindings, including an unbound thread or another Apple/Zink context. Preserve
the current-context fast path used by ordinary game swaps. Do not call CGL
reference/locking APIs while holding `bridges_lock`, and retain the carrier
during the slow swap path. The frontend's current-context accessor uses Mesa's
frontend userdata, initialized when creating each context.

Treat a missing native view as transient. If CGL reports no drawable yet,
defer the swap before allocating/blitting an Apple fallback. With a valid CGL
drawable, retain GPU fallback but retry native presentation after AppKit binds
a view. Preserve explicit `gpu` mode and real unsupported-device/quarantine
refusals. Do not use surface-backing override dimensions as a drawable test;
use the public `kCGLCPHasDrawable` query.

Honor Mesa's dirty-front notification after `glFlush`, `glFinish` and Apple's
render-flush aliases return. Do not present reentrantly from Mesa's callback.
Use the existing native/IOSurface GPU-copy path and select FRONT during this
handoff, without rotating double-buffered attachments. Ordinary back-buffer
and offscreen flushes must not publish. Keep failed or deferred handoffs pending
so a later flush can retry without another draw; refuse reentry, Begin/End and
quarantined contexts. Preserve normal swaps, GL state and completion lifetimes.

AnyDesk 9.8.1's remote viewer is white with Zink and renders with Apple GL on
the RTX 2060 SUPER / Tahoe 26.7.1 host. Its GL trace shows successful texture
uploads/draws followed by `clearCurrentContext` and a non-current window flush;
the preceding swap hook drops that frame. An independent window reproduces
that failure, plus a separate missing single-buffer publication. The preceding
libraries fail both compositor pixel checks. Twenty-one native/IOSurface cases
cover three successive frames, both Apple aliases, front/back isolation,
offscreen rendering, invalid Begin/End flushes, worker setup/retry and
non-current swaps with binding/state preservation. Non-current cases exercise
CGL, Cocoa and the public bridge API, checking Mesa restoration before a GL
adapter could restore it lazily. Late-view cases verify the initial GPU fallback
and subsequent native copies, preserve forced GPU mode, and flush a context
before it has any drawable. Pre-repair libraries fail these lifecycle checks.
The initial suite retained three identical large-resize failures on both the
preceding and candidate runtimes. The later probe investigation above identifies
incorrect expectations at the actual 1920x939 size, and all three pass with the
corrected test and unchanged installed driver. After installing the clean matched libraries,
the user confirms that AnyDesk's remote picture renders and keeps updating
on a normal launch. Both `NVKZINK_DISABLE` and `NVKZINK_PRESENT` are unset;
the viewer reports Mesa's compatibility renderer and loads the repaired Zink
libraries. Temporary tracing is absent from the installed candidate. Sequoia
and other GPUs are not retested here; no release is prepared for this trial.

## Parallels custom-drawable presentation, 2026-11-02

Before recording presentation, supply an empty `addPresentScheduledHandler:`
block to non-`CAMetalDrawable` drawables that implement that private selector.
Parallels' `AsyncDrawable` reportedly invokes its stored scheduled-present block
without a nil check. This covers `presentDrawable:`, `atTime:` and
`afterMinimumDuration:` through their shared recording method. Skip nil
drawables and recording after commitment as before. Keep CAMetalDrawable's
texture/surface path, serial deferred presentation, callbacks and resource
lifetimes unchanged. The handler must not capture the command buffer or drawable.
Do not introduce an unsynchronized static selector initialization.

`tools/test_custom_drawable.m` runs against an isolated compiled NVMTL plugin.
It passes 11 policy/lifetime cases and 64 concurrent custom presentations,
including all presentation variants, optional-selector absence and a
CAMetalDrawable with the private selector that must not receive the block.
The preceding plugin reproduces the unchecked nil-block failure in a separate
negative-control process. Retained host checks, 15 sampler pixel cases,
32 Core Image orientation/pixel frames, 64 IOSurface handoffs and 64 frames of
600 descriptor-reuse draws pass on RTX 2060 SUPER / Tahoe 26.7.1. This models
the reported custom-drawable failure; a real Parallels VM is not available here.
The user approved building and publishing this change as `v2.6.6.69-nvk`.

## Bounded address-lowering cache, 2026-11-02

Keep `nvmtl_address_shader_g` within a 128 MiB accounting budget for owned
input keys, lowered results, guard metadata and an allowance for entry overhead,
with the existing 4,096-entry ceiling. Evict least recently used entries instead
of clearing the whole cache. Oversized results remain usable without being cached.
Keep byte-exact keys covering both flags and the complete shader input. Share one
immutable key between the dictionary and LRU, update recency by object identity,
and freeze growable outputs so their spare capacity is not retained. Concurrent
misses must reuse the first inserted result and its matching guard metadata
without counting an entry twice. Retain negative-result caching and caller-owned
results after eviction.

This changes only retention of lowering results used during pipeline creation.
Preserve all binding/resource optimizations, compiled-pipeline ownership,
descriptor recycling, buffer pools, other cache budgets and surface synchronization.
Eviction may add translation work when a later pipeline needs an old result.

`python3 tools/test_address_shader_cache.py` passes eight ASan/UBSan cases,
including overlapping concurrent misses; the original cache fails the oversized
result negative control. The isolated 128-module, 1 MiB-per-module allocation
probe falls from about 260 MiB to 130 MiB. Three alternating warm-lookup samples
have medians of 0.088 ms before and 0.085 ms after. A SHA-256-key alternative
cost about 1.8–2.1 ms per 685 KiB warm lookup on this CPU, so it was not selected.
These synthetic measurements do not establish WindowServer savings or gameplay
performance. Private RTX 2060 SUPER / Tahoe 26.7.1 GPU probes pass sampler arrays,
Core Image pixels/orientation, IOSurface handoff, concurrent pipeline creation
and descriptor reuse. After installing `197ad27` and rebooting, the user reports
a smoother desktop. WindowServer loaded the expected plugin with normal memory
pressure and zero swap; an early footprint sample was about 650 MB versus about
940 MB in the preceding longer session. The user subsequently reports 711 MB.
These are different session ages and workloads, not a matched memory benchmark
or proof that all retention is fixed. The source was pushed at the user's
request; include it in the separately approved `v2.6.6.69-nvk` release.

## Parallels cache I/O compatibility, PR #4, 2026-11-02

PR #4 (`70056ae`) merges cleanly onto `d2df8bd`. Default shader-cache writes
and touches to the caller's thread only for the exact `prl_vm_app` process.
Ordinary clients, including WindowServer and Chromium, keep asynchronous I/O.
An explicit `NVMTL_CACHE_SYNC_IO` setting wins, including `0` or an empty value;
retain the existing first-character flag semantics. Initialize this policy with
`dispatch_once`: the original PR's shared integer allowed concurrent callers
to observe its temporary off value before process detection completed.

The contributor reports that this policy stopped a Parallels 26.5.0 VM startup
crash on RTX 2060 / Tahoe 26.7.2, with a run exceeding four minutes. Its cache
lifetime diagnosis is inferred from the crash report, not proven. Do not claim
an independently reproduced VM fix or generalize this synchronous policy to
other applications. Existing captured cache data and file paths already have
owned lifetimes; this change does not replace their ownership or exit-drain rules.

Validation: 14 policy/override cases pass with 64 concurrent callers per case,
plus real synchronous/asynchronous dispatch and drain under ASan/UBSan. The
original PR fails the strengthened concurrency regression. Eleven retained host
regressions pass. The release plugin builds for macOS 15.0; the translator is
reused byte-for-byte. Thirty sampler GPU pixel checks pass across ordinary and
real `prl_vm_app`-named probe processes, along with 32 Core Image frames and
64 IOSurface handoffs with zero stale reads. These are native probe tests on
RTX 2060 SUPER / Tahoe 26.7.1, not a Parallels VM test.

The user explicitly requested rebuilding and replacing the existing
`v2.6.6.1-nvk` package/release for this fix. Record both source identities and
asset digests externally; this one replacement does not change the normal rule
to use a new revision for future public updates.

## ANGLE WebGL sampler arrays and inlined constants, 2026-11-02

Preserve the address ABI of a one-element byval sampler array. AIR metadata
omits descriptor counts of one, so distinguish that array from a scalar sampler
using the emitted pointee type. Keep count/type consistency checks and the
stage's sampler binding limit. Carry runtime sampler state through each array
element's materialized load, and let the C wrapper specialize every used index
in the reflected array range, including nonzero elements.

When inlining helpers, retain the complete constant-expression argument, rather
than its last whitespace token. Three.js clearcoat shaders pass a vector `splat`
constant; losing that opcode corrupts an otherwise supported multiply. Parse a
complete value suffix in its LLVM spelling, accepting preceding parameter attributes.
Do not work around these translation failures by disabling ANGLE, Graphite,
WebGL, browser acceleration or the GPU sandbox.

Validation: 2,232 library tests, seven sampler-array integration tests and 44
wrapper tests pass; the wrapper's two ignores and two unavailable private
fixtures remain. Fifteen GPU pixel checks cover scalar, singleton and two-element
samplers, nonzero binding bases, nearest/linear filtering and pixel coordinates.
The exact captured clearcoat shader translates and passes SPIR-V validation.
On RTX 2060 SUPER / Tahoe 26.7.1, the live niemphat.online temple renders with
ANGLE Metal, Graphite, WebGL and the GPU sandbox enabled, with zero GPU-process
crashes in the observed run. The user confirms that it stays responsive without
whiteout. All 21 translator GPU cases and 32 Core Image pixel/orientation frames
pass. This is bounded host validation, not an endurance or Sequoia qualification.
Only the translator code changes; plugin machine code, kernels, Zink and the
gaming/video runtimes retain their preceding verified bytes. Included in the
user-approved `v2.6.6.1-nvk` release.

## ANGLE terminal and IOSurface handoff, 2026-11-02

Preserve early immutable resource-object forwarding in the translator's borrowed
and owned input paths, followed by the existing builder-field pass. Internal
helpers must be inlined before texture/sampler aliases are modeled. A no-op cast
of an alloca's base can name its first pointer field when used only for pointer
loads/stores; whole-object lifetime and escaping uses keep their original address.
Keep unique-store, type, dominance and escape checks. Previously xterm's ANGLE
glyph shader compiled successfully with all texture reads replaced by zero;
forwarding only nonzero fields still lost atlas slot zero. Keep the descriptor
band/count refusals, sampler specialization and buffer-address guards.

Report scheduled GPU work only after successful submission. For IOSurface handoff,
check the actual protection of every non-drawable output: shared VRAM alone does
not order another process's reads. WindowServer's accepted pending marks plus
early dirty publication retain asynchronous scheduling. Private outputs, refused
early dirty marks and other unfenced outputs publish before scheduled callbacks.
Keep explicit scheduling diagnostic overrides, event reservations, submit-ahead,
resource/quarantine lifetimes, callback order and the serial presentation queue.
Do not replace this policy with an all-command-buffer completion wait or add the
global `NVMTL_SCHEDULE_AFTER_SURFACE_WRITES=1` workaround to the shipped config.

Validation: 2,230 library tests and 43 wrapper tests pass; the wrapper's two ignores
and two unavailable private-fixture exclusions remain. All 16 pages of the actual
ANGLE glyph shader pass distinct-color GPU readback on RTX 2060 SUPER / Tahoe
26.7.1. The original VS Code terminal renders with WebGL, ANGLE Metal, Graphite
and the GPU sandbox enabled. A 64-frame IOSurface handoff probe changes from
64 stale reads to zero; forcing the old policy on the candidate restores stale
reads. Callback, queue, publication, resource lifetime and Photo Booth regressions
pass, including 32 Core Image pixel/orientation frames. The user confirms terminal
text and no observed flicker after relaunching their apps. A separate 30-second
Edge capture remains steady on both builds, so it is not an independent flicker
reproduction or an endurance qualification. Claude and Sequoia remain untested.
Three broader integration harnesses retain nine failures on both the baseline
and candidate: four corpus sweeps expect at least 20 fixtures where this checkout
has five, and five reflection checks omit the existing bindless image-heap binding.
Do not present those harnesses as passing or weaken their assertions for this fix.
Only the plugin and translator change; kernel and Zink binaries are reused.
Included in the user-approved `v2.6.6.1-nvk` release.

## PR #5 optimization branch integration, 2026-11-02

PR #5 (`3e28b36`) is integrated into HighDelay `main` at `b240c97`, with
an empty descriptor-free-list guard and a portable regression-test include path.
The contributor had already merged `b240c97`, which contains NullMoth
`470e391` (2.6.6); retain that ancestry rather than replaying rewritten
upstream history. See [the fork review](PR5-REVIEW.md) for independent validation.

Keep every recording cache and descriptor/resource lifetime repair listed below,
including the latest vertex/index/sampler caches, buffer-address cache, atomic
surface-seed publication, empty completion fast path and host query-pool reset.
Upstream now covers framebuffer-fetch mask exclusion/fallback; use that shared
implementation once. Resolve the buffer-upload conflict with one bulk 3D copy
and upstream's detailed failure diagnostics. Missing-image refusal belongs in
the common 3D copy implementation so both volume and single-plane callers are
protected. Keep managed initial clears before uploads and transfer ordering.

**Completion-witness exception:** this branch retains exact-match completion.
The historical 2.6.5 instruction to accept a later lap is superseded here:
sequence numbers are allocated before the queue submit lock, and pre-submit
hooks can submit recursively. A later recorded sequence in the same ring slot
therefore cannot certify an earlier command. Keep missing-witness quarantine
and the backend-preserving fallback. `tools/test_witness_order.py` runs the
production verdict and rejects reordered one/two-lap values; HighDelay's
unmodified verdict fails that negative control. This bounded integration does
not change submission locking or claim to fix witness-ring exhaustion.

Preserve Photo Booth's real storage backing and Unknown-usage handling, the
preview-only orientation adapter, two-plane publication and serial presentation
queue. All Tahoe, native OpenGL/Minecraft, MoltenVK, small-BAR Zink, video,
Wine/NVK/DXVK and optional-panel policies remain present. Translator, kernel,
native Vulkan runtime and installer sources are identical to `b240c97`; reuse
their verified 2.6.6 release binaries. Rebuild the plugin, native Zink library
and changed diagnostics. Keep the verified CGL/GLD adapter binaries and their
source identities; changing a diagnostic does not require rebuilding the adapter.

The [contributor's merge validation](NVMTL-UPSTREAM-2.6.6.md) is historical;
the fork review records this integration's checks separately. Installation/reboot,
Minecraft gameplay after resizing and live WindowServer measurements remain
pending. Prepare a draft under the normal release policy; publication requires
the user's explicit approval.

## Native Zink resize follow-up, 2026-11-02

The PR #5 follow-up also retains the native Zink presentation fallback described
in [resize validation](NVMTL-RESIZE-20261102.md). A full small BAR1 aperture can
refuse a new drawable's family VRAM even though the game continues rendering.
After an invalid external VRAM import, copy into aligned, bounded IOSurface
system pages on the same Zink device. Keep the write lock until confirmed GPU
completion and publish its seed; do not publish a family-VRAM dirty/tag marker
for this path. Preserve failed-import cleanup and uncertain-work quarantine.
The native Minecraft Vulkan/Wine frontend remains a separate implementation;
this fallback changes native Zink only.

## Upstream 2.6.6 integration

Two textual conflicts cover the drawable usage helper and remote plugin builder.
Keep the fork's real-storage-view request and fallback, Photo Booth orientation
adapter and WindowServer policy. Preserve upstream's `MTLTextureUsageUnknown`
when no storage view exists, so writes can use an optimal-tiled storage twin.
When a non-framebuffer-only app surface has real writable backing, expose the
explicit read/write/render-target usage even for an Unknown descriptor. Core
Image checks the public ShaderWrite bit; an unconditional early return bypasses
the fork's drawable repair and makes Photo Booth black. WindowServer and FBO
usage remain unchanged, and ShaderWrite must never advertise a missing view.

Take ordered initialization clears before managed uploads, missing-VkImage
copy refusal and bounded diagnostics, empty-assembly parsing, same-local-field
pointer-phi resolution, and capability-gated rate-map identity. The plugin must
report no rate-map support before setting translator capability bit 2; generic
translation still refuses opaque map data. Preserve all existing translator
pointer, callback, uniform-cbuf, sampler and rounding repairs.

Take the Metal-capable desktop-owner check in both accelerator builds and the
installer's root-only old-firmware backup/exclusive-create rollback, plus AMFI
guidance. Keep native GLD installation, optional NMIntelFB default No, external
kext retention, permanent collection paths, backup/recovery and opt-in handling.
Remote builders use upstream's environment-selected host/root while retaining
the fork's rotation headers and release `-O2`.

Native Zink, Minecraft/Wine, DXVK/vkd3d/DLSS, the VP8/VP9 overlay and browser
IOSurface repair are unchanged. Upstream's 15.0 Vulkan deployment-target change
does not lower this complete fork package's 15.5 minimum: its retained Zink and
gaming runtimes still require 15.5. Fork packages do not carry NullMoth's private
release signature. Build and validation records accompany the local 2.6.6 test
package; keep those exact assets for a later draft after user approval.

Validation on RTX 2060 SUPER / Tahoe 26.7.1: both accelerator variants and the
release plugin/translator compile. The library passes 2,227 tests plus 18 focused
integration tests; the wrapper passes 43, with its two upstream ignores and two
private-fixture exclusions unchanged. The test-only library copy prunes stale
workspace lock entries without changing remaining dependency versions/checksums;
the production dylib uses the tracked wrapper lock with `--locked`.
All 21 translator GPU readback cases, four unspecified/explicit IOSurface write
cases, 16 Core Image pixel/orientation frames and the C-ABI rate-map capability
checks pass. Host regressions retain cursor drain, import lifetime/publication,
installer rollback/external kexts, native GLD selection and optional panel policy.
The isolated Tahoe kernel collection links with and without NMIntelFB, leaving
the installed driver and live collection unchanged. This is not a boot test or
Sequoia/laptop qualification; local user testing remains pending. Signatures,
minimum OS, symlinks, runtime manifests and package checksums are verified before
staging. Unchanged kernel/rendering runtimes retain their previous source records
and exact hashes; upstream's deployment-target-only NVK replacement is not needed.

The initial merge missed that default-descriptor interaction: its Core Image
probe used explicit ShaderRead, while the Unknown-descriptor probe dispatched
Metal directly and missed Core Image's stricter usage check. The follow-up
reproduces both failures on the merged plugin and passes 32 Core Image
pixel/orientation frames across Unknown/ShaderRead, RGBA/BGRA and shared/private
backing. Keep that coverage alongside the no-storage/WindowServer/FBO host
cases and Unknown RGBA16Unorm GPU writes. The user confirmed the normal Photo
Booth preview is visible, moving and upright on Tahoe 26.7.1 after updating
only the plugin; Zink, codecs, translator and kernel remain unchanged.

## Optional Intel laptop panel framebuffer

The complete package builder compiles the real `NMIntelFB` into `Optional/`,
with source/payload hashes. Keep the virtual variant out of release archives.
The installer prompts with a default of No; do not infer opt-in from a spoofed
MacBook model. Preserve an existing opt-in during upgrades, explicit
`--intel-panel=yes|no`, selected-kext preflight, removal checks, backup/rollback,
interrupted-install recovery and uninstall handling. A new noninteractive
installation skips the kext unless explicitly selected.

This is experimental firmware-mode framebuffer support. The accelerator's
`debug.nvaccel_igpu` path remains disabled by default. Do not claim accelerated
Intel-panel gameplay or laptop sleep/wake qualification from a kernel link
check. See [Intel panel setup](INTEL-PANEL.md).

## Additional NVIDIA video codecs

Retain the pinned `video/` codec overlay and `build/build_nvenc.py`. Complete
future packages pass its verified output as `--video-runtime` to
`build/stage_package.py`; keep the library in the Metal bundle and the source,
license and hash record under `nvmtl/video`. Preserve an explicit addon opt-out.
TU10x adds VP8/VP9 VideoToolbox decoders and keeps the driver's matched H.264/HEVC
encoders/decoders. Do not register AV1 or VP8/VP9 hardware encoding on Turing.
Keep PCI display-function filtering, VP8 odd-width chroma and malformed-frame
checks, and qualify application codec selection separately from API decoding.
The overlay is built against pinned NVIDIA/WebKit headers; it does not change
the driver kernel, renderer, Zink or Wine interfaces. See [codec setup](../video/INSTALL.md).
Preserve Ada's upstream H.264/HEVC/AV1 encoders and decoder selection, TU117's
H.264/HEVC encoders and decoders, and Ampere's VP9/AV1 decoders alongside the
driver's H.264/HEVC encoders. Turing must not narrow these existing paths.

Keep decoder-owned pixel pools based on the negotiated output attributes,
including IOSurface backing and Metal compatibility, even when the client
requests only a pixel format. Copying only that minimal client dictionary
produces heap-backed frames and crashes Chromium's GPU process during VP9
playback. Preserve the direct-pool optimization and format/size conversion
fallback. The native regression must require IOSurface-backed output for both
minimal and explicit requests; retain the live Edge 2160p60 qualification in
[browser validation](BROWSER-VIDEO-DECODING.md).

## NVMTL command recording caches

The framebuffer-fetch optimization originated in v2.6.5 commits `4a377d9`
and `8784c0b` and is now shared with the integrated upstream: exclude separately
bound set 3 from stage descriptor masks and fall back to older valid masks when
new masks are unavailable. Retain the existing fetch barriers and bindings. See
[fetch-mask validation and deferred witness review](NVMTL-FRAMEBUFFER-FETCH-MASKS.md).

Retain the per-recording, per-stage texture-index cache used by address-table
snapshots and the single dummy-address query per snapshot. Invalidate sampled
texture indices when a view, resource kind or null binding changes; copy them
into each independently owned GPU snapshot. Buffer addresses and self-pointers
must use the current page/offset, and no cached array may outlive its bind table.
See [address snapshot validation](NVMTL-ADDRESS-SNAPSHOTS.md).
Keep dirty/copy bitmaps sized for all `NVMTL_NBIND` entries, independently of the
four-word base shader mask. Storage bindings 256–320 must neither overwrite
adjacent state nor qualify for buffer-offset-only descriptor reuse.

Keep the recording-local compute pipeline/descriptor binding cache, eight-entry
successful threadgroup-variant cache, and framebuffer-fetch binding cache. Reset
them on each command recording/event segment; preserve complete handle/layout/
offset identity, retained pipeline ownership, failed-variant retry, internal
occlusion pipeline tracking and invalidation before direct SASS hooks. A different
lower-set layout can disturb the higher framebuffer-fetch set. Retain fetch
barriers and all push constants even when bindings are unchanged.

Resource-set enumeration uses a synchronized snapshot with a non-null mutation
sentinel. Normal exhaustion must release resources before the app's pool drains;
early exits must not leak. Keep the existing backend VkSampler interning and
independent Metal wrappers. See [command cache validation](NVMTL-COMMAND-CACHES.md).

## Capture import reuse and planar GPU copies

Preserve the Photo Booth follow-up to upstream PR #433: request an actual
storage view before importing eligible non-framebuffer-only app drawables.
If a linear alias drops requested storage usage, use the tiled GPU bridge
described above when its optimal image supports those views; otherwise retain
the private fallback. Never advertise a nonexistent writable view. Keep WindowServer's
requested usage unchanged. Also retain Photo Booth's preview-only orientation
adapter, gated by process/bundle identity and actual NVMTL CAMetalLayer
drawables. Do not disable Zink, change camera buffers/saved photos, or globally
invert Core Image/Metal coordinates. See [validation and scope](PHOTOBOOTH-PREVIEW.md).

Preserve the installer/uninstaller permanent-path repair: recover obsolete
temporary collection references only through an unambiguous installed bundle
ID/version match. Published removal collections must use permanent `-p` paths,
preserve non-default external extensions, and verify retained IDs both before
and after publication. Keep firmware in removal rollback and the check-only
uninstaller mode. Never publish a collection whose dependencies live only in a
temporary staging repository that cleanup deletes.

Retain the bounded IOSurface import cache and shared page-span calculation from
PR #3. Cache only released wrapper imports, retaining the surface while idle;
keep count/byte/idle/age limits, opt-outs, separate host-page accounting and GPU
cleanup outside its mutex. Do not broaden this into reuse of in-flight buffers
or replace the stable parked backing without an import-invalidation contract.
Host imports must stay within the owned mapping, use a texel-aligned offset and
one overflow-checked span for both upload and readback. Preserve CPU fallback.

Keep the three-word `NVMTLVramAt:pN` tag shared with Minecraft/Wine and Zink.
The PR's incompatible four-word pending tag and capture-reader polling are
excluded. Preserve the existing kernel-gated conditional completion, overlapping
writer counts, deferred publication and uncertain-work quarantine. There is no
new cross-process busy-wait or encoder change. See
[capture review](CAPTURE-PR3-REVIEW.md) for the OBS path analysis, pixel tests and
remaining live-application/hardware validation limits.

## Native Zink OpenGL integration

Preserve `zink/` and its pinned Mesa overlay, build/packaging tools and local
validation probes. The native GLD is intended to select Zink/NVK automatically
for normal OpenGL apps and OpenGL Minecraft, without a mod, attach call or
launch argument. Its private CGL ABI was tested on x86_64 Tahoe 25G241.
The user requested removal of the exact OS-build activation/build/install gate
on 2026-10-30 and will test later macOS updates. Preserve that policy: a build
number change alone must not disable Zink. Private ABI changes can still
require a repair. Keep build provenance, symbol/slot/profile/device checks
and Sequoia/Tahoe accelerator selection; these are not an OS-build whitelist.
The 15.8.1 / 24H32 collection matches all 1,236 public dispatch entries and 131
Apple GLD exports, together with the private dispatch/profile/share offsets.
Retain the [static ABI review](ZINK-SEQUOIA-ABI.json) and distinguish that evidence
from application/boot qualification. The Sequoia RTX 2070 SUPER / 256 MiB BAR1
machine subsequently passed all 25 isolated tests (native pixel copies, core/
legacy contexts, sharing, resize, workers, windows, VSync and quarantine).
The user subsequently confirmed Minecraft 26.3 menus and single-player world
rendering on this Sequoia machine after the small-BAR mapping repair; see
[mapping evidence](ZINK-BUFFER-MAPPING.md). This does not qualify a fresh
Sequoia kernel-collection build or every application.
The shared adapter
continues targeting macOS 15.5; do not copy Apple binaries into the runtime.
The Zink package builder selects the GLD by default. Keep the original complete
base-package builder and its Minecraft/Wine/dual-OS support, then add the Zink
payload with `build/package_zink.py`. A local test build is not a release.

Retain private frontend/renderer selection, one process-owned GPU screen with
separate GL share groups, framebuffer invalidation after swap/resize, GPU VRAM
copy/dirty/generation-tag ordering and per-context native targets. Keep strict
same-device checks and uncertain-work quarantine. GPU IOSurface/Apple
presentation is a compatibility/comparison path, never a reason to execute
application shaders in Apple's OpenGL renderer outside the explicit client
compatibility options below, or silently use CPU pixels.
Keep the Zink batch condition-variable and Darwin barrier repairs; the latter
must have one serial return only after every other waiter leaves. Compare an
equivalent upstream fix before removing either repair. Preserve the descriptor-
set, staging-upload and bounded texture-worker fixes in the pinned overlay.

Preserve the small-BAR CPU-visible buffer allocation policy, including both
persistent storage and ordinary mutable buffers. Prefer coherent system RAM
for their backing; retain full-BAR placement and device-only VRAM resources.
Keep null-transfer handling after failed maps and allocation diagnostics.
The 8 KiB static/dynamic regression must cover the 2 MiB backing slab, not just
PBO allocations (which Mesa already classifies as staging).

Keep native display synchronization tied to each context's CGL swap interval,
including render-thread changes and independent windows. Preserve the explicit
`NVKZINK_DISABLE=1` diagnostic opt-out before any GL objects are created.
OBS 33.0.1's compatibility mode is enabled by default and checked only for
`com.obsproject.obs-studio`. An explicit false `NVKZinkCaptureCompatibility`
preference or process-local `NVKZINK_OBS_COMPAT=0` opts out; an explicit `1`
environment override wins over a saved preference. The mode keeps
the whole OBS context in Apple's driver and changes RGBA16F ScreenCaptureKit
requests to BGRA8. Never switch an existing context's object namespace or enable
this mode globally. The user confirms moving OBS capture after a normal launch
on RTX 2060 SUPER / Tahoe 26.7.1; this qualifies the SDR compatibility mode,
not native Zink IOSurface imports, HDR capture or encoder performance. See
[the setup and rollback](ZINK-OPENGL.md#obs-display-capture-compatibility).

Preserve native GLD rollback, build provenance and external-kext preservation during
both preflight and live kernel-collection builds. Do not replace permanent
external kext paths with disposable preflight paths. See
[Zink integration review](ZINK-PR2-REVIEW.md) for the review and validation limits.

## Upstream 2.6.5 integration

Five files conflicted: CHANGELOG, NVMTLDevice, NVMTLObjects, nvmtl_vk and the
ANGLE regression fixture. Retain both changelog histories and the fork's all-app
ANGLE default: upstream's loaded-library detection is unnecessary for that
broader policy. Retain the richer AIR texture component/access reflection already
in this fork instead of duplicating upstream's narrower implementation.

Combine upstream's ordered surface upload barrier with the fork's checked
page-span calculation and nonzero plane offset. Preserve released-import reuse,
VRAM publication tags, pending-write tracking and uncertain-work quarantine.
Take upstream's two-plane private-image schedule-after-publication default,
serial presentation queue, completion-witness reuse, descriptor-mask fallback,
ARGB8 sampling (but rejected colour attachments), compressed mip copy checks,
array reflection and shader translation improvements. These affect Metal and
OpenGL-over-Metal clients; native Zink/Minecraft/Wine renderer sources and all
kernel sources are unchanged. Preserve the qualified small-BAR Zink repair.

Build and test records are under `out/upstream-2.6.5-review/`. The merged plugin
and translator require new binaries; reuse unchanged kernel, native runtime,
DXVK/vkd3d and CGL components only with verified prior-release hashes. Previous
Sequoia Minecraft confirmation qualifies the unchanged Zink library, not a new
boot or capture test of the 2.6.5 plugin.

## Upstream 2.6.4 integration

Take sampler-array lowering, texture-array element routing through the per-draw
bindless table, and bounded rewriting of older inline constexpr samplers. Keep
the fork's typed-pointer/address-table/local-pointer and RGB10A2 `NoContraction`
repairs, pointer-escape guards, vector-prefix bounds and GL constant-buffer path.
Sampler arrays use one scalar descriptor per element, matching the plugin's
count-one bindings; preserve the 16-slot and metadata/type consistency checks.

Four files conflicted: the changelog and ANGLE, submission-copy and surface-
publication fixtures. Preserve both changelog histories and the stronger fork
fixtures: all-app ANGLE, uncertain-use/retirement state, imported-host-page and
CPU-fallback publication locking. Do not reintroduce the excluded PR 398 foreign-
process busy wait. Take the other upstream fixture updates; the resource-phi
harness needs the current Cargo dependency directory via `--deps`.

The added IOPCIFamily dependency in `kexts/NVRM/fb/Info.plist` already exists in
`kexts/NVRMFB/fb/Info.plist`, which this fork builds. All plugin, NVRMFB, NVAccel
and NVRMAGDC implementation trees remain byte-identical to their verified
producing sources. Reuse those fork binaries, including both 15/26 accelerator
variants, with recorded hashes. Use the official 2.6.4 RM, firmware, codecs,
installer and version data. Repeat isolated Tahoe kernel linking with that RM;
its executable hash differs from 2.6.3. The bundled NVK renderer is unchanged.

Rebuild the merged translator and both native frontends; include the compiled
D3D12/DLSS prefix helper and matched DXVK/vkd3d/NGX/NVAPI components. Checks pass:
2,217 translator library tests; 43 wrapper tests with two upstream ignored and
two private-fixture exclusions; 50 focused integrations; 36 driver host scripts;
21 GPU shader readback cases/28 dispatches; both frontend ASan/UBSan fixtures;
45 pixel-verified presentation frames with resize/restore per frontend; real
D3D11/D3D12 rendering and DLSS evaluation; 19 setup checks and three real Wine
registry/rollback scenarios. Isolated Tahoe kernel linking passes. Validation
records are retained separately from the package.

The final package still needs physical boot/application testing. Sequoia kernel
linking, other GPUs, commercial CrossOver, DLSSG and ray tracing remain
unqualified. Earlier user-confirmed four-monitor rotation and ZZZ/HSR/DLSS
results apply to the retained implementations, not a new boot test. Upstream
still reports overly dark WebGL shadows; sampler/texture-array fixes do not
establish that this separate rendering issue is resolved. No FPS gain is claimed.

## Upstream 2.6.3 integration

Nine files had textual conflicts: the changelog, framebuffer, plugin device/event/library/Vulkan
sources, GL performance fixture, translator inliner and wrapper. Preserve both release histories.
The merge includes the following equivalences and decisions:

- Upstream now includes the original GL constant-buffer/shadow PR and native rotation adaptation.
  Keep each implementation once. Retain the later 16-sampler retry, bounded vector-prefix loads,
  descriptor-array/pointer-escape guards, committed GL resource snapshots and the stronger cursor,
  identity and current-mode repairs. Keep both physical-address fallback and uniform-table exports.
- Take the typed immutable pointer-field forwarding, instruction-metadata parsing, native math,
  buffer-helper/write-only shader repairs, MPS imageblock handling and Fx hashing. Preserve the
  fork's sampler/A8/threadgroup retry options and AIR-based uniform eligibility.
- Take the submit-ahead queue and part-retirement locking; lower the fork's committed-resource
  uses under that same retirement lock. Keep uncertain-fence quarantine, conditional WindowServer
  completion and capture/page-off ordering. The separate PR 398 foreign-process busy-tag polling
  remains excluded, as in the prior fork; it is not required by native rotation or the new caches.
- Take retained host-page imports/readback, application surface-upload scheduling, sampler-slot
  restoration, framebuffer-fetch descriptor reuse and asynchronous shader-cache writes/touches.
  Keep per-surface publication locking, descriptor/sampler caches, submission-copy discovery,
  strict stencil detection, bounded badge mip tracking and release `-O2`.
- Keep ANGLE Metal enabled by default for every app, with existing overrides and the opt-out.
  Upstream's broader browser/framework detector still narrows the policy and is unnecessary here.
  Retain the tested Chromium GPU-process/bridge-access decoder policy and native service delegation.
- Take backlight persistence, display-audio mapping/refresh, raster bounds and HiDPI mode flags.
  Keep the DVI startup timing repair, Tahoe-only rotation/opt-outs, scale-list ownership, per-head
  identities and stable current timing IDs. Successful modesets refresh audio and cursor geometry.
- Take upstream's main RM BAR1 placement/utilization and optional Intel-panel/HDA changes. Reuse
  its matching release RM and unchanged NVK renderer rather than trying to link unavailable vendor
  Darwin archives. Both OS accelerator variants must include the new utilization interface.

Git's automatic merge silently concatenated both cursor-position transforms. Replace that result
with the fork's existing locked 64-bit coordinate path; otherwise positions would rotate twice.
Keep its kernel-priority ONCE worker, exported cancellation and accepted-work drain. The ordinary
audio worker joins the generic cleanup list; the cursor remains outside that list.

Use the official **2.6.3** package for RM, HDA/Intel extensions, codecs, firmware, installer and
version metadata. Rebuild the release plugin, translator, NVRMFB and both 15/26 NVAccel variants;
retain the coordinated AGDC stable-slot binary after source/hash verification. Include both the
Minecraft runtime and compact Wine/NVK provider/DXVK pair. Their sources and renderer ABI are
unchanged by this merge. Release preparation does not overwrite the manual Downloads/pkgroot.

Initial validation records are under `out/upstream-2.6.3-merge-20261028-111911/`. The native translator's
initial full library run reported 2191 passes, 17 failures and one ignored test; all 17 failures also occurred
on untouched upstream 2.6.3 under the same tools. The wrapper has 43 passes and two ignored tests;
two tests needing unpublished local AIR files are filtered explicitly. Focused shader integration
and host tests cover the retained repairs and new cache/audio/retirement behavior. This is not a
fully passing upstream suite, a hardware boot, a four-monitor repair confirmation or an FPS result.
The release plugin/translator, framebuffer and both accelerator variants compile. Thirty host
scripts, two C++ harnesses, 22 focused Rust integration cases and the GL C-ABI translation checks
pass. Package manifests/signatures and isolated Tahoe kernel-collection linking pass. Sequoia
kernel linking, physical boot and application/multi-monitor testing remain outstanding.

## Translator repair following 2.6.3

Retain strict typed-pointer address-space parsing, consistent direct/helper
`array_ref<void>` address-table access, generic pointer-table loads and real 64-bit
device addresses in local pointer stores, including derived offsets. Preserve the
RGB10A2 residual FMA's `NoContraction` decoration; GPU half-way rounding controls
fail without it. These are correctness repairs, not an FPS claim.

The refreshed tests recognize shared guarded-load helpers and bindless resource
slots while checking the actual data flow. Keep the positive callback/local-store
regressions and bounded GPU readback harness. Do not restore the old expected
callback rejection, ignored duplicate, exact diagnostic strings or obsolete fixed
texture bindings. Compare equivalent upstream implementations before future merges.

The repaired full library suite passes 2,210 tests, with no failures or ignored
tests. See [Translator validation](TRANSLATOR-VALIDATION.md) for coverage and wrapper
fixture exclusions. Local repair records are under
`out/translator-repair-20261028-122757/`. The isolated release replaces only the
translator; previously verified kext/plugin/runtime binaries retain their producing
commit and hashes. No live installation or new Downloads/pkgroot is part of this work.

## Native NVK runtime for Minecraft 26.3

Retain the optional frontend and build/test tools in `nvk/native-macos/`, alongside
the [NVK Bridge mod](https://github.com/HighDelay/minecraft-nvk). This does not change
the installed plugin, default Vulkan provider, framebuffer or existing Metal,
OpenGL and MoltenVK paths. An instance opts in by installing the companion mod
and selecting Minecraft's Vulkan backend; the runtime manifest is validated before selection.

Keep application Vulkan objects on the private renderer's device, with local symbol
resolution and a distinct renderer install name. Preserve stable swapchain images,
separate drawable reservations, queue synchronization, completion before dirty/tag
publication and retained resources after uncertain GPU completion. Normal presentation
copies on the GPU; do not replace it with a CPU pixel transfer or import foreign-device
Vulkan handles. When changing NVK or the driver, check the version-3 IOSurface import
record, `nvmtl_nvk_stage_import`, `nvmtl_nvk_surface_dirty` and VRAM generation tags
against the bundled renderer before rebuilding the runtime.

Preserve optional staging of both OS accelerator variants and the curated Minecraft
runtime under `Library/GPUBundles/nvmtl/minecraft`. Keep the bundled renderer's
Sequoia 15.5 minimum, the frontend's 15.0 deployment target, compatibility
checks, manifest/hash/signature validation and independent 15/26 checksum manifests.
The existing installer chooses the OS variant and manages the runtime through its
ordinary nvmtl copy/removal path; keep default Vulkan selection opt-in through the
companion mod. Mod 0.1.2+ discovers that installed directory automatically; explicit
custom paths take priority and `nvkbridge.enabled=false` must still disable selection.

RTX 2060 SUPER/Tahoe 26.7.1 tests cover actual device requirements, SDL presentation,
pixel checks, resize/restore, LWJGL/SDL provider equality, Fabric Essential gameplay
and a NeoForge 26.3.0.99 game session confirmed by the user and its native NVK log.
Other GPUs, broad WSI conformance and a controlled performance
comparison remain unqualified. See [Native Vulkan presentation](../nvk/native-macos/README.md).

## Wine presentation resource reuse, 2026-11-13

Retain default-on reuse of completed presentation resources in the Wine frontend,
the `WINE_NVK_PRESENT_REUSE=0` troubleshooting opt-out and independent, default-off
`WINE_NVK_PRESENT_PROFILE=1` counters. The native Minecraft frontend remains
unchanged. Keep the exact retained IOSurface/layout match, three-entry and 64 MiB
per-swapchain limits, drawable release after presentation, cache cleanup and
acyclic ownership. Reuse is allowed only after successful GPU completion; preserve
reset-failure handoff draining and uncertain-completion quarantine.

The matched ZZZ D3D11/DXVK samples at 1080p High with MSync measured 74.0 versus
74.9 FPS near the display's 75 Hz ceiling, while preparation/cleanup CPU overhead
fell from 0.441 to 0.005 ms per frame. The user reported smooth rendering. This is
a short single-host trial, not a large uncapped FPS or broad compatibility claim.
Preserve the unchanged seven-word stats ABI and distinguish render+copy fence
wait from GPU copy execution time. See the
[results and validation limits](WINE-PRESENT-REUSE-20261113-RESULTS.json).
Keep the existing driver-owned provider contract; no YAAGL runtime bundle or
launcher setting is required.

## Native NVK runtime for Wine / DXVK

Retain `nvk/wine-macos/`, `tools/wine-nvk/` and the shared frontend's
`NVKBRIDGE_WINE` build mode. This adds opt-in Wine support without changing the
plugin, framebuffer, desktop presentation or default Vulkan provider. The Wine
frontend exports `vkCreateMetalSurfaceEXT` directly through the same guarded
implementation as proc-address lookup. Preserve Wine-only filtering of unsupported
WSI/HDR/external-handle extensions and queries, the private staged IOSurface
import, stable swapchain images and completion/lifetime rules from Minecraft.
Resize of a valid drawable returns OUT_OF_DATE so Wine can recreate its chain.

Keep the matched DXVK 3.1.1 shared-resource fallback repair, source/license records,
`dxvk.enableDescriptorBuffer = False` default, and paired DLL install/rollback
journal. A rejected external-sharing request must not call an unavailable Win32
export function. Do not advertise general GPU-handle sharing or fake successful
exports. Descriptor sets restored ZZZ characters and HUD text in an actual A/B test.

Preserve optional `--wine-runtime` staging of the compact provider under
`Library/GPUBundles/nvmtl/wine`, separate from Minecraft. Validate the manifest,
hashes, signatures, Intel architecture, macOS 15.5 compatibility, provider alias,
licenses and checksums before publishing. YAAGL release archives contain launchers
only; Wine downloads on first setup, while NVK and the matched DXVK pair come
from the installed driver provider. Prefix DLL originals must
remain restorable after a runtime update, including interrupted operations.

YAAGL integration is maintained in HighDelay/yaagl-nvk, repaired DXVK in
HighDelay/dxvk-nvk (`nvk-3.1.1`). YAAGL requires the installed driver provider;
there is no bundled renderer fallback or separate DXVK download. Its private
Wine archive is checksum-pinned and prepared without altering ordinary Wine.
An identical DXVK pair can adopt the provider identity
without replacing original DLL backups; a different pair still requires restore.
ZZZ and Honkai: Star Rail Global D3D11 gameplay are user-confirmed on RTX 2060
SUPER / Tahoe 26.7.1 with YAAGL Wine 11.0-1, including scenery, characters and UI.
HSR uses its Jadeite route with the required `--` separator; Steam Patch and the
timeout fix default off for HSR and on for ZZZ/GI in YAAGL NVK 0.2.1. Preserve
these game-specific defaults. A 2026-10-31 user report confirms GI startup and in-world rendering on
RTX 4060 Laptop / native NVK with both Steam Patch and Timeout fix enabled.
GI's new defaults and Apply Recommended Settings match that configuration;
saved explicit choices are preserved. Do not apply GI's defaults to HSR.
The report did not specify OS/Wine versions.
Retain YAAGL's shared `WINEMSYNC=1` environment for prefix bootstrap,
configuration, registry setup/rollback, games and wineserver commands. The pinned
macOS Wine 11.0-1 build supports MSync, not the old `WINEESYNC` setting. The user
reported ZZZ 1080p High D3D11/DXVK improving from 41–50 to 60–74 FPS after
the MSync session restart on RTX 2060 SUPER / Tahoe 26.7.1. Logs and samples
confirmed MSync; this is not an automated benchmark or a universal speedup claim.
CN gameplay, commercial CrossOver, other Wine builds and Sequoia Wine hardware
remain unqualified.
Native and Windows Vulkan/D3D11 probes verify correctness, not an FPS improvement.
D3D12 integration now includes pinned vkd3d-proton 3.1, optional NVIDIA Wine
NGX/DXVK-NVAPI, and a GPU-copy bitmap HUD. ZZZ D3D12 and DLSS SR Quality were
user-confirmed on RTX 2060 SUPER / Tahoe 26.7.1. Preserve the independent D3D11,
vkd3d and DLSS rollback journals, prefix-local registry original, signature/hash
validation, `WINE_NVK_*` child-process forwarding and the D3D11 default. DLSSG
keeps normal hardware checks; frame generation and RT are unqualified. The final
registry-aware helper now passes compiled DLL/registry rollback tests in private
Wine prefixes. Build validation is recorded in the 2.6.4 integration above.

## Tahoe display rotation

The current local candidate uses the native CoreDisplay rotation approach from
`0700c208` in PR 398, with fork repairs. Its separate surface-busy/wait commit
`566c1307` is excluded. Do not restore the extra producer compute/CPU correction,
output pools or old mode/copy/GPU protocols: they would apply rotation twice.
WindowServer owns pixel rotation and the ordinary presentation path scans out
its physical raster. Keep conditional completion/capture synchronization.

Retain the native rotated-mode publication, timing-ID matching and aliases,
224-entry macOS timing capacity, full-raster/reflection/allocation checks,
aligned timing copies and per-display cursor bitmap/hotspot/geometry mapping.
Keep the consistent current-mode-ID repair: adopt a matching replacement ID and
its descriptor persistently, and make `getCurrentDisplayMode` a read-only query.
Do not restore `fNewIdOnce` or an equivalent query-consumed answer. Preserve the
previous ID only when unambiguous and capacity permits; retain an omitted active
timing without shadowing a reassigned ID or reporting the boot mode as active.
Existing scaling overrides must survive; replace/remove only the scale array
published by this instance. A first-frame reconnect must use the deferred
connection worker, never call the family from inside the presentation gate.

Keep the indexed IOService location on each NVRMDisplay provider before the
framebuffer starts and the controller's path/registry-ID check. Preserve the
kernel-priority ONCE cursor call, accepted-work count, exported cancellation,
shutdown drain, display gate and cursor-state locking. The native approach does
not replace these lifetime repairs, parked backing, BAR1 budgets or scanout guards.

Enable rotation only on Darwin 25, preserving property/`nvrmrotation=0` opt-outs.
Use implementation marker 14 and native accelerator protocol 1. Reject nonzero
probes without a matching accelerator on the same PCI device; Standard restore
remains available. Rebuild NVRMFB, Tahoe NVAccel and the release plugin together.
Upstream 2.6.3 now includes the original rotation adaptation. Keep its fork repairs
once rather than applying rotation twice. The separate surface-busy wait from
PR 398 remains excluded.

Earlier four-monitor RTX 3070 captures exposed wrong-monitor rotation, mode fallback
and mirroring; initial snapshots alone missed later failures. On 2026-10-28 the
same tester confirmed all four displays working, with both Dell P2214H displays
in portrait. The maintainer also reports working rotation with the second DVI
monitor on RTX 2060 SUPER. Exact four-monitor release/binary identity and the
necessity of individual repairs were not established. See
[Display rotation](DISPLAY-ROTATION.md) for the reports and remaining test limits.

The local controller-separation test adds `NVRMDisplayIndex` as the stable driver
slot, preferred by both NVAccel and NVRMAGDC with the older dependent-index fallback.
Keep PCI ownership, range/type/duplicate checks and Intel slot 4. When explicitly
enabled, secondary heads receive distinct controller IDs before family startup,
with local dependent index zero; head zero keeps the Intel-compatible legacy group.
`nvfbcontrollers=0`, rotation disablement and non-Tahoe kernels preserve shared
grouping. The three kexts must be staged together. Host checks pass, but the earlier
four-head capture still mirrored with this repair installed. The later consistent
timing-ID repair passes host mode/metadata tests. The subsequent successful user
report does not isolate either change as the mirroring fix. Keep controller
splitting disabled in ordinary packages and retain the independent identity and
timing-reporting correctness repairs.

## Digital DVI RGB scanout, 2026-11-16

Retain `620-dvi-rgb-scanout.patch`: digital TMDS DVI-D/DVI-I uses RGB8 in
indexed and explicit mode validation and the full modeset converter. Reject
remaining YUV420 dispositions because the public KAPI mode omits that sideband.
Keep validation and commit consistent. HDMI/DP/internal/analog connectors and
failed connector queries retain their existing color policy. Preserve EDID/link
validation, strict completion witnesses and surface retirement; never clear a
submission fault to force a later rotation through.

The user confirmed corrected DVI output and rotation alongside HDMI on RTX 2060
SUPER / Tahoe 26.7.1 with the verified candidate loaded. RGB8 modesets succeeded
without the previous completion fault. Sanitized production-policy tests and
negative controls, existing DVI/concurrency tests and isolated Tahoe kernel links
passed. Other GPUs, adapters, Sequoia and sleep/wake remain untested. See
[DVI modes](DVI-MODES.md).

## DVI startup timing validation

Retain `nvrm-dvi-modes.h` and its startup call before the boot raster/scanout is
established. Digital DVI-D/DVI-I must keep an already-valid timing; an invalid
one may use only a same-visible-resolution progressive mode accepted by both
indexed and explicit NVKMS validation. Preserve the nearest-refresh choice,
bounded enumeration and unchanged output on failure. Do not hardcode 1080p75,
force dual-link, disable EDID/link checks or carry over the diagnostic ioctl helper.
Keep explicit validation on KAPI-generated DVI refresh modes and the EDID match
shortcut. Other connector types retain their selection; presentation, cursor,
rotation, parked backing and public KAPI dependencies are unchanged.

The RTX 2060 SUPER/ITY V2422F test boot selected 1080p75 after rejecting 1080p60;
both DVI and HDMI modesets succeeded and the user confirmed output. That test
used shared controller grouping. Other GPUs/adapters and independent four-head
rotation remain unqualified. See [DVI modes](DVI-MODES.md).

## Historical upstream 2.6.2 integration

Upstream adds four commits after 2.5.5, changing only `package/install.sh` and
`CHANGELOG.md`. Resolve the changelog conflict by retaining the fork entries and
adding the 2.6.0, 2.6.1 and 2.6.2 release notes. Keep the installer as provided.

The installer queries existing no-team kext approvals and refreshes a signed
bundle's stored load-history hash only when its bundle is already allowed.
Unsigned bundles do not gain a signing requirement; first installs retain the
normal approval guidance. If every selected extension is already allowed, the
installer reports that the update takes one restart. Database-write failures
remain nonfatal and print the upstream note; that approval count is not proof
of a successful hash update or of the next boot's actual approval state.

No rendering, kernel, translator, native NVK, firmware or build source changed.
There are no new equivalents of the fork's DVI repair, rotation/controller
identities, cursor shutdown/drain, decoder policy, all-app ANGLE default or
OpenGL/Minecraft/MoltenVK follow-ups. Preserve them without applying another copy.
Compiled source retention is checked against fork `7004150`; no driver rebuild
is needed for this upstream delta.

Validation covers installer shell syntax, isolated approval-database fixtures,
existing installer/package checks and byte-for-byte retention of the protected
source. The fixtures use temporary databases and mocked signatures, not the live
KextPolicy database. They do not qualify macOS approval prompts or boot behavior.
Records are identified by `out/upstream-merge-latest.json`. This source merge
does not push, stage a package or alter the installed driver. When staging a
2.6.2 release, use its installer and version/dependency metadata with the retained
fork builds and native Minecraft runtime.

## Historical upstream 2.5.5 integration

Two files had textual conflicts: `CHANGELOG.md` and `plugin/NVMTLDevice.m`.
Keep both histories. Upstream's Discord decoder repair (`9a00f88`) covers the same
registration regression as the fork, but delegates only when a sandboxed process
has a `com.apple.*` bundle identifier. It registers in every other sandboxed
client without checking accelerator access. Retain the fork's tested policy:
only an exact Chromium GPU-process type with both RM bridge connections available
gets local registration; all other native sandboxed clients retain service decoding.
Keep service exceptions, cached probes, connection cleanup and codec-disable guards.
This preserves the Discord and QuickTime behavior already tested on the fork,
without adding a second registrar or relying on a bundle-name allowlist. Adapt
upstream's sandbox fixture to exercise the retained production policy and denied access.

Upstream's Brave and Edge additions are already covered by the fork's all-app
ANGLE Metal default. Preserve that default, existing overrides and the opt-out.

Take the same-size buffer-texture view repair (`86083d2`). A mutable linear/DRM
buffer alias now lists the device-supported compatible formats, including BGRA/RGBA
reinterpretation used by screen capture. Require the image's own format and every
requested usage feature, validate the combined list against the device, and keep
unlisted or wrong-size views refused. Failed or unavailable capability queries
fall back to the original single-format image. Keep `vfl` mask checks in resource
migration/purge eligibility, alongside the fork's caches and lifetime guards.

Take the NVAccel refusal-log throttle (`f675901`): first eight refusals and then
every 256th, while counting every refusal. It changes no allocation decision,
parked backing, scanout, BAR1 budget, flip or completion policy. The framebuffer,
main RM, translator, NVK, rotation, cursor serialization/drain and existing
OpenGL/MoltenVK optimizations are unchanged by this integration. No equivalent
upstream native-rotation or cursor shutdown/drain repair was added.

Use the official **2.5.5** package for the matching runtime, firmware, codecs,
installer and version/dependency metadata when staging. In particular, its release
ships encoder-library keyframe metadata and deployment-target repairs which are
not changes to these plugin sources. A source merge or local plugin rebuild alone
does not update those installed codec libraries. NVIDIA headers remain 615.78.08.

Build and source-retention records are identified by `out/upstream-merge-latest.json`.
The release plugin (`-O2`) and Tahoe accelerator compile, with no new warning
messages or accelerator kernel imports. Fourteen focused host checks pass,
covering real-sandbox decoder policy with mocked bridge access, decoder cleanup,
format-list filtering/fallback under ASan/UBSan, ANGLE defaults, cache/resource
lifetime, surface publication and retained source. The older GL descriptor fixture
failed identically on the pre-merge revision; update its mocks to the existing
uniform-buffer interface and retain its descriptor-reuse/state checks.
Validation is limited to host fixtures, source comparison and compilation; the
combined build still needs Discord/QuickTime, screen-capture, boot and physical
multi-display tests. This integration does not update the installed driver or
either existing package folder.

## Historical upstream 2.5.4 integration

Two files had textual conflicts: `CHANGELOG.md` and the ANGLE policy in `plugin/NVMTLDevice.m`.
Keep the fork history and upstream release entries. Upstream adds Discord to its browser-name allowlist;
the fork already covers Discord through its all-app default. Keep the broader default and
`NVMTL_ANGLE_METAL=0` opt-out once, without introducing a second constructor or a name filter.

Keep the H.264/HEVC registration access check, but do not restore upstream's blanket sandbox refusal.
Chromium checks local VideoToolbox hardware support before creating a decoder: suppressing registration
can reject H.264 without ever reaching the decoding service. Limit this exception to sandboxed Chromium
GPU processes, identified by the exact `--type=gpu-process` argument, without an app-name allowlist.
Respect the `--` terminator and the last process-type switch. Register only after both accelerator RM
bridge client types can be opened; close both probe connections immediately and cache the decision
across codecs. QuickTime and other native sandboxed clients keep upstream's service path even when
they could open the bridge. Checking driver access alone was too broad: QuickTime used local NVDEC and
froze its picture near three seconds while the playback clock continued. Clients denied access also
defer to the decoding service. Preserve the service/suffixed-name exception, decoder disable switches,
WindowServer exclusion and asynchronous
service registration. Encoder registration and the ANGLE policy remain independent.

On RTX 2060 Super / Tahoe 26.7.1, an isolated copy of Discord's Electron 42.11.15 reproduced
`kUnsupportedConfig` with zero H.264 frames before the initial repair. Restarting Discord confirmed
uploaded chat videos played with picture and sound, with its GPU sandbox still active. After narrowing
the exception, generated H.264 and HEVC clips decoded 642 and 644 frames respectively in the isolated
Electron test, with zero reported drops or playback errors and no diagnostic shim. The user confirmed
normal QuickTime playback of a 1920x1080 H.264/Opus clip that had frozen with the broader check; its
restarted process no longer loaded local NVDEC libraries. CPU checks cover
denied or missing clients, connection cleanup, cached decisions, the service exception, native clients,
custom helper names and exact process-type parsing (`tools/test_video_registration.py`). Other GPUs
and sandbox profiles have not been tested.

Take upstream's headless deadline check: if no NVIDIA head has published and no other GPU or
`NMIntelFB` owns the display, leave `MetalPluginName` withheld so WindowServer can use the firmware
framebuffer without acceleration. Existing early handover and Intel-panel handling remain. This
does not change normal display arming, parking, scanout allocation or flip verification.

Take the `nv_parms` repair as a null-terminated data table rather than a function stub. Its name/data
layout matches NVIDIA 615.78.08's registry-option parser. This changes the main RM and needs the
matching 2.5.4 RM binary when packaging; a plugin or accelerator rebuild alone does not include it.
Take the installer deny-only ACL handling, third-party kext failure diagnostics and installed licence texts.

No new equivalents of the remaining local fixes were added. The framebuffer, cursor shutdown/drain,
translator, native NVK, GL/MoltenVK caches, uniform-buffer promotion, bounded vector-prefix loads,
16-way sampler retry, specialization/pointer-escape guards, stencil and badge repairs, submission-copy
cache and release `-O2` are unchanged from the preceding fork. Parked-backing restoration and
upstream's conditional completion/capture synchronization remain included once. Keep the retired
experimental framebuffer policies and unconditional Dock wait removed.

Build/source-retention records are identified by `out/upstream-merge-latest.json`. Use NVIDIA
615.78.08 headers and matching official **2.5.4** RM, runtime, codecs, firmware, installer, licence
directory and version/dependency metadata for a subsequent package. This source merge does not
update `~/Downloads/pkgroot` or the installed driver. Earlier hardware results do not validate
sandboxed video, headless fallback, boot options or performance for this combined build.

The release plugin (`-O2`) and Tahoe accelerator compile. The changed RM OS-interface file also
compiles as an object using the public headers' `NV_KERNEL_INTERFACE_LAYER` branch, and `nv_parms`
is emitted as data; this is not a linked RM or a vendor ABI check. Seven focused CPU checks cover
ANGLE defaults/overrides, headless conditions, the parameter-table walk, installer ACLs, third-party
kext diagnostics and licence inclusion. Installer/build shell syntax checks pass. No kernel-collection,
boot, sandboxed-video, GPU or performance test was run for this integration.

## Upstream 2.5.2 integration

Three files had textual conflicts: `CHANGELOG.md`, `kexts/NVRMFB/fb/nvrm-fb.cpp` and
`translator/wrapper/src/buffer_addresses.rs`. Keep the fork history alongside upstream's new releases.

The framebuffer keeps upstream's NVIDIA 615 output-LUT surfaces, hotplug/EDID/audio association,
connection notifications and opt-in deferred flip verification. Preserve the fork's cursor allocation
at kernel priority with ONCE, its stopping/gate checks, accepted-work counter and exported
`thread_call_cancel` followed by explicit drain. Clean up `fConnectNotify` in the other worker list;
keep `fCursorCall` out of that list because its dedicated drain already freed it. Failed starts free
either allocated callback handle. Do not import `thread_call_cancel_wait`.

Keep upstream's descriptor path for data-dependent runtime arrays and its Uniform-table conversion
fallback when physical-address lowering is refused. Before mutating types, discover every supported
pointer chain to a fixed point. Check every operand for pointer escapes; a pointer stored as a value
must exclude its own root from logical retention even when the destination is another candidate root.
Keep table-helper/control-flow/store/return escape refusal. Clone all proven logical pointers,
independently of block order, after their pointee definitions and before their moved global variables.
Unsupported uses retain the existing descriptor fallback or controlled refusal; this does not add
helper-pointer support or general dynamic uniform-buffer promotion.

Take the remaining rendering, completion-witness, headless/startup, temperature and installer changes.
Keep upstream's default flip verification mode 1; mode 2 is opt-in and needs separate hardware testing.
The native NVK/Mesa source is unchanged. `nvAccelRestoreParkedForDirty` and GPU-current publication
are identical to upstream and must remain included once. The all-app ANGLE default, strict stencil
detection, stronger badge mip tracking, committed GL resource snapshots, argument-buffer routing,
guarded GL timing, submission-copy cache, MoltenVK binding/sampler caches and release `-O2` remain.
The experimental framebuffer allocation policies and unconditional Dock wait stay retired.

Use NVIDIA **615.78.08** headers. The fork build helpers now default to `~/ogkm615`; `OGKM` can name
another checkout. Public sources have no Darwin compile metadata or prelinked RM/KMS archives.
For packaging, use the official **2.5.2** RM, runtime, codecs, firmware and installer, preserve its
version/dependency metadata, and rebuild the plugin, translator, NVRMFB and Tahoe NVAccel. Do not reuse
the 2.4.3/610 RM or framebuffer as the new package base. The old 610-specific completion-archive
helper and vendor patches are historical; the 615 viewport patch belongs in a full matching vendor KMS build.

Compilation and source-retention records are identified by `out/upstream-merge-latest.json`.
The release plugin, translator, framebuffer and Tahoe accelerator compile with the 615.78.08
headers. Their source snapshots match the resolved merge. The translator exports the new
`nvmtl_table_to_uniform` fallback. Framebuffer imports add `OSString::metaClass` for display-audio
metadata and remove the old delayed-connect entry point; no unexported cancel/wait call is imported.
This integration does not stage/install or run automated tests, kernel-collection checks or GPU/runtime
measurements. Previous boots and performance results do not qualify this combination. The subsequent
local uniform-cbuf and shadow translation integration is documented below.

## GL uniform buffers and shadow translation follow-up

The previously separate uniform-cbuf source is now integrated locally on the 2.5.2 base,
including bounded integer-expression folding and the float32 stride-4 word extension.
Retain AIR name/access/address-space eligibility, fixed-range proof, supported-device limits,
separate binding-641 layouts, descriptor-layout/buffer/range cache identity and per-draw
alignment/range fallback to the original physical-address pipeline. Preserve the GL slot through
sampler specialization without replacing its specialized SPIR-V or attaching unspecialized AIR.
Keep both the promotion export and upstream's `nvmtl_table_to_uniform` export and fallback;
one does not replace the other. Keep release per-process disable/diagnostic switches.

The shadow repair targets the inliner's renamed actual first block. Retry-only sampler-phi
splitting is limited to sample/gather reads in the same block with dominating inputs, bounded
sampler fan-out and distinct generated names. Do not broaden this to arbitrary AIR calls or
duplicate texture writes/atomics. Shaders that translate as written keep their existing path.

The BSL/Complementary follow-up accepts 2 through 16 distinct incoming samplers on that retry;
17 stays refused. Preserve the same dominance, call-kind and use-site guards. For a shorter
32-bit float/int vector load through a wider vector GEP, load and bitcast only the requested
scalar lanes. The GEP's vector stride does not prove that the full vector is readable: an
8-byte prefix at offset 16 must work with a 24-byte buffer without reading to byte 32.
Keep this fallback after the earlier load handlers, including the reverse int-to-float case.
Preserve sampler/A8/threadgroup options across the pointer retry and refuse global pointer
captures before cbuf promotion, as established by the preceding translator repair.

The follow-up release translator compiles. CPU checks cover the prefix bounds, 9/16-sampler
translation, the 17-sampler refusal and retained specialization/cbuf behavior. An 84-module
public-input comparison retained identical bytes for all 68 previously translated inputs,
added one translated fixture and kept 15 controlled refusals. One descriptor-array assertion
also fails on the parent because this fork routes those textures through the heap; it is not
counted as a new regression. This does not qualify shader-pack visuals or FPS on hardware.

Framebuffer, cursor drain, NVAccel, native NVK and completion/parking policy are unchanged.
The user's other-machine success applies to the earlier uniform-cbuf build; the supplied shadow
report used offline text rewrites, and does not establish hardware correctness or FPS for this
integration. See [OpenGL performance](OPENGL-PERFORMANCE.md) for behavior, switches and limits.


## Historical upstream 2.4.3 integration

Two files had textual conflicts: `CHANGELOG.md` and the cursor allocation in
`kexts/NVRMFB/fb/nvrm-fb.cpp`. Keep the fork history alongside upstream's release entry. Take upstream's
`nvrmfbCall` helper and its ONCE allocations for vblank delivery, the fallback timer, hotplug/connect,
backlight, GPU wake, reflip and diagnostic workers. Each handle runs at most one invocation at a time;
different workers can still run concurrently. This extends the cursor serialization already in the fork.

Keep the fork's cursor allocation at `THREAD_CALL_PRIORITY_KERNEL` with `THREAD_CALL_OPTIONS_ONCE`,
its exported cancellation, outstanding accepted-work count/drain, stopping/status checks and failed-start
cleanup. Upstream uses HIGH priority for its helper and does not replace these cursor lifetime repairs.
The cursor scheduling, callback and stop/drain functions remain unchanged from the preceding fork.

Take upstream's AIR-text digest repair: mark internally produced long shader text and reuse its digest
state. The earlier mutable-string class check prevented reuse for CF-backed text. Cache keys and hashing
are unchanged; the new marker limits reuse to text the driver does not edit. Do not replace the existing
OpenGL/MoltenVK binding, sampler or resource caches with this separate optimization.

Main RM, NVAccel, Rust translator, BAR1 budgeting, scanout/modes, ANGLE defaults, strict stencil detection,
badge tracking, submission-copy caching, guarded GL timing and release `-O2` are unchanged by this merge.
Keep upstream's conditional completion policy; the unconditional Dock wait and experimental framebuffer
allocation policies remain retired.

Compilation records are under `out/upstream-2.4.3-merge-20261020-122844/`. The release plugin (`-O2`)
and framebuffer compile for x86_64 with a macOS 15.0 minimum. The framebuffer adds no undefined imports;
the unused `_thread_call_allocate` import is removed. Never introduce `thread_call_cancel_wait`.
No automated tests, kernel-collection
preflight, runtime/GPU checks or staging are performed for this merge. Before the next hardware test, use
the official 2.4.3 package and preserve its version/dependency plists while adding the fork outputs. The
previously verified fork build was 2.4.2; its boot and performance results do not qualify this combination.

## Historical upstream 2.4.2 integration

Only `CHANGELOG.md` had a textual conflict; retain both the fork history and new upstream entries.
The plugin's generation-specific Turing/Ampere H.264 and HEVC encoder registration and background stale
SPIR-V cache sweep merge alongside the existing fork behavior. Keep the matching codec libraries from the
official package. Cache pruning removes other translator builds' entries; it does not eliminate compilation
for a new translator. `NVMTL_SPVCACHE_KEEP_STALE` disables the sweep.

Take upstream's main RM power-source reporting and installer boot-guard job as provided. Firmware queries
read cached battery/mains state, refreshed at startup and power-state transitions outside firmware locks.
The new job treats WindowServer being present at two observations 45 seconds apart as a successful boot;
it does not verify a usable desktop or the same process. The existing kernel guard ordering is unchanged.

No new equivalents of the remaining fork-specific repairs were added in this update. Framebuffer, NVAccel
and translator source are unchanged from the preceding merge. Retain the kernel-priority ONCE cursor worker,
exported cancellation and outstanding-work drain, strict stencil detection, all-app ANGLE Metal default,
stronger badge mip tracking, guarded GL timing/resource/descriptor repairs, MoltenVK caches, submission-copy
caching and release `-O2`. Keep upstream's parked-backing restoration, scanout/allocation and conditional
WindowServer completion policy once; the old unconditional Dock wait and experimental framebuffer remain retired.

Use the official **2.4.2** package for the main RM, matching runtime, codecs, firmware, installer and version
metadata. Upstream reports that 2.4 shipped an outdated framebuffer binary and 2.4.1 still had a main RM built
without a required display interface; 2.4.2 corrects those release builds. Do not use either older package as
the staging base. Local NVIDIA headers still lack the Darwin RM/KMS archives needed to link the main RM.

Build/staging records are under `out/tahoe-2.4.2-stage-20261020-081614/`. The release plugin, framebuffer
and Tahoe accelerator compile successfully. The preceding signed fork translator is reused after checking
all 381 tracked translator inputs and its executable hash against the compiled snapshot. The staged
`~/Downloads/pkgroot` preserves 2.4.2 plist versions; both regenerated checksum manifests and all replacement
hashes pass. Kexts remain unsigned; userland signatures are verified. Validation is limited to source review, compilation, artifact
identity, signatures and package integrity; no automated tests, kernel-collection preflight or runtime/GPU
tests are run for this integration. Earlier successful boots and performance results do not qualify this build.

## Historical upstream 2.3.7 integration

Ten files had textual conflicts. Resolve the overlaps by behavior; do not reapply the earlier
OpenGL imports or parked-VRAM repair on top of upstream's equivalent code.

| Upstream inclusion | Fork decision |
| --- | --- |
| Parked-backing restoration and the earlier OpenGL/Minecraft and binding/sampler caches, included in 2.3.6. | Use the shared implementation once. NVAccel source now matches 2.3.7, including its plane-index bounds. |
| Fetch-disabled colour inputs and A8 render-target alpha/blend handling. | Keep both new upstream repairs alongside the existing ordered-triangle fetch path. |
| Dock badge mip filling. | Keep the fork's shared CPU/GPU eligibility checks, atomic root tracking, resolve/storage/view tracking, and checked row arithmetic. Upstream's simpler mip flag and broader GPU-copy fallback do not cover those guards. |
| Asynchronous cursor moves and refresh-aware OpenGL waits. | Keep the fork's serialized kernel-priority ONCE worker and exported cancellation/drain accounting; retain private-layout, timestamp, NVIDIA-backend and installation guards. |
| Default ANGLE Metal for Chrome/Chromium. | Keep the fork's enabled default for all apps, including Edge and Electron, with the same opt-out; retain upstream's activation log. The user reconfirmed this policy on 2026-10-19; do not narrow it to browser names or make Electron opt-in. |
| Submit-ahead from commit while the worker waits (PR 289), and single-field struct allocation repair (PR 291). | Take upstream's changes; keep committed-resource snapshots, uncertain-fence quarantine and upstream's conditional completion policy. |
| Private-copy installer and retired user NVKMS selectors (PRs 266 and 267). | Take upstream, including the follow-up that selects NVAccel from the checked copy. |

The branch also carries the 2.3.6 Tahoe icon/Liquid Glass regression repair: ask the translator for
visible-function references before deferring to pipeline linking, instead of depending on one error
message. Keep the fork's required function-constant checks, linked pixel-sampler variants and mesh/object
deferral. Preserve upstream's firmware-backup fallback and the card-mapping correction that accepts all
of the GPU's valid apertures, including display register windows; the earlier framebuffer-only restriction
caused WindowServer restarts. Keep the remaining upstream client/map-table, copy-region and shader-reader
bounds changes. NVRM source is identical to the release branch.

Still local: strict stencil marker detection, ANGLE Metal enabled by default for all apps,
submission-copy caching, release `-O2`, committed GL resource accounting, argument-buffer array/texel routing,
physical-pointer preflight/refusal, reflection/mipmap/constant repairs, serialized cursor shutdown and guarded
GL timing. The retired experimental framebuffer allocation/scanout policies remain removed.

The release plugin (`-O2`), translator, framebuffer and Tahoe accelerator compile for x86_64 with
macOS 15.0 as their minimum. The rebuilt framebuffer imports no new kernel symbols compared with the
preceding exported-cancellation repair. This is not a kernel-collection link or boot test.

Compilation and validation limits are recorded with the current build artifacts under
`out/upstream-2.3.7-merge-20261019-112636/`. The final staged package and review are recorded under
`out/tahoe-2.3.7-stage-20261019-115452/`. Five focused CPU fixtures passed: ANGLE environment overrides,
the all-app default (including Edge and Electron), surface pending-resource lifetime, overlapping surface
publication and GL committed-resource accounting. Both package manifests, userland signatures, output hashes
and all 503 saved source hashes were verified. No runtime/GPU checks were run for this integration.
Earlier successful boots, badge checks and performance measurements do not validate the combined
2.3.7 build or establish the gain from submit-ahead on the RTX 2060 Super. Recheck Dock previews, icons,
Terminal redraw, OpenGL/MoltenVK, window dragging and cursor/shutdown behavior after installation.

Use the official **2.3.7 package** for its matching main RM, runtime, installer and firmware layout, plus the
rebuilt fork plugin, translator, framebuffer and Tahoe accelerator. The local NVIDIA headers lack the Darwin
RM/KMS build metadata and archives needed to link a complete main RM, so reuse the official 2.3.7 NVRM binary;
its source is unchanged by this merge resolution. `pkgroot` and the installed driver are not updated by a source
merge or these compilation checks. Preserve package version metadata and refresh both checksum manifests when staging.

## WindowServer completion policy after the 2.3.7 integration

The fork's unconditional completion wait from `0b121d3` is retired. The user reports that official 2.3.7
already displays Dock previews correctly. That observation does not isolate the responsible change, but upstream
contains the explicit capture repair in `79f47d0`, strengthened by `40daaa6`: mark VRAM current at commit,
keep pending writes counted across command buffers, and wait at CPU page-off before copying unfinished pixels.
The 2.3.7 visible-function repair may also affect preview rendering; it has not been isolated here.

Restore upstream's `commitAndWaitUntilSubmitted` policy: WindowServer waits only until scheduled when NVAccel
accepts GPU marks, and waits until completed when it does not. `NVMTL_WS_WAIT_COMPLETION` still forces the
completion fallback, as does disabling the kernel marks. Preserve the surface/page-off synchronization and
submit-ahead coordination. Do not reintroduce the unconditional wait during a future merge based only on the
historical 2.1.0 preview result. The fork combination still needs fresh previews/capture and performance checks.

## Historical upstream 2.3.5 integration

The unconditional-wait decision in this historical section is superseded by the 2.3.7 policy above.

- The only textual conflict was `CHANGELOG.md`; both the fork additions and upstream release history are kept.
  The release plugin build also exposed a declaration of `nvtrace` colliding with the retained OpenGL
  call-site macro. Guard that declaration when the macro is already defined; keep the macro's argument-cost saving.
- NVRM and NVRMFB source remain identical to upstream. Keep its display-aware BAR1 budget, scanout lifetime,
  plugin-write cookies, capture ordering, refresh-derived flip waits, hardware-vblank retries and startup changes.
  Do not restore the retired experimental framebuffer policies.
- At this integration, upstream lacked the fork's parked-backing reactivation and unconditional WindowServer
  submit-and-wait completion wait, so both were retained. Restore the same parked allocation before upstream's dirty mark records its
  allocation cookie. Kernel-gated capture publication complements the Dock wait; it is not treated as proof
  that every offscreen consumer can safely use scheduling-only completion.
- Keep all ten OpenGL/Minecraft and MoltenVK commits below, strict stencil detection, Chromium's enabled default
  and release `-O2`. The new upstream remote release helper also uses `-O2`; the existing local build script
  retains its settings. Translator and native NVK sources are unchanged by this upstream update.
- Package against the official **2.3.5** RM/framebuffer and installer, with the rebuilt fork plugin and matching
  Tahoe accelerator. Reuse the fork translator built from the unchanged source. The prior 2.3.0 package is not
  the matching base for this merge.

The merged release plugin compiles for x86_64 with macOS 15.0 as its minimum and `-O2`; the Tahoe accelerator
also compiles. No runtime/GPU regression tests or boot of this combined build have been performed yet. Earlier
Minecraft results below, and the reported successful ReBAR-off boot before this integration, do not validate it.

The upstream boot guard is retained as provided. A previously identified source-level ordering concern remains:
NVRM checks the stored failure count, then increments it; later components read the incremented count against
the same limit. This is not a hardware-reproduced failure. A working boot alone does not cover the retry path.

## Earlier Tahoe and performance fixes to retain

- Tahoe icon rendering and System Settings/CoreImage glyph repairs.
- Parked-VRAM backing restoration before ordinary surface dirty updates, including the repair
  introduced by `eb6544b`.
- Upstream's capture/page-off synchronization and conditional completion fallback for WindowServer. The older
  unconditional Dock snapshot wait is retired on the 2.3.7 base.
- Dock badge mip initialization after full level-zero uploads. Keep the shared eligibility checks for CPU and
  buffer-copy uploads: single-layer, single-sample 8-bit 2D textures, at most 2048 pixels on either axis, with
  no explicitly authored higher mip levels. Preserve atomic root-texture tracking for uploads, copies, render
  attachments/resolves, storage bindings and application mip generation. Do not broaden automatic generation
  to integer/depth/compressed formats, array/cube/3D textures or texture views.
- Strict detection of the dedicated stencil-export push-constant block.
- ANGLE Metal enabled by default for all apps loading the plugin, including Chrome, Chromium, Edge and Electron.
  `NVMTL_ANGLE_METAL=0` skips the automatic workaround. Preserve existing ANGLE overrides and activation logging.
  This broader policy was reconfirmed by the user on 2026-10-19; do not adopt upstream's Electron opt-in default.
- Submission upload-discovery caching, copy-backing deduplication and release plugin `-O2`.
- Upstream framebuffer handling, framebuffer-resource allocation, scanout lifetime and GPU mapping.
  The experimental framebuffer policies and borrowed-scanout extension remain retired.
- Upstream's general Terminal framebuffer-fetch repair and event-driven flip/submission waits.

See [TAHOE.MD](../TAHOE.MD) for their implementation history and build/staging procedure.

## OpenGL/Minecraft and MoltenVK commits to retain

### Newer `perf` follow-up on 2.3.5

The follow-up adapts `8e88b38`, `e13b339` and `153fe3c` from pengubow's `perf` branch at
`153fe3c464aea4c76750720351134b4afb473d5b` into one fork commit. Its build scripts are not imported;
the older OpenGL changes and `0695972` declaration repair are already covered by this fork.

Retain these behaviors when reviewing an equivalent upstream implementation:

- Recognized `NSCGLSurface` waits use the window display's period when image-queue timing would impose a
  slower rate. Keep method/ivar layout checks, valid timestamp/rate bounds, a fallback IMP published before
  replacing the method, serialized installation and the `NVMTL_NO_GL_VBLANK_FIX` opt-out. The correction is
  gated on the NVIDIA backend; it does not alter `CAMetalLayer` or Vulkan swapchain methods.
- Cursor movement is coalesced onto a retained/drained framebuffer thread call. Position-only moves leave the
  cursor image surface alone. Allocate the worker with `THREAD_CALL_PRIORITY_KERNEL` and
  `THREAD_CALL_OPTIONS_ONCE`: one invocation can run and one rerun can be pending, using the latest cursor
  state. A default high-priority call permits overlapping workers to accumulate behind the display gate.
  Stop must cancel queued work with exported `thread_call_cancel`, then wait outside display/state locks for the
  outstanding cursor-work count. Register before enqueue, undo duplicate entries, and retire on cancellation or
  callback exit, including early stopping returns. This covers dispatched callbacks before their entry too.
  `thread_call_cancel_wait` is not exported to kexts; do not reintroduce it. Free only after draining. Recheck stopping after taking the display gate;
  freeing a call alone does not wait for a running callback. Keep the shared display gate and cursor-state lock. This is a
  targeted change from upstream's framebuffer; BAR1 allocation, primary scanout and display modes stay upstream's.
- GL completion queries count committed command buffers, including texture owners and buffer-backed textures.
  Reuse the last sealed part's immutable resource snapshot instead of allocating another list. Unknown fence
  completion keeps the snapshot alive and its resources busy, matching the existing in-flight quarantine.
- Heap-route eligible argument-buffer texture arrays and keep texel buffers on texel-view bindings. Skip
  per-draw plan allocation for heap-routed fields, bound other arrays by the shared texture/sampler slot limits,
  and check field-offset arithmetic. Sampler-token fallback is bounded by persistent sampler slots.
- Stage layouts carry one descriptor per binding: preserve the explicit refusal of larger/runtime/specialized
  non-heap descriptor arrays until layout, writes and binding plans support them together. Recognize texel arrays
  of one. Keep set-2 sampled-image heap arrays and ordinary buffer-internal arrays working.
- Preflight shared address-table pointer uses before physical-buffer lowering. Clone logical pointer types for
  supported chains/copies independently of traversal order; refuse helper/control-flow/store/return escapes rather
  than emitting mismatched pointer types. Keep the existing controlled descriptor fallback/refusal behavior.
- Preserve mip layers for arrays/cubes and halve 3D depth at each mip. Retain AIR-derived reflection and required
  function-constant errors while allowing the existing deferred mesh/object specialization.

All prior Tahoe, parked-VRAM, stencil, Chromium, `-O2` and OpenGL/MoltenVK cache fixes remain required.
For Dock captures use the current 2.3.7 completion policy above; badge mip tracking remains local.
Native NVK, NVRM and NVAccel are unchanged by this follow-up. Rebuild the plugin, translator and **NVRMFB**
for a test package; the stock 2.3.5 framebuffer lacks the asynchronous cursor change. The current staging helper
replaces only the framebuffer plist: the rebuilt framebuffer binary must also be included and checksums refreshed.

The three components compile and booted on the RTX 2060 Super / Tahoe 26.7.1. Eight CPU regression
harnesses pass; OpenGL, sampler-state, normalized MoltenVK and native Vulkan readbacks match their
references. The 30-second gameplay capture added no allocation failures, surface timeouts or refused/dropped
flips. The isolated OpenGL comparison measured the same rounded 2.8 ms/frame median as the parent; it
does not establish a Minecraft FPS improvement. Keep the committed-use regression harness and the submission
fixture's bookkeeping declaration. VSync-on pacing, sleep/wake, mode changes and other GPUs remain unqualified.
Earlier results below apply to their original builds. See [OpenGL performance](OPENGL-PERFORMANCE.md)
for the source commits and validation limits.

Two subsequent whole-machine failures on 2026-10-18 panicked with the high-priority thread-call pool at
500 threads, 499 blocked. The cursor worker's default allocation permits that accumulation, but the panic
reports lack the blocked-thread stacks needed to attribute it conclusively. The serialization repair above
was rebuilt, staged and confirmed loaded on RTX 2060 Super / Tahoe 26.7.1 by its matching UUID and
installed/package executable hashes. Extended runtime validation is ongoing; loading successfully and
the earlier short gameplay capture do not establish stability. Similar upstream freeze reports have not
been matched to this panic signature.

### Earlier imports

These ten commits follow `7f3c751`. Preserve their behavior when resolving upstream conflicts;
the benchmark-only commit provides supporting tools.

| Fork commit | Change |
| --- | --- |
| `ee4152f` | Minecraft OpenGL vertex shader translation. |
| `ff0f8d9` | OpenGL draw path, provoking vertex and texture-buffer views. |
| `34fd3e6` | OpenGL per-draw benchmark and path probe. |
| `16378da` | Remove per-draw allocations and retain the blit pipeline cache. |
| `fdbb5e1` | Reuse texel-buffer descriptor sets. |
| `fc13457` | Apply depth bias. |
| `c49edb2` | Honor per-instance vertex step rates. |
| `976d9ae` | Translate samplers and sampler arrays in mixed argument buffers. |
| `b0ba21d` | Harden resource caching, shader selection and render-state handling. |
| `d51bcd1` | Cache unchanged graphics bindings and sampler specialization keys. |

### OpenGL and translator requirements

- Retain the shader selection/dominance checks and zero-offset integer-vector load repair.
- Keep descriptor reuse sensitive to layout, buffer address, dynamic offset and table-page changes.
- Resource caches must keep the actual retained object alive, including when distinct objects compare equal.
- Reset primitive restart, provoking vertex and depth bias for each render encoder. Preserve the
  capability/profile checks and fallback for native last-vertex provoking behavior.
- Preserve supported instance divisors, texture-buffer format views and mixed argument-buffer samplers.

See [OpenGL performance](OPENGL-PERFORMANCE.md) for the selected source commits and test procedure.

### MoltenVK/NVMTL requirements from `d51bcd1`

- Cache graphics bindings per command recording and reset them for each new recording/event segment.
- Rebind when the pipeline, pipeline layout, descriptor sets, heap, set count or dynamic offsets/count change.
  Compute bindings use their separate bind point.
- Cache up to eight recent sampler-state keys per render encoder. Invalidate the active key when
  vertex/fragment sampler values or LOD clamps change, and preserve each pipeline's own variant lookup.
- Preserve resource retention, surface publication, completion waits and specialization refusal handling.

This commit changes NVMTL's submission path used by MoltenVK. It leaves the native NVK driver,
translator, kexts and framebuffer unchanged.

## Recorded results and limits

Hardware: RTX 2060 Super, 8 GiB BAR1, macOS Tahoe 26.7.1, 75 Hz display.

- Earlier OpenGL checks produced matching smooth/flat readback hashes and `GL_NO_ERROR` after installation.
  Short runs fell from roughly 16–21 ms/frame to 2–3 ms/frame. Background activity and the short baseline
  limit that comparison. A user screenshot showed Minecraft OpenGL at 149 FPS at 854×480; this was not a
  matched before/after gameplay test.
- For `d51bcd1`, five alternating runs per build of a normalized-sampler MoltenVK workload measured a median
  **5.3635 → 4.0128 ms/frame**, or **25.18% lower frame time**. The workload rendered 5,000 textured draws
  per frame into a 16×16 offscreen target for 40 measured frames. Readback matched the CPU reference.
  This result does not establish a Minecraft FPS increase or a native NVK performance improvement.
- Before staging, nine GPU state-transition cases and 23 surface publication/wrapper cases passed.
  OpenGL smooth/flat readbacks also matched between builds.
- After installation, Minecraft and WindowServer loaded the new plugin. A repeat 30-second sample during
  windowed play, with a 35-second counter capture, recorded no additional surface timeouts, allocation
  failures, flip refusals or drops. The surface-timeout counter stayed at nine; those earlier boot events
  remain unattributed. No explicit GPU fault/reset/device-lost messages appeared in the matching log interval.
- In that windowed sample, 3,214 of 5,067 render-thread samples (**63.43%**) were in drawable acquisition.
  The refresh-linked presentation ceiling remains unresolved: the earlier game test reached about 150 FPS
  at 75 Hz and 120 FPS at 60 Hz, and direct Metal reproduced the ceiling. Fullscreen switching is not
  required for the drawable wait. Sample percentages are not FPS measurements.
- An unnormalized-coordinate sampler test still fails through MoltenVK/NVMTL on both the older baseline
  and this candidate. Native NVK passes it. It was excluded from performance claims and remains unfixed.

All seven focused harnesses passed after the exact tested source was transferred into the fork:

```sh
python3 tools/test_render_sampler_cache.py
python3 tools/test_graphics_bindings.py
python3 tools/test_opengl_perf.py
python3 tools/test_surface_publication.py
python3 tools/test_submission_copies.py
python3 tools/test_resset_seal.py
python3 tools/test_resset_lifetime.py
```

The full translator suite still needs unpublished fixtures. These checks and one GPU's runtime results
do not establish behavior on every GPU or rule out regressions after a future merge.

Detailed local artifacts are under `out/vulkan-presentation-20261017-192317/`, including the repeat
capture in `windowed-20261017-202439/`. This document retains the summary if those artifacts are cleaned up.
