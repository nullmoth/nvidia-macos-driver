# Upstream 3.0 integration

The fork merges upstream `87e524ab36f12d17c04626d6cf7fc3bde1cc63e4`
from common ancestor `7412cdde81447f78da0ebb64216dc258d8beaedb`, retaining
the fork's supported macOS 15/26 implementation. Legacy macOS kits are excluded
at the user's request. This source integration does not lift the existing
full-driver build and release hold.

## Included changes and repairs

| Area | Integrated behavior |
| --- | --- |
| NVK streams | Command/upload chunks use uncached GPU access. Apply the narrow upstream repair last in the matched `2.8` patch chain. |
| Metal device creation | Resolve an old-SDK `10.16` compatibility answer using Darwin's version; safely retain a refused, uninitialized device instead of releasing it through initialized-device teardown. The supported OS range remains unchanged. |
| Surface publication | Mark each IOSurface plane once per command-buffer commit, including failed marks. Use the complete 32-bit surface ID and plane in the key; preserve the fork's linear-image exclusion and per-texture completion handling. |
| Packed YUV422 | Enable the existing constrained texture implementation by default, with `NVMTL_YUV422=0` as an opt-out. Unsupported writes, views and render-target use remain refused. |
| Multisample uploads | Refuse invalid buffer uploads into multisample images, including the fork's 3D copy entrypoint. Valid multisample rendering/readback remains supported. |
| Utilization sampling | A firmware query taking more than 500 ms disables subsequent utilization queries for that boot. Keep power-management accounting and sampler locking; fast failures may retry. |
| Scanout allocation | Reserve at least a 3840×2160 scanout, with mode-list and boot-size allocation fallbacks. Compare a late monitor's requirements with the bytes actually allocated. Reject arithmetic overflow. |
| Hot-plug notification | Repay a connect interrupt that became due before its handler registered. Recheck registration after publishing the debt to close the registration race. |
| Intel-panel startup | Drain workers and unwind published state on a failed start as well as stop. Reject a failed surface mapping before publishing callbacks. Keep the fork's controller grouping, backlight handling and timeout quarantine. |
| Virtual/firmware panels | Set dependent properties before superclass startup; map virtual scanout before registration and release failed mappings. NMGopFB remains outside the normal install package. |
| WhateverGreen setup | Add `gfxrst=4` only when WhateverGreen is loaded and no user-selected `gfxrst` exists. Publish ownership after a successful write, restore prior arguments if record publication fails, and remove owned arguments only after driver removal commits. Failed NVRAM reads/writes leave existing arguments intact. |

The upstream AIR grid-scan cache and HiDPI ownership fixes duplicate stronger
fork implementations. Retain bounded immutable AIR analysis caches and display
object identity, including externally replaced overrides and balanced reference
ownership. Do not add a second cache or substitute vendor/product matching for
the live display object.

## Retained boundaries

- Keep stable Mesa **26.3.0** at
  `0a6fa60abb737d26ac8a316a4aad7c9ddf64f9e4`, the NVIDIA **620.70.06**
  mapping/kernel interface, and default/release patch-chain selection **2.8**.
  Upstream 2.9/3.0 token, hub, power and broader display changes remain deferred.
  The later [optional notebook TGP manager](NOTEBOOK-TGP.md) is a separate narrow
  backport; it does not require the deferred NVK power chain.
- Keep the existing framebuffer head/cursor lifetimes, scale ownership,
  shutdown paths and Intel-panel controller architecture. The new upstream
  derived-refresh validation and backlight keeper depend on the deferred display
  implementation and are excluded from this integration.
- Preserve the native Zink/CGL and Minecraft GPU-copy paths, the separate Wine
  provider, matched DXVK contract, MSync, resource reuse, bounded asynchronous
  Wine presentation and synchronous fallback. Their frontend sources are unchanged.
- Preserve the translator's Safari texture/sampler/multisample repairs, all-app
  ANGLE Metal default, bounded caches, Photo Booth surface publication, native
  Zink HDR transfers, and the Safari frame-conversion optimization.
