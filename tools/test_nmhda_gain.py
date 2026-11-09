#!/usr/bin/env python3
"""NMHDA's software volume and mute (kexts/NMHDA/nmhda_gain.h), compiled and run on the host.
Arms: level 100 is exact unity and leaves a buffer byte-for-byte alone; level 0 and mute give silence; the curve is
monotonic and the cube of the level fraction (50 is -18 dB); 16-bit, packed 24-bit, 32-bit integer and 32-bit float
samples scale to the expected values; the most negative sample cannot overflow; only the requested frames change (the
engine's callback passes a window of the ring); an unknown format is refused and left untouched. The header is read at
a git revision (default: the working tree); a revision before it existed must FAIL (negative control).
  usage: test_nmhda_gain.py [rev]
"""
import pathlib, subprocess, sys, tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
rev = sys.argv[1] if len(sys.argv) > 1 else None
REL = "kexts/NMHDA/nmhda_gain.h"
if rev:
    r = subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{REL}"], capture_output=True, text=True)
    hdr = r.stdout if r.returncode == 0 else None
else:
    hdr = (ROOT / REL).read_text()
bad = 0


def check(name, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name)


TEST = r'''
#include <stdio.h>
#include <math.h>
#include "nmhda_gain.h"
int main(void)
{
    printf("g100=%u g0=%u gmute=%u g50=%u g10=%u g1=%u\n", nmhda_gain_q16(100, 0), nmhda_gain_q16(0, 0), nmhda_gain_q16(100, 1),
           nmhda_gain_q16(50, 0), nmhda_gain_q16(10, 0), nmhda_gain_q16(1, 0));
    int mono = 1; uint32_t prev = 0;
    for (int l = 0; l <= 100; l++) { uint32_t g = nmhda_gain_q16(l, 0); if (g < prev) mono = 0; prev = g; }
    printf("monotonic=%d db50=%.1f\n", mono, 20.0 * log10((double)nmhda_gain_q16(50, 0) / 65536.0));

    int16_t a[8] = { 1000, -1000, 32767, -32768, 7, -7, 0, 12345 }, orig[8];
    memcpy(orig, a, sizeof a);
    int rc = nmhda_gain_apply(a, 0, 4, 2, 16, 0, nmhda_gain_q16(100, 0));
    printf("unity_rc=%d unity_same=%d\n", rc, memcmp(a, orig, sizeof a) == 0);
    nmhda_gain_apply(a, 0, 3, 2, 16, 0, 32768);          /* half, three of the four frames */
    printf("s16=%d,%d,%d,%d\n", a[0], a[1], a[2], a[3]);
    printf("s16_untouched_tail=%d\n", a[6] == 0 && a[7] == 12345);

    int16_t w[6] = { 100, 100, 100, 100, 100, 100 };     /* a window: frames 1..2 of a 2-channel ring */
    nmhda_gain_apply(w, 1, 2, 2, 16, 0, 0);
    printf("window=%d,%d,%d,%d,%d,%d\n", w[0], w[1], w[2], w[3], w[4], w[5]);

    uint8_t t24[6] = { 0x00, 0x00, 0x40, 0x00, 0x00, 0xC0 };   /* +0x400000 and -0x400000 */
    nmhda_gain_apply(t24, 0, 2, 1, 24, 0, 32768);
    printf("s24=%02x%02x%02x,%02x%02x%02x\n", t24[2], t24[1], t24[0], t24[5], t24[4], t24[3]);

    int32_t i32[3] = { 1 << 30, -(1 << 30), INT32_MIN };
    nmhda_gain_apply(i32, 0, 3, 1, 32, 0, 16384);        /* quarter */
    printf("s32=%d,%d,%d\n", i32[0], i32[1], i32[2]);

    float f[3] = { 1.0f, -0.5f, 0.25f };
    nmhda_gain_apply(f, 0, 3, 1, 32, 1, 32768);
    printf("f32=%.4f,%.4f,%.4f\n", f[0], f[1], f[2]);

    int16_t m[4] = { 5, 6, 7, 8 };
    nmhda_gain_apply(m, 0, 4, 1, 16, 0, 0);
    printf("silent=%d%d%d%d\n", m[0], m[1], m[2], m[3]);

    int16_t u[2] = { 9, 9 };
    int r1 = nmhda_gain_apply(u, 0, 2, 1, 12, 0, 100), r2 = nmhda_gain_apply(u, 0, 2, 1, 16, 1, 100), r3 = nmhda_gain_apply(NULL, 0, 2, 1, 16, 0, 100);
    printf("refused=%d,%d,%d untouched=%d\n", r1, r2, r3, u[0] == 9 && u[1] == 9);
    return 0;
}
'''
if hdr is None:
    check("the gain header exists", False)
