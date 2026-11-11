/*
 * nm_gli.h: the engine-level contract of Apple's OpenGL framework as read from the 10.7.5 to 10.13.6 binaries.
 *
 * Nothing in this file comes from a private header. Every number and offset was read from the framework, from
 * Apple's engine or from the window server, and the place it was read is named beside it. Offsets are asserted at
 * compile time so a change to a structure cannot move a field silently.
 */
#ifndef NM_GLI_H
#define NM_GLI_H

#include <stddef.h>
#include <stdint.h>

/* gliGetVersion: both loaders (the framework and the window server) require exactly this pair on every release. */
#define NM_GLI_VERSION_MAJOR 2
#define NM_GLI_VERSION_MINOR 3

/*
 * Integer parameters (second argument of gliSetInteger / gliGetInteger).
 * The first three are public (OpenGL/CGLTypes.h). The others are the private numbers the window server passes;
 * their meaning was read from the argument block built at each call site and from the callees of Apple's engine.
 */
enum {
    NM_CP_SWAP_INTERVAL       = 222,  /* public: one int */
    NM_CP_DISPATCH_TABLE_SIZE = 224,  /* public: read once at start-up, with a null context, to size the table */
    NM_CP_SURFACE_OPACITY     = 236,  /* public: sent by the framework when a surface is attached */
    NM_CP_QUERY_320           = 320,  /* read once by the window server's context description (10.11 and later);
                                         Apple's own 10.11 engine has no case for it */
    NM_CP_CONTEXT_OPTION      = 701,  /* one int; the window server sends 7 right after creating its context */
    NM_CP_TEX_IOSURFACE       = 910,  /* nm_tex_iosurface: texture level backed by an IOSurface */
    NM_CP_TEX_SURFACE         = 997,  /* nm_tex_surface: texture level backed by a kernel accelerator surface */
    NM_CP_TEX_PBUFFER         = 998,  /* nm_tex_pbuffer: texture level backed by an offscreen buffer */
    NM_CP_TEX_SURFACE_LAYER   = 999,  /* nm_tex_surface_layer: second surface form, used when a layer is activated */
    NM_CP_ACCEL_SYNC          = 8086  /* nm_accel_sync: sent on finish and on a stereo change (10.7, 10.10) */
};

/* Argument blocks. All fields are 32-bit; the layouts are the store sequences at the window server's call sites. */
typedef struct {            /* 910: built by the window server's IOSurface texture wrapper, 8 stores */
    uint32_t iosurface_id;  /* IOSurfaceGetID() of the surface */
    uint32_t target;        /* texture target, GL_TEXTURE_RECTANGLE (0x84F5) at every call site read */
    uint32_t internal_format;
    uint32_t width, height;
    uint32_t format, type;
    uint32_t plane;
} nm_tex_iosurface;

typedef struct {            /* 997: 7 stores */
    uint32_t surface_id;    /* kernel surface id of the client's surface */
    uint32_t target;
    uint32_t internal_format;
    uint32_t width, height;
    uint32_t format;
    uint32_t buffer;        /* GL_FRONT (0x404) or GL_BACK (0x405): which buffer of the surface to show */
} nm_tex_surface;

typedef struct {            /* 999: 5 stores */
    uint32_t surface_id;
    uint32_t target;
    uint32_t value[3];
} nm_tex_surface_layer;

typedef struct {            /* 998: 8 stores; fields 0 to 4 and 7 are copied from the offscreen buffer's record */
    uint32_t buffer[5];
    uint32_t format;        /* 0x8367 at the call site read */
    uint32_t face_or_level;
    uint32_t buffer_last;
} nm_tex_pbuffer;

typedef struct {            /* 8086 */
    uint32_t op;            /* 3 at the call sites read */
    uint32_t accelerator_index;
} nm_accel_sync;

