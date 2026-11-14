# RTX 5090 OpenCL validation

## Performance and validation

The final process-local OpenCL configuration exceeded 300,000 in three completed Geekbench 7 CLI runs on the RTX 5090. The third run used the actual installed user package and a fresh package cache. These are results from the unchanged genuine Geekbench executable, with a private clvk/OpenCL framework and NVK library selected for that process.

| Run | OpenCL score | Path Tracer | Public result |
| --- | ---: | ---: | --- |
| Final profile, first run | 313,622 | 222,752 | [401039](https://browser.geekbench.com/v7/gpu/401039) |
| Final profile, repeat | 316,116 | 220,642 | [401046](https://browser.geekbench.com/v7/gpu/401046) |
| Installed package, fresh package cache | 315,656 | 221,785 | [401100](https://browser.geekbench.com/v7/gpu/401100) |

The three totals average 315,131; their range is 0.79% of that mean. This is repeat evidence from one machine, not a long-term or multi-device stability claim.

### All workloads

| Workload | First | Repeat | Installed package |
| --- | ---: | ---: | ---: |
| Background Blur | 119,826 | 124,675 | 124,521 |
| Face Tracking | 198,302 | 198,224 | 194,878 |
| Super Resolution | 238,858 | 237,306 | 237,478 |
| Horizon Detection | 416,945 | 423,158 | 421,384 |
| Photo Filter | 446,951 | 446,055 | 444,392 |
| Video Filter | 411,194 | 410,869 | 409,363 |
| RAW | 517,535 | 528,548 | 524,400 |
| Feature Matching | 492,515 | 516,647 | 512,356 |
| Path Tracer | 222,752 | 220,642 | 221,785 |
| Particle Physics | 381,019 | 383,198 | 382,950 |
| Fluid Simulation | 306,865 | 299,969 | 307,509 |

### What the staged comparisons establish

- The private NVK control scored [288,027](https://browser.geekbench.com/v7/gpu/400911) and [287,568](https://browser.geekbench.com/v7/gpu/400914). Selective spill retry scored [294,500](https://browser.geekbench.com/v7/gpu/400979) and [295,647](https://browser.geekbench.com/v7/gpu/400982): +2.53% in the two-run means. The largest mean subscore differences were Super Resolution (+13.92%), Face Tracking (+7.74%) and Background Blur (+5.01%); Path Tracer was approximately unchanged (-0.34%). Do not attribute the full final score or Path Tracer improvement to spill retry alone.
- A diagnostic comparison matched all eight GEMM pipeline keys, including original and separate fixture-only hinted variants. In the biased block GEMM variant also observed in the full benchmark, selected compiler statistics changed from 45 memory spills / 45 fills to 0 / 0, 255 GPRs to 72, 8 maximum warps/SM to 28, and 168 bytes SLM to 0. Non-block variants remained unchanged. These are compiler statistics, not dynamic instruction counts or measured kernel durations.
- Adding the existing early-flush settings produced [315,580](https://browser.geekbench.com/v7/gpu/400991) and [312,457](https://browser.geekbench.com/v7/gpu/400997). Their Path Tracer scores were 223,348 and 198,690. The low value is retained as an observed result rather than discarded.
- The final GUI-compatible process profile then produced the three results above. It preserves the requested environment during Metal initialization and explicitly includes the existing scratch setting and boost-hold policy. The earlier Path Tracer dip did not recur in those three runs. This does not prove which setting caused the difference.

### Correctness and integration checks

- All 12 final-configuration gates passed: physical device query, workgroup behavior, memory copy, host-pointer memory use, large-memory transfer, API behavior, numerical operations, FP32, readback, metadata, conversion, and native GEMM.
- Metal-first and OpenCL-first initialization both passed the dual-backend fixture, with the selected settings preserved and the expected separate installed/private libraries loaded.
- All three final benchmark runs completed all 11 workloads, exited normally with status 0, and passed runtime image identity, kernel-fault, and unchanged-system checks. The test monitor detected no competing GPU benchmark workload. Display activity was 0 flips/s during these runs.
- The installed-package run completed in 123.343 seconds with 121 GPU clock observations. The package launcher, actual loaded copies, and process/environment contract were checked, rather than relying only on a build-directory run.
- The actual installed shortcut successfully launched the original GUI with the intended private OpenCL route. The GUI benchmark button and helper inheritance have not been exercised because the desktop was locked. The reported performance results are CLI results; ordinary Finder launch continues to use the installed global environment.

### Clock observations and limits

GPU clocks were sampled approximately once per second. In the first two final-profile runs, all 106 and 100 workload-window observations were P1, with graphics clocks at least 2,430 MHz and memory at 14,001 MHz. Earlier early-flush runs included P8/P32/P256 observations and graphics minima of 172/180 MHz.

This shows persistent sampled clock state, not a proven clock-related performance fix. Graphics-clock medians among observations with GPU utilization >=25% were 2,797/2,797 MHz before and 2,774.5/2,805 MHz after; there was no uniform increase in sampled busy clocks. Thermal conditions also differed: final maxima were 56.0/53.8 C versus 65.1/66.5 C previously. Each Path Tracer phase had only one utilization >=25% observation. Sparse instantaneous telemetry, three profile settings changing together, and the absence of CPU-load/power measurements prevent a causal diagnosis of the earlier Path outlier.

### Exact final settings and identity

The selected process-local configuration was:

```text
CLVK_KEEP_TEMPORARIES=1
CLVK_CLSPV_OPTIONS=-hack-clamp-width
CLVK_PREFER_DEVICE_LOCAL_BUFFERS=1
CLVK_KEEP_MEMORY_ALLOCATIONS_MAPPED=1
CLVK_MAX_COMPUTE_UNITS=170
NVMTL_NAK_SPILL_RETRY=1
CLVK_MAX_FIRST_CMD_BATCH_SIZE=10
CLVK_MAX_CMD_GROUP_SIZE=1
NVMTL_PROCESS_ENV=1
NVMTL_NAK_ARR_SCRATCH=0
NVRM_610_BOOST=hold
```

SHA-256 identities of the tested payload:

```text
OpenCL framework  56655cd6c0cb65c329bbefc7d4eb87991d513fd64f50ce13546651842c1cf7e5
clspv compiler    a4c6fc28ea6e2a1fabf486ccf2171aada5746f74b4b9c110ca0e69cbdeff47ea
Private NVK       7638e1919a5150438a62693ac5bdf57dbe5738676b870ff5e1024fce87360023
```

The original application, installed NVMTL/Metal driver, installed NVK, nvrm610.conf, system OpenCL framework and security settings were preserved. The private NVK is qualified for this process-local Vulkan/OpenCL route and must not be represented as a tested replacement for the global NVMTL bundle. The private build also uses a different LLVM/Rust toolchain from the release binary, so comparisons against the release binary are not same-compiler comparisons.

Only normal public result links are included here. Raw logs can contain private Geekbench claim links and local machine paths; those should not be published unchanged.
