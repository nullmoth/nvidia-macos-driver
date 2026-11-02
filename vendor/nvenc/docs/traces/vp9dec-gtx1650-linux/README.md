# NVIDIA's Linux VP9 decoder on the GTX 1650 (TU117, NVDEC 0xc4b0), 2026-10-21

This is the fifth tracing session and the **first decoder trace**. The user approved it to settle one question: why our VP9
decoder failed on the libvpx vectors `15-segkey_adpq` and `19-skip` on this card. Their frames that keep the previous
segmentation map (`segmentation_update_map` 0) decoded as if segmentation were off.

**Setup.** `99-Service-Debian13-4090` with the GTX 1650 passed through. nouveau was unbound at run time, then the
`nvidia` driver 580.105.08 was loaded. The client was ffmpeg 7.1 with `-hwaccel cuda` (NVIDIA's NVDEC through cuvid).

**Tracer.** `trace-linux/nvtrace.so` now also follows channels with an NVDEC object (class `0xXXb0`) and names its methods
from `clc5b0.h`. `NVTRACE_ONLY=404,5c0,...` limits the dumps to those methods' buffers.

## Checks

NVIDIA's hardware decode is bit-exact against ffmpeg's software decoder on both vectors: per-frame MD5s in
`*.hw.md5` / `*.sw.md5`, produced by `hwcmp.sh`. So this card can decode them.

## Capture method

The driver rotates 4 picture-setup slots and 4 bitstream buffers. When the tracer falls behind, a dump holds a later
picture's setup. With every buffer dumped, it fell behind by whole ring cycles, even with `-re`. The reliable captures
decode streams cut to their first 3 frames (`skip3.ivf`, `segk3.ivf`), so the slots cannot be reused.

Per run (`t-skip3`, `t-segk3`):
- `log.txt.*.gz`: escapes, pushes and the method state at every EXECUTE;
- `exec<N>-404.bin`: the picture setup;
- `exec<N>-5c0.bin`: the probabilities;
- `exec<N>-5c4.bin`: the counters;
- `seg-exec<N>-5c8/5cc.bin`: the first 64 KB of the segment read / write buffers;
- `bs<N>.bin`: the first 4 KB of the bitstream.

## Findings

- **NVIDIA's driver does not send method 0x4c8.** That is Ada's `SET_INTRA_TOP_BUF_OFFSET`, and it is not in `clc5b0.h`.
  Our decoder did send it. Given an address there, Turing's engine:
  - wrote 32 bytes per superblock into that buffer;
  - decoded frames that keep the segmentation map with segment 0;
  - faulted (no completion) when the buffer was smaller than 32 bytes per superblock: 2048x1088 and 4K.

  Without the method both vectors are bit-exact on every frame, and 4K decodes with the normal buffers. Fix:
  `src/vp9dec.c` (`d->turing`).
- **Segment maps:** the driver manages them as we do. After a frame that writes a map, later frames read it and write
  into the other buffer. The map's layout and contents equal ours.
- **Segmentation tree and prediction probabilities:** on frames that keep the map, NVIDIA leaves the values of the frame
  context's last user (we write 255). This made no difference.
- **Bitstream:** NVIDIA's starts after the uncompressed header, so it includes the compressed header (`stream_len` 46
  against our 32 for `19-skip` frame 2). How the engine finds the tiles then was not captured: the tile buffer 5d0 was not
  dumped. Ours starts at the tile data, which also works.
- **`SET_CONTROL_PARAMS` = `0x0005_2xd9`:** VP9, GP timer, error concealment, MB timer, `ERROR_FRM_IDX` = picture slot,
  plus the undocumented bits 16 and 18. None of them changed our results.
- **`gptimer_timeout_value`** is `0x4d3f640` (ours: 0, so the microcode uses its default).
- **Filter-buffer layout:** the BSD control area comes before the filter-above area (offsets 0x24 / 0x27 x 256 bytes at
  208x144). These are offsets only.
- Frames 64 pixels or less still answer status 0x10 on this engine (unrelated to 0x4c8), so `vp9dec` still refuses them.
