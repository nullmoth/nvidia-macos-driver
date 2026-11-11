/*
 * nm_context.c: the engine contract served by this bundle itself: renderer-info and pixel-format objects in the
 * layouts the window server and the framework read, contexts on Mesa, drawables, and the integer parameters that
 * do real work (IOSurface and kernel-surface textures). The Mesa side is in nm_mesa.c.
 *
 * Kernel surfaces. Two requests name a surface of the kernel accelerator by id: attaching a context to one
 * (drawable types 0x50 and 0x5a) and showing one as a texture (parameters 997 and 999). Both go through
 * nm_surface_lookup(), which is where the accelerator's private client (kext/nm_accel_contract.h, "buffer behind
 * a surface id") plugs in:
 *   - host tests: the surface is a block of the test's memory registered under the id (NM SOFTWARE SURFACE
 *     FALLBACK below), drawn into or uploaded from by copy;
 *   - with our accelerator present: the private client is asked for the surface and its pages are mapped into
 *     this process (the accelerator's memory-backed surfaces; GPU-backed ones will hand out a memory handle);
 *   - anything else: the request fails with an error and is logged. It never succeeds silently with nothing shown.
 * The accelerator-present path cannot be run without loading the kext, so it is proved by compile, link and
 * import checks only.
 */
#include <OpenGL/gliDispatch.h>
#include <IOSurface/IOSurface.h>
#include <IOKit/IOKitLib.h>
#include <mach/mach.h>

#include "../kext/nm_accel_contract.h"

#include "../engine/nm_gli.h"
#include "../glue/nm_glue.h"
#include "nm_mesa.h"
#include "nm_own.h"

#include <pthread.h>
#include <stdlib.h>
#include <string.h>

/* Renderer identifier: the accelerated class (0x00020000) with a vendor byte no renderer Apple shipped uses in
 * the bundles read so far. Reported in both objects; the window server matches the two by it. */
#define NM_RENDERER_ID 0x00027e00u
#define NM_VIDEO_MEMORY_MB 1024u   /* placeholder until the accelerator reports the card's memory */

#define GL_INVALID_OPERATION 0x0502
#define GL_UNPACK_ROW_LENGTH 0x0CF2

extern void nm_fill_dispatch(GLIFunctionDispatch *d);
extern const unsigned nm_dispatch_slot_count;
extern void _dispatch_stub_TexImage2D(GLenum target, GLint level, GLint internalformat, GLsizei width, GLsizei height,
                                      GLint border, GLenum format, GLenum type, const GLvoid *pixels);
extern void _dispatch_stub_PixelStorei(GLenum pname, GLint param);
extern void _dispatch_stub_GetIntegerv(GLenum pname, GLint *params);

struct nm_ctx {
    uint32_t magic;
    struct nm_mesa_ctx *mesa;
    int drawable_type;
    uint32_t swap_interval;
};
#define NM_CTX_MAGIC 0x6e6d6778u

#define NM_MAX_OBJECTS 256
static struct {
    pthread_mutex_t lock;
    int active;
    enum nm_backend backend;
    unsigned display_mask;
    void *objects[NM_MAX_OBJECTS];       /* every node and context this bundle handed out */
    char kind[NM_MAX_OBJECTS];           /* 'r' renderer info, 'p' pixel format, 'c' context */
} own = { PTHREAD_MUTEX_INITIALIZER, 0, NM_BACKEND_SOFTWARE, 0, {0}, {0} };

static int nm_track(void *p, char kind)
{
    int ok = 0;
    pthread_mutex_lock(&own.lock);
    for (int i = 0; i < NM_MAX_OBJECTS && !ok; i++)
        if (!own.objects[i]) { own.objects[i] = p; own.kind[i] = kind; ok = 1; }
    pthread_mutex_unlock(&own.lock);
    return ok;
}
static int nm_is(const void *p, char kind)
{
    int r = 0;
    if (!p) return 0;
    pthread_mutex_lock(&own.lock);
    for (int i = 0; i < NM_MAX_OBJECTS && !r; i++) r = own.objects[i] == p && own.kind[i] == kind;
    pthread_mutex_unlock(&own.lock);
    return r;
}
static void nm_untrack(void *p)
{
    pthread_mutex_lock(&own.lock);
    for (int i = 0; i < NM_MAX_OBJECTS; i++) if (own.objects[i] == p) { own.objects[i] = NULL; own.kind[i] = 0; }
    pthread_mutex_unlock(&own.lock);
}

