# NMKbdBacklight

An opt-in VirtualSMC plugin that gives a laptop's keyboard backlight to macOS the way an Intel MacBook does: through the
SMC keys `LKSB` and `LKSS`, which CoreBrightness reads and writes. Off unless the boot argument `nmkbl=1` is present.

## What it covers

Matched by what the machine's ACPI tables carry, not by vendor name. Counts are from 648 DSDTs in user uploads.

| Laptop family | How the light is reached | Tables | State |
|---|---|---|---|
| ASUS | WMI device (`PNP0C14`, `_UID` "ATK"), `WMNB`, device id `0x00050021`, methods `DSTS` and `DEVS` | 33 | Built. Read from a real ASUS table: `WMNB`, `GLKB`/`SLKB`, level in the low 7 bits, `0x80` = on |
| Lenovo IdeaPad, Legion, LOQ | device `VPC2004`: `KBLC` for three-level lights, `HALS`/`SALS` for plain on/off ones | 44 with `KBLC`, 54 with `HALS`/`SALS` | Built from the public ideapad-laptop driver's documented encodings, written again. Checked against a real table: its `HALS` does not set the keyboard-light support bit, so the plugin correctly finds no light on that model |
| Lenovo ThinkPad | `HKEY` (`IBM0068`, `LEN0068` or `LEN0268`): `MLCG` and `MLCS` | 0 | Built from the public thinkpad_acpi driver's documented encodings (present = bit 9, level = low two bits). No ThinkPad table was in the uploads, so it has never met a real one |
| MSI | embedded-controller fields (`KBBL`, `KBL0`), written directly | 27 | Not built: a write to controller memory with no method behind it, at a per-model address (the public msi-ec driver keeps a table per model). I did not read that table |
| HP, Dell, Acer, Razer | HP and Dell use vendor WMI buffers (Dell's keyboard light goes through SMBIOS calls), Acer's through its own WMI | 33 HP, 31 Dell, 29 Acer | Not built. I did not read the HP, Dell or Acer drivers, and no Razer table was in the sample |

The plugin stands aside when VirtualSMC's `SMCLightSensor` is loaded and the machine has an ambient-light device with a
working `_ALI`, because that plugin already registers `LKSB` and `LKSS`; `nmkblforce=1` registers anyway.

## How macOS finds a keyboard backlight (read from a real Intel Mac Pro on macOS 26.7)

Two routes exist in the Intel build of macOS, and a PC's SMBIOS model decides which one applies:

1. SMC route, for Macs without a T2 chip. `AppleLMUController` (it matches the ambient light sensor, ACPI0008) writes the
   SMC key `LKSB`. Its `setLEDBrightness` packs a 12-bit brightness as `((v & 0xF) << 12) | ((v >> 4) & 0xFF)`, little-endian,
   and it needs the SMC to report mode `A` (`RMde`) and the ambient-light keys, which VirtualSMC provides. This plugin is
   that route's other end: it receives the `LKSB` write and forwards it to the laptop.
2. HID route, for T2-era Macs, which are every Intel Mac that macOS 26 supports. The HID keyboard driver
   (`AppleHIDKeyboardEventDriverV2::initKeyboardBacklight`) scans the keyboard's HID elements for usage page `0xFF00`
   (65280), usage `0x0F`, and talks to it with feature report id `0xBF` (191). CoreBrightness then finds the backlight as an
   HID service with a primary usage page and usage, not through the SMC. The SMBIOS models 1401 picks (MacPro7,1,
   iMac20,x, MacBookPro16,x) are T2-era models, so this is the route macOS 26 takes for them.

This plugin implements route 1 only. Route 2 needs a virtual HID keyboard that carries the vendor element and answers
report `0xBF`; the report's byte layout is not yet read, so it is not built. It is the missing piece for the PC's
macOS 26 slider to work, and route 1 alone should not be expected to move the slider on a T2-era model.

## What is unverified

* Whether CoreBrightness on a T2-era SMBIOS ever looks at `LKSB` (see route 2 above).
* VirtualSMC's `SMCLightSensor` also registers `LKSB` and `LKSS` as plain stored values; which registration wins when both
  load has not been run.
* Whether the ASUS method behaves as the table reads on every ASUS model (it was read from one table and matched by name
  and device id in 33).

## Build check

`build_check.sh` fetches Lilu and VirtualSMC (both BSD-3-Clause) and Apple's kernel headers (MacKernelSDK) at pinned
revisions into a scratch folder and compiles the plugin against them. `tools/test_kbl_core.c` tests the parts that do not
touch the kernel.

## Credits

The plugin follows the structure of VirtualSMC's own `SMCLightSensor` plugin and builds against Lilu and the VirtualSMC SDK,
all by acidanthera and all BSD-3-Clause. Build check revisions: Lilu 0249c3a, VirtualSMC b5c62c2, MacKernelSDK 3f75008.
The ASUS interface was read out of ASUS DSDTs with Intel's ACPICA disassembler; no ASUS or Linux driver code was copied.
