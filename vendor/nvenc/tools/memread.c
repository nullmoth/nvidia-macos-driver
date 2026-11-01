// memread.c: CPU read / copy speed from RM system memory (cpu-mapped, as the decoders' surfaces) vs malloc'd memory.
//   ./memread [MB]
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "rm.h"
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
static double bench(const char *name, const uint8_t *src, uint8_t *dst, size_t n)
{
    double best = 1e9; volatile uint64_t sink = 0;
    for (int r = 0; r < 5; r++) {
        double t0 = now(); memcpy(dst, src, n); double t = now() - t0; if (t < best) best = t;
    }
    double t0 = now(); uint64_t s = 0; for (size_t i = 0; i < n; i += 8) s += *(const uint64_t *)(src + i); sink = s; double tr = now() - t0;
    printf("%-22s memcpy %.2f ms (%.1f GB/s), read %.2f ms (%.1f GB/s)\n", name, best, n / best / 1e6, tr, n / tr / 1e6);
    (void)sink; return best;
}
int main(int argc, char **argv)
{
    size_t n = (size_t)(argc > 1 ? atoi(argv[1]) : 12) << 20;
    Rm rm; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "rm open failed\n"); return 1; }
    RmMem m; if (rm_mem_alloc(&rm, &m, n, 0x1000, true, true, true)) { fprintf(stderr, "alloc failed\n"); return 1; }
    uint8_t *a = malloc(n), *b = malloc(n); memset(a, 1, n); memset(b, 2, n); memset(m.cpu, 3, n);
    bench("malloc -> malloc", a, b, n);
    bench("RM sysmem -> malloc", m.cpu, b, n);
    bench("malloc -> RM sysmem", a, m.cpu, n);
    rm_mem_free(&rm, &m); rm_close(&rm);
    return 0;
}
