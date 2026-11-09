/* fetch_copy_rewrite: reads a SPIR-V module on stdin, writes the framebuffer-fetch-by-copies rewrite to stdout
   (nvmtl_fetch_copy.h). Exit 3 when the module is left alone. Used by test_fetch_copy.py. */
#include <stdio.h>
#include "nvmtl_fetch_copy.h"
int main(void)
{
    static uint32_t buf[1 << 20];
    size_t n = fread(buf, 4, sizeof buf / 4, stdin), outw = 0;
    uint32_t *r = nvmtl_spirv_fetch_to_texture(buf, n, &outw);
    if (!r) return 3;
    fwrite(r, 4, outw, stdout);
    free(r);
    return 0;
}
