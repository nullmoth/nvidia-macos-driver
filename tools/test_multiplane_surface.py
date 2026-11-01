#!/usr/bin/env python3
"""Offline checks for the production multi-plane opt-in, bounds and whole-surface copy.

No Metal device, driver installation, or GPU submission is used.
"""
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]


def function(path, name):
    source = (ROOT / path).read_text()
    start = source.index(name + "(")
    start = source.rfind("\n", 0, start) + 1
    brace = source.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


PREFIX = r'''
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <assert.h>
#define nvlog(...) ((void)0)
typedef int BOOL;
typedef unsigned long NSUInteger;
enum { NO=0, YES=1, MTLPixelFormatRGBA8Unorm=70, MTLPixelFormatRGBA8Unorm_sRGB=71,
       MTLPixelFormatBGRA8Unorm=80, MTLPixelFormatBGRA8Unorm_sRGB=81, MTLTextureType2D=2 };
typedef struct { size_t x,y,z; } MTLOrigin;
typedef struct { size_t width,height,depth; } MTLSize;
typedef struct { MTLOrigin origin; MTLSize size; } MTLRegion;
typedef struct { unsigned w,h,bpp,mtl_type,mips,layers,samples,bw,bh; } nvk_image;
typedef struct { void *_surf,*_parent,*_viewParent,*_tbView; unsigned _baseLevel,_baseSlice,_fmt;
                 BOOL _vramMulti; nvk_image _i; } NVMTLTexture;
static int mode;
static int sysctlbyname(const char *n, void *p, size_t *len, void *v, size_t vl) {
    (void)v; (void)vl;
    assert(!strcmp(n, "debug.nvaccel_pageoff_unlocked_wait"));
    if (mode == 0) return -1;
    *(int *)p = mode == 1 ? 0 : 1;
    *len = mode == 3 ? 1 : sizeof(int);
    return 0;
}
typedef struct { int unused; } nvk_queue;
typedef struct { void *buf, *map; size_t size; } nvk_buffer;
static int g_host_import = 1;
static size_t g_host_align = 64;
static int import_fail, stage_fail, fail_first_copy, copies, imports, drops;
static void *staging;
static int nvmtl_vk_buffer_import_host(void *base, size_t n, nvk_buffer *b) {
    imports++; if (import_fail) return -1;
    *b = (nvk_buffer){base, base, n}; return 0;
}
static void nvmtl_vk_buffer_destroy(nvk_buffer *b) { drops++; b->buf = NULL; }
static int nvmtl_vk_stage(nvk_queue *q, size_t n, nvk_buffer *b) {
    (void)q; if (stage_fail) return -1;
    staging = malloc(n); assert(staging); *b = (nvk_buffer){staging, staging, n}; return 0;
}
static int nvmtl_vk_copy_buffer(nvk_queue *q, nvk_buffer *s, size_t so,
                                nvk_buffer *d, size_t off, size_t n) {
    (void)q; copies++;
    if (fail_first_copy && copies == 1) return -1;
    assert(so <= s->size && n <= s->size - so);
    assert(off <= d->size && n <= d->size - off);
    memcpy((char *)d->map + off, (char *)s->map + so, n); return 0;
}
'''

