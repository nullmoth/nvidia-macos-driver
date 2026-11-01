# NVIDIA Linux driver encoding AV1 (M12)

Observed 2026-10-19 with `trace-linux/nvtrace.so` (LD_PRELOAD; RM ioctls, pushed NVENC methods, buffers; no
disassembly) in the Debian 13 VM that owns the RTX 4090, driver 580.105.08, ffmpeg 7.1 `av1_nvenc` from `testsrc2`
(the user's decision, docs/tasks.md M12). Per run: `log.gz` (escapes, allocations, pushes, method state at each
EXECUTE), `execN-710.bin` picture setup, `-718` status, `-724` RC process buffer (`-70c` RC data where taken), `out.ivf`.
Buffers are dumped when the tracer decodes the EXECUTE, so ones the engine writes may hold the previous frame's data.

| run | encode |
|---|---|
| k1 | 256x128, 1 frame, `-preset p1 -tune ll -rc constqp -qp 100` |
| p3 / m3 / f3 | 3 frames, same, at 256x128 / 640x360 / 1920x1080 |
| u3 | 3840x2160, 3 frames (two tile columns, three EXECUTEs per frame) |
| t10 | 1920x1080 10-bit (`-pix_fmt p010le`) |
| cbr | 1920x1080, 6 frames, `-rc cbr -b:v 5M`, low latency |
| vbr | 1920x1080, 6 frames, `-preset p4 -rc vbr -b:v 8M -bf 0` |
| seq | 640x360, 20 frames, constqp |
