/*
 * nm_mesa.c: a gallium front end for the engine bundle (the counterpart of Mesa's own wgl and hgl front ends).
 * It owns the pipe screen, creates contexts on Mesa's state tracker, and supplies the window-system framebuffer
 * the state tracker draws into. See nm_mesa.h for why this file includes Mesa's headers and nothing of Apple's.
 */
#include "nm_mesa.h"

#include "frontend/api.h"
#include "main/errors.h"
#include "pipe/p_context.h"
#include "pipe/p_screen.h"
#include "pipe/p_state.h"
#include "state_tracker/st_context.h"
#include "util/format/u_formats.h"
#include "util/u_atomic.h"
#include "util/u_inlines.h"
#include "util/u_memory.h"

#include "softpipe/sp_public.h"
#include "sw/null/null_sw_winsys.h"
#ifdef NM_WITH_ZINK
#include "zink/zink_public.h"
#endif

#include <pthread.h>
#include <string.h>

struct nm_mesa_drawable {
    struct pipe_frontend_drawable base;      /* must be first: the state tracker hands this pointer back */
    struct st_visual visual;
    struct pipe_resource *tex[ST_ATTACHMENT_COUNT];
    int width, height;                       /* size of the textures in tex[] */
    /* the caller's memory */
    int mem_width, mem_height, mem_row_bytes;
    void *mem_base;
};

struct nm_mesa_ctx {
    struct st_context *st;
    struct nm_mesa_drawable *draw;           /* NULL while no drawable is attached */
};

static struct {
    pthread_mutex_t lock;
    struct pipe_frontend_screen fscreen;
    struct pipe_screen *screen;
    enum nm_backend backend;
    uint32_t next_id;
} nm = { PTHREAD_MUTEX_INITIALIZER, {0}, NULL, NM_BACKEND_SOFTWARE, 0 };

/* The calling thread's current context. Below 10.7 the compiler has no thread-local storage, so the 10.6 build
 * keeps it under a per-thread key (the same switch and helper as the Mesa patch). */
#ifdef NM_NO_COMPILER_TLS
#include "nm_tls.h"
NM_TLS_KEY(nm_tls_current);
#define nm_tls_current NM_TLS_REF(struct nm_mesa_ctx *, nm_tls_current)
#else
static __thread struct nm_mesa_ctx *nm_tls_current;
#endif

const char *nm_mesa_backend_name(void)
{
    return nm.screen ? nm.screen->get_name(nm.screen) : "none";
}

int nm_mesa_init(enum nm_backend backend)
{
    int rc = 0;
    pthread_mutex_lock(&nm.lock);
    if (!nm.screen) {
        struct pipe_screen *s = NULL;
        if (backend == NM_BACKEND_SOFTWARE) {
            struct sw_winsys *ws = null_sw_create();
            if (ws) s = softpipe_create_screen(ws);
        }
#ifdef NM_WITH_ZINK
        else if (backend == NM_BACKEND_VULKAN) {
            /* zink opens the Vulkan driver that sits beside this bundle; with no window system of its own it
             * renders to images only. If the driver is absent or finds no card, the CPU rasteriser takes over:
             * the caller asked for a renderer, and the name it reports says which one it got. */
            s = zink_create_screen(NULL, NULL);
            if (!s) {
                struct sw_winsys *ws = null_sw_create();
                if (ws) s = softpipe_create_screen(ws);
                backend = NM_BACKEND_SOFTWARE;
            }
        }
#endif
        if (s) {
            nm.screen = s;
            nm.fscreen.screen = s;
            nm.backend = backend;
        } else {
            rc = -1;
        }
    } else if (nm.backend != backend) {
        rc = -1;   /* one screen per process: a second backend cannot be added after the first */
    }
    pthread_mutex_unlock(&nm.lock);
    return rc;
}

/* ---- the framebuffer the state tracker draws into --------------------------------------------------------- */

