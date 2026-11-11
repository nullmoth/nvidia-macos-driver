/*
 * context_test.c <engine bundle (test build with the context layer)>
 *
 * End-to-end host test with no GPU: the engine's own entry points are called in the order the window server calls
 * them, with its parameter values; the objects that come back are checked at the raw offsets the 10.11.6 window
 * server reads; then a frame is drawn through the 974-slot function table on Mesa's CPU rasteriser and the pixels
 * are counted. Every arm is a named sentence and prints the numbers it compared.
 */
#include <OpenGL/gliDispatch.h>
#include <IOSurface/IOSurface.h>
#include <CoreFoundation/CoreFoundation.h>

#include "../../engine/nm_gli.h"

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failed, arms;
static void arm(int ok, const char *name, const char *detail)
{
    arms++; failed += !ok;
    printf("%s  %s  [%s]\n", ok ? "PASS" : "FAIL", name, detail);
}
static uint32_t rd32(const void *p, size_t off) { uint32_t v; memcpy(&v, (const char *)p + off, 4); return v; }
static uint16_t rd16(const void *p, size_t off) { uint16_t v; memcpy(&v, (const char *)p + off, 2); return v; }

/* The window server's context: engine context, then the table, then its private part. */
struct ws_context { GLIContext rend; GLIFunctionDispatch disp; char priv[0x78]; };

/* Counts pixels of an exact BGRA value inside a rectangle of a top-row-first image. */
static int count(const uint8_t *img, int row_bytes, int x0, int y0, int x1, int y1, uint8_t b, uint8_t g, uint8_t r)
{
    int n = 0;
    for (int y = y0; y < y1; y++)
        for (int x = x0; x < x1; x++) {
            const uint8_t *p = img + (size_t)y * (size_t)row_bytes + (size_t)x * 4;
            n += p[0] == b && p[1] == g && p[2] == r;
        }
    return n;
}

static IOSurfaceRef make_iosurface(int w, int h, uint8_t b, uint8_t g, uint8_t r)
{
    int bpe = 4, bpr = w * 4; uint32_t fmt = 'BGRA';
    CFNumberRef vals[5]; const void *keys[5] = { kIOSurfaceWidth, kIOSurfaceHeight, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfacePixelFormat };
    vals[0] = CFNumberCreate(NULL, kCFNumberIntType, &w); vals[1] = CFNumberCreate(NULL, kCFNumberIntType, &h);
    vals[2] = CFNumberCreate(NULL, kCFNumberIntType, &bpe); vals[3] = CFNumberCreate(NULL, kCFNumberIntType, &bpr);
    vals[4] = CFNumberCreate(NULL, kCFNumberSInt32Type, &fmt);
    CFDictionaryRef d = CFDictionaryCreate(NULL, keys, (const void **)vals, 5, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    IOSurfaceRef s = IOSurfaceCreate(d);
    CFRelease(d); for (int i = 0; i < 5; i++) CFRelease(vals[i]);
    if (!s) return NULL;
    IOSurfaceLock(s, 0, NULL);
    uint8_t *p = IOSurfaceGetBaseAddress(s); size_t rb = IOSurfaceGetBytesPerRow(s);
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) { uint8_t *q = p + (size_t)y * rb + (size_t)x * 4; q[0] = b; q[1] = g; q[2] = r; q[3] = 255; }
    IOSurfaceUnlock(s, 0, NULL);
    return s;
}

/* A quad over one quadrant of clip space, with rectangle-texture coordinates when tw is not zero. */
static void quad(struct ws_context *c, float x0, float y0, float x1, float y1, float tw, float th)
{
    GLIFunctionDispatch *d = &c->disp;
    d->begin(c->rend, 0x0007 /* GL_QUADS */);
    if (tw != 0) d->tex_coord2f(c->rend, 0, 0);
    d->vertex2f(c->rend, x0, y0);
    if (tw != 0) d->tex_coord2f(c->rend, tw, 0);
    d->vertex2f(c->rend, x1, y0);
    if (tw != 0) d->tex_coord2f(c->rend, tw, th);
    d->vertex2f(c->rend, x1, y1);
    if (tw != 0) d->tex_coord2f(c->rend, 0, th);
    d->vertex2f(c->rend, x0, y1);
    d->end(c->rend);
}

