/* test_loss.c - the device-loss cause line, proven on the host.
   Build and run anywhere:  cc -I plugin tools/test_loss.c -o /tmp/test_loss && /tmp/test_loss */
#include "nvmtl_loss.h"
static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)
int main(void)
{
    char s[600];
    nvmtl_loss_cause(s, sizeof s, "fence", -4, 1200, "present", 90, "WindowServer");
    ARM("a lost fence names the process, the result, the wait, the caller and the uptime",
        strstr(s, "WindowServer") && strstr(s, "VK_ERROR_DEVICE_LOST") && strstr(s, "1200 ms") && strstr(s, "present") && strstr(s, "90 s"));
    ARM("a fence loss says the GPU faulted while the work ran", strstr(s, "faulted or was reset while this work ran"));
    nvmtl_loss_cause(s, sizeof s, "submit", -4, 0, "composite", 5, "WindowServer");
    ARM("a submit refused as lost says the channel was already dead", strstr(s, "already dead"));
    nvmtl_loss_cause(s, sizeof s, "fence", 2, 50000, "app", 700, "Game");
    ARM("a fence that never answers is called a stalled channel", strstr(s, "stalled channel") && strstr(s, "VK_TIMEOUT"));
    nvmtl_loss_cause(s, sizeof s, "submit", -2, 0, NULL, 1, NULL);
    ARM("running out of device memory is not blamed on a GPU fault", strstr(s, "ran out of memory") && !strstr(s, "faulted"));
    nvmtl_loss_cause(s, sizeof s, "submit", -99, 0, "x", 1, "p");
    ARM("an unknown result is still named", strstr(s, "an unknown VkResult") && strstr(s, "(-99)"));
    char tiny[24]; nvmtl_loss_cause(tiny, sizeof tiny, "fence", -4, 1, "x", 1, "p");
    ARM("a small buffer is cut, not overrun", strlen(tiny) < sizeof tiny);
    return fails ? 1 : 0;
}
