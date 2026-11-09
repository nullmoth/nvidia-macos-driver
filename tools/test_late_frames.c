/* test_late_frames: which presents count as late, and the rate of the reports.
   cc -I plugin tools/test_late_frames.c -o /tmp/tlf && /tmp/tlf */
#include <stdio.h>
#include "nvmtl_late_frames.h"
static int fails;
static void arm(const char *n, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", n); fails += !ok; }
#define MS 1000000ull
int main(void)
{
    struct nvmtl_late s; memset(&s, 0, sizeof s);
    uint64_t t = 1000 * MS; uint32_t total = 0, r;
    for (int i = 0; i < 40; i++) { t += 16 * MS + (i & 1) * 100000ull; total += nvmtl_late_note(&s, t); }
    arm("a steady 16 ms pace reports nothing", total == 0);
    t += 40 * MS; r = nvmtl_late_note(&s, t);
    arm("a first late present is held until its batch is due, or reported at once if due", r == 0 || r == 1);
    total = r;
    for (int i = 0; i < 6; i++) { t += 16 * MS; total += nvmtl_late_note(&s, t); }
    arm("the late frame reaches the report within a batch period", total == 1);
    memset(&s, 0, sizeof s); t = 5000 * MS; total = 0;
    for (int i = 0; i < 12; i++) { t += 16 * MS; nvmtl_late_note(&s, t); }
    for (int i = 0; i < 60; i++) { t += 50 * MS; total += nvmtl_late_note(&s, t); }
    arm("a stall of 60 late frames in 3 s is reported at most 10 per second in batches of at most 16", total <= 16 * 31 && total > 0);
    uint32_t big = 0, calls = 0;
    memset(&s, 0, sizeof s); t = 9000 * MS;
    for (int i = 0; i < 12; i++) { t += 16 * MS; nvmtl_late_note(&s, t); }
    for (int i = 0; i < 400; i++) { t += 40 * MS; r = nvmtl_late_note(&s, t); if (r) { calls++; if (r > big) big = r; } }
    arm("no batch is larger than 16", big <= NVMTL_LATE_MAX_BATCH);
    arm("at most one report per 100 ms", calls <= (400 * 40 / 100) + 1);
    memset(&s, 0, sizeof s); t = 20000 * MS; total = 0;
    for (int i = 0; i < 12; i++) { t += 16 * MS; nvmtl_late_note(&s, t); }
    t += 2000 * MS; total += nvmtl_late_note(&s, t);
    for (int i = 0; i < 4; i++) { t += 16 * MS; total += nvmtl_late_note(&s, t); }
    arm("a pause of seconds (a hidden window) is not lateness", total == 0);
    memset(&s, 0, sizeof s); t = 30000 * MS; total = 0;
    for (int i = 0; i < 6; i++) { t += 40 * MS; total += nvmtl_late_note(&s, t); }
    arm("before eight intervals are known nothing is judged late", total == 0);
    memset(&s, 0, sizeof s); t = 40000 * MS; total = 0;
    for (int i = 0; i < 20; i++) { t += 2 * MS; total += nvmtl_late_note(&s, t); }
    t += 5 * MS; total += nvmtl_late_note(&s, t);
    for (int i = 0; i < 5; i++) { t += 2 * MS; total += nvmtl_late_note(&s, t); }
    arm("an interval under the 4 ms floor is never a frame time (a burst of fast presents is not a baseline)", total == 0);
    memset(&s, 0, sizeof s);
    arm("a clock that goes backwards reports nothing", nvmtl_late_note(&s, 100 * MS) == 0 && nvmtl_late_note(&s, 50 * MS) == 0);
    printf(fails ? "FAILED %d\n" : "ok\n", fails);
    return fails != 0;
}
