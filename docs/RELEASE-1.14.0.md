# NullMoth NVIDIA driver and Mac app 1.14.0

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

Every fix below came from the logs, chats and GitHub issues you sent. For each: what was wrong, and what changed.

## Display
- **Black screen with only the cursor about a minute after start-up** (issues #16, #26, and many reports in chat). One
  minute after start-up the driver switched to showing frames straight from the card's memory. On a PC whose desktop is
  drawn on the Intel or AMD integrated GPU, or whose card has the small 256 MB BAR window, that memory never held the
  desktop. The driver now keeps the copy path on those machines, which is what `-nvrmnoflip` did for everyone who used it.
- **4K at 120 Hz and other fast modes black or offline** (#77). Modes faster than DisplayPort 1.4 can carry need
  compression (DSC). The driver now tells macOS the compression settings, so macOS stops judging those modes impossible.
- **The monitor kept the last frame when the display slept** (#90). The display now turns off, and on again on wake.
- **No picture after switching the monitor to another input and back** (#110). The driver only brought up displays it had
  never seen; a known display that comes back now gets its picture again.
- **Black screen after about 100 seconds on older Intel boards without Resizable BAR** (B150 and similar). Their firmware
  does not list a 64-bit PCI range, so the driver found no room for the card's memory window. On Intel CPUs it now uses
  the range every Intel board from 6th gen on decodes (fix found and tested by a community member).
- **Small-BAR cards refusing graphics memory** (#27, #28). The memory the driver hands out grows with the BAR window instead
  of a fixed 192 MB, and the boot screen's region no longer blocks the last megabyte.
- **The NullMoth boot logo** replaces the Apple logo while macOS starts and stays through the hand-over to the driver. On by
  default; Mac app → Boot logo off, or `-nmlogooff` for one start.

## macOS 26 (Tahoe)
- **Black desktop on macOS 26** (#51). Every package since 1.1.0 carried an out-of-date macOS 26 accelerator that could not
  talk to the current framebuffer. It is now built from the same source as the macOS 15 one.

## Apps
- **Windows and images going black or stale until clicked; Preview showing blocks** (#73, #74, #8). An image's contents
  were marked up to date before the GPU drew them (community patch, #98). 3D-texture copies now copy every layer (#84).
- **Photos face detection crashing (mediaanalysisd)** (#107). A Metal object was released after it was freed.
- **"Your browser does not support WebGL"**. Shaders that read the sample count failed to translate.
- **Hardware video encoding on RTX 5070 Ti and similar cards** (#105). The encoder only tried the card's first encoder
  engine, which those cards do not have; it now uses the first one the card has.

## Installing
- **Quadro T1200 and other Turing workstation cards refused as "older than the driver supports"**. Every Turing or newer
  card is accepted now.
- **The wrong OpenCore partition edited** when the macOS disk holds two (#81).
- **Start once without the driver**: a new boot-picker entry starts macOS or Recovery one time without the driver.
- **The app updates itself**: Updates → Check for updates finds a newer app too, and Update the app replaces it in place.
- **Send logs** now records every GPU of any brand (gpu-all.txt), so GPUs the driver does not cover yet can be built.
- **The display recorder never started** (#108), so Send logs and Record the flicker had no display trace. It now waits
  for the driver at start-up. The kernel log in Send logs is no longer filled by one line repeated every frame (1401 #16).

## Still open
- HDMI and DisplayPort audio from the NVIDIA card.
- Intel Iris Xe / Arc and AMD Ryzen AI / RX 7000+ acceleration: run 1401 Probe 2.1 on those machines to help.
