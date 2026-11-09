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

/* ---- Lenovo IdeaPad, Legion, LOQ: the VPC2004 device ----------------------------------------------------------------
 * The device carries a method KBLC. Its documented encodings (read from the public ideapad-laptop driver, written again
 * here): KBLC(1) asks the keyboard type; 5 means three levels, 7 three levels plus an automatic one. For those, a command
 * is  (type << 4) | 2  to read and  (level << 16) | (type << 4) | 3  to set, with type 2 (three levels) or 3 (automatic),
 * and the level read is bits 15..1 of the answer: 0 to 2 is a level, 3 is "automatic" and is reported as off.
 * Any other answer means a plain on/off light, read with HALS (bit 4 = supported, bit 5 = on) and set with SALS 8 (on)
 * or 9 (off). */
#define KBL_LENOVO_QUERY_TYPE      0x1u
#define KBL_LENOVO_TRI_ANSWER      5u
#define KBL_LENOVO_TRI_AUTO_ANSWER 7u
#define KBL_LENOVO_HALS_SUPPORT    (1u << 4)
#define KBL_LENOVO_HALS_ON         (1u << 5)
#define KBL_LENOVO_SALS_ON         0x8u
#define KBL_LENOVO_SALS_OFF        0x9u

/* 0 plain on/off, 2 three levels, 3 three levels with automatic (the type that goes in the command) */
static inline int kbl_lenovo_type(uint32_t query_answer)
{
    return query_answer == KBL_LENOVO_TRI_ANSWER ? 2 : query_answer == KBL_LENOVO_TRI_AUTO_ANSWER ? 3 : 0;
}

static inline int kbl_lenovo_max_level(int type) { return type ? 2 : 1; }

static inline uint32_t kbl_lenovo_get_command(int type) { return ((uint32_t)type << 4) | 2u; }

static inline uint32_t kbl_lenovo_set_command(int type, int level)
{
    if (level < 0) level = 0;
    if (level > 2) level = 2;
    return ((uint32_t)level << 16) | ((uint32_t)type << 4) | 3u;
}

/* the level an answer to the get command holds, or -1 when it is not one */
static inline int kbl_lenovo_level_from_answer(uint32_t answer)
{
    uint32_t v = (answer >> 1) & 0x7FFFu;
    if (v <= 2) return (int)v;
    if (v == 3) return 0;
    return -1;
}

static inline int kbl_lenovo_plain_supported(uint32_t hals) { return (hals & KBL_LENOVO_HALS_SUPPORT) != 0; }
static inline int kbl_lenovo_plain_level(uint32_t hals) { return (hals & KBL_LENOVO_HALS_ON) ? 1 : 0; }
static inline uint32_t kbl_lenovo_plain_set_argument(int level) { return level > 0 ? KBL_LENOVO_SALS_ON : KBL_LENOVO_SALS_OFF; }

/* ---- Lenovo ThinkPad: the HKEY device, methods MLCG and MLCS ---------------------------------------------------------
 * (public thinkpad_acpi driver, written again) MLCG(0) answers a status word; the light exists when bit 9 is set, and the
 * level is its low two bits. MLCS(level) sets it. The light has levels 0 (off), 1 and 2. */
#define KBL_THINKPAD_PRESENT_BIT (1u << 9)
#define KBL_THINKPAD_MAX_LEVEL   2

static inline int kbl_thinkpad_present(uint32_t mlcg) { return (mlcg & KBL_THINKPAD_PRESENT_BIT) != 0 && (int32_t)mlcg >= 0; }
static inline int kbl_thinkpad_level(uint32_t mlcg)
{
    int l = (int)(mlcg & 3u);
    return l > KBL_THINKPAD_MAX_LEVEL ? KBL_THINKPAD_MAX_LEVEL : l;
}

/* ---- coexistence with VirtualSMC's SMCLightSensor ---------------------------------------------------------------------
 * SMCLightSensor registers LKSB and LKSS as plain stored values whenever the machine has an ambient light device with a
 * working _ALI method. Two plugins must not own one key, so this plugin stands aside when both are true, unless the boot argument nmkblforce=1 says to register anyway. */
static inline int kbl_should_decline(int smc_light_sensor_loaded, int ambient_light_has_ali, int forced)
{
    return !forced && smc_light_sensor_loaded && ambient_light_has_ali;
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
