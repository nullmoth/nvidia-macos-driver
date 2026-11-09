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

## What is unverified

* Which of the two bytes of `LKSB` carries the brightness. VirtualSMC defines the key and the two-byte type and does not
  say. The first byte is treated as 0 to 255.
* That CoreBrightness on the macOS versions this driver supports drives the backlight from `LKSB` on a PC at all. On
  Apple silicon it finds the light through an HID device instead.
* VirtualSMC's `SMCLightSensor` also registers `LKSB` and `LKSS` as plain stored values; which registration wins when both
  load has not been run.

## Build check

`build_check.sh` fetches Lilu and VirtualSMC (both BSD-3-Clause) and Apple's kernel headers (MacKernelSDK) at pinned
revisions into a scratch folder and compiles the plugin against them. `tools/test_kbl_core.c` tests the parts that do not
touch the kernel.

## Credits

The plugin follows the structure of VirtualSMC's own `SMCLightSensor` plugin and builds against Lilu and the VirtualSMC SDK,
all by acidanthera and all BSD-3-Clause. Build check revisions: Lilu 0249c3a, VirtualSMC b5c62c2, MacKernelSDK 3f75008.
The ASUS interface was read out of ASUS DSDTs with Intel's ACPICA disassembler; no ASUS or Linux driver code was copied.
