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

/* macOS keeps the keyboard brightness in the SMC key LKSB as a 2-byte value. VirtualSMC documents the key but not the
 * meaning of its bytes; the first byte is taken as the brightness, 0 to 255. These two functions map it to and from the
 * firmware's levels and round to the nearest level. */
static inline int kbl_smc_to_level(uint8_t brightness, int max_level)
{
    return ((int)brightness * max_level + 127) / 255;
}

static inline uint8_t kbl_level_to_smc(int level, int max_level)
{
    if (level <= 0) return 0;
    if (level >= max_level) return 255;
    return (uint8_t)((level * 255 + max_level / 2) / max_level);
}

#endif
