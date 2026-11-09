/* The display pipe's 30-bit scanout conversions (kexts/NVRM/accel/nv_iop_pixel.h), on the host.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_iop_10bit.c -o /tmp/t && /tmp/t   -> 0 failed
 * Control: with -DNV_IOP_NO_30BIT every conversion collapses to the 8-bit copy the driver did before, and the arms that
 * need a 10-bit scanout fail. */
#include <stdio.h>
#include "nv_iop_pixel.h"
static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)
static unsigned l10r(unsigned a, unsigned r, unsigned g, unsigned b) { return (a << 30) | (r << 20) | (g << 10) | b; }
static unsigned x2b(unsigned a, unsigned b, unsigned g, unsigned r) { return (a << 30) | (b << 20) | (g << 10) | r; }
/* half float bit patterns */
#define H_0 0x0000u
#define H_HALF 0x3800u   /* 0.5 */
#define H_ONE 0x3c00u
#define H_TWO 0x4000u
#define H_NEG 0xbc00u    /* -1 */
#define H_INF 0x7c00u
#define H_NAN 0x7e00u
#define H_DEN 0x0001u
int main(void)
{
#ifdef NV_IOP_NO_30BIT
    ARM("30-bit scanout available", 0);
#else
    ARM("l10r red/blue swap: pure red", nv_iop_l10r_to_x2b10g10r10(l10r(3, 1023, 0, 0)) == x2b(3, 0, 0, 1023));
    ARM("l10r pure blue lands in the blue field", nv_iop_l10r_to_x2b10g10r10(l10r(3, 0, 0, 1023)) == x2b(3, 1023, 0, 0));
    ARM("l10r keeps all ten bits (no narrowing)", nv_iop_l10r_to_x2b10g10r10(l10r(0, 0x2A5, 0x155, 0x0F3)) == x2b(3, 0x0F3, 0x155, 0x2A5));
    ARM("l10r white stays white, black stays black", nv_iop_l10r_to_x2b10g10r10(l10r(0, 1023, 1023, 1023)) == 0xffffffffu &&
        nv_iop_l10r_to_x2b10g10r10(0) == 0xC0000000u);
    ARM("BGRA 8-bit widens by replication: 255 -> 1023, 0 -> 0, 128 -> 514", nv_iop_c8_to_c10(255) == 1023 && nv_iop_c8_to_c10(0) == 0 && nv_iop_c8_to_c10(128) == 514);
    ARM("BGRA pure red (0x00ff0000) -> red field", nv_iop_bgra_to_x2b10g10r10(0x00ff0000u) == x2b(3, 0, 0, 1023));
    ARM("BGRA pure blue (0x000000ff) -> blue field", nv_iop_bgra_to_x2b10g10r10(0x000000ffu) == x2b(3, 1023, 0, 0));
    ARM("half 1.0 -> 1023, 0.5 -> 512, 0 -> 0", nv_iop_half_to_unorm10(H_ONE) == 1023 && nv_iop_half_to_unorm10(H_HALF) == 512 && nv_iop_half_to_unorm10(H_0) == 0);
    ARM("half above 1, infinity clamp to 1023", nv_iop_half_to_unorm10(H_TWO) == 1023 && nv_iop_half_to_unorm10(H_INF) == 1023);
    ARM("half negative, NaN, denormal -> 0", nv_iop_half_to_unorm10(H_NEG) == 0 && nv_iop_half_to_unorm10(H_NAN) == 0 && nv_iop_half_to_unorm10(H_DEN) == 0);
    {
        unsigned prev = 0, mono = 1, h;                     /* the mapping never goes down as the half grows over [0, 1] */
        for (h = 0; h <= H_ONE; h++) { unsigned v = nv_iop_half_to_unorm10((unsigned short)h); if (v < prev) mono = 0; prev = v; }
        ARM("half -> 10-bit is monotonic over 0..1", mono);
    }
    {
        unsigned short px[8] = { H_ONE, H_HALF, H_0, H_ONE,   H_0, H_ONE, H_TWO, H_ONE };      /* two RGhA pixels */
        unsigned d[2] = { 0, 0 };
        ARM("RGhA row -> 30-bit", nv_iop_convert_row(d, (const unsigned *)px, 2, NV_IOP_FMT_RGHA, 1) == 0 &&
            d[0] == x2b(3, 0, 512, 1023) && d[1] == x2b(3, 1023, 1023, 0));
        ARM("RGhA row -> 8-bit", nv_iop_convert_row(d, (const unsigned *)px, 2, NV_IOP_FMT_RGHA, 0) == 0 && d[0] == 0xffff8000u + 0 * 1 && d[1] == 0xff00ffffu);
    }
    {
        unsigned s[2] = { 0x11223344u, l10r(3, 1023, 512, 0) }, d[2];
        ARM("BGRA row to an 8-bit scanout is unchanged", nv_iop_convert_row(d, s, 2, NV_IOP_FMT_BGRA, 0) == 0 && d[0] == s[0] && d[1] == s[1]);
        ARM("l10r row to an 8-bit scanout still narrows (login window)", nv_iop_convert_row(d, s + 1, 1, NV_IOP_FMT_L10R, 0) == 0 && d[0] == 0xffff8000u);
        ARM("an unknown source format is refused", nv_iop_convert_row(d, s, 1, 0x52474241u, 1) == -1);
        ARM("bytes per pixel: RGhA 8, others 4", NV_IOP_BYTES_PER_PIXEL(NV_IOP_FMT_RGHA) == 8 && NV_IOP_BYTES_PER_PIXEL(NV_IOP_FMT_L10R) == 4);
    }
#endif
    printf("%d failed\n", fails);
    return fails != 0;
}