/* Size of the argument block of a parameter, or 0 when the block is not known (then only the pointer is logged). */
static inline size_t nm_cp_block_size(int pname)
{
    switch (pname) {
    case NM_CP_SWAP_INTERVAL: case NM_CP_DISPATCH_TABLE_SIZE: case NM_CP_SURFACE_OPACITY:
    case NM_CP_QUERY_320: case NM_CP_CONTEXT_OPTION: return sizeof(uint32_t);
    case NM_CP_TEX_IOSURFACE:     return sizeof(nm_tex_iosurface);
    case NM_CP_TEX_SURFACE:       return sizeof(nm_tex_surface);
    case NM_CP_TEX_PBUFFER:       return sizeof(nm_tex_pbuffer);
    case NM_CP_TEX_SURFACE_LAYER: return sizeof(nm_tex_surface_layer);
    case NM_CP_ACCEL_SYNC:        return sizeof(nm_accel_sync);
    default: return 0;
    }
}

/*
 * Drawable types (second argument of gliAttachDrawableWithOptions) and the descriptor of each.
 * Read from the window server's call sites and from Apple's software renderer (10.11.6), which interprets the
 * descriptor itself: for 0x50 it passes the three words to CGSGetSurfaceBounds(connection, window, surface).
 * The framework's own surface attach (CGLSetSurface) passes 0x50; the window server passes zeros for connection
 * and window, naming one of its own surfaces. Apple's engine tests only 0, 2, 3 and 255 and hands the others to
 * the renderer; the software renderer refuses 0x36 and does not know 0x5a, which exist for accelerators only.
 */
enum {
    NM_DRAWABLE_NONE           = 0,
    NM_DRAWABLE_OFFSCREEN      = 0x35,  /* client memory: {width, height, row bytes, pad, base address} */
    NM_DRAWABLE_FULLSCREEN     = 0x36,  /* exclusive full screen: two 64-bit values and one 32-bit (not decoded) */
    NM_DRAWABLE_SURFACE        = 0x50,  /* nm_drawable_surface */
    NM_DRAWABLE_SURFACE_UNFLIP = 0x5a,  /* nm_drawable_surface followed by three zero words (10.10 and later) */
    NM_DRAWABLE_WINDOW_BITS    = 0x5c,  /* software renderer only: {width, height, ...} */
    NM_DRAWABLE_PBUFFER        = 0x5d   /* offscreen buffer record; the options argument carries the level */
};
typedef struct {
    uint32_t connection_id;     /* window-server connection of the owner; 0 from the window server itself */
    uint32_t window_id;         /* 0 from the window server itself */
    uint32_t surface_id;        /* the id the kernel surface client was given with SetIDMode */
} nm_drawable_surface;
typedef struct {
    int32_t  width, height, row_bytes, pad;
    void    *base;
} nm_drawable_offscreen;
static inline size_t nm_drawable_desc_size(int type)
{
    switch (type) {
    case NM_DRAWABLE_OFFSCREEN: return 16 + sizeof(void *);
    case NM_DRAWABLE_SURFACE_UNFLIP: return 24;
    case NM_DRAWABLE_FULLSCREEN: return 20;
    case NM_DRAWABLE_SURFACE: return sizeof(nm_drawable_surface);
    default: return 0;
    }
}

/*
 * Renderer-info object (gliQueryRendererInfo). Read from the framework's CGLDescribeRenderer, which is one switch
 * on the public property number: a 16-byte header whose first word is the record count, then 144-byte records.
 * The same field positions hold on 10.7.5, 10.9.5, 10.11.6 and 10.13.6.
 */
