/*
 * nm_mesa.h: the Mesa side of the context layer, as plain C.
 *
 * The context layer is two files on purpose. Apple's OpenGL headers and Mesa's own GL headers define the same type
 * names differently (the shader handle type, for one), so no file includes both: nm_context.c speaks Apple's
 * engine contract and includes Apple's headers, nm_mesa.c speaks Mesa's state tracker and includes Mesa's, and
 * this header, which names no GL type at all, is what they share.
 */
#ifndef NM_MESA_H
#define NM_MESA_H

#include <stdint.h>

struct nm_mesa_ctx;

/* Which pipe driver renders. */
enum nm_backend {
    NM_BACKEND_SOFTWARE = 0,   /* Mesa's CPU rasteriser on a null window system: the fallback, and the host tests */
    NM_BACKEND_VULKAN   = 1    /* zink on the system Vulkan driver: the product path */
};

/* Creates the process-wide screen on first use. Returns 0, or -1 when the backend cannot be created. */
int nm_mesa_init(enum nm_backend backend);
const char *nm_mesa_backend_name(void);

/* A compatibility-profile context, optionally sharing objects with another. NULL on failure. */
struct nm_mesa_ctx *nm_mesa_ctx_create(struct nm_mesa_ctx *share);
void nm_mesa_ctx_destroy(struct nm_mesa_ctx *c);

/* Makes c the calling thread's current Mesa context (NULL releases it). Returns 0 on success. */
int nm_mesa_make_current(struct nm_mesa_ctx *c);
struct nm_mesa_ctx *nm_mesa_current(void);

/*
 * Drawable in caller-owned memory (the engine contract's offscreen drawable): 32-bit BGRA rows of row_bytes,
 * top row first. The front buffer is copied into it whenever the context is flushed with front-buffer drawing
 * pending. base == NULL detaches. Returns 0 on success.
 */
int nm_mesa_set_memory_drawable(struct nm_mesa_ctx *c, int width, int height, int row_bytes, void *base);

/* Records a GL error code on the context (for table entries that have no implementation). */
void nm_mesa_record_error(struct nm_mesa_ctx *c, unsigned gl_error, const char *what);

#endif
