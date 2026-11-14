# Experimental process-local OpenCL for RTX 5090

This source reference captures the private clvk/NVK configuration that completed
Geekbench 7 OpenCL runs at **313,622, 316,116 and 315,656** on an RTX 5090.
The last CLI result came from the installed user package. A subsequent completed
GUI run scored **325,451** (Path Tracer **225,757**). All eleven workload scores,
the control experiments, clock observations and validation limits are in
[RESULTS.md](RESULTS.md).

The aim is to make the measured implementation reviewable. This directory is an
experimental patch and reproduction bundle, not an automatic installer or a
change to the default NullMoth release. It is a follow-up to
[#524](https://github.com/nullmoth/nvidia-macos-driver/pull/524); that PR's v2.7.2
Metal/OpenCL changes are a different execution path.

## Implementation

- **clvk memory transfers:** prefer CPU-visible device-local buffers when enabled;
  use cached host staging for readback and large writes; retain a bounded reusable
  staging allocation through the GPU wait and CPU copy. Keep the tested queue and
  event lifetime fixes, physical-device metadata, and timestamp calibration.
- **Selective NAK retry:** when an enabled compute compilation actually spills to
  memory, retry without load sinking. Accept only fewer memory spills/fills with
  no register-spill or occupancy regression. Preserve the existing six-argument
  compiler ABI, own the cloned NIR independently, and include the policy in the
  shader cache key. Selection does not match benchmark or kernel names.
- **Queue shutdown:** give barrier destruction to the owner after worker
  completion, avoiding dependence on Darwin's fallback barrier return value.
- **Process configuration:** use clvk's existing first-batch size 10 / group size 1
  settings. Preserve the explicit process environment through Metal discovery;
  match the existing scratch and boost-hold settings.
- **Launcher:** verify the pinned payload and original application, isolate the
  caches, quote the compiler path, and exec the genuine application. No benchmark
  binary or workload is modified.

The exact measured source changes are retained, including disabled diagnostics.
The three-run total is not the isolated gain of any single patch. Selective retry
alone improved the two-run mean by 2.53%; its Path Tracer score was approximately
unchanged. See the staged comparisons in the results.

## Review and reproduction

Start with [SOURCE.md](SOURCE.md) for pinned revisions, patch order and build
prerequisites. [sources.json](sources.json) identifies the inputs and source file
hashes. The CPU replay check verifies that the patches reproduce those source
files in temporary copies without altering the supplied source trees.

[launcher/README.md](launcher/README.md) explains the exact launcher and its
intentionally incomplete example manifest. Each rebuilt package needs its own
binary hashes and application/dependency identity checks before use; the example
does not authorize running an unqualified binary.

The recorded hardware checks passed all 12 correctness gates, both Metal/OpenCL
initialization orders, the installed-package CLI benchmark, and GUI startup.
[validation-summary.json](validation-summary.json) records the gate outcomes.
These are the original qualification results, not claims that a newly rebuilt
publication tree has been benchmarked.

## Scope and remaining work

The scores are from genuine Geekbench CLI runs with a private OpenCL framework
and private NVK selected only for that process. The original app, global NVMTL and
NVK libraries, system OpenCL framework, configuration and security settings were
unchanged. Ordinary Finder launch continues to use the normal installed route.

The private NVK is **not qualified as a replacement for the global NVMTL bundle**.
Its LLVM/Rust toolchain differs from the release build. The device's 170 compute
units were verified before enabling the corresponding clvk override; this is not
a suitable unconditional setting for other cards.

The subsequent GUI benchmark completed and its benchmark child loaded the
intended private libraries. Three CLI runs and one manual GUI run on one card
do not establish long-term or multi-device stability. The GUI run had no
concurrent clock/fault/overlap monitor. Approximately 1 Hz samples from the CLI
runs do not establish the cause of an earlier Path Tracer outlier.

No proprietary Geekbench files, compiled drivers, caches, raw claim links or
machine-specific logs are included. Source patches retain their upstream project
licensing. This contribution was prepared with AI assistance; it does not assert
that the author wrote every line without AI tools.
