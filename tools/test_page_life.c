/* test_page_life.c - the address-page lifetime, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_page_life.c -o /tmp/test_page_life && /tmp/test_page_life */
#include "nvmtl_page_life.h"
#include <string.h>

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)

int main(void)
{
    nvmtl_pl_state s; nvmtl_pl_node a, b, c, *due; char line[256];
    memset(&s, 0, sizeof s); memset(&a, 0, sizeof a); memset(&b, 0, sizeof b); memset(&c, 0, sizeof c);

    /* one submit, one page: the common case releases at once and nothing is held */
    nvmtl_pl_made(&s); nvmtl_pl_stamp(&a, 5);
    ARM("a page referenced by one submit is released as soon as that submit has completed", nvmtl_pl_release(&s, &a, 5, 1) == 1);
    ARM("  and the census counts it retired, none held", s.retired == 1 && s.held == 0 && s.live == 0 && s.ncool == 0);

    /* the negative control: two submits reference one page; the first completing must NOT release it */
    memset(&b, 0, sizeof b); nvmtl_pl_made(&s);
    nvmtl_pl_stamp(&b, 7); nvmtl_pl_stamp(&b, 9); nvmtl_pl_stamp(&b, 8);   /* stamps arrive out of order; the highest wins */
    ARM("a page referenced by submits 7 and 9 is NOT released when 7 has completed", nvmtl_pl_release(&s, &b, 7, 1) == 0);
    ARM("  it is held on the cooling list, counted as held past its own cb", s.ncool == 1 && s.held == 1 && s.live == 1);
    ARM("  reaping at completed sequence 8 returns nothing", nvmtl_pl_reap(&s, 8, &due) == 0 && due == NULL && s.ncool == 1);
    ARM("  reaping at completed sequence 9 returns exactly that page, with its reuse verdict intact",
        nvmtl_pl_reap(&s, 9, &due) == 1 && due == &b && b.reuse == 1 && due->cool == NULL && s.ncool == 0 && s.reaped == 1 && s.live == 0);

    /* a stale stamp from a previous owner never holds a page: the stamp is monotone and already completed */
    memset(&c, 0, sizeof c); nvmtl_pl_made(&s); nvmtl_pl_stamp(&c, 3);
    ARM("a page whose only stamp is from an already completed submit releases at once under a later completed sequence", nvmtl_pl_release(&s, &c, 20, 0) == 1);
    ARM("  a stamp lower than the page's highest never lowers it", (nvmtl_pl_stamp(&c, 1), c.last_seq == 3));

    /* several cooling pages with different stamps reap in sequence order, never early */
    nvmtl_pl_node p[4]; memset(p, 0, sizeof p);
    for (int i = 0; i < 4; i++) { nvmtl_pl_made(&s); nvmtl_pl_stamp(&p[i], 30 + (uint64_t)i * 2); ARM("  a page stamped past the completed sequence is held", nvmtl_pl_release(&s, &p[i], 29, i & 1) == 0); }
    ARM("four pages cooling", s.ncool == 4);
    ARM("reaping at 33 frees the two stamped 30 and 32 and keeps 34 and 36", nvmtl_pl_reap(&s, 33, &due) == 2 && s.ncool == 2);
    { int saw30 = 0, saw32 = 0, other = 0; for (nvmtl_pl_node *e = due; e; e = e->cool) { if (e == &p[0]) saw30++; else if (e == &p[1]) saw32++; else other++; }
      ARM("  and those two are exactly the ones returned", saw30 == 1 && saw32 == 1 && other == 0); }
    ARM("  each keeps the reuse verdict it was released with", p[0].reuse == 0 && p[1].reuse == 1);
    ARM("reaping at 36 frees the rest", nvmtl_pl_reap(&s, 36, &due) == 2 && s.ncool == 0 && s.live == 0);

    nvmtl_pl_census(&s, line, sizeof line);
    ARM("the census line names live, retired and held", strstr(line, "address pages: 0 live, 7 retired, 5 held past their own cb") != NULL);
    printf("%s\n", line);
    printf("test_page_life: %d failed\n", fails);
    return fails ? 1 : 0;
}
