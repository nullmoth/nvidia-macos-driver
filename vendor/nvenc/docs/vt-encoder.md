# The VideoToolbox encoder plugin interface (as worked out for M4)

Sources: Apple's SPI declarations published in WebKit (`vendor/webkit-spi/CMBaseObjectSPI.h`,
`VTVideoDecoderSPI.h`: CMBaseObject machinery and the *decoder* plugin interface), VideoToolbox's export table
(`dyld_info -exports`), black-box probing with our own stub encoder (`tools/vtprobe.m`), and the layout of the encoder
object nullmoth's `libnvenc_h264` hands to VideoToolbox (observed at runtime by `trace/nvtrace.c` with
`NVTRACE_VT=1`; no disassembly).

## Registration

    OSStatus VTRegisterVideoEncoderWithInfo(FourCharCode codec, CFDictionaryRef info, CreateInstanceFn factory);
    info: CMClassImplementationID (encoder ID), CMClassImplementationName, VTRating (nullmoth: 1000),
          VTIsHardwareAccelerated
    OSStatus factory(FourCharCode codecType, CFAllocatorRef allocator, VTVideoEncoderRef *encoderOut);

Per process; a session picks the encoder by `kVTVideoEncoderSpecification_EncoderID`, or by rating among the
encoders for the codec.

## The encoder object

`CMDerivedObjectCreate(allocator, &vtable.base, VTVideoEncoderGetClassID(), &obj)`; private data in
`CMBaseObjectGetDerivedStorage(obj)` (`CMBaseClass.derivedStorageSize` bytes; nullmoth: 0xa0).

    vtable (default alignment):  { const void *reserved = NULL; const CMBaseClass *base; const EncoderClass *enc; }
    CMBaseClass (pack 4):        version 1, derivedStorageSize, equal NULL, invalidate, finalize, copyDebugDescription,
                                 copyProperty, setProperty, notificationBarrier NULL, protocolTable NULL
    EncoderClass (pack 4!):      { uint32_t version = 1;
                                   OSStatus (*startSession)(VTVideoEncoderRef, VTEncoderSessionRef, CMVideoDimensions dims);
                                   OSStatus (*encodeFrame)(VTVideoEncoderRef, VTEncoderFrameRef frame, CVImageBufferRef image,
                                                           CMTime pts, CMTime duration, CFDictionaryRef frameProperties,
                                                           VTEncodeInfoFlags *infoFlagsOut);
                                   OSStatus (*completeFrames)(VTVideoEncoderRef, CMTime completeUntilPresentationTimeStamp);
                                   OSStatus (*copySupportedPropertyDictionary)(VTVideoEncoderRef, CFDictionaryRef *out);
                                   NULL ... }

The encoder class is packed to 4 bytes like CMBaseClass: the first function pointer is at offset 4. (Unpacked, it
sits at 8 and VideoToolbox reads garbage; that was the first crash.) nullmoth's class has exactly these four
functions; slots 5+ are NULL and never called by VideoToolbox in the flows tried.

## Call flow (VTCompressionSession, 256x128 H.264)

1. factory → `startSession(enc, session, {256, 128})` (two more argument registers hold leftovers; unused).
2. `copySupportedPropertyDictionary` is called on creation, before every property set/get and many times before the
   first frame. Its result has the shape of `VTSessionCopySupportedPropertyDictionary` (key → {PropertyType,
   ReadWriteStatus, ...}). Unlisted keys fail with `kVTPropertyNotSupportedErr` (-12900).
3. `VTSessionSetProperty` / `CopyProperty` of a listed key → the CMBaseObject `setProperty` / `copyProperty`
   (`AverageBitRate` arrives as a CFNumber, `RealTime` as a CFBoolean). `PrepareToEncodeFrames` reads properties.
4. `encodeFrame(enc, frame, pixelBuffer, pts, duration, frameProperties, &infoFlags)` per frame; `frame` is an opaque
   token (1, 2, ...). Output goes back with
   `VTEncoderSessionEmitEncodedFrame(session, frame, OSStatus, VTEncodeInfoFlags, CMSampleBufferRef)`. Called inside
   `encodeFrame`, the app's output handler runs synchronously.
