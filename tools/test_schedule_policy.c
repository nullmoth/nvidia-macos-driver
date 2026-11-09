/* test_schedule_policy.c - which surface writes a command buffer publishes before it reports "scheduled".
   Build and run anywhere:  cc -I plugin tools/test_schedule_policy.c -o /tmp/test_schedule_policy && /tmp/test_schedule_policy */
#include "nvmtl_schedule_policy.h"
#include <stdio.h>

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)

int main(void)
{
    ARM("Chrome, its helpers, Chromium and Edge publish every surface write first",
        nvmtl_schedule_after_mode(NULL, "Google Chrome") == 1 && nvmtl_schedule_after_mode(NULL, "Google Chrome Helper (GPU)") == 1 &&
        nvmtl_schedule_after_mode(NULL, "Chromium Helper (Renderer)") == 1 && nvmtl_schedule_after_mode(NULL, "Microsoft Edge Helper (GPU)") == 1);
    ARM("WindowServer, Safari, Electron apps and unnamed processes keep mode 3",
        nvmtl_schedule_after_mode(NULL, "WindowServer") == 3 && nvmtl_schedule_after_mode(NULL, "Safari") == 3 &&
        nvmtl_schedule_after_mode(NULL, "Electron") == 3 && nvmtl_schedule_after_mode(NULL, NULL) == 3 && nvmtl_schedule_after_mode("", "") == 3);
    int all = 1;
    for (char d = '0'; d <= '3'; d++) { const char s[2] = { d, 0 };
        all &= nvmtl_schedule_after_mode(s, "Google Chrome") == d - '0' && nvmtl_schedule_after_mode(s, "WindowServer") == d - '0'; }
    ARM("an explicit 0..3 wins in every process", all);
    ARM("a setting that is not a single digit 0..3 is ignored, not turned into a mode no path knows",
        nvmtl_schedule_after_mode("x", "WindowServer") == 3 && nvmtl_schedule_after_mode("7", "WindowServer") == 3 &&
        nvmtl_schedule_after_mode("12", "Google Chrome") == 1);
    return fails ? 1 : 0;
}
