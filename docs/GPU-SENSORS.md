# GPU sensors

NVRM reads power, clock and fan samples from NVIDIA's RM user shared data
(RUSD). NVAccel exposes the results on the accelerator for monitoring tools.
Temperature, utilization and memory accounting keep their existing data sources.
An invalid temperature read removes both temperature names on the next publication.
No additional kext or daemon is needed. When VirtualSMC is already present,
NVAccel also registers an optional read-only SMC plugin from inside the existing
driver. Native collection and IORegistry publication work without VirtualSMC.

These are IORegistry properties for monitoring clients, not a public Apple
sensor API or a guarantee of compatibility with every monitoring app.
`NVRMFanRPM` is specific to this driver. Clients must read the documented keys;
iStat Menus support must be checked separately for its GPU and Sensors views.
Its GPU reader uses IORegistry; the SMC publication below gives consumers such
as Stats Sensors another path for per-controller RPM and GPU temperature.

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
`PerformanceStatisticsAccum` can be stored internally as an `OSSerializer`,
even though IORegistry clients see a dictionary. NVAccel materializes that
no-reset snapshot with the public serialization APIs before reading its
`inUseVidMemoryBytes` gauge. It accepts at most 64 KiB of serialized XML for
parsing; this is not a hard allocation limit on the trusted family serializer.
If serialization, parsing or number lookup fails, the used-memory aliases are
omitted rather than published as a fabricated zero.

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

## Optional SMC publication

The integration uses VirtualSMC's public ABI 1 plugin interface and ships in
NVAccel, with no VirtualSMC or Lilu load-time link dependency. Setting the boot
argument `nvsmcoff=1` disables this layer. Missing VirtualSMC, an unsupported
AppleSMC connection, or an unreadable key inventory leaves native sensors
available without registering SMC keys.

Registration waits for native samples and a stable fan-controller mask. It
enumerates the complete AppleSMC key table and reads `FNum`. New fan slots are
appended after both the advertised count and the highest existing decimal
`F#xx` slot. Holes and existing zero-RPM fans remain occupied. Only slots 0–9
are used, matching the four-character decimal keys accepted by Stats. If all
reported controllers cannot fit, no fan slots are added. An independent free
GPU temperature index can still be used.

Each added controller exposes read-only `F#Ac` (`fpe2` RPM) and `F#ID` (`{fds`,
16 bytes, descriptive name). `FNum` extends the previous fan count. No existing
fan's RPM or name is replaced. There are no fan-control, minimum/maximum-RPM or
estimated percentage keys. `TG#D` and `TG#P` (`sp78`) share the existing measured
core temperature; they do not represent separate diode and proximity sensors.
The index is chosen only when no `TG#x` key exists for that index.

Both the inventory and target-key absence are checked again immediately before
submission. Failures or changes abort that attempt. Registration is serialized
across NVAccel devices. VirtualSMC does not offer an atomic reservation API,
so this cannot prevent an unrelated plugin registering conflicting keys at
the same instant or later overwriting `FNum`. All previously observed fan slots
remain outside the new plugin's key set.

SMC reads encode atomically cached samples, with no hardware access, allocation,
IORegistry access or sleeping lock in the callback. Fan freshness uses the
source sample's arrival time, not the time of repeated publication. A reading
older than five seconds or from a stopped service is unreadable; measured
zero RPM remains valid. Consumers may apply their own stale-value policy.

VirtualSMC retains the plugin descriptor and has no public removal API. After
a submission attempt, NVAccel refuses module unloading and keeps the callback
storage until reboot. Stopping the device invalidates readings but leaves the
registered count and labels. The controller set is fixed at registration;
a previously undiscovered controller that first spins later needs another
boot before it can receive an SMC key.

`NVRMSMCStatus`, `NVRMSMCSubmitResult`, `NVRMSMCOriginalFanCount`,
`NVRMSMCFanMap` (decimal SMC slot to native controller index), and
`NVRMSMCGPUIndex` on NVAccel report registration and mapping. These diagnostic
properties must be inspected after boot before claiming SMC support works on
a particular machine. Stats' GPU percentage row remains distinct from its
SMC RPM sensors; no percentage is invented to populate that row.

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
clang++ -std=c++17 -Wall -Wextra -Werror -fsanitize=address,undefined \
  tools/test_nvrm_smc_plan.cpp -o /tmp/nvrm-smc-plan-test
/tmp/nvrm-smc-plan-test
```

Hardware acceptance requires checking the new readings at idle and under load,
stopped and spinning fans, and sleep/wake while monitoring. Confirm that values
resume changing and that unavailable readings disappear. Check display and Metal
operation as well; compilation and fixture tests do not establish GPU support.
For the SMC integration, save a complete pre-install inventory, then after
reboot verify that earlier fan keys keep their types, names (where present),
and readings. Compare every added slot against its `NVRMSMCFanMap` controller
over multiple samples and check the allocated temperature keys. Verify both
the Sensors and GPU views in each monitoring app; one working view does not
establish compatibility with another.

Inspect the published values with:

```sh
ioreg -r -c NVAccel -d 1 -l -w 0
```

The sensor selection and monitoring keys follow
[SMCNvidiaGPU](https://github.com/zzzyy678/SMCNvidiaGPU), by zzzyy678 (MIT).
NVIDIA's public RUSD definitions are used directly. See `NOTICE` and
`licenses/SMCNvidiaGPU-LICENSE.txt` for attribution.
The minimal VirtualSMC/Lilu SDK headers and adapted value implementation retain
their upstream licenses under `kexts/NVRM/accel/smc/vendor`; see its
`VERSIONS.md` and the adapter README for the pinned versions and ABI checks.
