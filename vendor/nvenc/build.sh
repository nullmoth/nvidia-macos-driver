#!/bin/bash
# Build in the macOS guest (needs the Command Line Tools) over ssh; the binaries stay in the guest's ~/nvenc-dev.
#   ./build.sh [tool ...]          tools are tools/<name>.c, each linked with src/*.c (default: all);
#                                  trace/<name>.c become <name>.dylib (DYLD_INSERT_LIBRARIES interposers),
#                                  vt/<name>.c become lib<name>.dylib (VideoToolbox encoder, linked with src/*.c)
#   NVENC_GUEST=<ssh host>         the guest (default: tahoe)
# Headers come from vendor/ (scripts/fetch-vendor.sh).
set -euo pipefail
cd "$(dirname "$0")"
G=${NVENC_GUEST:-tahoe}
[ -d vendor/open-gpu-kernel-modules ] && [ -d vendor/open-gpu-doc ] || { echo "run scripts/fetch-vendor.sh first"; exit 1; }
tools=${*:-$(ls tools/*.c tools/*.m trace/*.c vt/*.c | grep -v '^vt/.*_' | xargs -n1 basename | sed 's/\.[cm]$//' | tr '\n' ' ')}
tar -C vendor/open-gpu-kernel-modules -chf - src/common/sdk/nvidia/inc src/nvidia/arch/nvalloc/unix/include \
  | ssh "$G" 'rm -rf ~/nvenc-dev/inc && mkdir -p ~/nvenc-dev/inc/ogkm && tar -xf - -C ~/nvenc-dev/inc/ogkm'
tar -C vendor/open-gpu-doc -chf - classes/video | ssh "$G" 'mkdir -p ~/nvenc-dev/inc/ogd && tar -xf - -C ~/nvenc-dev/inc/ogd'
tar -C vendor -chf - webkit-spi | ssh "$G" 'tar -xf - -C ~/nvenc-dev/inc'
VER=$(git describe --tags --always --dirty 2>/dev/null || echo dev)   # the version in the encoder names (vt/nvencvt.c)
tar -cf - src tools trace vt | ssh "$G" "cd ~/nvenc-dev && rm -rf src tools trace vt && tar -xf - && for t in $tools; do
  if [ -f vt/\$t.c ]; then
    clang -O2 -Wall -dynamiclib -DNVENC_VERSION='\"$VER\"' -o lib\$t.dylib vt/\$t.c \$(ls vt/\${t}_*.c 2>/dev/null) src/*.c -I src -I inc/webkit-spi \
      -I inc/ogkm/src/common/sdk/nvidia/inc -I inc/ogkm/src/nvidia/arch/nvalloc/unix/include -I inc/ogd/classes/video \
      -framework IOKit -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework Accelerate -framework IOSurface 2>&1 | grep -E 'error|warning'
  elif [ -f trace/\$t.c ]; then
    clang -O2 -Wall -dynamiclib -o \$t.dylib trace/\$t.c \
      -I inc/ogkm/src/common/sdk/nvidia/inc -I inc/ogkm/src/nvidia/arch/nvalloc/unix/include -framework IOKit -framework CoreFoundation \
      -framework VideoToolbox -framework CoreMedia 2>&1 | grep -E 'error|warning'
  elif [ -f tools/\$t.m ]; then
    clang -O2 -fobjc-arc -Wall -I inc/webkit-spi -I src -I inc/ogkm/src/common/sdk/nvidia/inc -I inc/ogkm/src/nvidia/arch/nvalloc/unix/include -I inc/ogd/classes/video -o \$t tools/\$t.m \$(grep -q '"rm.h"' tools/\$t.m && echo src/*.c || (grep -q blocklinear.h tools/\$t.m && echo src/blocklinear.c; grep -q hevchdr.h tools/\$t.m && echo src/hevchdr.c)) -framework IOKit -framework Foundation -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework Metal -framework IOSurface -framework Accelerate -framework AVFoundation 2>&1 | grep -E 'error|warning'
  else
    clang -O2 -Wall -Wno-unused-function -o \$t tools/\$t.c src/*.c -I src \
      -I inc/ogkm/src/common/sdk/nvidia/inc -I inc/ogkm/src/nvidia/arch/nvalloc/unix/include -I inc/ogd/classes/video \
      -framework IOKit -framework CoreFoundation -framework Accelerate 2>&1 | grep -E 'error|warning'
  fi; echo \"built ~/nvenc-dev/\$t\"; done"
