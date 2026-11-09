/* The display pipe's l10r narrowing (kexts/NVRM/accel/nv_iop_pixel.h), on the host.
 *   cc -O1 -Wall -I../kexts/NVRM/accel test_iop_l10r.c -o /tmp/t && /tmp/t          -> 0 failed
 *   cc -O1 -Wall -DNV_IOP_REFUSE_L10R ... (the 2.8 behaviour: frame refused)       -> failures */
#include <stdio.h>
#include "nv_iop_pixel.h"

static int fails;
#define ARM(name, cond) do { if (cond) printf("ok   %s\n", name); else { printf("FAIL %s\n", name); fails++; } } while (0)

/* l10r word from 10-bit channels */
static unsigned l10r(unsigned a2, unsigned r, unsigned g, unsigned b) { return (a2 << 30) | (r << 20) | (g << 10) | b; }

static int copy_frame(unsigned fmt, const unsigned *src, unsigned *dst, unsigned n)
{
#ifdef NV_IOP_REFUSE_L10R
    if (fmt != NV_IOP_FMT_BGRA) return -1;
#else
    if (fmt != NV_IOP_FMT_BGRA && fmt != NV_IOP_FMT_L10R) return -1;
#endif
    for (unsigned i = 0; i < n; i++) dst[i] = fmt == NV_IOP_FMT_L10R ? nv_iop_l10r_to_x8r8g8b8(src[i]) : src[i];
    return 0;
}

int main(void)
{
    ARM("white l10r is white X8R8G8B8", nv_iop_l10r_to_x8r8g8b8(l10r(3, 1023, 1023, 1023)) == 0xffffffffu);
    ARM("black l10r is opaque black", nv_iop_l10r_to_x8r8g8b8(l10r(0, 0, 0, 0)) == 0xff000000u);
    ARM("pure red keeps only red", nv_iop_l10r_to_x8r8g8b8(l10r(3, 1023, 0, 0)) == 0xffff0000u);
    ARM("pure green keeps only green", nv_iop_l10r_to_x8r8g8b8(l10r(3, 0, 1023, 0)) == 0xff00ff00u);
    ARM("pure blue keeps only blue", nv_iop_l10r_to_x8r8g8b8(l10r(3, 0, 0, 1023)) == 0xff0000ffu);
    ARM("each channel keeps its top 8 bits (0x2A4 -> 0xA9)", nv_iop_l10r_to_x8r8g8b8(l10r(0, 0x2A4, 0x2A4, 0x2A4)) == 0xffa9a9a9u);
    ARM("low 2 bits never leak into the next channel", nv_iop_l10r_to_x8r8g8b8(l10r(0, 3, 3, 3)) == 0xff000000u);
    unsigned sb[2] = { 0x11223344u, 0x55667788u }, db[2] = { 0, 0 };
    ARM("a BGRA frame is copied unchanged", copy_frame(NV_IOP_FMT_BGRA, sb, db, 2) == 0 && db[0] == sb[0] && db[1] == sb[1]);
    unsigned sl[1] = { l10r(3, 1023, 512, 0) }, dl[1] = { 0 };
    ARM("an l10r frame (login window) reaches the panel", copy_frame(NV_IOP_FMT_L10R, sl, dl, 1) == 0 && dl[0] == 0xffff8000u);
    ARM("any other format is still refused", copy_frame(0x52474241u, sb, db, 1) == -1);
    printf("%d failed\n", fails);
    return fails != 0;
}