else:
    with tempfile.TemporaryDirectory() as td:
        d = pathlib.Path(td)
        (d / "nmhda_gain.h").write_text(hdr)
        (d / "t.c").write_text(TEST)
        r = subprocess.run(["cc", "-w", "-I", str(d), "-o", str(d / "t"), str(d / "t.c"), "-lm"], capture_output=True, text=True)
        if r.returncode:
            print("FAIL could not compile:\n" + r.stderr[:800]); sys.exit(1)
        o = subprocess.run([str(d / "t")], capture_output=True, text=True).stdout
        got = dict(kv.split("=", 1) for kv in o.split())
    check("level 100 is exact unity", got["g100"] == "65536")
    check("level 0 and mute are silence", got["g0"] == "0" and got["gmute"] == "0")
    check("the curve is the cube of the level fraction (50 -> 8192, 10 -> 66, 1 -> 0)",
          got["g50"] == "8192" and got["g10"] == "66" and got["g1"] == "0")
    check("the curve never decreases and 50 is -18 dB", got["monotonic"] == "1" and got["db50"] == "-18.1")
    check("unity gain leaves the buffer byte for byte alone", got["unity_rc"] == "0" and got["unity_same"] == "1")
    check("16-bit samples halve with rounding, and the most negative cannot overflow", got["s16"] == "500,-500,16384,-16384")
    check("frames after the requested window are not touched", got["s16_untouched_tail"] == "1")
    check("only the requested frames of a ring window change", got["window"] == "100,100,0,0,0,0")
    check("packed 24-bit samples scale", got["s24"] == "200000,e00000")
    check("32-bit integer samples scale, including the most negative", got["s32"] == "268435456,-268435456,-536870912")
    check("32-bit float samples scale", got["f32"] == "0.5000,-0.2500,0.1250")
    check("gain 0 silences the window", got["silent"] == "0000")
    check("an unknown format, a float in a 16-bit container and a null buffer are refused and left alone",
          got["refused"] == "-1,-1,-1" and got["untouched"] == "1")
def src(path):
    if rev:
        r = subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{path}"], capture_output=True, text=True)
        return r.stdout if r.returncode == 0 else ""
    p = ROOT / path
    return p.read_text() if p.exists() else ""


main_cpp, vol_cpp = src("kexts/NMHDA/nmhda.cpp"), src("kexts/NMHDA/nmhda_volume.cpp")
import re
check("the volume hooks are installed only when nmhdavol=1 is on the boot line",
      "if (nmhdaVolumeRequested()) {" in main_cpp and main_cpp.count("onKextLoadForce(&kAppleHda") == 1
      and main_cpp.index("if (nmhdaVolumeRequested()) {") < main_cpp.index("onKextLoadForce(&kAppleHda"))
check("the controls are added only after the sample hook is in place",
      "gClipRouted && gVolumeApi.createVolumeControl && gVolumeApi.createMuteControl" in main_cpp)
check("only a published display sink engine is given controls (matched by class name, not by an object offset)",
      'kEngineClass[] = "AppleHDAEngineOutputDP"' in vol_cpp and "getClassName(), kEngineClass" in vol_cpp)
check("no private object layout is read or written (no byte offsets into AppleHDA objects)",
      vol_cpp != "" and re.search(r"\+ 0x[0-9a-f]+\)|\[0x[0-9a-f]+", vol_cpp + main_cpp[main_cpp.find("wrapClipOutputSamples"):main_cpp.find("static void pluginStart")]) is None)
print("PASS" if not bad else f"{bad} FAILED")
sys.exit(1 if bad else 0)
