/* kbl_core.h - the parts of the keyboard backlight driver that do not touch the kernel, so a host test can run them.
 *
 * ASUS laptops expose the backlight through the ACPI WMI device that carries _HID PNP0C14 and _UID "ATK". Its method
 * WMNB(instance, method id, buffer) takes an 8-byte buffer { device id, argument }. The method ids are the ASCII of the
 * method name read as a little-endian number. This was read out of ASUS DSDTs sent in by users: 33 of the 648 tables
 * examined handle device id 0x00050021, all with OEM id "_ASUS_".
 *
 *   DSTS (0x53545344)  device id 0x00050021 -> state: bit 16 present, low 7 bits = level 0..3
 *   DEVS (0x53564544)  device id 0x00050021, argument 0x80 | level to switch on at a level, 0 to switch off
 */
#ifndef NM_KBL_CORE_H
#define NM_KBL_CORE_H

#include <stdint.h>

#define KBL_ASUS_DEVID        0x00050021u
#define KBL_ASUS_METHOD_DSTS  0x53545344u
#define KBL_ASUS_METHOD_DEVS  0x53564544u
#define KBL_ASUS_UNSUPPORTED  0xFFFFFFFEu
#define KBL_ASUS_PRESENT_BIT  0x00010000u
#define KBL_ASUS_ON_BIT       0x80u
#define KBL_ASUS_MAX_LEVEL    3

static inline int kbl_asus_present(uint32_t dsts)
{
    return dsts != KBL_ASUS_UNSUPPORTED && dsts != 0xFFFFFFFFu && (dsts & KBL_ASUS_PRESENT_BIT) != 0;
}

/* The level the firmware reports, 0 (off) to 3. */
static inline int kbl_asus_level(uint32_t dsts)
{
    int l = (int)(dsts & 0x7F);
    return l > KBL_ASUS_MAX_LEVEL ? KBL_ASUS_MAX_LEVEL : l;
}

/* The argument that sets a level: 0 turns the light off, anything else is 0x80 | level. */
static inline uint32_t kbl_asus_set_argument(int level)
{
    if (level <= 0) return 0;
    if (level > KBL_ASUS_MAX_LEVEL) level = KBL_ASUS_MAX_LEVEL;
    return KBL_ASUS_ON_BIT | (uint32_t)level;
}

/* macOS keeps the keyboard brightness in the SMC key LKSB as a 2-byte value. Read out of Apple's own AppleLMUController on
 * an Intel Mac Pro (setLEDBrightness): the brightness is 12 bits; byte 0 is bits 11..4 and the high nibble of byte 1 is
 * bits 3..0, that is  word = ((v & 0xF) << 12) | ((v >> 4) & 0xFF)  stored little-endian. These functions convert between
 * that pair of bytes and the firmware's levels, rounding to the nearest level. */
static inline int kbl_smc_decode(uint8_t b0, uint8_t b1)
{
    return ((int)b0 << 4) | (b1 >> 4);
}

static inline void kbl_smc_encode(int v12, uint8_t *b0, uint8_t *b1)
{
    if (v12 < 0) v12 = 0;
    if (v12 > 0xFFF) v12 = 0xFFF;
    *b0 = (uint8_t)(v12 >> 4);
    *b1 = (uint8_t)((v12 & 0xF) << 4);
}

static inline int kbl_smc_to_level(int v12, int max_level)
{
    return (v12 * max_level + 2047) / 4095;
}

static inline int kbl_level_to_smc(int level, int max_level)
{
    if (level <= 0) return 0;
    if (level >= max_level) return 0xFFF;
    return (level * 4095 + max_level / 2) / max_level;
}

#endif
