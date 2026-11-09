/* test_kbl_core.c - the keyboard backlight driver's kernel-free rules. cc -I kexts/NMKbdBacklight tools/test_kbl_core.c -o /tmp/t && /tmp/t */
#include <stdio.h>
#include "kbl_core.h"
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
int main(void)
{
    /* the value the ASUS table returns for a present light at level 2 (device id | flags | 0x80 << 8 | level) */
    uint32_t dsts = 0x00050000u | 0x00200000u | 0x00100000u | (0x80u << 8) | 2u;
    arm("a state with the presence bit is present", kbl_asus_present(dsts));
    arm("the level is the low seven bits", kbl_asus_level(dsts) == 2);
    arm("the unsupported answer is not present", !kbl_asus_present(KBL_ASUS_UNSUPPORTED) && !kbl_asus_present(0xFFFFFFFFu));
    arm("a state without the presence bit is not present", !kbl_asus_present(0x00000002u));
    arm("a level above the maximum is clamped", kbl_asus_level(0x00010009u) == 3);
    arm("off is argument 0", kbl_asus_set_argument(0) == 0 && kbl_asus_set_argument(-1) == 0);
    arm("a level is 0x80 | level", kbl_asus_set_argument(1) == 0x81 && kbl_asus_set_argument(3) == 0x83);
    arm("a level above the maximum is set as the maximum", kbl_asus_set_argument(9) == 0x83);
    arm("0 maps to level 0 and the largest value to the maximum", kbl_smc_to_level(0, 3) == 0 && kbl_smc_to_level(0xFFF, 3) == 3);
    uint8_t b0, b1;
    kbl_smc_encode(0xABC, &b0, &b1);
    arm("a 12-bit brightness is byte 0 = bits 11..4 and the high nibble of byte 1 = bits 3..0 (Apple's setLEDBrightness)", b0 == 0xAB && b1 == 0xC0);
    arm("the word Apple writes, ((v & 0xF) << 12) | ((v >> 4) & 0xFF) little-endian, is the same two bytes",
        (uint8_t)((((0xABC & 0xF) << 12) | ((0xABC >> 4) & 0xFF)) & 0xFF) == b0 && (uint8_t)((((0xABC & 0xF) << 12) | ((0xABC >> 4) & 0xFF)) >> 8) == b1);
    arm("decoding gives the value back", kbl_smc_decode(b0, b1) == 0xABC);
    int ok = 1;
    for (int l = 0; l <= 3; l++) {
        kbl_smc_encode(kbl_level_to_smc(l, 3), &b0, &b1);
        if (kbl_smc_to_level(kbl_smc_decode(b0, b1), 3) != l) ok = 0;
    }
    arm("every level survives a round trip through the two SMC bytes", ok);
    ok = 1;
    for (int v = 0; v <= 0xFFF; v++) { int l = kbl_smc_to_level(v, 3); if (l < 0 || l > 3) ok = 0; }
    arm("no 12-bit value maps outside the levels", ok);
    /* Lenovo IdeaPad / Legion / LOQ (KBLC) */
    arm("the keyboard query answers 5 (three levels), 7 (three levels with automatic) or something else (plain)",
        kbl_lenovo_type(5) == 2 && kbl_lenovo_type(7) == 3 && kbl_lenovo_type(1) == 0 && kbl_lenovo_type(0) == 0);
    arm("a plain light has two levels, a tri-state one has three", kbl_lenovo_max_level(0) == 1 && kbl_lenovo_max_level(2) == 2 && kbl_lenovo_max_level(3) == 2);
    arm("the get command is (type << 4) | 2", kbl_lenovo_get_command(2) == 0x22 && kbl_lenovo_get_command(3) == 0x32);
    arm("the set command is (level << 16) | (type << 4) | 3", kbl_lenovo_set_command(2, 1) == 0x10023 && kbl_lenovo_set_command(3, 2) == 0x20033);
    arm("a level above the maximum is set as the maximum and a negative one as off", kbl_lenovo_set_command(2, 9) == 0x20023 && kbl_lenovo_set_command(2, -1) == 0x23);
    arm("the answer holds the level in bits 15..1", kbl_lenovo_level_from_answer(0x2) == 1 && kbl_lenovo_level_from_answer(0x4) == 2 && kbl_lenovo_level_from_answer(0x0) == 0);
    arm("the automatic value reads as off and an unknown one as an error", kbl_lenovo_level_from_answer(0x6) == 0 && kbl_lenovo_level_from_answer(0x20) == -1);
    arm("a plain light: HALS bit 4 is support, bit 5 is on, SALS 8 is on and 9 is off",
        kbl_lenovo_plain_supported(0x10) && !kbl_lenovo_plain_supported(0x20) && kbl_lenovo_plain_level(0x20) == 1 && kbl_lenovo_plain_level(0x10) == 0
        && kbl_lenovo_plain_set_argument(1) == 8 && kbl_lenovo_plain_set_argument(0) == 9);
    /* Lenovo ThinkPad (MLCG / MLCS) */
    arm("a ThinkPad has the light when MLCG bit 9 is set, and the level is the low two bits",
        kbl_thinkpad_present(0x00000202) && kbl_thinkpad_level(0x00000202) == 2 && !kbl_thinkpad_present(0x00000002) && !kbl_thinkpad_present(0xFFFFFFFFu));
    arm("a ThinkPad level above two is clamped", kbl_thinkpad_level(0x203) == 2);
    /* coexistence */
    arm("the plugin stands aside only when SMCLightSensor is loaded with a working _ALI and it was not forced",
        kbl_should_decline(1, 1, 0) && !kbl_should_decline(1, 0, 0) && !kbl_should_decline(0, 1, 0) && !kbl_should_decline(1, 1, 1));
    /* the HID route */
    {
        int depth = 0, fine = 1, has_vendor_input = 0, feat_bf_bytes = 0, out_b0_bytes = 0;
        unsigned page = 0, usage = 0, cnt = 0, size = 0, id = 0, last_vendor_usage = 0;
        const uint8_t *d = kKblHidDescriptor; unsigned n = sizeof kKblHidDescriptor, i = 0;
        while (i < n) {
            uint8_t b = d[i]; unsigned len = (b & 3) == 3 ? 4 : (b & 3);
            if (i + 1 + len > n) { fine = 0; break; }
            unsigned v = 0; for (unsigned k = 0; k < len; k++) v |= (unsigned)d[i + 1 + k] << (8 * k);
            unsigned tag = b & 0xFC;
            if (tag == 0x04) page = v; else if (tag == 0x08) { usage = v; if (page == 0xFF00) last_vendor_usage = v; }
            else if (tag == 0x94) cnt = v; else if (tag == 0x74) size = v; else if (tag == 0x84) id = v;
            else if (tag == 0xA0) depth++; else if (tag == 0xC0) depth--;
            else if (tag == 0x80 && page == 0xFF00 && usage == 0x0F && id == 1) has_vendor_input = 1;     /* Input */
            else if (tag == 0xB0 && id == 0xBF) feat_bf_bytes = (int)(cnt * size / 8);                       /* Feature */
            else if (tag == 0x90 && id == 0xB0) out_b0_bytes = (int)(cnt * size / 8);                        /* Output */
            i += 1 + len;
        }
        (void)last_vendor_usage;
        arm("the report descriptor is well formed and its collections balance", fine && depth == 0);
        arm("the descriptor carries the vendor element 0xFF00:0x0F as an input", has_vendor_input);
        arm("feature report 0xBF holds 7 bytes after its id and output report 0xB0 holds 5", feat_bf_bytes == KBL_HID_CONFIG_BYTES - 1 && out_b0_bytes == KBL_HID_SET_BYTES - 1);
    }
    {
        uint8_t c[KBL_HID_CONFIG_BYTES];
        kbl_hid_config_report(c);
        arm("the config report is { id 0xBF, version 1, off 0, on_min 1, on_max 255 } little-endian",
            c[0] == 0xBF && c[1] == 1 && (c[2] | c[3] << 8) == 0 && (c[4] | c[5] << 8) == 1 && (c[6] | c[7] << 8) == 255);
        uint8_t set[6] = { 0xB0, 1, 0x80, 0x00, 0x10, 0x00 };
        int br = -1, rate = -1;
        arm("a set report with its id parses to the brightness and rate", kbl_hid_parse_set(set, 6, &br, &rate) && br == 0x80 && rate == 0x10);
        arm("the same report without its id byte parses too", kbl_hid_parse_set(set + 1, 5, &br, &rate) && br == 0x80);
        uint8_t bad1[6] = { 0xB1, 1, 0, 0, 0, 0 }, bad2[6] = { 0xB0, 2, 0, 0, 0, 0 };
        arm("a wrong id, a wrong version or a wrong length is refused", !kbl_hid_parse_set(bad1, 6, &br, &rate) && !kbl_hid_parse_set(bad2, 6, &br, &rate) && !kbl_hid_parse_set(set, 3, &br, &rate));
        int ok = 1; for (int h = 0; h <= 255; h++) { int l = kbl_smc_to_level(kbl_hid_to_v12(h), 3); if (l < 0 || l > 3) ok = 0; }
        arm("every HID brightness maps to a level", ok);
        arm("full HID brightness is the top level and zero is off", kbl_smc_to_level(kbl_hid_to_v12(255), 3) == 3 && kbl_smc_to_level(kbl_hid_to_v12(0), 3) == 0);
    }
    /* Dell */
    {
        uint8_t b[KBL_DELL_BUF];
        kbl_dell_fill(b, 1, 0x11223344, 0xAABBCCDD, 0);
        arm("the Dell buffer is class 4, select 11, then four little-endian inputs", b[0] == 4 && b[1] == 0 && b[2] == 11 && b[3] == 0 && b[4] == 1 && b[8] == 0x44 && b[11] == 0x11 && b[12] == 0xDD && b[15] == 0xAA);
        for (int j = 0; j < 4; j++) b[20 + 4 + j] = (uint8_t)(0x01020304u >> (8 * j));
        arm("an output is read back little-endian from byte 20", kbl_dell_output(b, 1) == 0x01020304u);
        struct kbl_dell_state st;
        kbl_dell_parse_state(0x0A3F0004u, 0x8B030201u, &st);
        arm("the state parses as dell-laptop reads it", st.mode_bit == 2 && st.triggers == 0x3F && st.idle_value == 10 && st.idle_unit == 0
            && st.als_setting == 1 && st.als_value == 2 && st.level == 3 && st.idle_value_ac == 0x0B && st.idle_unit_ac == 2);
        uint32_t i1, i2;
        kbl_dell_set_inputs(&st, &i1, &i2);
        arm("a set sends the same state back, without the ambient-light value (dell-laptop does not send it)", i1 == 0x0A3F0004u && i2 == 0x8B030001u);
        st.level = 1; kbl_dell_set_inputs(&st, &i1, &i2);
        arm("only the level changes", ((i2 >> 16) & 0xFF) == 1 && i1 == 0x0A3F0004u && (i2 & 0xFFFF) == 0x0001 && (i2 >> 24) == 0x8B);
        arm("the number of levels is bits 23..16 of output 2", kbl_dell_info_levels(0x00030000u) == 3 && kbl_dell_info_levels(0) == 0);
        uint8_t wdg[40] = {0};
        for (int i = 0; i < 16; i++) wdg[20 + i] = kKblDellGuid[i];
        wdg[36] = 'A'; wdg[37] = 'A'; wdg[38] = 1; wdg[39] = 0x02;
        char obj[2] = {0, 0};
        arm("the Dell SMBIOS method is found in a _WDG buffer and named from its object characters", kbl_dell_wdg_find(wdg, 40, obj) && obj[0] == 'A' && obj[1] == 'A');
        wdg[39] = 0x00;
        arm("an entry that is not a method is ignored", !kbl_dell_wdg_find(wdg, 40, obj));
        wdg[39] = 0x02; wdg[25] ^= 1;
        arm("a different GUID is ignored", !kbl_dell_wdg_find(wdg, 40, obj));
    }
    /* MSI */
    {
        arm("a firmware the msi-ec table names is found", kbl_msi_find("14C1EMS1.012") && kbl_msi_find("14C1EMS1.012")->addr == 0xf3 && kbl_msi_find("14C1EMS1.012")->base == 0x80);
        arm("a firmware the table does not name is refused", !kbl_msi_find("99ZZEMS9.999") && !kbl_msi_find("14C1EMS1.013"));
        arm("the table carries the out-of-tree driver's firmware list", KBL_MSI_ROWS >= 200);
        const struct kbl_msi_row *r = kbl_msi_find("14C1EMS1.012");
        arm("a level is the base value OR-ed with the level, clamped to the highest state", kbl_msi_value(r, 0) == 0x80 && kbl_msi_value(r, 2) == 0x82 && kbl_msi_value(r, 9) == 0x83 && kbl_msi_value(r, -1) == 0x80);
        struct kbl_msi_row odd = { "TESTFW000000", 0xf3, 0x83, 3 };
        arm("OR, not addition: a base with low bits set keeps them (0x83 | 1 is 0x83, never 0x84)", kbl_msi_value(&odd, 1) == 0x83 && kbl_msi_value(&odd, 0) == 0x83 && kbl_msi_value(&odd, 3) == 0x83);
        struct kbl_msi_row base4 = { "TESTFW000001", 0xf3, 0x84, 3 };
        arm("a base of 0x84 with level 3 gives 0x87 by OR", kbl_msi_value(&base4, 3) == 0x87);
        arm("the level read is the low two bits of the byte, whatever the base", kbl_msi_level(r, 0x82) == 2 && kbl_msi_level(r, 0x03) == 3 && kbl_msi_level(r, 0x80) == 0 && kbl_msi_level(&odd, 0x83) == 3);
    }
    /* Dell descriptor */
    {
        uint8_t d[20] = { 'D', 'E', 'L', 'L', ' ', 'W', 'M', 'I', 1, 0, 0, 0, 0x00, 0x10, 0, 0, 7, 0, 0, 0 };
        uint32_t size = 0, hot = 0;
        arm("a valid descriptor gives the buffer length and the hotfix", kbl_dell_parse_descriptor(d, 20, &size, &hot) && size == 4096 && hot == 7);
        d[13] = 0x80;
        arm("a 32 KB buffer is read as 32768", kbl_dell_parse_descriptor(d, 20, &size, &hot) && size == 32768);
        d[4] = 'X';
        arm("a wrong signature is refused", !kbl_dell_parse_descriptor(d, 20, &size, &hot));
        d[4] = ' '; d[12] = 0; d[13] = 0; d[14] = 0; d[15] = 0;
        arm("a length too small for the call is refused", !kbl_dell_parse_descriptor(d, 20, &size, &hot));
        arm("a block shorter than a descriptor is refused", !kbl_dell_parse_descriptor(d, 10, &size, &hot));
        uint8_t wdg[20] = {0};
        for (int i = 0; i < 16; i++) wdg[i] = kKblDellDescGuid[i];
        wdg[16] = 'B'; wdg[17] = 'A'; wdg[19] = 0x00;
        char obj[2]; int fl = -1;
        arm("the descriptor block is found in a _WDG buffer with its object characters and flags", kbl_wdg_find(wdg, 20, kKblDellDescGuid, obj, &fl) && obj[0] == 'B' && obj[1] == 'A' && fl == 0);
    }
    /* census: the Razer Blade families carry Dell's SMBIOS call GUID with no descriptor block; the Dell families carry both. */
    {
        uint8_t razer[20] = {0}, dell[40] = {0};
        for (int i = 0; i < 16; i++) { razer[i] = kKblDellGuid[i]; dell[i] = kKblDellGuid[i]; dell[20 + i] = kKblDellDescGuid[i]; }
        razer[16] = 'W'; razer[17] = 'M'; razer[19] = 0x02;
        dell[16] = 'A'; dell[17] = 'A'; dell[19] = 0x02; dell[36] = 'B'; dell[37] = 'A';
        char o[2], d[2]; int f;
        arm("a Razer-style table (SMBIOS GUID, no descriptor) is found as SMBIOS but has no descriptor, so Dell is declined",
            kbl_dell_wdg_find(razer, 20, o) && !kbl_wdg_find(razer, 20, kKblDellDescGuid, d, &f));
        arm("a Dell table (both blocks) passes both lookups", kbl_dell_wdg_find(dell, 40, o) && o[0] == 'A' && kbl_wdg_find(dell, 40, kKblDellDescGuid, d, &f) && d[0] == 'B');
    }
    arm("the method ids are the ASCII of their names", KBL_ASUS_METHOD_DSTS == ('D' | 'S' << 8 | 'T' << 16 | 'S' << 24)
                                                    && KBL_ASUS_METHOD_DEVS == ('D' | 'E' << 8 | 'V' << 16 | 'S' << 24));
    return fails ? 1 : 0;
}
