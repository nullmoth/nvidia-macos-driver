# System sleep and wake

Before this change the driver had no system sleep support. On a desktop the PCIe slot loses power in S3, so the GPU
came back reset while NVRM still thought GSP-RM was running. On wake NVRMFB re-lit its head through NVKMS, the push to
the core channel spun in `nvEvoMakeRoom` forever, and the Mac needed a hard reset ("Darkwake Exit Failure",
`Sleep Wake Failure` report). The work below makes sleep and wake behave as on Linux, plus the steps macOS needs on top.

Tested on an RTX 5050 (GB207, FSP boot), MacPro7,1 SMBIOS, macOS 15.8.1: repeated sleep and wake with the desktop,
windows and Metal apps intact.

## What happens

| Step | Where | What |
|---|---|---|
| System is about to sleep | NVAccel | A root-domain priority power interest opens the *sleep window*: page-offs IOGraphicsAccelerator2 does now keep their VRAM (see below). |
| Display goes off | NVRMFB | As before (DPMS). |
| NVRM `setPowerState(0)` | NVRM | Tell the framebuffers; `nvKmsSuspend` (heads off, while the GPU still serves the core channel); close the RM gate; stop user channels; save PCI config, Resizable BAR control and the root port window; `rm_power_management(STANDBY)`. |
| — | RM | With `PreserveVideoMemoryAllocations=1` RM copies **every** VRAM allocation out through its temporary-file interface. NVRM backs that "file" with pageable kernel memory (no disk I/O). GSP-RM is unloaded. |
| Slot powered again, NVRM `setPowerState(1)` | NVRM | Restore the root port window and the Resizable BAR size (the firmware resets BAR1 to 256 MB), check `PMC_BOOT_0`, `rm_power_management(RESUME)` (GSP boots again through FSP and VRAM is copied back), restart user channels, `nvKmsResume`, open the RM gate, tell the framebuffers. |
| NVRMFB `setPowerState(1)` | NVRMFB | Re-light the head once NVRM says the GPU is back, then do IOGraphics' own wake work (below). |
| CPU back on | NVAccel | The sleep window closes: page-offs give VRAM back again. |

## Why each part is needed

**NVRM in the power tree.** NVRM was not a power child of the GPU's `IOPCIDevice`, so nothing suspended or resumed RM.
It now joins the tree under the PCI device. `-nvrmnopm` restores the old behaviour.

**All of VRAM is preserved.** By default RM saves only its own allocations. WindowServer's GPU channels (NVK through
NVAccel) keep push buffers and semaphores in VRAM; without them the compositor waited on fences forever after wake.
NVRM sets the RM registry key `PreserveVideoMemoryAllocations=1`. On Linux RM then writes VRAM to a file under
`NVreg_TemporaryFilePath`; here `os_open_temporary_file`/`os_write_file`/`os_read_file` (`os-xnu2.cpp`) store it in
4 MB chunks of pageable kernel memory, so the VM can compress it on a machine short of RAM, with wired memory as the
fallback. RAM keeps power in S3, and no disk is touched while the system goes to sleep. The RM itself is unmodified.

**The RM gate.** Linux freezes user space before suspend and also takes `nv_system_pm_lock` in every ioctl. macOS freezes
nothing: on wake WindowServer and Metal apps called into RM while the GPU was still in standby, which corrupted GMMU
state or handed errors to NVK. Every RM entry made for a client (escape ioctls, client close, NVAccel's GPU VA and
surface-share calls, `nvkms_call_rm`) now waits at a gate that the PM thread closes for the suspend and opens after
the resume. NVKMS has its own gate: ioctls wait (30 s at most), flips fail at once, timers wait.

**PCI state.** IOPCIFamily restores the config space it shadows, but not the Resizable BAR control register, and the
firmware's S3 resume path resets BAR1 to 256 MB. NVRM writes back the BAR1 size, the BARs and the root port window it
saved before standby.

**NVRMFB and IOGraphics.** NVRMFB overrides `IOFramebuffer::setPowerState`, so the family's own power work never runs,
and `IOFramebuffer::handleEvent()` does nothing off the IOGraphics controller's power thread. At system sleep IOGraphics
clears `pagingState` and sends `kIOFBNotifyWillSleep` to every client; after wake WindowServer's wake acknowledgement
(`_extEntry`) slept until `pagingState` came back, and the accelerator stayed disabled. On a wake after a GPU standby
NVRMFB now does what IOGraphics does (`IOFBController::computeState` and `handleEvent(kIOFBNotifyDidWake)` in
IOGraphics-599): deliver `DidWake` with `true` and then `false` (the second re-enables the accelerators), deliver
`DidPowerOn`, set `pagingState` and wake WindowServer on `fServerConnect`, under the framebuffer's work loop gate. It
never pushes to NVKMS before NVRM has resumed the GPU (`NVRMGpuReady`, `NVRMGpuPower`).

**NVAccel and VRAM across sleep.** At sleep IOGraphicsAccelerator2 pages off its VRAM resources, and the wake's mode
change pages them back on. If page-on got fresh VRAM, WindowServer kept drawing into the memory it had imported before
sleep while the display showed the new one: a black desktop with only the cursor. NVAccel's sticky VRAM
(`debug.nvaccel_vm_sticky`, on by default) keeps a surface's VRAM across page-off, and its next page-on takes the same
VRAM back; RM has kept the contents through the sleep, so nothing is lost.

## Switches and diagnostics

| Name | Kind | Effect |
|---|---|---|
| `-nvrmnopm` | boot-arg | NVRM stays out of the power tree and keeps the stock RM settings: the behaviour before this change. |
| `debug.nvaccel_vm_sticky` | sysctl (rw) | `1` (default) a surface keeps its VRAM across page-off; `0` page-off gives it back and the desktop is black after wake. |
| `debug.nvaccel_vm_parked`, `debug.nvaccel_vm_unparked` | sysctl | Page-offs that kept their VRAM, and page-ons answered with it. |
| `debug.nvrm_pm_waited` | sysctl | RM calls that waited at the gate for a resume. |
| `debug.nvrm_isr_count`, `debug.nvrm_isr_handled` | sysctl | GPU interrupts taken and claimed by RM. |
| `nvrm-pm` | NVRM registry property | `running`, `suspended` or `lost (resume failed)`. |

Kernel log lines to look for (`/usr/bin/log show --predicate 'process == "kernel"'`): `NVRM-xnu: sleep #`,
`NVRM-xnu: wake #`, `VRAM save file closed: N MB held`, `NVRM-fb: system wake:`.

## Not covered

- NMIntelFB (laptops whose panel is on the Intel GPU) has the same `setPowerState`/`handleEvent` pattern as NVRMFB and
  does not get the IOGraphics wake work yet.
- Hibernation (`hibernatemode` 3/25) is untested. Desktops default to `hibernatemode 0`.
- An open SSH session (`ttyskeepawake 1`) turns Apple menu → Sleep into a dark wake, not a sleep.
- Some boards wake every minute through USB (`pmset -g log`: `DarkWake ... due to XDCI ... XHCI`). That is the
  firmware's USB wake GPE (0x6D), not the driver; the usual `GPRW` → `XPRW` rename with `SSDT-GPRW` stops it, and then
  only the power button wakes the Mac.
