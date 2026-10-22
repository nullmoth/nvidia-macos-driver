# Selecting one NVIDIA GPU on a multi-card machine

An opt-in automatic selector is described in [AUTO-PRIMARY-GPU.md](AUTO-PRIMARY-GPU.md),
including its four-card boot result and the limits of its installer test coverage.

This is a manual workaround verified with driver **1.14.7**, macOS **15.8.1 (24H32)**,
a Supermicro H12SSL-i, one EPYC 7542, and four RTX 4090s. The monitor was connected
to `C2:00.0`. It does not add support for accelerating all four cards.

## Check what failed

Run `sysctl kern.bootargs` to check the arguments used for the current boot.
Removing `-nvoff` from an OpenCore config does not prove that the running system
used that config. Next check `kmutil showloaded --list-only` for all four components:
`com.nullmoth.NVRM`, `NVAccel`, `NVRMFB`, and `NVRMAGDC`.

On the affected machine, unrestricted matching produced overlapping GPU startup
attempts. C2 reached `RmInitAdapter succeeded`, but display setup failed. NVRM's
`gNVRMService` and NVAccel's `gAccel` are single global owners, so allowing several
instances is a suspect when the adapter that initialized and the display adapter
disagree. Restricting both drivers to the connected card resolved this machine's
startup/display failure. Other multi-GPU failures can have other causes.

If **none** of the four components loads, investigate the kernel collection and
installation first. Changing the GPU selector cannot fix a collection that is not loaded.

## Match the connected card

Keep a known working OpenCore config with `-nvoff` available from another operating
system, and boot with it while editing the driver. Back up the installed kexts,
their signatures, and `/Library/KernelCollections/AuxiliaryKernelExtensions.kc`.
Keep the driver's original installer backup for a complete rollback.

Read the PCI properties in macOS, rather than using Windows GPU enumeration order:

```sh
ioreg -r -c IOPCIDevice -d 1 -l -w0
```

Identify the card connected to the monitor. On this machine its properties were:

```text
vendor-id: <de100000>       # NVIDIA
device-id: <84260000>       # RTX 4090, 0x2684
pcidebug: 194:0:0           # C2:00.0, bus in decimal
acpi-path: IOACPIPlane:/_SB/S0D0@0/D0A0@30001/D00E@ffff
```

These values are examples, not defaults. Confirm that exactly one PCI device has
both the selected `pcidebug` and the full `acpi-path`. Slot changes can change them.

Add the following `IOPropertyMatch` dictionary inside **both** personalities:

- `/Library/Extensions/NVRM.kext/Contents/Info.plist` → `IOKitPersonalities` → `NVRM`
- `/Library/Extensions/NVAccel.kext/Contents/Info.plist` → `IOKitPersonalities` → `NVAccel`

```xml
<key>IOPropertyMatch</key>
<dict>
    <key>pcidebug</key>
    <string>194:0:0</string>
    <key>acpi-path</key>
    <string>IOACPIPlane:/_SB/S0D0@0/D0A0@30001/D00E@ffff</string>
</dict>
```

Use the current release's own property lists and keep its PCI vendor/class filters,
dependencies, and other settings. Review any existing `IOPropertyMatch` before editing.
Re-sign the two modified bundles with `codesign --force --sign -`, verify them with
`codesign --verify --strict`, and keep root:wheel ownership and permissions that
prevent group/other writes. The other cards remain visible as PCI devices.

## Rebuild using permanent installation paths

The official installer already builds its final collection from `/Library/Extensions`.
Follow that rule when making a custom candidate: an isolated preflight collection
can embed `/var/tmp/.../repo/*.kext` in `_PrelinkBundlePath`. Copying that collection
directly into the live KC location was a separate installation error in this test.
The next boot found the temporary paths and loaded none of the NVIDIA components.
Rebuilding from permanent paths restored loading.

Build to a separate output before replacing the live collection. A macOS 15 example,
run with root privileges, is:

```bash
work=$(mktemp -d /var/tmp/nvidia-live-kc.XXXXXX)
args=(--allow-missing-kdk -a x86_64)
if [ -s /System/Library/KernelCollections/SystemKernelExtensions.kc ]; then
    args+=(-S /System/Library/KernelCollections/SystemKernelExtensions.kc)
fi
if [ -f /System/Library/Kernels/kernel ]; then
    args+=(--kernel /System/Library/Kernels/kernel)
fi
kmutil create -n aux --volume-root / "${args[@]}" \
    -B /System/Library/KernelCollections/BootKernelExtensions.kc \
    --repository /Library/Extensions \
    -p /Library/Extensions/NVRM.kext -p /Library/Extensions/NVAccel.kext \
    -p /Library/Extensions/NVRMFB.kext -p /Library/Extensions/NVRMAGDC.kext \
    -A "$work/AuxiliaryKernelExtensions.kc" -z
```

Stop if the command fails. Inspect the output with `kmutil inspect -a x86_64 -A`
and `--show-prelink-info`. Before publishing it, verify all four components, both
C2 matching dictionaries, permanent `_PrelinkBundlePath` values, and any unrelated
third-party kexts that must remain in the collection. Keep the rollback copy intact.
Only after these checks, publish the new collection, remove `-nvoff` from the verified
boot config, and restart. If startup fails, restore the config with `-nvoff` first.

## Verified result and limits

After rebuilding with permanent paths, all four NVIDIA components loaded. I/O Registry
contained one active NVRM instance and one active NVAccel instance, both matching C2.
The log reported `GPU 0000:c2:00.0: RmInitAdapter succeeded!`, and
`system_profiler SPDisplaysDataType` reported RTX 4090, 24 GB, Metal 3, and the connected
Acer XB281HK at 3840×2160, 60 Hz. Framebuffer/accelerator state showed an armed display
and surface updates. The other three RTX 4090s remained visible without acceleration.

Copy-engine queue creation still fell back to a graphics channel, and the runtime
reported LOCAL+mapped allocations using system RAM with an 8 GiB BAR1. This validates
desktop output and Metal device registration, not Blender/editing performance, video
engines, sleep/wake, or long-term stability.

Official updates can overwrite these matching rules. Reapply them to the new release's
property lists and rebuild from the permanent paths. If maintaining the per-OS NVAccel
cache under `/Library/NullMoth/kexts`, keep its matching rule and checksum manifest
consistent too; otherwise an OS-update switch may restore unrestricted matching.
