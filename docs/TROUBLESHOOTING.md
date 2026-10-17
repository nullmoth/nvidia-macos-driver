# Why a machine does not work, and how to fix it

Every entry below comes from real logs people sent. Find the place your machine stops, read why, and do the fix. If yours
is not here, press **Send logs** (Mac app) or **Scan for logs and send them** (1401 for Windows) and post the report ID.

Before anything else: **use the newest 1401 and driver.** Most problems below are already fixed in a later version, and a
stick made by an old 1401 keeps the old problem until you make it again.

---

## 1. Making the stick (1401 for Windows)

| What you see | Why | Fix |
|---|---|---|
| "HTTP Error 502: Bad Gateway" from osrecovery.apple.com | A proxy or filter on your network breaks plain HTTP to Apple. | Fixed in 1.14 (1401 retries over HTTPS). Before that: a phone hotspot or another network. |
| "stream ended at 1048576 bytes", "HTTP Error 403/405", "could not reach osrecovery.apple.com" | Something between you and Apple cut the download: antivirus web scanning, a filtered network, or Apple refusing that network. | Turn off antivirus web scanning, or use a phone hotspot or a VPN, and press Next again. Or download `BaseSystem.dmg` and `BaseSystem.chunklist` with macrecovery on any computer and put them in the 1401\Recovery folder. |
| "EFI failed validation: ValidatorUnavailable: Windows application control blocked …" | Windows Smart App Control would not run OpenCore's validator. | Fixed in 1.14: 1401 checks the config against OpenCore's own sample instead. |
| "The only graphics card Windows reports is '…', with no maker or PCI ID" | Windows has no driver for the card, so the scan could not name it. | Fixed in 1.14 when the card's name says the maker (it builds with basic display). Otherwise install the card's Windows driver, restart, and run Check this PC again. |
| "Windows error 433", "the stick stopped accepting writes", "the disk is read-only", "could not erase the stick (diskpart)" | The USB stick or its port: a loose port, a failing or fake stick, or a lock switch. | Use a USB port on the back of the PC, another stick, and slide any lock switch off. |
| "this stick changed the macOS image after it was written" | The stick did not keep what was written (a failing or fake-capacity stick). At start-up this showed as "OCB: DMG has been altered". | Use another USB stick. 1.14 reads the image back to catch this before you boot. |
| "This PC has not been checked yet", "The build has no verified machine profile", "The scan changed after the EFI was built" | The steps ran out of order. | Run **Check this PC**, then **Build**, then **Create the stick**, in one go. |
| "VMware SVGA 3D" is the only graphics card | 1401 is running inside a virtual machine. | Run 1401 on the PC itself. |

## 2. Starting the stick or macOS

| What you see | Why | Fix |
|---|---|---|
| `EB.MM.AKM`, `STOP 0x16`, "StartImage failed - Aborted", "Couldn't allocate runtime area" (often Ryzen 7000/9000) | The firmware's memory map leaves boot.efi no room for macOS. | Make the stick again with the newest 1401: it builds Ryzen 7000/9000 with the right memory settings and, if one setting fails, tries the next. Restarting an old stick changes nothing. |
| Stops at `EXITBS:START` | macOS took over from the firmware and stopped before printing anything. On Intel 12th gen and newer and on AMD laptops this is usually the CPU core layout or the memory map. | Newest 1401 first. Then send the stick's `opencore-*.txt` (1401 offers to send it when the stick is plugged in). If a kernel panic shows, take a photo of the whole screen in verbose mode. |
| "This version of Mac OS X is not supported on this platform!" with a `Mac-…` code | The Mac model in the config is not one that macOS version installs on. macOS 26 refuses iMacPro1,1, and old models are refused by macOS 15. | 1.14 builds desktops with a graphics card as MacPro7,1, which runs both macOS 15 and 26. On your own EFI, 1401's "Check your EFI" names the problem. |
| The macOS install freezes about 19 minutes in (Caps Lock does nothing), Ryzen 7000/8000/9000 | These boards hang when the installer writes firmware settings. | Fixed in 1.14: make the stick again. On your own EFI: config.plist → Booter → Quirks → DisableVariableWrite = true. |
| The stick's boot hangs right after picking it, sometimes, and works after making the stick again | The stick or port is slow or failing while OpenCore reads the 900 MB recovery image. | Use a port on the back of the PC, or another stick. |
| "OCB: LoadImage failed - Unsupported", "Failed to retrieve boot file from DMG" | The recovery image on the stick is damaged. | Make the stick again on another USB stick (1.14 checks it after writing). |
| "OS.dmg.root_hash … Err(0xE)" | A stick made by an old 1401, or a recovery image for a different macOS. | Make the stick again with the newest 1401. |
| Stops at "PCI configuration" | Usually Resizable BAR or Above 4G Decoding settings. | In the BIOS: Above 4G Decoding on, CSM off. If it still stops, send the stick logs. |
| Loops at `AppleKeyStore` after installing the driver, GTX 16 / RTX 20 (Turing) | The card's firmware needed more time before the driver turned the display on. | Fixed in 1.13 (Setup adds `nvrmsettle=10000` on Turing). |

## 3. After installing the NVIDIA driver

