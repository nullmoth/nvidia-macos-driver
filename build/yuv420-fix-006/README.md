# GB203 DP-to-HDMI YUV420 4K60 experiment (NullMoth 2.6.5)

This is the source and patch sequence of a **hardware-tested opt-in experiment** for #383, not a claim that arbitrary displays or adapters support this mode. The NVIDIA KAPI changes are needed together with the XNU-side boot-argument hook.

## Reproduced result

- Host: ASUS ROG Maximus Z690 Extreme, Core i9-14900KF; PNY RTX 5080 Slim OC (GB203); macOS Tahoe 26.7.1, OpenCore 1.0.8.
- Displays: LG UltraGear on DP, 4K TV through Denon AVR, and two LG UltraFine displays via *active DP-to-HDMI* adapters (one with HDMI matrix, one with HDMI switch).
- Stock NullMoth 2.5.4 and 2.6.5: both UltraFines were limited to 3840x2160 at 30 Hz, with the 60 Hz timing advertised but rejected by NVKMS. Under Windows with identical hardware they operate at 4K60 using YCbCr 4:2:0 / 8 bpc.
- Custom NVRM built against NullMoth **v2.6.5** and NVIDIA **615.78.08**, with `nvyuv420fix=0x4400`: both UltraFines **simultaneously** run at 3840x2160 / 60 Hz, confirmed in both monitors' OSDs after a real Tahoe boot. The other two displays remained connected.

`0x400` and `0x4000` identify only the two display handles on the tested boot/GPU. The boot-arg gate does **not** implement automatic display discovery or a general-purpose fix for other configurations.

## Sources and layout

- NullMoth base: `v2.6.5` / `ceba63c15ba9d81deea7cd1c5dcc4c79497bea8f`.
- NVIDIA open GPU kernel modules: tag `615.78.08` / `1a82fbfda8f321b056eb3568850f9316c5e465c5`.
- `kexts/NVRM/os-nvkms-xnu.cpp`: the **exact, hash-verified** XNU source used in the successful fix-006 build. SHA-256: `4fb65aed709d993328d76ec23bc07ffc16b2c94f65b6c6e735ab2d08a5868e22`.
- `kexts/NVRM/patches/615-yuv420-diagnostic.patch`: earlier A/B diagnostic, including `nvkms-modepool.c`.
- `patches/615-yuv420-trigger-overview.patch`: follow-up diagnostic counters and broader trigger.
- `patches/615-yuv420-fix-discovery.patch`: revalidates a rejected EDID 4K60 mode with `preferYUV420=NV_TRUE`; requires confirmed `NV_YUV420_MODE_HW` and identical timing.
- `patches/615-yuv420-fix-validate-setmode.patch`: carries HW YUV420 through `ValidateDisplayMode` and `ApplyModeSetConfig` by matching the original EDID timing.

The last three vendor patches apply under the NVIDIA `src/nvidia-modeset` source directory, **in the order above**, after the diagnostic patch. The diagnostic and fix patches depend on the existing 615 Darwin baseline modifications and cannot simply be applied to a pristine upstream NVIDIA checkout. That baseline (five patches, compiler/linker recipe and SHA-256 provenance) is documented in the fork's earlier `experiment/nvrm-615-yuv420-fix005-dual` branch under `build/nvrm615/`. The current branch does not pretend these external NVIDIA KAPI patches are automatically picked up by NullMoth's package build; integration into the official NVRM build path is still required.

The 2.6.5 candidate changed **only NVRM.kext**; NVAccel, NVRMFB, NVRMAGDC and userspace stayed at official 2.6.5. Bundle metadata was kept equal to official NVRM 2.6.5.

## Evidence from the paired 2.6.5 builds

A candidate and a diagnostic-only control were built from identical 2.6.5 sources and the same compiler recipe/toolchain; the control reverted only the XNU production hook and NVIDIA KAPI production fix. Results:

- 1,335 of 1,337 compiled objects **byte-identical**. Only `glue/os-nvkms-xnu.o` and `nvkms-kapi.o` changed.
- Same **478 kernel imports**; no missing exports or unexpected imports. Compared with the diagnostic control, the only added global export was `_nvkms_yuv420_fix_enabled`.
- Conservative disassembly audit: 31,150 symbols examined; 23,472 comparable and identical, three different (`_ApplyModeSetConfig`, `_GetDisplayMode`, `_ValidateDisplayMode`), 7,675 not safely comparable. These results are not whole-program-equivalence proof.
- The candidate signed successfully and a test Auxiliary Kernel Collection linked all four NullMoth kexts.
- A real boot on GB203/Tahoe 26.7.1 subsequently confirmed simultaneous 4K60 on both affected physical displays.

Candidate NVRM executable SHA-256: `014e82903cb674156204023018a74c447651b56ba5cbc7496e6fd2d08f744330`.
Official 2.6.5 NVRM executable SHA-256: `51621ea4ae7ff09d829da64fec9d62ada2589e54b7c91e25bc39c9f1d4d47b2e`.

## Before merging

The current opt-in is deliberately **hardware/handle-specific** and was validated on one host. For upstream use, the selection must be reviewed for stable/automatic identification, the NVIDIA KAPI patch should be wired into the official 615.78.08 build recipe, and tests on additional DP-to-HDMI paths are desirable. This is the exact working research state, not a universal driver setting.
