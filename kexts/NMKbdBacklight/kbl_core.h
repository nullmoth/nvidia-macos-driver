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

/* ---- Dell: the SMBIOS calling interface through ACPI WMI ----------------------------------------------------------------
 * (read from the public dell-laptop and dell-smbios-wmi drivers, written again.) The call is WMI method 1, instance 0, on the
 * Dell SMBIOS GUID, with a buffer that begins with  { u16 class, u16 select, u32 input[4], u32 output[4] }  (36 bytes,
 * little-endian), padded to the size the BIOS expects (4096 or 32768). The keyboard light is class 4, select 11:
 *   input[0] = 0  info   -> output[2] bits 23..16 = number of levels (0 means no settable level)
 *   input[0] = 1  get    -> output[1] bits 15..0 = mode bits, 23..16 triggers, 29..24 idle value, 31..30 idle unit;
 *                           output[2] bits 7..0 = ALS setting, 15..8 = ALS value, 23..16 = level, 29..24 / 31..30 = AC idle
 *   input[0] = 2  set    -> input[1] = the mode bit as a mask | triggers << 16 | idle value << 24 | unit << 30,
 *                           input[2] = ALS setting | level << 16 | AC idle value << 24 | AC unit << 30
 * A set sends back the whole state with only the level changed. */
#define KBL_DELL_CLASS  4
#define KBL_DELL_SELECT 11
#define KBL_DELL_BUF    36

struct kbl_dell_state { int mode_bit, triggers, idle_value, idle_unit, als_setting, als_value, level, idle_value_ac, idle_unit_ac; };

static inline int kbl_dell_info_levels(uint32_t out2) { return (int)((out2 >> 16) & 0xFF); }

static inline void kbl_dell_parse_state(uint32_t out1, uint32_t out2, struct kbl_dell_state *st)
{
    uint32_t m = out1 & 0xFFFF;
    int bit = 0;
    if (m) { while (!((m >> bit) & 1)) bit++; }
    st->mode_bit = bit;
    st->triggers = (int)((out1 >> 16) & 0xFF);
    st->idle_value = (int)((out1 >> 24) & 0x3F);
    st->idle_unit = (int)((out1 >> 30) & 0x3);
    st->als_setting = (int)(out2 & 0xFF);
    st->als_value = (int)((out2 >> 8) & 0xFF);
    st->level = (int)((out2 >> 16) & 0xFF);
    st->idle_value_ac = (int)((out2 >> 24) & 0x3F);
    st->idle_unit_ac = (int)((out2 >> 30) & 0x3);
}

static inline void kbl_dell_set_inputs(const struct kbl_dell_state *st, uint32_t *in1, uint32_t *in2)
{
    *in1 = ((1u << st->mode_bit) & 0xFFFFu) | ((uint32_t)(st->triggers & 0xFF) << 16) | ((uint32_t)(st->idle_value & 0x3F) << 24)
         | ((uint32_t)(st->idle_unit & 3) << 30);
    *in2 = (uint32_t)(st->als_setting & 0xFF) | ((uint32_t)(st->level & 0xFF) << 16) | ((uint32_t)(st->idle_value_ac & 0x3F) << 24)
         | ((uint32_t)(st->idle_unit_ac & 3) << 30);
}

static inline void kbl_dell_fill(uint8_t b[KBL_DELL_BUF], uint32_t in0, uint32_t in1, uint32_t in2, uint32_t in3)
{
    uint32_t in[4] = { in0, in1, in2, in3 };
    for (int i = 0; i < KBL_DELL_BUF; i++) b[i] = 0;
    b[0] = KBL_DELL_CLASS; b[2] = KBL_DELL_SELECT;
    for (int k = 0; k < 4; k++) for (int j = 0; j < 4; j++) b[4 + 4 * k + j] = (uint8_t)(in[k] >> (8 * j));
}

static inline uint32_t kbl_dell_output(const uint8_t b[KBL_DELL_BUF], int k)
{
    uint32_t v = 0;
    for (int j = 0; j < 4; j++) v |= (uint32_t)b[20 + 4 * k + j] << (8 * j);
    return v;
}

/* A _WDG entry is 20 bytes: the GUID in mixed-endian order, two characters naming the object (the method is "WM" + them),
 * an instance count and flags (bit 1 = a method). This is the Dell SMBIOS GUID A80593CE-A997-11DA-B012-B622A1EF5492. */
