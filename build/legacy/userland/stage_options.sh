#!/bin/bash
# stage_options.sh: write the install-time options into AUDIT/stage/<release>/options/. Text, patch files and one ready
# property list per option; nothing is applied anywhere.
set -u
A=${AUDIT:?}
for R in 14 13 12 11 10.15 10.14 10.13 10.12 10.11; do
  S=$A/stage/$R; [ -d "$S" ] || continue; O=$S/options; rm -rf "$O"; mkdir -p "$O"
  case $R in 10.13|10.14)
    X=$O/compositor-route-A-configuration-key; mkdir -p "$X"
    C=$A/sys/$R/System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/Resources/Configuration.plist
    cp "$C" "$X/Configuration.plist.original"; plutil -insert BootToMetalCompositor -bool true -o "$X/Configuration.plist.with-key" "$C"
    cat > "$X/README.txt" <<EOT
Metal desktop on macOS $R, route A: the window server's own configuration key.

What changes: one file,
  /System/Library/PrivateFrameworks/SkyLight.framework/Versions/A/Resources/Configuration.plist
gains the boolean key BootToMetalCompositor = true. Nothing else. With the key true and a Metal device that is not
headless, the window server starts its Metal compositor (activate_compositor_default, read from the $R binary).
Configuration.plist.original is the file as Apple ships it on the recovery image of $R (INFERRED equal on a full
install; compare before replacing). Configuration.plist.with-key is that file plus the key.

To apply (system integrity protection off, system volume mounted writable, as root):
  cp -p <that path> /Library/NullMoth/Configuration.plist.before-route-A
  /usr/libexec/PlistBuddy -c "Add :BootToMetalCompositor bool true" <that path>
  restart
To undo:
  /usr/libexec/PlistBuddy -c "Delete :BootToMetalCompositor" <that path>     (or copy the saved file back)
  restart

Trade-off: no kernel change and no IOPresentment needed, but it edits a file inside a system framework, which a system
update replaces (the desktop then falls back to the OpenGL compositor until the key is added again).
Needs hardware: not run. First check after a restart: log show --last boot | grep -i "compositor activated"
EOT
    X=$O/compositor-route-B-service-name-and-preference; mkdir -p "$X"
    cat > "$X/README.txt" <<EOT
Metal desktop on macOS $R, route B: Apple's default path (IOPresentment decides, the window server follows).

What changes, two things:
 1. Kernel: an I/O Kit service whose class is named NVDA must exist. CoreDisplay looks for services of class
    AppleIntelFramebuffer, NVDA and AMDFramebuffer; with none found it turns IOPresentment off and the window server
    starts its OpenGL compositor. Our framebuffer class is NVRMNVDAFramebuffer, which does not count.
    NVRMFB-class-NVDA.patch adds a small marker service of that class beside the framebuffer (source and personality).
    It is a patch for the kernel lane's tree; it is NOT applied and NOT compiled here.
 2. Preference: useIOP_onNVIDIA = true in the domain com.apple.CoreDisplay (any user, current host). NVIDIA is off
    by default in CoreDisplay of $R; Intel is on.
       defaults write /Library/Preferences/com.apple.CoreDisplay useIOP_onNVIDIA -bool true
    To undo:  defaults delete /Library/Preferences/com.apple.CoreDisplay useIOP_onNVIDIA     and remove the marker.

Trade-off: no system file is edited and it survives system updates, but it switches IOPresentment on, so the
accelerator must carry that display path on $R, and a class named NVDA collides with Apple's own NVIDIA kext if a
second, older NVIDIA card makes that kext load.
Needs hardware: not run. First check: ioreg -c NVDA | head ; then the same log line as route A.
EOT
    cat > "$X/NVRMFB-class-NVDA.patch" <<'EOT'
Not applied, not compiled. For kexts/NVRMFB/fb in the kernel lane's tree.

--- a/kexts/NVRMFB/fb/nvrm-fb.cpp
+++ b/kexts/NVRMFB/fb/nvrm-fb.cpp
@@ after the line "OSDefineMetaClassAndStructors(NVRMFBClaim, IOService)" @@
+
+// CoreDisplay of macOS 10.13 and 10.14 decides whether to use IOPresentment (and with it the Metal compositor) by
+// looking for a service of class AppleIntelFramebuffer, NVDA or AMDFramebuffer. This marker carries that class name
+// beside our framebuffer; it does nothing else. Built only where the install-time option asks for it.
+#ifdef NVRMFB_PUBLISH_CLASS_NVDA
+class NVDA : public IOService
+{
+    OSDeclareDefaultStructors(NVDA)
+public:
+    bool start(IOService *provider) override {
+        if (!IOService::start(provider)) return false;
+        registerService();
+        return true;
+    }
+};
+OSDefineMetaClassAndStructors(NVDA, IOService)
+#endif

--- a/kexts/NVRMFB/fb/Info.plist
+++ b/kexts/NVRMFB/fb/Info.plist
@@ inside IOKitPersonalities, beside NVRMFBClaim @@
+		<key>NVDAMarker</key>
+		<dict>
+			<key>CFBundleIdentifier</key><string>com.nullmoth.NVRMFB</string>
+			<key>IOClass</key><string>NVDA</string>
+			<key>IOProviderClass</key><string>NVRMNVDAFramebuffer</string>
+			<key>IOMatchCategory</key><string>NVDAMarker</string>
+		</dict>
(use the bundle identifier that Info.plist already carries; add -DNVRMFB_PUBLISH_CLASS_NVDA to the framebuffer build)
EOT
  ;; esac
  case $R in 10.14|10.15|11|12|13|14)
    X=$O/opengl-through-apples-metal-renderer; mkdir -p "$X"
    case $R in 11|12) have="PRESENT on macOS $R (full installer file list): /System/Library/Extensions/AppleMetalOpenGLRenderer.bundle, x86_64, 131 gld entry points.";;
       *) have="NOT DETERMINED for macOS $R: no full installer of $R was read. Check on the machine: ls -d /System/Library/Extensions/AppleMetalOpenGLRenderer.bundle";; esac
    cat > "$X/README.txt" <<EOT