MAIN = r'''
static void overwrite(void) {
    NVMTLTexture t={._surf=&t,._fmt=MTLPixelFormatRGBA8Unorm,
        ._i={.w=32,.h=16,.bpp=4,.mtl_type=MTLTextureType2D,.mips=1,.layers=1,.samples=1,.bw=1,.bh=1}};
    MTLRegion r={{0,0,0},{32,16,1}};
    const unsigned formats[]={70,71,80,81};
    for (size_t f=0;f<4;f++) {
        t._fmt=formats[f]; t._vramMulti=NO;
        assert(nvmtl_surface_full_overwrite(&t,r,0,0));
        t._vramMulti=YES;
        assert(!nvmtl_surface_full_overwrite(&t,r,0,0));
    }
    t._vramMulti=NO; r.size.width=31;
    assert(!nvmtl_surface_full_overwrite(&t,r,0,0));
}
static void bounds(void) {
    size_t off = 999;
    assert(nvmtl_mp_plane_range(0x1000, 0x1040, 4160, 32, 32, 4, 128, 32, &off) && off == 64);
    assert(nvmtl_mp_plane_range(0x1000, 0x2040, 6208, 32, 16, 2, 128, 16, &off) && off == 4160);
    assert(!nvmtl_mp_plane_range(0, 64, 100, 1, 1, 1, 32, 1, &off));
    assert(!nvmtl_mp_plane_range(4096, 4064, 4160, 1, 1, 1, 32, 1, &off));
    assert(!nvmtl_mp_plane_range(4096, 4160, 4160, 32, 0, 4, 128, 32, &off));
    assert(!nvmtl_mp_plane_range(4096, 4160, 4159, 32, 32, 4, 128, 32, &off));
    assert(!nvmtl_mp_plane_range(4096, 4160, 4160, 32, 32, 4, 96, 32, &off));
    assert(!nvmtl_mp_plane_range(4096, 4161, 4160, 1, 1, 1, 32, 1, &off));
    assert(!nvmtl_mp_plane_range(4096, 4160, SIZE_MAX, UINT32_MAX, 1, SIZE_MAX, 128, 1, &off));
    assert(!nvmtl_mp_plane_range(4096, 4160, 4160, 32, UINT32_MAX, 4, 128, UINT32_MAX, &off));
}
static void copy(int kind) {
    unsigned char *src = NULL, dst[256]; assert(!posix_memalign((void **)&src, 64, 256));
    for (int i = 0; i < 256; i++) src[i] = (unsigned char)(i * 17 + 3);
    memset(dst, 0, sizeof dst);
    nvk_buffer b = {dst, dst, 111};
    copies = imports = drops = 0; staging = NULL;
    import_fail = kind == 1 || kind == 3;
    stage_fail = kind == 3;
    fail_first_copy = kind == 2;
    int rc = nvmtl_vk_buffer_fill_from_surface(NULL, &b, src, 111);
    if (kind == 3) { assert(rc); assert(!copies); }
    else { assert(!rc); assert(!memcmp(src, dst, 111)); assert(dst[111] == 0); }
    assert(imports == 1); assert(drops == (import_fail ? 0 : 1));
    assert(nvmtl_vk_buffer_fill_from_surface(NULL, &b, src, 112));
    assert(nvmtl_vk_buffer_fill_from_surface(NULL, &b, NULL, 111));
    assert(nvmtl_vk_buffer_fill_from_surface(NULL, &b, src, 0));
    free(staging); free(src);
}
int main(int argc, char **argv) {
    assert(argc == 3); mode = atoi(argv[1]); int setting = atoi(argv[2]);
    if (setting < 0) unsetenv("NVMTL_SURF_MULTIPLANE");
    else setenv("NVMTL_SURF_MULTIPLANE", setting == 1 ? "1" : setting == 2 ? "10" : "0", 1);
    assert(nvmtl_mp_on() == (mode == 2 && setting == 1));
    bounds(); overwrite(); for (int i = 0; i < 4; i++) copy(i);
    return 0;
}
'''


def main():
    gate = function("plugin/NVMTLDevice.m", "nvmtl_mp_on")
    bounds = function("plugin/NVMTLDevice.m", "nvmtl_mp_plane_range")
    copy = function("plugin/nvmtl_vk.c", "nvmtl_vk_buffer_fill_from_surface")
    overwrite = function("plugin/NVMTLObjects.m", "nvmtl_surface_full_overwrite")
    with tempfile.TemporaryDirectory(prefix="nvmtl-multiplane-") as tmp:
        tmp = pathlib.Path(tmp)
        source, exe = tmp / "test.c", tmp / "test"
        def build(body):
            source.write_text(PREFIX + gate + "\n" + bounds + "\n" + overwrite + "\n" + body + MAIN)
            subprocess.run(["clang", "-std=gnu11", "-O1", "-g", "-fsanitize=address,undefined",
                            "-o", str(exe), str(source)], check=True)
        build(copy)
        for mode in range(4):
            for setting in [-1, 0, 1, 2]:
                subprocess.run([str(exe), str(mode), str(setting)], check=True)
        # A partial-plane-style copy must fail the whole-allocation data check.
        broken = copy.replace("dst, 0, bytes);", "dst, 0, bytes - 1);")
        assert broken != copy
        build(broken)
        result = subprocess.run([str(exe), "2", "1"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert result.returncode != 0, "negative control accepted an incomplete copy"
    print("PASS: 16 opt-in/kernel gate cases, range bounds, complete copies and fallbacks; negative control rejected")


if __name__ == "__main__":
    main()
