#ifndef NVMTL_SASS_PARAMS_H
#define NVMTL_SASS_PARAMS_H
/*
 * nvmtl_sass_params.h — the parameter block a raw-SASS compute kernel reads.
 *
 * The compiled kernel takes everything it needs through NVIDIA's constant bank 0: buffer addresses,
 * the dispatch grid, bindless texture and sampler handles, the byte offset of each threadgroup
 * allocation inside the shared window, and any constant table the compiler lifted into the block.
 * The compiler records WHERE each of those goes (`launchrec.py`, read out of the cubin itself) and
 * this packs the values the driver resolved into exactly those places.
 *
 * Every offset here comes from the cubin, never from a prediction: the assembler places a
 *    by-value table or an inline-sampler word where it likes, and predicting those disagreed with it
 *    in a fifth of real kernels. A parameter the optimised code never reads is
 *    absent from the cubin; writing it anyway is harmless, leaving a gap in the block is not.
 *
 * Overlap is refused rather than resolved. Two parameters claiming the same byte means the record
 *    and the cubin disagree, and a kernel launched on a half-written bank reads another resource's
 *    descriptor — which the hardware cannot report and no pixel would reveal.
 */
#include <stdint.h>
#include <stddef.h>
#include <string.h>

enum nvmtl_sass_param_kind {
    NVMTL_SASS_BUFFER        = 1,  /* index into `addresses`: a Metal buffer's GPU address */
    NVMTL_SASS_GRID_X        = 2,  /* the dispatch size, in threads, per axis */
    NVMTL_SASS_GRID_Y        = 3,
    NVMTL_SASS_GRID_Z        = 4,
    NVMTL_SASS_TEXTURE       = 5,  /* index into `handles`: an NVK image descriptor index */
    NVMTL_SASS_SAMPLER       = 6,  /* index into `handles`: an NVK sampler descriptor index */
    NVMTL_SASS_TG_OFFSET     = 7,  /* index into `tg_offsets`: bytes into the shared window */
    NVMTL_SASS_CONST_TABLE   = 8,  /* bytes from `tables`, copied in by value */
    NVMTL_SASS_INLINE_SAMPLER = 9, /* index into `handles`: a constexpr sampler's descriptor index */
    /* an `array_ref<void>` argument: `addresses[index]` holds the address of a table THIS driver
     *    built, one 8-byte entry per Metal buffer index, because Metal materialises that array
     *    rather than the app passing it (MPS's ndArrayConvolution2D does this). */
    NVMTL_SASS_ADDRESS_TABLE = 10,
};

#define NVMTL_SASS_MAX_PARAM_BYTES 1024
#define NVMTL_SASS_MAX_PARAMS      64
#define NVMTL_SASS_MAX_TABLES      8

struct nvmtl_sass_param {
    uint32_t offset;   /* from the cubin */
    uint32_t size;     /* from the cubin */
    uint32_t kind;
    uint32_t index;    /* which buffer / handle / threadgroup index / table this is */
    uint32_t unused;   /* the shader never reads it; still written, so the block has no holes */
};

struct nvmtl_sass_table {
    const uint8_t *bytes;
    uint32_t       size;
};

/*
 * Pack one kernel's parameter block.
 *
 * `out` receives `*out_bytes` bytes to copy to constant bank 0 at the kernel's parameter offset.
 * Returns 0, or a negative number naming which precondition failed:
 *   -1 arguments      -2 a parameter lies outside the block      -3 two parameters overlap
 *   -4 a buffer index or address is unusable                     -5 a grid axis is zero or repeated
 *   -6 an unknown kind                                           -7 a handle index is unusable
 *   -8 a threadgroup offset is unusable                          -9 a table is missing or mis-sized
 */
