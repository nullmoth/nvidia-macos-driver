# GPU sensors

NVRM reads power, clock and fan samples from NVIDIA's RM user shared data
(RUSD). NVAccel exposes the results on the accelerator for monitoring tools.
Temperature, utilization and memory accounting keep their existing data sources.
An invalid temperature read removes both temperature names on the next publication.
No additional kext, daemon or VirtualSMC plugin is needed.

These are IORegistry properties for monitoring clients, not a public Apple
sensor API or a guarantee of compatibility with every monitoring app.
`NVRMFanRPM` is specific to this driver. Clients must read the documented keys;
iStat Menus support for the new power, clock and fan values has not been
verified. This change does not publish SMC keys or include a VirtualSMC bridge.

The [Stats GPU reader](https://github.com/exelban/stats/blob/683a0581d42839969ab21c14659f23d6054bb618/Modules/GPU/reader.swift#L123-L150)
matches an accelerator to its PCI device using `IOPCIMatch` and classifies NVIDIA
devices using `IOClass`. The concrete accelerator class is `NVAccelNVIDIA`, with
the existing `NVAccel` base preserved for IOKit class matching. `IOPCIMatch`
contains the attached device's actual device/vendor IDs; the personality retains
its broad NVIDIA vendor match. No specific card ID is hard-coded.

Stats reads `Temperature(C)`, `Device Utilization %` (or `GPU Activity(%)`),
`Core Clock(MHz)` and `Memory Clock(MHz)` with the units below. It reads
`Fan Speed(%)`, not RPM, and does not consume these power or per-controller fan
properties. No fan percentage is synthesized from RPM or an assumed maximum
speed. Source compatibility is not proof of end-to-end operation: a new build
still needs a reboot and verification in the consumer app.

## Published values

These keys are in NVAccel's `PerformanceStatistics` dictionary:

| Keys sharing a value | Value |
| --- | --- |
| `Temperature(C)`, `GPU Temperature(C)` | Existing GPU temperature in degrees Celsius |
| `Device Utilization %`, `GPU Core Utilization %`, `GPU Activity(%)`, `GPU Core Utilization` | Graphics engine utilization, 0–100 |
| `GPU Memory Utilization %`, `GPU FB Utilization(%)` | Memory controller utilization, 0–100; not VRAM allocation |
| `GPU Video Engine Utilization %`, `GPU Encoder Utilization(%)` | Video encoder utilization, 0–100 |
| `GPU Video Decode Utilization %`, `GPU Decoder Utilization(%)` | Video decoder utilization, 0–100 |
| `Total Power(W)`, `GPU Power(W)` | Average GPU power, rounded to watts |
| `GPU Average Power(mW)`, `RUSD Power(mW)` | The same average without rounding |
| `Instant Power(W)` | Instantaneous GPU power, rounded to watts |
| `GPU Instant Power(mW)` | The same instantaneous sample without rounding |
| `Core Clock(MHz)`, `GPU Core Clock(MHz)` | Actual graphics clock, converted from kHz |
| `Memory Clock(MHz)`, `GPU Memory Clock(MHz)` | Actual memory clock, converted from kHz |
| `Fan Speed(RPM)`, `fan-rpm`, `GPU Fan Speed(RPM)`, `Fan RPM`, `GPU Fan RPM` | Highest RPM among the reported fan controllers |
| `Fan Count`, `FanCount` | Number of reported fan controllers |
| `vramUsedBytes` | Existing `inUseVidMemoryBytes` accounting, as a 64-bit byte count |
| `vramTotalBytes` | PCI device's `VRAM,totalsize`, as a 64-bit byte count |
| `GPU Memory Used(MB)`, `GPU Memory Total(MB)` | The corresponding byte counts divided by 1,048,576 (MiB, despite the legacy key spelling) |

Aliases use the same source and validity as their original value. They are
compatibility names used by monitoring clients, not additional sensors or a
universal Apple standard. Memory aliases do not replace the accelerator family's
canonical memory accounting properties.
The memory aliases inherit the freshness of the family's current/accumulated
accounting fields; they do not have a separate RUSD timestamp or expiry timer.

`NVRMFanRPM` is a separate dictionary on NVAccel. Its numeric string keys are
controller indices; values are RPM. A controller can drive more than one
physical fan. Zero RPM is a valid stopped-fan reading. No fan percentage or
maximum RPM is inferred from a particular card model.
RUSD has no controller-presence mask, so a controller is exposed only after it
has reported a nonzero RPM once; an initially stopped fan can remain absent
until it first spins.

The four groups (average power, instantaneous power, clocks and fans) have
independent validity and timestamps. Unsupported or failed readings are omitted.
Repeated firmware timestamps do not refresh the cache; samples older than five
seconds disappear. After a sleep cycle, a new timestamp must show that polling
has resumed before values are published again.

## Implementation and build

`NVRMGpuTelemetry` is a versioned, kernel-only platform function. NVAccel calls
the NVRM service attached to its own PCI device from its existing sampling work
loop. Calls use NVRM's nonblocking sleep gate and serialize query and teardown.
Only copied values cross this interface, with no RM pointers or binary offsets.

Compile `nvrm_telemetry.c` with the same NVIDIA 615.78.08 headers and build flags
as the RM archive, using the existing RM compiler wrapper:

```sh
OGKM=/path/to/open-gpu-kernel-modules python3 kexts/NVRM/rmcc.py \
  kexts/NVRM/nvrm_telemetry.c /path/to/out/nvrm_telemetry.o
```

Include that object in the NVRM link alongside the other XNU adapters and
`libnvkernel.a`. Build NVAccel for both supported macOS versions as usual.
The telemetry ABI is separate from the existing utilization and VRAM ABIs.

## Validation

Run the decoder, cache and property-mapping tests without loading a driver:

```sh
clang++ -std=c++17 -Wall -Wextra -Werror tools/test_nvrm_telemetry.cpp -o /tmp/nvrm-telemetry-test
/tmp/nvrm-telemetry-test
```

Hardware acceptance requires checking the new readings at idle and under load,
stopped and spinning fans, and sleep/wake while monitoring. Confirm that values
resume changing and that unavailable readings disappear. Check display and Metal
operation as well; compilation and fixture tests do not establish GPU support.

Inspect the published values with:

```sh
ioreg -r -c NVAccel -d 1 -l -w 0
```

The sensor selection and monitoring keys follow
[SMCNvidiaGPU](https://github.com/zzzyy678/SMCNvidiaGPU), by zzzyy678 (MIT).
NVIDIA's public RUSD definitions are used directly. See `NOTICE` and
`licenses/SMCNvidiaGPU-LICENSE.txt` for attribution.