int nm_own_active(void) { return own.active; }
int nm_own_is_renderer_info(const void *n) { return nm_is(n, 'r'); }
int nm_own_is_pixel_format(const void *n)  { return nm_is(n, 'p'); }
int nm_own_is_context(const void *c)       { return nm_is(c, 'c'); }
void nm_own_free_node(void *n) { nm_untrack(n); free(n); }

/* The connection to our accelerator's private client, opened once when the library is initialised. */
static io_connect_t nm_accel_connect;

/* The accelerator of ours is recognised by its class in the registry. For every display service of that class
 * the display's bit is added to the mask this renderer serves (bit = position in the list the caller passed:
 * INFERRED to be the display index, as the callers build the list per display). */
void nm_own_library_init(const uint32_t *services, const uint8_t *framebuffer_indices, unsigned count)
{
    (void)framebuffer_indices;
    for (unsigned i = 0; services && i < count && i < 32; i++) {
        io_service_t svc = (io_service_t)services[i];
        if (!svc || !IOObjectConformsTo(svc, "NMLegacyAccelerator")) continue;
        if (!nm_accel_connect) {
            io_connect_t c = 0;
            if (IOServiceOpen(svc, mach_task_self(), NM_GL_CLIENT_TYPE, &c) != KERN_SUCCESS) { nm_log("context layer: accelerator found but its private client would not open"); continue; }
            struct nm_gl_version v = { 0, 0 }; size_t vs = sizeof v;
            if (IOConnectCallStructMethod(c, NM_GL_METHOD_GET_VERSION, NULL, 0, &v, &vs) != KERN_SUCCESS || v.contract != NM_GL_CONTRACT_VERSION) {
                nm_log("context layer: accelerator speaks contract %u, this bundle %u; not used", v.contract, NM_GL_CONTRACT_VERSION);
                IOServiceClose(c);
                continue;
            }
            nm_accel_connect = c;
        }
        own.display_mask |= 1u << i;
        /* The Vulkan backend draws on the card and its result is copied into the surface's pages at flush (the
         * same copy the CPU rasteriser uses); if the Vulkan driver cannot start, the CPU rasteriser draws. */
        own.backend = NM_BACKEND_VULKAN;
        own.active = 1;
    }
    if (!own.active) nm_log("context layer: no accelerator of ours among %u display services; every request is forwarded", count);
    else nm_log("context layer: serving display mask 0x%x", own.display_mask);
}

uint32_t nm_own_table_size(void) { return (uint32_t)sizeof(GLIFunctionDispatch); }

/* ---- renderer info and pixel format ----------------------------------------------------------------------- */

static void nm_fill_common(uint32_t *id, uint32_t *flags, uint32_t *display_mask)
{
    *id = NM_RENDERER_ID;
    *flags = NM_RF_WINDOW | NM_RF_FULLSCREEN | NM_RF_OFFSCREEN | NM_RF_BACKING_STORE | NM_RF_ACCELERATED |
             NM_RF_COMPLIANT | NM_RF_GPU_VERTEX | NM_RF_GPU_FRAGMENT;
    *display_mask = own.display_mask;
}

