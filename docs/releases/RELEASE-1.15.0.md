# 1.15.0

The last release for macOS 15 Sequoia. It fixes the black screen on GTX 16 cards that started in 1.14, and collects
the fixes since 1.14.7.

If something doesn't work, see [Troubleshooting](../TROUBLESHOOTING.md) and use Send logs.

## Driver

- Fix black screen with only a cursor on GTX 16 and other cards without Resizable BAR (regression in 1.14)
- Fix Unity 6 games hanging on the first frame (Vampire Survivors and others)
- Fix windows and web pages showing old content after a redraw
- Fix high WindowServer CPU and low frame rate at 4K after a few minutes of use
- Fix slow scrolling (window copies are up to 35x faster)
- Fix some monitors being shown as a 30-inch TV in About This Mac
- Fix the 165 Hz mode missing on some monitors (since 1.14.2)
- Fix a display staying dark after sleep; it is now retried on the next wake
- Allow Chrome, Edge and Electron apps to use the GPU with their sandbox on
- Add NVIDIA T400, T600, T1000 and the T-series laptop GPUs (#132, thanks @TararaDotJoshua)

## Mac app

- Ask for access to USB sticks instead of failing with "no OpenCore config"
- Say when the boot stick is unplugged
- Recover from an interrupted install instead of stopping at "installer guard already exists"
- Download from the NullMoth mirror when GitHub is blocked
- Send logs: always finish, keep the start of each boot, and stop re-sending old logs

## 1401 for Windows

- Fix the build stopping on ACPI tables that declare a method twice (BIOSTAR B650 and similar)

## Docs

- Grammar fixes in the 1.11 and 1.13 release notes (#128, #129, thanks @lunar-me)
- Translator test fix (#172, thanks @modesttao)

## Known issues

- WebGL in Chrome, Edge and Discord runs on the CPU by default. It corrupted pages on some RTX 30 cards in 1.14.5.
  To try the GPU path, start the app with `NVMTL_ANGLE_METAL=1` and tell us what you see.
- Ghost or black areas after windows overlap on cards with a BAR of 4 GB or more (#127)
- Chrome decodes video on the CPU while its GPU sandbox is on (#175)
- Thunderbolt eGPUs: the display does not come up
- Laptops with the screen on the Intel GPU: apps can use the NVIDIA card, the built-in screen is not accelerated
- No HDMI/DisplayPort audio; plugging in a monitor while macOS is running needs a restart of WindowServer
