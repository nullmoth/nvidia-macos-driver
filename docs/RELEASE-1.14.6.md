# NVIDIA driver and 1401 Mac 1.14.6

Maintenance update following 1.14.5.

## Changes

- Firefox WebGL: the NVIDIA Vulkan bridge can allocate its owned read-only handle inside the GPU sandbox. If access to the read-only null device is denied, it uses a fresh anonymous descriptor at EOF. Other paths, access modes and errors retain their normal behavior. No browser sandbox preference is changed.
- Chromium default behavior: withdraw automatic enablement of the NVIDIA ANGLE Metal path after partial-redraw corruption reports on Ampere and Blackwell. Explicit browser flags and existing feature overrides are preserved. The driver override is now a diagnostic opt-in (`NVMTL_ANGLE_METAL=1`), with rendering corruption still unresolved.
- Driver bundle signing: keep the unchanged runtime licence files in the bundle's Resources directory and require strict verification of the complete bundle signature before packaging.
- Windows recovery download: retry an Apple recovery-service HTTP 403 over certificate-verified HTTPS with the original request and session headers. An HTTPS refusal still stops; image downloads retain their existing transport rules.

## Validation

- RTX 5060, macOS 15.8.1, Firefox 157.0.1: default fresh-profile WebGL and WebGL2 render and read back the expected orange pixel within one 8-bit colour step, with zero GL errors and no lost contexts. The browser's renderer string masks the model; the hardware fixture is an RTX 5060.
- The initial Chrome 155 hardware-path probe passed WebGL readback and short video playback on the RTX 5060, but the fresh support review found partial page/UI corruption on other systems, including another RTX 5060. These narrow probes do not qualify browser composition. The final build preserves the browser's default NVIDIA policy; hardware Chromium composition remains unqualified. Default Chrome WebGL/WebGL2 are unavailable on the test fixture; this update does not claim to restore them.
- Owned-descriptor regressions cover 200 permission fallbacks, EOF behavior, close-on-exec, descriptor cleanup and preservation of unrelated errors.
- Apple recovery metadata is fetched successfully through verified HTTPS with HTTP access deliberately forced to return 403.
- Source regressions, matching payload pins, strict Mac bundle signatures, packaged Windows CI and public-content scans gate publication. Validation receipts accompany the assets.

## Remaining limits

Chromium hardware page composition, live refresh-rate changes, small-BAR1 redraw failures and machine-specific early boot failures remain under investigation. This release adds no GPU-model, macOS 26, DRM or universal application qualification. Kernel binaries and firmware are unchanged from 1.14.5.

## Updating

Update both 1401 apps to 1.14.6. Install the matching Mac driver and restart as instructed. Send logs from the updated app if a failure remains.
