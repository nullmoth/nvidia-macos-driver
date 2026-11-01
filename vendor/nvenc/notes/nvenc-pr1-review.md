# Review of PR #1: AV1 / VP9 decode on GA10x, and the decoder's own output pool

Tested on an RTX 4090 (Ada) under macOS 26.7.1 with NullMoth's driver 2.5.3 and the mickys overlay 2.5.3-mickys.5, on
2026-10-23, against `main` (89a0010; the PR merges onto it cleanly). We have no RTX 30 card, so the GA10x part is reviewed
but not run.

## Summary

- **GA10x registration (part 1):** reads right. On Ada and TU117 it registers exactly what it did before. Not tested on
  RTX 30.
- **Decoder output pool (part 2):** the speedup reproduces on Ada: ffmpeg's VideoToolbox hwaccel decodes 4K AV1 at
  **103-105 fps instead of 37-38**, because VideoToolbox no longer copies every picture. But the pool takes the client's
  destination attributes verbatim, and the decoder copies its picture in its own format and size, so **a client asking
  for another format gets a broken picture**: a 10-bit HEVC stream decoded for an 8-bit NV12 request comes out at 10 dB
  PSNR. A guard (below) keeps the speedup and stays bit-exact in every case we tried.
- Merged into `main` (1d7b06c), with the guard as the follow-up commit 1722c8f.

## Results

Each build was installed in place of the plugin bundle's `libnvencvt.dylib` in turn (see "How it was tested").
"emit" is the time per frame spent inside `VTDecoderSessionEmitDecodedFrame`; PSNR is our hardware decode against Apple's
software decoder (99 dB = identical).

| case | `main` | PR as submitted | PR + guard |
|---|---|---|---|
| `ffmpeg -hwaccel videotoolbox -c:v av1`, 4K AV1 (crowd, 2160p50) | emit 17.0 ms, **37-38 fps** | emit 0.13 ms, 102 fps | emit 0.13 ms, **103-105 fps** |
| 4K HEVC, ffmpeg-style request (NV12 + size + OpenGL-compatible IOSurface) | 99 dB, emit **32.2 ms** | 99 dB, 0.35 ms | 99 dB, **0.33 ms** |
| 1080p HEVC **10-bit**, client asks for 8-bit NV12 + size | 99 dB, 12.6 ms | **10.3 dB (broken)** | 99 dB, 11.3 ms |
| 1080p H.264, client asks for BGRA + size | 99 dB, 10.5 ms | 99 dB, 10.5 ms | 99 dB, 9.2 ms |
| 1080p H.264, client asks for 1280x720 | 99 dB, 8.6 ms | 99 dB, 9.4 ms | 99 dB, 7.2 ms |
| 1080p H.264, NV12 without a size | 99 dB, 0.28 ms | 99 dB, 0.27 ms | 99 dB, 0.27 ms |

The 4K AV1 frames are identical across the three builds.

## The problem with the verbatim pool

`set_output` builds the pool from whatever `VTDecoderSessionGetDestinationPixelBufferAttributes` returns, and
`output_buffer` then hands those buffers to the copy, which writes the picture in the decoder's own pixel format and size.
When the client's format is not the decoder's (an 8-bit request for a 10-bit stream, BGRA, a list of formats) or its size
is not the picture's (a scaled request, or a stream that changes size), the buffer does not match what is written into it.
In our test the 10-bit case visibly broke (10 dB); BGRA and the scaled size came out right here, but the decoder writing
into a buffer of another layout is not something to rely on. A request without a size also gets no pool at all (a pixel
buffer pool needs dimensions), so plain NV12 requests did not benefit.

To reproduce (10-bit HEVC, the client asking for 8-bit NV12 with the picture size; `vtdecode` options from the diff below):

```sh
VTDECODE_WH=1 ./vtdecode crowd1080-hevc10.mp4 60     # PR as submitted: psnr_hw_vs_sw avg 10.25 dB
```

## The guard

Our own pool only when its buffers hold the picture as it is: the client's pixel format is the decoder's (one format,
not a list) and its size, if it names one, is the picture's. The size is set explicitly so a request without one gets a
pool too. Anything else keeps the session's pool and VideoToolbox's own conversion, as before. `NVDEC_NOOWNPOOL=1`
switches the pool off.

