// nvmtl_tex_upload.h: ordering and lifetime decisions for asynchronous texture uploads.
//
// replaceRegion and IOSurface uploads currently submit a copy and wait for the GPU before returning
// (nvmtl_vk_image_write*), so a GL app that uploads many textures per frame stalls once per upload. An
// earlier attempt (community PR #96) deferred the wait but was reverted: nvmtl_vk_image_destroy frees the
// image and its memory at once, and IOSurface-backed textures are uploaded then destroyed, so a deferred
// copy could land in freed GPU memory.
//
// This is the decision logic for doing it safely, gated by NVMTL_ASYNC_TEX_UPLOAD. One async upload may be
// in flight per image; its in-flight handle is recorded on the image. Before anything that would overwrite,
// read back, CPU-map or free that image's memory, the pending upload is finished (fence waited) first. Pure,
// so the invariants are unit-tested without a GPU; the plugin supplies the fence primitives.
//
// Invariants (each an arm of test_tex_upload.c):
//   - synchronous path (flag off): a new upload submits-and-waits; nothing is ever left pending, so the
//     destroy/readback guard finds nothing to finish and is a no-op (this is what keeps the current path
//     byte-for-byte the same);
//   - a second upload to the same image finishes the first (it would otherwise overwrite a texture, or reuse
//     staging bytes, while the GPU is still copying);
//   - destroy, readback, CPU map and surface-out finish a pending upload first (no use-after-free: this is
//     the #96 regression this guard exists to prevent);
//   - a draw that merely SAMPLES the image needs no CPU finish: the upload's command buffer was submitted, so
//     queue order plus the image's TRANSFER->SHADER_READ barrier already order the copy before the sample.

#ifndef NVMTL_TEX_UPLOAD_H
#define NVMTL_TEX_UPLOAD_H

#include <stdint.h>

// Per-image async-upload bookkeeping. `infl` is an opaque handle to the in-flight submission (the plugin
// stores an nvmtl_inflight pointer, cast to uint64_t); 0 means no async upload is in flight for this image.
typedef struct { uint64_t infl; } nvmtl_up_state;

typedef enum {
    NVMTL_UP_SUBMIT_WAIT  = 0,   // submit the copy and wait now (the synchronous default)
    NVMTL_UP_SUBMIT_ASYNC = 1,   // submit without waiting; the caller records the handle with nvmtl_up_set
} nvmtl_up_submit;

// Decide how to submit a new upload to an image. If a previous async upload to THIS image is still in
// flight, it must be finished first: its handle is returned through *finish_first (0 means none). The submit
// mode is async only when the flag is on.
static inline nvmtl_up_submit nvmtl_up_before_upload(const nvmtl_up_state *s, int async_flag,
                                                     uint64_t *finish_first)
{
    *finish_first = s ? s->infl : 0;
    return async_flag ? NVMTL_UP_SUBMIT_ASYNC : NVMTL_UP_SUBMIT_WAIT;
}

// What a consumer (destroy, readback, CPU map, surface-out) must finish before touching the image's memory:
// the pending in-flight handle, or 0 for nothing. On the synchronous path `infl` is never set, so this
// returns 0 and the consumer proceeds exactly as it does today.
static inline uint64_t nvmtl_up_before_consume(const nvmtl_up_state *s)
{
    return s ? s->infl : 0;
}

// Record the in-flight handle after an async submit; clear it after the upload has been finished (waited).
static inline void nvmtl_up_set(nvmtl_up_state *s, uint64_t infl) { if (s) s->infl = infl; }
static inline void nvmtl_up_clear(nvmtl_up_state *s) { if (s) s->infl = 0; }

#endif
