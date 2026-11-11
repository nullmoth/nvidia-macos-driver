#!/bin/bash
# run_classify.sh <plugin source dir> <floor> <out.tsv>: classify every unguarded-availability site of the plugin.
# Environment: LLVM_PREFIX (an LLVM install with libclang and its python package), MESA_INC (Mesa include folder),
# PYVER (python minor version of that package folder, default 3.9).
L=${LLVM_PREFIX:?}; V=${PYVER:-3.9}
export LIBCLANG=$L/lib/libclang.dylib CINDEX=$L/lib/python$V/site-packages SDKROOT=$(xcrun --show-sdk-path) CLANG_RESOURCE=$($L/bin/clang -print-resource-dir)
exec python3 "$(dirname "$0")/classify.py" "$1" "${MESA_INC:?}" "$2" "$3"