```diff
diff --git a/vt/nvencvt_dec.c b/vt/nvencvt_dec.c
index d251876..e15cd74 100644
--- a/vt/nvencvt_dec.c
+++ b/vt/nvencvt_dec.c
@@ -455,6 +455,7 @@ static OSStatus set_output(DState *s, VTVideoDecoderSession session, CMVideoForm
     // VideoToolbox copies every frame into a compatible one inside EmitDecodedFrame (ffmpeg asks for OpenGL-compatible
     // IOSurfaces: 18.7 ms per 4K frame).
     CFDictionaryRef dst = VTDecoderSessionGetDestinationPixelBufferAttributes(session);
+    if (dst) CFRetain(dst);   // also for our own pool below (released at its end)
     CFNumberRef dpf = dst ? CFDictionaryGetValue(dst, kCVPixelBufferPixelFormatTypeKey) : NULL; OSType want = 0;
     if (!getenv("NVDEC_OWNATTRS") && dpf && CFGetTypeID(dpf) == CFNumberGetTypeID() && CFNumberGetValue(dpf, kCFNumberSInt32Type, &want) && want == pf) {
         CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(NULL, 0, dst);
@@ -469,25 +470,35 @@ static OSStatus set_output(DState *s, VTVideoDecoderSession session, CMVideoForm
     }
     st = VTDecoderSessionSetPixelBufferAttributes(session, attrs);
     CFRelease(attrs); CFRelease(ios); CFRelease(npf); CFRelease(nw); CFRelease(nh);
-    // Our own pool, made with the client's destination attributes verbatim. A client that asks for an
+    // Our own pool, made with the client's destination attributes verbatim (PR #1). A client that asks for an
     // OpenGL-compatible buffer (ffmpeg, mpv: kCVPixelBufferIOSurfaceOpenGLTextureCompatibilityKey) cannot have one on
     // this system: CVPixelBufferIsCompatibleWithAttributes answers 0 for every buffer, even one made with those very
-    // attributes. VideoToolbox then transfers every decoded picture into a buffer of its own (about 14 ms per 4K frame
-    // through the GPU driver's Metal path), instead of handing the decoder's buffer over (0.05 ms). The picture is
-    // copied into the buffer we hand out, so if that buffer carries the destination's own attributes VideoToolbox has
-    // nothing to transfer. (T8.07 measured the transfer; the fix is here.)
+    // attributes. VideoToolbox then transfers every decoded picture into a buffer of its own (14-32 ms per 4K frame
+    // through the GPU driver's Metal path, RTX 3060 Ti and RTX 4090), instead of handing the decoder's buffer over. The picture is copied into the
+    // buffer we hand out, so if that buffer carries the destination's own attributes VideoToolbox has nothing to transfer.
+    // Only when such a buffer holds our picture as it is: the client's pixel format is ours (one format, not a list) and
+    // its size, if it names one, is the picture's; anything else (BGRA, 8-bit for a 10-bit stream, a scaled size) keeps
+    // the session's pool and VideoToolbox's conversion. NVDEC_NOOWNPOOL=1 leaves it out. (T8.07 measured the transfer.)
     if (s->ownPool) { CVPixelBufferPoolRelease(s->ownPool); s->ownPool = NULL; }
-    CFDictionaryRef dpattr = VTDecoderSessionGetDestinationPixelBufferAttributes(session);
-    if (dpattr) {
-        CVPixelBufferPoolRef pool = NULL;
-        if (CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL, dpattr, &pool) == kCVReturnSuccess && pool) s->ownPool = pool;
-        else if (pool) CVPixelBufferPoolRelease(pool);
-        if (gDLog) { CFStringRef ds = CFCopyDescription(dpattr); char buf[1024] = "(none)";
-            if (ds) { CFStringGetCString(ds, buf, sizeof buf, kCFStringEncodingUTF8); CFRelease(ds); }
-            for (char *c = buf; *c; c++) if (*c == '\n') *c = ' ';
-            DLOG("  destination attributes: %s", buf);
-            DLOG("  our own pool: %s", s->ownPool ? "yes" : "no (the session's pool is used)"); }
+    if (dst && !getenv("NVDEC_NOOWNPOOL")) {
+        int dw = s->w, dh = s->h; CFNumberRef x;
+        if ((x = CFDictionaryGetValue(dst, kCVPixelBufferWidthKey)) && CFGetTypeID(x) == CFNumberGetTypeID()) CFNumberGetValue(x, kCFNumberIntType, &dw);
+        if ((x = CFDictionaryGetValue(dst, kCVPixelBufferHeightKey)) && CFGetTypeID(x) == CFNumberGetTypeID()) CFNumberGetValue(x, kCFNumberIntType, &dh);
+        const bool fits = want == pf && dw == s->w && dh == s->h;
+        if (fits) {
+            CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(NULL, 0, dst);   // the size made explicit (a pool needs one)
+            CFNumberRef w2 = CFNumberCreate(NULL, kCFNumberIntType, &s->w), h2 = CFNumberCreate(NULL, kCFNumberIntType, &s->h);
+            CFDictionarySetValue(m, kCVPixelBufferWidthKey, w2); CFDictionarySetValue(m, kCVPixelBufferHeightKey, h2);
+            CFRelease(w2); CFRelease(h2);
+            CVPixelBufferPoolRef pool = NULL;
+            if (CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL, m, &pool) == kCVReturnSuccess && pool) s->ownPool = pool;
+            else if (pool) CVPixelBufferPoolRelease(pool);
+            CFRelease(m);
+        }
+        DLOG("  our own pool: %s", s->ownPool ? "yes (the client's destination attributes)" : fits ? "no (CVPixelBufferPoolCreate failed)"
+                                    : "no (the client's format or size is not the picture's: the session's pool, VideoToolbox converts)");
     }
+    if (dst) CFRelease(dst);
     CFMutableDictionaryRef col = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
     // colour description and HDR metadata from the format description (the extension keys are the attachment keys), as
     // Apple's decoders attach them
```

