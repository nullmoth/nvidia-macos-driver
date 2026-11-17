# GPU utilization

NVAccel publishes `Device Utilization %`, `GPU Core Utilization %`,
`GPU Memory Utilization %`, `GPU Video Engine Utilization %` (encode), and
`GPU Video Decode Utilization %` in `PerformanceStatistics`. The device/core
value is RM's GR engine utilization. Memory is bandwidth utilization, not VRAM
allocation. Values are whole percentages rounded from RM's percent-times-100
units. No helper daemon, writable sysctl, or additional sensor kext is needed.

The accelerator asks the NVRM service on its own PCI device for a sample once
per second, on a separate work loop. NVRM uses a private kernel RM client and
the read-only `PERF_GET_GPUMON_PERFMON_UTIL_SAMPLES_V2` command. Its roughly
56 KB parameter buffer lives on the heap. Queries skip a closed system power
gate; teardown drains queries before releasing the RM client.

The latest nonzero timestamp wins within the bounded 72-entry ring. On the
tested RTX 3080, firmware leaves `count` at zero and returns a byte-offset
`tracker`; a tracker and its sample payload can also disagree by one entry.
Reading only `samples[0]` can return a sample about 14 seconds old. The parser
accepts entry and aligned byte offsets, validates sizes and utilization ranges,
and never substitutes an older sample for an invalid newest sample.

The existing 250 ms statistics publisher copies the cached reading without
calling RM on the graphics work loop. Failed queries and samples unchanged for
more than five seconds remove utilization keys instead of reporting a false
0%. Cache age includes time spent asleep. Both NVRM and NVAccel must be built
from this change; an older NVRM does not implement the query interface.

## Parser test

```sh
clang++ -std=c++17 -Wall -Wextra -Werror tools/test_nvrm_utilization.cpp -o /tmp/nvrm-utilization-test
/tmp/nvrm-utilization-test
```

Define `NVRM_TEST_NVIDIA_HEADERS` and add NVIDIA's `src/common/sdk/nvidia/inc`
and `src/common/inc` include paths to exercise the same parser with the public
RM structures. Optional positional arguments replay raw V2 parameter buffers
captured by a read-only probe.

Build NVAccel with `build/accel_build.sh`, once with `NM_TAHOE=1` for macOS 26
and once without it for macOS 15. Rebuild NVRM with its matching Darwin RM and
NVKMS archives. Before deploying, boot the paired kexts on real hardware and
check idle/load/recovery, sleep/wake, missing telemetry, and GPU teardown.

## Per-process Metal accounting (experimental)

Metal command buffers already record GPU start/end timestamps. The plugin uses
their validated intervals after successful completion to report cumulative GPU
execution time through a dedicated `NVGPUAccountingClient` on its selected
NVAccel. This is an `IOAccelSubmitter2` with the family's normal lifecycle and
public `accumulatedGPUTime` and `lastSubmittedTime` properties, which macOS's
Activity Monitor GPU scanner reads. It does not submit commands or map memory.

Reports contain nanoseconds of completed work and a last-submission timestamp
in `mach_absolute_time` units. The kernel binds each connection to its creating
task and accelerator; callers cannot supply a target PID. It rejects malformed,
replayed, decreasing and future reports, or totals exceeding the connection's
lifetime. This is application-reported telemetry, not a security or billing
measurement. The existing hardware utilization sampler remains unchanged.

Per-process accounting is **off by default**. Without an explicit
`NVMTL_GPU_ACCOUNTING=1`, the plugin opens no reporting client and creates no
reporting queue. The existing overall GPU utilization graph remains available.

To enable GPU % and GPU Time for Metal applications, add this to the user
settings block in `/Library/GPUBundles/nvmtl/nvrm610.conf` (inside the existing
block if one is already present):

```text
# --- my settings, kept on update ---
NVMTL_GPU_ACCOUNTING=1
# --- end my settings ---
```

Set it to `0` or remove that line to disable accounting. The user block survives
driver updates. From a source checkout, the settings helper can manage the
same option without changing other settings:

```sh
sudo python3 tools/nullmoth_settings.py set gpu-accounting on --apply
sudo python3 tools/nullmoth_settings.py set gpu-accounting off --apply
```

Relaunch applications after changing it. WindowServer needs a logout/login or
reboot; restarting Activity Monitor alone does not change other processes.
Counters cover work since a process enabled accounting; earlier work is not
recovered. To override the setting for one application, launch it with
`NVMTL_PROCESS_ENV=1 NVMTL_GPU_ACCOUNTING=1` (or `=0` to disable it).

When enabled, accounting stays active during Metal work even when Activity
Monitor is closed. Reports are batched on a background queue at
most four times per second. A final delayed report covers an app going idle;
there is no idle polling timer, additional GPU query, fence wait or queue-idle
operation. An unavailable or failed reporting connection disables telemetry
without changing rendering. An older accelerator without the versioned
capability simply receives no reports.

Intervals are merged to avoid counting overlap twice, including completions
collected out of order. History is bounded to 128 disjoint intervals; unusually
late completions outside that history can undercount, rather than charge work
again. Missing or invalid timestamps are skipped. Execution spans include GPU
stalls within the command buffer and are not a direct measurement of core
occupancy. Browsers can attribute work to a GPU helper process. Native NVK,
Wine/DXVK/VKD3D, Zink and independent video decode/encode engine work are outside
this feature's scope. Each process retains a small client and interval history;
the reporting interval also delays visible counter updates.

The trial requires both the Metal plugin and NVAccel, with a reboot to activate
NVAccel. It was boot-tested on RTX 2060 SUPER / Tahoe 26.7.1, and the user
confirmed both Activity Monitor columns. Six enabled/disabled pairs each of
paced and unpaced Metal work passed GPU readback, counter and process-exit
checks; CPU differences were smaller than the variation between runs. A
separate production-reporter test measured about 0.16 microseconds per completed
interval using synthetic timestamps and real kernel reports. This is accounting
cost, not a prediction of application performance. A ten-second idle check kept
GPU Time fixed, and client cleanup passed normal exits and a killed test process.
Both macOS accelerator variants compile and Tahoe kernel linking passes;
sleep/wake and macOS 15 execution remain untested.

```sh
clang++ -std=c++17 -O2 -Wall -Wextra -Werror tools/test_gpu_accounting.cpp -o out/test_gpu_accounting
out/test_gpu_accounting
clang -fobjc-arc -O2 -Wall -Wextra -Werror -I"$VULKAN_HEADERS/include" \
  tools/test_gpu_accounting_transport.m -framework Foundation -framework IOKit \
  -o out/test_gpu_accounting_transport
for mode in default empty invalid disabled missing unknown working failure; do
  out/test_gpu_accounting_transport "$mode" || exit 1
done
```

`tools/probe_gpu_accounting.m` performs isolated Metal work on two command queues,
checks GPU readback and the calling process's registry counters, and verifies
that GPU time stops increasing while idle. Build it with Foundation, Metal and
IOKit, pass the trial plugin executable, and add `--require-reporting` only
after activating the paired accelerator. Closing the probe must remove its
reporting client. Its controlled workload is not a full performance benchmark.
