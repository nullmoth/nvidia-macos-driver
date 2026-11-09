#!/usr/bin/env python3
"""The Screen Sharing viewer asks the HEVC decoder it gets for its MaxDecoderPixelRate and falls back to 60 frames per
second when the decoder does not answer. NVEncVT's decoders answer it. This checks the source; with a compiler it also
builds tools/vt_max_pixel_rate_probe.c, which asks the decoder macOS picks on the Mac it runs on (on a stock Apple-silicon
Mac it prints 7365427200, a CFNumber).

usage: test_nvencvt_pixel_rate.py            source checks only
       test_nvencvt_pixel_rate.py --run      also build and run the probe on this Mac"""
import pathlib, platform, shutil, subprocess, sys, tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'vendor/nvenc/vt/nvencvt_dec.c').read_text()
enc = (root / 'vendor/nvenc/vt/nvencvt.c').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


check('the decoder answers MaxDecoderPixelRate with a 64-bit number', 'CFEqual(key, NV_MAX_DECODER_PIXEL_RATE_KEY)' in src
      and 'CFNumberCreate(NULL, kCFNumberSInt64Type, &rate)' in src)
check('the key is listed in the decoder\'s supported properties, which is the only way VideoToolbox forwards a query',
      'UsingHardwareAcceleratedVideoDecoder, NV_MAX_DECODER_PIXEL_RATE_KEY }' in src)
check('the key is spelled as macOS spells it', 'CFSTR("MaxDecoderPixelRate")' in src)
check('the default is 4K at 120 frames per second', '(int64_t)3840 * 2160 * 120' in src)
check('NVMTL_NVDEC_MAX_PIXEL_RATE replaces it, and a value that is not a positive number is ignored',
      'NVMTL_NVDEC_MAX_PIXEL_RATE' in src and 'v > 0' in src and '!*end' in src)

check('the encoder answers MaxEncoderPixelRate (the Screen Sharing host reads it) and lists it', 
      'CFEqual(key, NV_MAX_ENCODER_PIXEL_RATE_KEY)' in enc and 'CFDictionarySetValue(d, NV_MAX_ENCODER_PIXEL_RATE_KEY, numRO)' in enc)
check('the encoder default is below what it measured, 1080p at 120 frames per second', '(int64_t)1920 * 1080 * 120' in enc)

if '--run' in sys.argv and platform.system() == 'Darwin' and shutil.which('clang'):
    with tempfile.TemporaryDirectory() as d:
        r = subprocess.run(['clang', '-o', d + '/p', str(root / 'tools/vt_max_pixel_rate_probe.c'), '-framework', 'VideoToolbox',
                            '-framework', 'CoreMedia', '-framework', 'CoreFoundation'], capture_output=True, text=True)
        check('the probe builds', r.returncode == 0, r.stderr.strip()[-200:])
        if r.returncode == 0:
            out = subprocess.run([d + '/p'], capture_output=True, text=True, timeout=60).stdout
            print(out.rstrip())
            check('the decoder macOS picks answers a pixel rate', 'type = kCFNumberSInt64Type' in out)
sys.exit(1 if fails else 0)