static const uint8_t kKblDellGuid[16] = { 0xCE, 0x93, 0x05, 0xA8, 0x97, 0xA9, 0xDA, 0x11, 0xB0, 0x12, 0xB6, 0x22, 0xA1, 0xEF, 0x54, 0x92 };

/* the two object characters of the first Dell SMBIOS method entry in a _WDG buffer, or 0 */
static inline int kbl_dell_wdg_find(const uint8_t *wdg, unsigned len, char obj[2])
{
    for (unsigned o = 0; o + 20 <= len; o += 20) {
        int same = 1;
        for (unsigned i = 0; i < 16; i++) if (wdg[o + i] != kKblDellGuid[i]) { same = 0; break; }
        if (same && (wdg[o + 19] & 0x02)) { obj[0] = (char)wdg[o + 16]; obj[1] = (char)wdg[o + 17]; return 1; }
    }
    return 0;
}

/* ---- MSI: the embedded controller, by firmware ---------------------------------------------------------------------------
 * The public msi-ec driver keeps, for each known MSI embedded-controller firmware, the controller address of the keyboard
 * light's state and the base value it sits on (msi_table.h is generated from that table). The firmware string is 12
 * characters at controller address 0xA0. An unknown firmware is REFUSED: the plugin writes controller memory only on a
 * firmware that table names. The controller is reached through ACPI's embedded-controller address space. The table gives the
 * address, the base value and the highest state. The driver BeardOverflow/msi-ec (GPL-2.0) carries the setter and getter. */
#define KBL_MSI_FW_ADDRESS 0xA0
#define KBL_MSI_FW_LENGTH  12
#include "msi_table.h"

/* the table row for a firmware string of KBL_MSI_FW_LENGTH characters (not terminated), or 0 */
static inline const struct kbl_msi_row *kbl_msi_find(const char *fw)
{
    for (unsigned i = 0; i < KBL_MSI_ROWS; i++) {
        const char *a = kKblMsiTable[i].fw;
        int same = 1;
        for (int k = 0; k < KBL_MSI_FW_LENGTH; k++) if (a[k] == 0 || a[k] != fw[k]) { same = 0; break; }
        if (same && a[KBL_MSI_FW_LENGTH] == 0) return &kKblMsiTable[i];
    }
    return 0;
}

/* what the msi-ec driver's keyboard-light setter writes: the base value OR-ed with the level 0 to 3 (not added: a base with low
 * bits set gives a different byte), and what its getter reads: the low two bits of the byte (MSI_EC_KBD_BL_STATE_MASK 0x3). */
#define KBL_MSI_STATE_MASK 0x03
static inline unsigned char kbl_msi_value(const struct kbl_msi_row *r, int level)
{
    if (level < 0) level = 0;
    if (level > r->max_state) level = r->max_state;
    if (level > KBL_MSI_STATE_MASK) level = KBL_MSI_STATE_MASK;
    return (unsigned char)(r->base | level);
}

static inline int kbl_msi_level(const struct kbl_msi_row *r, unsigned char v)
{
    int l = v & KBL_MSI_STATE_MASK;
    return l <= r->max_state ? l : r->max_state;
}

/* ---- Dell's descriptor: how big a buffer the BIOS expects ------------------------------------------------------------------
 * A WMI data block on the descriptor GUID 8D9DDCBC-A997-11DA-B012-B622A1EF5492 holds 20 bytes: "DELL" " WMI", the interface
 * version, the buffer length (4096 or 32768) and the hotfix number, all little-endian from offset 8. */
static const uint8_t kKblDellDescGuid[16] = { 0xBC, 0xDC, 0x9D, 0x8D, 0x97, 0xA9, 0xDA, 0x11, 0xB0, 0x12, 0xB6, 0x22, 0xA1, 0xEF, 0x54, 0x92 };

static inline int kbl_wdg_find(const uint8_t *wdg, unsigned len, const uint8_t guid[16], char obj[2], int *flags)
{
    for (unsigned o = 0; o + 20 <= len; o += 20) {
        int same = 1;
        for (unsigned i = 0; i < 16; i++) if (wdg[o + i] != guid[i]) { same = 0; break; }
        if (same) { obj[0] = (char)wdg[o + 16]; obj[1] = (char)wdg[o + 17]; *flags = wdg[o + 19]; return 1; }
    }
    return 0;
}

