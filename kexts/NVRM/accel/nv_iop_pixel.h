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

#endif
