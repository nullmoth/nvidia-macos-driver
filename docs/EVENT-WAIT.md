# Application event waits: CPU cost and latency

`NVRM_EVENT_WAIT=1` enables the existing graphics-completion event wait for ordinary
applications. On one RTX 5060 Laptop GPU, this reduced the CPU time spent in two
synchronous workloads, at the cost of higher latency in the short compute test.
This is an opt-in measurement result, not a proposed change to the shipped default.

## What the setting does

In `nvk/nvk-macos.patch`, `nvkmd_nvrm_wait_block()` defaults to event waits for
WindowServer and polling/backoff for other processes. An explicit
`NVRM_EVENT_WAIT=1` enables event waits after 20 microseconds, with a bounded
2-millisecond wait and completion-state rechecks. The code falls back when a
suitable event is unavailable. The setting does not increase GPU clocks or its
power limit, and WindowServer already takes the event path by default.

For a machine-local trial, back up `/Library/GPUBundles/nvmtl/nvrm610.conf`, then
set exactly one `NVRM_EVENT_WAIT=1` entry while preserving its other settings.
Reopen the application so it reads the new configuration. Restore the previous
configuration to undo the trial. No driver reinstall or kernel-cache rebuild is
needed for this setting.

Release builds clear inherited `NVRM_` tuning variables in the plugin constructor.
An environment variable set before launching an application is therefore not a
reliable A/B test. The probe below loads the installed plugin first, then sets the
single wait-policy override before it creates the Metal device. The override is
confined to that probe process.

## Measured system

- NVIDIA GeForce RTX 5060 Laptop GPU, 8 GiB, PCI ID `10de:2d19`, NVK GB206-A.
- Intel Core i7-13700HX, 16 GiB RAM, AC power.
- macOS 26.7.1 (`25G241`), SMBIOS `MacBookPro16,2`.
- Built-in display at 2560 x 1600, 180 Hz, NVIDIA-driven.
- Driver source baseline: v1.13.0 (`0960b2ef5dbc4a573056d4c8e042a5bd2e15d651`).
- Both arms used the same locally repaired plugin and Tahoe NVAccel. The plugin
  included the specialized AIR symbol / sampler linking fixes from PR #79, and
  NVAccel had been rebuilt for the Tahoe ABI. This is **not an unmodified release
  package result** and does not qualify macOS 26 support generally.

## Results

Each workload was run in five paired rounds, reversing the policy order on
alternate rounds. Every process did two warmups and 25 measured iterations:
125 measurements per workload and policy. The table reports the median of the
five per-run medians, not a pooled median. The raw timing samples and per-run
results are in [EVENT-WAIT-RTX5060.json](EVENT-WAIT-RTX5060.json).

| Workload / metric | Polling (`0`) | Event wait (`1`) | Change |
| --- | ---: | ---: | ---: |
| Integer compute / process CPU time | 0.612 ms | 0.286 ms | -53.3% |
| Integer compute / elapsed time | 0.607 ms | 0.734 ms | +20.9% (+0.127 ms) |
| Core Image blur / process CPU time | 6.559 ms | 4.127 ms | -37.1% |
| Core Image blur / elapsed time | 5.608 ms | 3.926 ms | -30.0% |

Elapsed time includes encoding, submission, execution, and synchronous waiting.
It is measured with `CLOCK_MONOTONIC`, not GPU timestamps. Process CPU time is
`getrusage(RUSAGE_SELF)` user plus system time across all process threads, so it
can exceed elapsed time. Compilation and the final compute correctness check
are outside the timed iterations. These are not whole-system CPU utilization
or GPU throughput measurements.

The compute workload transforms 1,048,576 integers with 64 dependent integer
iterations each; every result is compared with a CPU reference. The image test
renders a 1024 x 1024 RGBA8 procedural image through a radius-10 Core Image
Gaussian blur into a CPU bitmap, with intermediate caching disabled. Every
image run had identical output bytes by hash and opaque alpha; the test does
not establish independent image-reference accuracy.

Image timings varied considerably between runs. The five polling run medians
were 5.161, 5.608, 3.047, 6.145, and 8.171 ms; event-wait medians were 3.714, 4.558,
4.517, 3.690, and 3.926 ms. No confidence interval or universal speedup is claimed.
No GPU power, clock, temperature, fan-speed, or battery-life data was collected.
The test was short, on one system, with its normal desktop session running.

## Correctness and installation checks

- The compute reference checks passed for both policies.
- All paired image results had the same pixel hash (`10771374122383686649`).
- A cross-queue test committed a consumer's event wait before the producer's
  fill and signal. Both policies passed 32 rounds and verified all 128 MiB.
- Shared-to-private, private-to-shared, and private-to-private 32 MiB copies
  passed full byte comparisons. Their timings are not used to claim a speedup.
- No probe timeout or WindowServer restart occurred in the reported paired
  runs. The driver's VRAM refusal counter remained zero.
- After enabling the setting in the local configuration, new processes with
  no overrides logged `event wait ON` and passed the same functional checks.
  No driver binary was replaced by this configuration trial.

Existing icon and Tahoe ABI repairs are disclosed as the test baseline, not
submitted as new work here. Alternative compiler flags, memory-mapping changes,
and an experimental wait-threshold patch were not deployed or included.

## Reproduce without editing the system configuration

On an x86_64 Mac with the installed NVMTL driver as the default Metal device and
Apple Command Line Tools available, run from the repository root:

```sh
python3 tools/bench_event_wait.py /tmp/nvmtl-event-wait-results
```

Use a new output directory. The runner compiles the probe into a temporary
directory, alternates both policies for five rounds, verifies the event-wait
log, compares image hashes, and stops on a probe failure, timeout, or a changed
WindowServer PID. It then checks cross-queue synchronization and buffer copies
with both policies. `--rounds 1` is a smoke test, not the five-round measurement.
The tool writes local results and logs; it does not upload anything or edit the
driver configuration. Normal driver shader-cache activity can still occur.

Review local stderr logs before sharing them. The checked-in fixture retains
only timing samples, correctness results, policy, and round numbers; it excludes
local user paths, process IDs, and raw system logs.

For an individual probe, compile and run:

```sh
xcrun clang -O2 -fobjc-arc tools/bench_event_wait.m \
  -framework Foundation -framework Metal -framework CoreGraphics \
  -framework CoreImage -o /tmp/nvmtl-event-wait-probe
/tmp/nvmtl-event-wait-probe compute NVRM_EVENT_WAIT=0
/tmp/nvmtl-event-wait-probe compute NVRM_EVENT_WAIT=1
```

The submitted probe preserves the measured compute/image workloads and timing
boundaries, with unused experimental driver-loading paths removed and the
override restricted to `NVRM_EVENT_WAIT=0` or `1`. Fresh measurements will vary.
