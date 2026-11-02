# NVIDIA Linux driver: B-frames, references, 8K (2026-10-20)

Second tracing session in the Debian VM that owns the RTX 4090 (driver 580.105.08, ffmpeg 7.1), same method as
`docs/traces/av1-linux` (`trace-linux/nvtrace.so`: RM ioctls, pushed methods, buffers; no disassembly; the user's decision).
Per run: `log.*.gz`, `out.*`, and per EXECUTE `dump/execN-710.bin` (picture setup with its controls), `-718` (status),
`-724` (RC process), `-70c` (RC data). Source: `testsrc2`, `-rc constqp`.

| run | encode |
|---|---|
| hb2s / hb2 | h264_nvenc 256x128 / 1920x1080, 6 frames, `-preset p4 -qp 28 -bf 2 -b_ref_mode disabled` |
| hp1s | h264_nvenc 256x128, 4 frames, P only |
| hbr | h264_nvenc 640x360, 8 frames, `-bf 3 -b_ref_mode middle` |
| vb2s / vb2 | hevc_nvenc 256x128 / 1920x1080, 6 frames, `-bf 2 -b_ref_mode disabled` |
| vbr | hevc_nvenc 640x360, 8 frames, `-bf 3 -b_ref_mode middle` |
| ab2s / ab2 | av1_nvenc 256x128 / 1920x1080, 6 frames, `-qp 100 -bf 2 -b_ref_mode disabled` |
| ahq | av1_nvenc 640x360, 8 frames, `-preset p7 -tune hq` (references) |
| abr | av1_nvenc 640x360, 8 frames, `-bf 3 -b_ref_mode middle` |
| a8k | av1_nvenc 7680x4320, 2 frames, `-preset p1 -tune ll` (tiles) |

H.264 findings (used in `src/enc.c`, templates in `src/h264enctmpl.h` from hb2s via `scripts/h264enc-template.py`):
ME / MD / quant control blocks per picture type (IDR, P, B) do not depend on the picture size; B pictures: temporal
direct, list 1 / bi partitions, `dist_scale_factor[refidx1][refidx0]` = H.264 DistScaleFactor (-1 unused, 256 in P),
`diff_pic_order_cnt_zero` patterns, `temp_dist_lX` = POC difference / 2; the quant control (dead zones) is the same for all.
