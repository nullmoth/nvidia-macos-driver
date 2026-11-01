# Design

Goal: a VideoToolbox H.264 (later HEVC) encoder for pre-Blackwell NVIDIA GPUs under nullmoth's driver, written from
public documentation only, that keeps the frame on the GPU end to end (like NVENC on Windows), has real rate control
and low-latency settings, and more than one session per process.

Why: with nullmoth's closed `libnvenc_h264` (plus our layout fix) a 4K Parsec frame costs ~9 ms inside the library:
~2.5 ms of CPU tiling, ~1.3 ms of copies, our 1.3 ms rearrangement, and ~3.5 ms spin-waiting for the engine
([measurements](https://github.com/mickys/kvm-tahoe-nvidia/blob/main/docs/nvenc-enable-plan.md)). It also ignores the bitrate, refuses odd heights and allows one session per process.

## Sources (clean room)

| What | Source | Licence |
|---|---|---|
| NVENC engine interface: methods, picture setup, RC, surfaces, status | NVIDIA open-gpu-doc `classes/video/clc9b7.h`, `nvenc_drv.h` | MIT |
| RM API: objects, channels, memory, controls | NVIDIA open-gpu-kernel-modules (RM 610.57.04) | MIT/GPL |
| Talking to RM on macOS (escape interface, channel and memory mapping) | nullmoth's NVK port (`nvk/nvk-macos.patch`, nvkmd_nvrm_*) | MIT |
| Block-linear surface layout | open-gpu-doc / Mesa `nil`, and our measured tile mapping | MIT |
| VideoToolbox plugin interface | decoder side: Apple's SPI headers in WebKit (`VTVideoDecoderSPI.h`, `CMBaseObjectSPI.h`); encoder side: black-box probing (a stub encoder that logs which slots VideoToolbox calls) | |
| H.264 headers (SPS/PPS) | ITU-T H.264 | |

Not used: nullmoth's closed codec libraries (no disassembly, no patching; only their observable behaviour). Since
2026-10-18 (user's decision) that includes observing `libnvenc_h264` at runtime: its RM escapes, the commands it pushes
to the NVENC channel, and the buffers it hands the engine (`trace/nvtrace.c`, captures in `docs/traces/`).

## What the GPU offers (`tools/probe.c`, RTX 4090)

- Engines: NVENC0 `0x1b`, NVENC1 `0x1c` (two encoders), NVDEC, copy engines, GR.
- Classes: encoder `0xc9b7` (NVC9B7), channel `0xc56f` (AMPERE_CHANNEL_GPFIFO_A), usermode doorbell `0xc561`,
  copy `0xc7b5`, decoder `0xc9b0`, VA space `0x90f1`, 3D `0xc997`, compute `0xc9c0`.

## Design

```
VideoToolbox ── our VT encoder (CMBaseObject + encoder vtable) ── nvenc core     
                                                                     ├── RM client (src/rm.c): objects, controls
                                                                     ├── channel: GPFIFO on NVENC0/1, push buffers, doorbell, semaphores
                                                                     ├── surfaces: input NV12 (block-linear), reference/reconstructed pictures, bitstream, status
                                                                     ├── H.264 front end: SPS/PPS, picture setup (nvenc_drv.h), slice control, RC
                                                                     └── input path: IOSurface -> GPU block-linear surface on the GPU (copy engine or Metal), no CPU pass
```

Registration stays in the (patched) Metal plugin, which already decides per process; it would register our factory
instead of nullmoth's.

## Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 | RM client + capability probe | **done**: `src/rm.c`, `tools/probe.c` lists engines and classes |
| M1 | "Hello NVENC": VA space, GPFIFO channel on NVENC0, NVC9B7 object, push buffer with NOP + semaphore release, doorbell | **done**: `tools/hello` — host and NVENC semaphores written ~12 µs after the doorbell, on NVENC0 and NVENC1 |
| M2 | One H.264 IDR frame: block-linear input surface, picture setup, bitstream + status buffers, our SPS/PPS | **done** (2026-10-18): `tools/encode1`, 256x128 to 3840x2160 (and 3840x2025) decode at ≥ 45.8 dB luma, QP 26 |
| M3 | P-frames: reference/reconstructed pictures, frame numbering, RC modes (CBR/VBR, low latency), slices | **done** (2026-10-18): `src/enc.c` + `tools/encodeN`; 60-120 frame streams with 1-3 refs, CBR within ±5 %, one-frame-VBV low latency, 1-16 slices |
| M4 | VideoToolbox encoder plugin: probe the encoder vtable black-box, implement start/encode/complete/properties | **done** (2026-10-18): `vt/nvencvt.c` → `libnvencvt.dylib`, registered by the Metal plugin (`NVMTL_NVENCVT`, since 2026-10-18 the only plugin patch: nullmoth's closed encoder is no longer enabled before Blackwell); `vttest` and Parsec use it |
| M5 | GPU-resident input: IOSurface -> block-linear on the GPU; event-driven completion instead of spinning | **done** (2026-10-18): fast CPU tiling (4K 0.65 ms), event-driven completion, async encoding; **Parsec 4K encode latency 4.7-4.8 ms** (nullmoth's: ~10.5-12 ms) |
| M6 | Two sessions (NVENC0/1), HEVC | **done** (2026-10-18): sessions spread over NVENC0/1; HEVC IDR + P (1-3 refs) + RC at every size, through VideoToolbox and Parsec |

Step-by-step tasks for each milestone, with status and per-task reports: [tasks.md](tasks.md).

Risk notes: NVENC microcode is loaded by GSP-RM, so user space only needs the class methods; the picture-setup
structures are large and must be complete (unset fields are a common source of engine faults: check the status
buffer and RM's channel error notifier after every submit). A 2026 forum report of NVENC resetting a whole Linux host
with an RTX 4090 has not reproduced here in hundreds of encodes.

## M1 notes (what the working path needs)

- **Handles:** RM generates handles from `0xcaf00000` for objects it creates inside our client (a channel gets an
  internal channel group), so client-chosen handles must stay out of that range (`NV_ERR_INSERT_DUPLICATE_NAME`
  otherwise). `src/rm.c` uses `0x4e560000 + n`.
- **GPU addresses:** mapping memory into a `NV50_MEMORY_VIRTUAL` object with `NV_ESC_RM_MAP_MEMORY_DMA` returns the
  absolute GPU address in `dmaOffset`. Adding the object's offset again pointed the GPFIFO at an unmapped address:
  the error notifier (`NvNotification[0]`) reported `info32 0x1f` (31 = MMU fault) on engine `0x1b`.
- **CPU mappings on macOS:** `NV_ESC_RM_MAP_MEMORY` on the control connection names a second user-client connection by
  its bound fd number; `IOConnectMapMemory64` on that connection then maps it. Video memory via the device node,
  write-combined when RM says so.
- **Channel:** notifier + `NV01_CONTEXT_DMA` (error notifier), USERD 512 KB in VRAM, GPFIFO 0x8000 entries in system
  memory, `AMPERE_CHANNEL_GPFIFO_A` with `engineType` NVENC0/1, `NVA06F_CTRL_CMD_BIND`, the `NVC9B7` object
  (`NV_NVENC_ALLOCATION_PARAMETERS.engineInstance`), `GPFIFO_SCHEDULE`, work-submit token (also in notifier[1]).
  Submit = GPFIFO entry, `GPPut` in USERD, token to `NVC361_NOTIFY_CHANNEL_PENDING` of the `AMPERE_USERMODE_A` page.
- Engine object on subchannel 0 via `NVC56F_SET_OBJECT`; NVENC's own `SEMAPHORE_A..D` release works.

## M2 notes (what the docs don't say; details in docs/reports/T2.0x)

- **Input layout** (`nvenc_in_tile_plane`): with `input_cfg.block_height` = 2 and `input_bl_mode` 0, NVENC on Ada reads
  64-byte x 16-row tiles of 1 KB, row-major, with 16-byte x 4-row sub-blocks inside (order x16, y4, x32, y8). It is
  not the classic 64x8 GOB. Measured from a working client's input surface (T2.05d).
- **Two settings hang the engine** (app timer fires after ~1.08 s, `error_status` 1, RC 47): `pic_control.slice_mode` = 1,
  and `md_control.intra_ssd_cnt_4x4/8x8/16x16` = 0. Use `slice_mode` 0 (one slice control with all MBs = one slice)
  and SSD counts ≥ 1 (T2.05e).
- **Over-reads/-writes:** the engine reads 8-12 KB past the input's chroma plane, and writes the reconstructed picture
  with rows rounded up. Give the input 64 KB of slack and round the recon planes to 32 rows (T2.04, T2.09).
- **Reconstructed pictures:** block-linear with block_height 2 (`tiled_16x16` 0) works, as the reference client uses;
  the doc's "tiled_16x16 must be 1 for refpics" is not needed on Ada. Each plane gets its own method
  (`SET_OUT_REF_PIC_LUMA` / `_CHROMA`, `SET_IN_CUR_PIC` / `_CHROMA_U` / `_CHROMA_V`), with surface offsets 0.
- **Status:** `error_status` 2 with `ucode_error_status` 0 shows up on good frames. Real errors carry a code from
  `ERROR_CODES` (e.g. 3 `E_PICSETUP_OFFSET`, 13 input missing, 14 history missing), and a hang shows as `error_status` 1.
- The magic may be 0xC9B70006 (nvenc_drv.h) or 0xCFB70006 (what the reference client writes); both work.
- **Debugging:** `chan_fault()` gives the MMU fault address (`NV906F_CTRL_CMD_GET_MMU_FAULT_INFO`). RM's RC error log
  (`tools/rcerr`, root; `scripts/prbdecode.py`) has no NVENC detail. `trace/nvtrace.c` records a working client (RM
  escapes, pushed methods, buffers), and `encode1` with `NVENC_REPLAY` replays a captured setup.
- Speed (one IDR, intra 4x4 + 16x16, doorbell to semaphore): 2.0 ms at 1080p, 7.9 ms at 4K.

## M3 notes (details in docs/reports/T3.*)

- `src/enc.c` is the encoder session: `enc_open` / `enc_headers` / `enc_load_nv12` / `enc_encode`, synchronous.
- **References:** DPB of `num_refs` + 1 recon slots; `SET_IN_REF_PIC<slot>_LUMA/_CHROMA`; `l0[i]` = **2 x slot**
  (field-indexed), unused -2; `temp_dist_l0[i]` = i+1. **Every P slice must set `num_ref_idx_active_override_flag`**
  (with the PPS default count instead, the stream breaks at the first picture with all refs). POC type 2,
  `pic_order_cnt_lsb` = 2 x pictures since IDR. One colocated buffer (in = out), two ME predictor buffers swapped
  per frame.
- **ME/MD** follow the reference client's values (temporal + spatial + constant hints, stamps, FBM settings;
  16x16 intra only, RDO 1). That is also faster than M2's settings (1080p IDR 1.1 ms instead of 2.0).
- **Rate control:** RCMODE 1/2 (picture VBR/CBR) and 17/18 (MB-row) work with `Ravg` = `R` = bits per pixel per frame
  (23.8), `framerate` = fps x 256, CPB sizes in bits, `SET_IO_RC_PROCESS` zeroed at open. CBR lands within ±5 %;
  a one-frame VBV keeps every frame (IDRs included) under bitrate/fps. **`single_frame_VBV` = 1 breaks it** (3x the rate).
- **Slices:** `slice_mode` 0 with one slice control per slice. `slice_mode` 1 hangs the engine. `max_slice_size`
  has no effect yet.
- Speed: 1080p P-frames ~1.0 ms, 4K ~4 ms (doorbell to semaphore, spin wait).

## M4 notes (details in docs/vt-encoder.md, docs/reports/T4.*)

- The encoder plugin interface: factory `(codec, allocator, &obj)`, a CMBaseObject whose vtable's third pointer is a
  **4-byte-packed** encoder class with startSession / encodeFrame / completeFrames / copySupportedPropertyDictionary;
  properties via the CMBaseObject set/copyProperty; output with `VTEncoderSessionEmitEncodedFrame`.
- `libnvencvt.dylib` asks for NV12 (VideoToolbox converts BGRA), opens `enc.c` on the first frame, emits AVCC samples
  synchronously. AverageBitRate → MB-row CBR (2-frame VBV in real-time mode).
- Each session opens its own RM client: `rm_open` must close the fds its connections are bound to (it didn't, and
  sessions failed after 121), and CPU mappings are released NVK's way (`rm_unmap_cpu`).
- In the plugin bundle the dylib needs a real ad-hoc signature (`codesign -f -s -`) before the bundle is re-signed.
- Remaining CPU cost per frame: VideoToolbox's BGRA→NV12 conversion (when the app hands BGRA), our CPU tiling, and
  the spin-wait. M5 removes the last two.

## M5 notes (details in docs/reports/T5.*)

- NVENC reads only its own input layout; `input_bl_mode` is ignored, and classic GOB or pitch-linear input doesn't
  work. A copy engine can't do the conversion, so the input path stays on the CPU for now.
- Tiling: SSE loads, non-temporal stores in destination order, two threads: 4K 0.65 ms, 1080p 0.19 ms.
- Completion: RM OS event on `NV2080_NOTIFIERS_NVENC<n>`, waited with the user client's selector 6. NVENC's own
  `AWAKEN` bit raises nothing; the host must acquire the semaphore and then run `NON_STALL_INTERRUPT`. Same latency,
  CPU during the wait ~0.1 ms instead of the whole encode time.
- Async: `enc_submit` / `enc_wait`, up to 3 frames in flight (per-frame input/setup/bitstream/status); libnvencvt
  emits from a completion thread. Throughput is then bound by the engine: 4K 4.0 ms, 1080p 1.0 ms per frame.
- Two engines: sessions on NVENC0 and NVENC1 run in parallel without slowing each other (M6/T6.01).
- **Streaming apps want the output inside the encode call:** Parsec picks up an encoded frame only on its next encode
  call if the callback hasn't run yet (one frame period of latency). libnvencvt emits before `encodeFrame` returns in
  RealTime mode; Parsec then reports 4.7-4.8 ms at 4K. Parsec feeds NV12 (420f).
- BGRA input is converted by us (vImage per band + tiler, 4K 3.3 ms) instead of by VideoToolbox (~10 ms).
- The SPS carries a VUI (BT.709, no reordering).
- A GPU (Metal compute) tiling pass is correct but slower here: 1.9 ms wall at 4K vs 0.87 ms on the CPU, because
  IOSurfaces and mapped buffers are in system RAM on this driver. Worth revisiting only if capture surfaces move to VRAM
  (`tools/gpuswizzle.m`, T5.06).

## M6 notes (details in docs/reports/T6.*)

- **HEVC uses the H.264 methods** with `SET_APPLICATION_ID` 3 and `SET_CONTROL_PARAMS` codec type **4** (not in
  `clc9b7.h`); `pic_control.codec` is **5**. No colocated-data methods. Picture setup `nvenc_h265_drv_pic_setup_s`
  (1024 B); ME control is the H.264 struct; MD/quant/slice controls are H.265 ones. Values from observing nullmoth's
  `libnvenc_hevc` at runtime (`trace/nmreg.c` registers it in a test process).
- **No sign data hiding**: the engine doesn't apply it even when `pps_data` asks; a PPS that signals it makes every
  residual decode wrong (T6.05).
- **Code whole 32x32 CTBs** (conformance window crops): a partial last CTB row/column decodes wrong. Reference/output
  surfaces get the coded size, the input the display size (the engine pads). History buffer: 768 B per CTB column.
  Reference slots 64 KB larger than the planes (as nullmoth's).
- **`slice_control.slice_type` is numbered like `pic_type`** (0 P, 1 B, 2 I), not as the header comment says; with 1
  the first P picture faults.
- **References**: `SET_IN_REF_PIC<i>` by position in list 0 (`poc_entry_l0[i] = i`), `l0[i]` = 2 x DPB slot,
  `temp_dist_l0[i]` = POC distance (unused -1), `dist_scale_factor[td ref][tb ref]` = HEVC distScaleFactor (unused rows
  -1), **`same_poc` = 8x8 bit matrix, diagonal set** (0x201 for two refs; nullmoth's 1 only works with one), inline
  short-term RPS per P slice. 1-3 references work (T6.11).
- **Announce spare DPB pictures**: with `max_dec_pic_buffering` = refs + 1, a hardware decoder (AMD Polaris, macOS
  Sonoma) overwrote references still in use (ghosting on moving windows) while software decoders were bit-exact;
  `libnvencvt` announces refs + 3 (T6.12).
- **Main 10** (T6.14): `bit_depth_minus_8` 2, 16-bit planes with the pitch fields in samples, field offsets doubled,
  minQP -12, transform skip off, two undocumented values; input is P010 as CoreVideo has it (high bits). Parsec sends
  l10r (10-bit RGB): converted in the tiling pass, or VideoToolbox's Metal conversion adds 1-2 frame periods.
- Results: 4K QP 22/25 HEVC 30 Mbit/s at 50.2 dB vs H.264 52 Mbit/s at 48.7 dB, same encode time (~4.1 ms); CBR/VBR/
  low latency within 10 % of target; Parsec HEVC encode latency 4.4-5.3 ms at 4K.

