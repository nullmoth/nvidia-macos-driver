# Provenance of what ships

What each part of the driver package came from, under which licence, and what has to change before release. The rule is
plain: nothing here may come from taking apart a closed binary, and everything that is somebody else's keeps its licence
and credit. `NOTICE` is the short form that ships; the texts are in `licenses/`.

## 1. Written here (PolyForm Noncommercial 1.0.0, `LICENSE`)

NullMoth's own code: the kexts' glue and policy (`kexts/NVRM`, `NVRMFB`, `NVRMAGDC`, `NMIntelFB`, `NMHDA`, `NMDRM`,
`NVPrimarySelector`), the Metal plugin (`plugin/`), the installers (`package/`), the build scripts and the tests. Every one
of these files carries an SPDX line. The 107 files checked on this branch name PolyForm-Noncommercial-1.0.0 and nothing
else; none contains GPL text.

Not copied from the PolyForm-licensed `mickys/nvidia-macos-driver`: `NOTICE` records the three places where an idea came from
that project (EDID override files, the shared VRAM import cache, the browser surface default), each implemented
independently. In this tree the name occurs only in the MIT `mickys/nvenc` import and in the credit lines of `NOTICE`,
`CHANGELOG.md` and `docs/HOW-IT-WORKS.md`; code written from ideas in that project was not compared line by line against its
source, which this audit could not fetch, so the owner's statement that none was copied is what stands.

## 2. Taken from other projects, with their licence

