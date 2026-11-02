# NVIDIA Linux driver on the GTX 1650 (TU117, NVENC 6.6 = class 0xb4b7), 2026-10-20

Fourth tracing session (user: "start with 1", after our HEVC encode faulted on its second P picture on this card), same
method as `docs/traces/bframes-linux` (`trace-linux/nvtrace.so`, driver 580.105.08, ffmpeg 7.1, testsrc2), in
`99-Service-Debian13-4090` with the GTX 1650 passed through (nouveau unbound at run time). Per run: `log.*`, `out.*`, dumps
`dump/execN-7xx.bin` (setup 710, RC data 70c, status 718, RC process 724; the bitstream, history and co-located dumps were
left out for size).

| run | encode |
|---|---|
| g16-hevc-cqp / -cbr | hevc_nvenc 1080p `-preset p4 -rc constqp -qp 28` / `-rc cbr -b:v 6M`, `-bf 0`, 6 frames |
| g16-hevc-4k / -256 | the same at 3840x2160 / 256x128 (constqp), 4 frames |
| g16-hevc10 | Main 10 (`-profile:v main10 -pix_fmt p010le`), 4 frames |
| g16-h264-cqp / -cbr / -bf2 | h264_nvenc 1080p constqp / CBR 6M / constqp with `-bf 2` |

Findings (NVENC 6.6 against Ada's 8.2):
- **HEVC history buffer: 768 x CTB columns x (CTB rows + 1/2) bytes** (`hist_buf_size` 27648 / 1589760 / 6312960 at
  256x128 / 1080p / 4K; Ada: 768 x CTB columns); `src/enc.c` uses it on 0xb4b7.
- **HEVC references by DPB slot:** the reference of `l0[0]` = 2 (slot 1) goes in `SET_IN_REF_PIC1`, as in H.264; on Ada
  HEVC references go by list position. Ours by list position left REF_PIC1 unset when the second P picture's reference was
  in slot 1: **that was the MMU fault** (the history size alone did not fix it).
- **HEVC B pictures: not supported** (hevc_nvenc -bf 2: "No capable devices found"); H.264 B pictures work.
- Setup magic 0xB4B70006 (nvenc_drv.h's NVENC 6.6 value); the setup layout is the same as Ada's.
- The driver's HEVC p4 defaults here: input height = the coded (CTB) height, no source padding, `chroma_skip_threshold_4x4`
  0, intra SSD counts 0, simpler ME (two search shapes, no hint patterns) and MD (intra modes 0x55555557).
