#!/bin/bash
# build trace-linux/nvtrace.so on a Debian 13 machine (the dev VM; same glibc as the Linux 4090 guest)
set -e
cd "$(dirname "$0")"
O=../vendor/open-gpu-kernel-modules
gcc -O1 -g -shared -fPIC -Wall -o nvtrace.so nvtrace.c -ldl -lpthread \
    -I $O/src/common/sdk/nvidia/inc -I $O/src/nvidia/arch/nvalloc/unix/include -I $O/src/common/sdk/nvidia/inc/class
echo built $(pwd)/nvtrace.so
