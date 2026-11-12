/*
 * nm_own.h: what the engine entry points (nm_engine.c) ask of the context layer (nm_context.c) for requests this
 * bundle serves itself instead of forwarding.
 */
#ifndef NM_OWN_H
#define NM_OWN_H

#include <stdint.h>

/* One line to the build's record (nothing in a default build, the system log or the test file otherwise). */
void nm_log(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

/* Called from gliInitializeLibrary with the accelerator services of the displays. Decides whether this bundle
 * renders for any of them. */
void nm_own_library_init(const uint32_t *services, const uint8_t *framebuffer_indices, unsigned count);
int nm_own_active(void);

uint32_t nm_own_table_size(void);
void *nm_own_renderer_info(unsigned display_mask);      /* a node of ours for that display, or NULL */
int nm_own_is_renderer_info(const void *node);
void *nm_own_pixel_format(const void *attributes);
int nm_own_is_pixel_format(const void *node);
void nm_own_free_node(void *node);

long nm_own_create_context(void **rend_out, const void *pixel_format, void *share, void *table, void *dummy_table, long flags);
int nm_own_is_context(const void *rend);
long nm_own_destroy_context(void *rend);
long nm_own_attach_drawable(void *rend, int type, const void *desc, int screen, int options);
long nm_own_set_integer(void *rend, int pname, const void *value);
long nm_own_get_integer(void *rend, int pname, void *value);

/* The framework's error codes used as returns (OpenGL/CGLTypes.h). */
enum { NM_OK = 0, NM_BAD_ATTRIBUTE = 10000, NM_BAD_PIXEL_FORMAT = 10002, NM_BAD_CONTEXT = 10004, NM_BAD_DRAWABLE = 10005,
       NM_BAD_VALUE = 10008, NM_BAD_ALLOC = 10016 };

#ifdef NM_ENGINE_TEST
/* Host tests only. Makes the bundle own every request with the software backend, and stands in for the kernel
 * accelerator: a "surface" is a block of the test's own memory registered under an id. */
void nm_test_own_software(unsigned display_mask);
int nm_test_register_surface(uint32_t surface_id, int width, int height, int row_bytes, void *base);
#endif

#endif
