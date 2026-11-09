/* NMGopFB decisions (kexts/NMGopFB/nm_gopfb.h) on the host.
 *   cc -O1 -Wall -I../kexts/NMGopFB test_gopfb.c -o /tmp/t && /tmp/t     -> 0 failed
 * The geometry arms are the ones PrimeBridge's attach() makes; the selection arms are the rule for which panel driver
 * takes a GPU. */
#include <stdio.h>
#include <stdint.h>
#include "nm_gopfb.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

/* the 680M machine PrimeBridge runs on: GOP 1920x1080 at the start of BAR0 (256 MiB at 0x490000000) */
static const struct nmgop_bar BARS[3] = { { 0x490000000ull, 0x10000000ull }, { 0xFC000000ull, 0x200000ull }, { 0xFC500000ull, 0x80000ull } };

int main(void)
{
    struct nmgop_geom g;
    /* --- selection --- */
    ARM("a Radeon 680M (1002:1681) takes NMGopFB", nmgop_select(0x1002, 0x1681, 0, 0, 0) == NMGOP_USE_GOPFB);
    ARM("Phoenix 15BF and 1900, Strix Point 150E, Raphael 164E take NMGopFB",
        nmgop_select(0x1002, 0x15BF, 0, 0, 0) == NMGOP_USE_GOPFB && nmgop_select(0x1002, 0x1900, 0, 0, 0) == NMGOP_USE_GOPFB &&
        nmgop_select(0x1002, 0x150E, 0, 0, 0) == NMGOP_USE_GOPFB && nmgop_select(0x1002, 0x164E, 0, 0, 0) == NMGOP_USE_GOPFB);
    ARM("an RX 7600 (7480) and an RX 9070 (7550) take NMGopFB", nmgop_select(0x1002, 0x7480, 0, 0, 0) == NMGOP_USE_GOPFB &&
        nmgop_select(0x1002, 0x7550, 0, 0, 0) == NMGOP_USE_GOPFB);
    ARM("an Intel GPU with display version 12 to 20 stays with NMIntelFB",
        nmgop_select(0x8086, 0xA7A0, 13, 0, 0) == NMGOP_USE_INTELFB && nmgop_select(0x8086, 0x7D55, 14, 0, 0) == NMGOP_USE_INTELFB &&
        nmgop_select(0x8086, 0x9A49, 12, 0, 0) == NMGOP_USE_INTELFB && nmgop_select(0x8086, 0x6420, 20, 0, 0) == NMGOP_USE_INTELFB);
    ARM("a GPU macOS drives itself is declined: Polaris, Vega 10, Navi 21, Raven Ridge, Coffee Lake",
        nmgop_select(0x1002, 0x67EF, 0, 0, 0) == NMGOP_DECLINE && nmgop_select(0x1002, 0x687F, 0, 0, 0) == NMGOP_DECLINE &&
        nmgop_select(0x1002, 0x73BF, 0, 0, 0) == NMGOP_DECLINE && nmgop_select(0x1002, 0x15DD, 0, 0, 0) == NMGOP_DECLINE &&
        nmgop_select(0x8086, 0x3E9B, 0, 0, 0) == NMGOP_DECLINE);
    ARM("an NVIDIA GPU is never taken, even by the override", nmgop_select(0x10DE, 0x2684, 0, 0, 0x10DE2684) == NMGOP_DECLINE);
    ARM("an unknown Intel or AMD ID is declined unless the tester names it", nmgop_select(0x1002, 0x9999, 0, 0, 0) == NMGOP_DECLINE &&
        nmgop_select(0x1002, 0x9999, 0, 0, 0x10029999) == NMGOP_USE_GOPFB && nmgop_select(0x1002, 0x9999, 0, 0, 0x10029998) == NMGOP_DECLINE);
    ARM("the AMD / Intel test driver (NMXGPU) being loaded keeps NMGopFB away from the GPU, even with the override",
        nmgop_select(0x1002, 0x1681, 0, 1, 0) == NMGOP_DECLINE && nmgop_select(0x1002, 0x1681, 0, 1, 0x10021681) == NMGOP_DECLINE);
    ARM("the first and last device ID of each RDNA 3/4 range are in, the ones just outside are out",
        nmgop_amd_has_no_apple_driver(0x7448) && nmgop_amd_has_no_apple_driver(0x745F) && !nmgop_amd_has_no_apple_driver(0x7447) &&
        !nmgop_amd_has_no_apple_driver(0x7460) && nmgop_amd_has_no_apple_driver(0x7470) && nmgop_amd_has_no_apple_driver(0x749F) &&
        !nmgop_amd_has_no_apple_driver(0x74A0) && nmgop_amd_has_no_apple_driver(0x75AF) && !nmgop_amd_has_no_apple_driver(0x75B0));

    /* --- geometry --- */
    ARM("the 680M laptop's framebuffer (1920x1080, pitch 7680, start of BAR0) is accepted in BAR0",
        nmgop_geometry(0x490000000ull, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_OK && g.bar == 0 && g.bar_offset == 0 &&
        g.bytes == 7680ull * 1080 && g.pitch == 7680);
    ARM("a base address with the XNU flag bits set is masked with ~3 and the flags are reported",
        nmgop_geometry(0x490000001ull, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_OK && g.phys == 0x490000000ull && g.flags == 1);
    ARM("a base 4 KiB past the start is NOT rounded to a page (that would shift every pixel)",
        nmgop_geometry(0x490001001ull, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_OK && g.phys == 0x490001000ull && g.bar_offset == 0x1000);
    ARM("a 16 bits per pixel framebuffer is refused", nmgop_geometry(0x490000000ull, 1920, 1080, 3840, 16, BARS, 3, &g) == NMGOP_E_DEPTH);
    ARM("a 24 bits per pixel framebuffer is refused", nmgop_geometry(0x490000000ull, 1920, 1080, 5760, 24, BARS, 3, &g) == NMGOP_E_DEPTH);
    ARM("no base, no width or no height is refused", nmgop_geometry(0, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_E_EMPTY &&
        nmgop_geometry(0x490000000ull, 0, 1080, 7680, 32, BARS, 3, &g) == NMGOP_E_EMPTY && nmgop_geometry(0x490000000ull, 1920, 0, 7680, 32, BARS, 3, &g) == NMGOP_E_EMPTY);
    ARM("a pitch below width * 4, or not a multiple of 4, is refused", nmgop_geometry(0x490000000ull, 1920, 1080, 7676, 32, BARS, 3, &g) == NMGOP_E_PITCH &&
        nmgop_geometry(0x490000000ull, 1920, 1080, 7682, 32, BARS, 3, &g) == NMGOP_E_PITCH);
    ARM("a padded pitch (8192 for 1920 wide) is accepted", nmgop_geometry(0x490000000ull, 1920, 1080, 8192, 32, BARS, 3, &g) == NMGOP_OK && g.pitch == 8192);
    ARM("a framebuffer that starts before the BAR is refused", nmgop_geometry(0x48FFFF000ull, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_E_OUTSIDE);
    ARM("a framebuffer one byte past the end of the BAR is refused, one that ends exactly at the end is accepted",
        nmgop_geometry(0x4A0000000ull - 7680ull * 1080 + 4, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_E_OUTSIDE &&
        nmgop_geometry(0x4A0000000ull - 7680ull * 1080, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_OK);
    ARM("a framebuffer inside another BAR picks that BAR", nmgop_geometry(0xFC000000ull, 64, 64, 256, 32, BARS, 3, &g) == NMGOP_OK && g.bar == 1);
    ARM("a framebuffer in no BAR (ordinary RAM) is refused", nmgop_geometry(0x100000000ull, 1920, 1080, 7680, 32, BARS, 3, &g) == NMGOP_E_OUTSIDE);
    ARM("with no BAR at all it is refused", nmgop_geometry(0x490000000ull, 1920, 1080, 7680, 32, BARS, 0, &g) == NMGOP_E_OUTSIDE);
    ARM("a BAR whose end would wrap past 2^64 does not accept an address beyond it",
        ({ struct nmgop_bar w = { 0xFFFFFFFFFFFFF000ull, 0x1000ull }; nmgop_geometry(0xFFFFFFFFFFFFF000ull, 64, 64, 256, 32, &w, 1, &g) == NMGOP_E_OUTSIDE; }));
    ARM("a row pitch large enough to wrap the byte count is refused as a size, not accepted",
        nmgop_geometry(0x490000000ull, 1920, 1080, 0x100000000ull, 32, BARS, 3, &g) == NMGOP_E_SIZE &&
        nmgop_geometry(0x490000000ull, 20000, 1080, 80000, 32, BARS, 3, &g) == NMGOP_E_SIZE);
    printf("%d failed\n", fails);
    return fails ? 1 : 0;
}