static void nm_drawable_drop_textures(struct nm_mesa_drawable *d)
{
    for (int i = 0; i < ST_ATTACHMENT_COUNT; i++) pipe_resource_reference(&d->tex[i], NULL);
    d->width = d->height = 0;
}

/* The state tracker asks for the attachments it needs; textures are created at the size of the caller's memory. */
static bool nm_drawable_validate(struct st_context *st, struct pipe_frontend_drawable *drawable,
                                 const enum st_attachment_type *statts, unsigned count,
                                 struct pipe_resource **out, struct pipe_resource **resolve)
{
    (void)st;
    struct nm_mesa_drawable *d = (struct nm_mesa_drawable *)drawable;
    if (d->width != d->mem_width || d->height != d->mem_height) nm_drawable_drop_textures(d);
    for (unsigned i = 0; i < count; i++) {
        enum st_attachment_type a = statts[i];
        if (!d->tex[a]) {
            struct pipe_resource t;
            memset(&t, 0, sizeof t);
            t.target = PIPE_TEXTURE_2D;
            t.width0 = (unsigned)d->mem_width; t.height0 = (unsigned)d->mem_height;
            t.depth0 = 1; t.array_size = 1;
            if (a == ST_ATTACHMENT_DEPTH_STENCIL) { t.format = d->visual.depth_stencil_format; t.bind = PIPE_BIND_DEPTH_STENCIL; }
            else if (a == ST_ATTACHMENT_FRONT_LEFT || a == ST_ATTACHMENT_BACK_LEFT) {
                t.format = d->visual.color_format; t.bind = PIPE_BIND_RENDER_TARGET | PIPE_BIND_SAMPLER_VIEW;
            } else { out[i] = NULL; continue; }
            d->tex[a] = nm.screen->resource_create(nm.screen, &t);
            if (!d->tex[a]) return false;
        }
        out[i] = NULL;
        pipe_resource_reference(&out[i], d->tex[a]);
    }
    if (resolve) *resolve = NULL;
    d->width = d->mem_width; d->height = d->mem_height;
    return true;
}

/* Front-buffer drawing reached a flush: copy the colour texture into the caller's memory, row by row, clipped to
 * what both sides hold. The window-system framebuffer is stored top row first, as the memory is. */
static bool nm_drawable_flush_front(struct st_context *st, struct pipe_frontend_drawable *drawable,
                                    enum st_attachment_type statt)
{
    struct nm_mesa_drawable *d = (struct nm_mesa_drawable *)drawable;
    struct pipe_resource *tex = d->tex[statt];
    if (!tex || !d->mem_base) return true;
    struct pipe_context *pipe = st->pipe;
    struct pipe_box box;
    memset(&box, 0, sizeof box);
    box.width = d->width; box.height = d->height; box.depth = 1;
    struct pipe_transfer *xfer = NULL;
    const uint8_t *src = pipe->texture_map(pipe, tex, 0, PIPE_MAP_READ, &box, &xfer);
    if (!src) return false;
    int row = d->width * 4 < d->mem_row_bytes ? d->width * 4 : d->mem_row_bytes;
    for (int y = 0; y < d->height; y++)
        memcpy((uint8_t *)d->mem_base + (size_t)y * (size_t)d->mem_row_bytes, src + (size_t)y * xfer->stride, (size_t)row);
    pipe->texture_unmap(pipe, xfer);
    return true;
}

static struct nm_mesa_drawable *nm_drawable_new(void)
{
    struct nm_mesa_drawable *d = CALLOC_STRUCT(nm_mesa_drawable);
    if (!d) return NULL;
    /* single-buffered: the engine contract presents by flush, there is no swap at this level */
    d->visual.buffer_mask = ST_ATTACHMENT_FRONT_LEFT_MASK | ST_ATTACHMENT_DEPTH_STENCIL_MASK;
    d->visual.color_format = PIPE_FORMAT_B8G8R8A8_UNORM;
    d->visual.depth_stencil_format = PIPE_FORMAT_Z24_UNORM_S8_UINT;
    d->visual.accum_format = PIPE_FORMAT_NONE;
    d->visual.samples = 0;
    d->base.visual = &d->visual;
    d->base.fscreen = &nm.fscreen;
    d->base.validate = nm_drawable_validate;
    d->base.flush_front = nm_drawable_flush_front;
    pthread_mutex_lock(&nm.lock);
    d->base.ID = ++nm.next_id;
    pthread_mutex_unlock(&nm.lock);
    p_atomic_set(&d->base.stamp, 1);
    return d;
}

