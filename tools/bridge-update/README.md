# Wine bridge update for v3.0.1-nvk

Install `nvidia-nvk-bridge-update-3.0.1-nvk.tar.gz` for an **iGPU + NVIDIA dGPU**
setup if you play games through **Wine, CrossOver or YAAGL**. It corrects Wine's
initial window binding when the system-default Metal device is Intel or AMD.
The game still renders on NVIDIA and macOS composes its window onto the display.

This is a **one-time supplement for v3.0.1-nvk**. This fix will be included by
default in future driver updates. The existing full driver download is unchanged.

## Install

Requires the original v3.0.1-nvk Wine provider, Intel macOS 15.5+ and administrator
access. Extract the archive, then:

1. Quit all Wine/CrossOver/YAAGL games and launchers.
2. Open `Check.command`, then `Install.command`.
3. Relaunch your launcher and game. **No reboot is needed.**

The installer checks the existing provider, executable signatures, complete file
hashes and Vulkan-library alias. It replaces only the Wine frontend and its
manifests/checksums under `/Library/GPUBundles/nvmtl/wine`. The renderer, DXVK,
VKD3D, Minecraft runtime, Zink, kernel extensions and encoding components are
retained. Other provider versions or modified files are refused. Identical
installs are skipped. No game files, launch options or GPU spoof settings change.

## Rollback

Quit the same applications and open `Rollback.command` from this extracted
package. It restores the verified original files and metadata. Keep the package
until testing is complete. The protected backup and transaction journal are at:

```text
/Library/Application Support/NVK-Bridge-Updates/3.0.1-nvk-hybrid-1
```

An interrupted install can be recovered with `Install.command` or
`Rollback.command`. Rollback refuses files that no longer belong to this update,
preserving a later driver upgrade. Check or reinstall the supplement after
reinstalling the original v3.0.1-nvk driver.

## Validation limits

The affected Intel UHD 770 + CMP 40HX setup returned surface-loss errors with
Wine's Intel-bound layer. Both explicitly NVIDIA-bound and deferred-layer probes
displayed changing colors and completed 180 GPU-copy presents on that machine.
The patched game's rendering still needs confirmation there. AMD hybrid systems
and muxless laptops have not been tested.

Production surface-binding regression checks and existing bridge host checks
cover the initial foreign device, unchanged NVIDIA/native paths, later mismatch
rejection, small-BAR recovery and asynchronous presentation. Install, rollback,
interruption and integrity tests run against isolated provider copies.
