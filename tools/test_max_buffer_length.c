#include <assert.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

static uint64_t calc_max_buffer_bytes(uint64_t max_storage_range, uint64_t max_buffer_size,
                                      uint64_t max_alloc_size, uint64_t guard) {
    uint64_t cap = max_storage_range;
    if (max_buffer_size && (!cap || max_buffer_size < cap)) cap = max_buffer_size;
    if (max_alloc_size) {
        const uint64_t usable = max_alloc_size > guard ? max_alloc_size - guard : 0;
        if (!cap || usable < cap) cap = usable;
    }
    return cap & ~0x3fffULL;
}

int main(void) {
    const uint64_t gib = 1ULL << 30;
    const uint64_t cap = calc_max_buffer_bytes(4 * gib - 1, 0, 2 * gib, 65536);
    assert(cap == 2 * gib - 65536);
    assert((cap & 0x3fffULL) == 0);
    /* A 2515.44 MiB model exceeds cap so ggml_metal_buffer_map splits it across buffers. */
    assert((uint64_t)(2515ULL << 20) > cap);
    /* With guard added back, allocation stays <= 2 GiB. */
    assert(cap + 65536 <= 2 * gib);

    puts("test_max_buffer_length: OK");
    return 0;
}
