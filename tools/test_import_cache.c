/* test_import_cache.c - the shared family-VRAM import cache, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_import_cache.c -o /tmp/test_import_cache && /tmp/test_import_cache */
#include "nvmtl_import_cache.h"
#include <stdio.h>
#include <stdlib.h>

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)
typedef struct { void *buf; int tag; } H;   /* stands in for a buffer handle */

int main(void)
{
    nvmtl_ic *c = calloc(1, sizeof *c); H h = { (void *)0x1000, 7 }, out; unsigned char gone[NVMTL_IC_PAYLOAD], ev[NVMTL_IC_PAYLOAD]; uintptr_t evid;
    ARM("a second wrapper of the same surface reuses the import instead of making one",
        nvmtl_ic_insert(c, 42, 4096, 111, (uintptr_t)h.buf, &h, sizeof h, ev, &evid) == 1 &&
        nvmtl_ic_acquire(c, 42, 4096, 111, &out, sizeof out) == 1 && out.buf == h.buf && out.tag == 7);
    ARM("the import stays after the last wrapper goes, so the next frame's wrapper finds it",
        nvmtl_ic_release(c, (uintptr_t)h.buf, 1000, gone) == 1 && nvmtl_ic_release(c, (uintptr_t)h.buf, 2000, gone) == 1 &&
        nvmtl_ic_acquire(c, 42, 4096, 111, &out, sizeof out) == 1);
    nvmtl_ic_release(c, (uintptr_t)h.buf, 3000, gone);
    ARM("a different size of the same surface does not match", nvmtl_ic_acquire(c, 42, 8192, 111, &out, sizeof out) == 0);
    ARM("a reused surface id (another cookie) never gets the old surface's VRAM",
        nvmtl_ic_acquire(c, 42, 4096, 222, &out, sizeof out) == 0 && nvmtl_ic_acquire(c, 42, 4096, 111, &out, sizeof out) == 0);
    unsigned char list[4][NVMTL_IC_PAYLOAD];
    ARM("a stale entry with no wrapper left is handed back for destruction", nvmtl_ic_expire(c, 3001, 2000000000ull, 0, list, 4) == 1);
    H a = { (void *)0x2000, 1 };
    nvmtl_ic_insert(c, 5, 64, 9, (uintptr_t)a.buf, &a, sizeof a, ev, &evid);
    nvmtl_ic_release(c, (uintptr_t)a.buf, 10, gone);
    ARM("an idle entry younger than the idle time is kept", nvmtl_ic_expire(c, 10 + 1999999999ull, 2000000000ull, 0, list, 4) == 0);
    ARM("an idle entry older than the idle time is handed back", nvmtl_ic_expire(c, 10 + 2000000001ull, 2000000000ull, 0, list, 4) == 1);
    nvmtl_ic_insert(c, 5, 64, 9, (uintptr_t)a.buf, &a, sizeof a, ev, &evid);
    ARM("an entry a wrapper still holds is never expired, even when all are asked for", nvmtl_ic_expire(c, 1ull << 60, 1, 1, list, 4) == 0);
    ARM("the cache keeps nothing it does not know: releasing an unknown import says destroy it",
        nvmtl_ic_release(c, 0xdead, 5, gone) == 0);
    /* a stale entry that a wrapper still holds goes with its last wrapper, not before */
    nvmtl_ic_acquire(c, 5, 64, 77, &out, sizeof out);   /* wrong cookie: marks the held entry stale */
    ARM("a stale entry survives while a wrapper holds it, and is destroyed by the last release",
        nvmtl_ic_release(c, (uintptr_t)a.buf, 20, gone) == 2 && ((H *)gone)->buf == a.buf);
    /* a full table gives up the idle entry unused longest; entries in use are never given up */
    free(c); c = calloc(1, sizeof *c);
    for (uintptr_t i = 1; i <= NVMTL_IC_SLOTS; i++) { H x = { (void *)(0x10000 + i), (int)i }; nvmtl_ic_insert(c, (uint32_t)i, 64, i, (uintptr_t)x.buf, &x, sizeof x, ev, &evid); }
    H extra = { (void *)0x99999, 0 };
    ARM("a full table of busy entries caches nothing more and gives nothing up",
        nvmtl_ic_insert(c, 900, 64, 1, (uintptr_t)extra.buf, &extra, sizeof extra, ev, &evid) == 0 && evid == 0);
    nvmtl_ic_release(c, 0x10000 + 7, 500, gone); nvmtl_ic_release(c, 0x10000 + 9, 100, gone);
    ARM("a full table gives up the idle entry unused longest",
        nvmtl_ic_insert(c, 900, 64, 1, (uintptr_t)extra.buf, &extra, sizeof extra, ev, &evid) == 1 && evid == 0x10000 + 9 && ((H *)ev)->tag == 9);
    free(c);
    return fails ? 1 : 0;
}
