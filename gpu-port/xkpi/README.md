# LinuxKPI on macOS: Linux GPU kernel drivers for AMD and Intel GPUs

This tree runs Linux's GPU kernel drivers inside a macOS kext, the way FreeBSD runs them:

- **LinuxKPI** (FreeBSD's Linux kernel API, BSD licence) provides the Linux interfaces.
- **drm-kmod** (Linux DRM, amdgpu, i915 and TTM, adapted to LinuxKPI, MIT licence) provides the drivers.
- **This tree** supplies the FreeBSD kernel API that LinuxKPI is written against, implemented on xnu.

## Layout

| Path | Contents |
|---|---|
| `bsdcompat/include` | FreeBSD kernel headers implemented on xnu: locks, sleep queues, callouts, taskqueues, malloc(9), sysctl, sbuf, VM pages, PCI, bus resources, time. The pure BSD headers (tree, bitset, ctype, hash, ck, pcireg, fbio) are FreeBSD's own copies. |
| `bsdcompat/include/nmxkpi_prelude.h` | Included first in every file. It pulls in the xnu headers before any Linux macro exists. |
| `override/include` | The few LinuxKPI headers that must differ on macOS (see below). |
| `cc.sh` | Compiles one source. |
| `gen_flags.py` | Writes `amd.flags`: CONFIG_* defines and include paths, read from drm-kmod's own Makefiles. |
| `srclist.py` | Lists the sources drm-kmod's Makefiles build for a module, evaluated against the same KCONFIG. |
| `all.sh` | Compiles the whole AMD render path in parallel and ranks the remaining errors. |

The `override/include` headers:

- `linux/list.h`: without FreeBSD networking.
- `linux/compiler.h`: `counted_by` off; Xcode clang cannot take `typeof` of such arrays.
- `linux/dma-buf.h`, `linux/sync_file.h`, `linux/dma-fence-unwrap.h`: written for this port. drm-kmod's versions are GPL.

## Licences

- LinuxKPI and the FreeBSD headers are BSD.
- amdgpu and the DRM core are MIT, or GPL-2.0 OR MIT.
- ACPICA headers are used under their BSD-3-Clause option.

GPL-only files are excluded and replaced by this port's own code: `drm_gem_ttm_helper`, `drm_writeback`, `drm_hdcp_helper`, `sync_file`, `dma-fence-unwrap`, and drm-kmod's `linuxkpi/gplv2` headers.

## Status (2026-10-11)

All 428 sources of the AMD render path compile for the macOS kernel (x86_64): amdgpu core without the display core, plus DRM core, GPU scheduler, TTM and dma-fence.

22 of LinuxKPI's runtime files compile unchanged.

Next: the runtime pieces that are xnu-specific:
- threads and `current`
- page allocation
- PCI driver binding
- the device-node file layer
- shmem
- DMI and ACPI

Then link the kext and run its self-tests.
