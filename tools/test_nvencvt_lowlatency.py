#!/usr/bin/env python3
"""A client that asks for low-latency rate control (FFmpeg's low_delay, Sunshine) reaches the NVENC encoder.

VideoToolbox drops an encoder from the candidates of a client whose encoder specification names a selection key the
encoder does not declare, and answered -12902. The vendored encoder now declares EnableLowLatencyRateControl and handles
it: the session is real-time, and frame reordering stays off. This builds libnvencvt's encoder instance probe and runs
it against a built library. Needs a Mac with a compiler, the WebKit SPI headers, and the library:

usage: test_nvencvt_lowlatency.py <libnvencvt.dylib> <dir with CMBaseObjectSPI.h> [arch]   (arch default x86_64)
With no arguments it checks the source only."""
import pathlib, platform, shutil, subprocess, sys, tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'vendor/nvenc/vt/nvencvt.c').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


check('the H.264 and HEVC encoders declare EnableLowLatencyRateControl as a selection property',
      'sk[] = { kVTCompressionPropertyKey_ProfileLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl }' in src)
check('the key is accepted when set, so a session that names it is not refused',
      'CFEqual(key, kVTVideoEncoderSpecification_EnableLowLatencyRateControl)) {' in src)

if len(sys.argv) >= 3 and platform.system() == 'Darwin' and shutil.which('clang'):
    lib, hdr = sys.argv[1], sys.argv[2]
    arch = sys.argv[3] if len(sys.argv) > 3 else 'x86_64'
    with tempfile.TemporaryDirectory() as d:
        r = subprocess.run(['clang', '-arch', arch, '-I', hdr, '-o', d + '/probe', str(root / 'tools/nvencvt_lowlatency_probe.c'),
                            '-framework', 'VideoToolbox', '-framework', 'CoreMedia', '-framework', 'CoreFoundation'],
                           capture_output=True, text=True)
        check('the instance probe builds', r.returncode == 0, r.stderr.strip()[-300:])
        if r.returncode == 0:
            cmd = ['arch', f'-{arch}', d + '/probe', lib] if arch != platform.machine() else [d + '/probe', lib]
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
            print(out.rstrip())
            check('setting EnableLowLatencyRateControl on the encoder succeeds', 'set LLRC true -> 0' in out)
            check('it reads back true', 'copy LLRC -> 0 value true' in out)
            check('frame reordering reads false after it, even when the client asks for it', 'reads false' in out.split('AllowFrameReordering')[1].split('\n')[0])
            check('the session reports real-time', 'RealTime reads true' in out)
            check('a later RealTime=false does not undo it', 'set RealTime false -> 0, reads true' in out)
else:
    print('skip: instance checks need a library, the SPI headers and a Mac')

print(f'{fails} failed' if fails else 'all passed')
sys.exit(1 if fails else 0)
