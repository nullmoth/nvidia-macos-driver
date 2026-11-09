/* test_counter_heap.c - Metal 4 counter heap rules (plugin/nvmtl_counter_heap.h).
   cc -I plugin tools/test_counter_heap.c -lm -o /tmp/test_counter_heap && /tmp/test_counter_heap */
#include <stdio.h>
#include "nvmtl_counter_heap.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    arm("a 1 ns period is 1 GHz", nvmtl_ts_frequency(1.0, 64) == 1000000000ull);
    arm("a 0.5 ns period is 2 GHz and a 32 ns period 31.25 MHz", nvmtl_ts_frequency(0.5, 64) == 2000000000ull && nvmtl_ts_frequency(32.0, 64) == 31250000ull);
    arm("no timestamp bits means no frequency", nvmtl_ts_frequency(1.0, 0) == 0);
    arm("a zero, negative, infinite or NaN period means no frequency", nvmtl_ts_frequency(0.0, 64) == 0 && nvmtl_ts_frequency(-1.0, 64) == 0 && nvmtl_ts_frequency(INFINITY, 64) == 0 && nvmtl_ts_frequency(NAN, 64) == 0);
    arm("a timestamp entry is 8 bytes and the invalid type has none", nvmtl_heap_entry_size(NVMTL_HEAP_TYPE_TIMESTAMP) == 8 && nvmtl_heap_entry_size(NVMTL_HEAP_TYPE_INVALID) == 0);
    arm("a timestamp heap of 100 entries on a working device is allowed", nvmtl_heap_refusal(NVMTL_HEAP_TYPE_TIMESTAMP, 100, 1000000000ull) == 0);
    arm("an invalid type, zero or oversized count, and a device without timestamps are each refused", nvmtl_heap_refusal(0, 100, 1) && nvmtl_heap_refusal(1, 0, 1) && nvmtl_heap_refusal(1, 32769, 1) && nvmtl_heap_refusal(1, 100, 0));
    arm("a resolve fits when entries * 8 fit after the offset", nvmtl_heap_resolve_fits(4, 8, 40) && !nvmtl_heap_resolve_fits(4, 16, 40) && !nvmtl_heap_resolve_fits(0, 0, 40) && !nvmtl_heap_resolve_fits(1, 48, 40));
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
