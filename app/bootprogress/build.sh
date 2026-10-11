#!/bin/bash
# build.sh <out-dir>: NullMothBootProgress.app (x86_64). The empty __CGPreLoginApp,__cgpreloginapp section is what the
# window server checks before login (Installer Progress carries the same one); without it the window server answers
# "This user is not allowed access to the window system right now". The entitlement is the one Installer Progress holds.
set -euo pipefail
H=$(cd "$(dirname "$0")" && pwd); O=${1:?out dir}; A="$O/NullMothBootProgress.app"
rm -rf "$A"; mkdir -p "$A/Contents/MacOS" "$A/Contents/Resources"
: > "$O/.empty"
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -fobjc-arc -O2 -Wall -framework AppKit -framework Foundation \
  -Wl,-sectcreate,__CGPreLoginApp,__cgpreloginapp,"$O/.empty" "$H/main.m" -o "$A/Contents/MacOS/NullMothBootProgress"
cp "$H/../efi-logo/assets/logo1x.rgba" "$H/../efi-logo/assets/logo2x.rgba" "$A/Contents/Resources/"
cp "$H/Info.plist" "$A/Contents/Info.plist"
codesign --force -s - --entitlements "$H/entitlements.plist" "$A"
echo "built $A"
