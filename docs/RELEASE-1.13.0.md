# 1401 and NVIDIA driver 1.13.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## What was wrong in 1.12, and what changed

- **GTX 16 / RTX 20 (Turing) cards stuck at startup after updating.**
  *Cause:* at startup the driver waits a fixed time for the card's firmware (GSP) to settle before it turns the display on. The default (half a second) is enough for newer cards but not always for Turing, so some GTX 1660 SUPER and RTX 2070 SUPER machines stopped at boot. Users fixed it by hand with the boot argument `nvrmsettle=10000`.
  *Fix:* setup now adds `nvrmsettle=10000` on Turing cards (device IDs 1E00-1FFF and 2180-21FF). If you already set your own `nvrmsettle`, yours is kept.

- **"Remove NVIDIA driver" in the boot picker turned the driver off but never removed it.**
  *Cause:* the removal runs at the next macOS start, before anything has mounted the EFI partition. Installs that recorded their OpenCore config only by its path (`/Volumes/EFI/...`), or whose recorded partition was no longer attached, stopped with "the recorded config is missing" or "could not mount the recorded OpenCore partition". The driver then stayed off but installed, start after start.
  *Fix:* when the recorded config cannot be found, removal uses the OpenCore partition this Mac started from, which is the one whose picker asked for the removal. It finishes and restarts by itself.

- **"Remove NVIDIA driver" in the boot picker hung or closed at once.**
  *Cause:* the tool asks you to press Y before it does anything. OpenCore ran it in graphics mode, so its question never appeared on screen and it sat waiting for a key nobody could see (it gave up after 60 seconds). On other machines the Enter that started it from the picker was still queued, counted as "not Y", and the tool closed straight away.
  *Fix:* the picker entry now runs in text mode, so the question is visible, and the tool ignores keys typed before it asks. Updating with the Mac app switches existing entries and replaces the tool.

- **Crash reports showed crashes from before you updated.**
  *Cause:* the report includes recent crash files, and some were from an older driver. A Live Text crash fixed in 1.12 showed up in 1.12 reports and looked like it was still there.
  *Fix:* every crash in a report now carries its date and says when it happened before the current driver install.

## Still open
- Black screen or flicker above 60 Hz on some monitors: please send logs from the **1.12 or newer** Mac app (it records the display from every boot). Logs from older apps do not include this.
- HDMI and DisplayPort audio from the NVIDIA card: Apple's audio controller only accepts older NVIDIA cards; a runtime fix is being built.

## Scope
macOS 15 Sequoia. Requires OpenCore.
