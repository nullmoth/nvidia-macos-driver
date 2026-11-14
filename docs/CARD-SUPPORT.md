# NVIDIA card support and validation

The driver uses NVIDIA's r615 open RM, HAL, and GSP stack. That stack targets Turing and later
([upstream compatibility](https://github.com/NVIDIA/open-gpu-kernel-modules#compatible-gpus)).
The installer carries 247 distinct display-card PCI device IDs from NVIDIA's 615.78.08 table (`SUPPORTED_IDS` in
[`package/install.sh`](../package/install.sh)). The kernel matches NVIDIA display
and 3D-controller PCI classes, then asks NVIDIA's own supported-device gates to accept the card.
A card being in the table doesn't mean a given laptop works: its display wiring, firmware and BAR
setup still have to cooperate.

| Family | Examples covered by the device table | Physical validation in this release |
| --- | --- | --- |
| Turing | GTX 1650/1660, RTX 20, TITAN RTX, Quadro T1000/T2000/RTX, NVIDIA T400/T600/T1000, T500/T550/T600/T1200 laptop | Pending |
| Ampere | RTX 30, RTX A workstation and laptop cards | Pending |
| Ada | RTX 40, supported RTX workstation and laptop cards | Pending |
| Blackwell | RTX 50 and supported RTX PRO cards | RTX 5060 automated; RTX 5070/5080 reported working |

Pascal, Maxwell, Kepler, and earlier cards need a different kernel/firmware implementation.
They are outside the present GSP-based driver. Headless compute cards are not promised a
macOS desktop. A laptop also needs a display/output route the NVIDIA card can drive; accepting
its PCI ID does not implement an Optimus display mux.

## Startup correction

When BAR placement uses the fallback, automatic GPU startup waits 100 seconds. Previously the
40-second boot-hold deadline ran from driver start, released the hold first, and prevented the
later display-arm operation. The deadline now includes the settling delay and then gives GPU
bring-up 40 seconds. The actual settling delay is exposed as `nvrm-autogo-settle-ms`.

An RTX 5060 passed a guarded boot with a deliberately forced 100-second settling delay, then
passed a second boot with the production 500 ms delay. Both runs armed the display, loaded all
four kexts, and kept WindowServer stable for 60 seconds without a new desktop/kernel crash.
That shows the timing fix works, but it was not a physical small-BAR card. Resizable BAR on
is still the tested setup.

## Desktop presentation

WindowServer submits a display-pipe swap immediately after `commitAndWaitUntilSubmitted`.
The NVIDIA kernel path cannot order that swap after the plugin's NVK fence. Submission alone
therefore allows scanout to read a surface the GPU is still drawing, causing flashing under
cursor movement or continuous redraw. WindowServer now waits for command-buffer completion
before that call returns. Other applications retain the submission-only behavior.

A CPU regression executes the actual method with mocked command buffers, checks both process
paths and error propagation, and proves the previous submission-only method fails the desktop
ordering test. The physical desktop acceptance also requires page flipping and 60 seconds of
stable WindowServer operation with no flip-latch timeouts or new desktop/kernel crashes.

When a card still flashes under cursor movement with that ordering in place, `-nvrmnoflip` keeps
the composited frame on the copy path: NVRM never sets `debug.nvaccel_iop_flip`, so the display
reads a finished copy instead of the surface the GPU may still be writing. It costs one copy per
frame and is the supported way to trade that for a stable picture. NVRM turns zero-copy scan-out
on about 60 seconds after it releases the boot hold (`NVRM::autoGo`), which is why a desktop that
flashes may look clean for the first minute and then start.

## Shader target selection

The optional vendor compiler selects the SM target from the chip name reported by NVK. Unknown
chip names now refuse that optional path and use NVK/NAK instead of guessing `sm_120`. CPU tests
cover Turing, Ampere, Ada, Hopper, Blackwell, absent names, and unknown names. This prevents an
unknown chip from receiving a cubin compiled for a guessed architecture.

## Rendering and application evidence

On RTX 5060/macOS 15.8.1, the diagnostic candidate and signed release candidate passed:

- Eight texture row-pitch/offset cases: zero incorrect pixels.
- Twelve private/IOSurface render-target cases, five repetitions each: zero incorrect sampled pixels.
- Five MPS Gaussian blur runs: zero incorrect image components.
- A Core Image filter graph: zero incorrect interior image components.

Diagnostic runs produced nonempty per-run logs, zero GPU faults, and an unchanged WindowServer
PID. These tests cover rendering and system image-processing frameworks. They are not a
validation matrix for every application or every listed card.

## Reports that can explain card-specific failures

1401 prioritizes GPU state, kernel logs, plugin logs, the crash report, and the collection summary
before older OpenCore logs. GPU state includes PCI device/subsystem IDs, BAR addresses, and
bring-up properties. Kernel logs include the current message ring and explicit query status;
plugin logs include release errors and available diagnostic file logs. Release failures such as
`vkCreateDevice -> -8` are retained even when the message does not contain `FAIL`.

Send logs from 1401 after a failure and keep the report ID. A missing GPU in an old report or a
successful device-table lookup cannot prove a card initialized correctly.

## Regressions

Run `python3 tools/test_card_support.py` on macOS with command line developer tools. The tests
check that both installers' `SUPPORTED_IDS` and this page's count match `nvidia_gsp_ids.json`, then
compile production snippets for startup timing, chip selection, release error classification,
desktop completion ordering, and upload ordering. They require neither GPU access nor a driver installation.