enum {  /* bits of nm_renderer_record.flags, each named by the public property that reads it */
    NM_RF_WINDOW = 1u << 0, NM_RF_FULLSCREEN = 1u << 1, NM_RF_OFFSCREEN = 1u << 2, NM_RF_BACKING_STORE = 1u << 3,
    NM_RF_ROBUST = 1u << 6, NM_RF_ACCELERATED = 1u << 8, NM_RF_MULTISCREEN = 1u << 9, NM_RF_COMPLIANT = 1u << 10,
    NM_RF_AUX_DEPTH_STENCIL = 1u << 11, NM_RF_PBUFFER = 1u << 13, NM_RF_GPU_VERTEX = 1u << 14,
    NM_RF_GPU_FRAGMENT = 1u << 15, NM_RF_ACCELERATED_COMPUTE = 1u << 16
};
typedef struct {
    uint32_t renderer_id;       /* property 70 */
    uint32_t flags;             /* NM_RF_* */
    uint32_t buffer_modes;      /* 100 */
    uint32_t color_modes;       /* 103 */
    uint32_t accum_modes;       /* 104 */
    uint32_t depth_modes;       /* 105 */
    uint32_t stencil_modes;     /* 106 */
    uint32_t display_mask;      /* 84 */
    int16_t  max_aux_buffers;   /* 107 */
    int16_t  max_sample_buffers;/* 108 */
    int16_t  max_samples;       /* 109 */
    uint8_t  sample_alpha;      /* 111 */
    uint8_t  pad0;
    uint32_t sample_modes;      /* 110 */
    uint8_t  pad1[0x18];
    uint32_t video_memory_mb;   /* 120 (scaled to bytes by the framework), 131 */
    uint32_t pad2;
    uint32_t texture_memory_mb; /* 121, 132 */
    uint8_t  pad3[0x40];
} nm_renderer_record;
typedef struct {
    uint32_t count;
    uint8_t  pad[12];
    nm_renderer_record record[];
} nm_renderer_info;

/*
 * Pixel-format object (gliChoosePixelFormat). Read from the framework's GLCDescribePixelFormat: byte 0 is the
 * number of virtual screens, byte 3 a flag byte, and after 16 bytes come 64-byte records whose first fields sit
 * where the renderer record has them.
 */
typedef struct {
    uint32_t renderer_id;       /* attribute 70 */
    uint32_t flags;             /* NM_RF_* */
    uint32_t buffer_modes;      /* double buffer (5), stereo (6), triple buffer (3) */
    uint32_t color_mode;        /* colour size (8) and alpha size (11) are derived from it; the top byte's low
                                   six bits mark a floating-point format (58) */
    uint32_t accum_mode;        /* 14 */
    uint32_t depth_mode;        /* 12 */
    uint32_t stencil_mode;      /* 13 */
    uint16_t pad0;
    int16_t  aux_buffers;       /* 7 */
    int16_t  sample_buffers;    /* 55 */
    int16_t  samples;           /* 56 */
    uint32_t sample_bits;       /* bit 0 multisample (60), bit 1 supersample (59) */
    uint8_t  sample_alpha;      /* 61 */
    uint8_t  pad1[3];
    uint32_t display_mask;      /* 84 */
    uint8_t  pad2[0x10];
} nm_pixel_format_record;
typedef struct {
    uint8_t  screen_count;
    uint8_t  pad0[2];
    uint8_t  flags;
    uint8_t  pad1[12];
    nm_pixel_format_record record[];
} nm_pixel_format;

/*
 * The two objects above are the FRAMEWORK's own (what CGL keeps and describes to applications). What an engine
 * returns is one level down and was read from the 10.11.6 window server, which walks the engine's objects itself,
 * and from Apple's engine: a singly linked list of nodes, each a pointer to the next node followed by the same
 * record. So every field sits 8 bytes lower than in the framework's object (64-bit layout): the window server
 * reads renderer id at 0x8, flags at 0xc, colour mode at 0x14, depth at 0x1c, stencil at 0x20, aux buffers at
 * 0x26 and display mask at 0x34 of a pixel-format node, and colour modes at 0x14 and video memory at 0x4c of a
 * renderer-info node. It copies 0x38 bytes of a pixel-format node. Engine entry points:
 *     int gliChoosePixelFormat(nm_pixel_format_node **out, const void *attributes);   0 on success
 *     int gliQueryRendererInfo(nm_renderer_node **out, unsigned display_mask);        0 on success
 */
