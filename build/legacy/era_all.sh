#!/bin/bash
# era_all.sh: test-build the kernel side for every release we hold headers and a kernel kit for, then check each
# linked kext's imports against that release's real kernel and Apple kexts. One line of result per kext per release.
B=${B:?set B to the folder holding sdk/ (unpacked SDKs), kdk/ (unpacked kernel kits) and out/}; H=$(cd "$(dirname "$0")" && pwd); S=$B/sdk
P=Payload/Library/Developer/CommandLineTools/SDKs; K=System/Library/Frameworks/Kernel.framework/Versions/A/Headers
export SRC=${SRC:-$B/legacy-src/kexts} OGKM=${OGKM:?set OGKM to the patched open-gpu-kernel-modules tree with an RM build} SHOW=${SHOW:-4}
while read era mv kit tag khdr; do
  [ -n "$ONLY" ] && [ "$ONLY" != "$mv" ] && continue
  rm -rf "$B/out/$mv"; echo "=================== macOS $mv"
  # headers regenerated for this release from its own tables (gen_era_headers.py), when they exist
  if [ -d "$B/gen-$era" ]; then export ACCEL_HDR="$B/gen-$era"; else unset ACCEL_HDR; fi
  # names the release's kernel exports, for the duplicate-global rule of the kernel's kext linker (before 11 only:
  # from 11 on kexts are linked into a collection by a different tool that has no such rule)
  unset KERN_SYMS FAMBIN
  [ $era -lt 1100 ] && export FAMBIN=$(find "$B/kdk/$kit" -type f -name IOAcceleratorFamily2 -not -path "*dSYM*" | head -1)
  if [ $era -lt 1100 ]; then
    kern=$(find "$B/kdk/$kit" -type f \( -name kernel -o -name mach_kernel \) -not -path "*dSYM*" -not -path "*DEBUG*" -size +1000k | head -1)
    [ -n "$kern" ] && { xcrun nm -arch x86_64 -gU "$kern" | awk '{print $NF}' | sort -u > "$B/vt/kernsyms-$mv.txt"; export KERN_SYMS="$B/vt/kernsyms-$mv.txt"; }
  fi
  bash "$H/era_kexts.sh" $era $mv "$S/$khdr" "$B/out/$mv" 2>&1 | cut -c1-200
  python3 "$H/era_symcheck.py" "$B/out/$mv" "$B/kdk/$kit" "$tag" 2>&1 | cut -c1-140
done <<LIST
1006 10.6 10.6 xnu-1504.15.3 Xcode-3-2-6/MacOSX10.6.x/Payload/SDKs/MacOSX10.6.sdk/$K
1007 10.7 10.7 xnu-1699.32.7 Command-Line-Tools-OS-X-Lion-for-Xcode-November-2012/DevSDK/Payload/$K
1008 10.8 10.8 xnu-2050.48.11 Command-Line-Tools-OS-X-Mountain-Lion-for-Xcode-April-2014/DevSDK/Payload/$K
1009 10.9 10.9 xnu-2422.115.4 Command-Line-Tools-OS-X-Mavericks-for-Xcode-April-2014/MacOSX10_9_SDK/Payload/$K
1010 10.10 10.10 xnu-2782.40.9 Command-Line-Tools-OS-X-10-10-for-Xcode-6-4/DevSDK_OSX1010/$K
1011 10.11 10.11 xnu-3248.60.10 Command-Line-Tools-OS-X-10-11-for-Xcode-7-3-1/DevSDK_OSX1011/$K
1012 10.12 10.12 xnu-3789.70.16 Command-Line-Tools-macOS-10-12-for-Xcode-8-3-2/DevSDK_OSX1012.pkg/Payload/$K
1013 10.13 10.13 xnu-4570.71.2 Command-Line-Tools-macOS-10-13-for-Xcode-9-4-1/DevSDK_macOS1013_Public.pkg/Payload/$K
1014 10.14 10.14 xnu-4903.270.47 Command-Line-Tools-for-Xcode-11-5/CLTools_macOS1014_SDK.pkg/$P/MacOSX10.14.sdk/$K
1015 10.15 10.15 xnu-6153.141.1 Command-Line-Tools-for-Xcode-11-5/CLTools_macOS1015_SDK.pkg/$P/MacOSX10.15.sdk/$K
1100 11.0 11.7 xnu-7195.141.2 Command-Line-Tools-for-Xcode-12-5-1/CLTools_macOSNMOS_SDK.pkg/$P/MacOSX11.3.sdk/$K
1200 12.0 12.7 xnu-8020.140.41 Command-Line-Tools-for-Xcode-13-4/CLTools_macOSNMOS_SDK.pkg/$P/MacOSX12.3.sdk/$K
1300 13.0 13.7 xnu-8796.141.3 Command-Line-Tools-for-Xcode-14-3-1/CLTools_macOSNMOS_SDK.pkg/$P/MacOSX13.3.sdk/$K
1400 14.0 14.8 xnu-10063.121.3 Command-Line-Tools-for-Xcode-15-3/CLTools_macOSNMOS_SDK.pkg/$P/MacOSX14.4.sdk/$K
LIST
