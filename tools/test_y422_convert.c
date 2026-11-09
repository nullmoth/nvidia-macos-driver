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
    /* the check the earlier version had, kept to show what it accepted */
    #define OLD_ACCEPTS(w, h, pitch, off, ssz, dsz) (((w) && (h) && !((w) & 1u) && !((off) & 3) && !((pitch) & 3) && (pitch) >= (size_t)(w) * 2u && \
        (off) <= (ssz) && (ssz) - (off) >= (pitch) * ((h) - 1u) + (size_t)(w) * 2u && (dsz) >= (size_t)(w) * (h) * 4u))
    {   /* pitch*(h-1) wraps 64 bits to a small number */
        const uint64_t w = 64, h = 0x2000000000000001ull, pitch = 0x100, off = 0, ssz = 4096, dsz = 4096;
        arm("OLD check accepted a height whose pitch*(h-1) wraps (and a destination its w*h*4 also wrapped past)", OLD_ACCEPTS(w, h, pitch, off, ssz, dsz) != 0);
        arm("new check refuses that request", nvmtl_y422_convert_check(w, h, pitch, off, ssz, dsz) != NULL);
    }
    {   /* pitch/4 does not fit 32 bits: truncation in the old cast */
        const uint64_t w = 2, h = 1, pitch = 0x400000000ull, off = 0, ssz = 0x500000000ull, dsz = 64;
        arm("OLD check accepted a pitch whose word count truncates to 0 in 32 bits", OLD_ACCEPTS(w, h, pitch, off, ssz, dsz) != 0 && (uint32_t)(pitch / 4u) == 0);
        arm("new check refuses a pitch above 32 bits of words", nvmtl_y422_convert_check(w, h, pitch, off, ssz, dsz) != NULL);
    }
    {   /* (h-1)*pitch_words exceeds 32 bits in the shader although the byte extents fit 64 */
        const uint64_t w = 2, h = 0x20000, pitch = 0x40000 * 4ull, off = 0, ssz = 0x4000000000ull, dsz = 0x20000ull * 8;
        arm("OLD check accepted rows whose word offset wraps the shader's 32-bit index", OLD_ACCEPTS(w, h, pitch, off, ssz, dsz) != 0);
        arm("new check refuses a row offset beyond 32 bits", nvmtl_y422_convert_check(w, h, pitch, off, ssz, dsz) != NULL);
    }
    arm("new check refuses w*h beyond 32 bits", nvmtl_y422_convert_check(0x10000, 0x10000, 0x20000, 0, ~0ull >> 1, ~0ull >> 1) != NULL);
    arm("new check refuses a source extent plus offset that overflows", nvmtl_y422_convert_check(2, 2, 8, ~0ull - 3, ~0ull, 64) != NULL);
    arm("new check still accepts an ordinary padded 1920x1080 frame", nvmtl_y422_convert_check(1920, 1080, 4096, 256, 4096ull * 1080 + 256, 1920ull * 1080 * 4) == NULL);
    arm("new check refuses a source buffer one byte short", nvmtl_y422_convert_check(1920, 1080, 4096, 256, 4096ull * 1079 + 3840 + 255, 1920ull * 1080 * 4) != NULL);
    arm("push constants are 32 bytes", sizeof(nvmtl_y422_convert_args) == 32);
    arm("the SPIR-V has its magic", nvmtl_y422_convert_spv[0] == 0x07230203u);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