void *nm_own_renderer_info(unsigned display_mask)
{
    if (!own.active || !(display_mask & own.display_mask)) return NULL;
    nm_renderer_node *n = calloc(1, sizeof *n);
    if (!n) return NULL;
    nm_fill_common(&n->rec.renderer_id, &n->rec.flags, &n->rec.display_mask);
    n->rec.buffer_modes  = NM_BUF_MONOSCOPIC | NM_BUF_SINGLE | NM_BUF_DOUBLE;
    n->rec.color_modes   = NM_COLOR_RGB888 | NM_COLOR_ARGB8888;
    n->rec.accum_modes   = 0;
    n->rec.depth_modes   = NM_BITS_0 | NM_BITS_24;
    n->rec.stencil_modes = NM_BITS_0 | 0x80 /* 8 bits */;
    n->rec.max_aux_buffers = 0;
    n->rec.max_sample_buffers = 0;
    n->rec.max_samples = 0;
    n->rec.video_memory_mb = NM_VIDEO_MEMORY_MB;
    n->rec.texture_memory_mb = NM_VIDEO_MEMORY_MB;
    if (!nm_track(n, 'r')) { free(n); return NULL; }
    return n;
}

/*
 * The attribute block's format is not decoded (the window server passes a static block whose bytes are not in the
 * binary's initialised data), so every request gets the one format this renderer offers: the one the 10.11.6
 * window server accepts for its compositor (window or accelerated flag, 32-bit colour with alpha, no depth, no
 * stencil, no aux buffers).
 */
void *nm_own_pixel_format(const void *attributes)
{
    (void)attributes;
    if (!own.active) return NULL;
    nm_pixel_format_node *n = calloc(1, sizeof *n);
    if (!n) return NULL;
    nm_fill_common(&n->rec.renderer_id, &n->rec.flags, &n->rec.display_mask);
    n->rec.buffer_modes = NM_BUF_MONOSCOPIC | NM_BUF_SINGLE;
    n->rec.color_mode   = NM_COLOR_ARGB8888;
    n->rec.accum_mode   = 0;
    n->rec.depth_mode   = NM_BITS_0;
    n->rec.stencil_mode = NM_BITS_0;
    n->rec.aux_buffers  = 0;
    if (!nm_track(n, 'p')) { free(n); return NULL; }
    return n;
}

/* ---- contexts --------------------------------------------------------------------------------------------- */

static struct nm_ctx *nm_ctx_of(const void *rend)
{
    struct nm_ctx *c = (struct nm_ctx *)rend;
    return (nm_is(rend, 'c') && c->magic == NM_CTX_MAGIC) ? c : NULL;
}

/* (out engine context, pixel format, engine context to share with, table to fill, second table, flags): the
 * call the 10.11.6 window server makes. The caller owns the memory of both tables; the second one is the table
 * Apple's engine switches to for display-list compilation and is filled the same way here, since Mesa switches
 * modes inside its own dispatch. */
long nm_own_create_context(void **rend_out, const void *pixel_format, void *share, void *table, void *dummy_table, long flags)
{
    (void)flags;
    if (!rend_out || !table) return NM_BAD_VALUE;
    if (!nm_own_is_pixel_format(pixel_format)) return NM_BAD_PIXEL_FORMAT;
    struct nm_ctx *sh = share ? nm_ctx_of(share) : NULL;
    if (share && !sh) return NM_BAD_CONTEXT;        /* sharing with a context of another engine cannot work */
    if (nm_mesa_init(own.backend) != 0) { nm_log("context layer: backend %d could not be created", (int)own.backend); return NM_BAD_ALLOC; }
    struct nm_ctx *c = calloc(1, sizeof *c);
    if (!c) return NM_BAD_ALLOC;
    c->magic = NM_CTX_MAGIC;
    c->mesa = nm_mesa_ctx_create(sh ? sh->mesa : NULL);
    if (!c->mesa || !nm_track(c, 'c')) { if (c->mesa) nm_mesa_ctx_destroy(c->mesa); free(c); return NM_BAD_ALLOC; }
    nm_fill_dispatch((GLIFunctionDispatch *)table);
    if (dummy_table) nm_fill_dispatch((GLIFunctionDispatch *)dummy_table);
    *rend_out = c;
    nm_log("context layer: created context %p on %s", (void *)c, nm_mesa_backend_name());   /* names the renderer actually in use */
    return NM_OK;
}

long nm_own_destroy_context(void *rend)
{
    struct nm_ctx *c = nm_ctx_of(rend);
    if (!c) return NM_BAD_CONTEXT;
    nm_untrack(c);
    nm_mesa_ctx_destroy(c->mesa);
    c->magic = 0;
    free(c);
    return NM_OK;
}

