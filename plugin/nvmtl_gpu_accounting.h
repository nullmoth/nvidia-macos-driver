/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include <stdint.h>

/* Completed command buffers may be collected out of submission order and
 * their execution intervals may overlap. Keep a bounded, sorted interval union
 * rather than adding fence wait times or charging an overlapping span twice.
 * Beyond this history, late completions are conservatively undercounted. */
#define NVMTL_GPU_ACCOUNTING_RANGES 128u
typedef struct { uint64_t start, end; } nvmtl_gpu_accounting_range;
typedef struct {
    nvmtl_gpu_accounting_range ranges[NVMTL_GPU_ACCOUNTING_RANGES];
    uint64_t floor, busy_ns, last_submit_absolute;
    unsigned count;
} nvmtl_gpu_accounting_state;

static inline int nvmtl_gpu_accounting_add(nvmtl_gpu_accounting_state *state,
    uint64_t start, uint64_t end, uint64_t submitted_absolute)
{
    if (!submitted_absolute || !start || end <= start || end <= state->floor) return 0;
    if (start < state->floor) start = state->floor;
    uint64_t added = end - start, merged_start = start, merged_end = end;
    unsigned first = 0, after;
    while (first < state->count && state->ranges[first].end < start) first++;
    after = first;
    while (after < state->count && state->ranges[after].start <= end) {
        const nvmtl_gpu_accounting_range *r = &state->ranges[after];
        uint64_t a = start > r->start ? start : r->start;
        uint64_t b = end < r->end ? end : r->end;
        if (b > a) added -= b - a;
        if (r->start < merged_start) merged_start = r->start;
        if (r->end > merged_end) merged_end = r->end;
        after++;
    }
    if (added > UINT64_MAX - state->busy_ns) return 0;
    if (first == after && state->count == NVMTL_GPU_ACCOUNTING_RANGES) {
        if (!first) {
            /* An older, disjoint completion is safe to count once, then seal. */
            state->floor = end;
            state->busy_ns += added;
            if (submitted_absolute > state->last_submit_absolute)
                state->last_submit_absolute = submitted_absolute;
            return 1;
        }
        state->floor = state->ranges[0].end;
        for (unsigned i = 1; i < state->count; i++) state->ranges[i - 1] = state->ranges[i];
        state->count--; first--; after--;
    }
    const unsigned removed = after - first, tail = state->count - after;
    if (!removed) {
        for (unsigned i = state->count; i > first; i--) state->ranges[i] = state->ranges[i - 1];
    } else {
        for (unsigned i = 0; i < tail; i++) state->ranges[first + 1 + i] = state->ranges[after + i];
    }
    state->ranges[first].start = merged_start;
    state->ranges[first].end = merged_end;
    state->count = state->count + 1 - removed;
    state->busy_ns += added;
    if (submitted_absolute > state->last_submit_absolute)
        state->last_submit_absolute = submitted_absolute;
    return added != 0;
}