static void nm_drawable_free(struct nm_mesa_drawable *d)
{
    if (!d) return;
    st_api_destroy_drawable(&d->base);
    nm_drawable_drop_textures(d);
    FREE(d);
}

/* ---- contexts --------------------------------------------------------------------------------------------- */

struct nm_mesa_ctx *nm_mesa_ctx_create(struct nm_mesa_ctx *share)
{
    if (!nm.screen) return NULL;
    struct nm_mesa_ctx *c = CALLOC_STRUCT(nm_mesa_ctx);
    if (!c) return NULL;
    struct st_context_attribs attribs;
    memset(&attribs, 0, sizeof attribs);
    attribs.profile = API_OPENGL_COMPAT;     /* Apple's legacy contexts are compatibility-profile contexts */
    attribs.major = 1; attribs.minor = 0;    /* the minimum; the state tracker returns the highest it supports */
    attribs.visual.buffer_mask = ST_ATTACHMENT_FRONT_LEFT_MASK | ST_ATTACHMENT_DEPTH_STENCIL_MASK;
    attribs.visual.color_format = PIPE_FORMAT_B8G8R8A8_UNORM;
    attribs.visual.depth_stencil_format = PIPE_FORMAT_Z24_UNORM_S8_UINT;
    attribs.visual.accum_format = PIPE_FORMAT_NONE;
    enum st_context_error err = ST_CONTEXT_SUCCESS;
    c->st = st_api_create_context(&nm.fscreen, &attribs, &err, share ? share->st : NULL);
    if (!c->st) { FREE(c); return NULL; }
    return c;
}

void nm_mesa_ctx_destroy(struct nm_mesa_ctx *c)
{
    if (!c) return;
    if (nm_tls_current == c) nm_mesa_make_current(NULL);
    st_context_flush(c->st, 0, NULL, NULL, NULL);
    st_destroy_context(c->st);
    nm_drawable_free(c->draw);
    FREE(c);
}

int nm_mesa_make_current(struct nm_mesa_ctx *c)
{
    bool ok;
    if (!c) ok = st_api_make_current(NULL, NULL, NULL);
    else ok = st_api_make_current(c->st, c->draw ? &c->draw->base : NULL, c->draw ? &c->draw->base : NULL);
    if (ok) nm_tls_current = c;
    return ok ? 0 : -1;
}

struct nm_mesa_ctx *nm_mesa_current(void) { return nm_tls_current; }

int nm_mesa_set_memory_drawable(struct nm_mesa_ctx *c, int width, int height, int row_bytes, void *base)
{
    if (!c) return -1;
    if (!base) {
        struct nm_mesa_drawable *old = c->draw;
        c->draw = NULL;
        if (nm_tls_current == c) nm_mesa_make_current(c);
        nm_drawable_free(old);
        return 0;
    }
    if (width <= 0 || height <= 0 || width > 16384 || height > 16384 || row_bytes < width * 4) return -1;
    if (!c->draw && !(c->draw = nm_drawable_new())) return -1;
    c->draw->mem_width = width; c->draw->mem_height = height;
    c->draw->mem_row_bytes = row_bytes; c->draw->mem_base = base;
    p_atomic_inc(&c->draw->base.stamp);      /* tells the state tracker to validate the framebuffer again */
    if (nm_tls_current == c) return nm_mesa_make_current(c);
    return 0;
}

void nm_mesa_record_error(struct nm_mesa_ctx *c, unsigned gl_error, const char *what)
{
    if (c && c->st) _mesa_error(c->st->ctx, gl_error, "%s", what);
}