| Part | From | Licence | Where recorded |
|---|---|---|---|
| Kernel driver core (RM, display engine, user-mode interface headers) | NVIDIA open-gpu-kernel-modules 620.70.06 | MIT (GPL-2.0 alternative not used) | `NOTICE`, `licenses/NVIDIA-open-gpu-kernel-modules-COPYING.txt` |
| Display-engine 3D shaders `kexts/NVRM/nvkms_shaders.cpp` | `src/nvidia-modeset/src/shaders/g_*_shaders` of the same tree, xz-compressed unchanged | MIT | file header (was labelled as NullMoth's: corrected) |
| `nvidia-modeset-os-interface.h` (two copies) | the same tree | MIT, NVIDIA copyright | file header (SPDX said PolyForm: corrected) |
| Vulkan driver NVK and the NAK compiler | Mesa commit `90a959eb` (26.3.0-rc3) plus `nvk/*.patch` | MIT | `NOTICE`, `licenses/Mesa-license.rst` |
| Rust crates NAK builds from (proc-macro2, quote, syn, paste and similar, fetched by Mesa's meson wraps) | crates.io | MIT and/or Apache-2.0 | stays with Mesa's tree; see item P8 |
| Shader translator | `steelbrain/metal2vulkan` (Anees Iqbal) | LGPL-3.0-or-later | `NOTICE`, `translator/LICENSE`, `translator/COPYING` |
| The Rust crates linked into the translator | crates.io | MIT, Apache-2.0, Unlicense or MIT, Unicode-3.0 | `licenses/translator-rust-crates.txt` (new) |
| GSP firmware | NVIDIA 620.70.06 driver package, unmodified | NVIDIA "License For Customer Use of NVIDIA Software" | `licenses/NVIDIA-firmware-LICENCE.txt`; fetched on the user's Mac, see P1 (resolved) |
| Intel display firmware (optional, hybrid laptops) | Intel via linux-firmware at commit a42217fc, unmodified, fetched on the user's Mac by `package/dmc_fetch.sh` | Intel i915 firmware licence (binary redistribution unmodified; no reverse engineering) | `licenses/Intel-i915-firmware-LICENSE.txt` |
| Video encoders and decoders `vendor/nvenc` | `mickys/nvenc` 0.9.5, commit `1e91845` | MIT | `vendor/nvenc/SOURCE`, `licenses/mickys-nvenc-LICENSE.txt` |
| Headers `libnvencvt` is built against (fetched, not shipped) | NVIDIA open-gpu-kernel-modules 610.57.04, open-gpu-doc `9fdf5c4` (classes/video), WebKit `CMBaseObjectSPI.h` and `VTVideoDecoderSPI.h` | MIT, MIT, BSD | `vendor/nvenc/SOURCE` |
| Firmware-framebuffer mapping and checks `NMGopFB` | `PrimeBridge` (myvoow), `BootFramebuffer.cpp` method | MIT | `NOTICE`, `licenses/PrimeBridge-LICENSE.txt` |
| Hybrid-laptop resume, Type-C flow and C20 tables `NMIntelFB` | Linux kernel i915/xe display driver (Intel Corporation), ported sequences and copied PLL tables | MIT | `NOTICE`, `licenses/Linux-i915-display-LICENSE.txt` |
| Panel brightness method `NMIntelFB` | `WhateverXe` (b00t0x), idea and register sequence; register layout from Linux i915/xe headers | BSD-3-Clause, MIT | `NOTICE`, `licenses/WhateverXe-LICENSE.txt` |
| HD Audio (NMXAudio, separate product) | FreeBSD `snd_hda` | BSD-2-Clause | `NOTICE`, `licenses/FreeBSD-snd_hda-LICENSE.txt` |
| Zstandard decoder in `dlss/zstd`, used by `nm_unzstd` | facebook/zstd 1.5.6 lib/common and lib/decompress, unmodified | BSD-3-Clause (the GPL alternative is not used) | `NOTICE`, `licenses/zstd-LICENSE.txt` (new) |
| BLAKE3 in `dlss/blake3`, used by `dlss_cache` | BLAKE3 1.8.2 as vendored by Mesa | CC0-1.0 (Apache-2.0 alternative not used) | `NOTICE`, `licenses/BLAKE3-LICENSE-CC0.txt` (new) |
| `air-runtime` in the plugin bundle | LLVM, Zstandard | Apache-2.0 with LLVM exception, BSD-3-Clause | `NOTICE` |
| `nvmtl-air-dis` in the plugin bundle | NullMoth (`plugin/air_dis.c`), uses `air-runtime` | NullMoth | — |

Downloaded on the user's Mac, never shipped: NVIDIA's DLSS runtimes (SDK 310.9.16, from NVIDIA's GitHub), the NGX core
(one file from NVIDIA's Linux 620.70.06 package) and `libnvJitLink` (from NVIDIA's PyPI wheel). `package/dlss_fetch.sh` pins
each by SHA-256, asks before downloading, and installs them root-owned. They stay under NVIDIA's licences.

No closed NVIDIA library is in the package. `libnvencvt` talks to the card through the RM interface of the open
modules and the open-gpu-doc class headers.

Carried in older packages and removed in 2.9. The release build starts each package from the previous one and replaces
what it rebuilds, so a file nothing rebuilds rides along; an audit of the source tree alone does not see it. Two such
files were not ours to ship:

* `Library/GPUBundles/nvmtl/air-opt`, Apple's Metal toolchain program (2.0.3 to 2.8.1). Replaced by `nvmtl-air-dis`.
* `Library/GPUBundles/NVIDIAShared.bundle` (to 2.8.1): `libnvidia-ptxjitcompiler.so.610.57.04` and
  `libnvidia-nvvm.nocanary.so`, NVIDIA's closed compiler libraries from its Linux driver, the second one altered, with
  the host program that loaded them. The plugin reached them only with the developer switch `NVMTL_VENDOR_COMPILER`.
  2.9 does not carry the bundle; `install.sh` removes an installed copy; `check_package.sh` refuses the bundle and any
  ELF file anywhere in the tree.

Carried and still shipped, built outside this tree: NullMoth's own hardware video libraries in the plugin bundle
(`libnvdec_h264.dylib`, `libnvdec_h264_t2d.dylib`, `libnvdec_hevc.dylib`, `libnvenc_h264*.dylib`, `libnvenc_hevc*.dylib`),
the Vulkan loader `nvmtl/libvulkan.1.4.362.dylib` (Khronos, Apache-2.0) and `nullmoth-runtime-check`. Their sources are
not in this repository yet, so a 2.9 package does not rebuild them. Section 2a says which card needs which of the nine
video libraries and what `libnvencvt` would have to learn before each can go.

## 2a. Hardware video: who serves what, by generation

Two providers register with VideoToolbox: NullMoth's own libraries (carried binaries, rating 1000) and `libnvencvt`
(`vendor/nvenc`, built from this tree, rating 1100). "auto" is what a user gets with nothing set; "opt-in" needs
`NVMTL_NVENCVT=1` (or a list of process names) in `nvrm610.conf`. A dash is a codec nobody serves in hardware.

| Generation (NVENC / NVDEC class) | H.264 decode | HEVC decode | VP9, AV1 decode | MPEG-1/2/4 decode | H.264 / HEVC encode | AV1 encode |
|---|---|---|---|---|---|---|
| Turing GTX 16, TU117 (b4b7 / c4b0) | own `_t2d`; nvencvt opt-in | own; nvencvt opt-in | VP9 nvencvt opt-in; no AV1 in hardware | nvencvt opt-in | own `_tu10x`; nvencvt opt-in | none in hardware |
| Turing TU116, TU10x: GTX 1660, RTX 20 (c4b7 / c4b0) | own `_t2d`; nvencvt opt-in (new) | own; nvencvt opt-in (new) | VP9 nvencvt opt-in (new) | nvencvt opt-in (new) | own `_tu10x` only | none in hardware |
| Ampere GA10x (c7b7 / c7b0) | own `_t2d`; nvencvt with `NVENCVT_AMPERE_ALL_DEC=1` | own; nvencvt with the same switch | nvencvt auto | nvencvt with the same switch | own `_ga10x`; nvencvt with `NVENCVT_AMPERE_ENC=1`, untried | none in hardware |
| Ada (c9b7 / c9b0) | nvencvt auto (own `_t2d` registers below it) | nvencvt auto (own below it) | nvencvt auto | nvencvt auto | nvencvt auto (own: none) | nvencvt auto |
| Blackwell (cfb7 / cfb0) | own `libnvdec_h264`; nvencvt opt-in (new, not yet run on a card) | own only | - | - | own `libnvenc_h264`, `libnvenc_hevc` only | - |

What each carried library is still needed for, and what is missing before `libnvencvt` can replace it:

* `libnvdec_h264.dylib`: Blackwell H.264 decode. Replaced once the new opt-in path has decoded correctly on an RTX 50
  card (the classes, the plane layout and the picture setup are public; no public header lists class 0xcfb0's methods,
  so whether Ada's method list is enough is a question for the card).
* `libnvdec_h264_t2d.dylib`: H.264 decode on Turing and Ampere by default. `libnvencvt` decodes there only by switch:
  upstream ran its H.264 decoder on TU117 and Ada, not on GA10x.
* `libnvdec_hevc.dylib`: HEVC decode on Turing, Ampere and Blackwell. Blackwell is the hard one: `libnvencvt`'s HEVC
  decoder is written to Ada's method list (open-gpu-doc `clc9b0.h`), and open-gpu-doc has no header for 0xcfb0.
* `libnvenc_h264.dylib`, `libnvenc_hevc.dylib`: Blackwell encode. `nvenc_drv.h` lists no picture-setup magic for
  0xcfb7 and upstream has no setup for it.
* `libnvenc_h264_ga10x.dylib`, `libnvenc_hevc_ga10x.dylib`: Ampere encode (upstream's setups are Ada's and TU117's).
* `libnvenc_h264_tu10x.dylib`, `libnvenc_hevc_tu10x.dylib`: encode on TU10x and TU116 (class 0xc4b7; upstream has
  0xb4b7 only). TU117 alone could move to `libnvencvt`.

So none of the nine can leave the package yet, and no default changed in this pass. Two facts for the owner's decision
on "our own libraries or `libnvencvt`":

1. `libnvencvt`'s encoders are not free of NVIDIA's closed driver either. Their per-picture control blocks
   (`vendor/nvenc/src/h264enctmpl.h`, `hevcenctmpl.h`, `av1enctmpl.h`) are byte tables upstream generated from traces
   of NVIDIA's Linux driver encoding on the same cards, and `enc.c` cites such traces for buffer sizes. Its licence is
   MIT and the method is observation of output, not disassembly, but it is the same kind of source as our own encoder
   libraries' templates. Its decoders are written to `nvdec_drv.h` and are the cleaner half.
2. The route with no observed bytes at all is to write the encoder control blocks field by field from `nvenc_drv.h`
   (MIT; it documents every field of the motion-estimation, mode-decision and rate-control structures) with values
   chosen and tuned here. That is new work that needs each generation's card to verify; it was not attempted blind.

The vendored library's own changes are listed in `vendor/nvenc/SOURCE`.

## 3. GPL and PolyForm in the dependency list

* The kexts link NVIDIA's open modules under their MIT licence. The GPL alternative of that dual licence is not taken, and
  no file in this tree is GPL.
* The translator is LGPL-3.0-or-later and is a separate dynamic library the plugin loads; its source and licence ship with
  the repository. Its crates are permissive.
* NMX (separate repository): the NPU kext is GPL-2.0 by design and is its own product; xe, i915 and amdgpu are MIT; LinuxKPI
  and the FreeBSD layer are BSD. `xkpi/all-xe.sh` stops the build if a GPL file is left in the xe sources, and
  `tools/test_licence_audit.py` audits the licences; the build manifest records exact upstream commits so the GPL parts can
  be offered in source.
* `mickys/nvidia-macos-driver` (PolyForm Noncommercial) is not a source of any code here; see section 1.

## 4. Quarantined branch

`hwrt-28` holds four commits (hardware ray query, `--rtcore`, the `ttu` files) that were quarantined as vendor-derived. None
of them is an ancestor of `rel-29` or of this branch, none of their files exists in the tree (`nvk/hwrt/`, `ttu*`,
`dlss_fetch.sh --rtcore`), and the strings they introduced (`NVK_HWRT_QUERY`, `rtcore`, `libnvidia-rtcore`) occur nowhere.
The ray-query code in `nvk/nvk-macos.patch` is Mesa's own `nir_lower_ray_queries`. 2.9's hardware ray tracing is the
translator's own BVH path.

## 5. Items that must change or be decided

Severity is the legal exposure, not a code defect. Each names the evidence and the action.

> Status (2.9): resolved. The package no longer carries the firmware. `package/fw_fetch.sh` takes the four files from NVIDIA's own 620.70.06 Linux driver package (the .run that `dlss_fetch.sh` already pins), checks each against a SHA-256, and `install.sh` runs it before it changes anything; an earlier install that still matches is reused, and with no network and no earlier install the installer stops with the download address and changes nothing. Test: `tools/test_fw_fetch.py`.

**P1. GSP firmware redistribution (decision for the owner, highest).** `licenses/NVIDIA-firmware-LICENCE.txt` is the licence
`linux-firmware` ships these files under. Its only redistribution grant (section 2.1.2) is "solely for use on operating
systems distributed under the terms of an OSI-approved open source license", binary unmodified, with the licence copied.
macOS is not such an operating system, so the package's right to carry the firmware is doubtful, and `NOTICE` says "the same
terms under which linux-firmware redistributes" without noting that limit. Options: (a) fetch the firmware on the user's Mac
at install time from NVIDIA's package or `linux-firmware`, pinned by SHA-256, exactly as `dlss_fetch.sh` does for DLSS (the
1401 app already downloads other pinned files while it builds an EFI; a Mac installed offline would need the stick to carry
a download step); (b) ask NVIDIA for permission in writing; (c) keep shipping and accept the risk. Not changed here because it
reshapes the installer and the 1401 flow.

> Status (2.9): resolved. The relocation kinds and the instruction bits they patch are LLVM's public CUDA ELF definitions; the stub words come from NAK's own encoder source at patch time (`nvk/dlss/legal/nak_stub.py`); the trap is a NAK-encoded store to address 0 followed by EXIT. Sources, commands and tests: `nvk/dlss/legal/README.md`. Following the public definition also fixed the placement of kind 75 and added kind 58, which the older placement refused.

**P2. DLSS kernel-cache patcher cites `nvdisasm` output of NVIDIA's shipped DLSS kernels.** `nvk/dlss/patch_dlss_cache.py`
lines 243 and 425 state that the relocation kinds (56, 57, 75), the `CALL.ABS` operand layout and the two 128-bit words of the
`BPT.TRAP` / `EXIT` stub were measured by disassembling NVIDIA's cubins. The DLSS SDK licence and the CUDA EULA restrict
disassembly. What can be rebuilt from clean sources: the `EXIT` word's opcode (`0x94d`) is NAK's own (`sm70_encode.rs`,
`impl SM70Op for OpExit`; NAK is MIT and already in the build); NAK has no encoder for `BPT.TRAP`, so the trap can be replaced
by a NAK-encoded store to an unmapped address (a clean channel fault, which is what the stub is for); ELF relocation numbers
are in NVIDIA's public CUDA binary documentation. The `CALL.ABS` address-field layout is the part with no public source.
Action: generate the stub words from NAK at build time with a host test comparing them, and either find a public source for
the call-target layout or drop relocation handling for cubins that need it (the DLSS 4 transformer layers).

> Status (2.9): resolved. The comment now cites cubins compiled from our own PTX (`.nv.constant0` sizes, `nvk/dlss/legal/cubin_relocs.py`) and tinygrad's launcher (MIT).

**P3. `nvk-macos.patch` states cbuf0 offsets "measured by disassembling cubins" (line 13636, `nvk_cubin` launch).** The nvcc
constant-buffer layout (`0x358`, `0x37c`, `0x380`) is the same kind of fact as P2. Same decision. It is cross-checked against a
cubin NullMoth's own compiler produced, which is the clean source to cite instead.

> Status (2.9): resolved. The comment is a black-box description; `docs/RT-WORKING-SET-GATE.md` is the procedure to re-measure the figure. The knob stays.

**P4. `plugin/NVMTLDevice.m` line 735: `NVMTL_WS_RT_GATE_MB`** is documented as measured "in the disassembly of Cyberpunk 2077's
device set-up" (a game binary, not NVIDIA's; the game's licence forbids it). The knob is default off and holds one number
(the working-set figure the game compares against). Options: remove the knob and its comment; or keep the number and say only
that the game enables ray tracing above that working set, found by changing the reported figure and watching the result
(a black-box measurement). Not changed without a decision, because the Cyberpunk ray-tracing path depends on it.

**P5. Apple class layouts in `kexts/NVRM/accel/re`, `accel/iofam`, `agdc/AppleGraphicsDeviceControl_re.h`.** Class sizes come
from the kernel's own OSMetaClass registry at run time; method names, parameter lists and vtable order come from the symbol
tables and vtables of Apple's kernel collection (`vtable.py`, noted in `IOAccelerator.h`). No Apple code is copied, but
reading Apple's binaries is outside what Apple's licence allows apart from interoperability rights. These layouts are what
lets a third-party accelerator be the class Apple's frameworks expect, so the practical choice is to keep them as
interoperability data and not publish more than the signatures. Listed so the owner knows.

**P6. The DLSS host runs NVIDIA's Linux libraries on macOS** (`plugin/dlss/elfhost.c`, `lnxlibc.c`, `nvmtl_dlss.c`). The ELF
loader is written from the ELF and System V ABI specifications; the libc shim, the NVML and Vulkan stand-ins and the facts in
`nvmtl_dlss.c` about what the NGX core asks for (for example that it opens its snippets through `/proc/self/fd/N`, and that it
refuses a ray-reconstruction feature whose denoise mode is Off) were learned by running the libraries and logging their calls
(`NVMTL_DLSS_TRACE`), not from their code. The libraries are the user's own pinned downloads. The licence of the NGX core
(NVIDIA's Linux driver licence) also says the software is for NVIDIA processors and must not be translated; running it as
is on an NVIDIA GPU under another operating system is the grey part. No action in code; recorded.

**P7. Stale notices (fixed here).** `NOTICE`, `README.md` and `docs/HOW-IT-WORKS.md` named NVIDIA 615.78.08 as the kernel and
firmware source; the kexts and the firmware directory are 620.70.06. The two NVIDIA header/shader files carried the wrong SPDX
label (PolyForm instead of MIT). The translator's crate licences and the Zstandard and BLAKE3 texts were missing from
`licenses/`.

> Status (2.9): resolved. `build/build_nvk.sh` writes the licence texts of the fetched crates (`build/gen_nvk_rust_crates.py`), stops if `licenses/nvk-rust-crates.txt` differs, and the file ships in `licenses/`.

**P8. Mesa's Rust crates.** NVK's binary contains NAK, which builds from crates fetched by Mesa's `subprojects/*.wrap`
(proc-macro2, quote, syn, unicode-ident, paste and others, all MIT and/or Apache-2.0). Their copyright and licence texts must
ship with the binary the way P7's translator list now does. Not generated here because the crate sources are fetched during the
Mesa build; `build/build_nvk.sh` is the place to write `licenses/nvk-rust-crates.txt` from `subprojects/packagecache`.

> Status (2.9): resolved. The fixture is compiled from `fixtures/shadow_pcf/shadow_pcf_gather_compare.metal`, written for the test; the game-derived file is removed.

**P9. Test fixture reduced from a game shader.** `translator/translator/tests/fixtures/civ6/gather_compare_depth_2d_offsets.ll`
is a 44-line reduction of one fragment shader of a commercial game (a four-tap shadow filter). It holds a standard technique
and generic names. Low exposure; rewrite it from a Metal source written for the purpose if the owner wants no game-derived
bytes in the repository (done).

## 6. Reviewed, nothing to change

* `kexts/NVRM/accel/re/*.h` do not claim disassembly: they say sizes came from the registry (see P5).
* `translator/` mentions of "disassemble" are the SPIR-V text printer in its own tests.
* `vendor/nvenc` is MIT with credit; the build fetches only MIT/BSD headers.
* The firmware, the DLSS runtimes and the NGX core are never modified by the package.
* `dlss/zstd` and `dlss/blake3` are the upstream files without edits beyond those listed in their READMEs.
