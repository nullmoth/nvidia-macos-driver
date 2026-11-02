# GA10x (RTX 30): AV1 and VP9 decode through NVDEC on a 3060 Ti

**What was ported to consumer Ampere (GA10x) and measured on an RTX 3060 Ti (GA104, 3840x2160 / 2560x1440 / 1920x1080).**
The rest of the repository was written for Ada (RTX 40) and the GTX 1650 (TU117).

NVIDIA's Video Codec SDK 11.0 added "Driver support for GA10x GPUs" and "AV1 Main profile decoding up to level 6.0"
(the SDK's own NVDEC capability table has a "GA10x and Ada GPUs" column with AV1 = Y), so the RTX 30 series decodes AV1
in hardware, like Ada. The NVDEC *method* IDs are the same in both class files: of the 391 methods in `clc7b0.h`
(Ampere) and `clc9b0.h` (Ada) every ID is equal, only the class ID itself differs, and Ada adds two methods we do not use
(the VP9 / AV1 hint dump buffers). So `src/av1dec.c` and `src/vp9dec.c` (written against Ada's `clc9b0.h`) drive the Ampere
engine unchanged. `src/av1dec.c` has always allowed the Ampere class (`0xc7b0`); what was missing was the registration in
the VideoToolbox library.

## What the code needed

**`vt/nvencvt.c`**

- `GPU_AMPERE` (3) next to `GPU_ADA` / `GPU_TU117`.
- The PCI device ID check knows GA10x: 0x2200-0x25ff (GA102 / GA103 / GA104 / GA106 / GA107; the 3060 Ti is 0x2406).
  Before, a card outside Ada's 0x2600-0x28ff and TU117's 0x1f80-0x1fff was "not ours" and the library registered
  nothing at all.
- The RM fallback (`gpu_nvenc_kind`, for a card the registry does not name) knows NVENC 0xc7b7.
- **The encoders stay away on GA10x.** Our encoder setups are written for Ada's NVENC 8.2 and TU117's NVENC 6.6; on
  GA10x the engine is neither, and a registered encoder that cannot start is worse than none (VideoToolbox does not
  fall back once it picked ours). `NVENCVT_AMPERE_ENC=1` registers them anyway, for the day the NVENC 6.6 / 8.2
  differences are worked out. NullMoth's own encoders keep working for RTX 30 cards.

**`vt/nvencvt_dec.c`**

- The **VP9 and AV1** decoders register on GA10x. AV1 is decided by the *NVDEC class* (0xc7b0 / 0xc9b0 have it), not by
  the card's name: a decoder that registers on a card without AV1 would take the session away from Apple's own decoder
  and then fail. VP9 is in the same class of codecs, with the same method IDs as Ada's.
- The H.264 / HEVC / MPEG decoders were written for Ada's NVDEC (0xc9b0) and Turing's (0xc4b0) and were not tried on
  0xc7b0, so they stay out on GA10x (VideoToolbox does not fall back to another decoder once it picked ours).
  `NVENCVT_AMPERE_ALL_DEC=1` registers them all anyway.
- VP9 frames of 64 pixels or less are refused on GA10x as well (TU117's NVDEC cannot do them; NVIDIA lists 128x128 as
  VP9's minimum, and GA10x was not tried with them).

## What you need on the machine

1. **NullMoth's NVIDIA driver** (2.4.3) for the RTX 30 card, with its boot settings (SIP relaxed,
   `amfi_get_out_of_my_way=0x1`). The overlay (mickys' add-on) is what ships our `libnvencvt.dylib` and `nvenc-run`;
   the driver itself is the base it adds to. **Without the driver there is no RM device, so there is no NVDEC at all**
   and no library can decode anything: the whole project talks to the GPU through the driver's RM client.
2. **ffmpeg 8.0 or newer.** The VideoToolbox *hwaccel* only handles AV1 from ffmpeg 8.0 on
   (`av1_videotoolbox_hwaccel`); ffmpeg 7.1 and older have no AV1 in that path at all. VP9 is in it since long before.


## Run

```sh
DYLD_INSERT_LIBRARIES=$PWD/build/libnvencvt.dylib NVENCVT_LOG=1 ffmpeg -hwaccel videotoolbox -i build/AV1.mp4 -f null -
DYLD_INSERT_LIBRARIES=$PWD/build/libnvencvt.dylib NVENCVT_LOG=1 mpv --no-config --hwdec=videotoolbox build/AV1.mp4
```

The library registers itself in its constructor when it is injected (that is what `nvenc-run` does with the overlay's
copy). The overlay's prebuilt `libnvencvt.dylib` does *not* have the GA10x support: either use the one you built
(as above) or replace the overlay's copy with it.

Without the injection, ffmpeg decodes AV1 in software (dav1d / libaom) and VideoToolbox's own AV1 decoder is used by
AVFoundation apps.

## Measured on the RTX 3060 Ti (macOS 26.7.1, ffmpeg 9.0.2)

| stream | decoder | fps | engine | copy | **emit** | total |
|---|---|---|---|---|---|---|
| 4K AV1 (2160p50) | NVDEC (ours) | 50 (0.97x) | 2.7 ms | 0.5 ms | 14-15 ms | 18 ms |
| 4K AV1 | same, pure VideoToolbox client (`tools/vtdecode`) | **282** | 3.1 ms | 0.4 ms | **0.05 ms** | 3.5 ms |
| 4K AV1 | software (libdav1d) | 75 (1.5x) | - | - | - | user 39.1 s / 1000 frames |
| 1080p VP9 | NVDEC (ours) | 139 | 0.75 ms | 0.15 ms | 4.7 ms | 6.1 ms |
| 1080p VP9 10-bit | NVDEC (ours) | 8.4x | 0.91 ms | 0.6 ms | - | 1.6 ms |

The same 4K stream: **user CPU 10.8 s** with NVDEC against **39.1 s** with libdav1d for the same 1000 frames (4x less).

## The 4K finding and its fix: the client's destination attributes

At 4K the picture was handed to the client in **14-15 ms** - inside `VTDecoderSessionEmitDecodedFrame`, i.e. while
VideoToolbox delivers the frame. A `sample` profile of ffmpeg on the 4K stream showed the time in the NVIDIA driver's
surface path: `nvmtl_vk_image_write` (the driver writing the picture into the client's IOSurface), `IOSurfaceClientLock`
/ `IOSurfaceClientSetValue` (IPC into the driver) and `vk_common_QueueSubmit`. The cost is proportional to the picture
size (480p 2.0 ms, 1080p 4.7 ms, 4K 14.5 ms), i.e. about 0.9 GB/s - a PCIe-class transfer through the driver, not a
decode, and not CPU work (the CPU is idle: the profile's hot list is waiting, not running). So a 4K stream at 50/60 fps
did not fit into its frame period (16.7 ms) and the player dropped frames.

**The fix is in the library, not in the client** (`vt/nvencvt_dec.c`): the decoder now makes **its own pool** with the
client's destination attributes verbatim (`CVPixelBufferPoolCreate` with the dictionary
`VTDecoderSessionGetDestinationPixelBufferAttributes` returns) and hands the pictures out of it. The picture is copied
into that buffer anyway, and the buffer now carries the destination's own attributes, so VideoToolbox has nothing to
transfer:

| 4K AV1 (2160p50), 1000 frames | before | after |
|---|---|---|
| emit (inside EmitDecodedFrame) | 15.15 ms | **0.05 ms** |
| decodeFrame total | 18.5 ms | **4.3 ms** |
| frames per second the decoder can do | 54 | **230** |

4K VP9 at 60 fps: 4.2 ms per picture, 145 fps, 0 errors. The pictures are bit-exact against the software decoder
(4K AV1 and 4K/1080p VP9). The frames the player still drops are the display path, not the decode: on a local 4K60 file
mpv drops 38 frames with our decoder and 20 with the software decoder.

**What this means for the driver** (the open item): the transfer is not needed any more, so the per-transfer cost the
driver's own `tools/bench_vt_transfer` measures (a fixed ~25-27 ms per transfer) is out of the picture. Should a client
ever ask for a destination that VideoToolbox can satisfy but the driver makes expensive, the driver's IOSurface upload
path is where to look (a raw 12.4 MB copy on this machine takes 1.9 ms, the transfer 14.5 ms).