void nm_gl_enter(GLIContext ctx)
{
    struct nm_ctx *c = (struct nm_ctx *)ctx;
    if (nm_mesa_current() != c->mesa) nm_mesa_make_current(c->mesa);
}

void nm_gl_refuse(GLIContext ctx, const char *entry)
{
    struct nm_ctx *c = (struct nm_ctx *)ctx;
    nm_gl_enter(ctx);
    nm_mesa_record_error(c->mesa, GL_INVALID_OPERATION, entry);
    nm_log("table entry refused: %s", entry);
}

/* ---- kernel surfaces -------------------------------------------------------------------------------------- */

struct nm_surface { uint32_t id; int width, height, row_bytes; void *base; int writable; };

/* NM SOFTWARE SURFACE FALLBACK: host tests register memory under a surface id. A normal build has no entries and
 * no way to add one. */
#ifdef NM_ENGINE_TEST
static struct nm_surface nm_test_surface[16];
__attribute__((visibility("default")))
int nm_test_register_surface(uint32_t id, int width, int height, int row_bytes, void *base)
{
    for (int i = 0; i < 16; i++)
        if (!nm_test_surface[i].base || nm_test_surface[i].id == id) {
            nm_test_surface[i] = (struct nm_surface){ id, width, height, row_bytes, base, 1 };
            return 0;
        }
    return -1;
}
__attribute__((visibility("default")))
void nm_test_own_software(unsigned display_mask)
{
    own.backend = NM_BACKEND_SOFTWARE;
    own.display_mask = display_mask;
    own.active = 1;
}
#endif

/* The buffer behind a surface id. Returns 0 and fills *s, or -1 with the reason logged. */
static int nm_surface_lookup(uint32_t id, struct nm_surface *s)
{
#ifdef NM_ENGINE_TEST
    for (int i = 0; i < 16; i++)
        if (nm_test_surface[i].base && nm_test_surface[i].id == id) { *s = nm_test_surface[i]; return 0; }
#endif
    if (!nm_accel_connect) {
        nm_log("surface %u: the accelerator's private client is not available, so the buffer behind the surface cannot be found", id);
        return -1;
    }
    struct nm_gl_surface_query q = { id, 0x405 };
    struct nm_gl_surface_buffer b; size_t bs = sizeof b;
    memset(&b, 0, sizeof b);
    kern_return_t kr = IOConnectCallStructMethod(nm_accel_connect, NM_GL_METHOD_SURFACE_BUFFER, &q, sizeof q, &b, &bs);
    if (kr != KERN_SUCCESS || bs < sizeof b || !b.width || !b.height) { nm_log("surface %u: the accelerator does not know it (0x%x)", id, kr); return -1; }
    /* one mapping per (surface, generation): a reshape replaces the pages, so the old mapping is dropped */
    static struct { uint32_t id, generation; mach_vm_address_t addr; mach_vm_size_t size; } map[32];
    static pthread_mutex_t map_lock = PTHREAD_MUTEX_INITIALIZER;
    int rc = -1;
    pthread_mutex_lock(&map_lock);
    int slot = -1, free_slot = -1;
    for (int i = 0; i < 32; i++) { if (map[i].addr && map[i].id == id) slot = i; else if (!map[i].addr && free_slot < 0) free_slot = i; }
    if (slot >= 0 && map[slot].generation != b.generation) {
        IOConnectUnmapMemory(nm_accel_connect, id, mach_task_self(), map[slot].addr);
        map[slot].addr = 0; free_slot = slot; slot = -1;
    }
    if (slot < 0 && free_slot >= 0) {
        mach_vm_address_t addr = 0; mach_vm_size_t size = 0;
        if (IOConnectMapMemory(nm_accel_connect, id, mach_task_self(), &addr, &size, kIOMapAnywhere) == KERN_SUCCESS && size >= b.size) {
            slot = free_slot; map[slot].id = id; map[slot].generation = b.generation; map[slot].addr = addr; map[slot].size = size;
        }
    }
    if (slot >= 0) { *s = (struct nm_surface){ id, (int)b.width, (int)b.height, (int)b.row_bytes, (void *)(uintptr_t)map[slot].addr, b.access == 2 }; rc = 0; }
    pthread_mutex_unlock(&map_lock);
    if (rc != 0) nm_log("surface %u: its pages could not be mapped", id);
    return rc;
}

