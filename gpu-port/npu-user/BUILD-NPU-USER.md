# NPU userland for macOS x86_64

What installs at `/Library/NullMoth/npu`, and how each piece is built. Everything is built on an Apple-silicon Mac for
x86_64, deployment target macOS 15.0, with Xcode's clang and CMake (Unix Makefiles). Build root:
`$NULLMOTH_BUILD_ROOT/build/npu-user` (`R` below); this directory is `$R/npu-user`.

## Sources

| tree | commit | patch script |
|---|---|---|
| level-zero (oneapi-src/level-zero) | `9e66ef89998c5ce094732a51a13688071b1daa4c` | `patch_level_zero.py` |
| linux-npu-driver (intel) | `c2e2788afac6d7f1b3d7585440bf8137a3ce89c1` | `patch_npu_umd.py` |
| openvino (openvinotoolkit) | `d8047fb380b27a9d5827cb3f22aec7781ae5ac82` | `patch_openvino.py` |
| npu_compiler (openvinotoolkit) | `d70915de9af0a2b5989c2f4f7b9fc7572a1bd8fb` | `patch_npu_compiler.py` |
| xdna-driver (amd) | `4848482178aa242ea40887b982544070f47d0f75` | `patch_xdna.py` |
| xdna-driver/xrt (Xilinx/XRT) | `1932bdf94577410e29d96423e9de5aa680090471` | `patch_xrt.py` |
| xdna-driver/vtd (Xilinx/VTD) | `fa945e489497397269cbb912e3e7cce33d1935ff` | none (fetched at install, `nullmoth-npu-tests.txt`) |

The compiler's prebuilt SHAVE kernels are git-LFS files embedded into every compiled model: fetch them with
`python3 fetch_lfs.py $R/npu_compiler openvinotoolkit/npu_compiler sw_runtime_kernels/kernels/prebuild
sw_runtime_kernels/rt_kernel/prebuild` (794 files, each checked against its pointer's sha256; without them compilation
stops at "Incorrect ELF magic"). Builds may spread compiles over other Macs: `RCC_HOSTS="laptop:10"` with
build_openvino.sh (rcc.py launcher; the remote needs the same clang major as the build machine).

Each patch script runs once on a pristine checkout (`python3 patch_X.py <tree>`); every edit is an exact anchor with an
asserted count, so a script that no longer matches its tree stops instead of half-applying. OpenVINO and XRT are
cloned without git-lfs content (`-c filter.lfs.smudge= -c filter.lfs.process= -c filter.lfs.required=false`).

Compat headers: `compat/` (Level Zero, the Intel UMD), `compat-l0/` (OpenVINO), `compat-xrt/` (XRT, the XDNA shim;
`nmx_xrt_sysfs.hpp` is the macOS device discovery and mapping layer).

Dependencies built for x86_64 into `$R/deps-x86`: Boost 1.86.0 (static; atomic, filesystem, program_options, system,
thread), and empty `libuuid.a` / `librt.a` (glibc splits these out; macOS has them in libSystem). libdrm 2.4.134 comes
from the GPU userland sysroot (`$NULLMOTH_BUILD_ROOT/build/gpu-src/sysroot-x86`).

## Configure and build

Common: `-DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=x86_64 -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0`.

- **Level Zero** (`level-zero/build-x86`): `-DCMAKE_INSTALL_PREFIX=/Library/NullMoth/npu`,
  C/CXX flags `-I$R/npu-user/compat`; `make ze_loader`.
- **Intel NPU user-mode driver** (`linux-npu-driver/build-x86`): same prefix, C/CXX flags
  `-I$R/npu-user/compat -include $R/npu-user/compat/nmx_umd_compat.h`; `make ze_intel_npu`.
- **OpenVINO runtime and the NPU compiler**: `./build_openvino.sh` (both configure lines are in it). It builds from
  `$R/openvino-pin`, a worktree at the OpenVINO commit the compiler is validated against
  (`npu_compiler/validation/openvino_config.json`) and refuses any other: the compiler does not build against
  newer OpenVINO (its XML deserializer API changed). Runtime in `build-x86` (shared, NPU/CPU/AUTO plugins, samples),
  compiler in `build-cid` (OpenVINO static inside `libopenvino_intel_npu_compiler_loader` and the compiler).
- **XRT + XDNA shim + xrt-smi** (`xdna-build-x86`, source `xdna-driver`): `PKG_CONFIG_PATH=<sysroot>/lib/pkgconfig`,
  `-DCMAKE_SYSTEM_PROCESSOR=x86_64 -DCMAKE_CXX_STANDARD=17 -DBoost_ROOT=$R/deps-x86 -DBoost_USE_STATIC_LIBS=ON
  -DXRT_ENABLE_USDT=OFF -DAIEBU_BUILD_TESTS=OFF`, C/CXX flags `-I$R/npu-user/compat-xrt -I<sysroot>/include
  -I$R/deps-x86/include -include $R/npu-user/compat-xrt/nmx_xrt_compat.h`, linker flags `-L$R/deps-x86/lib`;
  `make xrt_coreutil xrt_core xrt_driver_xdna xrt-smi`.
- **nmx-npu-probe** (`tools/nmx-npu-probe.c`): `xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -Wextra
  -Werror -I$R/npu-user/compat-xrt tools/nmx-npu-probe.c -o $R/tools-x86/nmx-npu-probe`.
- **nmx-npu-compile-check** (`tools/nmx-npu-compile-check.c`): the same flags with
  `-I$R/npu_compiler/src/vpux_driver_compiler/include`, output `$R/tools-x86/nmx-npu-compile-check`.

## Stage

`./stage_npu.sh <out>` assembles the tree that installs at `/Library/NullMoth/npu` (layout in the script's header),
replaces build rpaths, signs every Mach-O ad hoc and refuses any file that names a home directory. The release
(`support-cycle/release.py`) takes it from `build/kexts/gpu/npu-user` and refuses a package missing any part.

## What was checked without NPU hardware

- Intel NPU compiler: `nmx-npu-compile-check` compiles networks for NPU 3720 (Meteor/Arrow Lake) and 4000 (Lunar Lake)
  to ELF blobs through the loader the user-mode driver uses (ReLU 65,312 / 58,720 bytes).
- Intel's user-mode driver unit tests on its mock NPU: `ze_intel_npu_tests` 164/164 with
  `--blob <3720 blob of tests/models/relu_flat> --model tests/models/add_abc.xml --model_flags <io options>` (the
  in-driver compiler tests compile through the macOS compiler; it must sit beside the test binary or libze_intel_npu),
  `npu_shared_tests` 60/60.

On an M4 Mac mini under Rosetta (no NPU present): `xrt-smi examine` loads XRT, finds the XDNA plug-in, scans for NPU
nodes and reports `0 devices found` (exit 0); `xrt-smi validate` asks for a device; `nmx-npu-probe` reports no
`/dev/accel`; OpenVINO's `hello_query_device` lists the CPU. The kernel side (NMXNPU) links; on NPU hardware the
first checks are `ls -la /dev/accel`, `nmx-npu-probe`, `xrt-smi examine` / `validate`, `hello_query_device`.
