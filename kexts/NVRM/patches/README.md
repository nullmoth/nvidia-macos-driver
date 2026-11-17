# NVIDIA 610 display completion archive

Current fork builds target NVIDIA **620.70.06**, revision
`e6a2e01579fbbd1ca2ed43fef2ee2589e9bd8bd9`; see
[the complete build recipe](../../../docs/NVRM-BUILD.md). The archived patches below
retain the same behavior. Completion contexts are adjusted to 620; NVIDIA's
620 FSP cleanup already clears `pPollEvent`, so that already-fixed hunk is
removed while the OS timer handle clear remains. The older instructions below
describe the origin of those repairs, not the current build target.

## Hardware vblank after partial modesets

`620-vblank-restore-unchanged-heads.patch` restores RG semaphore interrupts on
active, unchanged API heads. NVIDIA disables them on every head before a
committed modeset, but its unchanged-head return skips re-enabling them. Reuse
the existing enable helper and modeset update; retain unchanged timings,
head mappings, callback registrations and the inactive-head guard.

`tools/test_modeset_vblank_restore.py --ogkm <pristine-620-checkout>` compiles
the production disable loop, enable helper and unchanged-head branch against
display-engine doubles. Sanitized partial-mode, reconnect and fallback checks
pass; the unpatched negative control reproduces the missing interrupts.
The RTX 2060 SUPER desktop remains smooth with the trial loaded, but that GPU
reports this interrupt method unsupported and retains its timer fallback.
Hardware interrupt restoration, mixed-refresh and remote playback validation
are still pending.

## Archived display and RM repairs

The framebuffer and accelerator allocation interface changes must be built and installed together. Use the NVRMFB framebuffer source and the matching NVRM headers from this revision.

`610-display-completion.patch` implements the mapped flip notifier query and resets the selected slot for each committed surface. NVIDIA modeset treats the BEGUN notifier state as the latched flip; FINISHED is also accepted. Invalid or unavailable notifier state keeps the transaction pending.

`610-setmode-reason.patch` applies after it. A refused mode set logged only "commit rejected" in release builds (the reason lines are debug-only), so a WindowServer that could not set its first mode stopped with no cause on record. The patch logs one warning with the set-mode status, the display status, and each head's status.

`615-viewport-scaling.patch` applies to `kapi/src/nvkms-kapi.c` of NVIDIA 615.78.08 (from PR #273 by Leaf-lsgtky, the finding and implementation by lsCoding666 in #221). Primary-layer scaling (`sizeIn != sizeOut`) is not offered on GPUs that scale at the head, so NVKMS refused every scaled desktop and HiDPI mode. The primary layer stays 1:1 and the desktop size becomes the head's viewport input, so the head's scaler fits it to the display's timings.

`610-backlight-register-log.patch` applies to `src/nvkms-rm.c`. A laptop panel driven by the GPU has no brightness control when NVKMS registers no backlight, and both inputs to that decision (the internal display mask and the brightness query) are answered by the GPU firmware with nothing on record. The patch logs the internal display status and masks, each display's brightness query status and level, and the case where none answered. No behaviour change.

With the original NVIDIA 610.57.04 Darwin modeset archive and compile commands already built, run:

```
python3 kexts/NVRM/build-completion-archive.py --vendor /build/vendor/src/nvidia-modeset --output /build/paired-completion
```

Link NVRM against the resulting `libnvkms.a`, not the original archive. The script preserves the original archive and replaces only the completion object and release build metadata. Build NVAccel with the matching allocation cookie interface, once for macOS 15 and once for macOS 26.

## RM boot-failure cleanup

`610-fsp-timer-double-destroy.patch` applies to the open-gpu-kernel-modules 610.57.04 tree (`git apply` from its root) before the RM objects are built. When the FSP boot commands fail, `kfspCleanupBootState` runs from both the failure path and `kfspStateDestroy`; the second `tmrEventDestroy` faulted on the freed OS timer (page fault at 0x1010 in `nv_destroy_nano_timer`). The patch destroys the poll event once and clears the OS timer handle after cancelling it.
