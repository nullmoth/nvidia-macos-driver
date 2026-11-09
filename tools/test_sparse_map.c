/* test_sparse_map.c - sparse mapping arithmetic (plugin/nvmtl_sparse_map.h).
   cc -I plugin tools/test_sparse_map.c -o /tmp/test_sparse_map && /tmp/test_sparse_map */
#include <stdio.h>
#include "nvmtl_sparse_map.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    const uint32_t g[3] = { 128, 128, 1 };    /* a 32-bit 2D block */
    arm("mip dimensions halve and stop at 1", nvmtl_sparse_dim(1000, 0) == 1000 && nvmtl_sparse_dim(1000, 3) == 125 && nvmtl_sparse_dim(1000, 20) == 1 && nvmtl_sparse_dim(5, 40) == 1);
    arm("a 1024x1024 texture's tail starts at the first level smaller than a block: level 4 (64 < 128)", nvmtl_sparse_first_tail(1024, 1024, 1, 11, g) == 4);
    arm("a 512x64 texture is in its tail from level 0 (height 64 < 128)", nvmtl_sparse_first_tail(512, 64, 1, 7, g) == 0);
    arm("a level count below the tail leaves no tail", nvmtl_sparse_first_tail(1024, 1024, 1, 3, g) == 3);
    const uint32_t o0[3] = { 0, 0, 0 }, full[3] = { 1024, 1024, 1 }, one[3] = { 128, 128, 1 }, o1[3] = { 128, 256, 0 };
    arm("an aligned region is valid", nvmtl_sparse_region_refusal(1024, 1024, 1, 0, 4, g, o1, one) == NULL);
    const uint32_t odd[3] = { 64, 0, 0 };
    arm("an origin off the tile grid is refused", nvmtl_sparse_region_refusal(1024, 1024, 1, 0, 4, g, odd, one) != NULL);
    const uint32_t sz100[3] = { 100, 128, 1 };
    arm("a size that is not a tile multiple and stops short of the edge is refused", nvmtl_sparse_region_refusal(1024, 1024, 1, 0, 4, g, o0, sz100) != NULL);
    const uint32_t edge_o[3] = { 896, 0, 0 }, edge_s[3] = { 100, 128, 1 };
    arm("a ragged size that reaches the edge is fine (1000 wide: 896 + 104)", nvmtl_sparse_region_refusal(1000, 1024, 1, 0, 4, g, edge_o, (uint32_t[3]){ 104, 128, 1 }) == NULL
        && nvmtl_sparse_region_refusal(1000, 1024, 1, 0, 4, g, edge_o, edge_s) != NULL);
    arm("a region in the mip tail is refused", nvmtl_sparse_region_refusal(1024, 1024, 1, 4, 4, g, o0, (uint32_t[3]){ 64, 64, 1 }) != NULL);
    arm("a region outside the level is refused", nvmtl_sparse_region_refusal(1024, 1024, 1, 1, 4, g, (uint32_t[3]){ 512, 0, 0 }, one) != NULL);
    nvmtl_sparse_tile t[64];
    arm("the whole 1024x1024 level 0 is 8 x 8 = 64 tiles", nvmtl_sparse_tiles(1024, 1024, 1, 0, g, o0, full, t, 64) == 64);
    arm("a cap that is too small is reported, not truncated", nvmtl_sparse_tiles(1024, 1024, 1, 0, g, o0, full, t, 63) == -1);
    int n = nvmtl_sparse_tiles(1000, 1024, 1, 0, g, edge_o, (uint32_t[3]){ 104, 256, 1 }, t, 64);
    arm("an edge block is clipped to the level: 104 wide at x = 896", n == 2 && t[0].x == 896 && t[0].w == 104 && t[0].h == 128 && t[1].y == 128 && t[1].tx == 7 && t[1].ty == 1);
    n = nvmtl_sparse_tiles(1024, 1024, 1, 2, g, o0, (uint32_t[3]){ 256, 256, 1 }, t, 64);
    arm("level 2 is 256x256: 2 x 2 tiles", n == 4 && t[3].x == 128 && t[3].y == 128);
    uint64_t store[2]; nvmtl_page_pool p; nvmtl_page_pool_init(&p, store, 70);
    int a = nvmtl_page_alloc(&p), b = nvmtl_page_alloc(&p);
    arm("pages come out in order and are distinct", a == 0 && b == 1 && p.used == 2);
    arm("a freed page is reusable and a double free is refused", nvmtl_page_free(&p, 0) == 1 && nvmtl_page_free(&p, 0) == 0 && p.used == 1);
    int last = 0; for (int i = 0; i < 69; i++) last = nvmtl_page_alloc(&p);
    arm("the pool hands out all 70 pages and then says no", last >= 0 && p.used == 70 && nvmtl_page_alloc(&p) == -1);
    arm("a page beyond the pool cannot be freed", nvmtl_page_free(&p, 70) == 0 && nvmtl_page_free(&p, 1000) == 0);
    { uint64_t st2[1]; nvmtl_page_pool q; nvmtl_page_pool_init(&q, st2, 10);
      nvmtl_page_alloc(&q); nvmtl_page_alloc(&q); nvmtl_page_alloc(&q); nvmtl_page_free(&q, 1);   /* used: 0 and 2 */
      arm("a run of 2 skips the single free page at 1 and starts at 3", nvmtl_page_alloc_run(&q, 2) == 3 && q.used == 4);
      arm("a run of 1 can take the hole at page 1", nvmtl_page_alloc_run(&q, 1) == 1);
      arm("a run longer than any free stretch is refused and takes nothing", nvmtl_page_alloc_run(&q, 7) == -1 && q.used == 5);
      arm("a run of 5 fits the rest (pages 5..9)", nvmtl_page_alloc_run(&q, 5) == 5 && q.used == 10); }
    arm("the tier is 1 with the strict zero read and 0 without it (not sparse)", nvmtl_sparse_tier(1) == 1 && nvmtl_sparse_tier(0) == 0);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
