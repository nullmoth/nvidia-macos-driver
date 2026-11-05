#include "nvmtl_surface_schedule.h"
#include <stdio.h>

int main(void)
{
    const char *browsers[] = {"Google Chrome", "Google Chrome Helper (GPU)",
        "Chromium", "Chromium Helper (Renderer)", "Microsoft Edge", "Microsoft Edge Helper (GPU)"};
    const char *others[] = {NULL, "", "WindowServer", "QQ", "Electron", "Safari", "EdgeView"};
    for (unsigned i = 0; i < sizeof browsers / sizeof *browsers; i++)
        if (nvmtl_surface_schedule_after(NULL, browsers[i]) != 1
            || nvmtl_surface_schedule_after("", browsers[i]) != 1) return 1;
    for (unsigned i = 0; i < sizeof others / sizeof *others; i++)
        if (nvmtl_surface_schedule_after(NULL, others[i]) != 3) return 2;
    for (int mode = 0; mode <= 3; mode++) {
        const char setting[] = {(char)('0' + mode), 0};
        if (nvmtl_surface_schedule_after(setting, "Google Chrome Helper (GPU)") != mode
            || nvmtl_surface_schedule_after(setting, "WindowServer") != mode) return 3;
    }
    puts("PASS: Chromium surface publication policy, other-process scope, explicit overrides");
    return 0;
}