OpenGL apps on macOS $R through Apple's OpenGL-on-Metal renderer.

How the system picks an OpenGL driver on $R (read from libGFXShared.dylib of $R, gfxInitializeLibrary): it lists the
services of class IOAccelerator, reads the property IOGLBundleName from each, finds <name>.bundle with the trusted
bundle search (/Library/GPUBundles, /System/Library/Extensions), loads <bundle>/Contents/MacOS/<name> and asks it for
gldInitializeLibrary and gldGetVersion. There is no other route and no fallback to Metal.
So our accelerator must publish, on its IOAccelerator service (the NVAccel personality):
    IOGLBundleName = AppleMetalOpenGLRenderer
The bundle: $have

Today NVAccel sets that property in code, late: when its display heads are armed (nvrm-accel.cpp, onCountGrewLocked).
A process that asked OpenGL for its drivers before that moment has no accelerated renderer.
The option: NVAccel-IOGLBundleName.patch puts the same key into the personality, so it is there from the first match.
Not applied here. To undo: remove the key (the code path still sets it at arm time).

Trade-off: with the key present from boot, every OpenGL client finds the renderer, including the window server's own
OpenGL compositor on 10.14 if it starts in OpenGL; whether Apple's renderer accepts our Metal device there is unproven.
Needs hardware: not run. First check: ioreg -c IOAccelerator -l | grep IOGLBundleName
EOT
    cat > "$X/NVAccel-IOGLBundleName.patch" <<'EOT'
Not applied. For kexts/NVRM/accel/Info.plist in the kernel lane's tree.

--- a/kexts/NVRM/accel/Info.plist
+++ b/kexts/NVRM/accel/Info.plist
@@ in IOKitPersonalities / NVAccel, after MetalPluginClassName @@
 			<key>MetalPluginClassName</key><string>NVMTLDevice</string>
+			<key>IOGLBundleName</key><string>AppleMetalOpenGLRenderer</string>
EOT
  ;; esac
  case $R in 10.11|10.12)
    X=$O/plugin-in-system-extensions; mkdir -p "$X"
    cat > "$X/README.txt" <<EOT
Where Metal of macOS $R looks for the plugin: only /System/Library/Extensions/<MetalPluginName>.bundle
(+[MTLIOAccelDevice registerDevices] of $R; there is no trusted search and no /Library/GPUBundles on $R).

Default in this set: the bundle stays in /Library/GPUBundles, because the accelerator publishes the relative name
../../../Library/GPUBundles/NVMTLDriver and the path Metal builds from it,
/System/Library/Extensions/../../../Library/GPUBundles/NVMTLDriver.bundle, is that folder. INFERRED from the path
arithmetic and from the same name working on 15; not run on $R.

The option, if the relative name is not honoured on $R: place the bundle at
/System/Library/Extensions/NVMTLDriver.bundle (system integrity protection off) and publish the plain name
(NVAccel-plain-plugin-name.patch, not applied). To undo: remove that bundle and restore the relative name.

Trade-off: the default keeps every release on one layout; the option writes into the system extensions folder, which
the kext cache tooling of $R also scans.
Needs hardware: not run. First check: log show --last boot | grep -i "MetalPluginName\|NVMTLDevice"
EOT
    cat > "$X/NVAccel-plain-plugin-name.patch" <<'EOT'
Not applied. For kexts/NVRM/accel/Info.plist in the kernel lane's tree.

--- a/kexts/NVRM/accel/Info.plist
+++ b/kexts/NVRM/accel/Info.plist
@@ in IOKitPersonalities / NVAccel @@
-			<key>MetalPluginName</key><string>../../../Library/GPUBundles/NVMTLDriver</string>
+			<key>MetalPluginName</key><string>NVMTLDriver</string>
EOT
  ;; esac
  X=$O/video-decode; mkdir -p "$X"
  NOHEVC=""; if [ "$R" = 10.11 ] || [ "$R" = 10.12 ]; then NOHEVC="On $R the library is built without HEVC and HDR metadata: the system has neither."; fi
  cat > "$X/README.txt" <<EOT
Video decode and encode on macOS $R.

In this set: Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib, built for this release. It registers
its decoders and encoders with VideoToolbox (VTRegisterVideoDecoderWithInfo / VTRegisterVideoEncoderWithInfo). Both
calls and the six session calls it uses are exported by VideoToolbox of 10.11, 10.13, 10.14, 12 and 14 (read from each
binary); the library's required imports are all present on $R (IMPORTS.txt). The plugin loads it in every process that
gets our Metal device, as on 15. $NOHEVC

Not in this set: the older decode route. AppleVA finds a decode bundle through the accelerator's IOVABundleName /
IODVDBundleName (Apple's GeForce personality) or IOVARendererID, and Apple's own bundle of 10.13 exports exactly two
entry points, AVD_CreateAVDAccelerator and AVD_DestroyAVDAccelerator. What comes back from the first is a closed C++
accelerator object; a bundle of ours would need that object's whole interface per release, which has not been mapped.

Needs hardware: not run. First check: NVENCVT_LOG=1 with any player, then grep "registered" in the driver log.
EOT
done
echo options written: $(find "$A/stage" -path '*/options/*' -type f | wc -l | tr -d ' ') files
