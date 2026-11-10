#pragma once

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

// Reader skip is an experimental inference: IOSurface seed changes do not
// distinguish a read-write reader lock from a real CPU write. Keep the
// production default off and require an explicit, process-scoped opt-in.
enum { NVMTL_READER_SKIP_STREAK = 4 };

static inline bool nvmtl_reader_skip_opted_in(const char *setting, const char *process)
{
    return setting && setting[0] == '1' && setting[1] == '\0'
        && process && strcmp(process, "WindowServer") == 0;
}

// Returns YES after four consecutive one-seed advances, so the caller can
// treat the fifth as a possible reader-only lock. A multi-step advance is
// accepted only when the caller's plane history accounts for every earlier
// step as this process's writes to other planes. This remains heuristic.
static inline bool nvmtl_reader_skip_observe(uint32_t previous, uint32_t now, uint32_t *streak,
                                              bool shared_vram, bool plane_seed_on,
                                              bool vram_generation_unchanged,
                                              bool other_plane_bumps_cover)
{
    if (!streak) return false;
    if (now == previous) { *streak = 0; return false; }

    bool one_advance = now == (uint32_t)(previous + UINT32_C(1));
    if (!shared_vram) {
        if (!plane_seed_on || !vram_generation_unchanged) { *streak = 0; return false; }
        one_advance = one_advance || other_plane_bumps_cover;
    }
    if (!one_advance) { *streak = 0; return false; }
    if (*streak < NVMTL_READER_SKIP_STREAK) { (*streak)++; return false; }
    return true;
}
