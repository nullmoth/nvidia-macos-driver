# Source and build notes

This is a source reference for the measured process-local OpenCL configuration.
It does not install a driver or enable the profile for other applications.
`sources.json` records source revisions, the release patch order, input/output
hashes for all changed files, and the identities of the measured binaries.
Rebuilding with another toolchain requires fresh validation; the recorded
binary hashes are evidence identifiers, not a promise of reproducible signing.

## Source changes

- `patches/clvk-private-opencl.patch` is the complete final diff against clvk
  `863ce0ce5150478d4b4c679a869d63f60436a8b4`. It retains the default-off
  diagnostics compiled into the measured framework. It contains device-local
  buffer selection with the normal fallback, coherent cached staging, one
  leased staging buffer per external queue capped at 128 MiB, staged writes
  from 1 MiB, event/queue lifetime fixes, and minimum-deviation calibration.
  The staging lease covers submission, completion, CPU copying and destruction.
- `patches/mesa-queue-barrier-ownership.patch` makes the queue-finish owner
  destroy its barrier after every worker fence, instead of letting workers
  destroy it based on the Apple barrier's return value.
- `patches/nvk-selective-spill-retry.patch` adds default-off
  `NVMTL_NAK_SPILL_RETRY=1`. Compute shaders that actually spill to memory get a
  second compile without the load-sink pass. The retry wins only with strictly
  fewer memory spills/fills, no register spill/fill increase and no occupancy
  regression. This is a general compiler decision: no benchmark or kernel name
  is matched. The NIR snapshot has independent ownership. Cache flag bit 16
  distinguishes the policy. The original six-argument `nak_compile_shader`
  export is preserved; the retry has one new, separate C entrypoint.

The clspv compiler, LLVM, OpenCL-Headers, SPIRV-Headers and SPIRV-Tools checkouts
are unmodified. There is no additional clspv patch. Compiler flags in the final
profile are exactly `-hack-clamp-width`; the earlier conversion and FP32
workarounds are not part of it. Early submission uses existing clvk settings,
not an additional command-queue source patch.

## Prepare the pinned sources

Fetch the exact repositories/commits in `clvk_sources` from `sources.json`,
placing dependencies at their recorded paths relative to the clvk workspace.
Use a new checkout/build directory. Apply the clvk patch at the clvk root.

The two Mesa patches do **not** target vanilla Mesa. Start with Mesa
`90a959ebba9dbb36f2ea5176091c15d6ebdd0af0`, and follow the source preparation in
the repository's `build/build_nvk.sh` for release `3.0`:

1. Apply the existing `nvk/` patches in `mesa_release_patch_order` from
   `sources.json`. The manifest also pins the preparation inputs by hash.
2. Copy the hardware-ray-tracing/curve helper headers and shader sources as
   that script does, including the `curve_nir.h` include rewrite.
3. Prepare the NVIDIA open kernel module **610.57.04 headers** with
   `nvk/ogkm-nvos-620.patch` and `nvk/ogkm-warnings.patch`, and set the Mesa
   `nvkmd/nvrm/open-gpu-kernel-modules` link to that private copy. This does not
   build or install kernel modules.
4. Run `nvk/dlss/patch_dlss_cache.py` on that Mesa tree.
5. Apply the queue-ownership patch, then the selective-spill-retry patch.

Keep the original post-step-4 tree for the CPU replay check below. The existing
`NVK_EXTRA_PATCHES` hook runs before the DLSS script; it is not the recorded
application order for this bundle.

## CPU source replay and regression checks

Given a clean pinned clvk tree and the prepared Mesa tree **before step 5**:

```sh
python3 check_patches.py --clvk-tree "$CLVK" --mesa-tree "$MESA" --cpu-tests
```

The checker hashes every touched input, copies only those files into a temporary
directory, applies the patches, and checks every resulting file against the
measured sources. It does not modify either input tree. It rejects changed
patches and mismatched input sources.

The optional CPU checks require Xcode command-line tools. They reuse the
previously tested production-function extraction harnesses:

- Calibration: real function bodies with Vulkan timestamp mocks; verifies
  sample pairing, minimum selection, ties, maximum values, host-only queries,
  errors and unchanged mapping semantics. A broken selector must fail.
- Selective retry: 26 allocation/compiler-stub cases plus nine intentionally
  broken variants, with ASan/UBSan. It checks ownership, losing-bin cleanup,
  chosen NIR constant data, overflow-safe sums, cache keys and the existing ABI.
- Queue shutdown: actual finish, callback, Apple barrier and fence functions
  using real pthreads; 600 finish calls, multiple worker/caller counts and
  deterministically delayed waiters. The old ownership is a negative control.

These bounded CPU adapters do not replace actual NIR/Rust compilation, OpenCL
conformance or GPU execution. The previously completed GPU validation is
described separately in `RESULTS.md`.

## Build configuration

