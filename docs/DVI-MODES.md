# DVI timing and RGB scanout

Some DVI-to-HDMI displays are detected and have readable EDIDs but stay black
because their preferred timing fails NVIDIA's modeset validation.

On an RTX 2060 SUPER with an ITY V2422F, NVIDIA's indexed mode query accepted
1920×1080 at 60 Hz, but explicit validation rejected the returned public KAPI
mode as outside the EDID. The EDID's checksums and base detailed timing were
correct. Native indexed diagnostics showed YUV420 metadata on the 60/59.94 Hz
modes; the public KAPI conversion omits that metadata. The same display's
1920×1080 at 75.003 Hz mode had no YUV420 requirement and passed both validators.
This is consistent with metadata loss during conversion, rather than an absent
connector or unreadable EDID.

With NVIDIA 620, the same display could also start with a stretched, green/pink
image and disappear when rotated. Its initial primary flip was accepted but
never completed, so subsequent modesets correctly stopped at the completion
guard. The RGB8 repair below restores a working scanout without bypassing that
guard.

## RGB8 validation and modesets

`kexts/NVRM/patches/620-dvi-rgb-scanout.patch` makes digital TMDS DVI-D/DVI-I
use explicit RGB at eight bits per component in indexed validation, explicit
validation and full modeset conversion. Modes with a remaining software or
hardware YUV420 disposition are not advertised as valid: the public KAPI mode
cannot carry that sideband and the framebuffer supplies RGB surfaces.

Validation and commit use the same color contract. HDMI, DisplayPort, internal
panels, analog DVI-I and connector-query failures retain their existing policy.
The patch keeps NVIDIA's EDID/link checks and the framebuffer's completion,
surface-lifetime, cursor and rotation handling. It adds no KAPI ABI or boot
argument. Rebuild NVRM and its NVKMS archive; the matching framebuffer,
accelerators and AGDC need no code changes.

## Startup timing fallback

For digital TMDS DVI-D/DVI-I only, explicitly validate the chosen startup timing
before sizing the boot raster and scanout. If it fails, search the enumerated
modes for a progressive timing with exactly the same visible resolution that
passes both indexed and explicit validation. Select the closest refresh rate;
keep an already-valid startup timing. No resolution, refresh rate or monitor name
is hardcoded. If no eligible timing exists or enumeration errors, keep the existing
startup path and its failure handling.

Apply the same explicit check to KAPI-generated DVI refresh modes and the EDID
timing-match shortcut. Otherwise a rejected timing could be offered again or
bypass explicit validation when macOS supplies its detailed timings.

The ordinary modeset retains NVIDIA's configuration, EDID and link-limit checks.
This does not force dual-link mode, synthesize a timing or disable validation.
HDMI, DisplayPort, internal panels and analog DVI-I keep their existing selection.
The repair uses the public function table only; it needs no diagnostic ioctl helper,
private NVIDIA structure offsets, additional RM dependency or RM metadata change.
Presentation, rotation, cursor lifetime and parked-VRAM behavior are unchanged.

## Validation

The earlier startup-selector test used shared controllers and selected 1080p75
after rejecting the preferred timing. On 2026-11-16, the user confirmed the RGB8
candidate fixes both DVI output and rotation with the HDMI monitor still working
on RTX 2060 SUPER / Tahoe 26.7.1. The loaded binary identity was verified. Boot
logs show RGB8 at 1920×1080 with a 174.57 MHz pixel clock, successful later
modesets, and no prior-completion faults or modeset refusals during the test.
Four-head independent rotation, other DVI adapters, sleep/wake, Sequoia and
other GPUs remain unqualified.

`python3 tools/test_kapi_dvi_rgb.py --ogkm <pristine-NVIDIA-source>` applies the
actual KAPI patches and checks the production validators and full modeset
converter under ASan/UBSan. It covers DVI RGB8, all YUV420 dispositions,
validation/commit consistency, other connectors and query failures. Negative
controls verify that removing either the disposition filter or modeset policy
fails the test. Both isolated kernel-link cases, with and without optional
components, passed on Tahoe.

`python3 tools/test_dvi_modes.py` builds the production selector and extracts
the startup, EDID matching and refresh-publication methods for ASan/UBSan checks.
It covers the recorded timing rejection, native-resolution preservation, both
validators, nearest refresh, unchanged valid modes, enumeration errors/bounds,
other resolutions and HDMI/DP/LVDS/analog isolation. Use `--ogkm` or `OGKM` to
select the NVIDIA source headers.

Runtime properties are `NVRMDviModeSelectionVersion=1`,
`NVRMDviModeFallbackApplied` and, when used, `NVRMDviReplacementModeIndex`.
The startup selector lives in NVRMFB; changes to that selector require rebuilding
NVRMFB. The RGB8 policy above lives in NVRM/NVKMS.
