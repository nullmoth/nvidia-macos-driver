# 1.14.7

Driver updates no longer re-enable the optional EFI boot logo. New Windows installer builds omit it, and Mac driver installation disables an existing logo entry. The logo remains available for explicit testing.

Mac installation preserves the existing OpenCore GPU BAR and kernel-placement settings. Fixed resizing values from a different board could prevent a previously working configuration from booting.

The Mac updater now reports installation, cancellation and failure after the download completes, instead of leaving the download message visible. The main device table shows the installed driver package version separately from the app version. The loaded-component check counts the four NVIDIA components explicitly, so unrelated Wi-Fi, DRM or optional GPU kexts cannot make a partial installation appear complete. A restart is still required to load newly installed kernel components.

These are installer and app repairs. Kernel and graphics runtime payloads remain unchanged from 1.14.6. High-refresh display switching, small-BAR surface failures, Chromium hardware composition, sleep/wake failures and machine-specific boot or panic reports remain open. This release does not qualify every macOS 15 system or announce official Tahoe support.
