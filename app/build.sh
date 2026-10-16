#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
OUT=${1:-build}; APP=$OUT/1401.app
[ -f Resources/NullMothSafe.efi ] || { echo "STOP: Resources/NullMothSafe.efi missing (build efi-safe first)"; exit 1; }
SDK=$(xcrun --sdk macosx --show-sdk-path)
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
xcrun swiftc -O -target x86_64-apple-macos15.0 -sdk "$SDK" -framework WebKit -framework Metal -framework IOKit \
  Sources/main.swift Sources/profile.swift Sources/redaction.swift Sources/actions.swift Sources/diagnostics.swift Sources/HardwareMap.swift Sources/HardwareMapWorker.swift Sources/NativePeripheralFacts.swift Sources/SavedReports.swift Sources/ReportQueue.swift Sources/Privileged.swift Sources/ReleaseSignature.swift -o "$APP/Contents/MacOS/1401"
cp Resources/* "$APP/Contents/Resources/"
xcrun clang -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/main.c -o "$APP/Contents/Resources/nullmoth-runtime-check"
codesign --force --runtime-version 15.0.0 --options runtime,library -s - "$APP/Contents/Resources/nullmoth-runtime-check"
xcrun clang -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/log_watch.c -o "$APP/Contents/Resources/nullmoth-log-watch"
codesign --force --runtime-version 15.0.0 --options runtime,library -s - "$APP/Contents/Resources/nullmoth-log-watch"
# Plain ad-hoc, deliberately without the hardened runtime: it has to load the NVIDIA Metal plugin, which is ad-hoc signed
# and which library validation refuses (see RuntimeCheck/metal_devices.m).
xcrun clang -fobjc-arc -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/metal_devices.m \
  -framework Foundation -framework Metal -o "$APP/Contents/Resources/nullmoth-metal-devices"
codesign --force -s - "$APP/Contents/Resources/nullmoth-metal-devices"
# "Map my GPU": the same separate-process reason as nullmoth-metal-devices (RuntimeCheck/gpumap.m)
xcrun clang -fobjc-arc -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/gpumap.m \
  -framework Foundation -framework Metal -o "$APP/Contents/Resources/nullmoth-gpumap"
codesign --force -s - "$APP/Contents/Resources/nullmoth-gpumap"
chmod 755 "$APP/Contents/Resources/nullmoth-runtime-check.sh"
chmod 755 "$APP/Contents/Resources/nullmoth-setup.sh"
# setup runs the installers only when they are executable ([ -x ]); a source file that lost its mode made every app
# install stop with "the audited installer is missing from the app" (1.15 review), so the bundle sets it here
chmod 755 "$APP/Contents/Resources/nullmoth-install.sh" "$APP/Contents/Resources/nullmoth-uninstall.sh"
xcrun clang -fobjc-arc -O1 -Wno-deprecated-declarations -target x86_64-apple-macos15.0 -isysroot "$SDK" \
  -ffile-prefix-map="$(pwd)/DiagnosticsNative=/src/diagnostics" -fdebug-prefix-map="$(pwd)/DiagnosticsNative=/src/diagnostics" \
  DiagnosticsNative/preflight.m DiagnosticsNative/authority/NDSecurityState.m DiagnosticsNative/authority/NDAuthorityPolicy.m \
  DiagnosticsNative/authority/NDCaptureRight.m DiagnosticsNative/module/NDNativeDiagnostics.m DiagnosticsNative/module/NDBoundedProcess.m \
  DiagnosticsNative/module/NDTracePolicy.c -framework Foundation -framework Security -framework IOKit \
  -o "$APP/Contents/Resources/nullmoth-diagnostics-preflight"
strip -S "$APP/Contents/Resources/nullmoth-diagnostics-preflight"
codesign --force --runtime-version 15.0.0 --options runtime,library -s - "$APP/Contents/Resources/nullmoth-diagnostics-preflight"
IS=$(mktemp -d)/m.iconset; mkdir -p "$IS"
for s in 16 32 128 256 512; do
  sips -z $s $s Resources/moth-mark.jpg --setProperty format png --out "$IS/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) Resources/moth-mark.jpg --setProperty format png --out "$IS/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$IS" -o "$APP/Contents/Resources/1401.icns"
cat > "$APP/Contents/Info.plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleExecutable</key><string>1401</string>
<key>CFBundleIconFile</key><string>1401</string>
<key>CFBundleIdentifier</key><string>com.nullmoth.1401</string>
<key>CFBundleName</key><string>1401</string>
<key>CFBundleDisplayName</key><string>1401</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>2.1.0</string>
<key>CFBundleVersion</key><string>88</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHumanReadableCopyright</key><string>© 2026 NullMoth Systems</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSRemovableVolumesUsageDescription</key><string>1401 reads the OpenCore settings on your USB stick or disk to install the driver there.</string>
<key>NSAppleEventsUsageDescription</key><string>1401 asks macOS for your password to install the driver, and to restart when you click Restart.</string>
</dict></plist>
PL
# --options runtime,library: library validation, so only code signed by the
# same identity loads into the bundle. Was a bare ad-hoc signature, which let
# any dropped-in library load into the process that asks for the password.
# The helper binaries above were already signed this way.
# The hardened runtime blocks Apple Events the app sends itself (Restart asks System Events) unless the signature
# carries this entitlement. The Metal device list runs in nullmoth-metal-devices for the same reason the app is hardened.
ENT=$(mktemp -d)/1401.entitlements
cat > "$ENT" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>com.apple.security.automation.apple-events</key><true/>
</dict></plist>
PL
codesign --force --runtime-version 15.0.0 --options runtime,library --entitlements "$ENT" -s - "$APP"
echo "built $APP ($(du -sh "$APP" | cut -f1))"
