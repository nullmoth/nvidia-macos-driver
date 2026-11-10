#!/usr/bin/env python3
"""libnvencvt on Blackwell and on the Turing chips other than TU117: what the library needs to know about them.

The vendored library knew Ada, consumer Ampere and TU117 only. On an RTX 50 card it found no channel class, no NVDEC
class and no card kind, so it registered nothing there, and it would have read a decoded plane in the wrong byte order
had it got that far. This checks, without a GPU:

  * the byte order of a decoded plane on Blackwell, against a model written from Mesa NIL's tables (copy.rs), with the
    older layout as the control: it must NOT give the picture back;
  * which layout a plane gets from the NVDEC class and its element size;
  * the class table (control: the revision before this change has no Blackwell class);
  * the card kind from the PCI device ID;
  * that a channel which stopped fetching cannot make the library write past its push buffer (control: the function
    as it was before does).

usage: test_nvencvt_blackwell.py [repo root]"""
import pathlib, platform, random, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
V = root / 'vendor/nvenc'
BEFORE = '4f4f70d6'      # the revision this change started from
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


def function(src, signature):
    i = src.find(signature)
    return None if i < 0 else src[i:src.index('\n}\n', i) + 3]


def old(path):
    r = subprocess.run(['git', '-C', str(root), 'show', f'{BEFORE}:{path}'], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


# ---- the model: where each run of bytes of a 64 x 8 GOB lies, as Mesa NIL's for_each_gob_line lists them ----
def gob_lines(kind):
    """-> [(offset in the GOB, x, y, run length)]"""
    out = []
    if kind == 'blackwell8':
        for x in range(8):
            for y in range(8):
                out.append((x * 0x40 + y * 8, x * 8, y, 8))
        return out
    groups = {'turing': [(0x00, 0, 0), (0x40, 16, 0), (0x80, 0, 4), (0xc0, 16, 4)],
              'blackwell16': [(0x00, 0, 0), (0x80, 16, 0), (0x40, 0, 4), (0xc0, 16, 4)]}[kind]
    for i in range(2):
        for base, x, y0 in groups:
            for r in range(4):
                out.append((i * 0x100 + base + r * 0x10, i * 32 + x, y0 + r, 16))
    return out


def tile(linear, width, rows, pitch, kind):
    """A plane as the engine writes it: rows of 64 x 16 tiles, each two GOBs one above the other."""
    size = pitch * ((rows + 15) // 16) * 16
    plane = bytearray(b'\xee' * size)
    lines = gob_lines(kind)
    for ty in range((rows + 15) // 16):
        for tx in range(pitch // 64):
            for g in range(2):
                base = ty * pitch * 16 + tx * 1024 + g * 512
                for off, x, y, n in lines:
                    X, Y = tx * 64 + x, ty * 16 + g * 8 + y
                    if Y >= rows or X >= width:
                        continue
                    m = min(n, width - X)
                    plane[base + off:base + off + m] = linear[Y * width + X:Y * width + X + m]
    return bytes(plane)


HARNESS = r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "blocklinear.h"
int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "layouts")) {
        printf("%d %d %d %d %d\n", gob_layout_for(0xcfb0, 1), gob_layout_for(0xcfb0, 2), gob_layout_for(0xcfb0, 4),
               gob_layout_for(0xc9b0, 1), gob_layout_for(0xc4b0, 2));
        return 0;
    }
    if (argc == 2 && !strcmp(argv[1], "bijection")) {
        for (int l = 0; l < 3; l++) {
            static unsigned char seen[256 * 48]; memset(seen, 0, sizeof seen); int bad = 0;
            for (size_t y = 0; y < 48; y++) for (size_t x = 0; x < 256; x++) {
                size_t o = gob_offset(l, x, y, 256);
                if (o >= sizeof seen || seen[o]++) bad++;
            }
            printf("%d ", bad);
        }
        printf("\n"); return 0;
    }
    // untile <layout> <width> <rows> <pitch> <in> <out>
    int layout = atoi(argv[2]); size_t w = (size_t)atol(argv[3]), rows = (size_t)atol(argv[4]), pitch = (size_t)atol(argv[5]);
    size_t size = pitch * ((rows + 15) / 16) * 16;
    unsigned char *src; if (posix_memalign((void **)&src, 64, size)) return 2;
    unsigned char *dst = malloc(w * rows);
    FILE *f = fopen(argv[6], "rb"); if (!f || fread(src, 1, size, f) != size) return 2; fclose(f);
    gob_untile_plane(layout, dst, w, src, w, rows, pitch);
    f = fopen(argv[7], "wb"); fwrite(dst, 1, w * rows, f); fclose(f);
    return 0;
}
'''

bl = V / 'src/blocklinear.c'
have_cc = shutil.which('clang') and platform.system() == 'Darwin'
check('the library has a layout per generation', 'gob_untile_plane' in bl.read_text() and 'gob_layout_for' in bl.read_text())
if have_cc and 'gob_untile_plane' in bl.read_text():
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        (d / 'h.c').write_text(HARNESS)
        r = subprocess.run(['clang', '-arch', 'x86_64', '-O1', '-I', str(V / 'src'), '-o', str(d / 'h'), str(d / 'h.c'), str(bl),
                            '-framework', 'Accelerate'], capture_output=True, text=True)
        check('the layout code builds on its own', r.returncode == 0, r.stderr.strip()[-400:])
        if r.returncode == 0:
            run = lambda *a, env=None: subprocess.run([str(d / 'h'), *map(str, a)], capture_output=True, text=True, env=env)
            import os
            clean = {k: v for k, v in os.environ.items() if k != 'NVDEC_GOB'}
            got = run('layouts', env=clean).stdout.split()
            check('Blackwell: 1-byte elements, 2-byte elements and wider ones each get their layout; older cards keep Turing\'s',
                  got == ['1', '2', '0', '0', '0'], ' '.join(got))
            got = run('layouts', env=dict(clean, NVDEC_GOB='turing')).stdout.split()
            check('NVDEC_GOB=turing overrides the class', got == ['0', '0', '0', '0', '0'], ' '.join(got))
            got = run('layouts', env=dict(clean, NVDEC_GOB='blackwell')).stdout.split()
            check('NVDEC_GOB=blackwell overrides the class', got == ['1', '2', '0', '1', '2'], ' '.join(got))
            got = run('bijection').stdout.split()
            check('every layout maps each byte of a plane to its own place', got == ['0', '0', '0'], ' '.join(got))
            rnd = random.Random(29)
            # sizes: a width that is no multiple of 64 or of the run, rows that end inside a tile; and a plane tall
            # enough for the threaded path
            for width, rows, pitch in ((200, 70, 256), (1920, 1080, 1920), (100, 9, 128)):
                linear = bytes(rnd.getrandbits(8) for _ in range(width * rows))
                for layout, kind in ((1, 'blackwell8'), (2, 'blackwell16'), (0, 'turing')):
                    (d / 'in').write_bytes(tile(linear, width, rows, pitch, kind))
                    run('untile', layout, width, rows, pitch, d / 'in', d / 'out')
                    out = (d / 'out').read_bytes() if (d / 'out').exists() else b''
                    check(f'{kind} plane {width}x{rows} comes back as the picture', out == linear)
                    if kind != 'turing':
                        run('untile', 0, width, rows, pitch, d / 'in', d / 'out')
                        check(f'control: the Turing layout does not read a {kind} plane {width}x{rows}', (d / 'out').read_bytes() != linear)
else:
    print('SKIP the layout arms need clang on a Mac')

# ---- classes ----
rm = (V / 'src/rm.c').read_text()
want = function(rm, 'NvU32 rm_class(')
check('the class table names Blackwell\'s encoder, decoder and channel classes',
      want is not None and all(c in want for c in ('0xcfb7', '0xcfb0', '0xc96f')))
check('the older classes are all still there',
      want is not None and all(c in want for c in ('0xc9b7', '0xc7b7', '0xc4b7', '0xb4b7', '0xc9b0', '0xc7b0', '0xc4b0', '0xc56f', '0xc46f', '0xc361')))
o = old('vendor/nvenc/src/rm.c')
if o is None:
    print('skip the control for the class table: this is not a full clone')
else:
    check('control: the revision before had no Blackwell class', '0xc96f' not in o and '0xcfb0' not in o)

# ---- card kinds ----
vt = (V / 'vt/nvencvt.c').read_text()
kind = function(vt, 'static int gpu_kind_for_device(')
check('the card kind is a function of the PCI device ID', kind is not None)
if kind and have_cc:
    enum = vt[vt.index('enum { GPU_NONE'):vt.index('\n', vt.index('enum { GPU_NONE'))]
    src = ('#include <stdio.h>\n' + enum + '\n' + kind +
           'int main(void) { unsigned ids[] = { 0x2d05, 0x2b85, 0x2f04, 0x2684, 0x2484, 0x1f82, 0x1f08, 0x1e84, 0x2184, 0x1c03, 0x3000 };\n'
           '  for (int i = 0; i < 11; i++) printf("%d ", gpu_kind_for_device(ids[i])); return 0; }\n')
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d); (d / 'k.c').write_text(src)
        r = subprocess.run(['clang', '-o', str(d / 'k'), str(d / 'k.c')], capture_output=True, text=True)
        got = subprocess.run([str(d / 'k')], capture_output=True, text=True).stdout.split() if r.returncode == 0 else [r.stderr[-200:]]
        # RTX 5060, 5090, 5070 -> Blackwell 4; RTX 4090 -> Ada 1; RTX 3070 -> Ampere 3; GTX 1650 -> TU117 2;
        # RTX 2060, 2080, GTX 1660 -> Turing 5; GTX 1060 (Pascal) and an ID past Blackwell -> none
        check('RTX 50 is Blackwell, RTX 20 and GTX 1660 are Turing, the kinds known before are unchanged, Pascal is none',
              got == ['4', '4', '4', '1', '3', '2', '5', '5', '5', '0', '0'], ' '.join(got))
dec = (V / 'vt/nvencvt_dec.c').read_text()
reg = function(dec, 'OSStatus NVDecVT_Register(')
check('Blackwell registers the H.264 decoder alone',
      reg is not None and 'h264Only = gNvencvtGpu == 4' in reg and reg.count('!h264Only') == 3)
regenc = function(vt, '__attribute__((visibility("default"))) OSStatus NVEncVT_Register(')
check('no encoder registers on Blackwell or on the Turing chips without an encoder setup',
      regenc is not None and 'encoders = gpu == GPU_ADA || gpu == GPU_TU117 ||' in regenc)
h264 = (V / 'src/dec.c').read_text()
check('the H.264 decoder reads its planes in the card\'s layout, cropped pictures included',
      'nvenc_in_untile_plane' not in h264 and 'nvenc_in_offset' not in h264 and h264.count('gob_offset(d->gob') == 4)

# ---- the push buffer ----
STUB = r'''
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
typedef unsigned NvU32;
typedef struct { NvU32 *cur, *end; bool overflow; } Chan;
#define DRF_NUM(a, b, c, v) ((NvU32)(v))
#define NVC56F_DMA_SEC_OP_INC_METHOD 1
%s
int main(void) {
    static NvU32 mem[64 + 64];
    for (int i = 0; i < 64; i++) mem[64 + i] = 0xfeedfaceu;
    Chan c = { mem, mem + 64, false };
    for (int i = 0; i < 100; i++) chan_mthd(&c, 0, 0x400, 3, 1u, 2u, 3u);
    int past = 0; for (int i = 0; i < 64; i++) past += mem[64 + i] != 0xfeedfaceu;
    printf("%%d %%d\n", past, (int)c.overflow); return 0; }
'''
ch = (V / 'src/channel.c').read_text()
if have_cc:
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)

        def past(source):
            fn = function(source, 'void chan_mthd(')
            (d / 'p.c').write_text(STUB % fn)
            r = subprocess.run(['clang', '-o', str(d / 'p'), str(d / 'p.c')], capture_output=True, text=True)
            if r.returncode:
                return None, r.stderr[-300:]
            return subprocess.run([str(d / 'p')], capture_output=True, text=True).stdout.split(), ''
        got, err = past(ch)
        check('methods that do not fit the push buffer are dropped and the channel is marked', got == ['0', '1'], err or ' '.join(got or []))
        oc = old('vendor/nvenc/src/channel.c')
        if oc is None:
            print('skip the control for the push buffer: this is not a full clone')
        else:
            got, err = past(oc)
            check('control: the function as it was wrote past the buffer', got is not None and int(got[0]) > 0, err or ' '.join(got or []))
check('a submission after an overflow fails instead of sending a cut picture',
      'if (c->overflow)' in function(ch, 'NV_STATUS chan_submit('))
check('the push buffer is reused after a completion the decoder saw, not only by the GPGet word',
      '!c->settled && chan_gpget(c) != c->gpPut' in ch and 'chan_done(c);' in h264)

print(f'{fails} failed' if fails else 'all passed')
sys.exit(1 if fails else 0)