/* 1 and the buffer length and hotfix when the block is a valid descriptor with a length the SMBIOS call can use, else 0 */
static inline int kbl_dell_parse_descriptor(const uint8_t *b, unsigned len, uint32_t *size, uint32_t *hotfix)
{
    if (len < 20) return 0;
    static const char sig[8] = { 'D', 'E', 'L', 'L', ' ', 'W', 'M', 'I' };
    for (int i = 0; i < 8; i++) if ((char)b[i] != sig[i]) return 0;
    uint32_t n = (uint32_t)b[12] | (uint32_t)b[13] << 8 | (uint32_t)b[14] << 16 | (uint32_t)b[15] << 24;
    if (n < KBL_DELL_BUF || n > 65536) return 0;
    *size = n;
    *hotfix = (uint32_t)b[16] | (uint32_t)b[17] << 8 | (uint32_t)b[18] << 16 | (uint32_t)b[19] << 24;
    return 1;
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

/* ---- The HID route (macOS 26 on T2-era Mac models) ------------------------------------------------------------------
 * Apple's HID keyboard driver looks for a keyboard that carries the vendor element usage page 0xFF00, usage 0x0F in an input
 * report, then reads the light's range with feature report 0xBF and sets it with output report 0xB0. The layouts below are
 * the ones the public Linux hid-apple driver documents and the macOS driver's own calls match (feature report 191, vendor
 * page 65280, usage 15), written again here:
 *   feature 0xBF (get):  { 0xBF, version = 1, u16 off, u16 on_min, u16 on_max }          8 bytes with the id
 *   output  0xB0 (set):  { 0xB0, version = 1, u16 brightness, u16 rate }                 6 bytes with the id
 * The brightness here runs 0 to KBL_HID_MAX. All fields are little-endian. */
#define KBL_HID_REPORT_CONFIG 0xBF
#define KBL_HID_REPORT_SET    0xB0
#define KBL_HID_VERSION       1
#define KBL_HID_MAX           255
#define KBL_HID_CONFIG_BYTES  8
#define KBL_HID_SET_BYTES     6

static const uint8_t kKblHidDescriptor[] = {
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,                                   /* Generic Desktop, Keyboard, Application */
    0x85, 0x01,                                                           /*   Report ID 1 */
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,   /* modifier keys */
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,                                   /* reserved byte */
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,               /* key array */
    0x06, 0x00, 0xFF, 0x09, 0x0F, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,   /* vendor 0xFF00:0x0F input */
    0x85, 0xBF, 0x06, 0x00, 0xFF, 0x09, 0x10, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x07, 0xB1, 0x02,  /* feature 0xBF */
    0x85, 0xB0, 0x06, 0x00, 0xFF, 0x09, 0x11, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x05, 0x91, 0x02,  /* output 0xB0 */
    0xC0
};

static inline void kbl_hid_config_report(uint8_t out[KBL_HID_CONFIG_BYTES])
{
    out[0] = KBL_HID_REPORT_CONFIG; out[1] = KBL_HID_VERSION;
    out[2] = 0; out[3] = 0;                          /* off */
    out[4] = 1; out[5] = 0;                          /* on_min */
    out[6] = KBL_HID_MAX; out[7] = 0;                /* on_max */
}

/* 1 with the brightness and rate when the buffer is a valid set report (with its id byte, or 5 bytes when the transport
 * already took the id off), else 0 */
static inline int kbl_hid_parse_set(const uint8_t *b, unsigned len, int *brightness, int *rate)
{
    if (len == KBL_HID_SET_BYTES) { if (b[0] != KBL_HID_REPORT_SET) return 0; b++; }
    else if (len != KBL_HID_SET_BYTES - 1) return 0;
    if (b[0] != KBL_HID_VERSION) return 0;
    *brightness = b[1] | (b[2] << 8);
    *rate = b[3] | (b[4] << 8);
    return 1;
}

/* HID brightness 0..255 to the 12-bit value kbl_smc_to_level() takes, and back */
static inline int kbl_hid_to_v12(int hid) { return hid <= 0 ? 0 : hid >= KBL_HID_MAX ? 0xFFF : (hid * 4095 + 127) / 255; }
static inline int kbl_v12_to_hid(int v12) { return v12 <= 0 ? 0 : v12 >= 0xFFF ? KBL_HID_MAX : (v12 * 255 + 2047) / 4095; }

#endif
