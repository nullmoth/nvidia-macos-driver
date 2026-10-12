# NullMoth NVIDIA driver and Mac app 1.14.1

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

A fix release for 1.14.0. Everything in [1.14.0](RELEASE-1.14.0.md) is included.

## Fixed
- **Some PCs stopped starting after updating to 1.14.0.** The verbose screen stopped right after the NVIDIA card was set
  up, usually just after an `ACPI Warning: ... _DSM: Argument #4 type mismatch` line (seen on desktops and laptops,
  RTX 20 to 40 series). 1.14.0 added laptop panel brightness, which asks the GPU's firmware for its backlight tables
  while the card starts. On many PCs that firmware call is not a backlight table at all, and the start hung there. The
  driver no longer makes that call, as in 1.13. Laptop panel brightness can still be tried with the boot argument
  `nvrmacpibl=1`.
- **"Boot logo on/off" and the "AMD / Intel GPU and NPU driver (test)" switches did nothing** and stopped with
  "package not found". Both work again.
- **Preparing a 1401 desktop for macOS 26 (Tahoe).** Every desktop 1401 built before 1.14 is set up as an iMac Pro
  (iMacPro1,1), which macOS 26 refuses ("not supported on this platform"). Preparing for Tahoe now moves a desktop to
  MacPro7,1, which runs macOS 15 and 26. A laptop on a model macOS 26 dropped is told to make a new EFI with 1401.

## If your PC stopped starting on 1.14.0
At the boot picker choose **1401: Start once without NVIDIA driver**, log in, then Mac app → **Check for updates** → **Update driver**. Restart.

## Still open
- **"Start once without NVIDIA driver" can leave the screen dark** on some PCs (being checked). With 1.14.1 the start
  that needed it works again; if your PC still stops, choose **1401: Remove NVIDIA driver** instead. It works
  even if the screen stays dark: give it a few minutes, it restarts by itself when the driver is removed.
- HDMI and DisplayPort audio from the NVIDIA card.
- WebGL in Chrome (Safari works): Chrome turns its Metal path off on NVIDIA GPUs.
- AMD and Intel GPU acceleration has not run on those GPUs yet; the Intel NPU does not start yet.
