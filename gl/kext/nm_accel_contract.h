/*
 * nm_accel_contract.h: the kernel accelerator's user-client contract for the releases that composite through
 * OpenGL, shared by the kext and by the engine bundle.
 *
 * Two parts:
 *   1. The PUBLIC surface client (client type 0). Its method numbers and structures are Apple's and are in the
 *      public headers IOKit/graphics/IOAccelSurfaceConnect.h and IOAccelTypes.h; the window server and the OpenGL
 *      framework reach it through the open-source IOAccel* functions of the IOKit user library. The table below
 *      records, per method, the argument shape those functions send (read from that open source, 10.6 line), so
 *      the kext can check a call before acting on it.
 *   2. OUR private client (the first type number after the public ones). Apple's accelerators of this era each
 *      have private clients for their own GL bundle; ours has one job the public client cannot do: tell the
 *      engine which GPU buffer is behind a surface id, for a surface that may belong to another process. That is
 *      what turns the window server's "texture from surface" request and "attach to surface" request into work on
 *      a buffer the Vulkan driver can address.
 */
#ifndef NM_ACCEL_CONTRACT_H
#define NM_ACCEL_CONTRACT_H

#include <stdint.h>

/* ---- 1. public surface client ---------------------------------------------------------------------------- */

#define NM_SURFACE_CLIENT_TYPE 0u     /* kIOAccelSurfaceClientType */
#define NM_SURFACE_METHOD_COUNT 18u   /* kIOAccelNumSurfaceMethods */

/* Argument shape of one method as the user library calls it. A struct size of NM_VARIABLE means "at least the
 * fixed header": the device region carries a rectangle array after it. */
#define NM_VARIABLE 0xffffffffu
struct nm_surface_method_shape {
    const char *name;        /* the enum name without its prefix, for logs */
    uint32_t scalar_in;      /* number of 64-bit scalar inputs */
    uint32_t struct_in;      /* size of the input structure, 0 for none */
    uint32_t scalar_out;
    uint32_t struct_out;     /* size of the output structure, 0 for none */
};

/* Sizes of the public structures on a 64-bit kernel (IOAccelTypes.h, the non-10.5 layout). */
#define NM_SIZEOF_SURFACE_INFORMATION 88u  /* IOAccelSurfaceInformation: 4 x 8 + 5 x 4 + 4 x 4 + 4 x 4, padded */
#define NM_SIZEOF_SURFACE_READ_DATA   32u  /* IOAccelSurfaceReadData */
#define NM_SIZEOF_SURFACE_SCALING     44u  /* IOAccelSurfaceScaling */
#define NM_SIZEOF_DEVICE_REGION_HEAD  12u  /* IOAccelDeviceRegion without the rectangle array */

/* Index = method number. Order is the public enum's. GetState (2) and SetShapeBacking (6) have no caller in the
 * user library of the 10.6 line (the second is reached only as SetShapeBackingAndLength); their shapes are taken from
 * the neighbouring methods and from the method names in Apple's 10.7 accelerators. */
#define NM_SURFACE_METHOD_SHAPES                                                                              \
    { "ReadLockOptions",            1, 0,                           0, NM_SIZEOF_SURFACE_INFORMATION },       \
    { "ReadUnlockOptions",          1, 0,                           0, 0 },                                   \
    { "GetState",                   0, 0,                           1, 0 },                                   \
    { "WriteLockOptions",           1, 0,                           0, NM_SIZEOF_SURFACE_INFORMATION },       \
    { "WriteUnlockOptions",         1, 0,                           0, 0 },                                   \
    { "Read",                       0, NM_SIZEOF_SURFACE_READ_DATA, 0, 0 },                                   \
    { "SetShapeBacking",            4, NM_VARIABLE,                 0, 0 },                                   \
    { "SetIDMode",                  2, 0,                           0, 0 },                                   \
    { "SetScale",                   1, NM_SIZEOF_SURFACE_SCALING,   0, 0 },                                   \
    { "SetShape",                   2, NM_VARIABLE,                 0, 0 },                                   \
    { "Flush",                      2, 0,                           0, 0 },                                   \
    { "QueryLock",                  0, 0,                           0, 0 },                                   \
    { "ReadLock",                   0, 0,                           0, NM_SIZEOF_SURFACE_INFORMATION },       \
    { "ReadUnlock",                 0, 0,                           0, 0 },                                   \
    { "WriteLock",                  0, 0,                           0, NM_SIZEOF_SURFACE_INFORMATION },       \
    { "WriteUnlock",                0, 0,                           0, 0 },                                   \
    { "Control",                    2, 0,                           1, 0 },                                   \
    { "SetShapeBackingAndLength",   5, NM_VARIABLE,                 0, 0 }

