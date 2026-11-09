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
| Dell | the SMBIOS calling interface through the Dell WMI GUID, class 4 select 11 | 31 tables carry Dell WMI methods | Built from the public dell-laptop and dell-smbios-wmi drivers' documented encodings, written again. The buffer size is read from Dell's descriptor block ("DELL WMI" signature, length 4096 or 32768); with no valid descriptor the plugin refuses. Never run on a Dell |
| MSI | embedded-controller byte, by firmware | 27 | Built behind a firmware check, from the msi-ec driver (BeardOverflow/msi-ec, GPL-2.0), which carries the light's setter and getter. The plugin reads the 12-character firmware string at controller address 0xA0 through ACPI's embedded-controller address space and writes only when it is one of the 208 strings that driver gives a light address for; any other firmware is refused. The setter writes the base value OR-ed with the level 0 to 3 and the getter reads the low two bits, exactly as that driver does (a test fails if OR is replaced by addition). Never run on an MSI |
| HP, Acer | the firmware changes the light itself | 33 HP, 29 Acer | Nothing to drive. The public hp-wmi driver lists the light's level changes (level 2 to off, off to level 1, level 1 to level 2) as events it deliberately ignores, and acer-wmi ignores the "automatic keyboard background light toggle" event: the Fn keys work with no operating-system driver because the BIOS changes the light and only tells the OS afterwards. What is missing on macOS is the slider and idle dimming, and neither Linux driver contains a call that sets a level, so there is no encoding to take |
| Razer | a USB HID feature report | none in ACPI | `tools/razer/razer_kbd_backlight.c`, a user-space setter built from OpenRazer's documented 90-byte report, with a test that rebuilds the report from the description. Never run on a Razer: macOS asks for Input Monitoring permission, and which USB interface takes the report is per model |

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

This plugin implements both. Route 2 is a virtual built-in keyboard (`NMKbdBacklightHID`, an IOHIDDevice) whose report
descriptor carries the vendor element 0xFF00:0x0F as an input, a feature report 0xBF that answers `{ version 1, off, on_min,
on_max }`, and an output report 0xB0 that takes `{ version 1, brightness, rate }`; the layouts are the ones the public Linux
hid-apple driver documents and the macOS driver's own calls match. A set report is passed to the laptop through the same
backend as the SMC route. `nmkblhid=0` turns the HID route off.

## How the two routes meet VirtualSMC

Read from VirtualSMC's keystore source: a plugin key that is already in VirtualSMC's own table is an override, and a second
plugin overriding the same key panics; `LKSB` and `LKSS` are not in that table. Two plugins that both add them are merged in
the order they loaded, with the first one found on a read. So the order, not a rule, would decide. This plugin therefore
checks twice, at probe time and again just before it submits its keys, whether SMCLightSensor is loaded on a machine with an
ambient-light `_ALI`; if it is, this plugin gives up only the SMC keys and keeps the HID route, which does not collide.
`nmkblforce=1` registers the SMC keys anyway, and then the plugin that loaded first answers.

## What is unverified

* Whether macOS 26 on a PC, with no T2 chip behind the keyboard, accepts a virtual keyboard as the built-in one and drives its backlight from it. The descriptor, the report layouts and the registry flags follow what the macOS driver was read to expect; the whole route has not run.
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
