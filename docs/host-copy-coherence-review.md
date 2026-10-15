# Host-copy coherence adaptation for PR #178

Based on 1.15.0 (`56bc4f9ae3105df052eefd3ea094c782e56853c3`), adapting the missing texture-backed host-copy synchronization identified by [modesttao in PR #178](https://github.com/nullmoth/nvidia-macos-driver/pull/178). This branch contains the original tested adaptation and its tests.

`nvmtl_submission_copies` finds copy-backed buffers bound directly or through textures/views, and deduplicates backing objects within each submission part. Both submission paths upload before GPU submission. Successful completion publishes surfaces and downloads potentially written buffers before signaling the completion event. Direct managed buffers retain their existing writeback path.

Texture usage `Unknown`, `ShaderWrite`, and `RenderTarget` permits writeback; explicit read-only usage does not. Buffers with neither a caller-memory copy nor a managed shadow are excluded. The default submit-ahead host-pointer hold includes textures/views. Finishing an already submitted part does not upload again. GPU completion waits and failure handling remain in place.

## CPU regression

On macOS with the Command Line Tools:

```sh
python3 tools/test_submission_copies.py
```

The test extracts the production discovery helper, byte-transfer functions, and both submission methods into an Objective-C fixture. It checks post-wrapping host edits, texture views, shared backing deduplication, unknown/render-target usage, read-only protection, direct managed writeback, allocation failure, failed submission isolation, publication before event signaling, and submit-ahead completion. It runs both default and `NVMTL_GSUB_HOSTPTR_LIFT=1` modes. Vulkan/GPU execution is stubbed. Restoring the original submission methods in the same fixture reproduces the stale-pixel assertion.

## GPU regression

```sh
clang -fobjc-arc tools/bench_copy_coherence.m -o /tmp/bench_copy_coherence \
  -framework Foundation -framework Metal -framework IOKit
/tmp/bench_copy_coherence /absolute/path/to/NVMTLDriver
NVMTL_ASYNC_COMMIT=1 NVMTL_GSUB_HOSTPTR_LIFT=1 /tmp/bench_copy_coherence /absolute/path/to/NVMTLDriver
```

Use a built plugin with its matching runtime libraries available beside it. The harness explicitly loads that binary into its own process; it does not install it. It intentionally uses an unaligned pointer and checks the driver's `_hostPtr` fallback, so this is a NullMoth-specific regression. An aligned/zero-copy buffer is rejected as the wrong test path. Apply a process timeout when automating GPU runs.

RTX 4050 Laptop (AD107), Tahoe 26.7.1 / 25G241: original 1.15.0 failed all 16 host-to-texture and all 16 texture-to-host cases; this adaptation passed all cases, including alternating texture views. Enabling submit-ahead options gave the same results. Those GPU runs enable the options but do not trace which parts used ahead submission; the CPU fixture executes that method directly.

The full plugin builds. Improved scrolling/window movement after installation and reboot is a subjective desktop observation; these tests do not measure FPS or establish long-term stability.
