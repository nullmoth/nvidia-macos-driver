// av1probs.h (M9): the AV1 decoder's default probability table (the CDFs a frame without a primary reference frame
// starts from), as NVDEC reads it. Layout from mesa-nova (src/nouveau/vulkan/rust/video/decode/av1_probs.rs, MIT): each
// table at a fixed byte offset, each CDF at a fixed stride of 16-bit values, stored as 32768 minus each value, without
// the final 32768 and without counters. Values from the AV1 specification's default tables (av1tables.h).
#pragma once
#include <stdint.h>

enum { AV1_PROBS_B = 0x2fc0 };
static inline int av1_q_ctx(int q) { return q <= 20 ? 0 : q <= 60 ? 1 : q <= 120 ? 2 : 3; }
// The default table of coefficient CDF context q (av1_q_ctx of base_q_idx).
void av1_default_probs(int q, uint8_t table[AV1_PROBS_B]);