- Preserve scoped codec registration and HEVC parser/arithmetic repairs. The
  legacy codec availability changes are excluded together with the legacy
  accelerator layouts, compatibility headers, GL stack and package scripts.
- Keep optional audio ownership and removal rules. Do not remove independently
  installed audio or the entire vendor namespace during uninstall.

## Safari WebGL helper EFI detection

Automatic NVWebKitFixup installation reads EFI partition types from
`diskutil list -plist`, checks every candidate through the existing nonmutating
OpenCore/Lilu preflight and selects only a unique compatible candidate. Multiple
candidates require an interactive choice or an explicit target; no compatible
candidate leaves manual setup available. An explicit target bypasses discovery
and still receives all preflight checks.

Manual remains the default. Keep signature/architecture verification, Lilu
version and kernel-range checks, vault refusal, transaction locking, guarded
backups/restore and unchanged-install detection. Restore requires the original
explicit target. A partition mounted by the helper is unmounted afterward;
existing mounts remain mounted. The helper stays out of the driver's kernel
collections and `/Library/Extensions`. See [setup and recovery](SAFARI-WEBGL.md).

## Validation and limits

Validation was performed on Intel macOS Tahoe 26.7.1 with an RTX 2060 SUPER.
Private userland probes selected the candidate plugin and renderer explicitly;
no new driver components were installed or loaded into the running kernel.

| Check | Result |
| --- | --- |
| Focused host regression suite | 30 entrypoints passed, plus the production per-display scale-ownership fixture. This includes 26 EFI cases, five installer-choice cases, and controlled-clock utilization failure tests with a negative control. |
| Historical broad discovery | Baseline: 220 passed, 79 failed, one skipped, 11 not run. Integration snapshot: 231 passed, 76 failed, one skipped, 11 not run; no new failures. The subsequently added utilization fixture passes separately. |
| NVK and native compiler tools | Built from matching patched Mesa sources; complete patch chain applied without fuzz. Renderer build had zero warnings, no unresolved strong symbols and only system-library dependencies. |
| Metal plugin | Release and diagnostic builds passed; one existing SDK deprecation warning in the release build. |
| Kernel components | NVRMFB and NMGopFB passed zero-warning build gates. Real/virtual NMIntelFB built with 37/38 existing SDK warnings. NVRM's changed translation unit compiled against matching patched 620 headers with 38 existing warnings. |
| Isolated kernel collection | Linked and inspected all six staged kext identities using system kernel collections as read-only inputs. The link reused the recorded installed NVRM, NVAccel and NVRMAGDC as ABI dependencies. |
| Private Metal GPU probes | Passed WebGPU texture/uniform and multisample readbacks, both argument encoders, half-float mip generation, unknown-usage textures and A8 framebuffer fetch. Corrected image-isolation runs passed 32 Core Image frames and 120 render-feedback checks. |
| Native Zink runtime | All 43 cases passed with the candidate NVK renderer and the unchanged verified stable frontend: CGL/GLD, IOSurface/OBS interop, resize, worker, multiple-window and presentation fallback cases. |
| Installer checks | Production shell failure fixtures and syntax checks passed. Real partition metadata was read without mounting EFI; all EFI mutations were confined to fixtures. |

The 76 historical failures include fixtures for deferred implementations and
probes needing explicit runtime arguments. Broad discovery is not an all-green
qualification claim; the focused workflow lists the applicable integration
checks. Three previously failing installation/layout fixtures were repaired
without weakening their negative cases.

An initial Core Image probe loaded two plugin images because of harness staging;
the corrected runs verified one candidate plugin and the new renderer. This
does not establish Safari application or WindowServer qualification.

The changed NVRM object has **not** been linked into a complete new RM kext;
the isolated collection check used the recorded installed RM. No full-driver
archive, release draft, installation, new boot, Sequoia qualification, laptop
panel qualification or other-GPU application testing was performed. Both
accelerator source variants and their ABI headers remain unchanged. Full RM
linking, complete packaging and real boot/application tests remain necessary
before releasing this integration.