The measured x86_64 macOS build used Apple clang 21.0.0, CMake 3.31.10, Ninja
1.13.2, Meson 1.9.1, Rust 1.90.0 and LLVM 24 snapshot `71a85ef`. The release NVK
was built with LLVM 22/Rust 1.97.1; this is a material difference. Pinned
SPIRV-LLVM-Translator `d916d6b3` and its SPIRV-Headers `575b6512` match the private
LLVM snapshot. `sources.json` also records glslang, bindgen, cbindgen and the six
Mesa Rust crates. The Rust license inventory must still equal the release's
`licenses/nvk-rust-crates.txt`.

For clvk, include `framework-abi.cmake` with
`CMAKE_PROJECT_clvk_INCLUDE`. It sets the Mach-O framework compatibility version
to 1.0.0 for applications linked against Apple's framework; it does not change
OpenCL platform/version reporting. Set `Vulkan_INCLUDE_DIRS` to the prepared
Mesa headers and `Vulkan_LIBRARIES` to the installed NullMoth Vulkan loader.
The measured configuration used:

```text
-G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=x86_64
-DCLVK_VULKAN_IMPLEMENTATION=custom
-DCLVK_COMPILER_AVAILABLE=ON -DCLVK_CLSPV_ONLINE_COMPILER=OFF
-DCLVK_ENABLE_SPIRV_IL=OFF -DCLVK_BUILD_TESTS=ON
-DCLVK_BUILD_STATIC_TESTS=OFF -DCLVK_BUILD_CONFORMANCE_TESTS=OFF
-DCLSPV_BUILD_TESTS=OFF -DENABLE_CLSPV_OPT=OFF
-DLLVM_TARGETS_TO_BUILD=X86 -DLLVM_BUILD_TESTS=OFF
-DLLVM_INCLUDE_TESTS=ON -DLLVM_INCLUDE_EXAMPLES=OFF
-DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_DOCS=OFF
-DCLANG_INCLUDE_TESTS=OFF -DCLANG_ENABLE_CIR=OFF
-DCLANG_ENABLE_STATIC_ANALYZER=OFF -DCLANG_ENABLE_OBJC_REWRITER=OFF
-DLLVM_PARALLEL_COMPILE_JOBS= -DLLVM_PARALLEL_LINK_JOBS=
-DCMAKE_JOB_POOLS=clvk_link=1 -DCMAKE_JOB_POOL_LINK=clvk_link
-DLLVM_EXTERNAL_PROJECT_BUILD_TOOL_ARGS=-j1
```

Build targets: `OpenCL`, `clspv`, `spirv-val`, `simple_test`, `api_tests`; eight
compile jobs and one linker were used. Locally ad-hoc sign and strictly verify
the completed framework before testing it.

For NVK, reuse the host-tool and cross-build stages of `build/build_nvk.sh`
against the prepared tree; do not rerun its destructive source-reset stage on
the patched tree. Build `mesa_clc` and `vtn_bindgen2` from that **same Mesa tree**.
The private host-tool configuration additionally used
`-Dshared-llvm=disabled -Dcpp_rtti=false`, with the pinned LLVM/translator and
private Python 3.11.18. The NVK cross-build otherwise used the repository's
release-3.0 flags, x86_64 target, macOS 15.0 deployment floor, eight jobs, zero
compiler/linker warnings and no unresolved strong dynamic symbols. Strip debug
symbols and ad-hoc sign the private library. Export differences from the release
must be reviewed; they are not an instruction to relax a global driver ABI gate.

## Exact measured process profile

```text
CLVK_KEEP_TEMPORARIES=1
CLVK_CLSPV_OPTIONS=-hack-clamp-width
CLVK_PREFER_DEVICE_LOCAL_BUFFERS=1
CLVK_KEEP_MEMORY_ALLOCATIONS_MAPPED=1
CLVK_MAX_COMPUTE_UNITS=170
CLVK_MAX_FIRST_CMD_BATCH_SIZE=10
CLVK_MAX_CMD_GROUP_SIZE=1
NVMTL_NAK_SPILL_RETRY=1
NVMTL_PROCESS_ENV=1
NVMTL_NAK_ARR_SCRATCH=0
NVRM_610_BOOST=hold
```

170 is the queried physical SM count of the tested RTX 5090; it is not a
portable value for other cards. The framework, compiler path, private NVK ICD,
cache and temporary directories must be process-local and the cache identity
must include all three binary hashes and these settings. Diagnostic tracing is
off for scored runs. `CLVK_CLSPV_PATH` is consumed by clvk as a shell command
prefix: the validated launcher shell-quotes that path, including spaces in
`Application Support`. Do not concatenate an unquoted path into that setting.

The installed Metal driver strips private environment flags unless its existing
`NVMTL_PROCESS_ENV=1` opt-in is set in a normal-user process. The measured launch
preserved the explicit profile and cleared inherited backend overrides before
opting in. `NVMTL_NAK_ARR_SCRATCH=0` and `NVRM_610_BOOST=hold` matched that
machine's existing configuration/profile; hold is not an overclock.

The private NVK is for clvk's Vulkan API use only. Do not replace the system
NVK library: the installed Metal plugin also depends on a guarded internal
layout/LC_UUID contract which this private build does not satisfy. Normal
Finder launches and the global OpenCL/Metal drivers are outside this bundle.
