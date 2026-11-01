# GPU sensors

NVRM reads power, clock and fan samples from NVIDIA's RM user shared data
(RUSD). NVAccel exposes the results on the accelerator for monitoring tools.
The existing temperature, utilization and memory accounting paths are unchanged.
No additional kext, daemon or VirtualSMC plugin is needed.

## Published values

These keys are in NVAccel's `PerformanceStatistics` dictionary:

| Key | Value |
| --- | --- |
| `Total Power(W)` | Average GPU power, rounded to watts |
| `GPU Average Power(mW)` | The same average without rounding |
| `Instant Power(W)` | Instantaneous GPU power, rounded to watts |
| `GPU Instant Power(mW)` | The same instantaneous sample without rounding |
| `Core Clock(MHz)` | Actual graphics clock, converted from kHz |
| `Memory Clock(MHz)` | Actual memory clock, converted from kHz |
| `Fan Speed(RPM)` | Highest RPM among the reported fan controllers |

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

Run the cache tests without loading a driver:

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
