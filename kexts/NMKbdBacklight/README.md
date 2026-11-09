# NMKbdBacklight

An opt-in VirtualSMC plugin that gives a laptop's keyboard backlight to macOS the way an Intel MacBook does: through the
SMC keys `LKSB` and `LKSS`, which CoreBrightness reads and writes. Off unless the boot argument `nmkbl=1` is present.

## What it covers

| Laptop family | How the light is reached | State |
|---|---|---|
| ASUS | ACPI WMI device (`PNP0C14`, `_UID` "ATK"), method `WMNB`, device id `0x00050021`, methods `DSTS` and `DEVS` | Built. Read from ASUS DSDTs sent in by users: 33 of 648 tables handle the device id, all with OEM id `_ASUS_` |
| Lenovo IdeaPad, Legion, LOQ | `VPC0.KBLC` on the `VPC2004` device. The method takes a packed command whose capability bits come from the embedded controller (`KBGC`) | Not built: the packing of the set command is not documented outside Lenovo and the table alone does not give the level encoding |
| Lenovo ThinkPad | `HKEY.MLCG` / `MLCS` | Not built: present in the tables, argument layout not confirmed |
| MSI | embedded controller fields (`KBBL`, `KBL0`), written directly | Not built: a write to controller memory with no method behind it |
| HP, Dell, Acer, Razer | HP and Dell expose vendor WMI buffers whose layout differs per model; no keyboard-backlight method was found that takes a plain level | Not built |

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
