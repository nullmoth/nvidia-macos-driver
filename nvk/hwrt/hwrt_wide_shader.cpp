// Runs the passes of encode_wide_body.h (the text the GLSL shader is made of) on the host, one invocation at a time, with the barrier
// between passes being the end of a loop. Called from hwrt_wide.c; see there for what it is compared with.
#include <cmath>
#include <cstdint>
#include <cstring>

struct wide_args {
   uint64_t ir, dst;
   uint32_t leaf_count, internal_count, leaf_type, out_leaf_size, hdr, ir_leaf_size, ir_box_size, pad;
};
#define W_ARGS const wide_args &a
static inline uint32_t ld32(uint64_t p) { uint32_t v; memcpy(&v, (const void *)p, 4); return v; }
static inline float ldf(uint64_t p) { float v; memcpy(&v, (const void *)p, 4); return v; }
static inline void st32(uint64_t p, uint32_t v) { memcpy((void *)p, &v, 4); }
static inline void stf(uint64_t p, float v) { memcpy((void *)p, &v, 4); }
static inline void st8(uint64_t p, uint32_t v) { *(uint8_t *)p = (uint8_t)v; }
#include "encode_wide_body.h"

extern "C" void shader_wide_encode(uint8_t *ir, uint8_t *out, uint32_t leaf_count, uint32_t internal_count, uint32_t leaf_type,
                                   uint32_t out_leaf_size, uint32_t hdr, uint32_t ir_leaf_size, uint32_t ir_box_size, uint32_t rounds)
{
   wide_args a = { (uint64_t)(uintptr_t)ir, (uint64_t)(uintptr_t)out, leaf_count, internal_count, leaf_type, out_leaf_size, hdr, ir_leaf_size, ir_box_size, 0 };
   uint32_t span = internal_count;
   for (uint32_t g = 0; g < span; g++) wide_pass_init(a, g);
   for (uint32_t r = 0; r < rounds; r++)
      for (uint32_t g = 0; g < span; g++) wide_pass_round(a, g);
   for (uint32_t g = 0; g < span; g++) wide_pass_write(a, g);
}
