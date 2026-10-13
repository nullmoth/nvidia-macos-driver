# 1401 and NVIDIA driver 1.6.0

**Something not working? [nullmothsystems.com/help](https://nullmothsystems.com/help) lists every known problem and exactly what to do.**

## Driver / Mac
- **The driver no longer removes itself a few seconds after login.** On some laptops and boards, a change macOS makes to NVRAM never reaches the firmware, so an old "remove the driver" request from the boot picker came back at every start: the driver installed, macOS ran for a couple of seconds, restarted, and the driver was gone. Installing now hands that leftover request to OpenCore, which clears it before macOS starts.
- **Ryzen with your own OpenCore EFI: the driver starts instead of stopping at `RmInitAdapter failed! (0x25:0x40:1310)`.** If your config has Shaneee's "Fix PAT" patch on and Algrey's off, installing switches them (Algrey's is the one 1401 builds with, and the one that reached the desktop on the reporting machine).
- **Send logs now includes your OpenCore config** (with your serial numbers, board serial, UUID and ROM removed) and the list of kexts and drivers in your EFI, so a problem on your own EFI can be fixed from what is actually in it.

## Windows (1401 1.6.0)
- **Sticks whose serial number changes on every read are accepted.** Some USB sticks (SMI controllers) report a new serial each time Windows asks, so every write stopped with "disk N is not the USB stick you picked any more". The stick now counts as the same one when it is the only USB disk of that exact size and model at the picked disk number.
- **The macOS download keeps going on networks that pause it for minutes.** It now waits out up to about 15 minutes of empty connections (was about 2.5), asks Apple for a fresh download link along the way, and a reset on a resume no longer ends the download.
- **A Linux USB probe report can now be built into an EFI.** If the Windows scan will not run or finish on your PC, upload the probe and support can build your EFI from it.

## Ryzen 7000 / 9000 (AM5)
- If the stick stops with `EB.MM.AKM` / "Couldn't allocate", restart and pick the installer again 2–3 times before rebuilding — on AM5 this is often random from one start to the next.

## Scope
macOS 15 Sequoia. Requires OpenCore.
