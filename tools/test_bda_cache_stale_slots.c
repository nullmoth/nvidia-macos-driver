#include <assert.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    uint64_t lastw0 = (1ULL << 2) | (1ULL << 33);
    uint64_t chg0 = (1ULL << 2);
    lastw0 &= ~(chg0 & 0xffffffffULL);
    assert((lastw0 & (1ULL << 2)) == 0);
    assert((lastw0 & (1ULL << 33)) != 0);

    puts("test_bda_cache_stale_slots: OK");
    return 0;
}
