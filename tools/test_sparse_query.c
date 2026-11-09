/* test_sparse_query.c - sparse tile sizes come from the device (plugin/nvmtl_sparse_query.h).
   cc -I plugin tools/test_sparse_query.c -o /tmp/test_sparse_query && /tmp/test_sparse_query */
#include <stdio.h>
#include "nvmtl_sparse_query.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    /* Vulkan's standard 2D block shapes by texel size (spec, "Standard Sparse Image Block Shapes"), and the 3D ones */
    arm("8-bit 2D: 256 x 256 is one 64 KiB block", nvmtl_sparse_block_is_standard(256, 256, 1, 1, 1));
    arm("16-bit 2D: 256 x 128", nvmtl_sparse_block_is_standard(256, 128, 1, 2, 1));
    arm("32-bit 2D: 128 x 128", nvmtl_sparse_block_is_standard(128, 128, 1, 4, 1));
    arm("64-bit 2D: 128 x 64", nvmtl_sparse_block_is_standard(128, 64, 1, 8, 1));
    arm("128-bit 2D: 64 x 64", nvmtl_sparse_block_is_standard(64, 64, 1, 16, 1));
    arm("32-bit 3D: 32 x 32 x 16", nvmtl_sparse_block_is_standard(32, 32, 16, 4, 1));
    arm("32-bit, 4 samples: 64 x 64 (and 128 x 64 would be two blocks)", nvmtl_sparse_block_is_standard(64, 64, 1, 4, 4) && !nvmtl_sparse_block_is_standard(128, 64, 1, 4, 4));
    arm("the old stub's 32 x 32 x 1 is not a standard block for any common texel size", !nvmtl_sparse_block_is_standard(32, 32, 1, 1, 1) && !nvmtl_sparse_block_is_standard(32, 32, 1, 4, 1) && !nvmtl_sparse_block_is_standard(32, 32, 1, 16, 1));
    const uint32_t g[3] = { 128, 128, 1 }; uint32_t out[3];
    nvmtl_sparse_tile_size(1, g, out);
    arm("a reported granularity is the tile size", out[0] == 128 && out[1] == 128 && out[2] == 1);
    nvmtl_sparse_tile_size(0, g, out);
    arm("no sparse properties for the format gives (0, 0, 0), not a guess", out[0] == 0 && out[1] == 0 && out[2] == 0);
    arm("the block size is 64 KiB when the device reports sparse properties, 0 when it does not", nvmtl_sparse_tile_bytes(1) == 65536u && nvmtl_sparse_tile_bytes(0) == 0u);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