/* ---- drawables -------------------------------------------------------------------------------------------- */

long nm_own_attach_drawable(void *rend, int type, const void *desc, int screen, int options)
{
    (void)screen; (void)options;
    struct nm_ctx *c = nm_ctx_of(rend);
    if (!c) return NM_BAD_CONTEXT;
    switch (type) {
    case NM_DRAWABLE_NONE:
        c->drawable_type = 0;
        return nm_mesa_set_memory_drawable(c->mesa, 0, 0, 0, NULL) == 0 ? NM_OK : NM_BAD_DRAWABLE;
    case NM_DRAWABLE_OFFSCREEN: {
        const nm_drawable_offscreen *d = desc;
        if (!d || !d->base) return NM_BAD_DRAWABLE;
        if (nm_mesa_set_memory_drawable(c->mesa, d->width, d->height, d->row_bytes, d->base) != 0) return NM_BAD_DRAWABLE;
        c->drawable_type = type;
        return NM_OK;
    }
    case NM_DRAWABLE_SURFACE:
    case NM_DRAWABLE_SURFACE_UNFLIP: {
        const nm_drawable_surface *d = desc;
        struct nm_surface s;
        if (!d) return NM_BAD_DRAWABLE;
        if (nm_surface_lookup(d->surface_id, &s) != 0) return NM_BAD_DRAWABLE;
        /* another process's surface is mapped read-only by the accelerator: it can be shown, never drawn into */
        if (!s.writable) { nm_log("attach: surface %u belongs to another process and is read-only here", d->surface_id); return NM_BAD_DRAWABLE; }
        if (nm_mesa_set_memory_drawable(c->mesa, s.width, s.height, s.row_bytes, s.base) != 0) return NM_BAD_DRAWABLE;
        c->drawable_type = type;
        return NM_OK;
    }
    default:
        /* 0x36 (exclusive full screen) has an undecoded descriptor; 0x5c and 0x5d are not requested of an
         * accelerated renderer by any caller read so far */
        nm_log("attach: drawable type 0x%x is not implemented", type);
        return NM_BAD_DRAWABLE;
    }
}

/* ---- integer parameters ----------------------------------------------------------------------------------- */

/* Uploads rows of client memory as the image of the texture bound to target. Copy semantics. */
static void nm_upload(GLenum target, GLint internal_format, GLsizei width, GLsizei height, GLenum format, GLenum type,
                      const void *base, int row_pixels)
{
    GLint old = 0;
    _dispatch_stub_GetIntegerv(GL_UNPACK_ROW_LENGTH, &old);
    _dispatch_stub_PixelStorei(GL_UNPACK_ROW_LENGTH, row_pixels);
    _dispatch_stub_TexImage2D(target, 0, internal_format, width, height, 0, format, type, base);
    _dispatch_stub_PixelStorei(GL_UNPACK_ROW_LENGTH, old);
}

/* 910: the image of an IOSurface becomes the image of the bound texture. First form: by copy at the time of the
 * call (correct whenever the caller sends 910 again after changing the surface, which the window server does by
 * design of its seed check; a zero-copy binding comes with the Vulkan backend). */
