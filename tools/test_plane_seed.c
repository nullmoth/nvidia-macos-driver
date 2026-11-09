/* test_plane_seed.c - own-write tracking for the planes of one IOSurface, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_plane_seed.c -o /tmp/test_plane_seed && /tmp/test_plane_seed */
#include "nvmtl_plane_seed.h"
#include <stdio.h>
#include <string.h>
static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)
#define S 1000000000ull
int main(void)
{
    static nvmtl_ps p; memset(&p, 0, sizeof p);
    nvmtl_ps_record(&p, 7, 11, 0, 5, 5, 1 * S);   /* our surface-out of plane 0 moved the seed 4 -> 5 */
    ARM("plane 1's image stays current after our own write to plane 0", nvmtl_ps_covers(&p, 7, 11, 1, 4, 5, 2 * S, 2 * S));
    ARM("plane 0's image does not: those are new pixels for it", !nvmtl_ps_covers(&p, 7, 11, 0, 4, 5, 2 * S, 2 * S));
    ARM("a seed that moved further than our records is somebody else's write", !nvmtl_ps_covers(&p, 7, 11, 1, 4, 6, 2 * S, 2 * S));
    nvmtl_ps_record(&p, 7, 11, 1, 6, 6, 3 * S);
    ARM("a chain of our writes to other planes is covered", nvmtl_ps_covers(&p, 7, 11, 2, 4, 6, 4 * S, 5 * S));
    ARM("a chain through this plane's own write is not", !nvmtl_ps_covers(&p, 7, 11, 1, 4, 6, 4 * S, 5 * S));
    ARM("an old record is not trusted", !nvmtl_ps_covers(&p, 7, 11, 1, 4, 5, 10 * S, 2 * S));
    ARM("a reused surface id (another cookie) never matches", !nvmtl_ps_covers(&p, 7, 99, 1, 4, 5, 2 * S, 2 * S) && !nvmtl_ps_covers(&p, 8, 11, 1, 4, 5, 2 * S, 2 * S));
    ARM("no cookie, no cover", !nvmtl_ps_covers(&p, 7, 0, 1, 4, 5, 2 * S, 2 * S));
    ARM("a lock whose unlock reported another seed (a foreign write between) records nothing",
        nvmtl_ps_record(&p, 9, 11, 0, 5, 6, S) == 0 && !nvmtl_ps_covers(&p, 9, 11, 1, 4, 5, 2 * S, 2 * S));
    ARM("an unchanged seed needs no record", nvmtl_ps_covers(&p, 7, 11, 1, 5, 5, 2 * S, 2 * S));
    return fails ? 1 : 0;
}