int main(int argc, char **argv)
{
    if (argc < 3) return 2;
    unsigned want_slots = (unsigned)atoi(argv[2]);   /* table slots of the release the bundle was built for */
    void *e = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!e) { fprintf(stderr, "cannot open: %s\n", dlerror()); return 2; }
#define SYM(ret, name, args) ret (*name) args = (ret (*) args)dlsym(e, #name); if (!name) { fprintf(stderr, "missing %s\n", #name); return 2; }
    SYM(void, nm_test_own_software, (unsigned))
    SYM(int, nm_test_register_surface, (uint32_t, int, int, int, void *))
    SYM(intptr_t, gliInitializeLibrary, (const uint32_t *, const uint8_t *, unsigned, void *, void *, void *, intptr_t))
    SYM(int, gliGetVersion, (int *, int *, int *))
    SYM(intptr_t, gliGetInteger, (void *, int, void *))
    SYM(intptr_t, gliSetInteger, (void *, int, const void *))
    SYM(intptr_t, gliChoosePixelFormat, (void **, const void *))
    SYM(intptr_t, gliQueryRendererInfo, (void **, unsigned))
    SYM(intptr_t, gliDestroyRendererInfo, (void *))
    SYM(intptr_t, gliDestroyPixelFormat, (void *))
    SYM(intptr_t, gliCreateContext, (void **, const void *, void *, void *, void *, intptr_t))
    SYM(intptr_t, gliDestroyContext, (void *))
    SYM(intptr_t, gliAttachDrawableWithOptions, (void *, int, const void *, int, int))
    char d[320];

    /* ---- start-up, in the window server's order -------------------------------------------------------- */
    nm_test_own_software(1);
    uint32_t services[1] = { 0 }; uint8_t fbidx[1] = { 0 };
    gliInitializeLibrary(services, fbidx, 1, NULL, NULL, NULL, 0);
    int ma = 0, mi = 0, id = 0, vr = gliGetVersion(&ma, &mi, &id);
    snprintf(d, sizeof d, "returned %d, version %d.%d", vr, ma, mi);
    arm(vr == 1 && ma == 2 && mi == 3, "gliGetVersion answers true with 2.3, which both loaders require", d);

    uint32_t tsize = 0; intptr_t r = gliGetInteger(NULL, 224, &tsize);
    snprintf(d, sizeof d, "returned %ld, size %u, structure %zu, slots %zu", (long)r, tsize, sizeof(GLIFunctionDispatch), sizeof(GLIFunctionDispatch) / sizeof(void *));
    arm(r == 0 && tsize == sizeof(GLIFunctionDispatch) && tsize == want_slots * 8, "the table-size query with a null context returns the release's table size", d);

    static const uint32_t attribs[16] = { 0 };
    void *pf = NULL; r = gliChoosePixelFormat(&pf, attribs);
    /* the 10.11.6 window server's own acceptance test of a pixel-format node, at its raw offsets */
    int accepted = pf && (rd32(pf, 0x34) & 1u) && (rd16(pf, 0xc) & 0x101) && (rd32(pf, 0x14) == 0x4000 || rd32(pf, 0x14) == 0x8000) &&
                   rd32(pf, 0x1c) == 1 && rd32(pf, 0x20) == 1 && rd16(pf, 0x26) == 0;
    snprintf(d, sizeof d, "returned %ld; mask %#x flags %#x colour %#x depth %u stencil %u aux %u id %#x", (long)r, pf ? rd32(pf, 0x34) : 0, pf ? rd32(pf, 0xc) : 0,
             pf ? rd32(pf, 0x14) : 0, pf ? rd32(pf, 0x1c) : 0, pf ? rd32(pf, 0x20) : 0, pf ? rd16(pf, 0x26) : 0, pf ? rd32(pf, 0x8) : 0);
    arm(r == 0 && accepted, "the pixel-format node passes the window server's own filter at the offsets it reads", d);

    /* byte for byte: the 0x38 bytes the window server copies, against values written out independently */
    uint8_t want[0x38]; memset(want, 0, sizeof want);
    uint32_t w32;
    w32 = 0x00027e00; memcpy(want + 0x8, &w32, 4);
    w32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x100 | 0x400 | 0x4000 | 0x8000; memcpy(want + 0xc, &w32, 4);
    w32 = 0x1 | 0x4; memcpy(want + 0x10, &w32, 4);
    w32 = 0x8000; memcpy(want + 0x14, &w32, 4);
    w32 = 1; memcpy(want + 0x1c, &w32, 4); memcpy(want + 0x20, &w32, 4);
    w32 = 1; memcpy(want + 0x34, &w32, 4);
    int same = pf && memcmp((char *)pf + 8, want + 8, 0x30) == 0 && *(void **)pf == NULL;
    int firstdiff = -1; for (int i = 8; pf && i < 0x38 && firstdiff < 0; i++) if (((uint8_t *)pf)[i] != want[i]) firstdiff = i;
    snprintf(d, sizeof d, "48 bytes compared after the link pointer, first difference at offset %d, link %p", firstdiff, pf ? *(void **)pf : NULL);
    arm(same, "the 0x38 bytes of the node that the window server copies equal the expected bytes exactly", d);

    void *ri = NULL; r = gliQueryRendererInfo(&ri, 1);
    snprintf(d, sizeof d, "returned %ld; id %#x flags %#x colour modes %#x video memory %u MB", (long)r, ri ? rd32(ri, 0x8) : 0, ri ? rd32(ri, 0xc) : 0, ri ? rd32(ri, 0x14) : 0, ri ? rd32(ri, 0x4c) : 0);
    arm(r == 0 && ri && rd32(ri, 0x8) == rd32(pf, 0x8) && (rd32(ri, 0xc) & 0x100) && (rd32(ri, 0x14) & 0xc000) == 0xc000 && rd32(ri, 0x4c) > 0,
        "the renderer-info node carries the same renderer id, the accelerated flag, both 32-bit colour modes and a video-memory size where the window server reads them", d);
    void *ri2 = (void *)1; r = gliQueryRendererInfo(&ri2, 2);
    snprintf(d, sizeof d, "returned %ld, node %p", (long)r, ri2);
    arm(r != 0 && ri2 == NULL, "a display mask this renderer does not serve gets no node and an error", d);
    gliDestroyRendererInfo(ri);

    struct ws_context *c = calloc(1, sizeof *c), *dummy = calloc(1, sizeof *dummy);
    r = gliCreateContext((void **)&c->rend, pf, NULL, &c->disp, &dummy->disp, 0);
    int empty = 0; for (size_t i = 0; i < sizeof(GLIFunctionDispatch) / sizeof(void *); i++) empty += ((void **)&c->disp)[i] == NULL;
    snprintf(d, sizeof d, "returned %ld, context %p, empty slots %d of %zu", (long)r, (void *)c->rend, empty, sizeof(GLIFunctionDispatch) / sizeof(void *));
    arm(r == 0 && c->rend && empty == 0, "gliCreateContext with our pixel format returns a context and fills every slot of the caller's table", d);
    if (!c->rend) { printf("%d arms, %d failed\n", arms, failed + 1); return 1; }
    gliDestroyPixelFormat(pf);

    uint32_t seven = 7; intptr_t r1 = gliSetInteger(c->rend, 701, &seven);
    intptr_t r2 = gliAttachDrawableWithOptions(c->rend, 0, NULL, 0, 0);
    snprintf(d, sizeof d, "set 701 -> %ld, attach none -> %ld", (long)r1, (long)r2);
    arm(r1 == 0 && r2 == 0, "the window server's two calls after creation (parameter 701 = 7, attach with no drawable) succeed", d);

    /* ---- a frame through the table --------------------------------------------------------------------- */
    enum { W = 64, H = 64, RB = W * 4 + 16 };   /* row bytes larger than the width, as a caller may pass */
    uint8_t *img = calloc(H, RB);
    nm_drawable_offscreen off = { W, H, RB, 0, img };
    r = gliAttachDrawableWithOptions(c->rend, NM_DRAWABLE_OFFSCREEN, &off, 0, 0);
    GLIFunctionDispatch *t = &c->disp;
    t->viewport(c->rend, 0, 0, W, H);
    t->clear_color(c->rend, 0, 0, 1, 1);
    t->clear(c->rend, 0x4000 /* colour */);
    t->color3f(c->rend, 1, 0, 0);
    quad(c, -1, -1, 0, 0, 0, 0);                 /* lower-left quadrant, red */
    t->finish(c->rend);
    int red_ll = count(img, RB, 0, H / 2, W / 2, H, 0, 0, 255), red_all = count(img, RB, 0, 0, W, H, 0, 0, 255), blue_all = count(img, RB, 0, 0, W, H, 255, 0, 0);
    snprintf(d, sizeof d, "attach -> %ld; red in the lower-left quadrant %d of %d, red anywhere %d, blue %d of %d", (long)r, red_ll, W * H / 4, red_all, blue_all, W * H * 3 / 4);
    arm(r == 0 && red_ll == W * H / 4 && red_all == W * H / 4 && blue_all == W * H * 3 / 4,
        "PIXELS: a blue clear and a red immediate-mode quad drawn through the table land exactly where expected in the caller's memory", d);

    /* negative control for the pixel check: before anything is drawn in the upper-right quadrant it is not green */
    int green_ur0 = count(img, RB, W / 2, 0, W, H / 2, 0, 255, 0);
    snprintf(d, sizeof d, "green in the upper-right quadrant before the textured quad: %d", green_ur0);
    arm(green_ur0 == 0, "control: the pixel counter reports 0 for a colour that has not been drawn", d);

    /* ---- IOSurface as a texture: parameter 910 ---------------------------------------------------------- */
    IOSurfaceRef ios = make_iosurface(16, 16, 0, 255, 0);
    GLuint tex[2] = { 0, 0 };
    t->gen_textures(c->rend, 2, tex);
    t->bind_texture(c->rend, 0x84F5, tex[0]);
    t->tex_parameteri(c->rend, 0x84F5, 0x2801, 0x2600); t->tex_parameteri(c->rend, 0x84F5, 0x2800, 0x2600);   /* nearest */
    nm_tex_iosurface b910 = { ios ? IOSurfaceGetID(ios) : 0, 0x84F5, 0x1908, 16, 16, 0x80E1, 0x8367, 0 };
    r = gliSetInteger(c->rend, 910, &b910);
    t->enable(c->rend, 0x84F5);
    t->tex_envi(c->rend, 0x2300, 0x2200, 0x1E01);   /* replace */
    t->color3f(c->rend, 1, 1, 1);
    quad(c, 0, 0, 1, 1, 16, 16);                   /* upper-right quadrant */
    t->finish(c->rend);
    int green_ur = count(img, RB, W / 2, 0, W, H / 2, 0, 255, 0);
    snprintf(d, sizeof d, "IOSurface %p, set 910 -> %ld; green in the upper-right quadrant %d of %d", (void *)ios, (long)r, green_ur, W * H / 4);
    arm(ios && r == 0 && green_ur == W * H / 4, "PIXELS: an IOSurface bound through parameter 910 textures a quad with its contents", d);
    nm_tex_iosurface bad910 = { 0x7fffff01, 0x84F5, 0x1908, 16, 16, 0x80E1, 0x8367, 0 };
    r = gliSetInteger(c->rend, 910, &bad910);
    snprintf(d, sizeof d, "returned %ld", (long)r);
    arm(r != 0, "parameter 910 with an IOSurface id that does not exist fails instead of leaving the texture as it was silently", d);

    /* ---- kernel surface as a texture: parameter 997, through the software stand-in for the accelerator -- */
    enum { SW = 8, SH = 8 };
    uint8_t *surf = malloc(SW * SH * 4);
    for (int i = 0; i < SW * SH; i++) { surf[4 * i] = 255; surf[4 * i + 1] = 0; surf[4 * i + 2] = 255; surf[4 * i + 3] = 255; }   /* magenta */
    nm_test_register_surface(77, SW, SH, SW * 4, surf);
    t->bind_texture(c->rend, 0x84F5, tex[1]);
    t->tex_parameteri(c->rend, 0x84F5, 0x2801, 0x2600); t->tex_parameteri(c->rend, 0x84F5, 0x2800, 0x2600);
    nm_tex_surface b997 = { 77, 0x84F5, 0x1908, SW, SH, 0x1908, 0x405 };
    r = gliSetInteger(c->rend, 997, &b997);
    quad(c, -1, 0, 0, 1, SW, SH);                  /* upper-left quadrant */
    t->finish(c->rend);
    int mag_ul = count(img, RB, 0, 0, W / 2, H / 2, 255, 0, 255);
    snprintf(d, sizeof d, "set 997 -> %ld; magenta in the upper-left quadrant %d of %d", (long)r, mag_ul, W * H / 4);
    arm(r == 0 && mag_ul == W * H / 4, "PIXELS: a surface id bound through parameter 997 textures a quad with the surface's contents", d);
    nm_tex_surface bad997 = { 4242, 0x84F5, 0x1908, SW, SH, 0x1908, 0x405 };
    r = gliSetInteger(c->rend, 997, &bad997);
    snprintf(d, sizeof d, "returned %ld", (long)r);
    arm(r != 0, "parameter 997 with a surface id the accelerator does not know fails: there is no silent fallback", d);
    t->disable(c->rend, 0x84F5);

    /* the three earlier quadrants are still what they were: nothing was overwritten */
    int still = count(img, RB, 0, H / 2, W / 2, H, 0, 0, 255) + count(img, RB, W / 2, 0, W, H / 2, 0, 255, 0) + count(img, RB, W / 2, H / 2, W, H, 255, 0, 0);
    snprintf(d, sizeof d, "red + green + blue quadrant pixels %d of %d", still, 3 * W * H / 4);
    arm(still == 3 * W * H / 4, "PIXELS: the final frame holds all four quadrants: red, green, magenta and the blue clear", d);

    /* ---- a refused table entry records a GL error; an implemented one does not ------------------------- */
    GLenum e0 = t->get_error(c->rend);
    t->vertex_blend_ARB(c->rend, 1);
    GLenum e1 = t->get_error(c->rend), e2 = t->get_error(c->rend);
    snprintf(d, sizeof d, "error before %#x, after the refused entry %#x, then %#x", e0, e1, e2);
    arm(e0 == 0 && e1 == 0x0502 && e2 == 0, "the frame left no GL error, and a refused table entry records GL_INVALID_OPERATION once", d);

    /* ---- attach by surface id: drawable type 0x5a -------------------------------------------------------- */
    enum { DW = 32, DH = 32 };
    uint8_t *dsurf = calloc(DW * DH, 4);
    nm_test_register_surface(88, DW, DH, DW * 4, dsurf);
    uint32_t desc5a[6] = { 0, 0, 88, 0, 0, 0 };
    r = gliAttachDrawableWithOptions(c->rend, NM_DRAWABLE_SURFACE_UNFLIP, desc5a, 0, 0);
    t->viewport(c->rend, 0, 0, DW, DH);
    t->clear_color(c->rend, 0, 1, 0, 1);
    t->clear(c->rend, 0x4000);
    t->finish(c->rend);
    int green_surf = count(dsurf, DW * 4, 0, 0, DW, DH, 0, 255, 0);
    snprintf(d, sizeof d, "attach 0x5a -> %ld; green pixels in the surface %d of %d", (long)r, green_surf, DW * DH);
    arm(r == 0 && green_surf == DW * DH, "PIXELS: a context attached by surface id (type 0x5a, the window server's descriptor) draws into that surface", d);
    uint32_t bad5a[6] = { 0, 0, 9999, 0, 0, 0 };
    r = gliAttachDrawableWithOptions(c->rend, NM_DRAWABLE_SURFACE_UNFLIP, bad5a, 0, 0);
    snprintf(d, sizeof d, "returned %ld", (long)r);
    arm(r != 0, "attaching to a surface id the accelerator does not know fails", d);

    /* ---- teardown --------------------------------------------------------------------------------------- */
    r1 = gliDestroyContext(c->rend); r2 = gliDestroyContext(c->rend);
    snprintf(d, sizeof d, "first %ld, second %ld", (long)r1, (long)r2);
    arm(r1 == 0 && r2 != 0, "destroying the context succeeds once and the second attempt is refused", d);

    printf("%d arms, %d failed\n", arms, failed);
    return failed != 0;
}
