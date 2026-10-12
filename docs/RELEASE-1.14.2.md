# NullMoth NVIDIA driver and Mac app 1.14.2

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

A fix release for 1.14.1. Everything in [1.14.1](RELEASE-1.14.1.md) and [1.14.0](RELEASE-1.14.0.md) is included.

## Fixed
- **Black login window on high refresh rate monitors** (seen on 2560x1440 at 180 Hz over DisplayPort). The monitor
  stayed on but showed nothing until you typed your password blind; after login the desktop was normal. 1.14.0 started
  listing every resolution the monitor supports, including ones smaller than its own, and the login window came up
  black with them. Only resolutions larger than the monitor's start-up resolution are added now (the 4K fix from 1.14.0
  stays); smaller desktops still come from macOS's own scaled modes. Thanks to the user who tracked it down to the
  exact change.
- **"Start once without NVIDIA driver" and "Remove NVIDIA driver" left the screen black.** The installed EFI turns off
  macOS's own firmware display so it cannot take the screen from the driver; with the driver off, nothing lit the screen.
  With the driver off, the driver now shows the firmware's picture itself (one resolution, no acceleration), so you can
  log in, use Recovery, or watch the removal.
- **"AMD / Intel GPU and NPU driver (test)" stopped with "downloading the GPU firmware failed (network?)".** The
  firmware was fetched one file at a time (about 700 files for an AMD GPU) and both firmware servers started refusing
  (HTTP 429 and 503). It now comes in one download per firmware folder, and every file is still checked.
- **System windows such as the password prompt (SecurityAgent) and the Installer could crash** on some RTX 40 PCs when
  the GPU reported an unexpected value at start-up: the Vulkan layer stopped the whole app. Now that app just runs
  without GPU acceleration, and the value is logged.

## If you are on 1.14.0 and your PC does not start
1.14.1 and 1.14.2 fix that, but 1.14.0 has to be replaced first. At the boot picker choose **1401: Remove NVIDIA driver**
and press Y. The screen can stay dark while it works (that is the second fix above, which your 1.14.0 does not have
yet): give it a few minutes, it restarts by itself when the driver is removed. Then install 1.14.2 from the Mac app.

## Still open
- HDMI and DisplayPort audio from the NVIDIA card.
- WebGL in Chrome (Safari works): Chrome turns its Metal path off on NVIDIA GPUs.
- Plugging in a new monitor while macOS runs.
- AMD and Intel GPU acceleration has not run on those GPUs yet; the Intel NPU does not start yet.
