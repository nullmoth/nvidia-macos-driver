# Security

## Reporting a vulnerability

Report it privately through GitHub: [open a security advisory](https://github.com/nullmoth/nvidia-macos-driver/security/advisories/new).
Please do not open a public issue for a vulnerability. Include the driver version, the macOS version, and the steps that
show the problem. We answer in the advisory thread.

## Where the driver comes from

The only official sources are this repository's [releases](https://github.com/nullmoth/nvidia-macos-driver/releases),
the 1401 app (which downloads from those releases), and [nullmothsystems.com](https://nullmothsystems.com). Copies on
other sites or in other repositories are not ours. The 1401 apps check every driver package and app update against a
detached Ed25519 signature made with the NullMoth release key before they install it, and refuse anything unsigned.

## What the driver changes on your Mac

macOS will not load a GPU driver Apple did not sign unless some of its protections are relaxed, so the setup makes two
system-wide changes:

- **Boot arguments** `amfi_get_out_of_my_way=0x1` and `amfi=0x80` turn off macOS's check of app signatures and the
  entitlements they claim. A program running as you can load code into your other apps, including ones with camera,
  microphone, or Full Disk Access, and can give itself entitlements Apple would normally refuse. `ipc_control_port_options=0`
  relaxes a related protection on task ports.
- **System Integrity Protection** is partly off (`csr-active-config` 0x0A43).

Removing the driver with the Mac app restores what its setup changed. `uninstall.sh` on its own does not change them. While
these settings are on, only run software you trust, as you would on any Mac with these protections off.