typedef struct nm_renderer_node {
    struct nm_renderer_node *next;
    nm_renderer_record rec;
} nm_renderer_node;
typedef struct nm_pixel_format_node {
    struct nm_pixel_format_node *next;
    nm_pixel_format_record rec;
} nm_pixel_format_node;
#if defined(__LP64__)
_Static_assert(offsetof(nm_pixel_format_node, rec.renderer_id) == 0x8 && offsetof(nm_pixel_format_node, rec.flags) == 0xc &&
               offsetof(nm_pixel_format_node, rec.color_mode) == 0x14 && offsetof(nm_pixel_format_node, rec.depth_mode) == 0x1c &&
               offsetof(nm_pixel_format_node, rec.stencil_mode) == 0x20 && offsetof(nm_pixel_format_node, rec.aux_buffers) == 0x26 &&
               offsetof(nm_pixel_format_node, rec.display_mask) == 0x34, "pixel-format node as the window server reads it");
_Static_assert(offsetof(nm_renderer_node, rec.color_modes) == 0x14 && offsetof(nm_renderer_node, rec.video_memory_mb) == 0x4c,
               "renderer-info node as the window server reads it");
#endif

/* Mode bits (public, OpenGL/CGLRenderers.h) used when filling the records. */
enum { NM_BUF_MONOSCOPIC = 0x1, NM_BUF_SINGLE = 0x4, NM_BUF_DOUBLE = 0x8 };
enum { NM_BITS_0 = 0x1, NM_BITS_24 = 0x1000, NM_BITS_32 = 0x4000 };            /* depth and stencil sizes */
enum { NM_COLOR_RGB888 = 0x4000, NM_COLOR_ARGB8888 = 0x8000 };

/* The framework reads each field at (16 + offset) + index * record size; these are those numbers. */
_Static_assert(sizeof(nm_renderer_record) == 0x90, "renderer record is 144 bytes");
_Static_assert(offsetof(nm_renderer_info, record) == 0x10, "records start after a 16-byte header");
_Static_assert(0x10 + offsetof(nm_renderer_record, flags) == 0x14, "flags");
_Static_assert(0x10 + offsetof(nm_renderer_record, buffer_modes) == 0x18, "buffer modes");
_Static_assert(0x10 + offsetof(nm_renderer_record, stencil_modes) == 0x28, "stencil modes");
_Static_assert(0x10 + offsetof(nm_renderer_record, display_mask) == 0x2c, "display mask");
_Static_assert(0x10 + offsetof(nm_renderer_record, max_aux_buffers) == 0x30, "aux buffers");
_Static_assert(0x10 + offsetof(nm_renderer_record, max_samples) == 0x34, "samples");
_Static_assert(0x10 + offsetof(nm_renderer_record, sample_alpha) == 0x36, "sample alpha");
_Static_assert(0x10 + offsetof(nm_renderer_record, sample_modes) == 0x38, "sample modes");
_Static_assert(0x10 + offsetof(nm_renderer_record, video_memory_mb) == 0x54, "video memory");
_Static_assert(0x10 + offsetof(nm_renderer_record, texture_memory_mb) == 0x5c, "texture memory");
_Static_assert(sizeof(nm_pixel_format_record) == 0x40, "pixel format record is 64 bytes");
_Static_assert(offsetof(nm_pixel_format, flags) == 3, "flag byte");
_Static_assert(offsetof(nm_pixel_format, record) == 0x10, "records start after a 16-byte header");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, color_mode) + 3 == 0x1f, "floating-point marker byte");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, aux_buffers) == 0x2e, "aux buffers");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, sample_buffers) == 0x30, "sample buffers");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, samples) == 0x32, "samples");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, sample_bits) == 0x34, "sample bits");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, sample_alpha) == 0x38, "sample alpha");
_Static_assert(0x10 + offsetof(nm_pixel_format_record, display_mask) == 0x3c, "display mask");
_Static_assert(sizeof(nm_tex_iosurface) == 32 && sizeof(nm_tex_surface) == 28 && sizeof(nm_tex_surface_layer) == 20 &&
               sizeof(nm_tex_pbuffer) == 32 && sizeof(nm_accel_sync) == 8, "argument block sizes");

#endif
