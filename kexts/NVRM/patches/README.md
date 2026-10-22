# NVIDIA 610 display completion archive

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