| What you see | Why | Fix |
|---|---|---|
| The desktop shows for about a minute, then black with only the cursor | One minute after start-up the driver switched to showing frames straight from the card's memory. That is blank when the desktop is drawn on the Intel/AMD integrated GPU, or when the card's BAR is the small 256 MB window. | Fixed in 1.14 (the driver keeps the copy path on those machines). Before 1.14: add `-nvrmnoflip` to boot-args. |
| Black or flickering screen at 100–240 Hz, or after changing resolution or refresh rate | Fast modes above DisplayPort 1.4's bandwidth need compression (DSC), and macOS was not told. | 1.14 sends the compression settings. If it still happens, press **Send logs** while it happens: the app records the display path. |
| macOS 26 (Tahoe): black desktop | The macOS 26 version of the accelerator in the package was out of date. | Fixed in 1.14. |
| No picture after switching the monitor to another input and back | The driver only brought up displays it had not seen before. | Fixed in 1.14. Before that: unplug and replug the cable. |
| Black after about 100 s on an older Intel board without Resizable BAR (B150 and similar), log says "cannot hold … not placing" | The firmware lists no 64-bit PCI range, so the card's memory window had nowhere to go. | Fixed in 1.14. |
| The monitor stays on with a frozen picture when the display sleeps | The driver did not turn the display off. | Fixed in 1.14. |
| After system sleep the screen stays black and the Mac needs a hard reset (`Sleep Wake Failure`, "Darkwake Exit Failure") | The driver did not suspend or resume the GPU, so on wake it pushed to a GPU that had lost power. | Fixed by system sleep support ([docs/SLEEP-WAKE.md](SLEEP-WAKE.md)). Before that: keep system sleep off (`sudo pmset -a sleep 0`) and do not use Apple menu → Sleep. |
| After wake the desktop is black with only the cursor, or frozen on the picture from before sleep | WindowServer was never told the framebuffer woke, or its VRAM was replaced at wake. | Fixed by system sleep support. Send logs if you still see it: they hold the `NVRM-xnu: wake #` and `NVRM-fb: system wake` lines. |
| The Mac wakes about every minute while asleep (`pmset -g log`: "DarkWake ... due to XDCI ... XHCI") | The firmware lets USB wake the system (GPE 0x6D). Not the driver. | Rename `GPRW` to `XPRW` in `ACPI → Patch` with `SSDT-GPRW`. Then only the power button wakes the Mac. |
| Windows go black or show an old picture until clicked; Preview shows images as blocks | An image's contents were marked up to date before the GPU had drawn them. | 1.14 includes the fix (community patch). Send logs if you still see it. |
| "Your browser does not support WebGL" | Some WebGL shaders failed to translate. | Fixed in 1.14. |
| Photos / face detection keeps crashing (mediaanalysisd) | A Metal object was freed twice in our driver. | Fixed in 1.14. |
| Hardware video encoding off on RTX 5070 Ti and similar cards | The card has no first encoder engine, and the encoder only tried that one. | Fixed in the encoder (it uses the first engine the card has). |
| "Quadro T1200 / T-series … is older than the driver supports" | The installer's list did not include every Turing workstation card. | Fixed in 1.14: every Turing or newer card is accepted. |
| Kernel panic as the login screen appears (WindowServer, `IOFramebuffer::postOpen`, page fault in `memcpy`), with WhateverGreen installed | WhateverGreen copies the boot screen on the first framebuffer start from an address it saved at boot. macOS can free that mapping before the NVIDIA display opens (seen on a laptop whose boot screen is on the Intel GPU). | Add `gfxrst=4` to boot-args. It turns that copy off; the driver draws its own start-up screen. |
| The app edits the wrong OpenCore partition | Two OpenCore partitions on the macOS disk. | Fixed in 1.14 (the boot arguments decide which one started the Mac). |

### Several NVIDIA cards

If several NVIDIA cards attempt startup, or a custom kernel collection loads none of
the driver components, see [selecting one GPU](MULTI-GPU.md). The workaround was boot-tested
on four RTX 4090s with 1.14.7 and macOS 15.8.1; it restricts NVRM/NVAccel to the display card
and rebuilds the final collection from permanent installation paths.

## 4. Getting back in

- Start once without the driver: pick **1401: Start once without NVIDIA driver** in the OpenCore picker, press Y,
  then choose macOS or Recovery. The driver is back at the next start. (1.14)
- Remove the driver: pick **1401: Remove NVIDIA driver**, press Y, then choose macOS. It removes itself and restarts.
  Removal also puts the saved screen resolution and refresh rate back to a safe default, so the Mac comes up with a
  picture even if the driver had been set to a mode the built-in graphics cannot show.
- If you cannot see the entry: the installer and every update make the OpenCore picker show it — the picker appears
  on start-up, uses the Builtin picker (Apple's own picker never lists these entries), stops hiding entries behind the
  spacebar, and lists both NullMoth entries without pressing Space. Re-run the 1401 app (Install or Repair) once from a
  working start to apply this to a picker built before 1.15. If the entry still does not appear, the app was editing an
  OpenCore partition other than the one that starts the Mac — keep the disk or USB stick the Mac starts from attached,
  and choose its `config.plist` in the app.
- If the screen is black even at the picker, or the Mac starts only from a USB stick you no longer have: boot into
  macOS Recovery (the driver cannot run there), open **Terminal**, run
  `nvram 7C436110-AB2A-4BBB-A880-FE41995C9F82:nullmoth-remove=1`, then restart into macOS. The Mac removes the driver by
  itself, with no picture needed, and restarts clean. This is the same removal the picker entry runs.
- Boot logo: earlier versions could add `NullMothLogo.efi` to OpenCore. It is no longer part of the driver; installing,
  updating or removing the driver takes its entry and file out. If the picker never appears on an EFI that still has
  it, disable that entry in the EFI configuration from another operating system.

## 5. GPUs the driver does not cover yet

GTX 10 and older (Pascal, Kepler), AMD RX 7000/9000, Intel Iris Xe and Arc, and Ryzen AI graphics run macOS with basic
display (no graphics acceleration). To help support them, run **1401 Probe 2.1** on that machine: its GPU test records
what Linux's own driver does on your GPU, which is how support for a new GPU gets built.
