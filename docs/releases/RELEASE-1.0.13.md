# 1401 Mac 1.0.13 / driver package 1.0.9

This maintenance release fixes installation, updating, removal and USB-map handling. The NVIDIA kernel extensions, Metal plugin, shader translator, Vulkan backend and firmware match driver 1.0.8. New GPU qualification and graphics feature work remain pending for 1.1.

## Changes

- Validate the package manifest, both OS-specific accelerator variants and exact kernel-extension identifiers before installation.
- Build and inspect a staged kernel collection before publishing it. Restore the previous installation if a later install step fails.
- Use the app's audited installer while keeping the downloaded package and its checksum unchanged.
- Check the recorded OpenCore partition, configuration and backup before removal. Preserve unrelated configuration edits and shared AMFI flags unless the install record establishes ownership; carry that ownership across verified upgrades. Keep recovery available until removal completes.
- Rebuild recovery from the current extension repository, preserving other drivers. Refuse incompatible backup collections and retain the recovery trigger after failure.
- Clear stale installed-version records and driver-owned update-parking entries during successful removal.
- Recognize two-part releases such as 1.1 and 1.11, with numeric version ordering. Reset failed download controls so an update can be retried.
- Identify USB controllers by their registry paths, preserve sparse port numbers, enforce the 15-port selection limit and reject stale or invalid selections.
- Keep USB scan results on the main queue and discard results from superseded scans.
- Restrict Tahoe preparation to macOS 15, and recover stale preparation state instead of waiting for an impossible second major-version change.
- Include installed-component fingerprints and processor information in explicitly requested logs. Include recent Firefox, plugin-container and Blender crash reports; paths and identifying information are redacted.

## Validation

- 6 installer rollback and integrity tests, 20 removal/preflight tests, 15 recovery tests and 4 OS-update guard tests passed.
- Production Swift USB-map and version-parser checks passed; updater UI checks passed.
- The release app builds for x86_64 macOS 15 and passes signature and public-content checks. Live startup, updater discovery and USB-controller scanning passed on macOS 15.8.1.
- The exact driver package passed kernel-collection preflight on macOS 15.8.1 without changing the installed driver.
- The packaged NVIDIA binaries and firmware are unchanged from the verified 1.0.8 release asset.

## Remaining work

This release does not qualify additional cards or complete Blender, Firefox, DRM, adjustable refresh rates, switchable graphics or macOS 26 support. Those items remain under investigation for 1.1. Existing boot-policy requirements remain in effect.

Use the DMG for the app and offline driver package together. The ZIP contains the app; the driver tarball is also available separately. Verify downloads against SHA256SUMS.txt.