/* ---- 2. our private client -------------------------------------------------------------------------------- */

#define NM_GL_CLIENT_TYPE 1u          /* first number after kIOAccelNumClientTypes */

enum {
    NM_GL_METHOD_GET_VERSION = 0,     /* out: nm_gl_version */
    NM_GL_METHOD_SURFACE_BUFFER,      /* in: nm_gl_surface_query, out: nm_gl_surface_buffer */
    NM_GL_METHOD_SURFACE_PRESENTED,   /* in: nm_gl_surface_query: the engine finished drawing into the buffer */
    NM_GL_METHOD_SURFACE_GRANT,       /* in: nm_gl_surface_grant: the owner (or a privileged caller) lets another process read a surface */
    NM_GL_METHOD_COUNT
};

#define NM_GL_CONTRACT_VERSION 1u

/* Memory mapping on the private client: the mapping type IS the surface id. While surfaces are backed by ordinary
 * memory this is how the engine reaches the pixels; nm_gl_surface_buffer.memory_handle is 0 in that case.
 * Which surface an id reaches, and whether the mapping is writable, is decided by nm_surface_table.h: a process
 * reaches its own surfaces read-write, and another process's only read-only and only when granted or privileged. */

struct nm_gl_version {
    uint32_t contract;                /* NM_GL_CONTRACT_VERSION; the engine refuses to run against another */
    uint32_t reserved;
};

struct nm_gl_surface_query {
    uint32_t surface_id;              /* the id given to kIOAccelSurfaceSetIDMode, any process's surface */
    uint32_t buffer;                  /* 0x404 front, 0x405 back: the window server's own choice for parameter 997 */
};

struct nm_gl_surface_buffer {
    uint64_t memory_handle;           /* handle of the buffer in the GPU memory manager, as the Vulkan driver names it */
    uint64_t size;
    uint32_t width, height;
    uint32_t row_bytes;
    uint32_t format;                  /* one of the kIOAccelSurfaceModeColorDepth values of the surface's mode */
    uint32_t generation;              /* changes when the surface is reshaped and the buffer replaced */
    uint32_t access;                  /* 1 read-only, 2 read-write: what the caller's mapping of the pages will allow */
};

struct nm_gl_surface_grant {
    uint32_t surface_id;
    int32_t  owner_pid;               /* 0 = the caller itself */
    int32_t  grantee_pid;
    uint32_t revoke;                  /* 0 grant, 1 revoke */
};

#ifdef __cplusplus
static_assert(sizeof(nm_gl_surface_query) == 8 && sizeof(nm_gl_surface_buffer) == 40 && sizeof(nm_gl_version) == 8 && sizeof(nm_gl_surface_grant) == 16,
              "private client structures have the same layout for 32-bit and 64-bit callers");
#else
_Static_assert(sizeof(struct nm_gl_surface_query) == 8 && sizeof(struct nm_gl_surface_buffer) == 40 &&
               sizeof(struct nm_gl_version) == 8 && sizeof(struct nm_gl_surface_grant) == 16, "private client structures have the same layout for 32-bit and 64-bit callers");
#endif

#endif
