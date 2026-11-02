/* test_live.c - the live GPU-range registry, proven on the host without a GPU.
   Build and run anywhere:  cc -I plugin tools/test_live.c -o /tmp/test_live && /tmp/test_live */
#include "nvmtl_live.h"

static int fails;
#define ARM(name, cond) do { const int ok = !!(cond); printf("%s  %s\n", ok ? "ok  " : "FAIL", name); if (!ok) fails++; } while (0)

int main(void)
{
    nvmtl_lv_state s; memset(&s, 0, sizeof s); char line[256];
    const uint32_t a = nvmtl_lv_add(&s, 0x36f100000ull, 0x40000, NVMTL_LV_BUFFER, 7000, NULL);
    const uint32_t b = nvmtl_lv_add(&s, 0x4b36c0000ull, 0x60000, NVMTL_LV_PAGE, 7001, NULL);
    const uint32_t z = nvmtl_lv_add(&s, 0, 0x1000, NVMTL_LV_BUFFER, 7002, NULL);
    ARM("two ranges registered, a range without an address refused", a == 1 && b == 2 && z == 0 && s.live == 2 && s.refused == 1);
    const nvmtl_lv_entry *e = nvmtl_lv_find(&s, 0x36f122000ull);
    ARM("an address inside a live buffer resolves to that buffer", e && e->lo == 0x36f100000ull && e->kind == NVMTL_LV_BUFFER && e->seq == 7000);
    ARM("the end of a range is exclusive, the start inclusive", nvmtl_lv_find(&s, 0x36f140000ull) == NULL && nvmtl_lv_find(&s, 0x36f100000ull) == e);
    ARM("an address in no range resolves to nothing (negative control)", nvmtl_lv_find(&s, 0x2ef8a2000ull) == NULL);
    nvmtl_lv_describe(&s, 0x4b36c0100ull, line, sizeof line);
    ARM("a page describes itself", strcmp(line, "live address page 0x4b36c0000..0x4b3720000 (393216 bytes, made at seq 7001)") == 0);
    nvmtl_lv_relabel(&s, a, NVMTL_LV_PARKED);
    ARM("a relabel changes the kind and nothing else", nvmtl_lv_find(&s, 0x36f122000ull)->kind == NVMTL_LV_PARKED && s.live == 2);
    nvmtl_lv_remove(&s, a, 7777);
    ARM("after removal the address resolves to nothing", nvmtl_lv_find(&s, 0x36f122000ull) == NULL && s.live == 1 && s.removed == 1);
    nvmtl_lv_remove(&s, a, 7778);
    ARM("removing twice is harmless", s.live == 1 && s.removed == 1 && s.e[a - 1].died == 7777);
    const nvmtl_lv_entry *ev = nvmtl_lv_find_ever(&s, 0x36f122000ull);
    ARM("the history still says what the address WAS and when it died", ev && ev->kind == NVMTL_LV_PARKED && ev->seq == 7000 && ev->died == 7777);
    ARM("an address never mapped has no history either (negative control)", nvmtl_lv_find_ever(&s, 0x2ef8a2000ull) == NULL);
    const uint32_t c = nvmtl_lv_add(&s, 0x100000ull, 0x1000, NVMTL_LV_POOLED, 7003, NULL);
    ARM("a dead range keeps its slot: history is never overwritten", c == 3 && s.n == 3 && s.e[a - 1].died == 7777);
    for (uint32_t k = 0; k < 5000; k++) if (!nvmtl_lv_add(&s, 0x10000000ull + (uint64_t)k * 0x10000, 0x8000, NVMTL_LV_PLACED, 8000 + k, NULL)) fails++;
    ARM("the table grows past its first capacity and every entry stays resolvable", s.live == 5002 && s.cap >= 5002
        && nvmtl_lv_find(&s, 0x10000000ull + 4999ull * 0x10000 + 0x7fff)->seq == 8000 + 4999 && nvmtl_lv_find(&s, 0x4b36c0100ull)->kind == NVMTL_LV_PAGE);
    static const uint64_t host[2] = { 0x36f100000ull, 7 }; const uint32_t m = nvmtl_lv_add(&s, 0x50000000ull, 0x1000, NVMTL_LV_BUFFER, 9000, host);
    ARM("a host-visible range keeps its host view for reading pointers at device loss", nvmtl_lv_find(&s, 0x50000800ull)->map == host && ((const uint64_t *)nvmtl_lv_find(&s, 0x50000800ull)->map)[0] == 0x36f100000ull);
    nvmtl_lv_remove(&s, m, 9001);
    ARM("  and loses it on removal", s.live == 5002 && nvmtl_lv_find_ever(&s, 0x50000800ull)->map == NULL);
    FILE *f = tmpfile(); const uint32_t n = nvmtl_lv_dump(&s, f);
    ARM("the dump writes one line per live range and counts the live ones", n == 5002);
    rewind(f); { char ln[256]; unsigned was = 0; while (fgets(ln, sizeof ln, f)) if (!strncmp(ln, "was ", 4)) was++; ARM("  dead ranges are written as history lines", was == 2); }
    rewind(f); char first[256] = ""; while (fgets(first, sizeof first, f) && strncmp(first, "live pooled", 11)) {}
    ARM("  in the resolver's line format", strncmp(first, "live pooled buffer 0x100000 0x101000 4096 seq 7003", 50) == 0);
    fclose(f); free(s.e);
    printf("test_live: %d failed\n", fails);
    return fails ? 1 : 0;
}
