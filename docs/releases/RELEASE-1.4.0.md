# 1401 and NVIDIA driver 1.4.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Driver / Mac
- **Screen flicker at login is cleared automatically.** The first time the desktop comes up after a boot, the driver gives the compositor a clean restart, so the strobing many people saw at the login screen and first minute is gone. (Opt out by creating the file `/Library/NullMoth/no-wsreset`.)
- If your screen still flashes when you change refresh rate or use two monitors, **Send logs** — 1.4 captures exactly what each display is doing (its refresh rate and the driver's frame timing) so it can be fixed for your setup.

## Windows (1401 1.4.0)
- **PCs whose ACPI tables crash the disassembler build anyway.** Some firmware made the tool crash outright; 1401 now records the crash and keeps the firmware table so the build continues and the exact table can be looked at.
- **Partial hardware scans are accepted.** A scan missing the USB or Storage Controllers section no longer gets the whole report rejected; the build continues.
- Carries everything from 1.3: stick-write and erase fixes, the EXITBS boot-freeze ladder, basic display for unsupported-only PCs, and resume-from-last-byte downloads.

## Everything from 1.3 still applies
16 GB RTX 50 black-screen fix, hot-plug monitors, Blender, Geekbench OpenCL, macOS 15 and 26 installer, pre-Turing cards left alone.

## Scope
macOS 15 Sequoia. Requires OpenCore.

## Known issues, being fixed from your logs
- Two displays or above 60 Hz can still flash on a refresh-rate change until the automatic restart; Send logs if it persists.
- Glitches in local video played with hardware decode (QuickTime); a slow cursor on some laptops.
