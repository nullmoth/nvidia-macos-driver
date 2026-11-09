/* The zero-copy decision (kexts/NVRM/accel/nm_hybrid_p2p.h): off by default, listed chipsets only unless forced, and the refusals that
 * no switch overrides (a path that does not exist, a BAR too small, a translated DMA).
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_hybrid_p2p.c -o /tmp/t && /tmp/t     -> 0 failed */
#include <stdio.h>
#include "nm_hybrid_p2p.h"
static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
int main(void)
{
    struct nmp_topo t = { 0x8086, 0x9a14, 1, 1, 256ull << 20, 0 };
    const uint64_t frame = 3456ull * 2160 * 4;
    ARM("the boot-arg: absent and 0 are off, 1 is listed-only, 2 is forced, anything else is off", nmp_parse(0, 1) == NMP_OFF && nmp_parse(1, 0) == NMP_OFF && nmp_parse(1, 1) == NMP_LISTED && nmp_parse(1, 2) == NMP_FORCE && nmp_parse(1, 7) == NMP_OFF);
    ARM("off by default", nmp_decide(NMP_OFF, &t, frame) == NMP_NO_SWITCH && !nmp_allowed(NMP_NO_SWITCH));
    ARM("an unlisted host bridge is refused under nmhybridp2p=1", nmp_decide(NMP_LISTED, &t, frame) == NMP_NOT_LISTED && !nmp_allowed(NMP_NOT_LISTED));
    ARM("the built-in list has no entries yet, so no machine is allowed without being forced", !nmp_is_listed(0x8086, 0x9a14) && !nmp_is_listed(0, 0));
    ARM("nmhybridp2p=2 allows an unlisted host bridge", nmp_decide(NMP_FORCE, &t, frame) == NMP_OK_FORCED && nmp_allowed(NMP_OK_FORCED));
    { struct nmp_topo u = t; u.dgpu_reachable = 0; ARM("no path is refused even when forced", nmp_decide(NMP_FORCE, &u, frame) == NMP_NO_PATH); }
    { struct nmp_topo u = t; u.bar1_bytes = 16ull << 20; ARM("a BAR1 smaller than the frame is refused even when forced", nmp_decide(NMP_FORCE, &u, frame) == NMP_NO_BAR); }
    { struct nmp_topo u = t; u.bar1_64bit_prefetch = 0; ARM("a BAR1 that is not 64-bit prefetchable is refused even when forced", nmp_decide(NMP_FORCE, &u, frame) == NMP_NO_BAR); }
    { struct nmp_topo u = t; u.igpu_dma_translated = 1; ARM("a translated DMA is refused even when forced", nmp_decide(NMP_FORCE, &u, frame) == NMP_NO_IOMMU); }
    { uint64_t pa[8]; ARM("a BAR1 frame becomes consecutive pages", nmp_pages(0x4000000000ull, 1 << 20, 5 * 4096 - 7, pa, 5) && pa[0] == 0x4000000000ull && pa[4] == 0x4000004000ull);
      ARM("an unaligned window address, a frame longer than the allocation, or a wrong page count is refused", !nmp_pages(0x4000000800ull, 1 << 20, 4096, pa, 1) && !nmp_pages(0x4000000000ull, 4096, 8192, pa, 2) && !nmp_pages(0x4000000000ull, 1 << 20, 8192, pa, 3));
      ARM("a page beyond bit 45 is refused (the table entry cannot address it)", !nmp_pages(1ull << 46, 1 << 20, 4096, pa, 1) && nmp_pages((1ull << 46) - 4096, 1 << 20, 4096, pa, 1)); }
    ARM("every verdict has a distinct reason line", nmp_name(NMP_OK_LISTED) != nmp_name(NMP_NO_BAR) && nmp_name(NMP_NOT_LISTED)[0] && nmp_name(NMP_NO_PATH)[0]);
    printf("%d failed\n", fails);
    return fails != 0;
}
