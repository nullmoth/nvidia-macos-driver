# NullMoth NVIDIA driver and Mac app 1.15.0

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

The last release for macOS 15 Sequoia. It gathers every fix since 1.14.7, the community pull requests that were ready,
and a full review of the code.

## Fixed
- **GTX 16 and other cards without Resizable BAR (regression since 1.14).** Memory the driver set aside next to the
  screen counted against the small 256 MB window, so windows were refused memory: black screens with only a cursor,
  and flicker, on cards that worked on 1.13. That memory no longer counts against the window.
- **Games built with Unity 6 hung on their first frame** (Vampire Survivors and others). Fixed since 1.14.5; included.
- **Windows and pages keeping old content.** Copies of a window's pixels could be taken before the GPU had finished
  drawing them, and parts of a window outside the redrawn area could be lost.
- **Surfaces that were never released** filled a table after a few minutes and pushed every frame through the CPU
  (high WindowServer CPU, low frame rate at 4K).
- **Slow scrolling** from slow copies of window contents (up to about 35 times faster copies).
- **Monitors shown as a 30-inch TV** in About This Mac and treated as one.
- **A 165 Hz mode missing** on some monitors since 1.14.2.
- **Display sleep and wake**: a display that fails to wake is retried on the next wake instead of staying dark.
- **Installing on a USB stick macOS will not let 1401 read**: 1401 now asks for access, and says exactly where to turn
  it on (Privacy & Security, Files & Folders, Removable Volumes) instead of "no OpenCore config".
- **"installer guard already exists"** after an install was interrupted: the interrupted install is rolled back and
  the new one goes ahead.
- **AMFIPass could not be downloaded** on networks that block GitHub: downloads fall back to the NullMoth mirror (the
  same files, checked against the same checksums).
- **The boot stick unplugged**: setup now says so instead of "no OpenCore config".
- **Send logs**: finishes on every machine (it could run out of time and drop the most important log), keeps the start
  of each boot, and no longer sends the same old logs again with every report.
- **Chrome, Edge and Electron apps**: the GPU can start inside their sandbox. WebGL on the GPU is still opt-in (below).
- 1401 for Windows: an ACPI table with a method declared twice (BIOSTAR B650) no longer stops the build.
- New supported cards: NVIDIA T400, T600, T1000 and the T-series laptop GPUs (community contribution).

## Still open
- **Chrome / Edge / Discord WebGL on the GPU is opt-in.** It was clean in testing on an RTX 5060 but corrupted pages on
  some RTX 30 cards in 1.14.5. If you want to try it, start the app with `NVMTL_ANGLE_METAL=1` and report what you see.
- Windows with a GPU memory window of 4 GB or more can show ghost or black areas after windows overlap (#127).
- Thunderbolt eGPUs: the display does not come up (the memory window is too small); being worked on.
- Laptops whose screen is on the Intel GPU: the NVIDIA card works for apps; the built-in screen is not accelerated.
- HDMI / DisplayPort audio from the NVIDIA card; plugging in a new monitor while macOS runs.
