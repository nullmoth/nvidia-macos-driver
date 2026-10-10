#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
OUT=${1:-build}; APP=$OUT/1401.app
[ -f Resources/NullMothSafe.efi ] || { echo "STOP: Resources/NullMothSafe.efi missing (build efi-safe first)"; exit 1; }
SDK=$(xcrun --sdk macosx --show-sdk-path)
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
xcrun swiftc -O -target x86_64-apple-macos15.0 -sdk "$SDK" -framework WebKit -framework Metal -framework IOKit \
  Sources/main.swift Sources/profile.swift Sources/redaction.swift Sources/actions.swift Sources/diagnostics.swift Sources/HardwareMap.swift Sources/HardwareMapWorker.swift Sources/NativePeripheralFacts.swift Sources/SavedReports.swift Sources/ReportQueue.swift Sources/Privileged.swift -o "$APP/Contents/MacOS/1401"
cp Resources/* "$APP/Contents/Resources/"
xcrun clang -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/main.c -o "$APP/Contents/Resources/nullmoth-runtime-check"
codesign --force --runtime-version 15.0.0 --options runtime,library -s - "$APP/Contents/Resources/nullmoth-runtime-check"
xcrun clang -O2 -Wall -Wextra -Werror -target x86_64-apple-macos15.0 -isysroot "$SDK" RuntimeCheck/log_watch.c -o "$APP/Contents/Resources/nullmoth-log-watch"
codesign --force --runtime-version 15.0.0 --options runtime,library -s - "$APP/Contents/Resources/nullmoth-log-watch"
chmod 755 "$APP/Contents/Resources/nullmoth-runtime-check.sh"
chmod 755 "$APP/Contents/Resources/nullmoth-setup.sh"
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
<key>CFBundleShortVersionString</key><string>1.10.0</string>
<key>CFBundleVersion</key><string>35</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>NSHumanReadableCopyright</key><string>© 2026 NullMoth Systems</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSAppleEventsUsageDescription</key><string>1401 asks macOS for your password to install the driver, and to restart when you click Restart.</string>
</dict></plist>
PL
codesign --force --deep -s - "$APP"
echo "built $APP ($(du -sh "$APP" | cut -f1))"