And two `vtdecode` options used for the client cases above (the picture size, and an OpenGL-compatible request as
ffmpeg makes it):

```diff
diff --git a/tools/vtdecode.m b/tools/vtdecode.m
index 46b00b5..ae2e2fe 100644
--- a/tools/vtdecode.m
+++ b/tools/vtdecode.m
@@ -7,6 +7,7 @@
 //   VTDECODE_SW=1: with VTDECODE_REF, compare the software decoder (Apple's) instead
 //   VTDECODE_AVREAD=1: decode through AVAssetReader (alwaysCopiesSampleData NO) instead, compared with VTDECODE_REF
 //   VTDECODE_PF=x420: ask for that output format (default 420v)
+//   VTDECODE_WH=1 (or WxH): ask for the picture size too (or another size); VTDECODE_GL=1: OpenGL-compatible IOSurfaces (ffmpeg)
 //   VTDECODE_ATT=1: print the first decoded frame's attachments (colour, HDR metadata)
 //   VTDECODE_SPEED=hw|sw: only the decode speed of the hardware / software decoder (frames released at once)
 //   VTDECODE_ASYNC=1: allow asynchronous decompression (as AVFoundation does)
@@ -37,7 +38,16 @@ static int decode(NSArray *samples, CMFormatDescriptionRef fd, bool hw, Out *o,
                             : @{ (id)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: @NO };
     const char *pfs = getenv("VTDECODE_PF");   // the output format to ask for (four characters, e.g. x420); default NV12 420v
     const OSType pf = pfs && strlen(pfs) == 4 ? (OSType)pfs[0] << 24 | (OSType)pfs[1] << 16 | (OSType)pfs[2] << 8 | (OSType)pfs[3] : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
-    NSDictionary *attrs = @{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf) };
+    NSMutableDictionary *attrs = [@{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf) } mutableCopy];
+    if (getenv("VTDECODE_WH")) {   // also the picture size, as ffmpeg and AVFoundation ask (VTDECODE_WH=WxH: another size)
+        CMVideoDimensions d = CMVideoFormatDescriptionGetDimensions(fd); int w = d.width, h = d.height;
+        if (strchr(getenv("VTDECODE_WH"), 'x')) sscanf(getenv("VTDECODE_WH"), "%dx%d", &w, &h);
+        attrs[(id)kCVPixelBufferWidthKey] = @(w); attrs[(id)kCVPixelBufferHeightKey] = @(h);
+    }
+    if (getenv("VTDECODE_GL")) {   // OpenGL-compatible IOSurfaces, as ffmpeg's videotoolbox hwaccel asks
+        attrs[(id)kCVPixelBufferIOSurfacePropertiesKey] = @{};
+        attrs[(id)kCVPixelBufferIOSurfaceOpenGLTextureCompatibilityKey] = @YES;
+    }
     VTDecompressionOutputCallbackRecord rec = { cb, o };
     VTDecompressionSessionRef s = NULL;
     OSStatus st = VTDecompressionSessionCreate(NULL, fd, (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)attrs, &rec, &s);
```

## How it was tested, and a pitfall

**`DYLD_INSERT_LIBRARIES` did not test the injected build here.** VideoToolbox's hardware path creates a Metal device, so
NullMoth's plugin loads, and the plugin loads its own `libnvencvt.dylib` from its bundle. Both register the same decoders
at the same rating, the bundle's last, and it wins. That happens for ffmpeg too. On an RTX 30 card with the overlay's
current library the bundle copy registers no VP9 / AV1 decoder (it does not know GA10x), so injection probably did test
your build there; on Ada it does not. We tested by putting each build into the bundle in place of the installed one
(ad-hoc signed, moved into place, bundle re-signed), running everything without injection, and putting the original back.

## Smaller notes

- The PCI range 0x2200-0x25ff also covers some Hopper data-centre IDs (H100 is 0x2330). Harmless on a Mac.
- `docs/ga10x.md` names NullMoth 2.4.3 as the driver; 2.5.3 is current.
- Unrelated to this PR, the test showed a problem on `main` as well: through ffmpeg's hwaccel our AV1 decoder delivers only
  51 distinct pictures in 299 frames of the 4K clip (the rest are repeats). It is ours to fix; we mention it in case you
  saw the same.
