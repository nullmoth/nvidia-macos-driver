# 1401 and NVIDIA driver 1.3.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Mac app
- **Send logs no longer freezes the app.** Collecting runs in the background (a few minutes); the app keeps working. In 1.2.0 the whole app hung while it collected, so most people could not send logs at all.
- **Installing no longer sits at "Finding the OpenCore partition."** The password prompt runs in its own process and always shows.
- **Setup finds the partition your Mac started from** by its serial number, instead of asking you to pick it.
- **"Remove NVIDIA driver" asks first.** In the boot picker it waits for Y; any other key, or 60 seconds, changes nothing. The screen stays on while the driver is off.

## Driver
- **No more black screen / crash loop on 16 GB RTX 50 cards.** Under heavy graphics load the driver could abort the whole desktop (WindowServer) on an RTX 5060 Ti / 5070 Ti. Fixed.
- **A monitor plugged in after you log in now lights up** instead of staying dark until a restart.
- **Blender 4.2 starts** instead of crashing.
- **Geekbench's OpenCL test no longer crashes** on RTX 30 and 40 cards.
- **Pre-RTX cards (GTX 10 and older) are left alone.** The driver only runs cards it supports (RTX 20 / Turing and newer); on an older card macOS keeps running on the firmware's screen instead of going black.
- Sent logs now keep the whole boot (the driver stopped filling the log with routine messages).

## Windows (1401 1.3.0)
- **Apple downloads finish on networks that cut connections.** Some networks close every download after 1 MB; 1401 keeps what arrived and continues from that byte (1.2.0 stopped with "stream ended at 1048576/10485760").
- **Reuse a stick that has macOS or Windows on it** — 1401 names what is on it and asks before erasing.
- **Sticks that fail to erase or format** are handled: the partition table is cleared first, a refused write retries with the disk taken offline, formatting falls back to the built-in formatter, and the stick is found again by serial after erasing (fixes "the requested object could not be found," "Windows error 1," and disks that change number mid-write).
- **PCs that freeze at the hand-off to macOS (EXITBS:START).** With the stick plugged in, 1401 reads its startup logs and steps through the memory settings that get past it, on Intel and AMD.
- **Unsupported-only PCs build anyway.** A PC whose only card is a GTX 10, RX 7000 or Arc now builds and runs macOS on the firmware's screen (basic display, no acceleration) instead of stopping.
- **macOS shows in the OpenCore picker** without pressing Space.

## Scope
macOS 15 Sequoia. Requires OpenCore.

## Known issues, being fixed from your logs
- Two displays, or a refresh rate above 60 Hz: changing a display's mode can make a screen flash until you restart. If this hits you, send logs from the Mac app and post the report ID in the Discord.
- Glitches in the first minute after login, and a slow cursor on some laptops — send logs if you see them.
