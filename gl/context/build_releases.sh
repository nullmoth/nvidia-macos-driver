#!/bin/bash
# build_releases.sh <audit base> <out dir> [release ...]
# Runs build_context.sh for each release with that release's own OpenGL headers (so the table has that release's
# layout), the Mesa build of the right floor, that release's measured entry lists and that release's libraries.
# Floors: Mesa needs compiler thread-local storage, which starts at 10.7, so 10.7 to 10.10 share the 10.7-floor
# build and 10.11 to 10.13 the 10.11-floor build. One summary line per release at the end.
set -uo pipefail
B=${1:?}; OUT=${2:?}; shift 2; H=$(cd "$(dirname "$0")" && pwd); S=$B/sdk; F=System/Library/Frameworks
want=${*:-10.13 10.12 10.11 10.10 10.09 10.08 10.07 10.06}
S6=$S/Xcode-3-2-6/MacOSX10.6.x/Payload/SDKs/MacOSX10.6.sdk; export NM_LIBSTDCXX=$S6/usr/lib/libstdc++.6.dylib
IOS11=$S/Command-Line-Tools-OS-X-10-11-for-Xcode-7-3-1/DevSDK_OSX1011/$F/IOSurface.framework/Versions/A/Headers
while read rel root floor; do
  case " $want " in *" $rel "*) ;; *) continue;; esac
  glh=$S/$root/$F/OpenGL.framework/Versions/A/Headers; ios=$S/$root/$F/IOSurface.framework/Versions/A/Headers
  [ -f "$ios/IOSurface.h" ] || ios=$IOS11      # the 10.7 package keeps OpenGL apart; the calls used are unchanged since 10.6
  [ -f "$glh/gliDispatch.h" ] || { echo "SUMMARY $rel: blocked: no OpenGL headers at $root"; continue; }
  o=$OUT/$rel; mkdir -p "$o"
  libs="$B/gl/extract/$rel/syslib"; [ "$rel" = 10.06 ] && libs="$S6/usr/lib $S6/System/Library/Frameworks"   # 10.6: the SDK's stub libraries
  bash "$H/build_context.sh" "$glh" "$ios" "$B/gl/zink/mesa/build-gl-$floor" "$o" "$rel" "$floor" auto $libs > "$o/run.log" 2>&1; rc=$?
  echo "SUMMARY $rel: $([ $rc = 0 ] && echo PASS || echo FAIL) | $(grep -m1 '^table entries' "$o/run.log" | cut -d';' -f1) | $(grep -m1 'TOTAL strong' "$o/run.log" | xargs) | $(grep -m1 'weak import the release lacks' "$o/run.log") | $(grep -m1 'arms,' "$o/run.log") | pixels: $(grep -c '^PASS  PIXELS' "$o/run.log") of $(grep -c 'PIXELS' "$o/run.log") pixel arms"
done <<LIST
10.13 Command-Line-Tools-macOS-10-13-for-Xcode-9-4-1/DevSDK_macOS1013_Public.pkg/Payload 10.11
10.12 Command-Line-Tools-macOS-10-12-for-Xcode-8-3-2/DevSDK_OSX1012.pkg/Payload 10.11
10.11 Command-Line-Tools-OS-X-10-11-for-Xcode-7-3-1/DevSDK_OSX1011 10.11
10.10 Command-Line-Tools-OS-X-10-10-for-Xcode-6-4/DevSDK_OSX1010 10.7
10.09 Command-Line-Tools-OS-X-Mavericks-for-Xcode-April-2014/MacOSX10_9_SDK/Payload 10.7
10.08 Command-Line-Tools-OS-X-Mountain-Lion-for-Xcode-April-2014/DevSDK/Payload 10.7
10.07 Command-Line-Tools-OS-X-Lion-for-Xcode-November-2012/OpenGLSDK/Payload 10.7
10.06 Xcode-3-2-6/MacOSX10.6.x/Payload/SDKs/MacOSX10.6.sdk 10.6
LIST
