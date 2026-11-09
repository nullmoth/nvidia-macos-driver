/* test_y422_convert.c - the GPU conversion's pixel rule against the CPU decoder that matched Apple's sampler.
   cc -I plugin tools/test_y422_convert.c -o /tmp/test_y422_convert && /tmp/test_y422_convert
   The shader includes the same nvmtl_y422_pixel.inc and is validated with spirv-val. */
#include <stdio.h>
#include <string.h>
#include "nvmtl_yuv422.h"
#include "nvmtl_y422_convert.h"
typedef uint32_t uint;
#include "nvmtl_y422_pixel.inc"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
/* the shader's main(), one invocation per pixel, over a width x height image with a pitch in words */
static void run_kernel(uint fmt, const uint32_t *src, uint w, uint h, uint pitch, uint32_t *dst)
{
    for (uint i = 0; i < w * h; i++) {
        uint y = i / w, x = i % w, k = x / 2u, pairs = w / 2u, odd = x & 1u;
        uint kn = odd ? (k + 1u < pairs ? k + 1u : k) : (k > 0u ? k - 1u : k);
        dst[i] = y422_pixel(fmt, src[y * pitch + k], src[y * pitch + kn], odd);
    }
}
int main(void)
{
    enum { W = 12, H = 3, PITCH = 8 };   /* 6 pairs per row, rows padded to 8 words */
    uint32_t src[H * PITCH]; uint8_t *sb = (uint8_t *)src;
    for (unsigned i = 0; i < sizeof src; i++) sb[i] = (uint8_t)(i * 37u + 11u);
    for (int f = 0; f < 2; f++) {
        const uint fmt = f ? NVMTL_PF_BGRG422 : NVMTL_PF_GBGR422;
        uint32_t out[W * H]; run_kernel(fmt, src, W, H, PITCH, out);
        int same = 1;
        for (int y = 0; y < H; y++) {
            uint8_t cpu[W * 4]; nvmtl_y422_decode_row(fmt, sb + y * PITCH * 4, W, cpu);
            if (memcmp(cpu, (uint8_t *)out + y * W * 4, sizeof cpu)) same = 0;
        }
        arm(f ? "BGRG422: the shader's rule equals the measured CPU decoder on every pixel of a padded 12x3 image"
              : "GBGR422: the shader's rule equals the measured CPU decoder on every pixel of a padded 12x3 image", same);
    }
    arm("one invocation per pixel: 12x3 needs 1 group, 1920x1080 needs 32400", nvmtl_y422_convert_groups(12, 3) == 1 && nvmtl_y422_convert_groups(1920, 1080) == 32400);
    arm("push constants are 32 bytes", sizeof(nvmtl_y422_convert_args) == 32);
    arm("the SPIR-V has its magic", nvmtl_y422_convert_spv[0] == 0x07230203u);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
