#!/bin/bash
# build_openvino.sh: the OpenVINO runtime (with its NPU plugin) and Intel's NPU compiler for macOS x86_64, from the
# OpenVINO commit the compiler is validated against (npu_compiler/validation/openvino_config.json) - the pair Intel's
# CI builds. Both trees must already carry patch_openvino.py / patch_npu_compiler.py.
#   runtime:  $OV/build-x86  (shared libraries, plugins, samples)
#   compiler: $OV/build-cid  (OpenVINO static inside the driver-compiler library the NPU user-mode driver loads)
# Outputs land in $OV/bin/intel64/Release (stage_npu.sh takes them from there).
set -euo pipefail
R=$NULLMOTH_BUILD_ROOT/build/npu-user
OV=$R/openvino-pin
C=$R/npu-user/compat-l0
export PATH=/opt/homebrew/bin:$PATH
JOBS=${JOBS:-9}
want=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["openvinotoolkit"])' \
       "$R/npu_compiler/validation/openvino_config.json")
have=$(git -C "$OV" rev-parse HEAD)
[ "$want" = "$have" ] || { echo "build_openvino: $OV is $have, the compiler is validated against $want" >&2; exit 1; }

common=(-G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=x86_64
        -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 -DCMAKE_SYSTEM_PROCESSOR=x86_64 -DTHREADING=SEQ
        "-DCMAKE_C_FLAGS=-I$C" "-DCMAKE_CXX_FLAGS=-I$C"
        -DENABLE_INTEL_GPU=OFF -DENABLE_DOCS=OFF -DENABLE_PYTHON=OFF -DENABLE_TESTS=OFF
        -DENABLE_OV_ONNX_FRONTEND=OFF -DENABLE_OV_PADDLE_FRONTEND=OFF -DENABLE_OV_PYTORCH_FRONTEND=OFF
        -DENABLE_OV_TF_FRONTEND=OFF -DENABLE_OV_TF_LITE_FRONTEND=OFF -DENABLE_OV_JAX_FRONTEND=OFF)
# RCC_HOSTS="laptop:10 ...": compiles are spread over those Macs by rcc.py (preprocessed here, compiled there, local
# fallback); unset, the launcher is cleared so a cached one never lingers
launcher=""
[ -z "${RCC_HOSTS:-}" ] || launcher=${RCC:-$NULLMOTH_BUILD_ROOT/build/rcc/rcc.py}
common+=("-DCMAKE_C_COMPILER_LAUNCHER=$launcher" "-DCMAKE_CXX_COMPILER_LAUNCHER=$launcher")

if [ "${1:-all}" != cid ]; then
    cmake -S "$OV" -B "$OV/build-x86" "${common[@]}" -DCMAKE_INSTALL_PREFIX=/Library/NullMoth/npu \
          -DENABLE_INTEL_NPU=ON -DENABLE_SAMPLES=ON
    make -C "$OV/build-x86" -j"$JOBS"
fi
if [ "${1:-all}" != runtime ]; then
    cmake -S "$OV" -B "$OV/build-cid" "${common[@]}" -DCMAKE_CXX_STANDARD=17 \
          -DOPENVINO_EXTRA_MODULES="$R/npu_compiler" -DBUILD_COMPILER_FOR_DRIVER=ON -DBUILD_SHARED_LIBS=OFF \
          -DENABLE_INTEL_NPU=ON -DENABLE_NPU_EXECUTION_ENGINE=OFF -DENABLE_ZEROAPI_BACKEND=OFF \
          -DENABLE_INTEL_CPU=OFF -DENABLE_AUTO=OFF -DENABLE_AUTO_BATCH=OFF -DENABLE_HETERO=OFF -DENABLE_PROXY=OFF \
          -DENABLE_TEMPLATE=OFF -DENABLE_SAMPLES=OFF
    make -C "$OV/build-cid" -j"$JOBS" openvino_intel_npu_compiler openvino_intel_npu_compiler_loader
fi
echo "build_openvino: done ($want)"
