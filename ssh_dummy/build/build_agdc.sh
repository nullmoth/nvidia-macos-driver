#!/bin/bash
set -u
D="$HOME/nvmtl-build/nm/agdc"; B="$HOME/nvmtl-build/nm/out/agdc"; rm -rf "$B"; mkdir -p "$B/NVRMAGDC.kext/Contents/MacOS"
SDK="$(xcrun --show-sdk-path)"; KHDR="$SDK/System/Library/Frameworks/Kernel.framework/Headers"
xcrun clang++ -arch x86_64 -fapple-kext -mkernel -nostdinc -I"$KHDR" -I"$D" -DKERNEL -DKERNEL_PRIVATE -DDRIVER_PRIVATE -DAPPLE -DNeXT \
  -std=c++17 -fno-rtti -fno-exceptions -fno-builtin -fno-common -Wall -Wno-unused-parameter -Wno-inconsistent-missing-override -Wno-unused-function -O2 \
  -c "$D/nvrm-agdc.cpp" -o "$B/k1.o" 2>"$B/cc.err" || { grep error "$B/cc.err" | head; exit 1; }
xcrun clang++ -arch x86_64 -fapple-kext -nostdlib -Xlinker -kext -lkmodc++ -lkmod -lcc_kext "$B/k1.o" -o "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC" 2>"$B/ld.err" || { head "$B/ld.err"; exit 1; }
cp "$D/Info.plist" "$B/NVRMAGDC.kext/Contents/Info.plist"
echo "NVRMAGDC $(md5 -q "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC") $(dwarfdump --uuid "$B/NVRMAGDC.kext/Contents/MacOS/NVRMAGDC" | awk '{print $2}')"
