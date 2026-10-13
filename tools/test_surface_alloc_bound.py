#!/usr/bin/env python3
"""A texture backed by an IOSurface is only copied to or from the surface when the copy ends inside the surface's
allocation. Compiles the production -[NVMTLTexture nvmtlSurfaceCopyable] body against stand-in IOSurface functions and
checks a normal surface, a two-plane video surface, and surfaces whose rows describe more memory than they own (the
shape of two Tahoe crashes in the row copy at a page boundary). Run against the 1.15 source, the oversized arms fail."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLObjects.m').read_text()
start = src.index('- (BOOL)nvmtlSurfaceCopyable {')
body = src[start + len('- (BOOL)nvmtlSurfaceCopyable {'):]
depth, i = 1, 0
while depth:
    depth += {'{': 1, '}': -1}.get(body[i], 0)
    i += 1
body = body[:i - 1]

HARNESS = r'''
#include <stdio.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#define BOOL int
#define YES 1
#define NO 0
#define MIN(a, b) ((a) < (b) ? (a) : (b))
typedef struct { size_t alloc, bpr[2], h[2], w[2], bpe[2], off[2]; char *base; } surf_t;
static surf_t *S;
static size_t IOSurfaceGetAllocSize(surf_t *s) { return s->alloc; }
static void *IOSurfaceGetBaseAddress(surf_t *s) { return s->base; }
static void *IOSurfaceGetBaseAddressOfPlane(surf_t *s, uint32_t p) { return s->base + s->off[p]; }
static size_t IOSurfaceGetBytesPerRowOfPlane(surf_t *s, uint32_t p) { return s->bpr[p]; }
static size_t IOSurfaceGetHeightOfPlane(surf_t *s, uint32_t p) { return s->h[p]; }
static size_t IOSurfaceGetWidthOfPlane(surf_t *s, uint32_t p) { return s->w[p]; }
static size_t IOSurfaceGetBytesPerElementOfPlane(surf_t *s, uint32_t p) { return s->bpe[p]; }
#define nvlog(...) ((void)0)
struct { uint32_t w, h, bpp; } _i;
static surf_t *_surf; static unsigned long _plane;
static int copyable(void) {''' + body + r'''}
static char mem[64 << 20];
static int run(size_t alloc, size_t bpr, size_t h, size_t w, size_t bpe, size_t off, unsigned plane, uint32_t iw, uint32_t ih, uint32_t ibpp) {
    surf_t s; memset(&s, 0, sizeof s); s.alloc = alloc; s.base = mem;
    s.bpr[plane] = bpr; s.h[plane] = h; s.w[plane] = w; s.bpe[plane] = bpe; s.off[plane] = off;
    _surf = &s; _plane = plane; _i.w = iw; _i.h = ih; _i.bpp = ibpp;
    return copyable();
}
int main(void) {
    printf("%d\n", run(1920 * 4 * 1080, 1920 * 4, 1080, 1920, 4, 0, 0, 1920, 1080, 4));              /* BGRA 1080p, exact */
    printf("%d\n", run(1920 * 1080 * 3 / 2, 1920, 540, 960, 2, 1920 * 1080, 1, 960, 540, 2));      /* 420 CbCr plane, exact */
    printf("%d\n", run(1920 * 4 * 1080 - 4096, 1920 * 4, 1080, 1920, 4, 0, 0, 1920, 1080, 4));     /* rows run 4 KB past the end */
    printf("%d\n", run(1920 * 1080 + 1920 * 270, 1920, 540, 960, 2, 1920 * 1080, 1, 960, 540, 2)); /* chroma plane half missing */
    return 0;
}
'''

with tempfile.TemporaryDirectory() as d:
    c, exe = pathlib.Path(d) / 'bound.c', pathlib.Path(d) / 'bound'
    c.write_text(HARNESS.replace('_surf, (uint32_t)_plane', '_surf, (uint32_t)_plane'))
    r = subprocess.run(['cc', '-w', '-o', str(exe), str(c)], capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-1500:])
        sys.exit('could not compile nvmtlSurfaceCopyable - check is UNKNOWN, not clean')
    got = [int(x) for x in subprocess.check_output([str(exe)], text=True).split()]
names = ['a 1080p BGRA surface is copied', 'a 4:2:0 chroma plane that fits is copied',
         'rows that run 4 KB past the allocation are refused', 'a chroma plane with half its rows missing is refused']
want = [1, 1, 0, 0]
fails = 0
for n, g, w in zip(names, got, want):
    ok = g == w
    print(('ok   ' if ok else 'FAIL ') + n + f' (copyable {g})')
    fails += not ok
sys.exit(1 if fails else 0)
