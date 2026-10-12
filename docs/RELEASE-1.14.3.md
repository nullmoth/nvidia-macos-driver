# NullMoth NVIDIA driver and Mac app 1.14.3

**Something not working? [Why a machine does not work, and how to fix it](TROUBLESHOOTING.md) lists every known problem.**

A fix release for 1.14.2. Everything in [1.14.2](RELEASE-1.14.2.md) is included. The driver itself is unchanged.

## Fixed
- **Ryzen 7000/8000/9000 (AM5 and their laptops): macOS could write to the board's firmware memory.** 1401 1.14 turns
  this off for new EFIs, but EFIs made before it left it on, and a restart from macOS froze one AM5 board badly enough
  to need repair. Installing or updating the driver now turns it off on those EFIs too (OpenCore's
  `DisableVariableWrite`, when `OpenRuntime.efi` is loaded). macOS can then no longer change the startup disk by itself;
  OpenCore still sets every setting at each start.

## Still open
- "Start once without NVIDIA driver" can leave the screen dark or frozen.
- Full-screen flicker on some machines: the cause is found and the fix is being tested; it comes in the next release.
- HDMI and DisplayPort audio from the NVIDIA card; plugging in a new monitor while macOS runs.