5. `VTCompressionSessionCompleteFrames` → `completeFrames(enc, kCMTimeInvalid)`.
6. Invalidate → base `invalidate`, then `finalize`.

Other exports a full encoder will want: `VTEncoderSessionSetPixelBufferAttributes` (ask for NV12),
`VTEncoderSessionCreateVideoFormatDescription`, `VTEncoderSessionCreateCMBlockBuffer`. The format description can
also come from the public `CMVideoFormatDescriptionCreateFromH264ParameterSets`, as the probe does.

## Rate control (T7.05, T7.08, T7.09)

With a bitrate `libnvencvt` runs the engine's MB-row CBR (rc 18; `DataRateLimits` above the average: MB-row VBR, rc 17),
NVIDIA's CBR fields, and the RC process buffer initialized as NVIDIA's driver does it (complexity weights, GOP counts, CPB
half full: on target and no oversized first IDR). `NVENCVT_TWOPASS=1` turns on NVIDIA's two-pass mode (`EncConfig.two_pass`:
a first pass per picture writes statistics the second reads; HEVC I / P only); off by default, because over long runs it
gives -0.25..+0.65 VMAF at equal rate for 2x (H.264) / +7 % (HEVC) engine time.

## B pictures (T7.03)

`libnvencvt` reorders when `AllowFrameReordering` is true (VideoToolbox's default; AVAssetWriter never sets it, OBS sets it
from its "B-frames" box) and `RealTime` is not true (Parsec sets it), for H.264 Main / High in 4:2:0 and HEVC; not for
Baseline, 4:4:4 or AV1. Two B pictures between anchors (`NVENCVT_BFRAMES=0..3`); not real time also turns on NVIDIA's slower
H.264 ME / MD controls (`EncConfig.quality`; `NVENCVT_FAST=1` keeps the fast ones).

- `encodeFrame` retains the pixel buffer of a frame that is not an anchor and returns (asynchronous). When `bframes + 1`
  frames wait, the newest is encoded first (P), then the others in display order as B pictures (`enc_submit_ex` with their
  display index; POC type 0). A key frame (first frame, `ForceKeyFrame`, `MaxKeyFrameInterval`) first encodes the waiting
  frames as a shorter group, then itself as an IDR alone, so no B picture refers across it. `completeFrames` and
  invalidate encode a partial group the same way. After an encode error (enc.c then starts over with an IDR) the waiting
  frames go in display order as IDR / P.
- Samples come out in decode order with a decode time stamp = the presentation time of the picture one before in
  display order (the first: its PTS minus one duration). With non-reference B pictures `num_reorder_frames` is 1: a B
  picture decodes when it is shown, an anchor one picture before the first B picture that waits for it. B samples carry
  `IsDependedOnByOthers = false`.

## Colour, HDR and rate properties (T7.05)

- Colour: `ColorPrimaries` / `TransferFunction` / `YCbCrMatrix` (CoreVideo strings, mapped to H.273 codes: BT.709, BT.601 625 / 525,
  BT.2020, DCI-P3, P3-D65; BT.709 / 240M / linear / sRGB / BT.2020 / PQ / ST 428 / HLG), else the first frame's attachments,
  else BT.709. Written to the VUI / AV1 colour config and to the format description.
- HDR: `MasteringDisplayColorVolume` (24 bytes) and `ContentLightLevelInfo` (4 bytes) in the SEI payload layout; sent with every
  key frame (SEI NAL unit / AV1 metadata OBUs) and as format description extensions.
- Rate: `AverageBitRate` alone = MB-row CBR (VBV one second, two frames in real time); `ConstantBitRate` the same at its rate;
  `DataRateLimits` [bytes, seconds] above the average = MB-row VBR capped there. `PrioritizeEncodingSpeedOverQuality` = the fast
  controls when not real time.