static long nm_set_tex_iosurface(struct nm_ctx *c, const nm_tex_iosurface *b)
{
    IOSurfaceRef s = IOSurfaceLookup(b->iosurface_id);
    if (!s) { nm_log("910: IOSurface %u not found", b->iosurface_id); return NM_BAD_VALUE; }
    long rc = NM_BAD_VALUE;
    size_t planes = IOSurfaceGetPlaneCount(s);
    size_t sw = planes ? IOSurfaceGetWidthOfPlane(s, b->plane) : IOSurfaceGetWidth(s);
    size_t sh = planes ? IOSurfaceGetHeightOfPlane(s, b->plane) : IOSurfaceGetHeight(s);
    size_t bpe = planes ? IOSurfaceGetBytesPerElementOfPlane(s, b->plane) : IOSurfaceGetBytesPerElement(s);
    size_t bpr = planes ? IOSurfaceGetBytesPerRowOfPlane(s, b->plane) : IOSurfaceGetBytesPerRow(s);
    if ((planes == 0 ? b->plane == 0 : b->plane < planes) && bpe && b->width && b->height && b->width <= sw && b->height <= sh) {
        if (IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL) == 0) {
            const void *base = planes ? IOSurfaceGetBaseAddressOfPlane(s, b->plane) : IOSurfaceGetBaseAddress(s);
            if (base) {
                nm_gl_enter((GLIContext)c);
                nm_upload(b->target, (GLint)b->internal_format, (GLsizei)b->width, (GLsizei)b->height, b->format, b->type, base, (int)(bpr / bpe));
                rc = NM_OK;
            }
            IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
        }
    } else {
        nm_log("910: request %ux%u plane %u does not fit IOSurface %u (%zux%zu, %zu planes)", b->width, b->height, b->plane, b->iosurface_id, sw, sh, planes);
    }
    CFRelease(s);
    return rc;
}

/* 997 and 999: the image of a kernel surface becomes the image of the bound texture. */
static long nm_set_tex_surface(struct nm_ctx *c, uint32_t surface_id, GLenum target, GLint internal_format)
{
    struct nm_surface s;
    if (nm_surface_lookup(surface_id, &s) != 0) return NM_BAD_VALUE;
    nm_gl_enter((GLIContext)c);
    /* The block's format word describes the texture the caller wants (RGBA at every call site read), not the
     * surface's memory, which is 32-bit BGRA: blue first in memory. */
    nm_upload(target, internal_format, s.width, s.height, 0x80E1 /* BGRA */, 0x1401 /* unsigned byte */, s.base, s.row_bytes / 4);
    return NM_OK;
}

long nm_own_set_integer(void *rend, int pname, const void *value)
{
    struct nm_ctx *c = nm_ctx_of(rend);
    if (!c) return NM_BAD_CONTEXT;
    if (!value) return NM_BAD_VALUE;
    switch (pname) {
    case NM_CP_SWAP_INTERVAL:   c->swap_interval = *(const uint32_t *)value; return NM_OK;
    case NM_CP_CONTEXT_OPTION:  return NM_OK;      /* accepted; its effect in Apple's engine is not decoded */
    case NM_CP_SURFACE_OPACITY: return NM_OK;
    case NM_CP_ACCEL_SYNC:      return NM_OK;
    case NM_CP_TEX_IOSURFACE:   return nm_set_tex_iosurface(c, value);
    case NM_CP_TEX_SURFACE: {
        const nm_tex_surface *b = value;
        return nm_set_tex_surface(c, b->surface_id, b->target, (GLint)b->internal_format);
    }
    case NM_CP_TEX_SURFACE_LAYER: {
        const nm_tex_surface_layer *b = value;
        return nm_set_tex_surface(c, b->surface_id, b->target, 0x1908 /* RGBA */);
    }
    default:
        nm_log("set integer: parameter %d is not implemented for a context of ours", pname);
        return NM_BAD_VALUE;
    }
}

long nm_own_get_integer(void *rend, int pname, void *value)
{
    if (!value) return NM_BAD_VALUE;
    if (pname == NM_CP_DISPATCH_TABLE_SIZE) { *(uint32_t *)value = nm_own_table_size(); return NM_OK; }
    struct nm_ctx *c = nm_ctx_of(rend);
    if (!c) return NM_BAD_CONTEXT;
    switch (pname) {
    case NM_CP_SWAP_INTERVAL: *(uint32_t *)value = c->swap_interval; return NM_OK;
    case 309 /* current renderer id, public */: *(uint32_t *)value = NM_RENDERER_ID; return NM_OK;
    default:
        nm_log("get integer: parameter %d is not implemented for a context of ours", pname);
        return NM_BAD_VALUE;
    }
}