static inline int nvmtl_sass_pack(
    const struct nvmtl_sass_param *params, uint32_t count,
    const uint64_t *addresses, uint32_t address_count,
    const uint64_t *handles, uint32_t handle_count,
    const uint32_t *tg_offsets, uint32_t tg_count,
    const struct nvmtl_sass_table *tables, uint32_t table_count,
    const uint32_t threads[3],
    uint8_t *out, uint32_t out_capacity, uint32_t *out_bytes)
{
    uint8_t claimed[NVMTL_SASS_MAX_PARAM_BYTES];
    uint32_t end = 0, seen_grid = 0;

    if (!params || !count || count > NVMTL_SASS_MAX_PARAMS || !out || !out_bytes || !threads)
        return -1;
    if (out_capacity > NVMTL_SASS_MAX_PARAM_BYTES)
        out_capacity = NVMTL_SASS_MAX_PARAM_BYTES;
    memset(claimed, 0, sizeof claimed);
    memset(out, 0, out_capacity);
    *out_bytes = 0;

    for (uint32_t i = 0; i < count; i++) {
        const struct nvmtl_sass_param *p = &params[i];
        if (!p->size || p->offset >= out_capacity || p->size > out_capacity - p->offset)
            return -2;
        for (uint32_t b = p->offset; b < p->offset + p->size; b++) {
            if (claimed[b])
                return -3;
            claimed[b] = 1;
        }
        uint8_t *dst = out + p->offset;

        switch (p->kind) {
        case NVMTL_SASS_ADDRESS_TABLE:
        case NVMTL_SASS_BUFFER: {
            if (p->size != 8 || (p->offset & 7) || !addresses || p->index >= address_count)
                return -4;
            uint64_t v = addresses[p->index];
            if (!v && !p->unused)
                return -4;                      /* a bound buffer at address 0 is never valid */
            memcpy(dst, &v, 8);
            break;
        }
        case NVMTL_SASS_GRID_X:
        case NVMTL_SASS_GRID_Y:
        case NVMTL_SASS_GRID_Z: {
            uint32_t axis = p->kind - NVMTL_SASS_GRID_X;
            if (p->size != 4 || (p->offset & 3) || !threads[axis] || (seen_grid & (1u << axis)))
                return -5;
            seen_grid |= 1u << axis;
            memcpy(dst, &threads[axis], 4);
            break;
        }
        case NVMTL_SASS_TEXTURE:
        case NVMTL_SASS_SAMPLER:
        case NVMTL_SASS_INLINE_SAMPLER: {
            /* the handle word the shader reads: NVK's descriptor index, already packed by
             * nvmtl_nvk_sass_*_handle_v1 on the side that owns the descriptor tables */
            if (p->size != 8 || (p->offset & 7) || !handles || p->index >= handle_count)
                return -7;
            uint64_t v = handles[p->index];
            memcpy(dst, &v, 8);
            break;
        }
        case NVMTL_SASS_TG_OFFSET: {
            if (p->size != 4 || (p->offset & 3) || !tg_offsets || p->index >= tg_count)
                return -8;
            memcpy(dst, &tg_offsets[p->index], 4);
            break;
        }
        case NVMTL_SASS_CONST_TABLE: {
            if (!tables || p->index >= table_count || p->index >= NVMTL_SASS_MAX_TABLES)
                return -9;
            const struct nvmtl_sass_table *t = &tables[p->index];
            if (!t->bytes || t->size != p->size)
                return -9;
            memcpy(dst, t->bytes, t->size);
            break;
        }
        default:
            return -6;
        }
        if (p->offset + p->size > end)
            end = p->offset + p->size;
    }

    /* every axis of the grid must be present: a kernel whose guard reads an unwritten axis would
     * run the wrong number of threads and still complete */
    if (seen_grid != 7)
        return -5;

    *out_bytes = (end + 7) & ~7u;              /* the bank is filled in 8-byte words */
    return 0;
}

/*
 * The shared window: Metal gives a length per threadgroup index, the kernel reads one window with a
 * byte offset per index. Returns the window size, or 0 if it does not fit `limit`.
 */
static inline uint32_t nvmtl_sass_threadgroup_window(
    const uint32_t *lengths, uint32_t count, uint32_t align, uint32_t limit,
    uint32_t *offsets_out)
{
    uint32_t cursor = 0;
    if (!lengths || !offsets_out || !align || (align & (align - 1)))
        return 0;
    for (uint32_t i = 0; i < count; i++) {
        cursor = (cursor + align - 1) & ~(align - 1);
        offsets_out[i] = cursor;
        if (lengths[i] > limit - cursor)
            return 0;                          /* refuse rather than overlap two allocations */
        cursor += lengths[i];
    }
    return cursor;
}

#endif
