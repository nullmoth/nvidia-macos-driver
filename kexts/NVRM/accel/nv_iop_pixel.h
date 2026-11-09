/*
 * Pixel conversion for the display pipe's copy path. Plain C, no kernel headers, so tools/test_iop_l10r.c runs the
 * same code on the host.
 *
 * macOS 26 composites the login window and the lock screen into 'l10r': ARGB2101010 in one little-endian 32-bit word
 * (alpha 31-30, red 29-20, green 19-10, blue 9-0). The scanout is X8R8G8B8, so each channel keeps its top eight bits
 * and the unused byte is set to 0xff. Refusing those frames left the panel on its last BGRA frame: a black login
 * window with a live cursor until the user typed the password blind.
 */
#ifndef NV_IOP_PIXEL_H
#define NV_IOP_PIXEL_H

#define NV_IOP_FMT_BGRA 0x42475241u   /* 'BGRA' */
#define NV_IOP_FMT_L10R 0x6c313072u   /* 'l10r' */

static inline unsigned nv_iop_l10r_to_x8r8g8b8(unsigned v)
{
    return 0xff000000u | ((v >> 6) & 0x00ff0000u) | ((v >> 4) & 0x0000ff00u) | ((v >> 2) & 0x000000ffu);
}


/*
 * 30-bit scanout. With a 10-bit mode set, the framebuffer's scanout is X2B10G10R10 (NVKMS naming, most significant
 * first: alpha 31-30, blue 29-20, green 19-10, red 9-0) and says so in the scanout reply (NVRM_VRAM_SCANOUT_30BIT). NVKMS
 * has no ARGB2101010 layout, so 'l10r' sources swap red and blue; BGRA sources widen each 8-bit channel to 10 bits by bit
 * replication (255 -> 1023); half-float 'RGhA' sources are clamped to 0..1 and scaled. Plain C, unsigned math only.
 */
#define NV_IOP_FMT_RGHA 0x52476841u   /* 'RGhA': four IEEE half floats, red first */

static inline unsigned nv_iop_l10r_to_x2b10g10r10(unsigned v)
{
    return 0xC0000000u | ((v & 0x3ffu) << 20) | (v & 0x000ffc00u) | ((v >> 20) & 0x3ffu);
}
static inline unsigned nv_iop_c8_to_c10(unsigned c) { return (c << 2) | (c >> 6); }
static inline unsigned nv_iop_bgra_to_x2b10g10r10(unsigned v)
{
    return 0xC0000000u | (nv_iop_c8_to_c10(v & 0xffu) << 20) | (nv_iop_c8_to_c10((v >> 8) & 0xffu) << 10) |
           nv_iop_c8_to_c10((v >> 16) & 0xffu);
}
/* half float -> 0..1023, rounded; negative, denormal, NaN -> 0; 1.0 and above, infinity -> 1023 */
static inline unsigned nv_iop_half_to_unorm10(unsigned short h)
{
    const unsigned e = (h >> 10) & 31u, m = h & 1023u;
    if (h & 0x8000u) return 0;
    if (e == 31u) return m ? 0 : 1023u;
    if (e == 0) return 0;
    if (e >= 15u) return 1023u;
    {
        const unsigned long long num = (unsigned long long)(1024u + m) * 1023u;
        const unsigned sh = 25u - e;
        return (unsigned)((num + (1ull << (sh - 1))) >> sh);
    }
}
static inline unsigned nv_iop_unorm10_to_8(unsigned c) { return (c * 255u + 511u) / 1023u; }

/*
 * One row of pixels from a source format into the scanout: `dst30` selects X2B10G10R10 (1) or X8R8G8B8 (0). `px` is the
 * pixel count. BGRA = 4 bytes per source pixel, l10r = 4, RGhA = 8. Returns 0, or -1 for a format it does not know.
 */
static inline int nv_iop_convert_row(volatile unsigned *dp, const volatile unsigned *sp, unsigned px, unsigned srcFmt, int dst30)
{
    unsigned i;
    if (srcFmt == NV_IOP_FMT_BGRA) {
        if (dst30) for (i = 0; i < px; i++) dp[i] = nv_iop_bgra_to_x2b10g10r10(sp[i]);
        else       for (i = 0; i < px; i++) dp[i] = sp[i];
    } else if (srcFmt == NV_IOP_FMT_L10R) {
        if (dst30) for (i = 0; i < px; i++) dp[i] = nv_iop_l10r_to_x2b10g10r10(sp[i]);
        else       for (i = 0; i < px; i++) dp[i] = nv_iop_l10r_to_x8r8g8b8(sp[i]);
    } else if (srcFmt == NV_IOP_FMT_RGHA) {
        const volatile unsigned short *h = (const volatile unsigned short *)sp;
        for (i = 0; i < px; i++) {
            const unsigned r = nv_iop_half_to_unorm10(h[4 * i]), g = nv_iop_half_to_unorm10(h[4 * i + 1]), b = nv_iop_half_to_unorm10(h[4 * i + 2]);
            dp[i] = dst30 ? (0xC0000000u | (b << 20) | (g << 10) | r)
                          : (0xff000000u | (nv_iop_unorm10_to_8(r) << 16) | (nv_iop_unorm10_to_8(g) << 8) | nv_iop_unorm10_to_8(b));
        }
    } else return -1;
    return 0;
}
#define NV_IOP_BYTES_PER_PIXEL(fmt) ((fmt) == NV_IOP_FMT_RGHA ? 8u : 4u)

#endif
