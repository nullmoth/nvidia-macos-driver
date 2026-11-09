// test_igpu_proxy.c: the decision that makes the NVIDIA framebuffer a proxy head for an integrated-GPU panel.
//
//   cc -std=c11 -Wall -Wextra -I../kexts/NVRMFB -o /tmp/test_igpu_proxy test_igpu_proxy.c && /tmp/test_igpu_proxy
//
// Negative controls that must FIRE: a decision that ignored the boot-arg, the bus check or the BAR overlap fails here.

#include "nvrm_igpu_proxy.h"
#include <stdio.h>

static int g_fail;
#define CHECK(cond, msg) do { if (!(cond)) { printf("  FAIL: %s\n", msg); g_fail++; } } while (0)

int main(void)
{
    const struct nvrm_proxy_console good = { 0x7c000000ull, 2560, 1600, 2560 * 4, 32 };
    const struct nvrm_proxy_range bars[2] = { { 0x90000000ull, 0x1000000ull }, { 0x4000000000ull, 0x400000000ull } };

    CHECK(nvrm_proxy_decide(1, &good, 1, bars, 2) == NVRM_PROXY_OK, "a 32-bit console outside the NVIDIA BARs with an Intel display device is a proxy target");
    CHECK(nvrm_proxy_decide(0, &good, 1, bars, 2) == NVRM_PROXY_OFF, "without nvfbigpu=1 nothing changes");
    CHECK(nvrm_proxy_decide(1, 0, 1, bars, 2) == NVRM_PROXY_NO_CONSOLE, "no console record means no proxy");

    struct nvrm_proxy_console c = good; c.base = 0;
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_NO_CONSOLE, "a zero base address means no proxy");
    c = good; c.depth = 16;
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_BAD_FORMAT, "a 16-bit console is not copied into");
    c = good; c.pitch = 2560 * 4 - 4;
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_BAD_FORMAT, "a pitch smaller than a row is refused");
    c = good; c.width = 70000; c.pitch = 70000 * 4;
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_TOO_LARGE, "a corrupt width is refused before any mapping");
    CHECK(nvrm_proxy_decide(1, &good, 0, bars, 2) == NVRM_PROXY_NO_IGPU, "no Intel display device means there is no panel to stand in for");

    c = good; c.base = 0x90000000ull;
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_OWN_CONSOLE, "a console inside the NVIDIA GPU's BAR is its own console");
    c = good; c.base = 0x8fffe000ull; c.pitch = 0x1000; c.width = 1000; c.height = 2;   // ends at the BAR base
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_OK, "a console that ends exactly below the BAR is still a proxy target");
    c.height = 8;   // 0x8fffe000 + 0x8000 crosses 0x90000000
    CHECK(nvrm_proxy_decide(1, &c, 1, bars, 2) == NVRM_PROXY_OWN_CONSOLE, "a console that runs into the BAR is refused");

    printf(g_fail ? "FAILED %d\n" : "ok\n", g_fail);
    return g_fail != 0;
}
