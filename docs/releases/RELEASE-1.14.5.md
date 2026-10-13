# NVIDIA driver and 1401 Mac 1.14.5

Maintenance update following 1.14.4.

## Changes

- Send logs: each system-log query has its own time limit and keeps partial output. Current-boot kernel evidence is collected separately from previous-boot evidence. Only the live EFI configuration and its two newest backups are included, so older backups cannot fill every report slot. The collection watchdog allows the bounded queries to finish.
- Chromium WebGL: preserve hardware device creation in sandboxed GPU processes when the physical-memory sysconf query is denied, using the Mach host memory query in the driver's own Vulkan image. Disable ANGLE's NVIDIA Metal exclusion when the NVIDIA Metal bundle loads. Existing ANGLE feature overrides are preserved.
- Command-buffer completion: avoid locking the command buffer or command queue that an application can hold while waiting for completion. Capacity release is atomic and happens once. This fixes the reproduced Unity-style lock/commit/wait deadlock.

## Validation

- RTX 5060, macOS 15.8.1: 200 lock/commit/wait command-buffer cycles complete, including the expected final buffer value. The installed test plugin matches the built plugin.
- Chrome 155: WebGL and WebGL2 were exercised with the GPU sandbox enabled in the development check. Broader Chromium and Electron coverage requires application-specific validation.
- Source regression checks, Mac signatures and artifact identity, packaged Windows CI, payload pins and public-content scans gate publication. Validation receipts accompany the downloadable assets.

## Remaining limits

Firefox's GPU-process WebGL context creation remains under investigation; native OpenGL context creation succeeds outside that process. This release does not claim a complete Vampire Survivors playthrough, universal game support, DRM qualification or new GPU-model qualification. Existing kernel binaries and firmware are preserved from 1.14.4.

## Updating

Use Check for updates in 1401, install 1.14.5 and restart as instructed. If a failure remains, use Send logs from the updated Mac app so the report includes the bounded kernel and display evidence.
