# Experimental automatic primary GPU selection

`NVPrimarySelector.kext` identifies the NVIDIA GPU that owns the firmware console
before NVRM can move its BAR1. It requires the whole console image to fit in a
BAR1 aperture whose base agrees with live PCI configuration. It reads existing
metadata and does not open the PCI provider, map registers, size BARs or start RM.

The module retries an invalid registry traversal or a missing/ambiguous owner
for at most 30 seconds. A unique owner receives `nullmoth-auto-primary = true`.
NVRM and NVAccel require that property in their `IOPropertyMatch`, so they attach
to the same card. The other cards remain visible without being accelerated.
Selection is frozen before requesting matching; it is not recomputed after BAR
relocation. `nvselector=1` enables the marker. Without it, the module only records
the selection in I/O Registry and does not activate the new match.

## Build and initial qualification

Build using the same kernel SDK as the other kexts:

```sh
build/build_primary_selector.sh /private/var/tmp/primary-build
```

For MacKernelSDK, set `KHDR` to its `Headers` and `KLIB` to `Library/x86_64`.
The result is `NVPrimarySelector.kext`. Release packaging must place that signed
bundle under `Library/Extensions` and include it in `SHA256SUMS`.

Before enabling automatic matching, keep a working boot config and complete
driver/runtime/KC backup. Test the selector in read-only mode with `-nvoff` and
without `nvselector=1`; check that it uniquely identifies the known display GPU.
Then opt in to installation using `NM_PRIMARY_GPU=1`, retaining `-nvoff` until
the rebuilt kernel collection and metadata have been verified. Enable
`nvselector=1` and remove `-nvoff` only for the qualified boot configuration.
The installer does not modify OpenCore or perform this initial activation.

The installer detects an existing automatic match on both NVRM and NVAccel and
reapplies it to the new package's metadata. It retains a previously installed
selector when the incoming package does not ship one; otherwise it uses the
packaged selector. It checks the selector signature and inclusion in both test
and live collections. Both per-macOS NVAccel caches are re-signed and their
checksum manifests regenerated. A partial installed match stops before live
replacement. Backup, rollback and removal include the optional selector.

The archive and its official checksum manifest are not edited. Matching changes
are made to private staged bundles after package integrity verification. The
release's code/data sections stay unchanged; code signatures are regenerated.

## Hardware evidence and limits

The standalone selector and property matching booted with release 2.1.0 on macOS
15.8.1 (24H32), a Supermicro H12SSL-i, one EPYC 7542 and four RTX 4090s. One of four
cards uniquely owned the console. NVRM and NVAccel each had one active instance,
on the connected `C2:00.0` card, with automatic-property matching rather than a
hardcoded PCI address. The desktop reported RTX 4090, 24 GB, Metal 3 and
3840×2160 at 60 Hz. The other three cards had neither the primary marker nor an
RM claim. The loaded UUIDs matched all four official 2.1.0 components.
No crash file was collected.

This establishes startup display selection on that topology. Moving the cable
or card, sleep/wake, long-running graphics workloads and other machines are
unqualified. It does not implement multi-GPU acceleration or runtime switching.
The update-preservation changes have private installer fixture coverage; that
exact upstream installer path has not been used to upgrade the test machine.
macOS 26 is not hardware-qualified by this report. Raw user logs are not included.

The installer side lives in the driver's `package/install.sh` and
`package/uninstall.sh`; the 1401 application is unchanged. The installer tests
below run against mocked system commands. Building and signing the selector,
and a run on a multi-card machine, are separate checks.

Run the policy and installer tests:

```sh
clang++ -std=c++17 tools/test_primary_gpu_range.cpp -o /tmp/primary-range
/tmp/primary-range
python3 tools/test_primary_gpu_install.py
```
